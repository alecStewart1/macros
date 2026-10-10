//! The criteria mini-language of `COUNTIF`, `SUMIFS` and friends, and the
//! wildcard matcher they share with `SEARCH`, `MATCH`, `VLOOKUP` and
//! `XLOOKUP`.
//!
//! A criterion is a value. Text may start with an operator — `=`, `<>`, `<`,
//! `>`, `<=`, `>=` — followed by the operand, which is read like typed input
//! (`">=10"`, `"<>apple"`, `"<1/1/2024"`, `"=TRUE"`). Following Excel:
//!
//! - **Numbers** compare numerically. For equality (`5`, `"5"`, `"=5"`,
//!   `"<>5"`) a text cell that reads as the same number also matches — Excel
//!   converts such cells, which is why `COUNTIF` famously treats long digit
//!   strings that agree in their first 15 digits as equal. Ordering criteria
//!   (`">5"`) only match real numbers.
//! - **Text** equality is case-insensitive with wildcards (`*` any run, `?`
//!   one character, `~` escaping either or itself); it never matches numbers.
//!   Ordering against text compares text cells case-insensitively, without
//!   wildcards.
//! - **Booleans and errors** match only themselves (`"TRUE"`, `"#N/A"`).
//! - `""` matches empty cells and empty text; `"="` matches only truly empty
//!   cells; `"<>"` matches every non-empty cell (an empty-text formula result
//!   counts as non-empty).
//! - A criterion that is an empty cell is treated as 0 (documented for
//!   `COUNTIFS`).
//! - `<>x` matches everything that `=x` doesn't, including empty cells.

use std::cmp::Ordering;

use super::date;
use super::value::{ErrorKind, Value, cmp_text, parse_number};

/// The case folding used for matching: one character in, one out, so
/// character positions survive (`SEARCH` reports positions in the original).
pub fn fold(c: char) -> char {
    if c.is_ascii() {
        c.to_ascii_lowercase()
    } else {
        c.to_lowercase().next().unwrap_or(c)
    }
}

/// Does `s` contain an unescaped wildcard (or a `~` escape)? Lookups use this
/// to decide between a hash probe and a pattern scan.
pub fn has_wildcards(s: &str) -> bool {
    s.contains(['*', '?', '~'])
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Lit(char),
    Any,
}

/// A compiled wildcard pattern (`*`, `?`, `~` escapes), matched
/// case-insensitively. Matching never backtracks: the pattern is split at
/// its stars into segments, and each segment is placed at its leftmost
/// possible position, which is optimal for `*`-separated segments — so the
/// cost is O(text × pattern) in the worst case, never exponential.
#[derive(Clone, Debug)]
pub struct Pattern {
    /// Segments between `*`s.
    segs: Vec<Vec<Tok>>,
    /// Pattern starts / ends with `*`.
    lead_star: bool,
    trail_star: bool,
}

impl Pattern {
    pub fn new(pat: &str) -> Pattern {
        let mut segs = vec![Vec::new()];
        let mut lead_star = false;
        let mut trail_star = false;
        let mut chars = pat.chars().peekable();
        let mut first = true;
        while let Some(c) = chars.next() {
            trail_star = false;
            match c {
                '*' => {
                    if first {
                        lead_star = true;
                    }
                    if !segs.last().is_some_and(Vec::is_empty) {
                        segs.push(Vec::new());
                    }
                    trail_star = true;
                }
                '?' => segs.last_mut().unwrap().push(Tok::Any),
                '~' if matches!(chars.peek(), Some('*' | '?' | '~')) => {
                    let e = chars.next().unwrap();
                    segs.last_mut().unwrap().push(Tok::Lit(e));
                }
                c => segs.last_mut().unwrap().push(Tok::Lit(fold(c))),
            }
            first = false;
        }
        if segs.len() > 1 && segs.last().is_some_and(Vec::is_empty) {
            segs.pop();
        }
        Pattern {
            segs,
            lead_star,
            trail_star,
        }
    }

    fn seg_at(seg: &[Tok], text: &[char], at: usize) -> bool {
        at + seg.len() <= text.len()
            && seg.iter().zip(&text[at..]).all(|(t, &c)| match t {
                Tok::Any => true,
                Tok::Lit(l) => *l == c,
            })
    }

