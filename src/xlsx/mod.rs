//! A minimal `.xlsx` (Office Open XML SpreadsheetML) reader and writer.
//!
//! Scope: cell values and formulas — what a grid buffer holds. Everything is
//! written from scratch on `std` alone, so the package format is fully under
//! our control and adds no dependencies:
//!
//! - [`crc32`], [`zip`], [`inflate`] and [`deflate`]: the ZIP container.
//! - [`xml`]: a pull tokenizer and the escaping SpreadsheetML needs.
//! - [`read`](mod@read): package → [`Book`] — every worksheet's cells with
//!   cached values and formulas, plus a list of what a rewrite would lose.
//! - [`write`](mod@write): one sheet of cells → a minimal package that Excel,
//!   LibreOffice and Google Sheets open without repair prompts.
//!
//! Formula text is passed through untouched (without the leading `=`); the
//! formula engine owns parsing, and shared formulas are handed to it unexpanded
//! because only its parser can shift their relative references.
//!
//! The reader is honest about loss rather than clever about preservation:
//! [`Book::not_preserved`] names every feature found in the package that
//! [`write`] can't reproduce (styles, merged cells, charts, …), so the editor
//! can warn before a save would drop them. A file this module wrote reads
//! back with that list empty.

pub mod crc32;
pub mod deflate;
pub mod inflate;
pub mod read;
pub mod write;
pub mod xml;
pub mod zip;

use std::fmt;

// The entry points, re-exported for the grid integration (not yet wired).
#[allow(unused_imports)]
pub use read::read;
#[allow(unused_imports)]
pub use write::{WriteCell, write};

/// A cell's value — for a formula cell, the result cached in the file.
#[derive(Clone, Debug, PartialEq)]
pub enum XValue {
    Number(f64),
    Text(String),
    Bool(bool),
    /// An error value as Excel spells it: `"#N/A"`, `"#DIV/0!"`, ….
    Error(String),
    Empty,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum XFormula {
    /// Plain formula text WITHOUT the leading '=' exactly as stored in `<f>`.
    Normal(String),
    /// A shared formula: the master cell carries text + the range it applies
    /// to; other cells only carry the group index. The caller expands them
    /// (it owns the formula parser that translates relative references).
    SharedMaster {
        si: u32,
        text: String,
        range: String,
    },
    SharedChild {
        si: u32,
    },
    /// An array (CSE) formula: text and its range.
    Array {
        text: String,
        range: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct XCell {
    /// 0-based.
    pub row: usize,
    /// 0-based.
    pub col: usize,
    pub value: XValue,
    pub formula: Option<XFormula>,
    /// Index into [`Book::styles`]; 0 is the default (no formatting).
    pub style: usize,
}

/// The cell formatting this module reads and writes: bold, a font color and
/// a solid fill (ARGB), and a number-format code. A file's cell format
/// holding anything more (italics, borders, alignment, a named style, …) is
/// reported in [`Book::not_preserved`] as "cell styles" instead.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct XStyle {
    pub bold: bool,
    pub color: Option<u32>,
    pub fill: Option<u32>,
    /// `None` is General.
    pub num_fmt: Option<String>,
}

impl XStyle {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct XSheet {
    pub name: String,
    /// Row-major order.
    pub cells: Vec<XCell>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Book {
    /// Worksheets in workbook (tab) order. Chart sheets and macro sheets
    /// aren't included; they show up in `not_preserved`.
    pub sheets: Vec<XSheet>,
    pub date1904: bool,
    /// The cell formats cells refer to by [`XCell::style`] (the file's
    /// `cellXfs`, in order; one this module can't represent reads as the
    /// default and is noted in `not_preserved`).
    pub styles: Vec<XStyle>,
    /// Value of `docProps/app.xml` `<Application>`, if any ("Macros" for
    /// files we wrote).
    pub application: Option<String>,
    /// Everything in the package this module would NOT preserve on a
    /// rewrite, as short human-readable reasons ("2 sheets", "charts",
    /// "cell styles", "merged cells", "unknown part xl/foo.xml", …), each
    /// listed once in order of discovery. Empty means a rewrite with
    /// [`write`] loses nothing.
    pub not_preserved: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum XlsxError {
    /// Not a ZIP archive at all.
    NotZip,
    /// A ZIP archive or package part that is damaged or malformed.
    Corrupt(String),
    /// Password-protected (an OLE-wrapped encrypted package, or encrypted
    /// ZIP entries).
    Encrypted,
    /// Valid, but uses something this module doesn't handle (ZIP64, `.xlsb`,
    /// a compression method other than deflate, …).
    Unsupported(String),
}

impl fmt::Display for XlsxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            XlsxError::NotZip => write!(f, "not an .xlsx file (no ZIP archive found)"),
            XlsxError::Corrupt(why) => write!(f, "damaged .xlsx file: {why}"),
            XlsxError::Encrypted => write!(f, "the file is password-protected"),
            XlsxError::Unsupported(what) => write!(f, "unsupported .xlsx feature: {what}"),
        }
    }
}

impl std::error::Error for XlsxError {}

/// Excel's limits: 1,048,576 rows × 16,384 columns (A..XFD).
pub const MAX_ROWS: usize = 1 << 20;
pub const MAX_COLS: usize = 1 << 14;

/// Column letters for a 0-based column: 0 → "A", 27 → "AB".
pub fn col_name(mut col: usize) -> String {
    let mut buf = Vec::new();
    loop {
        buf.push(b'A' + (col % 26) as u8);
        if col < 26 {
            break;
        }
        col = col / 26 - 1;
    }
    buf.reverse();
    String::from_utf8(buf).unwrap_or_default()
}

/// Parse column letters (`A`, `aa`, …) to a 0-based index (any length;
/// `None` past `usize`).
pub fn parse_col_name(s: &str) -> Option<usize> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_alphabetic()) {
        return None;
    }
    let mut n: usize = 0;
    for b in s.bytes() {
        n = n
            .checked_mul(26)?
            .checked_add((b.to_ascii_uppercase() - b'A') as usize + 1)?;
    }
    Some(n - 1)
}

