//! Column type inference.
//!
//! The type only changes how cells are shown and compared (numbers
//! right-aligned and sorted numerically); the stored and saved text is always
//! exactly what was typed. So a wrong guess is cosmetic, and a cheap sample is
//! enough: up to [`SAMPLE`] non-empty cells, accepting a type when at least 95%
//! of them match it.

use std::ops::ControlFlow;

use super::intern::StringPool;
use super::sheet::Sheet;

/// Non-empty cells sampled per column.
pub const SAMPLE: usize = 1000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ColType {
    Int,
    Float,
    Bool,
    Date,
    #[default]
    Text,
}

impl ColType {
    /// The name Scheme and the mode line use: `int`, `float`, `bool`,
    /// `date`, `text`.
    pub fn name(self) -> &'static str {
        match self {
            ColType::Int => "int",
            ColType::Float => "float",
            ColType::Bool => "bool",
            ColType::Date => "date",
            ColType::Text => "text",
        }
    }

    /// Int or Float: right-aligned, sorted by value.
    pub fn is_numeric(self) -> bool {
        matches!(self, ColType::Int | ColType::Float)
    }
}

fn all_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// `YYYY-MM-DD` or `YYYY/MM/DD`, optionally followed by a `T`/space time part.
fn is_iso_date(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() < 10 {
        return false;
    }
    let sep = b[4];
    if !(sep == b'-' || sep == b'/') || b[7] != sep {
        return false;
    }
    // Check bytes before slicing: a non-ASCII value may not split at 4/7/10.
    if ![0, 1, 2, 3, 5, 6, 8, 9]
        .iter()
        .all(|&i| b[i].is_ascii_digit())
    {
        return false;
    }
    let (m, d) = (&s[5..7], &s[8..10]);
    let (m, d): (u32, u32) = (m.parse().unwrap_or(0), d.parse().unwrap_or(0));
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return false;
    }
    match b.get(10) {
        None => true,
        Some(b'T' | b' ') => s[11..]
            .bytes()
            .all(|c| c.is_ascii_digit() || b":.+-Z".contains(&c)),
        Some(_) => false,
    }
}

/// `M/D/YYYY`, `D/M/YY`, `D.M.YYYY` and friends (US and European order).
fn is_short_date(s: &str) -> bool {
    let sep = if s.contains('/') { '/' } else { '.' };
    let parts: Vec<&str> = s.split(sep).collect();
    if parts.len() != 3 || !parts.iter().all(|p| all_digits(p)) {
        return false;
    }
    let (a, b, y) = (parts[0].len(), parts[1].len(), parts[2].len());
    (1..=2).contains(&a) && (1..=2).contains(&b) && (y == 2 || y == 4)
}

/// The type of one non-empty value.
pub fn classify(value: &str) -> ColType {
    let s = value.trim();
    if s.is_empty() {
        return ColType::Text;
    }
    if s.eq_ignore_ascii_case("true") || s.eq_ignore_ascii_case("false") {
        return ColType::Bool;
    }
    let unsigned = s.strip_prefix(['+', '-']).unwrap_or(s);
    if all_digits(unsigned) {
        return ColType::Int;
    }
    // Restrict the alphabet first: Rust's float parser also accepts "inf",
    // "NaN" and "infinity", which are words, not numbers, in a spreadsheet.
    if s.bytes().any(|b| b.is_ascii_digit())
        && s.bytes()
            .all(|b| b.is_ascii_digit() || b"+-.eE".contains(&b))
        && s.parse::<f64>().is_ok()
    {
        return ColType::Float;
    }
    if is_iso_date(s) || is_short_date(s) {
        return ColType::Date;
    }
    ColType::Text
}

/// The type of a set of samples under the 95% rule. Ints count toward Float
/// (a column of `1, 2.5, 3` is Float).
pub fn decide(counts: &TypeCounts) -> ColType {
    let n = counts.total;
    let hit = |k: usize| n > 0 && k * 100 >= n * 95;
    if hit(counts.int) {
        ColType::Int
    } else if hit(counts.int + counts.float) {
        ColType::Float
    } else if hit(counts.bool) {
        ColType::Bool
    } else if hit(counts.date) {
        ColType::Date
    } else {
        ColType::Text
    }
}

#[derive(Default, Debug)]
pub struct TypeCounts {
    pub total: usize,
    pub int: usize,
    pub float: usize,
    pub bool: usize,
    pub date: usize,
}

impl TypeCounts {
    pub fn add(&mut self, value: &str) {
        self.total += 1;
        match classify(value) {
            ColType::Int => self.int += 1,
            ColType::Float => self.float += 1,
            ColType::Bool => self.bool += 1,
            ColType::Date => self.date += 1,
            ColType::Text => {}
        }
    }
}

/// Infer column `col` of `sheet` from its first [`SAMPLE`] non-empty cells,
/// skipping the header row when the sheet has one.
pub fn infer_column(sheet: &Sheet, pool: &StringPool, col: usize) -> ColType {
    let mut counts = TypeCounts::default();
    let start = usize::from(sheet.header());
    sheet.try_for_each_in_column(col, start..sheet.rows(), |_, cell| {
        if !cell.is_empty() {
            counts.add(cell.get(sheet.source(), pool));
            if counts.total >= SAMPLE {
                return ControlFlow::Break(());
            }
        }
        ControlFlow::Continue(())
    });
    decide(&counts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_classify_values() {
        assert_eq!(classify("42"), ColType::Int);
        assert_eq!(classify("-007"), ColType::Int);
        assert_eq!(classify(" 12 "), ColType::Int);
        assert_eq!(classify("1e5"), ColType::Float);
        assert_eq!(classify("-3.25"), ColType::Float);
        assert_eq!(classify("inf"), ColType::Text);
        assert_eq!(classify("NaN"), ColType::Text);
        assert_eq!(classify("TRUE"), ColType::Bool);
        assert_eq!(classify("false"), ColType::Bool);
        assert_eq!(classify("2024-02-29"), ColType::Date);
        assert_eq!(classify("2024-02-29T10:30:00Z"), ColType::Date);
        assert_eq!(classify("12/31/2023"), ColType::Date);
        assert_eq!(classify("31.12.2023"), ColType::Date);
        assert_eq!(classify("2024-13-01"), ColType::Text);
        assert_eq!(classify("1.2.3.4"), ColType::Text);
        assert_eq!(classify("hello"), ColType::Text);
        assert_eq!(classify("12abc"), ColType::Text);
    }

    #[test]
    fn grid_decide_threshold() {
        let mut c = TypeCounts::default();
        for _ in 0..95 {
            c.add("1");
        }
        for _ in 0..5 {
            c.add("x");
        }
        assert_eq!(decide(&c), ColType::Int);
        c.add("y");
        assert_eq!(decide(&c), ColType::Text);
        let mut f = TypeCounts::default();
        for v in ["1", "2.5", "3", "1e3"] {
            f.add(v);
        }
        assert_eq!(decide(&f), ColType::Float);
        assert_eq!(decide(&TypeCounts::default()), ColType::Text);
    }
}
