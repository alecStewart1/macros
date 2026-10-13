//! Argument helpers shared by the category files: 2-D views over range and
//! array arguments, flattening, and the date-system-aware coercions.

use std::borrow::Cow;

use super::{Arg, Array, Ctx, RangeRef};
use crate::formula::date;
use crate::formula::value::{ErrorKind, Value};

/// Excel's sheet size, for the dimensions of open-ended references
/// (`ROWS(A:A)` is 1,048,576).
pub const SHEET_ROWS: usize = 1_048_576;
pub const SHEET_COLS: usize = 16_384;

/// A rectangular argument: a (clamped) range, an array, or a lone value
/// standing in as a 1×1 area.
#[derive(Clone, Debug)]
pub enum Area<'a> {
    Range(RangeRef),
    Array(Cow<'a, Array>),
    Scalar(Value),
}

/// View an argument as an area. Ranges are clamped to the used area where
/// open-ended; a missing sheet is `#REF!`.
pub fn area<'a>(arg: &'a Arg, ctx: &dyn Ctx) -> Result<Area<'a>, ErrorKind> {
    Ok(match arg {
        Arg::Range(r) => Area::Range(ctx.clamp(r).ok_or(ErrorKind::Ref)?),
        Arg::Array(a) => Area::Array(Cow::Borrowed(a)),
        Arg::Value(v) => Area::Scalar(v.clone()),
        Arg::Omitted => Area::Scalar(Value::Empty),
    })
}

impl Area<'_> {
    pub fn rows(&self) -> usize {
        match self {
            Area::Range(r) => r.rows(),
            Area::Array(a) => a.rows,
            Area::Scalar(_) => 1,
        }
    }

    pub fn cols(&self) -> usize {
        match self {
            Area::Range(r) => r.cols(),
            Area::Array(a) => a.cols,
            Area::Scalar(_) => 1,
        }
    }

    pub fn len(&self) -> usize {
        self.rows() * self.cols()
    }

    /// One row or one column.
    pub fn is_vector(&self) -> bool {
        self.rows() == 1 || self.cols() == 1
    }

    /// The value at `(r, c)` (0-based, in bounds).
    pub fn get(&self, ctx: &dyn Ctx, r: usize, c: usize) -> Value {
        match self {
            Area::Range(rg) => ctx.cell(rg.sheet.as_deref(), rg.r0 + r, rg.c0 + c),
            Area::Array(a) => a.get(r, c).clone(),
            Area::Scalar(v) => v.clone(),
        }
    }

    /// Element `i` of a vector (down a column, or along a row).
    pub fn vget(&self, ctx: &dyn Ctx, i: usize) -> Value {
        if self.cols() == 1 {
            self.get(ctx, i, 0)
        } else {
            self.get(ctx, 0, i)
        }
    }

    /// The `rows`×`cols` sub-area at `(r0, c0)`. For a range it may extend
    /// past the original (Excel's `SUMIF` sum-range resizing).
    pub fn sub(&self, r0: usize, c0: usize, rows: usize, cols: usize) -> Area<'static> {
        match self {
            Area::Range(rg) => Area::Range(RangeRef {
                sheet: rg.sheet.clone(),
                r0: rg.r0 + r0,
                c0: rg.c0 + c0,
                r1: rg.r0 + r0 + rows.max(1) - 1,
                c1: rg.c0 + c0 + cols.max(1) - 1,
            }),
            Area::Array(a) => {
                let mut data = Vec::with_capacity(rows * cols);
                for r in r0..r0 + rows {
                    for c in c0..c0 + cols {
                        data.push(if r < a.rows && c < a.cols {
                            a.get(r, c).clone()
                        } else {
                            Value::Error(ErrorKind::NA)
                        });
                    }
                }
                Area::Array(Cow::Owned(Array { rows, cols, data }))
            }
            Area::Scalar(v) => Area::Scalar(v.clone()),
        }
    }

    /// The values of the top-left `rows`×`cols` window, row-major, reading
    /// only the used part of a range (everything else is empty).
    pub fn window(&self, ctx: &dyn Ctx, rows: usize, cols: usize) -> Vec<Value> {
        let mut out = vec![Value::Empty; rows * cols];
        match self {
            Area::Range(rg) => {
                let win = RangeRef {
                    sheet: rg.sheet.clone(),
                    r0: rg.r0,
                    c0: rg.c0,
                    r1: rg.r0 + rows.max(1) - 1,
                    c1: rg.c0 + cols.max(1) - 1,
                };
                if rows == 0 || cols == 0 {
                    return out;
                }
                if let Some(part) = ctx.data_part(&win) {
                    ctx.for_each(&part, &mut |r, c, v| {
                        out[(r - rg.r0) * cols + (c - rg.c0)] = v;
                    });
                }
            }
            _ => {
                for r in 0..rows.min(self.rows()) {
                    for c in 0..cols.min(self.cols()) {
                        out[r * cols + c] = self.get(ctx, r, c);
                    }
                }
            }
        }
        out
    }

    /// All values, row-major.
    pub fn values(&self, ctx: &dyn Ctx) -> Vec<Value> {
        self.window(ctx, self.rows(), self.cols())
    }

    /// How many leading rows and columns hold any data (the rest of the
    /// area is empty). Arrays and scalars are all data.
    pub fn data_dims(&self, ctx: &dyn Ctx) -> (usize, usize) {
        match self {
            Area::Range(rg) => match ctx.data_part(rg) {
                Some(p) => (p.r1 - rg.r0 + 1, p.c1 - rg.c0 + 1),
                None => (0, 0),
            },
            _ => (self.rows(), self.cols()),
        }
    }
}