/// `C7` for row 6, column 2.
pub fn cell_name(row: usize, col: usize) -> String {
    format!("{}{}", col_name(col), row + 1)
}

/// Parse an A1 reference ("B3", "$B$3") to 0-based (row, col), within
/// Excel's limits.
pub fn parse_ref(s: &str) -> Option<(usize, usize)> {
    let s = s.trim();
    let s = s.strip_prefix('$').unwrap_or(s);
    let letters = s.bytes().take_while(u8::is_ascii_alphabetic).count();
    if letters == 0 || letters > 3 {
        return None;
    }
    let col = parse_col_name(&s[..letters])? + 1;
    let digits = &s[letters..];
    let digits = digits.strip_prefix('$').unwrap_or(digits);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) || digits.len() > 7 {
        return None;
    }
    let row: usize = digits.parse().ok()?;
    if row == 0 || row > MAX_ROWS || col > MAX_COLS {
        return None;
    }
    Some((row - 1, col - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a1_references() {
        assert_eq!(col_name(0), "A");
        assert_eq!(col_name(25), "Z");
        assert_eq!(col_name(26), "AA");
        assert_eq!(col_name(701), "ZZ");
        assert_eq!(col_name(702), "AAA");
        assert_eq!(col_name(MAX_COLS - 1), "XFD");
        for col in [0, 1, 25, 26, 51, 52, 701, 702, 16383] {
            assert_eq!(parse_ref(&format!("{}7", col_name(col))), Some((6, col)));
        }
        assert_eq!(parse_ref("b3"), Some((2, 1)));
        assert_eq!(parse_ref("$C$10"), Some((9, 2)));
        assert_eq!(parse_ref("XFD1048576"), Some((MAX_ROWS - 1, MAX_COLS - 1)));
        for bad in [
            "", "A", "1", "A0", "XFE1", "A1048577", "AAAA1", "A1B", "A-1", "A+1",
        ] {
            assert_eq!(parse_ref(bad), None, "{bad}");
        }
    }
}
