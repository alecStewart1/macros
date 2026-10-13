//! A1 reference notation (`C7`, `$B$3`, column letters) and Excel's sheet
//! size, shared by the formula engine and the `.xlsx` reader and writer.

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
