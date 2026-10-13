//! Excel number-format codes, as `TEXT(value, format_text)` applies them.
//!
//! Supported:
//! - Up to four sections, `positive;negative;zero;text`, and conditions
//!   (`[>=100]0;[<0]…`); colours (`[Red]`, `[Color10]`) are parsed and
//!   ignored, `[$€-407]` currency/locale tags show their symbol.
//! - Digit placeholders `0 # ?`, the decimal point, the thousands
//!   separator and scaling commas (`#,##0,,"M"`), `%`, scientific `E+ E-`,
//!   and `General`.
//! - Literals: `"quoted text"`, `\x`, `_x` (a space), `*x` fills (dropped:
//!   `TEXT` has no column width to fill), and the usual punctuation.
//! - `@` (the text itself).
//! - Dates and times: `yyyy yy mmmm mmm mm m mmmmm dddd ddd dd d hh h mm m
//!   ss s AM/PM am/pm A/P [h] [mm] [ss]` and fractional seconds `ss.000`,
//!   with Excel's rule that `m`/`mm` mean minutes right after an hour code
//!   or right before a seconds code.
//!
//! Rounding is half away from zero on the 15-digit decimal form, as Excel.
//! Not supported (deviation): fractions (`# ?/?`), which print `/` as a
//! literal; locale-specific calendars (`B2`, `e`, `g`).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use super::date;
use super::decimal::{Dec, Mode};
use super::value::{ErrorKind, Value, general_format, parse_number};

