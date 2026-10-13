//! Information functions: `IS…`, `NA`, `ERROR.TYPE`, `TYPE`, `N`, `T`.

use super::args::def;
use super::{Arg, Ctx, FuncDef, scalar};
use crate::formula::value::{ErrorKind, Value};

pub(super) const FUNCS: &[FuncDef] = &[
    def!("ISBLANK", 1, 1, isblank),
    def!("ISNUMBER", 1, 1, isnumber),
    def!("ISTEXT", 1, 1, istext),
    def!("ISNONTEXT", 1, 1, isnontext),
    def!("ISLOGICAL", 1, 1, islogical),
    def!("ISERROR", 1, 1, iserror),
    def!("ISERR", 1, 1, iserr),
    def!("ISNA", 1, 1, isna),
    def!("ISEVEN", 1, 1, iseven),
    def!("ISODD", 1, 1, isodd),
    def!("NA", 0, 0, na),
    def!("ERROR.TYPE", 1, 1, error_type),
    def!("TYPE", 1, 1, type_),
    def!("N", 1, 1, n),
    def!("T", 1, 1, t),
];

fn is(args: &[Arg], ctx: &mut dyn Ctx, f: fn(&Value) -> bool) -> bool {
    f(&scalar(&args[0], ctx))
}

/// Only a truly empty cell is blank; a formula's `""` is text.
fn isblank(args: &[Arg], ctx: &mut dyn Ctx) -> bool {
    is(args, ctx, |v| matches!(v, Value::Empty))
}

fn isnumber(args: &[Arg], ctx: &mut dyn Ctx) -> bool {
    is(args, ctx, |v| matches!(v, Value::Number(_)))
}

fn istext(args: &[Arg], ctx: &mut dyn Ctx) -> bool {
    is(args, ctx, |v| matches!(v, Value::Text(_)))
}

fn isnontext(args: &[Arg], ctx: &mut dyn Ctx) -> bool {
    is(args, ctx, |v| !matches!(v, Value::Text(_)))
}

fn islogical(args: &[Arg], ctx: &mut dyn Ctx) -> bool {
    is(args, ctx, |v| matches!(v, Value::Bool(_)))
}

fn iserror(args: &[Arg], ctx: &mut dyn Ctx) -> bool {
    is(args, ctx, |v| matches!(v, Value::Error(_)))
}

fn iserr(args: &[Arg], ctx: &mut dyn Ctx) -> bool {
    is(
        args,
        ctx,
        |v| matches!(v, Value::Error(e) if *e != ErrorKind::NA),
    )
}

fn isna(args: &[Arg], ctx: &mut dyn Ctx) -> bool {
    is(args, ctx, |v| matches!(v, Value::Error(ErrorKind::NA)))
}

/// ISEVEN/ISODD: the number is truncated; booleans and non-numeric text are
/// `#VALUE!` (as in Excel); an empty cell is 0.
fn parity(args: &[Arg], ctx: &mut dyn Ctx, odd: bool) -> Result<bool, ErrorKind> {
    let n = match scalar(&args[0], ctx) {
        Value::Bool(_) => return Err(ErrorKind::Value),
        v => v.to_number()?,
    };
    let is_odd = (n.trunc() % 2.0).abs() == 1.0;
    Ok(is_odd == odd)
}

fn iseven(args: &[Arg], ctx: &mut dyn Ctx) -> Result<bool, ErrorKind> {
    parity(args, ctx, false)
}

fn isodd(args: &[Arg], ctx: &mut dyn Ctx) -> Result<bool, ErrorKind> {
    parity(args, ctx, true)
}

fn na(_: &[Arg], _: &mut dyn Ctx) -> Value {
    Value::Error(ErrorKind::NA)
}

/// Excel's error numbers. This engine's own errors get numbers past
/// Excel's: `#ERROR!` (parse) 101, `#CIRC!` 102; `#CALC!` is Excel's 14.
fn error_type(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    match scalar(&args[0], ctx) {
        Value::Error(e) => Value::Number(match e {
            ErrorKind::Null => 1.0,
            ErrorKind::Div0 => 2.0,
            ErrorKind::Value => 3.0,
            ErrorKind::Ref => 4.0,
            ErrorKind::Name => 5.0,
            ErrorKind::Num => 6.0,
            ErrorKind::NA => 7.0,
            ErrorKind::Calc => 14.0,
            ErrorKind::Parse => 101.0,
            ErrorKind::Circ => 102.0,
        }),
        _ => Value::Error(ErrorKind::NA),
    }
}

/// 1 number (and empty), 2 text, 4 boolean, 16 error, 64 array (an array or
/// a multi-cell range).
fn type_(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    let code = match &args[0] {
        Arg::Array(_) => 64.0,
        Arg::Range(r) if !r.is_cell() => 64.0,
        a => match scalar(a, ctx) {
            Value::Number(_) | Value::Empty => 1.0,
            Value::Text(_) => 2.0,
            Value::Bool(_) => 4.0,
            Value::Error(_) => 16.0,
        },
    };
    Value::Number(code)
}

