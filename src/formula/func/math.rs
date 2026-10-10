//! Math functions.

use super::args::{MANY, area, def, opt_int, opt_num, trunc_i64, volatile};
use super::{Arg, Ctx, FuncDef, num, numbers, ok};
use crate::formula::decimal::{self, Mode, to_15_digits};
use crate::formula::value::{ErrorKind, Value};

pub(super) const FUNCS: &[FuncDef] = &[
    def("SUM", 1, MANY, sum),
    def("PRODUCT", 1, MANY, product),
    def("ABS", 1, 1, abs),
    def("ROUND", 2, 2, round),
    def("ROUNDUP", 2, 2, roundup),
    def("ROUNDDOWN", 2, 2, rounddown),
    def("INT", 1, 1, int),
    def("TRUNC", 1, 2, trunc),
    def("MOD", 2, 2, modulo),
    def("POWER", 2, 2, power),
    def("SQRT", 1, 1, sqrt),
    def("EXP", 1, 1, exp),
    def("LN", 1, 1, ln),
    def("LOG", 1, 2, log),
    def("LOG10", 1, 1, log10),
    def("CEILING", 2, 2, ceiling),
    def("CEILING.MATH", 1, 3, ceiling_math),
    def("FLOOR", 2, 2, floor),
    def("FLOOR.MATH", 1, 3, floor_math),
    def("SIGN", 1, 1, sign),
    def("PI", 0, 0, pi),
    volatile("RAND", 0, 0, rand),
    volatile("RANDBETWEEN", 2, 2, randbetween),
    def("SUMPRODUCT", 1, MANY, sumproduct),
    def("QUOTIENT", 2, 2, quotient),
    def("GCD", 1, MANY, gcd),
    def("LCM", 1, MANY, lcm),
    def("FACT", 1, 1, fact),
    def("EVEN", 1, 1, even),
    def("ODD", 1, 1, odd),
    def("MROUND", 2, 2, mround),
];

fn unary(args: &[Arg], ctx: &mut dyn Ctx, f: impl Fn(f64) -> Result<f64, ErrorKind>) -> Value {
    ok(num(&args[0], ctx).and_then(f))
}

fn binary(
    args: &[Arg],
    ctx: &mut dyn Ctx,
    f: impl Fn(f64, f64) -> Result<f64, ErrorKind>,
) -> Value {
    ok((|| f(num(&args[0], ctx)?, num(&args[1], ctx)?))())
}

fn sum(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    ok(numbers(args, ctx).map(|ns| ns.iter().sum()))
}

fn product(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    ok(numbers(args, ctx).map(|ns| {
        if ns.is_empty() {
            0.0
        } else {
            ns.iter().product()
        }
    }))
}

fn abs(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    unary(args, ctx, |x| Ok(x.abs()))
}

fn round_with(args: &[Arg], ctx: &mut dyn Ctx, mode: Mode) -> Value {
    ok((|| {
        let x = num(&args[0], ctx)?;
        // num_digits is truncated, as Excel does.
        let d = trunc_i64(num(&args[1], ctx)?)?.clamp(-400, 400) as i32;
        Ok(decimal::round(x, d, mode))
    })())
}

fn round(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    round_with(args, ctx, Mode::Half)
}

fn roundup(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    round_with(args, ctx, Mode::Up)
}

fn rounddown(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    round_with(args, ctx, Mode::Down)
}

fn int(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    unary(args, ctx, |x| Ok(x.floor()))
}

fn trunc(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    ok((|| {
        let x = num(&args[0], ctx)?;
        let d = opt_int(args.get(1), ctx, 0)?.clamp(-400, 400) as i32;
        Ok(decimal::round(x, d, Mode::Down))
    })())
}

