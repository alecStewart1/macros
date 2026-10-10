//! Text functions. Positions and lengths count characters (Unicode scalar
//! values), not bytes.

use std::sync::Arc;

use super::args::{MANY, arg_values, def, opt_bool, opt_int, to_serial, trunc_i64};
use super::{Arg, Ctx, FuncDef, num, ok, scalar, text};
use crate::formula::criteria::Pattern;
use crate::formula::decimal::{Dec, Mode};
use crate::formula::textfmt;
use crate::formula::value::{ErrorKind, Value, parse_number};

pub(super) const FUNCS: &[FuncDef] = &[
    def("LEN", 1, 1, len),
    def("LEFT", 1, 2, left),
    def("RIGHT", 1, 2, right),
    def("MID", 3, 3, mid),
    def("UPPER", 1, 1, upper),
    def("LOWER", 1, 1, lower),
    def("PROPER", 1, 1, proper),
    def("TRIM", 1, 1, trim),
    def("CONCAT", 1, MANY, concat),
    def("CONCATENATE", 1, MANY, concatenate),
    def("TEXTJOIN", 3, MANY, textjoin),
    def("SUBSTITUTE", 3, 4, substitute),
    def("REPLACE", 4, 4, replace),
    def("FIND", 2, 3, find),
    def("SEARCH", 2, 3, search),
    def("VALUE", 1, 1, value),
    def("TEXT", 2, 2, text_fn),
    def("REPT", 2, 2, rept),
    def("EXACT", 2, 2, exact),
    def("CHAR", 1, 1, char_fn),
    def("CODE", 1, 1, code),
    def("CLEAN", 1, 1, clean),
    def("NUMBERVALUE", 1, 3, numbervalue),
    def("FIXED", 1, 3, fixed),
    def("DOLLAR", 1, 2, dollar),
];

/// Excel's cell text limit; longer results are `#VALUE!`.
const MAX_LEN: usize = 32_767;

fn txt(r: Result<String, ErrorKind>) -> Value {
    match r {
        Ok(s) if s.chars().count() > MAX_LEN => Value::Error(ErrorKind::Value),
        Ok(s) => Value::Text(Arc::from(s)),
        Err(e) => Value::Error(e),
    }
}

/// A count argument: truncated, negative is `#VALUE!`.
fn count_arg(arg: Option<&Arg>, ctx: &mut dyn Ctx, default: i64) -> Result<usize, ErrorKind> {
    let n = opt_int(arg, ctx, default)?;
    if n < 0 {
        Err(ErrorKind::Value)
    } else {
        Ok(n as usize)
    }
}

fn len(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    match text(&args[0], ctx) {
        Ok(s) => Value::Number(s.chars().count() as f64),
        Err(e) => Value::Error(e),
    }
}

fn left(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    txt((|| {
        let s = text(&args[0], ctx)?;
        let n = count_arg(args.get(1), ctx, 1)?;
        Ok(s.chars().take(n).collect())
    })())
}

fn right(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    txt((|| {
        let s = text(&args[0], ctx)?;
        let n = count_arg(args.get(1), ctx, 1)?;
        let total = s.chars().count();
        Ok(s.chars().skip(total.saturating_sub(n)).collect())
    })())
}

fn mid(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    txt((|| {
        let s = text(&args[0], ctx)?;
        let start = trunc_i64(num(&args[1], ctx)?)?;
        let n = count_arg(args.get(2), ctx, 0)?;
        if start < 1 {
            return Err(ErrorKind::Value);
        }
        Ok(s.chars().skip(start as usize - 1).take(n).collect())
    })())
}

fn map_text(args: &[Arg], ctx: &mut dyn Ctx, f: fn(&str) -> String) -> Value {
    txt(text(&args[0], ctx).map(|s| f(&s)))
}

fn upper(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    map_text(args, ctx, str::to_uppercase)
}

fn lower(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    map_text(args, ctx, str::to_lowercase)
}

