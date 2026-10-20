//! The built-in function library and the interface every function uses.
//!
//! A function is a plain `fn(&[Arg], &mut dyn Ctx) -> R`, where `R` is
//! anything [`IntoValue`] (typically `Result<f64, ErrorKind>`). Arguments
//! arrive evaluated, except for the handful of short-circuiting forms (`IF`,
//! `IFS`, `IFERROR`, `IFNA`, `CHOOSE`, `SWITCH`) that the evaluator runs
//! itself. Ranges arrive unevaluated as [`RangeRef`]s and are read through
//! the [`Ctx`], which walks the grid's column-major storage directly — so
//! `SUM(A:A)` over a million rows is one pass, not a million lookups.
//!
//! Each category lives in its own file and exposes a `FUNCS` table; the
//! registry here concatenates them.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use super::value::{ErrorKind, Value};

/// A rectangle of cells, inclusive, in absolute 0-based coordinates. A
/// whole-column reference (`A:A`) has `r1 == usize::MAX`; a whole-row one
/// (`3:3`) has `c1 == usize::MAX`. Use [`Ctx::clamp`] before iterating.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RangeRef {
    /// `None` = the formula's own sheet.
    pub sheet: Option<Arc<str>>,
    pub r0: usize,
    pub c0: usize,
    pub r1: usize,
    pub c1: usize,
}

impl RangeRef {
    pub fn rows(&self) -> usize {
        self.r1.saturating_sub(self.r0).saturating_add(1)
    }

    pub fn cols(&self) -> usize {
        self.c1.saturating_sub(self.c0).saturating_add(1)
    }

    pub fn is_cell(&self) -> bool {
        self.r0 == self.r1 && self.c0 == self.c1
    }
}

/// A 2-D array of values, row-major (array literals `{1,2;3,4}` and the
/// results of array arithmetic such as `(A1:A3>1)*B1:B3`).
#[derive(Clone, Debug, PartialEq)]
pub struct Array {
    pub rows: usize,
    pub cols: usize,
    pub data: Vec<Value>,
}

impl Array {
    pub fn get(&self, r: usize, c: usize) -> &Value {
        &self.data[r * self.cols + c]
    }
}

/// One argument as a function receives it.
#[derive(Clone, Debug, PartialEq)]
pub enum Arg {
    Value(Value),
    Range(RangeRef),
    Array(Array),
    /// Left out: `IF(A1,,1)` / `ROUND(2.5)` style. Functions treat it as
    /// their documented default.
    Omitted,
}

/// What a function can see of the world.
pub trait Ctx {
    /// The value of one cell (the result, for a formula cell).
    fn cell(&self, sheet: Option<&str>, row: usize, col: usize) -> Value;
    /// `(rows, cols)` of the used area of `sheet`; `None` if no such sheet.
    fn extent(&self, sheet: Option<&str>) -> Option<(usize, usize)>;
    /// Visit every cell of `range` (already clamped) in row-major order.
    /// The default walks `cell`; the grid overrides it with a chunked scan.
    fn for_each(&self, range: &RangeRef, f: &mut dyn FnMut(usize, usize, Value)) {
        for r in range.r0..=range.r1 {
            for c in range.c0..=range.c1 {
                f(r, c, self.cell(range.sheet.as_deref(), r, c));
            }
        }
    }
    /// The formula's own `(row, col)` (`ROW()`, `COLUMN()`).
    fn here(&self) -> (usize, usize);
    /// The current date-time as an Excel serial number (`NOW`, `TODAY`).
    fn now(&self) -> f64;
    /// A uniform random number in `[0, 1)` (`RAND`, `RANDBETWEEN`).
    fn random(&mut self) -> f64;
    /// Call a function the library doesn't have — a Scheme function defined
    /// with `define-grid-function`. `None` when no such function exists
    /// (the call is then `#NAME?`). The grid returns `#CALC!` while the call
    /// runs and recalculates when its result arrives.
    fn call_external(&mut self, name: &str, args: &[Arg]) -> Option<Value> {
        let _ = (name, args);
        None
    }

    /// `range` with open ends (`A:A`, `3:3`) closed at the used area, so a
    /// whole-column reference never walks the infinite grid. Explicit
    /// bounds are kept (`ROWS(A1:A100)` is 100 even with 3 rows of data).
    /// `None` for a missing sheet.
    fn clamp(&self, range: &RangeRef) -> Option<RangeRef> {
        let (rows, cols) = self.extent(range.sheet.as_deref())?;
        let mut r = range.clone();
        if r.r1 == usize::MAX {
            r.r1 = rows.saturating_sub(1).max(r.r0);
        }
        if r.c1 == usize::MAX {
            r.c1 = cols.saturating_sub(1).max(r.c0);
        }
        Some(r)
    }