    fn seg_find(seg: &[Tok], text: &[char], from: usize) -> Option<usize> {
        if seg.len() > text.len() {
            return None;
        }
        (from..=text.len() - seg.len()).find(|&i| Self::seg_at(seg, text, i))
    }

    /// Match all of `text` (already folded), with `anchor_end` false meaning
    /// the pattern need only match a prefix (an implicit trailing `*`).
    fn match_chars(&self, text: &[char], anchor_end: bool) -> bool {
        let trail = self.trail_star || !anchor_end;
        let n = self.segs.len();
        let mut pos = 0;
        for (i, seg) in self.segs.iter().enumerate() {
            let first = i == 0;
            let last = i == n - 1;
            if first && !self.lead_star {
                if !Self::seg_at(seg, text, 0) {
                    return false;
                }
                pos = seg.len();
                if last && !trail {
                    return pos == text.len();
                }
                continue;
            }
            if last && !trail {
                // Anchor the final segment at the end.
                if text.len() < pos + seg.len() {
                    return false;
                }
                return Self::seg_at(seg, text, text.len() - seg.len());
            }
            match Self::seg_find(seg, text, pos) {
                Some(p) => pos = p + seg.len(),
                None => return false,
            }
        }
        true
    }

    /// Does the whole of `text` match?
    pub fn matches(&self, text: &str) -> bool {
        let t: Vec<char> = text.chars().map(fold).collect();
        self.match_chars(&t, true)
    }

    /// The first character index at or after `start` where the pattern
    /// matches a prefix of the rest of `text` (`SEARCH`).
    pub fn find(&self, text: &str, start: usize) -> Option<usize> {
        let t: Vec<char> = text.chars().map(fold).collect();
        (start..=t.len()).find(|&i| self.match_chars(&t[i..], false))
    }
}

/// A comparison operator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl Op {
    fn test(self, o: Ordering) -> bool {
        match self {
            Op::Eq => o == Ordering::Equal,
            Op::Ne => o != Ordering::Equal,
            Op::Lt => o == Ordering::Less,
            Op::Le => o != Ordering::Greater,
            Op::Gt => o == Ordering::Greater,
            Op::Ge => o != Ordering::Less,
        }
    }
}

#[derive(Clone, Debug)]
enum Operand {
    Number(f64),
    Bool(bool),
    Error(ErrorKind),
    /// Text, with its wildcard pattern for equality.
    Text(String, Pattern),
    /// `""`: empty cells and empty text (with `=`), the reverse with `<>`.
    Blank,
    /// `"="`: truly empty cells only.
    EmptyOnly,
}

/// A parsed criterion.
#[derive(Clone, Debug)]
pub struct Criterion {
    op: Op,
    operand: Operand,
}

impl Criterion {
    /// Read a criterion value. Partial dates (`"<3/1"`) need a year and so
    /// stay text; full dates (`">=3/1/2024"`) compare as their serials in
    /// the 1900 system — use [`Criterion::parse_with`] to choose.
    #[cfg(test)]
    pub fn parse(v: &Value) -> Criterion {
        Self::parse_with(v, false)
    }

    /// [`Criterion::parse`] for the given date system.
    pub fn parse_with(v: &Value, date1904: bool) -> Criterion {
        let (op, operand) = match v {
            Value::Number(n) => (Op::Eq, Operand::Number(*n)),
            Value::Empty => (Op::Eq, Operand::Number(0.0)),
            Value::Bool(b) => (Op::Eq, Operand::Bool(*b)),
            Value::Error(e) => (Op::Eq, Operand::Error(*e)),
            Value::Text(s) => {
                let (op, rest) = if let Some(r) = s.strip_prefix("<=") {
                    (Op::Le, r)
                } else if let Some(r) = s.strip_prefix(">=") {
                    (Op::Ge, r)
                } else if let Some(r) = s.strip_prefix("<>") {
                    (Op::Ne, r)
                } else if let Some(r) = s.strip_prefix('<') {
                    (Op::Lt, r)
                } else if let Some(r) = s.strip_prefix('>') {
                    (Op::Gt, r)
                } else if let Some(r) = s.strip_prefix('=') {
                    (Op::Eq, r)
                } else {
                    (Op::Eq, &s[..])
                };
                let explicit_eq = s.starts_with('=');
                let operand = if rest.is_empty() {
                    if explicit_eq {
                        Operand::EmptyOnly
                    } else {
                        Operand::Blank
                    }
                } else {
                    read_operand(rest, date1904)
                };
                (op, operand)
            }
        };
        Criterion { op, operand }
    }