/// `max_args` for variadic functions.
pub const MANY: usize = usize::MAX;

/// A function definition for a category's `FUNCS` table:
/// `def!("NAME", min_args, max_args, body)`, or `def!(volatile "NOW", …)`
/// for one re-run on every recalculation. `body` is any
/// `fn(&[Arg], &mut dyn Ctx) -> R` where `R:` [`IntoValue`](super::IntoValue);
/// the result is converted to a cell value here, once.
macro_rules! def {
    (volatile $name:literal, $min:expr, $max:expr, $f:path) => {
        $crate::formula::func::args::def!(@ $name, $min, $max, $f, true)
    };
    ($name:literal, $min:expr, $max:expr, $f:path) => {
        $crate::formula::func::args::def!(@ $name, $min, $max, $f, false)
    };
    (@ $name:literal, $min:expr, $max:expr, $f:path, $volatile:expr) => {
        $crate::formula::func::FuncDef {
            name: $name,
            min_args: $min,
            max_args: $max,
            f: |args: &[$crate::formula::func::Arg], ctx: &mut dyn $crate::formula::func::Ctx| {
                $crate::formula::func::IntoValue::into_value($f(args, ctx))
            },
            volatile: $volatile,
        }
    };
}
pub(crate) use def;

/// Every value of an argument, row-major (a lone value as itself; omitted
/// as nothing). A missing sheet is `#REF!`.
pub fn arg_values(arg: &Arg, ctx: &dyn Ctx) -> Result<Vec<Value>, ErrorKind> {
    match arg {
        Arg::Omitted => Ok(Vec::new()),
        Arg::Value(v) => Ok(vec![v.clone()]),
        _ => Ok(area(arg, ctx)?.values(ctx)),
    }
}

/// An optional numeric argument: its default when left out. (An empty cell
/// passed explicitly is 0, as in Excel.)
pub fn opt_num(arg: Option<&Arg>, ctx: &mut dyn Ctx, default: f64) -> Result<f64, ErrorKind> {
    match arg {
        None | Some(Arg::Omitted) => Ok(default),
        Some(a) => super::num(a, ctx),
    }
}

/// A number truncated toward zero (Excel's treatment of count and digit
/// arguments), or `#NUM!` when it can't fit an integer.
pub fn trunc_i64(x: f64) -> Result<i64, ErrorKind> {
    if !x.is_finite() || x.abs() >= 9.0e18 {
        return Err(ErrorKind::Num);
    }
    Ok(x.trunc() as i64)
}

/// An optional integer argument (truncated).
pub fn opt_int(arg: Option<&Arg>, ctx: &mut dyn Ctx, default: i64) -> Result<i64, ErrorKind> {
    match arg {
        None | Some(Arg::Omitted) => Ok(default),
        Some(a) => trunc_i64(super::num(a, ctx)?),
    }
}

/// An optional boolean argument.
pub fn opt_bool(arg: Option<&Arg>, ctx: &mut dyn Ctx, default: bool) -> Result<bool, ErrorKind> {
    match arg {
        None | Some(Arg::Omitted) => Ok(default),
        Some(a) => super::scalar(a, ctx).to_bool(),
    }
}

/// The current year, for dates typed without one.
pub fn current_year(ctx: &dyn Ctx) -> i64 {
    date::serial_to_ymd(ctx.now(), ctx.date1904())
        .map(|(y, _, _)| y)
        .unwrap_or(2000)
}

/// A value as a date serial: numbers as-is, text as a number or as a typed
/// date/time (`"2024-03-01"`, `"3/1/2024 2:30 PM"`), booleans as 1/0, empty
/// as 0.
pub fn to_serial(v: &Value, ctx: &dyn Ctx) -> Result<f64, ErrorKind> {
    match v {
        Value::Text(s) => {
            crate::formula::value::parse_typed_number(s, Some(current_year(ctx)), ctx.date1904())
                .ok_or(ErrorKind::Value)
        }
        v => v.to_number(),
    }
}

/// A date argument: [`to_serial`] of the scalar, `#NUM!` when negative or
/// past 9999-12-31.
pub fn date_arg(arg: &Arg, ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    let v = super::scalar(arg, ctx);
    let s = to_serial(&v, ctx)?;
    if s < 0.0 || s >= date::max_serial(ctx.date1904()) + 1.0 {
        return Err(ErrorKind::Num);
    }
    Ok(s)
}