    /// The part of `range` that overlaps the used area — what's worth
    /// iterating, since every cell outside it is empty. `None` when nothing
    /// overlaps (or the sheet is missing). Count empties outside it from the
    /// dimensions of [`Ctx::clamp`] where that matters (`COUNTBLANK`).
    fn data_part(&self, range: &RangeRef) -> Option<RangeRef> {
        let (rows, cols) = self.extent(range.sheet.as_deref())?;
        if rows == 0 || cols == 0 || range.r0 >= rows || range.c0 >= cols {
            return None;
        }
        let mut r = range.clone();
        r.r1 = r.r1.min(rows - 1);
        r.c1 = r.c1.min(cols - 1);
        Some(r)
    }

    /// The workbook uses the 1904 date system (serial 0 = 1904-01-01)
    /// instead of the 1900 one.
    fn date1904(&self) -> bool {
        false
    }

    /// A hash index over a one-row or one-column `range` (already clamped)
    /// for exact-match lookups (`VLOOKUP(…, FALSE)`, `MATCH(…, 0)`,
    /// `XLOOKUP`), so 100k lookups over 100k rows aren't O(n²). The grid
    /// caches one per range and invalidates it when a cell in the range
    /// changes, building it with [`lookup::LookupIndex::build`]. `None`
    /// (the default) makes lookups scan linearly.
    fn lookup_index(&mut self, _range: &RangeRef) -> Option<Arc<lookup::LookupIndex>> {
        None
    }

    /// `A1#`: the range the formula at `(row, col)` spills into (just that
    /// cell for a formula of one value). An error when the formula's result
    /// is one (`#SPILL!` while it's blocked), and `#REF!` when the cell holds
    /// no formula — the default, for hosts without spilling.
    fn spill_range(
        &mut self,
        sheet: Option<&str>,
        row: usize,
        col: usize,
    ) -> Result<RangeRef, ErrorKind> {
        let _ = (sheet, row, col);
        Err(ErrorKind::Ref)
    }
}

/// The body of a dynamic-array function (see [`FuncDef::array`]).
pub type ArrayFn = fn(&[Arg], &mut dyn Ctx) -> Result<Array, ErrorKind>;

/// A function's signature and implementation.
pub struct FuncDef {
    /// Upper-case name, as Excel stores it (`SUM`, `STDEV.P`).
    pub name: &'static str,
    pub min_args: usize,
    /// `usize::MAX` for variadic functions.
    pub max_args: usize,
    /// The argument names, for the signature hint shown while a formula is
    /// typed (see [`sigs`]).
    pub sig: &'static str,
    /// The body, its result already converted to a cell value (see
    /// [`args::def`]).
    pub f: fn(&[Arg], &mut dyn Ctx) -> Value,
    /// For a dynamic-array function (`FILTER`, `SEQUENCE`, …), the body
    /// returning its whole array, which a formula spills; `f` then gives
    /// the top-left value. `None` for a function of one value.
    pub array: Option<ArrayFn>,
    /// Re-run on every recalculation (`NOW`, `RAND`).
    pub volatile: bool,
}

/// The function named `name` (case-insensitive).
pub fn lookup(name: &str) -> Option<&'static FuncDef> {
    static BY_NAME: OnceLock<HashMap<&str, &FuncDef>> = OnceLock::new();
    let by_name = BY_NAME.get_or_init(|| {
        let mut map = HashMap::new();
        for f in all() {
            map.entry(f.name).or_insert(f);
        }
        map
    });
    // Names in a parsed formula are already upper case (`Expr::Call`).
    if name.bytes().any(|b| b.is_ascii_lowercase()) {
        by_name.get(name.to_ascii_uppercase().as_str()).copied()
    } else {
        by_name.get(name).copied()
    }
}

/// Every built-in function, for completion and docs.
pub fn all() -> impl Iterator<Item = &'static FuncDef> {
    TABLES.iter().flat_map(|t| t.iter())
}

