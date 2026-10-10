//! Excel serial dates: calendar conversion, the 1900 and 1904 date systems,
//! month arithmetic, weekdays, and parsing of typed dates and times.
//!
//! A serial is a day count with the time of day as its fraction. In the 1900
//! system serial 1 is 1900-01-01 and, as in Excel (which copied a Lotus 1-2-3
//! bug), serial 60 is the non-existent 1900-02-29, so every serial from 61
//! (1900-03-01) on agrees with Excel. Serial 0 is Excel's "1900-01-00". In the
//! 1904 system serial 0 is 1904-01-01 and there is no fake day.

/// The largest serial Excel accepts: 9999-12-31 in the 1900 system.
pub const MAX_SERIAL_1900: f64 = 2_958_465.0;
/// 9999-12-31 in the 1904 system.
pub const MAX_SERIAL_1904: f64 = 2_957_003.0;

/// The largest valid serial in the given system.
pub fn max_serial(date1904: bool) -> f64 {
    if date1904 {
        MAX_SERIAL_1904
    } else {
        MAX_SERIAL_1900
    }
}

/// Days since 1970-01-01 of the proleptic Gregorian date `y-m-d` (Howard
/// Hinnant's `days_from_civil`). Out-of-range days roll over arithmetically
/// within the month's era (`m` must be 1..=12).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let m = m as i64;
    let mp = if m > 2 { m - 3 } else { m + 9 }; // [0, 11], March = 0
    let doy = (153 * mp + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// The proleptic Gregorian date of a day count since 1970-01-01 (Howard
/// Hinnant's `civil_from_days`).
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Gregorian leap year.
pub fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Days in month `m` of year `y` (real calendar: February 1900 has 28).
pub fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if is_leap(y) => 29,
        _ => 28,
    }
}

/// Days in month as Excel's 1900 system sees it: February 1900 has 29.
pub fn days_in_month_excel(y: i64, m: u32, date1904: bool) -> u32 {
    if !date1904 && y == 1900 && m == 2 {
        29
    } else {
        days_in_month(y, m)
    }
}

const DAYS_1899_12_31: i64 = -25_568; // days_from_civil(1899, 12, 31)
const DAYS_1904_01_01: i64 = -24_107; // days_from_civil(1904, 1, 1)

/// The serial (whole days) of a valid calendar date, or `None` before the
/// system's epoch. `1900-02-29` is serial 60 in the 1900 system.
pub fn ymd_to_serial(y: i64, m: u32, d: u32, date1904: bool) -> Option<i64> {
    if date1904 {
        let s = days_from_civil(y, m, d) - DAYS_1904_01_01;
        return (s >= 0).then_some(s);
    }
    if (y, m, d) == (1900, 2, 29) {
        return Some(60);
    }
    let s = days_from_civil(y, m, d) - DAYS_1899_12_31;
    if s < 0 {
        None
    } else if s >= 60 {
        Some(s + 1)
    } else {
        Some(s)
    }
}

/// The calendar date of a serial's day part. Serial 0 in the 1900 system is
/// Excel's `1900-01-00`; 60 is `1900-02-29`. `None` for negative serials or
/// past 9999-12-31.
pub fn serial_to_ymd(serial: f64, date1904: bool) -> Option<(i64, u32, u32)> {
    if !serial.is_finite() || serial < 0.0 || serial >= max_serial(date1904) + 1.0 {
        return None;
    }
    let s = serial.floor() as i64;
    if date1904 {
        return Some(civil_from_days(DAYS_1904_01_01 + s));
    }
    match s {
        0 => Some((1900, 1, 0)),
        60 => Some((1900, 2, 29)),
        s if s < 60 => Some(civil_from_days(DAYS_1899_12_31 + s)),
        s => Some(civil_from_days(DAYS_1899_12_31 + s - 1)),
    }
}

/// Excel's `DATE(year, month, day)` with its normalisation: a year in
/// 0..=1899 has 1900 added; months outside 1..=12 roll the year; days
/// outside the month roll the month (`DATE(2024,1,32)` is 2024-02-01,
/// `DATE(2024,3,0)` is 2024-02-29). `None` (`#NUM!`) outside the valid range.
pub fn date_serial(year: i64, month: i64, day: i64, date1904: bool) -> Option<f64> {
    let year = if (0..1900).contains(&year) {
        year + 1900
    } else {
        year
    };
    serial_from_parts(year, month, day, date1904)
}