    /// Does a cell value satisfy the criterion?
    pub fn matches(&self, cell: &Value) -> bool {
        let op = self.op;
        match &self.operand {
            Operand::EmptyOnly => match op {
                Op::Eq => matches!(cell, Value::Empty),
                _ => !matches!(cell, Value::Empty),
            },
            Operand::Blank => {
                let blank = match cell {
                    Value::Empty => true,
                    Value::Text(s) => s.is_empty(),
                    _ => false,
                };
                match op {
                    Op::Eq => blank,
                    // "<>": any non-empty cell, "" text included.
                    Op::Ne => !matches!(cell, Value::Empty),
                    // "<" / ">" with nothing: compare text against "".
                    _ => match cell {
                        Value::Text(s) => op.test(cmp_text(s, "")),
                        _ => false,
                    },
                }
            }
            Operand::Number(n) => {
                let cell_num = match cell {
                    Value::Number(x) => Some(*x),
                    Value::Text(s) if matches!(op, Op::Eq | Op::Ne) => parse_number(s.trim()),
                    _ => None,
                };
                match (op, cell_num) {
                    (Op::Ne, None) => true,
                    (_, None) => false,
                    (op, Some(x)) => op.test(x.partial_cmp(n).unwrap_or(Ordering::Equal)),
                }
            }
            Operand::Bool(b) => match cell {
                Value::Bool(x) => op.test(x.cmp(b)),
                _ => op == Op::Ne,
            },
            Operand::Error(e) => match cell {
                Value::Error(x) => (x == e) == (op != Op::Ne),
                _ => op == Op::Ne,
            },
            Operand::Text(t, pat) => match op {
                Op::Eq | Op::Ne => {
                    let hit = match cell {
                        Value::Text(s) => pat.matches(s),
                        _ => false,
                    };
                    hit == (op == Op::Eq)
                }
                _ => match cell {
                    Value::Text(s) => op.test(cmp_text(s, t)),
                    _ => false,
                },
            },
        }
    }

    /// Whether an empty cell matches (lets callers count the empty cells
    /// outside the used area without visiting them).
    pub fn matches_empty(&self) -> bool {
        self.matches(&Value::Empty)
    }
}

