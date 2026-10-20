//! Argument helpers shared by the category files: 2-D views over range and
//! array arguments, flattening, and the date-system-aware coercions.

use std::borrow::Cow;

use super::{Arg, Array, Ctx, RangeRef};
use crate::formula::date;
use crate::formula::value::{ErrorKind, Value};

/// Most cells a formula may expand into one dense array (about 8.4M: four
/// full Excel columns). Past it the result is `#NUM!` rather than an
/// allocation of billions of values that aborts the whole editor.
pub const MAX_ARRAY_CELLS: usize = 1 << 23;

/// `rows × cols` when it fits under [`MAX_ARRAY_CELLS`], else `#NUM!`.
pub fn dense_cells(rows: usize, cols: usize) -> Result<usize, ErrorKind> {
    rows.checked_mul(cols)
        .filter(|&n| n <= MAX_ARRAY_CELLS)
        .ok_or(ErrorKind::Num)
}

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
                if dense_cells(rows, cols).is_err() {
                    return Area::Scalar(Value::Error(ErrorKind::Num));
                }
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
    /// `#NUM!` past [`MAX_ARRAY_CELLS`]: the whole area becomes one dense
    /// list, so an explicit `A1:Z99999999` would otherwise try to allocate
    /// billions of cells and abort the app.
    pub fn values(&self, ctx: &dyn Ctx) -> Result<Vec<Value>, ErrorKind> {
        dense_cells(self.rows(), self.cols())?;
        Ok(self.window(ctx, self.rows(), self.cols()))
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
/// `def!("NAME", min_args, max_args, body, "signature")`, or
/// `def!(volatile "NOW", …)` for one re-run on every recalculation. `body`
/// is any
/// `fn(&[Arg], &mut dyn Ctx) -> R` where `R:` [`IntoValue`](super::IntoValue);
/// the result is converted to a cell value here, once. `def!(array
/// "SORT", …)` defines a dynamic-array function, whose body returns
/// `Result<Array, ErrorKind>`.
macro_rules! def {
    (array $name:literal, $min:expr, $max:expr, $f:path, $sig:literal) => {
        $crate::formula::func::FuncDef {
            name: $name,
            min_args: $min,
            max_args: $max,
            sig: $sig,
            f: |args: &[$crate::formula::func::Arg], ctx: &mut dyn $crate::formula::func::Ctx| {
                match $f(args, ctx) {
                    Ok(a) => a.data.into_iter().next().unwrap_or($crate::formula::value::Value::Empty),
                    Err(e) => $crate::formula::value::Value::Error(e),
                }
            },
            array: Some($f),
            volatile: false,
        }
    };
    (volatile $name:literal, $min:expr, $max:expr, $f:path, $sig:literal) => {
        $crate::formula::func::args::def!(@ $name, $min, $max, $f, $sig, true)
    };
    ($name:literal, $min:expr, $max:expr, $f:path, $sig:literal) => {
        $crate::formula::func::args::def!(@ $name, $min, $max, $f, $sig, false)
    };
    (@ $name:literal, $min:expr, $max:expr, $f:path, $sig:literal, $volatile:expr) => {
        $crate::formula::func::FuncDef {
            name: $name,
            min_args: $min,
            max_args: $max,
            sig: $sig,
            f: |args: &[$crate::formula::func::Arg], ctx: &mut dyn $crate::formula::func::Ctx| {
                $crate::formula::func::IntoValue::into_value($f(args, ctx))
            },
            array: None,
            volatile: $volatile,
        }
    };
}
pub(crate) use def;

/// Fold every value of every argument into `acc`, the way Excel's
/// aggregates read them: a value given directly goes to `direct`, each
/// value inside a range or array to `element` (row-major, reading only the
/// used part of a range, so `A:A` is one pass over the data). Omitted
/// arguments and empty values are skipped. A reference to a missing sheet
/// is one `#REF!` element, as Excel reads a deleted sheet's reference. The
/// first error a callback returns ends the fold.
pub fn fold_args<A>(
    args: &[Arg],
    ctx: &mut dyn Ctx,
    mut acc: A,
    direct: impl Fn(&mut A, &Value) -> Result<(), ErrorKind>,
    element: impl Fn(&mut A, &Value) -> Result<(), ErrorKind>,
) -> Result<A, ErrorKind> {
    for arg in args {
        match arg {
            Arg::Omitted | Arg::Value(Value::Empty) => {}
            Arg::Value(v) => direct(&mut acc, v)?,
            Arg::Array(a) => {
                for v in &a.data {
                    if *v != Value::Empty {
                        element(&mut acc, v)?;
                    }
                }
            }
            Arg::Range(r) => {
                if ctx.extent(r.sheet.as_deref()).is_none() {
                    element(&mut acc, &Value::Error(ErrorKind::Ref))?;
                    continue;
                }
                let Some(r) = ctx.data_part(r) else {
                    continue;
                };
                let mut err = None;
                ctx.for_each(&r, &mut |_, _, v| {
                    if err.is_none() && v != Value::Empty {
                        err = element(&mut acc, &v).err();
                    }
                });
                if let Some(e) = err {
                    return Err(e);
                }
            }
        }
    }
    Ok(acc)
}

/// Every value of an argument, row-major (a lone value as itself; omitted
/// as nothing). A missing sheet is `#REF!`.
pub fn arg_values(arg: &Arg, ctx: &dyn Ctx) -> Result<Vec<Value>, ErrorKind> {
    match arg {
        Arg::Omitted => Ok(Vec::new()),
        Arg::Value(v) => Ok(vec![v.clone()]),
        _ => area(arg, ctx)?.values(ctx),
    }
}

/// An optional argument that was given: `None` when it's left out, past
/// the end of the list or as an empty slot (`ROUND(2.5,)`).
pub fn given(arg: Option<&Arg>) -> Option<&Arg> {
    arg.filter(|a| !matches!(a, Arg::Omitted))
}

/// An optional numeric argument: its default when left out. (An empty cell
/// passed explicitly is 0, as in Excel.)
pub fn opt_num(arg: Option<&Arg>, ctx: &mut dyn Ctx, default: f64) -> Result<f64, ErrorKind> {
    match given(arg) {
        None => Ok(default),
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

/// An integer argument: the number truncated ([`trunc_i64`]).
pub fn int(arg: &Arg, ctx: &mut dyn Ctx) -> Result<i64, ErrorKind> {
    trunc_i64(super::num(arg, ctx)?)
}

/// A 1-based position (`MID`'s start, `VLOOKUP`'s column): below 1 is
/// `#VALUE!`.
pub fn positive(n: i64) -> Result<usize, ErrorKind> {
    if n < 1 {
        Err(ErrorKind::Value)
    } else {
        Ok(n as usize)
    }
}

/// A 1-based position argument ([`int`], then [`positive`]).
pub fn pos_int(arg: &Arg, ctx: &mut dyn Ctx) -> Result<usize, ErrorKind> {
    positive(int(arg, ctx)?)
}

/// An optional integer argument (truncated).
pub fn opt_int(arg: Option<&Arg>, ctx: &mut dyn Ctx, default: i64) -> Result<i64, ErrorKind> {
    match given(arg) {
        None => Ok(default),
        Some(a) => int(a, ctx),
    }
}

/// An optional count argument (`LEFT`'s `num_chars`): truncated, negative
/// is `#VALUE!`.
pub fn count_arg(arg: Option<&Arg>, ctx: &mut dyn Ctx, default: i64) -> Result<usize, ErrorKind> {
    let n = opt_int(arg, ctx, default)?;
    if n < 0 {
        Err(ErrorKind::Value)
    } else {
        Ok(n as usize)
    }
}

/// An optional boolean argument.
pub fn opt_bool(arg: Option<&Arg>, ctx: &mut dyn Ctx, default: bool) -> Result<bool, ErrorKind> {
    match given(arg) {
        None => Ok(default),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formula::func::testing::{Mock, check, check0};

    fn range(r0: usize, c0: usize, r1: usize, c1: usize) -> Arg {
        Arg::Range(RangeRef {
            sheet: None,
            r0,
            c0,
            r1,
            c1,
        })
    }

    /// [`area`] of a range, which borrows nothing.
    fn range_area(m: &Mock, r0: usize, c0: usize, r1: usize, c1: usize) -> Area<'static> {
        match area(&range(r0, c0, r1, c1), m).unwrap() {
            Area::Range(r) => Area::Range(r),
            other => panic!("{other:?}"),
        }
    }

    fn missing_sheet() -> Arg {
        Arg::Range(RangeRef {
            sheet: Some("Nope".into()),
            r0: 0,
            c0: 0,
            r1: 0,
            c1: 0,
        })
    }

    fn num(n: f64) -> Arg {
        Arg::Value(Value::Number(n))
    }

    fn text(s: &str) -> Arg {
        Arg::Value(Value::text(s))
    }

    #[test]
    fn dense_cells_caps_at_the_limit() {
        assert_eq!(dense_cells(3, 4), Ok(12));
        assert_eq!(dense_cells(MAX_ARRAY_CELLS, 1), Ok(MAX_ARRAY_CELLS));
        assert_eq!(dense_cells(MAX_ARRAY_CELLS, 2), Err(ErrorKind::Num));
        assert_eq!(dense_cells(usize::MAX, 2), Err(ErrorKind::Num));
    }

    #[test]
    fn area_clamps_open_ranges_and_rejects_missing_sheets() {
        let m = Mock::grid(&[&["1", "2"], &["3", "4"], &["5", "6"]]);
        // A:A closes at the used area's last row.
        let a = range_area(&m, 0, 0, usize::MAX, 0);
        assert_eq!((a.rows(), a.cols()), (3, 1));
        assert!(a.is_vector());
        // Explicit bounds are kept past the data.
        let a = range_area(&m, 0, 0, 9, 1);
        assert_eq!((a.rows(), a.cols(), a.len()), (10, 2, 20));
        assert!(matches!(area(&missing_sheet(), &m), Err(ErrorKind::Ref)));
        // An omitted argument is a lone empty value.
        let a = area(&Arg::Omitted, &m).unwrap();
        assert_eq!(a.get(&m, 0, 0), Value::Empty);
        assert_eq!(a.len(), 1);
    }

    #[test]
    fn area_reads_ranges_arrays_and_scalars() {
        let m = Mock::grid(&[&["1", "2"], &["3", "4"]]);
        let r = range_area(&m, 0, 0, 1, 1);
        assert_eq!(r.get(&m, 1, 0), Value::Number(3.0));
        let row = range_area(&m, 1, 0, 1, 1);
        assert_eq!(row.vget(&m, 1), Value::Number(4.0));
        let col = range_area(&m, 0, 1, 1, 1);
        assert_eq!(col.vget(&m, 1), Value::Number(4.0));

        let arr = Arg::Array(Array {
            rows: 2,
            cols: 2,
            data: vec![
                Value::Number(1.0),
                Value::Number(2.0),
                Value::Number(3.0),
                Value::Number(4.0),
            ],
        });
        let a = area(&arr, &m).unwrap();
        assert_eq!(a.get(&m, 1, 1), Value::Number(4.0));
        assert!(!a.is_vector());
        let seven = num(7.0);
        let s = area(&seven, &m).unwrap();
        assert_eq!(s.get(&m, 5, 5), Value::Number(7.0));
    }

    #[test]
    fn sub_pads_arrays_with_na_and_extends_ranges() {
        let m = Mock::new();
        let arr = Arg::Array(Array {
            rows: 1,
            cols: 2,
            data: vec![Value::Number(1.0), Value::Number(2.0)],
        });
        let a = area(&arr, &m).unwrap();
        let s = a.sub(0, 1, 2, 2);
        assert_eq!((s.rows(), s.cols()), (2, 2));
        assert_eq!(s.get(&m, 0, 0), Value::Number(2.0));
        assert_eq!(s.get(&m, 0, 1), Value::Error(ErrorKind::NA));
        assert_eq!(s.get(&m, 1, 0), Value::Error(ErrorKind::NA));
        // Too large to materialise: one `#NUM!`.
        assert!(matches!(
            a.sub(0, 0, MAX_ARRAY_CELLS, 2),
            Area::Scalar(Value::Error(ErrorKind::Num))
        ));
        // A range sub-area may run past the original (SUMIF's resizing).
        let r = Area::Range(RangeRef {
            sheet: None,
            r0: 2,
            c0: 3,
            r1: 2,
            c1: 3,
        });
        match r.sub(1, 1, 3, 2) {
            Area::Range(rg) => assert_eq!((rg.r0, rg.c0, rg.r1, rg.c1), (3, 4, 5, 5)),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn window_values_and_data_dims_read_only_the_used_part() {
        let m = Mock::grid(&[&["1", ""], &["", "4"]]);
        let a = range_area(&m, 0, 0, 3, 2);
        assert_eq!(a.data_dims(&m), (2, 2));
        let v = a.values(&m).unwrap();
        assert_eq!(v.len(), 12);
        assert_eq!(v[0], Value::Number(1.0));
        assert_eq!(v[1], Value::Empty);
        assert_eq!(v[4], Value::Number(4.0));
        assert!(v[5..].iter().all(|x| *x == Value::Empty));
        // A range entirely past the data has no data part.
        let far = range_area(&m, 10, 10, 12, 12);
        assert_eq!(far.data_dims(&m), (0, 0));
        assert!(far.values(&m).unwrap().iter().all(|x| *x == Value::Empty));
        // An explicit range too large to list is `#NUM!`, not an abort.
        let huge = range_area(&m, 0, 0, 99_999_999, 25);
        assert_eq!(huge.values(&m), Err(ErrorKind::Num));
    }

    #[test]
    fn fold_args_splits_direct_values_from_elements() {
        // A1 holds "x" text, A2 TRUE, A3 is blank, A4 is 5.
        let mut m = Mock::grid(&[&["'x"], &["TRUE"], &[""], &["5"]]);
        let args = [
            num(1.0),
            Arg::Omitted,
            Arg::Value(Value::Empty),
            text("t"),
            range(0, 0, 3, 0),
        ];
        let (direct, element) = fold_args(
            &args,
            &mut m,
            (Vec::new(), Vec::new()),
            |acc, v| {
                acc.0.push(v.clone());
                Ok(())
            },
            |acc, v| {
                acc.1.push(v.clone());
                Ok(())
            },
        )
        .unwrap();
        // Omitted and empty arguments never reach either callback.
        assert_eq!(direct, [Value::Number(1.0), Value::text("t")]);
        // Blank cells are skipped; everything else is an element.
        assert_eq!(
            element,
            [Value::text("x"), Value::Bool(true), Value::Number(5.0)]
        );
    }

    #[test]
    fn fold_args_reports_missing_sheet_as_ref_and_stops_at_first_error() {
        let mut m = Mock::grid(&[&["1"], &["2"], &["3"]]);
        let seen = fold_args(
            &[missing_sheet(), num(9.0)],
            &mut m,
            Vec::new(),
            |acc: &mut Vec<Value>, v| {
                acc.push(v.clone());
                Ok(())
            },
            |acc, v| {
                acc.push(v.clone());
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(seen, [Value::Error(ErrorKind::Ref), Value::Number(9.0)]);

        let res = fold_args(
            &[range(0, 0, 2, 0)],
            &mut m,
            (),
            |_, _| Ok(()),
            |_, v| {
                if *v == Value::Number(2.0) {
                    Err(ErrorKind::Div0)
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(res, Err(ErrorKind::Div0));
    }

    #[test]
    fn arg_values_lists_every_value() {
        let m = Mock::grid(&[&["1", "2"]]);
        assert_eq!(arg_values(&Arg::Omitted, &m), Ok(vec![]));
        assert_eq!(arg_values(&num(3.0), &m), Ok(vec![Value::Number(3.0)]));
        assert_eq!(
            arg_values(&range(0, 0, 0, 2), &m),
            Ok(vec![Value::Number(1.0), Value::Number(2.0), Value::Empty])
        );
        assert_eq!(arg_values(&missing_sheet(), &m), Err(ErrorKind::Ref));
    }

    #[test]
    fn optional_arguments_take_defaults_only_when_left_out() {
        let mut m = Mock::new();
        assert!(given(None).is_none());
        assert!(given(Some(&Arg::Omitted)).is_none());
        assert!(given(Some(&num(1.0))).is_some());

        assert_eq!(opt_num(None, &mut m, 7.0), Ok(7.0));
        assert_eq!(opt_num(Some(&Arg::Omitted), &mut m, 7.0), Ok(7.0));
        // An empty value given explicitly is 0, not the default.
        assert_eq!(
            opt_num(Some(&Arg::Value(Value::Empty)), &mut m, 7.0),
            Ok(0.0)
        );
        assert_eq!(opt_num(Some(&text("2.5")), &mut m, 7.0), Ok(2.5));
        assert_eq!(
            opt_num(Some(&text("abc")), &mut m, 7.0),
            Err(ErrorKind::Value)
        );

        assert_eq!(opt_int(None, &mut m, -3), Ok(-3));
        assert_eq!(opt_int(Some(&num(-2.9)), &mut m, 0), Ok(-2));
        assert_eq!(count_arg(None, &mut m, 1), Ok(1));
        assert_eq!(count_arg(Some(&num(2.7)), &mut m, 1), Ok(2));
        assert_eq!(
            count_arg(Some(&num(-1.0)), &mut m, 1),
            Err(ErrorKind::Value)
        );

        assert_eq!(opt_bool(None, &mut m, true), Ok(true));
        assert_eq!(opt_bool(Some(&num(0.0)), &mut m, true), Ok(false));
        assert_eq!(opt_bool(Some(&text("true")), &mut m, false), Ok(true));
        assert_eq!(
            opt_bool(Some(&text("yes")), &mut m, false),
            Err(ErrorKind::Value)
        );
    }

    #[test]
    fn integer_and_position_arguments() {
        let mut m = Mock::new();
        assert_eq!(trunc_i64(2.9), Ok(2));
        assert_eq!(trunc_i64(-2.9), Ok(-2));
        assert_eq!(trunc_i64(f64::NAN), Err(ErrorKind::Num));
        assert_eq!(trunc_i64(f64::INFINITY), Err(ErrorKind::Num));
        assert_eq!(trunc_i64(1e19), Err(ErrorKind::Num));
        assert_eq!(int(&Arg::Value(Value::Bool(true)), &mut m), Ok(1));
        assert_eq!(
            int(&Arg::Value(Value::Error(ErrorKind::NA)), &mut m),
            Err(ErrorKind::NA)
        );
        assert_eq!(positive(1), Ok(1));
        assert_eq!(positive(0), Err(ErrorKind::Value));
        assert_eq!(pos_int(&num(3.5), &mut m), Ok(3));
        assert_eq!(pos_int(&num(0.5), &mut m), Err(ErrorKind::Value));
    }

    #[test]
    fn date_arguments_accept_typed_dates_and_reject_out_of_range() {
        let mut m = Mock::new();
        assert_eq!(current_year(&m), 2024);
        assert_eq!(to_serial(&Value::text("2024-01-01"), &m), Ok(45292.0));
        assert_eq!(to_serial(&Value::Bool(true), &m), Ok(1.0));
        assert_eq!(to_serial(&Value::Empty, &m), Ok(0.0));
        assert_eq!(to_serial(&Value::text("soon"), &m), Err(ErrorKind::Value));
        assert_eq!(date_arg(&text("2024-01-01"), &mut m), Ok(45292.0));
        assert_eq!(date_arg(&num(-1.0), &mut m), Err(ErrorKind::Num));
        assert_eq!(date_arg(&num(3_000_000.0), &mut m), Err(ErrorKind::Num));
        // 1904 system: the same date is 1462 days smaller.
        m.d1904 = true;
        assert_eq!(date_arg(&text("2024-01-01"), &mut m), Ok(45292.0 - 1462.0));
    }

    #[test]
    fn aggregates_coerce_direct_values_but_skip_them_in_ranges() {
        // A1 TRUE, A2 "3" as text, A3 blank, A4 empty-string text, A5 2.
        let mut m = Mock::grid(&[&["TRUE"], &["'3"], &[""], &["'"], &["2"]]);
        check(
            &mut m,
            &[
                // A boolean or numeric text given directly is a number…
                ("SUM(TRUE, \"3\", 2)", "6"),
                ("COUNT(TRUE, \"3\", 2)", "3"),
                // …but inside a range it's skipped.
                ("SUM(A1:A5)", "2"),
                ("COUNT(A1:A5)", "1"),
                // COUNTA counts `""` text but not a blank cell.
                ("COUNTA(A1:A5)", "4"),
                ("COUNTBLANK(A1:A5)", "2"),
                // `""` given directly isn't a number; an omitted one is skipped.
                ("SUM(\"\")", "#VALUE!"),
                ("SUM(1,,2)", "3"),
                ("SUM(\"abc\")", "#VALUE!"),
                ("SUM({1,TRUE,\"3\"})", "1"),
            ],
        );
    }

    #[test]
    fn errors_spread_from_direct_values_ranges_and_arrays() {
        let mut m = Mock::grid(&[&["1"], &["#DIV/0!"], &["3"]]);
        check(
            &mut m,
            &[
                ("SUM(A1:A3)", "#DIV/0!"),
                ("SUM(1, #N/A)", "#N/A"),
                ("SUM({1,#REF!})", "#REF!"),
                ("SUM(Nope!A1)", "#REF!"),
                ("ROUND(#NUM!, 1)", "#NUM!"),
                ("ROUND(1, #N/A)", "#N/A"),
                ("LEFT(\"abc\", -1)", "#VALUE!"),
                // Not every function propagates: ISERROR reads it.
                ("ISERROR(A2)", "TRUE"),
            ],
        );
    }

    #[test]
    fn blank_cell_and_empty_text_differ_by_function() {
        // A1 is blank, B1 holds `""` text.
        let mut m = Mock::grid(&[&["", "'"], &["x"]]);
        check(
            &mut m,
            &[
                ("ISBLANK(A1)", "TRUE"),
                ("ISBLANK(B1)", "FALSE"),
                ("ISTEXT(B1)", "TRUE"),
                ("LEN(A1)", "0"),
                ("LEN(B1)", "0"),
                ("ABS(A1)", "0"),
                ("ABS(B1)", "#VALUE!"),
            ],
        );
        check0(&[("ROUND(2.5,)", "3"), ("LEFT(\"abc\")", "\"a\"")]);
    }
}