/// Functions the evaluator implements itself because they don't evaluate
/// every argument (they short-circuit): `(name, min_args, max_args,
/// signature)`.
pub const SPECIAL: &[(&str, usize, usize, &str)] = &[
    ("IF", 2, 3, "logical_test, value_if_true, [value_if_false]"),
    (
        "IFS",
        2,
        usize::MAX,
        "condition1, value1, [condition2, value2], …",
    ),
    ("IFERROR", 2, 2, "value, value_if_error"),
    ("IFNA", 2, 2, "value, value_if_na"),
    ("CHOOSE", 2, usize::MAX, "index, value1, [value2], …"),
    (
        "SWITCH",
        3,
        usize::MAX,
        "expression, value1, result1, [value2, result2], …, [default]",
    ),
];

/// The [`SPECIAL`] entry for `name` (case-insensitive).
fn special_form(name: &str) -> Option<&'static (&'static str, usize, usize, &'static str)> {
    SPECIAL.iter().find(|s| s.0.eq_ignore_ascii_case(name))
}

/// Arity of an evaluator-implemented form (see [`SPECIAL`]).
pub fn special(name: &str) -> Option<(usize, usize)> {
    special_form(name).map(|&(_, min, max, _)| (min, max))
}

/// The argument list of function `name`, built-in or evaluator form.
pub fn signature(name: &str) -> Option<&'static str> {
    lookup(name)
        .map(|f| f.sig)
        .or_else(|| special_form(name).map(|s| s.3))
}

pub mod args;
pub use args::dense_cells;
mod array;
mod datefn;
mod info;
mod logic;
pub mod lookup;
pub mod math;
#[cfg(test)]
mod regression_tests;
pub mod sigs;
mod stat;
#[cfg(test)]
mod testing;
mod text;

/// The category tables. Each file contributes one.
const TABLES: &[&[FuncDef]] = &[
    math::FUNCS,
    stat::FUNCS,
    logic::FUNCS,
    lookup::FUNCS,
    text::FUNCS,
    datefn::FUNCS,
    info::FUNCS,
    array::FUNCS,
];

// ---- helpers shared by the categories ---------------------------------------

/// How an aggregate treats non-numbers, following Excel: numbers given
/// directly (`SUM(1,"2",TRUE)` = 4) are coerced, but text and booleans
/// *inside ranges or arrays* are skipped (`SUM(A1:A3)` ignores a text cell).
/// Errors anywhere propagate. Returns the collected numbers.
pub fn numbers(args: &[Arg], ctx: &mut dyn Ctx) -> Result<Vec<f64>, ErrorKind> {
    let d1904 = ctx.date1904();
    args::fold_args(
        args,
        ctx,
        Vec::new(),
        |out, v| {
            out.push(v.to_number_in(d1904)?);
            Ok(())
        },
        |out, v| match v {
            Value::Number(n) => {
                out.push(*n);
                Ok(())
            }
            Value::Error(e) => Err(*e),
            _ => Ok(()),
        },
    )
}

/// A single value from an argument: a range must be one cell (or, for a
/// range in the formula's own row/column, Excel's implicit intersection —
/// handled by the evaluator before the call, so here a multi-cell range is
/// `#VALUE!`). An omitted argument is [`Value::Empty`].
pub fn scalar(arg: &Arg, ctx: &mut dyn Ctx) -> Value {
    match arg {
        Arg::Value(v) => v.clone(),
        Arg::Omitted => Value::Empty,
        Arg::Array(a) => a.data.first().cloned().unwrap_or(Value::Empty),
        // A multi-cell range in a one-value argument: implicit intersection
        // with the formula's row or column (`=ABS(A1:A3)` in row 2 is A2).
        Arg::Range(r) => super::eval::to_scalar(super::eval::Ev::R(r.clone()), ctx),
    }
}

/// [`scalar`] coerced to a number.
pub fn num(arg: &Arg, ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    let d1904 = ctx.date1904();
    scalar(arg, ctx).to_number_in(d1904)
}

/// [`scalar`] coerced to text.
pub fn text(arg: &Arg, ctx: &mut dyn Ctx) -> Result<Arc<str>, ErrorKind> {
    scalar(arg, ctx).to_text()
}

/// Excel's cell text limit; longer text results are `#VALUE!`.
pub const MAX_TEXT_LEN: usize = 32_767;

/// What a function body can return: anything that becomes one cell value.
/// The [`FuncDef`] tables wrap each body with [`IntoValue::into_value`], so
/// a body returns `Result<f64, ErrorKind>` (or `String`, `bool`, `Value`, …)
/// and uses `?` freely.
pub trait IntoValue {
    fn into_value(self) -> Value;
}

impl IntoValue for Value {
    fn into_value(self) -> Value {
        self
    }
}