/// Excel's MOD: the result takes the divisor's sign (`MOD(-3,2)` is 1,
/// `MOD(3,-2)` is -1).
pub fn excel_mod(n: f64, d: f64) -> Result<f64, ErrorKind> {
    if d == 0.0 {
        return Err(ErrorKind::Div0);
    }
    let mut r = n - d * (n / d).floor();
    if r != 0.0 && (r < 0.0) != (d < 0.0) {
        r += d;
    }
    if r.abs() >= d.abs() {
        r = 0.0;
    }
    Ok(r)
}

fn modulo(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    binary(args, ctx, excel_mod)
}

pub fn excel_power(x: f64, y: f64) -> Result<f64, ErrorKind> {
    if x == 0.0 {
        return match y {
            0.0 => Err(ErrorKind::Num),
            y if y < 0.0 => Err(ErrorKind::Div0),
            _ => Ok(0.0),
        };
    }
    if x < 0.0 && y.fract() != 0.0 {
        return Err(ErrorKind::Num);
    }
    let r = x.powf(y);
    if r.is_finite() {
        Ok(r)
    } else {
        Err(ErrorKind::Num)
    }
}

fn power(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    binary(args, ctx, excel_power)
}

fn sqrt(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    unary(args, ctx, |x| {
        if x < 0.0 {
            Err(ErrorKind::Num)
        } else {
            Ok(x.sqrt())
        }
    })
}

fn exp(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    unary(args, ctx, |x| Ok(x.exp()))
}

fn ln(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    unary(args, ctx, |x| {
        if x <= 0.0 {
            Err(ErrorKind::Num)
        } else {
            Ok(x.ln())
        }
    })
}

fn log(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    ok((|| {
        let x = num(&args[0], ctx)?;
        let base = opt_num(args.get(1), ctx, 10.0)?;
        if x <= 0.0 || base <= 0.0 {
            return Err(ErrorKind::Num);
        }
        if base == 1.0 {
            return Err(ErrorKind::Div0);
        }
        Ok(if base == 10.0 {
            x.log10()
        } else if base == 2.0 {
            x.log2()
        } else {
            // ln/ln leaves noise (LOG(8,2) would be 3.0000000000000004).
            to_15_digits(x.ln() / base.ln())
        })
    })())
}

fn log10(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    unary(args, ctx, |x| {
        if x <= 0.0 {
            Err(ErrorKind::Num)
        } else {
            Ok(x.log10())
        }
    })
}

/// `x / sig` at 15 digits, so `1.5 / 0.1` counts as exactly 15.
fn quotient_15(x: f64, sig: f64) -> f64 {
    to_15_digits(x / sig)
}

fn ceiling(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    binary(args, ctx, |x, sig| {
        if x == 0.0 || sig == 0.0 {
            return Ok(0.0);
        }
        if x > 0.0 && sig < 0.0 {
            return Err(ErrorKind::Num);
        }
        Ok(to_15_digits(quotient_15(x, sig).ceil() * sig))
    })
}

fn floor(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    binary(args, ctx, |x, sig| {
        if sig == 0.0 {
            return if x == 0.0 {
                Ok(0.0)
            } else {
                Err(ErrorKind::Div0)
            };
        }
        if x > 0.0 && sig < 0.0 {
            return Err(ErrorKind::Num);
        }
        Ok(to_15_digits(quotient_15(x, sig).floor() * sig))
    })
}

fn ceiling_math(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    round_math(args, ctx, true)
}

fn floor_math(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    round_math(args, ctx, false)
}

/// CEILING.MATH (`up`) / FLOOR.MATH: to a multiple of the significance,
/// up or down; a non-zero mode turns the direction around for negatives.
fn round_math(args: &[Arg], ctx: &mut dyn Ctx, up: bool) -> Value {
    ok((|| {
        let x = num(&args[0], ctx)?;
        let sig = opt_num(args.get(1), ctx, 1.0)?.abs();
        let mode = opt_num(args.get(2), ctx, 0.0)?;
        if sig == 0.0 {
            return Ok(0.0);
        }
        let q = quotient_15(x, sig);
        let k = if (x < 0.0 && mode != 0.0) == up {
            q.floor()
        } else {
            q.ceil()
        };
        Ok(to_15_digits(k * sig))
    })())
}