/// [`date_serial`] without the two-digit-year rule: `year` is taken as
/// is (months and days still roll over). `None` before the system's epoch
/// or after 9999-12-31.
pub fn serial_from_parts(year: i64, month: i64, day: i64, date1904: bool) -> Option<f64> {
    if !(0..10_000).contains(&year) {
        return None;
    }
    let m0 = month - 1;
    let y = year + m0.div_euclid(12);
    let m = (m0.rem_euclid(12) + 1) as u32;
    if !(0..=10_000).contains(&y) {
        return None;
    }
    let first = month_start_serial(y, m, date1904)?;
    let s = first + day - 1;
    (s >= 0 && s as f64 <= max_serial(date1904)).then_some(s as f64)
}

/// Serial of the first of a month, allowed to be negative (for 1904-system
/// arithmetic before the epoch, which is then rejected by the caller).
fn month_start_serial(y: i64, m: u32, date1904: bool) -> Option<i64> {
    if date1904 {
        return Some(days_from_civil(y, m, 1) - DAYS_1904_01_01);
    }
    let s = days_from_civil(y, m, 1) - DAYS_1899_12_31;
    // Every month start from March 1900 on sits after the fake 29 Feb.
    Some(if s >= 60 { s + 1 } else { s })
}

/// Move `y-m` by `months`, returning the new year and month.
pub fn add_months(y: i64, m: u32, months: i64) -> (i64, u32) {
    let total = y * 12 + (m as i64 - 1) + months;
    (total.div_euclid(12), (total.rem_euclid(12) + 1) as u32)
}

/// `EDATE`: the serial `months` after `serial`, clamping the day to the
/// target month's end (Jan 31 + 1 month is Feb 28/29).
pub fn edate(serial: f64, months: i64, date1904: bool) -> Option<f64> {
    let (y, m, d) = serial_to_ymd(serial, date1904)?;
    let (ny, nm) = add_months(y, m, months);
    let d = d.max(1).min(days_in_month_excel(ny, nm, date1904));
    // Excel's 1900-01-00 (serial 0) moves as day 0 would.
    let d = if serial.floor() == 0.0 && !date1904 {
        0
    } else {
        d
    };
    let s = serial_from_parts(ny, nm as i64, d as i64, date1904)?;
    Some(s)
}

/// `EOMONTH`: the last day of the month `months` after `serial`'s month.
pub fn eomonth(serial: f64, months: i64, date1904: bool) -> Option<f64> {
    let (y, m, _) = serial_to_ymd(serial, date1904)?;
    let (ny, nm) = add_months(y, m, months);
    let d = days_in_month_excel(ny, nm, date1904);
    if !(1900..10_000).contains(&ny) && !date1904 {
        return None;
    }
    let s = ymd_to_serial(ny, nm, d, date1904)? as f64;
    (s <= max_serial(date1904)).then_some(s)
}

/// Day of week of a serial, 0 = Sunday … 6 = Saturday, as Excel computes it
/// (in the 1900 system serial 1 is a Sunday, consistent with the fake leap
/// day; from 1900-03-01 on this is the true weekday).
pub fn weekday0(serial: f64, date1904: bool) -> u32 {
    let s = serial.floor() as i64;
    let offset = if date1904 { 5 } else { 6 };
    (s + offset).rem_euclid(7) as u32
}

/// The time of day of a serial as `(hours, minutes, seconds)`: rounded to
/// the millisecond (Excel's time resolution), then truncated, as Excel's
/// `HOUR`/`MINUTE`/`SECOND` do (so 23:59:59.9 is still 23:59:59, and only a
/// time within half a millisecond of midnight becomes 0:00:00).
pub fn hms(serial: f64) -> (u32, u32, u32) {
    let frac = serial - serial.floor();
    let ms = (frac * 86_400_000.0).round() as u64;
    let secs = (ms / 1000) % 86_400;
    (
        (secs / 3600) as u32,
        ((secs / 60) % 60) as u32,
        (secs % 60) as u32,
    )
}

// ---- parsing ---------------------------------------------------------------

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

/// Full English month names, January first.
pub fn month_name(m: u32) -> &'static str {
    const NAMES: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    NAMES[(m as usize).clamp(1, 12) - 1]
}

/// Full English weekday names, Sunday first.
pub fn weekday_name(wd0: u32) -> &'static str {
    const NAMES: [&str; 7] = [
        "Sunday",
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
    ];
    NAMES[wd0 as usize % 7]
}