/// A number; `#NUM!` when not finite.
impl IntoValue for f64 {
    fn into_value(self) -> Value {
        if self.is_finite() {
            Value::Number(self)
        } else {
            Value::Error(ErrorKind::Num)
        }
    }
}

impl IntoValue for bool {
    fn into_value(self) -> Value {
        Value::Bool(self)
    }
}

/// Text; `#VALUE!` past [`MAX_TEXT_LEN`] characters.
impl IntoValue for String {
    fn into_value(self) -> Value {
        if self.chars().count() > MAX_TEXT_LEN {
            Value::Error(ErrorKind::Value)
        } else {
            Value::Text(Arc::from(self))
        }
    }
}

/// Text; `#VALUE!` past [`MAX_TEXT_LEN`] characters.
impl IntoValue for Arc<str> {
    fn into_value(self) -> Value {
        if self.chars().count() > MAX_TEXT_LEN {
            Value::Error(ErrorKind::Value)
        } else {
            Value::Text(self)
        }
    }
}

impl<T: IntoValue> IntoValue for Result<T, ErrorKind> {
    fn into_value(self) -> Value {
        match self {
            Ok(v) => v.into_value(),
            Err(e) => Value::Error(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::Mock;
    use super::*;

    fn cell(r: usize, c: usize) -> Arg {
        Arg::Range(RangeRef {
            sheet: None,
            r0: r,
            c0: c,
            r1: r,
            c1: c,
        })
    }

    #[test]
    fn lookup_is_case_insensitive_and_skips_special_forms() {
        let sum = lookup("SUM").unwrap();
        assert!(std::ptr::eq(sum, lookup("sum").unwrap()));
        assert!(std::ptr::eq(sum, lookup("Sum").unwrap()));
        assert!(lookup("NO.SUCH.FUNCTION").is_none());
        assert!(lookup("if").is_none());
        assert_eq!(special("if"), Some((2, 3)));
        assert_eq!(special("Choose"), Some((2, usize::MAX)));
        assert_eq!(special("SUM"), None);
        assert_eq!(signature("iferror"), Some("value, value_if_error"));
        assert_eq!(signature("round"), Some(lookup("ROUND").unwrap().sig));
        assert_eq!(signature("NOPE"), None);
    }

    #[test]
    fn parser_checks_arity_against_the_registry() {
        use crate::formula::parse::{arity, parse};
        assert_eq!(arity("round"), Some((2, 2)));
        assert_eq!(arity("IF"), Some((2, 3)));
        assert_eq!(arity("MYFN"), None);
        for (src, msg) in [
            ("=ROUND(1)", "ROUND takes 2 arguments, not 1"),
            ("=NOW(1)", "NOW takes 0 arguments, not 1"),
            ("=LEFT()", "LEFT takes 1 to 2 arguments, not 0"),
            ("=SUM()", "SUM takes at least 1 argument, not 0"),
            ("=IF(1)", "IF takes 2 to 3 arguments, not 1"),
        ] {
            let err = format!("{:?}", parse(src).expect_err(src));
            assert!(err.contains(msg), "{src}: {err}");
        }
        // Unknown names parse (a Scheme function may supply them later).
        assert!(parse("=MYFN(1, 2, 3)").is_ok());
        assert!(parse("=ROUND(1, )").is_ok());
    }

    #[test]
    fn scalar_reads_one_value_from_any_argument() {
        let mut m = Mock::grid(&[&["5"], &["'x"]]);
        assert_eq!(scalar(&Arg::Omitted, &mut m), Value::Empty);
        assert_eq!(scalar(&cell(0, 0), &mut m), Value::Number(5.0));
        assert_eq!(scalar(&cell(7, 7), &mut m), Value::Empty);
        let arr = Arg::Array(Array {
            rows: 1,
            cols: 2,
            data: vec![Value::text("first"), Value::Number(2.0)],
        });
        assert_eq!(scalar(&arr, &mut m), Value::text("first"));
        let empty = Arg::Array(Array {
            rows: 0,
            cols: 0,
            data: vec![],
        });
        assert_eq!(scalar(&empty, &mut m), Value::Empty);

        assert_eq!(num(&cell(0, 0), &mut m), Ok(5.0));
        assert_eq!(num(&cell(1, 0), &mut m), Err(ErrorKind::Value));
        assert_eq!(num(&Arg::Value(Value::text(" 12 ")), &mut m), Ok(12.0));
        assert_eq!(
            text(&Arg::Value(Value::Number(1.5)), &mut m).as_deref(),
            Ok("1.5")
        );
        assert_eq!(
            text(&Arg::Value(Value::Bool(false)), &mut m).as_deref(),
            Ok("FALSE")
        );
        assert_eq!(text(&Arg::Omitted, &mut m).as_deref(), Ok(""));
        assert_eq!(
            text(&Arg::Value(Value::Error(ErrorKind::NA)), &mut m),
            Err(ErrorKind::NA)
        );
    }

    #[test]
    fn numbers_coerces_direct_values_and_skips_others_in_ranges() {
        // A1 TRUE, A2 "7" typed as text, A3 4.
        let mut m = Mock::grid(&[&["TRUE"], &["'7"], &["4"]]);
        let range = Arg::Range(RangeRef {
            sheet: None,
            r0: 0,
            c0: 0,
            r1: 2,
            c1: 0,
        });
        assert_eq!(
            numbers(
                &[
                    Arg::Value(Value::Bool(true)),
                    Arg::Value(Value::text("7")),
                    range.clone()
                ],
                &mut m
            ),
            Ok(vec![1.0, 7.0, 4.0])
        );
        assert_eq!(
            numbers(&[Arg::Value(Value::text("x")), range.clone()], &mut m),
            Err(ErrorKind::Value)
        );
        m.put(None, 3, 0, "#N/A");
        let whole = Arg::Range(RangeRef {
            sheet: None,
            r0: 0,
            c0: 0,
            r1: usize::MAX,
            c1: 0,
        });
        assert_eq!(numbers(&[whole], &mut m), Err(ErrorKind::NA));
        // Date text given directly follows the workbook's date system.
        let date = [Arg::Value(Value::text("2024-01-01"))];
        assert_eq!(numbers(&date, &mut m), Ok(vec![45292.0]));
        m.d1904 = true;
        assert_eq!(numbers(&date, &mut m), Ok(vec![45292.0 - 1462.0]));
    }

    #[test]
    fn into_value_maps_results_to_cells() {
        assert_eq!(2.5.into_value(), Value::Number(2.5));
        assert_eq!(f64::NAN.into_value(), Value::Error(ErrorKind::Num));
        assert_eq!(f64::INFINITY.into_value(), Value::Error(ErrorKind::Num));
        assert_eq!(true.into_value(), Value::Bool(true));
        assert_eq!("hi".to_string().into_value(), Value::text("hi"));
        let max = "é".repeat(MAX_TEXT_LEN);
        assert_eq!(max.clone().into_value(), Value::text(&max));
        let long = max + "x";
        assert_eq!(long.clone().into_value(), Value::Error(ErrorKind::Value));
        assert_eq!(
            Arc::<str>::from(long).into_value(),
            Value::Error(ErrorKind::Value)
        );
        let err: Result<f64, ErrorKind> = Err(ErrorKind::Div0);
        assert_eq!(err.into_value(), Value::Error(ErrorKind::Div0));
        let ok: Result<String, ErrorKind> = Ok("x".into());
        assert_eq!(ok.into_value(), Value::text("x"));
    }

    #[test]
    fn range_ref_dimensions_saturate_on_open_ends() {
        let r = RangeRef {
            sheet: None,
            r0: 2,
            c0: 1,
            r1: 4,
            c1: 1,
        };
        assert_eq!((r.rows(), r.cols(), r.is_cell()), (3, 1, false));
        let col = RangeRef {
            r0: 0,
            r1: usize::MAX,
            ..r.clone()
        };
        assert_eq!(col.rows(), usize::MAX);
        let one = RangeRef { r1: 2, ..r };
        assert!(one.is_cell());
    }

    #[test]
    fn ctx_defaults_clamp_and_trim_to_the_used_area() {
        let m = Mock::grid(&[&["1", "2"], &["3", "4"]]);
        let whole_col = RangeRef {
            sheet: None,
            r0: 0,
            c0: 1,
            r1: usize::MAX,
            c1: 1,
        };
        assert_eq!(m.clamp(&whole_col).map(|r| r.r1), Some(1));
        let explicit = RangeRef {
            r1: 50,
            ..whole_col.clone()
        };
        assert_eq!(m.clamp(&explicit).map(|r| r.r1), Some(50));
        assert_eq!(m.data_part(&explicit).map(|r| r.r1), Some(1));
        let below = RangeRef { r0: 5, ..explicit };
        assert_eq!(m.data_part(&below), None);
        let other = RangeRef {
            sheet: Some("Missing".into()),
            ..whole_col
        };
        assert_eq!(m.clamp(&other), None);
        assert_eq!(m.data_part(&other), None);
    }
}