fn sign(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    unary(args, ctx, |x| {
        Ok(if x > 0.0 {
            1.0
        } else if x < 0.0 {
            -1.0
        } else {
            0.0
        })
    })
}

fn pi(_: &[Arg], _: &mut dyn Ctx) -> Value {
    Value::Number(std::f64::consts::PI)
}

fn rand(_: &[Arg], ctx: &mut dyn Ctx) -> Value {
    Value::Number(ctx.random())
}

fn randbetween(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    ok((|| {
        let lo = num(&args[0], ctx)?.ceil();
        let hi = num(&args[1], ctx)?.floor();
        if lo > hi {
            return Err(ErrorKind::Num);
        }
        let r = ctx.random();
        Ok((lo + (r * (hi - lo + 1.0)).floor()).min(hi))
    })())
}

fn sumproduct(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    ok((|| {
        let mut acc: Option<Vec<f64>> = None;
        let mut dims = None;
        for a in args {
            let ar = area(a, ctx)?;
            let d = (ar.rows(), ar.cols());
            if *dims.get_or_insert(d) != d {
                return Err(ErrorKind::Value);
            }
            let vals = ar.values(ctx);
            let mut nums = Vec::with_capacity(vals.len());
            for v in vals {
                nums.push(match v {
                    Value::Number(n) => n,
                    Value::Error(e) => return Err(e),
                    _ => 0.0,
                });
            }
            acc = Some(match acc {
                None => nums,
                Some(mut p) => {
                    p.iter_mut().zip(&nums).for_each(|(x, y)| *x *= y);
                    p
                }
            });
        }
        Ok(acc.unwrap_or_default().iter().sum())
    })())
}

fn quotient(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    binary(args, ctx, |n, d| {
        if d == 0.0 {
            Err(ErrorKind::Div0)
        } else {
            Ok((n / d).trunc())
        }
    })
}

const MAX_EXACT: f64 = 9_007_199_254_740_992.0; // 2^53

fn integers(args: &[Arg], ctx: &mut dyn Ctx) -> Result<Vec<u64>, ErrorKind> {
    numbers(args, ctx)?
        .into_iter()
        .map(|x| {
            if x < 0.0 || x >= MAX_EXACT {
                Err(ErrorKind::Num)
            } else {
                Ok(x.trunc() as u64)
            }
        })
        .collect()
}