/// A month from its English name or 3+ letter abbreviation (`Mar`, `Sept`).
fn parse_month(s: &str) -> Option<u32> {
    let l = s.to_ascii_lowercase();
    if l.len() < 3 {
        return None;
    }
    MONTHS
        .iter()
        .position(|name| name.starts_with(&l))
        .map(|i| i as u32 + 1)
}

/// What a typed date/time string holds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DateTime {
    /// The date's serial, if a date was given.
    pub date: Option<f64>,
    /// The time of day as a fraction of a day, if a time was given.
    pub time: Option<f64>,
}

impl DateTime {
    /// The full serial (date + time; a time alone is its fraction).
    pub fn serial(&self) -> f64 {
        self.date.unwrap_or(0.0) + self.time.unwrap_or(0.0)
    }
}

/// Parse a date and/or time the way Excel reads typed input (en-US):
/// `2024-03-01`, `2024/3/1`, `3/1/2024` and `3/1/24` (month first), `3/1`
/// (in `current_year`), `1-Mar-2024`, `1-Mar`, `Mar-2024`, `1 Mar 2024`,
/// `March 1, 2024`, `Mar 1 2024`, optionally followed (or, `T`-separated,
/// for ISO) by a time `14:30`, `14:30:15`, `14:30:15.25`, `2:30 PM`, `2 pm`.
/// Two-digit years 00–29 are 2000–2029 and 30–99 are 1930–1999, as in Excel.
/// A date without a year needs `current_year`; `None` there rejects it.
pub fn parse_datetime(text: &str, current_year: Option<i64>, date1904: bool) -> Option<DateTime> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    // Split off a trailing time: the last whitespace-separated words that
    // contain ':' or are AM/PM markers (or an ISO 'T').
    let (date_part, time_part) = split_time(t);
    let time = match time_part {
        Some(tp) => Some(parse_time(tp)?),
        None => None,
    };
    let date = match date_part {
        Some(dp) if !dp.is_empty() => Some(parse_date(dp, current_year, date1904)?),
        _ => None,
    };
    if date.is_none() && time.is_none() {
        return None;
    }
    Some(DateTime { date, time })
}

fn split_time(t: &str) -> (Option<&str>, Option<&str>) {
    // ISO "2024-03-01T14:30".
    if let Some(i) = t.find(['T', 't'])
        && i > 0
        && t[..i].bytes().all(|b| b.is_ascii_digit() || b == b'-')
        && t[i + 1..].contains(':')
    {
        return (Some(&t[..i]), Some(&t[i + 1..]));
    }
    let words: Vec<(usize, &str)> = t
        .split_whitespace()
        .map(|w| (w.as_ptr() as usize - t.as_ptr() as usize, w))
        .collect();
    let is_ampm = |w: &str| {
        matches!(
            w.to_ascii_lowercase().as_str(),
            "am" | "pm" | "a" | "p" | "a.m." | "p.m."
        )
    };
    // Find the first word that starts a time.
    for (k, (pos, w)) in words.iter().enumerate() {
        let lw = w.to_ascii_lowercase();
        let starts_time = w.contains(':')
            || ((lw.ends_with("am") || lw.ends_with("pm"))
                && lw[..lw.len() - 2].bytes().all(|b| b.is_ascii_digit())
                && lw.len() > 2)
            || (w.bytes().all(|b| b.is_ascii_digit())
                && words.get(k + 1).is_some_and(|(_, n)| is_ampm(n))
                && k + 2 == words.len());
        if starts_time {
            let date = t[..*pos].trim();
            return ((!date.is_empty()).then_some(date), Some(&t[*pos..]));
        }
    }
    (Some(t), None)
}

