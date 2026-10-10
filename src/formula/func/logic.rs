//! Logical functions. `IF`, `IFS`, `IFERROR`, `IFNA`, `SWITCH` (and
//! `CHOOSE`) short-circuit, so the evaluator implements them itself.

use super::args::{MANY, area, def};
use super::{Arg, Ctx, FuncDef, scalar};
use crate::formula::value::{ErrorKind, Value};

pub(super) const FUNCS: &[FuncDef] = &[
    def("AND", 1, MANY, and),
    def("OR", 1, MANY, or),
    def("XOR", 1, MANY, xor),
    def("NOT", 1, 1, not),
    def("TRUE", 0, 0, true_),
    def("FALSE", 0, 0, false_),
];

/// The logical values of the arguments, as Excel's `AND`/`OR`/`XOR` read
/// them: values given directly are coerced (`"TRUE"` text works, other text
/// is `#VALUE!`); inside ranges and arrays, numbers and booleans count while
/// text and empty cells are skipped. Errors propagate. No values at all is
/// `#VALUE!`.
fn logicals(args: &[Arg], ctx: &mut dyn Ctx) -> Result<Vec<bool>, ErrorKind> {
    let mut out = Vec::new();
    for arg in args {
        match arg {
            Arg::Omitted | Arg::Value(Value::Empty) => {}
            Arg::Value(v) => out.push(v.to_bool()?),
            _ => {
                let ar = area(arg, ctx)?;
                let (r, c) = ar.data_dims(ctx);
                for v in ar.window(ctx, r, c) {
                    match v {
                        Value::Bool(b) => out.push(b),
                        Value::Number(n) => out.push(n != 0.0),
                        Value::Error(e) => return Err(e),
                        Value::Text(_) | Value::Empty => {}
                    }
                }
            }
        }
    }
    if out.is_empty() {
        return Err(ErrorKind::Value);
    }
    Ok(out)
}

fn logical(args: &[Arg], ctx: &mut dyn Ctx, f: fn(&[bool]) -> bool) -> Value {
    match logicals(args, ctx) {
        Ok(bs) => Value::Bool(f(&bs)),
        Err(e) => Value::Error(e),
    }
}

fn and(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    logical(args, ctx, |bs| bs.iter().all(|b| *b))
}

fn or(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    logical(args, ctx, |bs| bs.iter().any(|b| *b))
}

fn xor(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    logical(args, ctx, |bs| bs.iter().filter(|b| **b).count() % 2 == 1)
}

fn not(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    match scalar(&args[0], ctx).to_bool() {
        Ok(b) => Value::Bool(!b),
        Err(e) => Value::Error(e),
    }
}

fn true_(_: &[Arg], _: &mut dyn Ctx) -> Value {
    Value::Bool(true)
}

fn false_(_: &[Arg], _: &mut dyn Ctx) -> Value {
    Value::Bool(false)
}

#[cfg(test)]
mod tests {
    use super::super::testing::{Mock, check};

    #[test]
    fn logic() {
        let mut m = Mock::grid(&[
            &["TRUE", "x"],
            &["1", ""],
            &["0", "#N/A"],
            &["'TRUE", "FALSE"],
        ]);
        check(
            &mut m,
            &[
                ("AND(TRUE, TRUE)", "TRUE"),
                ("AND(TRUE, FALSE)", "FALSE"),
                ("AND(2, 1)", "TRUE"),
                ("AND(\"true\", 1)", "TRUE"),
                ("AND(\"x\")", "#VALUE!"),
                ("AND(A1:A2)", "TRUE"),
                ("AND(A1:A3)", "FALSE"),
                ("AND(B1:B2)", "#VALUE!"),
                ("AND(B1:B4)", "#N/A"),
                ("AND(A4)", "#VALUE!"),
                ("AND({1,\"x\",TRUE})", "TRUE"),
                ("OR(TRUE)", "TRUE"),
                ("OR(FALSE, FALSE)", "FALSE"),
                ("OR(0, 0)", "FALSE"),
                ("OR(A3, B4)", "FALSE"),
                ("OR(A1:A3)", "TRUE"),
                ("OR(B1:B2)", "#VALUE!"),
                ("OR(#DIV/0!, TRUE)", "#DIV/0!"),
                ("XOR(TRUE, 1)", "FALSE"),
                ("XOR(TRUE, FALSE)", "TRUE"),
                ("XOR(TRUE, TRUE, TRUE)", "TRUE"),
                ("XOR(A1:A3)", "FALSE"),
                ("XOR(B2)", "#VALUE!"),
                ("NOT(FALSE)", "TRUE"),
                ("NOT(1)", "FALSE"),
                ("NOT(0)", "TRUE"),
                ("NOT(B2)", "TRUE"),
                ("NOT(\"x\")", "#VALUE!"),
                ("NOT(#N/A)", "#N/A"),
                ("TRUE()", "TRUE"),
                ("FALSE()", "FALSE"),
            ],
        );
    }
}