#[derive(Clone, Debug, PartialEq)]
enum DTok {
    Year(usize),
    /// Unresolved `m` run, before the month/minute rule.
    MOrMin(usize),
    Month(usize),
    Minute(usize),
    Day(usize),
    Hour(usize),
    Second(usize),
    /// Fractional seconds after `s`/`ss`: the digit count.
    FracSec(usize),
    AmPm(String, String),
    ElapsedH(usize),
    ElapsedM(usize),
    ElapsedS(usize),
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Lit(String),
    Digit(char),
    Point,
    Comma,
    Percent,
    Exp(bool),
    At,
    General,
    Date(DTok),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum CondOp {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
}

#[derive(Clone, Debug)]
struct Section {
    toks: Vec<Tok>,
    cond: Option<(CondOp, f64)>,
    /// How `toks` lay out a number (unused by a date section).
    layout: NumLayout,
}

impl Section {
    fn is_date(&self) -> bool {
        self.toks.iter().any(|t| matches!(t, Tok::Date(_)))
    }
    fn has_at(&self) -> bool {
        self.toks.contains(&Tok::At)
    }
    fn test(&self, n: f64) -> bool {
        match self.cond {
            None => true,
            Some((op, v)) => match op {
                CondOp::Lt => n < v,
                CondOp::Le => n <= v,
                CondOp::Gt => n > v,
                CondOp::Ge => n >= v,
                CondOp::Eq => n == v,
                CondOp::Ne => n != v,
            },
        }
    }
}

fn split_sections(code: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let b = code.as_bytes();
    let (mut i, mut start) = (0, 0);
    let mut quote = false;
    let mut bracket = false;
    while i < b.len() {
        match b[i] {
            b'"' if !bracket => quote = !quote,
            b'\\' | b'_' | b'*' if !quote && !bracket => {
                // Skip the escaped character (whole UTF-8 char).
                i += 1;
                while i + 1 < b.len() && (b[i + 1] & 0xC0) == 0x80 {
                    i += 1;
                }
            }
            b'[' if !quote => bracket = true,
            b']' if !quote => bracket = false,
            b';' if !quote && !bracket => {
                out.push(&code[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out.push(&code[start.min(code.len())..]);
    out
}

fn parse_section(src: &str) -> Section {
    let chars: Vec<char> = src.chars().collect();
    let mut toks: Vec<Tok> = Vec::new();
    let mut cond = None;
    let mut i = 0;
    let push_lit = |toks: &mut Vec<Tok>, s: &str| {
        if let Some(Tok::Lit(l)) = toks.last_mut() {
            l.push_str(s);
        } else {
            toks.push(Tok::Lit(s.to_string()));
        }
    };
    let run = |i: usize, set: &[char]| -> usize {
        let mut j = i;
        while j < chars.len() && set.contains(&chars[j]) {
            j += 1;
        }
        j - i
    };
    while i < chars.len() {
        let c = chars[i];
        let rest: String = chars[i..].iter().take(7).collect();
        let lower = rest.to_ascii_lowercase();
        match c {
            '"' => {
                let mut j = i + 1;
                let mut s = String::new();
                while j < chars.len() && chars[j] != '"' {
                    s.push(chars[j]);
                    j += 1;
                }
                push_lit(&mut toks, &s);
                i = j + 1;
            }
            '\\' => {
                if let Some(&e) = chars.get(i + 1) {
                    push_lit(&mut toks, &e.to_string());
                }
                i += 2;
            }
            '_' => {
                push_lit(&mut toks, " ");
                i += 2;
            }
            '*' => i += 2,
            '[' => {
                let mut j = i + 1;
                while j < chars.len() && chars[j] != ']' {
                    j += 1;
                }
                let inner: String = chars[i + 1..j.min(chars.len())].iter().collect();
                i = j + 1;
                let li = inner.to_ascii_lowercase();
                if let Some(cur) = inner.strip_prefix('$') {
                    let sym = cur.split('-').next().unwrap_or("");
                    if !sym.is_empty() {
                        push_lit(&mut toks, sym);
                    }
                } else if !li.is_empty() && li.chars().all(|c| c == 'h') {
                    toks.push(Tok::Date(DTok::ElapsedH(li.len())));
                } else if !li.is_empty() && li.chars().all(|c| c == 'm') {
                    toks.push(Tok::Date(DTok::ElapsedM(li.len())));
                } else if !li.is_empty() && li.chars().all(|c| c == 's') {
                    toks.push(Tok::Date(DTok::ElapsedS(li.len())));
                } else if let Some(c) = parse_cond(&inner) {
                    cond = Some(c);
                }
                // Otherwise a colour: ignored.
            }
            '0' | '#' | '?' => {
                toks.push(Tok::Digit(c));
                i += 1;
            }
            '.' => {
                let after_sec = toks
                    .iter()
                    .rev()
                    .find_map(|t| match t {
                        Tok::Date(d) => Some(matches!(d, DTok::Second(_) | DTok::ElapsedS(_))),
                        _ => None,
                    })
                    .unwrap_or(false);
                let zeros = run(i + 1, &['0']);
                if after_sec && zeros > 0 {
                    toks.push(Tok::Date(DTok::FracSec(zeros.min(3))));
                    i += 1 + zeros;
                } else {
                    toks.push(Tok::Point);
                    i += 1;
                }
            }
            ',' => {
                toks.push(Tok::Comma);
                i += 1;
            }
            '%' => {
                toks.push(Tok::Percent);
                i += 1;
            }
            '@' => {
                toks.push(Tok::At);
                i += 1;
            }
            'E' | 'e' if matches!(chars.get(i + 1), Some('+' | '-')) => {
                toks.push(Tok::Exp(chars[i + 1] == '+'));
                i += 2;
            }
            _ if lower.starts_with("general") => {
                toks.push(Tok::General);
                i += 7;
            }
            _ if lower.starts_with("am/pm") => {
                let am: String = chars[i..i + 2].iter().collect();
                let pm: String = chars[i + 3..i + 5].iter().collect();
                toks.push(Tok::Date(DTok::AmPm(am, pm)));
                i += 5;
            }
            _ if lower.starts_with("a/p") => {
                toks.push(Tok::Date(DTok::AmPm(
                    chars[i].to_string(),
                    chars[i + 2].to_string(),
                )));
                i += 3;
            }
            'y' | 'Y' => {
                let n = run(i, &['y', 'Y']);
                toks.push(Tok::Date(DTok::Year(n)));
                i += n;
            }
            'm' | 'M' => {
                let n = run(i, &['m', 'M']);
                toks.push(Tok::Date(DTok::MOrMin(n)));
                i += n;
            }
            'd' | 'D' => {
                let n = run(i, &['d', 'D']);
                toks.push(Tok::Date(DTok::Day(n)));
                i += n;
            }
            'h' | 'H' => {
                let n = run(i, &['h', 'H']);
                toks.push(Tok::Date(DTok::Hour(n)));
                i += n;
            }
            's' | 'S' => {
                let n = run(i, &['s', 'S']);
                toks.push(Tok::Date(DTok::Second(n)));
                i += n;
            }
            _ => {
                push_lit(&mut toks, &c.to_string());
                i += 1;
            }
        }
    }
    resolve_minutes(&mut toks);
    let layout = NumLayout::of(&toks);
    Section { toks, cond, layout }
}

/// `m`/`mm` are minutes right after an hour code or right before a seconds
/// code (literals in between don't count as codes); otherwise months.
fn resolve_minutes(toks: &mut [Tok]) {
    let date_idx: Vec<usize> = (0..toks.len())
        .filter(|&i| matches!(toks[i], Tok::Date(_)))
        .collect();
    for (k, &i) in date_idx.iter().enumerate() {
        let Tok::Date(DTok::MOrMin(n)) = toks[i] else {
            continue;
        };
        let prev_hour = k > 0
            && matches!(
                toks[date_idx[k - 1]],
                Tok::Date(DTok::Hour(_) | DTok::ElapsedH(_))
            );
        let next_sec = date_idx
            .get(k + 1)
            .is_some_and(|&j| matches!(toks[j], Tok::Date(DTok::Second(_) | DTok::ElapsedS(_))));
        toks[i] = Tok::Date(if n <= 2 && (prev_hour || next_sec) {
            DTok::Minute(n)
        } else {
            DTok::Month(n)
        });
    }
}

fn parse_cond(s: &str) -> Option<(CondOp, f64)> {
    let (op, rest) = if let Some(r) = s.strip_prefix("<=") {
        (CondOp::Le, r)
    } else if let Some(r) = s.strip_prefix(">=") {
        (CondOp::Ge, r)
    } else if let Some(r) = s.strip_prefix("<>") {
        (CondOp::Ne, r)
    } else if let Some(r) = s.strip_prefix('<') {
        (CondOp::Lt, r)
    } else if let Some(r) = s.strip_prefix('>') {
        (CondOp::Gt, r)
    } else {
        (CondOp::Eq, s.strip_prefix('=')?)
    };
    Some((op, parse_number(rest.trim())?))
}

/// A parsed format code, ready to format any number of values: each
/// section's tokens and numeric layout are worked out once
/// ([`NumberFormat::parse`]) rather than on every call.
#[derive(Clone, Debug)]
pub struct NumberFormat {
    /// Every section as written, the text one (the fourth) included.
    secs: Vec<Section>,
}

impl NumberFormat {
    pub fn parse(code: &str) -> NumberFormat {
        NumberFormat {
            secs: split_sections(code)
                .into_iter()
                .map(parse_section)
                .collect(),
        }
    }

    /// Format any value, as `TEXT` does: numbers (and text that reads as a
    /// number) through the numeric sections, other text through the text
    /// section (or unchanged), booleans as `TRUE`/`FALSE`.
    pub fn format_value(&self, v: &Value, date1904: bool) -> Result<String, ErrorKind> {
        match v {
            Value::Error(e) => Err(*e),
            Value::Bool(b) => Ok(if *b { "TRUE" } else { "FALSE" }.to_string()),
            Value::Empty => self.format(0.0, date1904),
            Value::Number(n) => self.format(*n, date1904),
            Value::Text(s) => match parse_number(s.trim()) {
                Some(n) => self.format(n, date1904),
                None => match date::parse_datetime(s, None, date1904) {
                    Some(dt) => self.format(dt.serial(), date1904),
                    None => Ok(self.format_text(s)),
                },
            },
        }
    }

    /// Format text: the fourth section, or a lone section holding `@`;
    /// otherwise the text unchanged.
    pub fn format_text(&self, s: &str) -> String {
        let secs = &self.secs;
        let sec = if secs.len() >= 4 {
            &secs[3]
        } else if secs.len() == 1 && secs[0].has_at() {
            &secs[0]
        } else {
            return s.to_string();
        };
        let mut out = String::new();
        for t in &sec.toks {
            match t {
                Tok::At => out.push_str(s),
                Tok::Lit(l) => out.push_str(l),
                _ => {}
            }
        }
        out
    }

    /// Format a number.
    pub fn format(&self, n: f64, date1904: bool) -> Result<String, ErrorKind> {
        if !n.is_finite() {
            return Err(ErrorKind::Num);
        }
        if self.secs.len() > 4 {
            return Err(ErrorKind::Value);
        }
        // The text section takes no part in numbers.
        let secs = &self.secs[..self.secs.len().min(3)];
        let has_cond = secs.iter().take(2).any(|s| s.cond.is_some());
        let (sec, show_minus) = if has_cond {
            if secs[0].test(n) {
                (&secs[0], n < 0.0)
            } else if secs.len() >= 2 && secs[1].cond.is_some() && secs[1].test(n) {
                (&secs[1], false)
            } else if secs.len() >= 3 {
                (&secs[2], n < 0.0)
            } else if secs.len() == 2 && secs[1].cond.is_none() {
                (&secs[1], n < 0.0)
            } else {
                return Err(ErrorKind::Value);
            }
        } else {
            match secs.len() {
                1 => (&secs[0], n < 0.0),
                2 => (if n < 0.0 { &secs[1] } else { &secs[0] }, false),
                _ => (
                    if n > 0.0 {
                        &secs[0]
                    } else if n < 0.0 {
                        &secs[1]
                    } else {
                        &secs[2]
                    },
                    false,
                ),
            }
        };
        let a = n.abs();
        if sec.is_date() {
            // Dates and times can't be negative.
            if n < 0.0 && (show_minus || secs.len() == 1) {
                return Err(ErrorKind::Value);
            }
            return format_date(a, sec, date1904);
        }
        let body = format_num_section(a, sec);
        Ok(if show_minus { format!("-{body}") } else { body })
    }
}

/// `code` parsed, from a small per-thread cache: grid cells format with a
/// handful of codes, over and over.
fn cached(code: &str) -> Rc<NumberFormat> {
    /// Past this many codes the cache starts over.
    const CAPACITY: usize = 256;
    thread_local! {
        static CACHE: RefCell<HashMap<String, Rc<NumberFormat>>> = RefCell::new(HashMap::new());
    }
    CACHE.with_borrow_mut(|cache| {
        if let Some(f) = cache.get(code) {
            return Rc::clone(f);
        }
        if cache.len() >= CAPACITY {
            cache.clear();
        }
        let f = Rc::new(NumberFormat::parse(code));
        cache.insert(code.to_string(), Rc::clone(&f));
        f
    })
}

/// Format any value with a format code ([`NumberFormat::format_value`]).
pub fn format_value(v: &Value, code: &str, date1904: bool) -> Result<String, ErrorKind> {
    cached(code).format_value(v, date1904)
}

/// The shape of a numeric section, which depends only on its tokens:
/// worked out when the code is parsed.
#[derive(Clone, Debug, Default)]
struct NumLayout {
    /// Where the exponent token is (`E+`), if scientific.
    exp_at: Option<usize>,
    /// The mantissa is `toks[..mant_end]` (everything before the exponent).
    mant_end: usize,
    /// Digit placeholder kinds (`0 # ?`) left and right of the point.
    int_kinds: Vec<char>,
    frac_kinds: Vec<char>,
    /// The exponent's placeholder kinds.
    exp_kinds: Vec<char>,
    /// A thousands separator between integer placeholders.
    thousands: bool,
    /// Scaling commas: each divides by 1,000.
    scale: i32,
    /// Which mantissa commas show as literal commas.
    comma_lit: Vec<bool>,
    /// `%` signs in the mantissa: each multiplies by 100.
    percents: i32,
    has_general: bool,
    has_at: bool,
}

impl NumLayout {
    fn of(toks: &[Tok]) -> NumLayout {
        let exp_at = toks.iter().position(|t| matches!(t, Tok::Exp(_)));
        let mant_end = exp_at.unwrap_or(toks.len());
        let mant = &toks[..mant_end];
        let point_at = mant.iter().position(|t| *t == Tok::Point);
        let int_end = point_at.unwrap_or(mant.len());
        let int_kinds = digit_kinds(&mant[..int_end]);
        let frac_kinds = point_at
            .map(|p| digit_kinds(&mant[p + 1..]))
            .unwrap_or_default();
        let exp_kinds = exp_at
            .map(|e| digit_kinds(&toks[e + 1..]))
            .unwrap_or_default();

        // Classify commas: thousands (between int digits), scaling (after
        // the last digit placeholder), else literal.
        let last_digit = mant.iter().rposition(|t| matches!(t, Tok::Digit(_)));
        let first_digit = mant.iter().position(|t| matches!(t, Tok::Digit(_)));
        let mut thousands = false;
        let mut scale = 0i32;
        let mut comma_lit = vec![false; mant.len()];
        for (i, t) in mant.iter().enumerate() {
            if *t != Tok::Comma {
                continue;
            }
            let Some(fd) = first_digit else {
                comma_lit[i] = true;
                continue;
            };
            if i < fd {
                comma_lit[i] = true;
            } else if i < int_end
                && mant[i + 1..int_end]
                    .iter()
                    .any(|t| matches!(t, Tok::Digit(_)))
            {
                thousands = true;
            } else if last_digit.is_none_or(|ld| i > ld)
                || mant[i + 1..].iter().find(|t| **t != Tok::Comma) == Some(&Tok::Point)
            {
                scale += 1;
            } else {
                comma_lit[i] = true;
            }
        }
        NumLayout {
            exp_at,
            mant_end,
            int_kinds,
            frac_kinds,
            exp_kinds,
            thousands,
            scale,
            comma_lit,
            percents: mant.iter().filter(|t| **t == Tok::Percent).count() as i32,
            has_general: toks.contains(&Tok::General),
            has_at: toks.contains(&Tok::At),
        }
    }

    fn any_digit(&self) -> bool {
        !self.int_kinds.is_empty() || !self.frac_kinds.is_empty()
    }
}

/// The placeholder kinds (`0 # ?`) among `toks`, in order.
fn digit_kinds(toks: &[Tok]) -> Vec<char> {
    toks.iter()
        .filter_map(|t| match t {
            Tok::Digit(c) => Some(*c),
            _ => None,
        })
        .collect()
}

/// Render a non-negative number through a numeric section (the caller adds
/// any minus sign).
fn format_num_section(a: f64, sec: &Section) -> String {
    let toks = &sec.toks;
    let lay = &sec.layout;

    // Scale as decimal digits, not by float multiplication.
    let mut d = Dec::from_f64(a);
    if !d.is_zero() {
        d.point += 2 * lay.percents - 3 * lay.scale;
    }

    if !lay.any_digit() {
        return render_digitless(&d, toks, lay);
    }
    let mut exponent = 0i32;
    if lay.exp_at.is_some() {
        exponent = normalise_exponent(&mut d, lay);
    }
    let mut out = render_mantissa(&d, &toks[..lay.mant_end], lay);
    if let Some(e) = lay.exp_at {
        let Tok::Exp(plus) = toks[e] else {
            unreachable!()
        };
        render_exponent(&mut out, exponent, plus, &toks[e + 1..], &lay.exp_kinds);
    }
    out
}

/// A section with no digit placeholders: `General` (with any literals), a
/// literal-only section, or `@` in a numeric section (General again).
fn render_digitless(d: &Dec, toks: &[Tok], lay: &NumLayout) -> String {
    let mut out = String::new();
    if lay.has_general {
        let g = general_format(d.to_f64());
        for t in toks {
            match t {
                Tok::General => out.push_str(&g),
                Tok::Lit(l) => out.push_str(l),
                Tok::Percent => out.push('%'),
                Tok::At => out.push_str(&g),
                _ => {}
            }
        }
    } else if !lay.has_at {
        for (i, t) in toks.iter().enumerate() {
            match t {
                Tok::Lit(l) => out.push_str(l),
                Tok::Percent => out.push('%'),
                Tok::Point => out.push('.'),
                Tok::Comma if lay.comma_lit.get(i).copied().unwrap_or(true) => out.push(','),
                _ => {}
            }
        }
    } else {
        let g = general_format(d.to_f64());
        for t in toks {
            match t {
                Tok::At | Tok::General => out.push_str(&g),
                Tok::Lit(l) => out.push_str(l),
                _ => {}
            }
        }
    }
    out
}

/// Move `d`'s point for scientific notation and return the exponent.
/// `##0.0E+0` (several placeholders with a '#') is engineering notation:
/// the exponent is a multiple of the placeholder count. Otherwise the
/// mantissa fills every integer placeholder.
fn normalise_exponent(d: &mut Dec, lay: &NumLayout) -> i32 {
    if d.is_zero() {
        return 0;
    }
    let frac_n = lay.frac_kinds.len();
    let p = lay.int_kinds.len().max(1) as i32;
    let engineering = p > 1 && lay.int_kinds.contains(&'#');
    let mag = d.point - 1; // floor(log10)
    let mut exponent = if engineering {
        mag.div_euclid(p) * p
    } else {
        mag - (p - 1)
    };
    d.point -= exponent;
    // Rounding up can add a digit (9.995 → 10.00): renormalise.
    if d.round(frac_n as i32, Mode::Half).point > p {
        let step = if engineering { p } else { 1 };
        exponent += step;
        d.point -= step;
    }
    exponent
}

/// The mantissa: `d` rounded to the fraction placeholders and laid into the
/// placeholders, separators and literals of `mant`.
fn render_mantissa(d: &Dec, mant: &[Tok], lay: &NumLayout) -> String {
    let frac_n = lay.frac_kinds.len();
    let frac_kinds = &lay.frac_kinds;
    let r = d.round(frac_n as i32, Mode::Half);
    let (int_s, frac_s) = r.fixed_parts(frac_n);
    let int_digits: Vec<char> = int_s.chars().collect();

    // Fraction: trailing zeros at '#'/'?' placeholders drop/blank.
    let mut frac_out: Vec<Option<char>> = frac_s.chars().map(Some).collect();
    for k in (0..frac_n).rev() {
        if frac_kinds[k] == '0' || frac_out[k] != Some('0') {
            break;
        }
        frac_out[k] = if frac_kinds[k] == '?' {
            Some(' ')
        } else {
            None
        };
    }

    let thousands = lay.thousands;
    let p_int = lay.int_kinds.len();
    let mut out = String::new();
    let mut int_k = 0usize; // int placeholder index
    let mut frac_k = 0usize;
    let mut in_frac = false;
    let mut emitted_digit = false;
    let emit_int = |out: &mut String, pos: usize, kind: char, emitted: &mut bool| {
        let ch = if pos < int_digits.len() {
            Some(int_digits[int_digits.len() - 1 - pos])
        } else {
            match kind {
                '0' => Some('0'),
                '?' => Some(' '),
                _ => None,
            }
        };
        if let Some(ch) = ch {
            out.push(ch);
            if ch != ' ' {
                *emitted = true;
            }
            if thousands && pos > 0 && pos.is_multiple_of(3) && *emitted && ch != ' ' {
                out.push(',');
            }
        }
    };
    for (i, t) in mant.iter().enumerate() {
        match t {
            Tok::Digit(kind) if !in_frac => {
                let pos = p_int - 1 - int_k;
                if int_k == 0 {
                    // The leftmost placeholder takes any extra digits.
                    for extra in (p_int..int_digits.len()).rev() {
                        emit_int(&mut out, extra, '0', &mut emitted_digit);
                    }
                }
                emit_int(&mut out, pos, *kind, &mut emitted_digit);
                int_k += 1;
            }
            Tok::Digit(_) => {
                if let Some(Some(ch)) = frac_out.get(frac_k) {
                    out.push(*ch);
                }
                frac_k += 1;
            }
            Tok::Point => {
                if p_int == 0 {
                    for extra in (0..int_digits.len()).rev() {
                        emit_int(&mut out, extra, '0', &mut emitted_digit);
                    }
                }
                out.push('.');
                in_frac = true;
            }
            Tok::Comma => {
                if lay.comma_lit[i] {
                    out.push(',');
                }
            }
            Tok::Percent => out.push('%'),
            Tok::Lit(l) => out.push_str(l),
            Tok::At | Tok::General => {}
            Tok::Date(_) | Tok::Exp(_) => {}
        }
    }
    out
}

/// `E`, the sign, and the exponent laid into its placeholders (`etoks`,
/// whose kinds are `ekinds`).
fn render_exponent(out: &mut String, exponent: i32, plus: bool, etoks: &[Tok], ekinds: &[char]) {
    out.push('E');
    if exponent < 0 {
        out.push('-');
    } else if plus {
        out.push('+');
    }
    let edigits: Vec<char> = exponent.unsigned_abs().to_string().chars().collect();
    let pe = ekinds.len();
    let mut k = 0;
    for t in etoks {
        match t {
            Tok::Digit(kind) => {
                if k == 0 {
                    for extra in (pe..edigits.len()).rev() {
                        out.push(edigits[edigits.len() - 1 - extra]);
                    }
                }
                let pos = pe - 1 - k;
                if pos < edigits.len() {
                    out.push(edigits[edigits.len() - 1 - pos]);
                } else if *kind == '0' {
                    out.push('0');
                } else if *kind == '?' {
                    out.push(' ');
                }
                k += 1;
            }
            Tok::Lit(l) => out.push_str(l),
            Tok::Percent => out.push('%'),
            _ => {}
        }
    }
    if pe == 0 {
        out.extend(edigits);
    }
}

fn pad(n: u64, width: usize) -> String {
    format!("{n:0width$}")
}

fn format_date(a: f64, sec: &Section, date1904: bool) -> Result<String, ErrorKind> {
    if a > date::max_serial(date1904) + 1.0 {
        return Err(ErrorKind::Value);
    }
    let toks = &sec.toks;
    let fd = toks
        .iter()
        .filter_map(|t| match t {
            Tok::Date(DTok::FracSec(k)) => Some(*k),
            _ => None,
        })
        .max()
        .unwrap_or(0) as u32;
    let unit = 10u64.pow(fd);
    let per_day = 86_400 * unit;
    let total = (a * per_day as f64).round() as u64;
    let day = total / per_day;
    let tod = total % per_day;
    let secs = tod / unit;
    let sub = tod % unit;
    let (y, mo, d) = date::serial_to_ymd(day as f64, date1904).ok_or(ErrorKind::Value)?;
    let wd = date::weekday0(day as f64, date1904);
    let ampm = toks.iter().any(|t| matches!(t, Tok::Date(DTok::AmPm(..))));
    let h24 = secs / 3600;
    let hour = if ampm {
        match h24 % 12 {
            0 => 12,
            h => h,
        }
    } else {
        h24
    };
    let elapsed_s = total / unit;
    let mut out = String::new();
    for t in toks {
        match t {
            Tok::Lit(l) => out.push_str(l),
            Tok::Digit(c) => out.push(*c),
            Tok::Point => out.push('.'),
            Tok::Comma => out.push(','),
            Tok::Percent => out.push('%'),
            Tok::Exp(p) => out.push_str(if *p { "E+" } else { "E-" }),
            Tok::At | Tok::General => {}
            Tok::Date(dt) => match dt {
                DTok::Year(n) => {
                    if *n <= 2 {
                        out.push_str(&pad((y.rem_euclid(100)) as u64, 2));
                    } else {
                        out.push_str(&pad(y as u64, 4));
                    }
                }
                DTok::Month(n) => match n {
                    1 => out.push_str(&mo.to_string()),
                    2 => out.push_str(&pad(mo as u64, 2)),
                    3 => out.push_str(&date::month_name(mo)[..3]),
                    5 => out.push_str(&date::month_name(mo)[..1]),
                    _ => out.push_str(date::month_name(mo)),
                },
                DTok::Day(n) => match n {
                    1 => out.push_str(&d.to_string()),
                    2 => out.push_str(&pad(d as u64, 2)),
                    3 => out.push_str(&date::weekday_name(wd)[..3]),
                    _ => out.push_str(date::weekday_name(wd)),
                },
                DTok::Hour(n) => out.push_str(&pad(hour, (*n).min(2))),
                DTok::Minute(n) => out.push_str(&pad((secs / 60) % 60, (*n).min(2))),
                DTok::Second(n) => out.push_str(&pad(secs % 60, (*n).min(2))),
                DTok::MOrMin(_) => {}
                DTok::FracSec(k) => {
                    out.push('.');
                    let s = pad(sub, fd as usize);
                    out.push_str(&s[..*k]);
                }
                DTok::AmPm(am, pm) => out.push_str(if h24 < 12 { am } else { pm }),
                DTok::ElapsedH(w) => out.push_str(&pad(elapsed_s / 3600, *w)),
                DTok::ElapsedM(w) => out.push_str(&pad(elapsed_s / 60, *w)),
                DTok::ElapsedS(w) => out.push_str(&pad(elapsed_s, *w)),
            },
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format_number(n: f64, code: &str, date1904: bool) -> Result<String, ErrorKind> {
        NumberFormat::parse(code).format(n, date1904)
    }

    fn format_text(s: &str, code: &str) -> String {
        NumberFormat::parse(code).format_text(s)
    }

    fn f(n: f64, code: &str) -> String {
        format_number(n, code, false).unwrap()
    }

    #[test]
    fn numbers() {
        let cases: &[(f64, &str, &str)] = &[
            (1234.567, "#,##0.00", "1,234.57"),
            (1234567.891, "#,##0.00", "1,234,567.89"),
            (-1234.567, "#,##0.00", "-1,234.57"),
            (0.0, "#,##0.00", "0.00"),
            (0.5, "#,##0", "1"),
            (999.0, "#,##0", "999"),
            (1000.0, "#,##0", "1,000"),
            (0.285, "0%", "29%"),
            (0.285, "0.0%", "28.5%"),
            (1.0, "0%", "100%"),
            (12345.678, "0.00E+00", "1.23E+04"),
            (0.000123, "0.00E+00", "1.23E-04"),
            (0.0, "0.00E+00", "0.00E+00"),
            (9.999, "0.00E+00", "1.00E+01"),
            (12345.0, "0.0E-0", "1.2E4"),
            (12345.0, "##0.0E+0", "12.3E+3"),
            (1234567.0, "##0.0E+0", "1.2E+6"),
            (2.675, "0.00", "2.68"),
            (1.005, "0.00", "1.01"),
            (-2.5, "0", "-3"),
            (2.5, "0", "3"),
            (-0.4, "0", "-0"),
            (5.0, "000", "005"),
            (12.0, "#", "12"),
            (0.0, "#", ""),
            (0.5, "#.##", ".5"),
            (1.5, "0.0#", "1.5"),
            (1.25, "0.0#", "1.25"),
            (1.5, "0.0?", "1.5 "),
            (3.0, "??0", "  3"),
            (12345678.0, "#,##0,,\"M\"", "12M"),
            (123456789.0, "#,##0.0,,\"M\"", "123.5M"),
            (1234567.0, "#,##0,", "1,235"),
            (5.0, "0.0;(0.0);\"zero\"", "5.0"),
            (-5.0, "0.0;(0.0);\"zero\"", "(5.0)"),
            (0.0, "0.0;(0.0);\"zero\"", "zero"),
            (1234.5, "$#,##0.00", "$1,234.50"),
            (-1234.5, "$#,##0.00", "-$1,234.50"),
            (-1234.5, "$#,##0.00_);($#,##0.00)", "($1,234.50)"),
            (1234.5, "$#,##0.00_);($#,##0.00)", "$1,234.50 "),
            (123456789.0, "000-00-0000", "123-45-6789"),
            (5551234.0, "###-####", "555-1234"),
            (42.0, "0 \"units\"", "42 units"),
            (42.0, "\\$0", "$42"),
            (1.0, "General", "1"),
            (0.1 + 0.2, "General", "0.3"),
            (-1.5, "General", "-1.5"),
            (1.5, "0.00 \"kg\";-0.00 \"kg\"", "1.50 kg"),
            (150.0, "[>=100]\"big\";[<0]\"neg\";\"small\"", "big"),
            (50.0, "[>=100]\"big\";[<0]\"neg\";\"small\"", "small"),
            (-5.0, "[>=100]\"big\";[<0]\"neg\";\"small\"", "neg"),
            (5.0, "[Red]0.00", "5.00"),
            (-5.0, "0;[Red]0", "5"),
            (1234.0, "[$€-407] #,##0", "€ 1,234"),
            (1234.0, "", ""),
            (0.0, "0;-0;;@", ""),
            (1e20, "0", "100000000000000000000"),
            (0.75, ".00", ".75"),
            (1.75, ".00", "1.75"),
            (3.0, "0.", "3."),
        ];
        for (n, code, want) in cases {
            assert_eq!(f(*n, code), *want, "TEXT({n}, {code:?})");
        }
    }

    #[test]
    fn dates() {
        // 2024-03-05 14:07:09.25 is serial 45356 + time.
        let t = 45356.0 + (14.0 * 3600.0 + 7.0 * 60.0 + 9.25) / 86400.0;
        let cases: &[(f64, &str, &str)] = &[
            (t, "yyyy-mm-dd", "2024-03-05"),
            (t, "mmm d, yyyy", "Mar 5, 2024"),
            (t, "mmmm d, yyyy", "March 5, 2024"),
            (t, "dddd, mmmm dd", "Tuesday, March 05"),
            (t, "ddd", "Tue"),
            (t, "m/d/yy", "3/5/24"),
            (t, "mmmmm", "M"),
            (t, "h:mm AM/PM", "2:07 PM"),
            (t, "hh:mm:ss", "14:07:09"),
            (t, "h:mm:ss.00", "14:07:09.25"),
            (t, "hh:mm am/pm", "02:07 pm"),
            (t, "h A/P", "2 P"),
            (t, "yyyy-mm-dd hh:mm", "2024-03-05 14:07"),
            (t, "mm:ss", "07:09"),
            (t, "m", "3"),
            (t, "h\"h\" m\"m\"", "14h 7m"),
            (1.25, "[h]:mm", "30:00"),
            (2.5, "[h]:mm:ss", "60:00:00"),
            (0.5, "[mm]:ss", "720:00"),
            (0.5, "[ss]", "43200"),
            (0.0, "h:mm AM/PM", "12:00 AM"),
            (0.5, "h:mm AM/PM", "12:00 PM"),
            (0.999_999_9, "h:mm:ss", "0:00:00"),
            (60.0, "yyyy-mm-dd", "1900-02-29"),
            (0.0, "yyyy-mm-dd", "1900-01-00"),
            (39448.0, "d-mmm-yy", "1-Jan-08"),
            (45356.0, "dd/mm/yyyy", "05/03/2024"),
        ];
        for (n, code, want) in cases {
            assert_eq!(f(*n, code), *want, "TEXT({n}, {code:?})");
        }
        assert_eq!(format_number(-1.0, "yyyy", false), Err(ErrorKind::Value));
        assert_eq!(
            format_number(1462.0, "yyyy-mm-dd", true).unwrap(),
            "1908-01-02"
        );
    }

    #[test]
    fn text_and_values() {
        assert_eq!(format_text("abc", "@\" units\""), "abc units");
        assert_eq!(format_text("abc", "0;0;0;\"[\"@\"]\""), "[abc]");
        assert_eq!(format_text("abc", "0.00"), "abc");
        assert_eq!(
            format_value(&Value::text("12.5"), "0.00", false).unwrap(),
            "12.50"
        );
        assert_eq!(
            format_value(&Value::Bool(true), "0", false).unwrap(),
            "TRUE"
        );
        assert_eq!(format_value(&Value::Empty, "0.0", false).unwrap(), "0.0");
        assert_eq!(
            format_value(&Value::text("2024-03-05"), "mmm d", false).unwrap(),
            "Mar 5"
        );
        assert_eq!(
            format_value(&Value::Error(ErrorKind::NA), "0", false),
            Err(ErrorKind::NA)
        );
    }

    #[test]
    fn more_text_formats() {
        let f =
            |n: f64, c: &str| format_number(n, c, false).unwrap_or_else(|e| e.as_str().to_string());
        let cases: &[(f64, &str, &str)] = &[
            (1234.567, "#,##0.00", "1,234.57"),
            (0.5, "0%", "50%"),
            (45292.0, "yyyy-mm-dd", "2024-01-01"),
            (0.75, "h:mm AM/PM", "6:00 PM"),
            (1234567.0, "#,##0,", "1,235"),
            (-1234.5, "#,##0.00;(#,##0.00)", "(1,234.50)"),
            (0.000123, "0.00E+00", "1.23E-04"),
            (12.0, "000", "012"),
            (2.5, "0", "3"),
            (-2.5, "0", "-3"),
            (1.005, "0.00", "1.01"),
            (45292.5, "mm/dd/yyyy hh:mm", "01/01/2024 12:00"),
            (1.5, "[h]:mm", "36:00"),
            (0.0, "0.0;-0.0;\"zero\"", "zero"),
            (3.0, "\"x\"0", "x3"),
            (12345.678, "$#,##0.00", "$12,345.68"),
            (5.0, "00.0", "05.0"),
            (0.123, ".00", ".12"),
            (1234.0, "#", "1234"),
            (0.0, "#", ""),
            (1e6, "0.0,,\"M\"", "1.0M"),
            (45292.0, "ddd, mmm d", "Mon, Jan 1"),
            (45292.0, "dddd", "Monday"),
            (0.5, "hh:mm:ss", "12:00:00"),
            (1234567.891, "#,##0", "1,234,568"),
            (999.0, "#,##0", "999"),
            (1000.0, "#,##0", "1,000"),
            (123456.0, "#,##0", "123,456"),
            (0.5, "#,##0", "1"),
            (0.4, "#,##0", "0"),
            (-1000.0, "#,##0", "-1,000"),
            (1234.5, "0.0#", "1234.5"),
            (12345.0, "0.00E+0", "1.23E+4"),
            (60.0, "mmm d, yyyy", "Feb 29, 1900"),
            (61.0, "dddd", "Thursday"),
            (45292.0, "m/d/yy", "1/1/24"),
            (0.0006944444444444445, "h:mm", "0:01"),
            (1.0, "General", "1"),
            (0.1, "0.0%", "10.0%"),
            (1234.0, "#,###", "1,234"),
            (100.0, "[>=100]\"big\";\"small\"", "big"),
            (5.0, "[>=100]\"big\";\"small\"", "small"),
        ];
        let mut bad = Vec::new();
        for &(n, c, want) in cases {
            let got = f(n, c);
            if got != want {
                bad.push(format!("TEXT({n},{c:?}) = {got:?}, want {want:?}"));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }
}