/// Parse a time of day: `h[:mm[:ss[.fff]]] [AM|PM]`, `h AM`. Hours past 23
/// without AM/PM are allowed as Excel allows them in `TIMEVALUE` (`25:00`
/// is 1:00); the result is the fraction of a day (mod 1).
pub fn parse_time(t: &str) -> Option<f64> {
    let mut s = t.trim().to_ascii_lowercase();
    let mut ampm = None;
    for (suffix, pm) in [
        ("a.m.", false),
        ("p.m.", true),
        ("am", false),
        ("pm", true),
        ("a", false),
        ("p", true),
    ] {
        if let Some(rest) = s.strip_suffix(suffix) {
            ampm = Some(pm);
            s = rest.trim_end().to_string();
            break;
        }
    }
    let parts: Vec<&str> = s.split(':').collect();
    if parts.is_empty() || parts.len() > 3 || (parts.len() == 1 && ampm.is_none()) {
        return None;
    }
    let int = |p: &str| -> Option<u64> {
        (!p.is_empty() && p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit()))
            .then(|| p.parse().ok())
            .flatten()
    };
    let mut h = int(parts[0])? as f64;
    let m = match parts.get(1) {
        Some(p) => int(p)? as f64,
        None => 0.0,
    };
    let sec = match parts.get(2) {
        Some(p) => {
            let (whole, frac) = p.split_once('.').unwrap_or((p, ""));
            let w = int(whole)? as f64;
            let f = if frac.is_empty() {
                0.0
            } else if frac.bytes().all(|b| b.is_ascii_digit()) {
                format!("0.{frac}").parse::<f64>().ok()?
            } else {
                return None;
            };
            if w >= 60.0 {
                return None;
            }
            w + f
        }
        None => 0.0,
    };
    if parts.len() > 1 && m >= 60.0 && parts.len() > 2 {
        return None;
    }
    if let Some(pm) = ampm {
        if !(0.0..=12.0).contains(&h) {
            return None;
        }
        if h == 12.0 {
            h = 0.0;
        }
        if pm {
            h += 12.0;
        }
    }
    let total = h * 3600.0 + m * 60.0 + sec;
    let frac = total / 86_400.0;
    Some(frac - frac.floor())
}

fn two_digit_year(y: i64, len: usize) -> i64 {
    if len <= 2 {
        if y < 30 { 2000 + y } else { 1900 + y }
    } else {
        y
    }
}

fn valid_ymd(y: i64, m: u32, d: u32, date1904: bool) -> Option<f64> {
    if !(1..=12).contains(&m) || d < 1 || d > days_in_month_excel(y, m, date1904) {
        return None;
    }
    if !(1900..10_000).contains(&y) {
        return None;
    }
    ymd_to_serial(y, m, d, date1904).map(|s| s as f64)
}