fn read_operand(s: &str, date1904: bool) -> Operand {
    if let Some(n) = parse_number(s) {
        return Operand::Number(n);
    }
    if s.eq_ignore_ascii_case("TRUE") {
        return Operand::Bool(true);
    }
    if s.eq_ignore_ascii_case("FALSE") {
        return Operand::Bool(false);
    }
    if s.starts_with('#')
        && let Some(e) = ErrorKind::parse(s)
    {
        return Operand::Error(e);
    }
    if s.bytes().any(|b| b.is_ascii_digit())
        && let Some(dt) = date::parse_datetime(s, None, date1904)
    {
        return Operand::Number(dt.serial());
    }
    Operand::Text(s.to_string(), Pattern::new(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(crit: Value, cell: Value) -> bool {
        Criterion::parse(&crit).matches(&cell)
    }
    fn t(s: &str) -> Value {
        Value::text(s)
    }
    fn n(x: f64) -> Value {
        Value::Number(x)
    }

    #[test]
    fn wildcards() {
        let p = |pat: &str, s: &str| Pattern::new(pat).matches(s);
        assert!(p("a*", "apple"));
        assert!(p("A*", "apple"));
        assert!(!p("a*", "banana"));
        assert!(p("*an*", "banana"));
        assert!(p("b?n?n?", "banana"));
        assert!(!p("b?n", "banana"));
        assert!(p("*", ""));
        assert!(p("**", "x"));
        assert!(!p("?", ""));
        assert!(p("*a", "banana"));
        assert!(p("*a*a*a", "banana"));
        assert!(!p("*a*a*a*a", "banana"));
        assert!(p("~*", "*"));
        assert!(!p("~*", "x"));
        assert!(p("wh~?", "wh?"));
        assert!(!p("wh~?", "why"));
        assert!(p("a~~b", "a~b"));
        assert!(p("~x", "~x"));
        assert!(p("ÄB*", "äbc"));
        assert!(p("a*b*c", "aXbYc"));
        assert!(!p("a*b*c", "aXbYcZ"));
        assert!(p("abc", "ABC"));
        assert!(!p("abc", "abcd"));
        // Pathological input stays fast.
        let long = "a".repeat(5000);
        assert!(!p(&("*a".repeat(50) + "b"), &long));
    }

    #[test]
    fn wildcard_find() {
        assert_eq!(Pattern::new("b?n").find("Banana", 0), Some(0));
        assert_eq!(Pattern::new("n").find("Banana", 3), Some(4));
        assert_eq!(Pattern::new("a*a").find("xbaba", 0), Some(2));
        assert_eq!(Pattern::new("z").find("banana", 0), None);
        assert_eq!(Pattern::new("").find("abc", 1), Some(1));
    }

    #[test]
    fn numbers() {
        assert!(m(n(5.0), n(5.0)));
        assert!(m(n(5.0), t("5")));
        assert!(m(t("5"), n(5.0)));
        assert!(m(t("=5"), t("5")));
        assert!(!m(n(5.0), Value::Bool(true)));
        assert!(m(t(">=10"), n(10.0)));
        assert!(!m(t(">=10"), n(9.99)));
        assert!(!m(t(">5"), t("6")));
        assert!(!m(t(">5"), Value::Empty));
        assert!(m(t("<>5"), Value::Empty));
        assert!(!m(t("<>5"), t("5")));
        assert!(m(t("<>5"), t("x")));
        assert!(m(t("<0"), n(-1.0)));
        assert!(m(t("<=1e3"), n(1000.0)));
        assert!(m(t("50%"), n(0.5)));
        assert!(m(Value::Empty, n(0.0)));
        assert!(!m(Value::Empty, Value::Empty));
    }

    #[test]
    fn text_and_specials() {
        assert!(m(t("apples"), t("APPLES")));
        assert!(!m(t("apples"), t("apple")));
        assert!(m(t("=app*"), t("Apples")));
        assert!(m(t("<>app*"), t("pears")));
        assert!(!m(t("<>app*"), t("apple")));
        assert!(m(t("<>app*"), Value::Empty));
        assert!(!m(t("a*"), n(1.0)));
        assert!(m(t(">b"), t("C")));
        assert!(!m(t(">b"), t("a")));
        assert!(!m(t(">b"), n(5.0)));
        assert!(m(t(""), Value::Empty));
        assert!(m(t(""), t("")));
        assert!(!m(t(""), t("x")));
        assert!(m(t("="), Value::Empty));
        assert!(!m(t("="), t("")));
        assert!(m(t("<>"), t("x")));
        assert!(m(t("<>"), t("")));
        assert!(!m(t("<>"), Value::Empty));
        assert!(m(t("TRUE"), Value::Bool(true)));
        assert!(!m(t("TRUE"), t("TRUE")));
        assert!(m(Value::Bool(false), Value::Bool(false)));
        assert!(m(t("#N/A"), Value::Error(ErrorKind::NA)));
        assert!(!m(t("#N/A"), Value::Error(ErrorKind::Div0)));
        assert!(m(t("<>#N/A"), n(1.0)));
        assert!(m(t("~*"), t("*")));
        assert!(!m(t("~*"), t("x")));
    }

    #[test]
    fn dates_in_criteria() {
        // 2024-01-01 is serial 45292.
        assert!(m(t(">=1/1/2024"), n(45292.0)));
        assert!(!m(t(">1/1/2024"), n(45292.0)));
        assert!(m(t("<2024-01-02"), n(45292.0)));
    }
}
