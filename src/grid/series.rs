//! Continuing a series down a column: org-table's copy-down (`S-RET`) and
//! filling a selection. Pure functions over cell text.
//!
//! A series continues from the last value and, when there is one, the value
//! before it (which sets the step): numbers (keeping their decimals and any
//! zero padding), ISO dates (by days), weekday and month names (keeping
//! their case and abbreviation), and text ending in a number (`Item 9` →
//! `Item 10`). Anything else repeats unchanged.

use crate::formula::date::{civil_from_days, days_from_civil};

const WEEKDAYS: [&str; 7] = [
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
];
const MONTHS: [&str; 12] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];

/// The value `n` steps after `cur` in the series `prev, cur, …` (`prev` is
/// the value before `cur`, if any). `n = 1` is the very next value.
pub fn next_in_series(prev: Option<&str>, cur: &str, n: i64) -> String {
    let prev = prev.filter(|p| !p.trim().is_empty());
    if let Some(next) = number_series(prev, cur, n)
        .or_else(|| date_series(prev, cur, n))
        .or_else(|| name_series(prev, cur, n))
        .or_else(|| suffix_series(prev, cur, n))
    {
        return next;
    }
    cur.to_string()
}

/// `(value, decimals, zero-padded width)` for a plain number: an optional
/// sign, digits, and an optional fraction. No exponents, no grouping.
fn number(s: &str) -> Option<(f64, usize, Option<usize>)> {
    let t = s.trim();
    let unsigned = t.strip_prefix(['-', '+']).unwrap_or(t);
    let (int, frac) = match unsigned.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (unsigned, None),
    };
    let digits = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
    if !digits(int) || frac.is_some_and(|f| !digits(f)) {
        return None;
    }
    let pad = (frac.is_none() && int.len() > 1 && int.starts_with('0')).then_some(int.len());
    Some((t.parse().ok()?, frac.map_or(0, str::len), pad))
}

fn number_series(prev: Option<&str>, cur: &str, n: i64) -> Option<String> {
    let (v, dec, pad) = number(cur)?;
    let prev = prev.and_then(|p| Some((p, number(p)?)));
    // Whole numbers step exactly (an ID past 2^53 would drift in f64).
    let int = |s: &str| s.trim().parse::<i128>().ok();
    if dec == 0
        && prev.is_none_or(|(_, (_, pdec, _))| pdec == 0)
        && let Some(iv) = int(cur)
    {
        let step = prev.map_or(Some(1), |(p, _)| iv.checked_sub(int(p)?));
        if let Some(next) = step.and_then(|s| s.checked_mul(i128::from(n))?.checked_add(iv)) {
            return Some(match pad {
                Some(width) if next >= 0 => format!("{next:0width$}"),
                _ => next.to_string(),
            });
        }
    }
    let (step, dec) = match prev {
        Some((_, (p, pdec, _))) => (v - p, dec.max(pdec)),
        None => (1.0, dec),
    };
    let next = v + step * n as f64;
    Some(match pad {
        Some(width) if dec == 0 && next >= 0.0 => format!("{:0width$.0}", next),
        _ => format!("{next:.dec$}"),
    })
}

/// `YYYY-MM-DD` (or with `/`) as days, plus its separator.
fn iso_date(s: &str) -> Option<(i64, char)> {
    let t = s.trim();
    let b = t.as_bytes();
    if b.len() != 10 || !matches!(b[4], b'-' | b'/') || b[7] != b[4] {
        return None;
    }
    let sep = b[4] as char;
    let mut it = t.split(sep).map(|p| p.parse::<i64>().ok());
    let (y, m, d) = (it.next()??, it.next()??, it.next()??);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some((days_from_civil(y, m as u32, d as u32), sep))
}

fn date_series(prev: Option<&str>, cur: &str, n: i64) -> Option<String> {
    let (days, sep) = iso_date(cur)?;
    let step = prev.and_then(iso_date).map_or(1, |(p, _)| days - p);
    let (y, m, d) = civil_from_days(days + step * n);
    Some(format!("{y:04}{sep}{m:02}{sep}{d:02}"))
}