fn parse_date(t: &str, current_year: Option<i64>, date1904: bool) -> Option<f64> {
    let num = |s: &str| -> Option<i64> {
        (!s.is_empty() && s.len() <= 4 && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse().ok())
            .flatten()
    };
    // Normalise separators: "March 1, 2024" → ["March", "1", "2024"].
    let cleaned: String = t.replace(',', " ");
    let seps: &[char] = &['-', '/', ' ', '.'];
    let parts: Vec<&str> = cleaned
        .split(|c| seps.contains(&c))
        .filter(|p| !p.is_empty())
        .collect();
    let numeric = parts.iter().all(|p| num(p).is_some());
    match parts.as_slice() {
        [a, b, c] if numeric => {
            let (a_n, b_n, c_n) = (num(a)?, num(b)?, num(c)?);
            if a.len() == 4 {
                // ISO y-m-d (or y/m/d).
                valid_ymd(a_n, b_n as u32, c_n as u32, date1904)
            } else {
                // US m/d/y.
                let y = two_digit_year(c_n, c.len());
                valid_ymd(y, a_n as u32, b_n as u32, date1904)
            }
        }
        [a, b] if numeric => {
            let (a_n, b_n) = (num(a)?, num(b)?);
            if a.len() == 4 {
                // "2024-03" / "2024/3": first of the month.
                return valid_ymd(a_n, b_n as u32, 1, date1904);
            }
            // m/d in the current year, else m/yyyy.
            if let Some(cy) = current_year
                && (1..=12).contains(&a_n)
                && b_n >= 1
                && b_n <= days_in_month(cy, a_n as u32) as i64
            {
                return valid_ymd(cy, a_n as u32, b_n as u32, date1904);
            }
            if b.len() == 4 || b_n > 31 {
                return valid_ymd(two_digit_year(b_n, b.len()), a_n as u32, 1, date1904);
            }
            None
        }
        [a, b, c] => {
            // d-Mon-y, Mon-d-y.
            if let (Some(d), Some(m), Some(y)) = (num(a), parse_month(b), num(c)) {
                return valid_ymd(two_digit_year(y, c.len()), m, d as u32, date1904);
            }
            if let (Some(m), Some(d), Some(y)) = (parse_month(a), num(b), num(c)) {
                return valid_ymd(two_digit_year(y, c.len()), m, d as u32, date1904);
            }
            // y-Mon-d.
            if let (Some(y), Some(m), Some(d)) = (num(a), parse_month(b), num(c))
                && a.len() == 4
            {
                return valid_ymd(y, m, d as u32, date1904);
            }
            None
        }
        [a, b] => {
            // d-Mon (current year), Mon-d (current year), Mon-yyyy.
            if let (Some(d), Some(m)) = (num(a), parse_month(b)) {
                if d <= 31
                    && let Some(cy) = current_year
                {
                    return valid_ymd(cy, m, d as u32, date1904);
                }
                return None;
            }
            if let (Some(m), Some(n)) = (parse_month(a), num(b)) {
                if b.len() <= 2
                    && n <= days_in_month(current_year.unwrap_or(2000), m) as i64
                    && let Some(cy) = current_year
                {
                    return valid_ymd(cy, m, n as u32, date1904);
                }
                return valid_ymd(two_digit_year(n, b.len()), m, 1, date1904);
            }
            None
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ymd(y: i64, m: u32, d: u32) -> i64 {
        ymd_to_serial(y, m, d, false).unwrap()
    }

    #[test]
    fn hinnant_round_trips() {
        for z in (-800_000..3_000_000).step_by(997) {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
        }
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1899, 12, 31), DAYS_1899_12_31);
        assert_eq!(days_from_civil(1904, 1, 1), DAYS_1904_01_01);
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }

    #[test]
    fn serials_match_excel_1900() {
        assert_eq!(ymd(1900, 1, 1), 1);
        assert_eq!(ymd(1900, 2, 28), 59);
        assert_eq!(ymd(1900, 2, 29), 60);
        assert_eq!(ymd(1900, 3, 1), 61);
        assert_eq!(ymd(2008, 1, 1), 39448);
        assert_eq!(ymd(2024, 3, 1), 45352);
        assert_eq!(ymd(9999, 12, 31), 2_958_465);
        assert_eq!(serial_to_ymd(59.0, false), Some((1900, 2, 28)));
        assert_eq!(serial_to_ymd(60.0, false), Some((1900, 2, 29)));
        assert_eq!(serial_to_ymd(61.0, false), Some((1900, 3, 1)));
        assert_eq!(serial_to_ymd(0.0, false), Some((1900, 1, 0)));
        assert_eq!(serial_to_ymd(1.5, false), Some((1900, 1, 1)));
        assert_eq!(serial_to_ymd(-1.0, false), None);
        assert_eq!(serial_to_ymd(2_958_466.0, false), None);
        for s in 1..80_000 {
            let (y, m, d) = serial_to_ymd(s as f64, false).unwrap();
            assert_eq!(ymd_to_serial(y, m, d, false), Some(s));
        }
    }

    #[test]
    fn serials_1904() {
        assert_eq!(ymd_to_serial(1904, 1, 1, true), Some(0));
        assert_eq!(ymd_to_serial(2008, 1, 1, true), Some(39448 - 1462));
        assert_eq!(serial_to_ymd(0.0, true), Some((1904, 1, 1)));
        assert_eq!(serial_to_ymd(37986.0, true), Some((2008, 1, 1)));
        assert_eq!(ymd_to_serial(1903, 12, 31, true), None);
        assert_eq!(weekday0(0.0, true), 5); // 1904-01-01 was a Friday
    }

    #[test]
    fn date_normalisation() {
        assert_eq!(date_serial(2008, 1, 2, false), Some(39449.0));
        assert_eq!(date_serial(108, 1, 2, false), Some(39449.0));
        assert_eq!(date_serial(2008, 14, 2, false), Some(39846.0)); // 2009-02-02
        assert_eq!(date_serial(2008, -3, 2, false), Some(39327.0)); // 2007-09-02
        assert_eq!(date_serial(2008, 1, 35, false), Some(39482.0)); // 2008-02-04
        assert_eq!(date_serial(2008, 1, -15, false), Some(39432.0)); // 2007-12-16
        assert_eq!(date_serial(1900, 3, 0, false), Some(60.0));
        assert_eq!(date_serial(1900, 1, 0, false), Some(0.0));
        assert_eq!(date_serial(1900, 1, -1, false), None);
        assert_eq!(date_serial(10000, 1, 1, false), None);
        assert_eq!(date_serial(-1, 1, 1, false), None);
        assert_eq!(
            date_serial(2024, 3, 0, false),
            Some(ymd(2024, 2, 29) as f64)
        );
    }

    #[test]
    fn month_arithmetic() {
        let jan31 = ymd(2011, 1, 31) as f64;
        assert_eq!(edate(jan31, 1, false), Some(ymd(2011, 2, 28) as f64));
        assert_eq!(
            edate(ymd(2011, 1, 15) as f64, -1, false),
            Some(ymd(2010, 12, 15) as f64)
        );
        assert_eq!(
            eomonth(ymd(2011, 1, 1) as f64, 1, false),
            Some(ymd(2011, 2, 28) as f64)
        );
        assert_eq!(
            eomonth(ymd(2011, 1, 1) as f64, -3, false),
            Some(ymd(2010, 10, 31) as f64)
        );
        assert_eq!(
            eomonth(ymd(2024, 1, 10) as f64, 1, false),
            Some(ymd(2024, 2, 29) as f64)
        );
        assert_eq!(add_months(2024, 1, -1), (2023, 12));
        assert_eq!(add_months(2024, 12, 1), (2025, 1));
    }

    #[test]
    fn weekdays_and_times() {
        assert_eq!(weekday0(1.0, false), 0); // Excel: 1900-01-01 is "Sunday"
        assert_eq!(weekday0(61.0, false), 4); // 1900-03-01, a Thursday
        assert_eq!(weekday0(ymd(2024, 3, 1) as f64, false), 5); // Friday
        assert_eq!(hms(0.5), (12, 0, 0));
        assert_eq!(hms(0.75 + 1.0 / 86400.0), (18, 0, 1));
        assert_eq!(hms(0.999_999_99), (23, 59, 59));
        assert_eq!(hms(0.999_999_999_99), (0, 0, 0));
    }

    #[test]
    fn parses_dates() {
        let p = |s: &str| parse_datetime(s, Some(2024), false).map(|d| d.serial());
        let d = ymd(2024, 3, 1) as f64;
        assert_eq!(p("2024-03-01"), Some(d));
        assert_eq!(p("2024/3/1"), Some(d));
        assert_eq!(p("3/1/2024"), Some(d));
        assert_eq!(p("3/1/24"), Some(d));
        assert_eq!(p("3/1"), Some(d));
        assert_eq!(p("1-Mar-2024"), Some(d));
        assert_eq!(p("1-mar"), Some(d));
        assert_eq!(p("1 Mar 2024"), Some(d));
        assert_eq!(p("March 1, 2024"), Some(d));
        assert_eq!(p("Mar 1 2024"), Some(d));
        assert_eq!(p("Mar-2024"), Some(d));
        assert_eq!(p("2024-03-01 12:00"), Some(d + 0.5));
        assert_eq!(p("2024-03-01T18:00:00"), Some(d + 0.75));
        assert_eq!(
            p("3/1/2024 2:30 PM").map(|x| (x * 1440.0).round()),
            Some(((d + 14.5 / 24.0) * 1440.0).round())
        );
        assert_eq!(p("1/1/30"), Some(ymd(1930, 1, 1) as f64));
        assert_eq!(p("2/30/2024"), None);
        assert_eq!(p("13/1/2024"), None);
        assert_eq!(p("hello"), None);
        assert_eq!(p("2/29/1900"), Some(60.0));
        assert_eq!(parse_datetime("3/1", None, false), None);
    }

    #[test]
    fn parses_times() {
        let near = |a: Option<f64>, b: f64| (a.unwrap() - b).abs() < 1e-12;
        assert!(near(parse_time("14:30"), 14.5 / 24.0));
        assert!(near(parse_time("2:30 PM"), 14.5 / 24.0));
        assert!(near(parse_time("12:00 AM"), 0.0));
        assert!(near(parse_time("12:15 pm"), 12.25 / 24.0));
        assert!(near(parse_time("6 pm"), 0.75));
        assert!(near(
            parse_time("6:35:15"),
            (6.0 * 3600.0 + 35.0 * 60.0 + 15.0) / 86400.0
        ));
        assert!(near(parse_time("0:00:01.5"), 1.5 / 86400.0));
        assert_eq!(parse_time("13:00 PM"), None);
        assert_eq!(parse_time("ab:cd"), None);
        assert_eq!(parse_time("14"), None);
        let t = parse_datetime("2:30 PM", None, false).unwrap();
        assert_eq!(t.date, None);
    }
}