/// The argument's value, or for a multi-cell range its top-left cell.
fn first(arg: &Arg, ctx: &mut dyn Ctx) -> Value {
    match arg {
        Arg::Range(r) => ctx.cell(r.sheet.as_deref(), r.r0, r.c0),
        a => scalar(a, ctx),
    }
}

fn n(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    match first(&args[0], ctx) {
        Value::Number(n) => Value::Number(n),
        Value::Bool(b) => Value::Number(if b { 1.0 } else { 0.0 }),
        Value::Error(e) => Value::Error(e),
        Value::Text(_) | Value::Empty => Value::Number(0.0),
    }
}

fn t(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    match first(&args[0], ctx) {
        Value::Text(s) => Value::Text(s),
        Value::Error(e) => Value::Error(e),
        _ => Value::text(""),
    }
}

#[cfg(test)]
mod tests {
    use super::super::testing::{Mock, check};

    #[test]
    fn info() {
        let mut m = Mock::grid(&[
            &["Gold", "330.92"],
            &["", "#REF!"],
            &["'", "#N/A"],
            &["TRUE", "'19"],
            &["7", "-2.5"],
        ]);
        check(
            &mut m,
            &[
                ("ISBLANK(A2)", "TRUE"),
                ("ISBLANK(A1)", "FALSE"),
                ("ISBLANK(A3)", "FALSE"),
                ("ISBLANK(0)", "FALSE"),
                ("ISBLANK(\"\")", "FALSE"),
                ("ISNUMBER(B1)", "TRUE"),
                ("ISNUMBER(B4)", "FALSE"),
                ("ISNUMBER(A2)", "FALSE"),
                ("ISNUMBER(TRUE)", "FALSE"),
                ("ISNUMBER(#N/A)", "FALSE"),
                ("ISTEXT(A1)", "TRUE"),
                ("ISTEXT(A3)", "TRUE"),
                ("ISTEXT(B4)", "TRUE"),
                ("ISTEXT(B1)", "FALSE"),
                ("ISTEXT(A2)", "FALSE"),
                ("ISNONTEXT(A1)", "FALSE"),
                ("ISNONTEXT(A2)", "TRUE"),
                ("ISNONTEXT(B2)", "TRUE"),
                ("ISNONTEXT(19)", "TRUE"),
                ("ISNONTEXT(\"19\")", "FALSE"),
                ("ISLOGICAL(TRUE)", "TRUE"),
                ("ISLOGICAL(A4)", "TRUE"),
                ("ISLOGICAL(\"TRUE\")", "FALSE"),
                ("ISLOGICAL(1)", "FALSE"),
                ("ISLOGICAL(A2)", "FALSE"),
                ("ISERROR(B2)", "TRUE"),
                ("ISERROR(B3)", "TRUE"),
                ("ISERROR(B1)", "FALSE"),
                ("ISERROR(A1:A2)", "TRUE"),
                ("ISERROR(\"#N/A\")", "FALSE"),
                ("ISERR(B2)", "TRUE"),
                ("ISERR(B3)", "FALSE"),
                ("ISERR(#DIV/0!)", "TRUE"),
                ("ISERR(1)", "FALSE"),
                ("ISERR(A2)", "FALSE"),
                ("ISNA(B3)", "TRUE"),
                ("ISNA(B2)", "FALSE"),
                ("ISNA(#N/A)", "TRUE"),
                ("ISNA(NA())", "TRUE"),
                ("ISNA(1)", "FALSE"),
                ("ISEVEN(-1)", "FALSE"),
                ("ISEVEN(2.5)", "TRUE"),
                ("ISEVEN(5)", "FALSE"),
                ("ISEVEN(0)", "TRUE"),
                ("ISEVEN(A2)", "TRUE"),
                ("ISEVEN(TRUE)", "#VALUE!"),
                ("ISEVEN(\"x\")", "#VALUE!"),
                ("ISODD(-1)", "TRUE"),
                ("ISODD(2)", "FALSE"),
                ("ISODD(-3.7)", "TRUE"),
                ("ISODD(\"3\")", "TRUE"),
                ("ISODD(#N/A)", "#N/A"),
                ("NA()", "#N/A"),
                ("ERROR.TYPE(#NULL!)", "1"),
                ("ERROR.TYPE(#DIV/0!)", "2"),
                ("ERROR.TYPE(B2)", "4"),
                ("ERROR.TYPE(#N/A)", "7"),
                ("ERROR.TYPE(1)", "#N/A"),
                ("ERROR.TYPE(#NUM!)", "6"),
                ("TYPE(A1)", "2"),
                ("TYPE(B1)", "1"),
                ("TYPE(A2)", "1"),
                ("TYPE(TRUE)", "4"),
                ("TYPE(#N/A)", "16"),
                ("TYPE({1,2})", "64"),
                ("TYPE(A1:B2)", "64"),
                ("N(B1)", "330.92"),
                ("N(A1)", "0"),
                ("N(A4)", "1"),
                ("N(B2)", "#REF!"),
                ("N(\"7\")", "0"),
                ("N(A5:B5)", "7"),
                ("T(A1)", "\"Gold\""),
                ("T(B1)", "\"\""),
                ("T(A4)", "\"\""),
                ("T(B3)", "#N/A"),
                ("T(\"Rainfall\")", "\"Rainfall\""),
            ],
        );
    }
}