fn gcd2(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

fn gcd(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    ok(integers(args, ctx).map(|ns| ns.into_iter().fold(0, gcd2) as f64))
}

fn lcm(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    ok((|| {
        let mut acc: u128 = 1;
        for n in integers(args, ctx)? {
            if n == 0 {
                return Ok(0.0);
            }
            let n = n as u128;
            acc = acc / gcd2(acc as u64, n as u64) as u128 * n;
            if acc as f64 >= MAX_EXACT {
                return Err(ErrorKind::Num);
            }
        }
        Ok(acc as f64)
    })())
}

fn fact(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    unary(args, ctx, |x| {
        if x < 0.0 {
            return Err(ErrorKind::Num);
        }
        let n = x.trunc() as u64;
        if n > 170 {
            return Err(ErrorKind::Num);
        }
        Ok((1..=n).fold(1.0, |acc, k| acc * k as f64))
    })
}

fn even(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    unary(args, ctx, |x| Ok(away_to_parity(x, false)))
}

fn odd(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    unary(args, ctx, |x| Ok(away_to_parity(x, true)))
}

/// `x` rounded away from zero to the nearest even (or `odd`) integer.
fn away_to_parity(x: f64, odd: bool) -> f64 {
    let a = to_15_digits(x.abs()).ceil();
    let a = if a % 2.0 == f64::from(u8::from(!odd)) {
        a + 1.0
    } else {
        a
    };
    if x < 0.0 { -a } else { a }
}

fn mround(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    binary(args, ctx, |n, m| {
        if m == 0.0 || n == 0.0 {
            return Ok(0.0);
        }
        if (n < 0.0) != (m < 0.0) {
            return Err(ErrorKind::Num);
        }
        Ok(to_15_digits(quotient_15(n, m).round() * m))
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{Mock, check, check0, eval};
    use crate::formula::value::Value;

    #[test]
    fn sum_product_abs() {
        let mut m = Mock::grid(&[
            &["5", "x"],
            &["15", "TRUE"],
            &["30", ""],
            &["'40", "#DIV/0!"],
        ]);
        check(
            &mut m,
            &[
                ("SUM(3, 2)", "5"),
                ("SUM(\"5\", 15, TRUE)", "21"),
                ("SUM(A1:A4)", "50"),
                ("SUM(A1:A3, 15)", "65"),
                ("SUM(A:A)", "50"),
                ("SUM(B1:B3)", "0"),
                ("SUM(B1:B4)", "#DIV/0!"),
                ("SUM(\"x\")", "#VALUE!"),
                ("SUM({1,2;3,\"4\"})", "6"),
                ("SUM(Nope!A1)", "#REF!"),
                ("PRODUCT(A1:A3)", "2250"),
                ("PRODUCT(A1:A3, 2)", "4500"),
                ("PRODUCT(B1:B3)", "0"),
                ("PRODUCT(2, \"3\")", "6"),
                ("PRODUCT(1, #N/A)", "#N/A"),
                ("ABS(2)", "2"),
                ("ABS(-2)", "2"),
                ("ABS(-4.5)", "4.5"),
                ("ABS(\"x\")", "#VALUE!"),
                ("ABS(B1)", "#VALUE!"),
                ("ABS(C1)", "0"),
            ],
        );
    }

    #[test]
    fn rounding() {
        check0(&[
            ("ROUND(2.15, 1)", "2.2"),
            ("ROUND(2.149, 1)", "2.1"),
            ("ROUND(-1.475, 2)", "-1.48"),
            ("ROUND(21.5, -1)", "20"),
            ("ROUND(626.3, -3)", "1000"),
            ("ROUND(1.98, -1)", "0"),
            ("ROUND(-50.55, -2)", "-100"),
            ("ROUND(2.675, 2)", "2.68"),
            ("ROUND(-2.5, 0)", "-3"),
            ("ROUND(2.5, 0.9)", "3"),
            ("ROUND(\"a\", 1)", "#VALUE!"),
            ("ROUNDUP(3.2, 0)", "4"),
            ("ROUNDUP(76.9, 0)", "77"),
            ("ROUNDUP(3.14159, 3)", "3.142"),
            ("ROUNDUP(3.14159, 2)", "3.15"),
            ("ROUNDUP(-3.14159, 1)", "-3.2"),
            ("ROUNDUP(31415.92654, -2)", "31500"),
            ("ROUNDDOWN(3.2, 0)", "3"),
            ("ROUNDDOWN(76.9, 0)", "76"),
            ("ROUNDDOWN(3.14159, 3)", "3.141"),
            ("ROUNDDOWN(-3.14159, 1)", "-3.1"),
            ("ROUNDDOWN(31415.92654, -2)", "31400"),
            ("ROUNDDOWN(#N/A, 1)", "#N/A"),
            ("INT(8.9)", "8"),
            ("INT(-8.9)", "-9"),
            ("INT(-1.5)", "-2"),
            ("INT(5)", "5"),
            ("INT(\"x\")", "#VALUE!"),
            ("TRUNC(8.9)", "8"),
            ("TRUNC(-8.9)", "-8"),
            ("TRUNC(-1.5)", "-1"),
            ("TRUNC(0.45)", "0"),
            ("TRUNC(3.14159, 2)", "3.14"),
            ("TRUNC(1234.5, -2)", "1200"),
            ("TRUNC(#DIV/0!)", "#DIV/0!"),
        ]);
        // Integral results are exact.
        let mut m = Mock::new();
        assert_eq!(eval(&mut m, "ROUND(-2.5, 0)"), Value::Number(-3.0));
        assert_eq!(eval(&mut m, "ROUND(2.675, 2)"), Value::Number(2.68));
    }

    #[test]
    fn modulo_power_roots() {
        check0(&[
            ("MOD(3, 2)", "1"),
            ("MOD(-3, 2)", "1"),
            ("MOD(3, -2)", "-1"),
            ("MOD(-3, -2)", "-1"),
            ("MOD(10, 0)", "#DIV/0!"),
            ("MOD(5.5, 2)", "1.5"),
            ("MOD(0, 3)", "0"),
            ("POWER(5, 2)", "25"),
            ("POWER(98.6, 3.2)", "2401077.22206958"),
            ("POWER(4, 1.25)", "5.65685424949238"),
            ("POWER(0, 0)", "#NUM!"),
            ("POWER(0, -1)", "#DIV/0!"),
            ("POWER(-8, 0.5)", "#NUM!"),
            ("POWER(-2, 3)", "-8"),
            ("SQRT(16)", "4"),
            ("SQRT(-16)", "#NUM!"),
            ("SQRT(ABS(-16))", "4"),
            ("SQRT(2)", "1.4142135623731"),
            ("SQRT(\"x\")", "#VALUE!"),
            ("EXP(1)", "2.71828182845905"),
            ("EXP(2)", "7.38905609893065"),
            ("EXP(0)", "1"),
            ("EXP(1000)", "#NUM!"),
            ("EXP(\"x\")", "#VALUE!"),
            ("LN(86)", "4.45434729625351"),
            ("LN(2.7182818)", "0.999999989530502"),
            ("LN(EXP(3))", "3"),
            ("LN(0)", "#NUM!"),
            ("LN(-1)", "#NUM!"),
            ("LOG(10)", "1"),
            ("LOG(8, 2)", "3"),
            ("LOG(86, 2.7182818)", "4.45434734288829"),
            ("LOG(0)", "#NUM!"),
            ("LOG(10, 1)", "#DIV/0!"),
            ("LOG(10, -2)", "#NUM!"),
            ("LOG10(86)", "1.93449845124357"),
            ("LOG10(10)", "1"),
            ("LOG10(1E+5)", "5"),
            ("LOG10(0)", "#NUM!"),
            ("LOG10(\"x\")", "#VALUE!"),
        ]);
    }

    #[test]
    fn ceiling_floor() {
        check0(&[
            ("CEILING(2.5, 1)", "3"),
            ("CEILING(-2.5, -2)", "-4"),
            ("CEILING(-2.5, 2)", "-2"),
            ("CEILING(1.5, 0.1)", "1.5"),
            ("CEILING(0.234, 0.01)", "0.24"),
            ("CEILING(2.5, -2)", "#NUM!"),
            ("CEILING(2.5, 0)", "0"),
            ("CEILING.MATH(24.3, 5)", "25"),
            ("CEILING.MATH(6.7)", "7"),
            ("CEILING.MATH(-8.1, 2)", "-8"),
            ("CEILING.MATH(-5.5, 2, -1)", "-6"),
            ("CEILING.MATH(5, 0)", "0"),
            ("CEILING.MATH(\"x\")", "#VALUE!"),
            ("FLOOR(3.7, 2)", "2"),
            ("FLOOR(-2.5, -2)", "-2"),
            ("FLOOR(-2.5, 2)", "-4"),
            ("FLOOR(2.5, -2)", "#NUM!"),
            ("FLOOR(1.58, 0.1)", "1.5"),
            ("FLOOR(0.234, 0.01)", "0.23"),
            ("FLOOR(2, 0)", "#DIV/0!"),
            ("FLOOR.MATH(24.3, 5)", "20"),
            ("FLOOR.MATH(6.7)", "6"),
            ("FLOOR.MATH(-8.1, 2)", "-10"),
            ("FLOOR.MATH(-5.5, 2, -1)", "-4"),
            ("FLOOR.MATH(5, 0)", "0"),
            ("MROUND(10, 3)", "9"),
            ("MROUND(-10, -3)", "-9"),
            ("MROUND(1.3, 0.2)", "1.4"),
            ("MROUND(5, -2)", "#NUM!"),
            ("MROUND(7.5, 5)", "10"),
            ("MROUND(5, 0)", "0"),
            ("EVEN(1.5)", "2"),
            ("EVEN(3)", "4"),
            ("EVEN(2)", "2"),
            ("EVEN(-1)", "-2"),
            ("EVEN(0)", "0"),
            ("EVEN(\"x\")", "#VALUE!"),
            ("ODD(1.5)", "3"),
            ("ODD(3)", "3"),
            ("ODD(2)", "3"),
            ("ODD(-1)", "-1"),
            ("ODD(-2)", "-3"),
            ("ODD(0)", "1"),
        ]);
    }

    #[test]
    fn misc_math() {
        let mut m = Mock::grid(&[&["3", "2"], &["4", "7"], &["8", "6"], &["6", "7"]]);
        check(
            &mut m,
            &[
                ("SIGN(10)", "1"),
                ("SIGN(-0)", "0"),
                ("SIGN(-0.00001)", "-1"),
                ("SIGN(0)", "0"),
                ("SIGN(\"x\")", "#VALUE!"),
                ("PI()", "3.14159265358979"),
                ("SUMPRODUCT(A1:A4, B1:B4)", "124"),
                ("SUMPRODUCT({3,4;8,6;1,9}, {2,7;6,7;5,3})", "156"),
                ("SUMPRODUCT(A1:A4)", "21"),
                ("SUMPRODUCT(A1:A4, B1:B3)", "#VALUE!"),
                ("SUMPRODUCT({1,\"a\",3}, {1,1,1})", "4"),
                ("SUMPRODUCT({1,#N/A}, {1,1})", "#N/A"),
                ("QUOTIENT(5, 2)", "2"),
                ("QUOTIENT(4.5, 3.1)", "1"),
                ("QUOTIENT(-10, 3)", "-3"),
                ("QUOTIENT(1, 0)", "#DIV/0!"),
                ("QUOTIENT(\"x\", 1)", "#VALUE!"),
                ("GCD(5, 2)", "1"),
                ("GCD(24, 36)", "12"),
                ("GCD(7, 1)", "1"),
                ("GCD(5, 0)", "5"),
                ("GCD(-1, 2)", "#NUM!"),
                ("GCD(A1:A4)", "1"),
                ("LCM(5, 2)", "10"),
                ("LCM(24, 36)", "72"),
                ("LCM(3, 0)", "0"),
                ("LCM(-1, 2)", "#NUM!"),
                ("LCM(4.9, 6)", "12"),
                ("FACT(5)", "120"),
                ("FACT(1.9)", "1"),
                ("FACT(0)", "1"),
                ("FACT(-1)", "#NUM!"),
                ("FACT(1)", "1"),
                ("FACT(171)", "#NUM!"),
            ],
        );
        // Deterministic mock randomness: 0.25, 0.5, …
        check(
            &mut m,
            &[
                ("RAND()", "0.25"),
                ("RANDBETWEEN(1, 100)", "51"),
                ("RANDBETWEEN(-1, 2)", "2"),
                ("RANDBETWEEN(5, 5)", "5"),
                ("RANDBETWEEN(1, 10)", "10"),
                ("RANDBETWEEN(3, 1)", "#NUM!"),
            ],
        );
    }
}