/// Capitalise each letter that follows a non-letter, lower-case the rest
/// (`"2-way street"` → `"2-Way Street"`, `"it's"` → `"It'S"`, as Excel).
fn proper(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    map_text(args, ctx, |s| {
        let mut out = String::with_capacity(s.len());
        let mut prev_letter = false;
        for c in s.chars() {
            if c.is_alphabetic() {
                if prev_letter {
                    out.extend(c.to_lowercase());
                } else {
                    out.extend(c.to_uppercase());
                }
                prev_letter = true;
            } else {
                out.push(c);
                prev_letter = false;
            }
        }
        out
    })
}

/// Excel's TRIM: ASCII spaces only — drop leading/trailing ones and
/// collapse inner runs to one (non-breaking spaces are kept).
fn trim(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    map_text(args, ctx, |s| {
        s.split(' ')
            .filter(|w| !w.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    })
}

/// CONCAT: every value of every argument, ranges flattened row by row.
fn concat(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    txt((|| {
        let mut out = String::new();
        for a in args {
            for v in arg_values(a, &*ctx)? {
                out.push_str(&v.to_text()?);
                if out.len() > MAX_LEN * 4 {
                    return Err(ErrorKind::Value);
                }
            }
        }
        Ok(out)
    })())
}

/// CONCATENATE: one value per argument (a multi-cell range is `#VALUE!`).
fn concatenate(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    txt((|| {
        let mut out = String::new();
        for a in args {
            out.push_str(&text(a, ctx)?);
        }
        Ok(out)
    })())
}

/// TEXTJOIN(delimiter, ignore_empty, text…): a range or array of
/// delimiters is used in turn.
fn textjoin(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    txt((|| {
        let delims: Vec<Arc<str>> = arg_values(&args[0], &*ctx)?
            .iter()
            .map(Value::to_text)
            .collect::<Result<_, _>>()?;
        let ignore_empty = opt_bool(args.get(1), ctx, true)?;
        let mut parts: Vec<Arc<str>> = Vec::new();
        for a in &args[2..] {
            for v in arg_values(a, &*ctx)? {
                let t = v.to_text()?;
                if ignore_empty && t.is_empty() {
                    continue;
                }
                parts.push(t);
            }
        }
        let mut out = String::new();
        for (i, p) in parts.iter().enumerate() {
            if i > 0 && !delims.is_empty() {
                out.push_str(&delims[(i - 1) % delims.len()]);
            }
            out.push_str(p);
            if out.len() > MAX_LEN * 4 {
                return Err(ErrorKind::Value);
            }
        }
        Ok(out)
    })())
}

/// SUBSTITUTE(text, old, new, [instance]): case-sensitive; every
/// occurrence, or only the `instance`-th.
fn substitute(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    txt((|| {
        let s = text(&args[0], ctx)?;
        let old = text(&args[1], ctx)?;
        let new = text(&args[2], ctx)?;
        let instance = match args.get(3) {
            None | Some(Arg::Omitted) => None,
            Some(a) => {
                let n = trunc_i64(num(a, ctx)?)?;
                if n < 1 {
                    return Err(ErrorKind::Value);
                }
                Some(n as usize)
            }
        };
        if old.is_empty() {
            return Ok(s.to_string());
        }
        Ok(match instance {
            None => s.replace(&*old, &new),
            Some(k) => match s.match_indices(&*old).nth(k - 1) {
                Some((at, _)) => format!("{}{}{}", &s[..at], new, &s[at + old.len()..]),
                None => s.to_string(),
            },
        })
    })())
}

/// REPLACE(old, start, count, new) by character position.
fn replace(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    txt((|| {
        let s: Vec<char> = text(&args[0], ctx)?.chars().collect();
        let start = trunc_i64(num(&args[1], ctx)?)?;
        let n = count_arg(args.get(2), ctx, 0)?;
        let new = text(&args[3], ctx)?;
        if start < 1 {
            return Err(ErrorKind::Value);
        }
        let a = (start as usize - 1).min(s.len());
        let b = a.saturating_add(n).min(s.len());
        let mut out: String = s[..a].iter().collect();
        out.push_str(&new);
        out.extend(&s[b..]);
        Ok(out)
    })())
}

/// The shared argument handling of FIND and SEARCH: `(needle, haystack,
/// 0-based start)`, or `#VALUE!` for a start outside the text.
fn find_args(args: &[Arg], ctx: &mut dyn Ctx) -> Result<(Arc<str>, Arc<str>, usize), ErrorKind> {
    let needle = text(&args[0], ctx)?;
    let hay = text(&args[1], ctx)?;
    let start = opt_int(args.get(2), ctx, 1)?;
    let len = hay.chars().count() as i64;
    // An empty needle is found at the start position (even one past the end
    // of empty text); otherwise the start must be inside the text.
    if start < 1 || start > len.max(if needle.is_empty() { len + 1 } else { 0 }) {
        return Err(ErrorKind::Value);
    }
    Ok((needle, hay, start as usize - 1))
}

/// FIND: case-sensitive, no wildcards.
fn find(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    let r = find_args(args, ctx).and_then(|(needle, hay, start)| {
        let h: Vec<char> = hay.chars().collect();
        let n: Vec<char> = needle.chars().collect();
        if n.is_empty() {
            return Ok(start + 1);
        }
        (start..h.len())
            .find(|&i| h[i..].starts_with(&n))
            .map(|i| i + 1)
            .ok_or(ErrorKind::Value)
    });
    match r {
        Ok(i) => Value::Number(i as f64),
        Err(e) => Value::Error(e),
    }
}

/// SEARCH: case-insensitive, with `*`/`?`/`~` wildcards.
fn search(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    let r = find_args(args, ctx).and_then(|(needle, hay, start)| {
        Pattern::new(&needle)
            .find(&hay, start)
            .map(|i| i + 1)
            .ok_or(ErrorKind::Value)
    });
    match r {
        Ok(i) => Value::Number(i as f64),
        Err(e) => Value::Error(e),
    }
}

/// VALUE: text as typing it into a cell would read it (numbers with
/// grouping, currency or percent, dates, times); a boolean is `#VALUE!`.
fn value(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    match scalar(&args[0], ctx) {
        Value::Bool(_) => Value::Error(ErrorKind::Value),
        v => ok(to_serial(&v, ctx)),
    }
}

fn text_fn(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    txt((|| {
        let v = scalar(&args[0], ctx);
        if let Value::Error(e) = v {
            return Err(e);
        }
        let code = text(&args[1], ctx)?;
        textfmt::format_value(&v, &code, ctx.date1904())
    })())
}

fn rept(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    txt((|| {
        let s = text(&args[0], ctx)?;
        let n = count_arg(args.get(1), ctx, 0)?;
        if s.chars().count().saturating_mul(n) > MAX_LEN {
            return Err(ErrorKind::Value);
        }
        Ok(s.repeat(n))
    })())
}

fn exact(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    let r = (|| Ok::<_, ErrorKind>(text(&args[0], ctx)? == text(&args[1], ctx)?))();
    match r {
        Ok(b) => Value::Bool(b),
        Err(e) => Value::Error(e),
    }
}

/// Windows-1252's characters for bytes 0x80–0x9F (Excel on Windows maps
/// CHAR/CODE through the ANSI code page); undefined slots map to the C1
/// control of the same number.
const CP1252_HIGH: [char; 32] = [
    '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8D}', 'Ž', '\u{8F}',
    '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9D}', 'ž', 'Ÿ',
];

fn char_fn(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    txt((|| {
        let n = trunc_i64(num(&args[0], ctx)?)?;
        let c = match n {
            1..=127 | 160..=255 => char::from_u32(n as u32).ok_or(ErrorKind::Value)?,
            128..=159 => CP1252_HIGH[n as usize - 128],
            _ => return Err(ErrorKind::Value),
        };
        Ok(c.to_string())
    })())
}

/// CODE: the Windows-1252 code of the first character; characters outside
/// that code page give 63 (`?`), as Excel's conversion does.
fn code(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    match text(&args[0], ctx) {
        Err(e) => Value::Error(e),
        Ok(s) => match s.chars().next() {
            None => Value::Error(ErrorKind::Value),
            Some(c) => {
                let u = c as u32;
                let code = if u < 128 || (160..=255).contains(&u) {
                    u
                } else if let Some(i) = CP1252_HIGH.iter().position(|&x| x == c) {
                    128 + i as u32
                } else {
                    63
                };
                Value::Number(code as f64)
            }
        },
    }
}

/// CLEAN: drop the non-printing ASCII control characters (0–31).
fn clean(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    map_text(args, ctx, |s| {
        s.chars().filter(|c| (*c as u32) >= 32).collect()
    })
}

/// NUMBERVALUE(text, [decimal_sep], [group_sep]): locale-independent
/// parsing. Spaces anywhere are ignored, group separators before the
/// decimal separator are dropped (after it they're `#VALUE!`), trailing
/// `%` signs divide by 100 each, empty text is 0.
fn numbervalue(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    let r = (|| {
        let s = text(&args[0], ctx)?;
        let sep = |a: Option<&Arg>, ctx: &mut dyn Ctx, d: char| -> Result<char, ErrorKind> {
            match a {
                None | Some(Arg::Omitted) => Ok(d),
                Some(a) => text(a, ctx)?.chars().next().ok_or(ErrorKind::Value),
            }
        };
        let dec = sep(args.get(1), ctx, '.')?;
        let group = sep(args.get(2), ctx, ',')?;
        if dec == group {
            return Err(ErrorKind::Value);
        }
        let mut body: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        if body.is_empty() {
            return Ok(0.0);
        }
        let mut pct = 0;
        while let Some(b) = body.strip_suffix('%') {
            body = b.to_string();
            pct += 1;
        }
        let mut out = String::new();
        let mut seen_dec = false;
        for c in body.chars() {
            if c == dec {
                if seen_dec {
                    return Err(ErrorKind::Value);
                }
                seen_dec = true;
                out.push('.');
            } else if c == group {
                if seen_dec {
                    return Err(ErrorKind::Value);
                }
            } else {
                out.push(c);
            }
        }
        let n = parse_number(&out).ok_or(ErrorKind::Value)?;
        Ok(n / 100f64.powi(pct))
    })();
    super::ok(r)
}

/// `number` rounded to `decimals` places as text: `1,234.57`. Negative
/// decimals round left of the point and show no fraction.
fn fixed_text(x: f64, decimals: i64, commas: bool) -> Result<String, ErrorKind> {
    if decimals > 127 {
        return Err(ErrorKind::Value);
    }
    let decimals = decimals.max(-400) as i32;
    let d = Dec::from_f64(x).round(decimals, Mode::Half);
    let nd = decimals.max(0) as usize;
    let (int, frac) = d.fixed_parts(nd);
    let int = if int.is_empty() { "0".to_string() } else { int };
    let mut grouped = String::new();
    for (i, ch) in int.chars().enumerate() {
        if commas && i > 0 && (int.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    let mut out = String::new();
    if x < 0.0 && !d.is_zero() {
        out.push('-');
    }
    out.push_str(&grouped);
    if nd > 0 {
        out.push('.');
        out.push_str(&frac);
    }
    Ok(out)
}

fn fixed(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    txt((|| {
        let x = num(&args[0], ctx)?;
        let d = opt_int(args.get(1), ctx, 2)?;
        let no_commas = opt_bool(args.get(2), ctx, false)?;
        fixed_text(x, d, !no_commas)
    })())
}

/// DOLLAR: currency text, negatives in parentheses (`($1,234.57)`), as
/// Excel's en-US currency format.
fn dollar(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    txt((|| {
        let x = num(&args[0], ctx)?;
        let d = opt_int(args.get(1), ctx, 2)?;
        let body = fixed_text(x.abs(), d, true)?;
        let zero = body.chars().all(|c| matches!(c, '0' | '.' | ','));
        Ok(if x < 0.0 && !zero {
            format!("(${body})")
        } else {
            format!("${body}")
        })
    })())
}

#[cfg(test)]
mod tests {
    use super::super::testing::{Mock, check};

    fn m() -> Mock {
        Mock::grid(&[
            &["Phoenix, AZ", "Sale Price"],
            &["", "  First Quarter Earnings  "],
            &["1234.567", "Fluid Flow"],
            &["#N/A", "x"],
        ])
    }

    #[test]
    fn slicing() {
        check(
            &mut m(),
            &[
                ("LEN(A1)", "11"),
                ("LEN(A2)", "0"),
                ("LEN(\"     One   \")", "11"),
                ("LEN(123.45)", "6"),
                ("LEN(\"héllo\")", "5"),
                ("LEN(A4)", "#N/A"),
                ("LEFT(B1, 4)", "\"Sale\""),
                ("LEFT(\"Sweden\")", "\"S\""),
                ("LEFT(\"abc\", 10)", "\"abc\""),
                ("LEFT(\"abc\", 0)", "\"\""),
                ("LEFT(\"abc\", -1)", "#VALUE!"),
                ("LEFT(\"日本語\", 2)", "\"日本\""),
                ("RIGHT(B1, 5)", "\"Price\""),
                ("RIGHT(\"Stock Number\")", "\"r\""),
                ("RIGHT(\"abc\", 9)", "\"abc\""),
                ("RIGHT(\"abc\", -1)", "#VALUE!"),
                ("RIGHT(1234, 2)", "\"34\""),
                ("MID(B3, 1, 5)", "\"Fluid\""),
                ("MID(B3, 7, 20)", "\"Flow\""),
                ("MID(B3, 20, 5)", "\"\""),
                ("MID(B3, 0, 5)", "#VALUE!"),
                ("MID(B3, 1, -1)", "#VALUE!"),
                ("MID(\"日本語\", 2, 1)", "\"本\""),
            ],
        );
    }

    #[test]
    fn case_and_cleanup() {
        check(
            &mut m(),
            &[
                ("UPPER(\"total\")", "\"TOTAL\""),
                ("UPPER(\"Yield\")", "\"YIELD\""),
                ("UPPER(\"straße\")", "\"STRASSE\""),
                ("UPPER(1.5)", "\"1.5\""),
                ("UPPER(A4)", "#N/A"),
                ("LOWER(\"E. E. Cummings\")", "\"e. e. cummings\""),
                ("LOWER(\"Apt. 2B\")", "\"apt. 2b\""),
                ("LOWER(TRUE)", "\"true\""),
                ("LOWER(\"ÉCOLE\")", "\"école\""),
                ("LOWER(A2)", "\"\""),
                ("PROPER(\"this is a TITLE\")", "\"This Is A Title\""),
                ("PROPER(\"2-way street\")", "\"2-Way Street\""),
                ("PROPER(\"76BudGet\")", "\"76Budget\""),
                ("PROPER(\"it's\")", "\"It'S\""),
                ("PROPER(A4)", "#N/A"),
                (
                    "TRIM(\" First Quarter Earnings \")",
                    "\"First Quarter Earnings\"",
                ),
                ("TRIM(B2)", "\"First Quarter Earnings\""),
                ("TRIM(\"a    b\")", "\"a b\""),
                ("TRIM(\"\u{a0}a\u{a0}\")", "\"\u{a0}a\u{a0}\""),
                ("TRIM(\"   \")", "\"\""),
                ("CLEAN(\"a\u{9}b\u{1}c\")", "\"abc\""),
                ("CLEAN(\"text\")", "\"text\""),
                ("CLEAN(\"\u{7}\")", "\"\""),
                ("CLEAN(5)", "\"5\""),
                ("CLEAN(A4)", "#N/A"),
            ],
        );
    }

    #[test]
    fn joining() {
        let mut g = Mock::grid(&[&["a", "b"], &["", "c"], &["1", "#DIV/0!"]]);
        check(
            &mut g,
            &[
                ("CONCAT(\"The\", \" \", \"sun\")", "\"The sun\""),
                ("CONCAT(A1:B2)", "\"abc\""),
                ("CONCAT(A1:A3, \"!\")", "\"a1!\""),
                ("CONCAT(1, TRUE)", "\"1TRUE\""),
                ("CONCAT(A1:B3)", "#DIV/0!"),
                (
                    "CONCATENATE(\"Stream population for \", A1, \" is \", 32)",
                    "\"Stream population for a is 32\"",
                ),
                ("CONCATENATE(A1, B1)", "\"ab\""),
                ("CONCATENATE(A1:B1)", "#VALUE!"),
                ("CONCATENATE(A2, \"x\")", "\"x\""),
                ("CONCATENATE(#N/A, \"x\")", "#N/A"),
                ("TEXTJOIN(\", \", TRUE, A1:B2)", "\"a, b, c\""),
                ("TEXTJOIN(\", \", FALSE, A1:B2)", "\"a, b, , c\""),
                ("TEXTJOIN(\"-\", TRUE, \"x\", \"\", \"y\")", "\"x-y\""),
                ("TEXTJOIN({\"+\",\"=\"}, TRUE, 1, 2, 3)", "\"1+2=3\""),
                ("TEXTJOIN(\"\", TRUE, A1:A3)", "\"a1\""),
                ("TEXTJOIN(\",\", TRUE, A1:B3)", "#DIV/0!"),
                ("REPT(\"*-\", 3)", "\"*-*-*-\""),
                ("REPT(\"-\", 10)", "\"----------\""),
                ("REPT(\"x\", 0)", "\"\""),
                ("REPT(\"x\", -1)", "#VALUE!"),
                ("REPT(\"ab\", 20000)", "#VALUE!"),
                ("EXACT(\"word\", \"word\")", "TRUE"),
                ("EXACT(\"Word\", \"word\")", "FALSE"),
                ("EXACT(\"w ord\", \"word\")", "FALSE"),
                ("EXACT(1, \"1\")", "TRUE"),
                ("EXACT(A2, \"\")", "TRUE"),
                ("EXACT(#N/A, \"\")", "#N/A"),
            ],
        );
    }

    #[test]
    fn substitution_and_search() {
        check(
            &mut m(),
            &[
                (
                    "SUBSTITUTE(\"Sales Data\", \"Sales\", \"Cost\")",
                    "\"Cost Data\"",
                ),
                (
                    "SUBSTITUTE(\"Quarter 1, 2008\", \"1\", \"2\", 1)",
                    "\"Quarter 2, 2008\"",
                ),
                (
                    "SUBSTITUTE(\"Quarter 1, 2011\", \"1\", \"2\", 3)",
                    "\"Quarter 1, 2012\"",
                ),
                ("SUBSTITUTE(\"aaa\", \"a\", \"b\", 4)", "\"aaa\""),
                ("SUBSTITUTE(\"aaa\", \"A\", \"b\")", "\"aaa\""),
                ("SUBSTITUTE(\"aaa\", \"\", \"b\")", "\"aaa\""),
                ("SUBSTITUTE(\"aaa\", \"a\", \"b\", 0)", "#VALUE!"),
                ("REPLACE(\"abcdefghijk\", 6, 5, \"*\")", "\"abcde*k\""),
                ("REPLACE(\"2009\", 3, 2, \"10\")", "\"2010\""),
                ("REPLACE(\"123456\", 1, 3, \"@\")", "\"@456\""),
                ("REPLACE(\"abc\", 10, 1, \"X\")", "\"abcX\""),
                ("REPLACE(\"abc\", 0, 1, \"X\")", "#VALUE!"),
                ("REPLACE(\"abc\", 1, -1, \"X\")", "#VALUE!"),
                ("FIND(\"M\", \"Miriam McGovern\")", "1"),
                ("FIND(\"m\", \"Miriam McGovern\")", "6"),
                ("FIND(\"M\", \"Miriam McGovern\", 3)", "8"),
                ("FIND(\"x\", \"abc\")", "#VALUE!"),
                ("FIND(\"a\", \"abc\", 0)", "#VALUE!"),
                ("FIND(\"a\", \"abc\", 4)", "#VALUE!"),
                ("FIND(\"\", \"abc\")", "1"),
                ("FIND(\"\", \"abc\", 3)", "3"),
                ("FIND(\"*\", \"a*c\")", "2"),
                ("SEARCH(\"e\", \"Statements\", 6)", "7"),
                ("SEARCH(\"margin\", \"Profit Margin\")", "8"),
                ("SEARCH(\"M?r\", \"Profit Margin\")", "8"),
                ("SEARCH(\"p*t\", \"Profit Margin\")", "1"),
                ("SEARCH(\"~*\", \"a*c\")", "2"),
                ("SEARCH(\"z\", \"abc\")", "#VALUE!"),
                ("SEARCH(\"a\", \"abc\", 5)", "#VALUE!"),
                ("SEARCH(\"\", \"abc\", 2)", "2"),
                ("SEARCH(A4, \"abc\")", "#N/A"),
            ],
        );
    }

    #[test]
    fn numbers_as_text() {
        check(
            &mut m(),
            &[
                ("VALUE(\"$1,000\")", "1000"),
                ("VALUE(\"16:48:00\")", "0.7"),
                ("VALUE(\"12:00\")", "0.5"),
                ("VALUE(\" 42 \")", "42"),
                ("VALUE(\"1.5e3\")", "1500"),
                ("VALUE(\"25%\")", "0.25"),
                ("VALUE(\"(100)\")", "-100"),
                ("VALUE(\"-$1,234.50\")", "-1234.5"),
                ("VALUE(\"2024-03-01\")", "45352"),
                ("VALUE(\"abc\")", "#VALUE!"),
                ("VALUE(\"\")", "#VALUE!"),
                ("VALUE(\",1\")", "#VALUE!"),
                ("VALUE(TRUE)", "#VALUE!"),
                ("VALUE(A2)", "0"),
                ("VALUE(7)", "7"),
                ("NUMBERVALUE(\"2.500,27\", \",\", \".\")", "2500.27"),
                ("NUMBERVALUE(\"3.5%\")", "0.035"),
                ("NUMBERVALUE(\"1 234.5\")", "1234.5"),
                ("NUMBERVALUE(\"\")", "0"),
                ("NUMBERVALUE(\"1.2.3\")", "#VALUE!"),
                ("NUMBERVALUE(\"1.234,5\")", "#VALUE!"),
                ("NUMBERVALUE(\"5%%\")", "0.0005"),
                ("NUMBERVALUE(\"1,5\", \",\", \",\")", "#VALUE!"),
                ("FIXED(1234.567, 1)", "\"1,234.6\""),
                ("FIXED(1234.567, -1)", "\"1,230\""),
                ("FIXED(-1234.567, -1, TRUE)", "\"-1230\""),
                ("FIXED(44.332)", "\"44.33\""),
                ("FIXED(2.675, 2)", "\"2.68\""),
                ("FIXED(0.5, 0)", "\"1\""),
                ("FIXED(1234567, 0)", "\"1,234,567\""),
                ("FIXED(\"x\")", "#VALUE!"),
                ("FIXED(1, 128)", "#VALUE!"),
                ("DOLLAR(1234.567, 2)", "\"$1,234.57\""),
                ("DOLLAR(1234.567, -2)", "\"$1,200\""),
                ("DOLLAR(-1234.567, -2)", "\"($1,200)\""),
                ("DOLLAR(-0.123, 4)", "\"($0.1230)\""),
                ("DOLLAR(99.888)", "\"$99.89\""),
                ("DOLLAR(\"x\")", "#VALUE!"),
            ],
        );
    }

    #[test]
    fn text_formatting_and_codes() {
        check(
            &mut m(),
            &[
                ("TEXT(1234.567, \"$#,##0.00\")", "\"$1,234.57\""),
                ("TEXT(0.285, \"0.0%\")", "\"28.5%\""),
                ("TEXT(45352, \"mm/dd/yy\")", "\"03/01/24\""),
                ("TEXT(45352, \"dddd\")", "\"Friday\""),
                ("TEXT(0.6, \"h:mm AM/PM\")", "\"2:24 PM\""),
                ("TEXT(\"2024-03-01\", \"mmm d\")", "\"Mar 1\""),
                ("TEXT(A3, \"#,##0.0\")", "\"1,234.6\""),
                ("TEXT(\"abc\", \"@\")", "\"abc\""),
                ("TEXT(A4, \"0\")", "#N/A"),
                ("TEXT(TRUE, \"0\")", "\"TRUE\""),
                ("TEXT(1234567, \"#,##0,,\"\"M\"\"\")", "\"1M\""),
                ("CHAR(65)", "\"A\""),
                ("CHAR(33)", "\"!\""),
                ("CHAR(128)", "\"€\""),
                ("CHAR(233)", "\"é\""),
                ("CHAR(0)", "#VALUE!"),
                ("CHAR(256)", "#VALUE!"),
                ("CODE(\"A\")", "65"),
                ("CODE(\"!\")", "33"),
                ("CODE(\"Alphabet\")", "65"),
                ("CODE(\"€\")", "128"),
                ("CODE(\"é\")", "233"),
                ("CODE(\"日\")", "63"),
                ("CODE(\"\")", "#VALUE!"),
            ],
        );
    }
}