/// A weekday or month name: `(table, index, abbreviated)`.
fn name(s: &str) -> Option<(&'static [&'static str], usize, bool)> {
    let lower = s.trim().to_lowercase();
    if lower.len() < 3 {
        return None;
    }
    for table in [&WEEKDAYS[..], &MONTHS[..]] {
        if let Some(i) = table.iter().position(|n| *n == lower) {
            return Some((table, i, false));
        }
        if lower.len() == 3
            && let Some(i) = table.iter().position(|n| n.starts_with(&lower))
        {
            return Some((table, i, true));
        }
    }
    None
}

fn name_series(prev: Option<&str>, cur: &str, n: i64) -> Option<String> {
    let (table, i, abbrev) = name(cur)?;
    let len = table.len() as i64;
    let step = prev
        .and_then(name)
        .filter(|(t, _, _)| std::ptr::eq(*t, table))
        .map_or(1, |(_, p, _)| i as i64 - p as i64);
    let next = table[(i as i64 + step * n).rem_euclid(len) as usize];
    let next = if abbrev { &next[..3] } else { next };
    let t = cur.trim();
    Some(if t.chars().all(|c| !c.is_lowercase()) {
        next.to_uppercase()
    } else if t.chars().next().is_some_and(char::is_uppercase) {
        let mut out = next[..1].to_uppercase();
        out.push_str(&next[1..]);
        out
    } else {
        next.to_string()
    })
}

/// `(prefix, number, digit count)` for text ending in digits after a
/// non-digit prefix.
fn suffix_number(s: &str) -> Option<(&str, u64, usize)> {
    let digits = s.bytes().rev().take_while(u8::is_ascii_digit).count();
    if digits == 0 || digits == s.len() || digits > 18 {
        return None;
    }
    let (prefix, num) = s.split_at(s.len() - digits);
    Some((prefix, num.parse().ok()?, digits))
}

fn suffix_series(prev: Option<&str>, cur: &str, n: i64) -> Option<String> {
    let (prefix, v, width) = suffix_number(cur)?;
    let step = prev
        .and_then(suffix_number)
        .filter(|(p, _, _)| *p == prefix)
        .map_or(Some(1), |(_, p, _)| (v as i64).checked_sub(p as i64))?;
    // Values run up to 10^18: an overflowing step or fill stops the series
    // (the caller repeats the value) instead of panicking / wrapping.
    let next = step.checked_mul(n)?.checked_add(v as i64)?.max(0);
    let padded = cur[prefix.len()..].starts_with('0');
    Some(if padded {
        format!("{prefix}{next:0width$}")
    } else {
        format!("{prefix}{next}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_series_numbers_dates_names_and_suffixes() {
        let next = |p: Option<&str>, c: &str| next_in_series(p, c, 1);
        assert_eq!(next(None, "7"), "8");
        assert_eq!(next(Some("5"), "10"), "15");
        assert_eq!(next(Some("1.5"), "2"), "2.5");
        assert_eq!(next(None, "0.25"), "1.25");
        assert_eq!(next(None, "007"), "008");
        // A fill that would overflow i64 stops the series instead of panicking.
        assert_eq!(suffix_series(Some("a0"), "a999999999999999999", 10), None);
        assert_eq!(next(Some("3"), "1"), "-1");
        assert_eq!(next(None, "2024-02-28"), "2024-02-29");
        assert_eq!(next(Some("2024-01-01"), "2024-01-08"), "2024-01-15");
        assert_eq!(next(None, "2023-12-31"), "2024-01-01");
        assert_eq!(next(None, "Mon"), "Tue");
        assert_eq!(next(None, "SUNDAY"), "MONDAY");
        assert_eq!(next(Some("Jan"), "Mar"), "May");
        assert_eq!(next(None, "december"), "january");
        assert_eq!(next(None, "Item 9"), "Item 10");
        assert_eq!(next(Some("v1"), "v3"), "v5");
        assert_eq!(next(None, "file007"), "file008");
        assert_eq!(next(None, "hello"), "hello");
        assert_eq!(next(None, ""), "");
        assert_eq!(next_in_series(Some("1"), "2", 3), "5");
        assert_eq!(next(Some("may"), "Maybe"), "Maybe");
    }
}
