//! Date and time functions, over Excel serial dates (see
//! [`crate::formula::date`]). Date arguments accept serials, booleans, and
//! date/time text (`"2024-03-01"`, `"3/1/2024 2:30 PM"`); the date system
//! (1900 or 1904) comes from [`Ctx::date1904`].

use std::collections::HashSet;

use super::args::{arg_values, current_year, date_arg, def, int, opt_int, to_serial};
use super::{Arg, Ctx, FuncDef, scalar, text};
use crate::formula::date::{self, days_from_civil, days_in_month, is_leap};
use crate::formula::value::{ErrorKind, Value};

pub(super) const FUNCS: &[FuncDef] = &[
    def!("DATE", 3, 3, date_fn),
    def!(volatile "TODAY", 0, 0, today),
    def!(volatile "NOW", 0, 0, now),
    def!("YEAR", 1, 1, year),
    def!("MONTH", 1, 1, month),
    def!("DAY", 1, 1, day),
    def!("HOUR", 1, 1, hour),
    def!("MINUTE", 1, 1, minute),
    def!("SECOND", 1, 1, second),
    def!("TIME", 3, 3, time),
    def!("WEEKDAY", 1, 2, weekday),
    def!("WEEKNUM", 1, 2, weeknum),
    def!("EDATE", 2, 2, edate),
    def!("EOMONTH", 2, 2, eomonth),
    def!("DATEDIF", 3, 3, datedif),
    def!("DAYS", 2, 2, days),
    def!("NETWORKDAYS", 2, 3, networkdays),
    def!("WORKDAY", 2, 3, workday),
    def!("DATEVALUE", 1, 1, datevalue),
    def!("TIMEVALUE", 1, 1, timevalue),
    def!("YEARFRAC", 2, 3, yearfrac),
];

fn date_fn(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    let y = int(&args[0], ctx)?;
    let m = int(&args[1], ctx)?;
    let d = int(&args[2], ctx)?;
    date::date_serial(y, m, d, ctx.date1904()).ok_or(ErrorKind::Num)
}

fn today(_: &[Arg], ctx: &mut dyn Ctx) -> Value {
    Value::Number(ctx.now().floor())
}

fn now(_: &[Arg], ctx: &mut dyn Ctx) -> Value {
    Value::Number(ctx.now())
}

fn ymd(args: &[Arg], ctx: &mut dyn Ctx) -> Result<(i64, u32, u32), ErrorKind> {
    let s = date_arg(&args[0], ctx)?;
    date::serial_to_ymd(s, ctx.date1904()).ok_or(ErrorKind::Num)
}

fn year(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    ymd(args, ctx).map(|(y, _, _)| y as f64)
}

fn month(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    ymd(args, ctx).map(|(_, m, _)| m as f64)
}

fn day(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    ymd(args, ctx).map(|(_, _, d)| d as f64)
}

fn hms(args: &[Arg], ctx: &mut dyn Ctx) -> Result<(u32, u32, u32), ErrorKind> {
    Ok(date::hms(date_arg(&args[0], ctx)?))
}

fn hour(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    hms(args, ctx).map(|t| t.0 as f64)
}

fn minute(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    hms(args, ctx).map(|t| t.1 as f64)
}

fn second(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    hms(args, ctx).map(|t| t.2 as f64)
}

/// TIME(h, m, s): components roll over (`TIME(0,750,0)` is 12:30) and the
/// result wraps at a day; each component past 32767 or a negative total is
/// `#NUM!`.
fn time(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    let mut total = 0i64;
    for (a, mult) in args.iter().zip([3600, 60, 1]) {
        let v = int(a, ctx)?;
        // Bounded both ways (and so the sum can't overflow).
        if !(-32_767..=32_767).contains(&v) {
            return Err(ErrorKind::Num);
        }
        total += v * mult;
    }
    if total < 0 {
        return Err(ErrorKind::Num);
    }
    Ok((total % 86_400) as f64 / 86_400.0)
}

/// 0 = Sunday … 6 = Saturday, from a date argument.
fn weekday0_arg(arg: &Arg, ctx: &mut dyn Ctx) -> Result<(f64, u32), ErrorKind> {
    let s = date_arg(arg, ctx)?;
    Ok((s, date::weekday0(s, ctx.date1904())))
}

/// WEEKDAY return types: 1 Sun=1…Sat=7, 2 Mon=1…Sun=7, 3 Mon=0…Sun=6,
/// 11–17 = 1 on Mon…Sun.
fn weekday(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    let (_, w) = weekday0_arg(&args[0], ctx)?;
    let t = opt_int(args.get(1), ctx, 1)?;
    let w = w as i64;
    let r = match t {
        1 | 17 => w + 1,
        2 | 11 => (w + 6) % 7 + 1,
        3 => (w + 6) % 7,
        12..=16 => {
            let start = t - 10; // 2 = Tuesday … 6 = Saturday
            (w - start).rem_euclid(7) + 1
        }
        _ => return Err(ErrorKind::Num),
    };
    Ok(r as f64)
}

/// WEEKNUM: system 1 (the week holding 1 January is week 1, weeks start on
/// the given day: 1/17 Sun, 2/11 Mon, 12–16 Tue–Sat) or 21 (ISO 8601).
fn weeknum(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    let s = date_arg(&args[0], ctx)?.floor();
    let t = opt_int(args.get(1), ctx, 1)?;
    let d1904 = ctx.date1904();
    let (y, m, d) = date::serial_to_ymd(s, d1904).ok_or(ErrorKind::Num)?;
    // Day 0 (Excel's 1900-01-00) counts as the day before 1 January.
    let doy0 = days_from_civil(y, m, d.max(1)) - days_from_civil(y, 1, 1) - (d == 0) as i64;
    if t == 21 {
        return Ok(iso_week(y, m, d.max(1)) as f64);
    }
    let start: i64 = match t {
        1 | 17 => 0,
        2 | 11 => 1,
        12..=16 => t - 10,
        _ => return Err(ErrorKind::Num),
    };
    // Weekday of 1 January in the real calendar (Excel's fake 1900 leap
    // day only shifts January/February 1900).
    let jan1 = (days_from_civil(y, 1, 1) + 4).rem_euclid(7); // 0 = Sunday
    let offset = (jan1 - start).rem_euclid(7);
    Ok(((doy0 + offset) / 7 + 1) as f64)
}

fn iso_week(y: i64, m: u32, d: u32) -> i64 {
    let days = days_from_civil(y, m, d);
    let wd = (days + 3).rem_euclid(7) + 1; // Monday = 1
    let doy = days - days_from_civil(y, 1, 1) + 1;
    let week = (doy - wd + 10) / 7;
    let weeks_in = |yy: i64| {
        let jan1 = (days_from_civil(yy, 1, 1) + 3).rem_euclid(7) + 1;
        if jan1 == 4 || (jan1 == 3 && is_leap(yy)) {
            53
        } else {
            52
        }
    };
    if week < 1 {
        weeks_in(y - 1)
    } else if week > weeks_in(y) {
        1
    } else {
        week
    }
}

fn edate(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    let s = date_arg(&args[0], ctx)?;
    let m = int(&args[1], ctx)?;
    date::edate(s, m, ctx.date1904()).ok_or(ErrorKind::Num)
}

fn eomonth(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    let s = date_arg(&args[0], ctx)?;
    let m = int(&args[1], ctx)?;
    date::eomonth(s, m, ctx.date1904()).ok_or(ErrorKind::Num)
}

/// DATEDIF(start, end, unit) with Excel's arithmetic, quirks included:
/// `"MD"` borrows the length of the month before `end`, so it can go
/// negative (`DATEDIF(31-Jan, 1-Mar, "MD")` is -2 in a non-leap year), and
/// `"YD"` moves `start` into `end`'s year with DATE's roll-over (29 Feb in a
/// common year becomes 1 March).
fn datedif(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    let s = date_arg(&args[0], ctx)?.floor();
    let e = date_arg(&args[1], ctx)?.floor();
    let unit = text(&args[2], ctx)?.to_ascii_uppercase();
    if s > e {
        return Err(ErrorKind::Num);
    }
    let d1904 = ctx.date1904();
    let (y1, m1, d1) = date::serial_to_ymd(s, d1904).ok_or(ErrorKind::Num)?;
    let (y2, m2, d2) = date::serial_to_ymd(e, d1904).ok_or(ErrorKind::Num)?;
    let mut months = (y2 - y1) * 12 + (m2 as i64 - m1 as i64);
    if d2 < d1 {
        months -= 1;
    }
    Ok(match unit.as_str() {
        "D" => e - s,
        "M" => months as f64,
        "Y" => (months / 12) as f64,
        "YM" => (months % 12) as f64,
        "MD" => {
            if d2 >= d1 {
                (d2 - d1) as f64
            } else {
                let (py, pm) = date::add_months(y2, m2, -1);
                let dim = date::days_in_month_excel(py, pm, d1904) as i64;
                (d2 as i64 + dim - d1 as i64) as f64
            }
        }
        "YD" => {
            let shift =
                |y| date::serial_from_parts(y, m1 as i64, d1 as i64, d1904).ok_or(ErrorKind::Num);
            let mut shifted = shift(y2)?;
            if shifted > e {
                shifted = shift(y2 - 1)?;
            }
            e - shifted
        }
        _ => return Err(ErrorKind::Num),
    })
}

fn days(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    let e = date_arg(&args[0], ctx)?.floor();
    let s = date_arg(&args[1], ctx)?.floor();
    Ok(e - s)
}

/// The holiday serials of an optional holidays argument.
fn holidays(arg: Option<&Arg>, ctx: &mut dyn Ctx) -> Result<HashSet<i64>, ErrorKind> {
    let mut out = HashSet::new();
    let Some(arg) = arg else { return Ok(out) };
    for v in arg_values(arg, &*ctx)? {
        match v {
            Value::Empty => {}
            Value::Error(e) => return Err(e),
            Value::Bool(_) => return Err(ErrorKind::Value),
            v => {
                let s = to_serial(&v, &*ctx)?;
                if s < 0.0 {
                    return Err(ErrorKind::Num);
                }
                out.insert(s.floor() as i64);
            }
        }
    }
    Ok(out)
}

fn is_workday(serial: i64, d1904: bool) -> bool {
    !matches!(date::weekday0(serial as f64, d1904), 0 | 6)
}

/// NETWORKDAYS: Monday–Friday days from start to end inclusive, minus
/// holidays; negative when end is before start.
fn networkdays(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    let s = date_arg(&args[0], ctx)?.floor() as i64;
    let e = date_arg(&args[1], ctx)?.floor() as i64;
    let hol = holidays(args.get(2), ctx)?;
    let d1904 = ctx.date1904();
    let (a, b, sign) = if s <= e { (s, e, 1.0) } else { (e, s, -1.0) };
    let n = b - a + 1;
    let mut count = n / 7 * 5;
    for k in 0..n % 7 {
        if is_workday(a + n / 7 * 7 + k, d1904) {
            count += 1;
        }
    }
    count -= hol
        .iter()
        .filter(|&&h| h >= a && h <= b && is_workday(h, d1904))
        .count() as i64;
    Ok(sign * count as f64)
}

/// WORKDAY: the date `days` working days (Mon–Fri, not holidays) after
/// (or before) start.
fn workday(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    let s = date_arg(&args[0], ctx)?.floor() as i64;
    let n = int(&args[1], ctx)?;
    let hol = holidays(args.get(2), ctx)?;
    let d1904 = ctx.date1904();
    let max = date::max_serial(d1904) as i64;
    let step = n.signum();
    let mut left = n.abs();
    let mut d = s;
    while left > 0 {
        d += step;
        if d < 0 || d > max {
            return Err(ErrorKind::Num);
        }
        if is_workday(d, d1904) && !hol.contains(&d) {
            left -= 1;
        }
    }
    Ok(d as f64)
}

/// DATEVALUE: text only (a number is `#VALUE!`); the time part is dropped;
/// a date without a year is in the current year.
fn datevalue(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    typed_datetime(&args[0], ctx).and_then(|dt| dt.date.ok_or(ErrorKind::Value))
}

/// A text argument read as a typed date and/or time (DATEVALUE, TIMEVALUE).
fn typed_datetime(arg: &Arg, ctx: &mut dyn Ctx) -> Result<date::DateTime, ErrorKind> {
    match scalar(arg, ctx) {
        Value::Text(s) => date::parse_datetime(&s, Some(current_year(&*ctx)), ctx.date1904())
            .ok_or(ErrorKind::Value),
        Value::Error(e) => Err(e),
        _ => Err(ErrorKind::Value),
    }
}

/// TIMEVALUE: text only; the date part is dropped (a date alone is 0).
fn timevalue(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    typed_datetime(&args[0], ctx).map(|dt| dt.time.unwrap_or(0.0))
}

/// YEARFRAC(start, end, [basis]) with Excel's day-count conventions:
/// 0 US (NASD) 30/360, 1 actual/actual, 2 actual/360, 3 actual/365,
/// 4 European 30/360. Bases 0 and 1 follow Excel's exact (and
/// idiosyncratic) rules as reverse-engineered by David A. Wheeler.
fn yearfrac(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    let mut s = date_arg(&args[0], ctx)?.floor();
    let mut e = date_arg(&args[1], ctx)?.floor();
    let basis = opt_int(args.get(2), ctx, 0)?;
    if !(0..=4).contains(&basis) {
        return Err(ErrorKind::Num);
    }
    if s > e {
        std::mem::swap(&mut s, &mut e);
    }
    let d1904 = ctx.date1904();
    let (y1, m1, d1) = date::serial_to_ymd(s, d1904).ok_or(ErrorKind::Num)?;
    let (y2, m2, d2) = date::serial_to_ymd(e, d1904).ok_or(ErrorKind::Num)?;
    let diff = e - s;
    Ok(match basis {
        0 => {
            let last_feb = |y: i64, m: u32, d: u32| m == 2 && d == days_in_month(y, 2);
            let (mut dd1, mut dd2) = (d1 as i64, d2 as i64);
            if dd1 == 31 && dd2 == 31 {
                dd1 = 30;
                dd2 = 30;
            } else if dd1 == 31 {
                dd1 = 30;
            } else if dd1 == 30 && dd2 == 31 {
                dd2 = 30;
            } else if last_feb(y1, m1, d1) && last_feb(y2, m2, d2) {
                dd1 = 30;
                dd2 = 30;
            } else if last_feb(y1, m1, d1) {
                dd1 = 30;
            }
            days360(y1, m1, dd1, y2, m2, dd2) / 360.0
        }
        1 => {
            let le_year = y1 == y2 || (y2 == y1 + 1 && (m1 > m2 || (m1 == m2 && d1 >= d2)));
            if le_year {
                let feb29_between =
                    (is_leap(y1) && (m1, d1) < (3, 1)) || (is_leap(y2) && (m2, d2) >= (3, 1));
                let year_len =
                    if (y1 == y2 && is_leap(y1)) || feb29_between || (m2 == 2 && d2 == 29) {
                        366.0
                    } else {
                        365.0
                    };
                diff / year_len
            } else {
                let years = (y2 - y1 + 1) as f64;
                let span = (days_from_civil(y2 + 1, 1, 1) - days_from_civil(y1, 1, 1)) as f64;
                diff / (span / years)
            }
        }
        2 => diff / 360.0,
        3 => diff / 365.0,
        _ => {
            let dd1 = (d1 as i64).min(30);
            let dd2 = (d2 as i64).min(30);
            days360(y1, m1, dd1, y2, m2, dd2) / 360.0
        }
    })
}

fn days360(y1: i64, m1: u32, d1: i64, y2: i64, m2: u32, d2: i64) -> f64 {
    ((y2 - y1) * 360 + (m2 as i64 - m1 as i64) * 30 + (d2 - d1)) as f64
}

#[cfg(test)]
mod tests {
    use super::super::testing::{Mock, check};

    #[test]
    fn construction_and_parts() {
        let mut m = Mock::grid(&[&["2008-07-15 13:45:30"], &["x"], &["-1"]]);
        check(
            &mut m,
            &[
                ("DATE(2008, 1, 2)", "39449"),
                ("DATE(108, 1, 2)", "39449"),
                ("DATE(2008, 14, 2)", "39846"),
                ("DATE(2008, -3, 2)", "39327"),
                ("DATE(2008, 1, 35)", "39482"),
                ("DATE(2008, 1, -15)", "39432"),
                ("DATE(1900, 2, 29)", "60"),
                ("DATE(1900, 3, 1)", "61"),
                ("DATE(10000, 1, 1)", "#NUM!"),
                ("DATE(-1, 1, 1)", "#NUM!"),
                ("DATE(\"x\", 1, 1)", "#VALUE!"),
                ("DATE(2024.9, 2.9, 29.9)", "45351"),
                ("TODAY()", "45356"),
                ("NOW()", "45356.6041666667"),
                ("YEAR(A1)", "2008"),
                ("YEAR(\"2024-03-01\")", "2024"),
                ("YEAR(0)", "1900"),
                ("YEAR(A3)", "#NUM!"),
                ("YEAR(A2)", "#VALUE!"),
                ("YEAR(3000000)", "#NUM!"),
                ("MONTH(A1)", "7"),
                ("MONTH(\"15-Apr-2011\")", "4"),
                ("MONTH(60)", "2"),
                ("MONTH(A2)", "#VALUE!"),
                ("MONTH(-5)", "#NUM!"),
                ("DAY(A1)", "15"),
                ("DAY(\"15-Apr-2011\")", "15"),
                ("DAY(60)", "29"),
                ("DAY(0)", "0"),
                ("DAY(#N/A)", "#N/A"),
                ("HOUR(0.75)", "18"),
                ("HOUR(A1)", "13"),
                ("HOUR(\"3:30:30 PM\")", "15"),
                ("HOUR(29747.7)", "16"),
                ("HOUR(-1)", "#NUM!"),
                ("HOUR(\"x\")", "#VALUE!"),
                ("MINUTE(\"12:45:00 PM\")", "45"),
                ("MINUTE(A1)", "45"),
                ("MINUTE(0.5)", "0"),
                ("MINUTE(0.999999)", "59"),
                ("MINUTE(0.99999999999)", "0"),
                ("MINUTE(A2)", "#VALUE!"),
                ("SECOND(\"4:48:18 PM\")", "18"),
                ("SECOND(\"4:48 PM\")", "0"),
                ("SECOND(A1)", "30"),
                ("SECOND(0.0000115740740740741)", "1"),
                ("SECOND(0.999999)", "59"),
                ("SECOND(-0.5)", "#NUM!"),
                ("TIME(12, 0, 0)", "0.5"),
                ("TIME(16, 48, 10)", "0.700115740740741"),
                ("TIME(27, 0, 0)", "0.125"),
                ("TIME(0, 750, 0)", "0.520833333333333"),
                ("TIME(0, -1, 0)", "#NUM!"),
                ("TIME(32768, 0, 0)", "#NUM!"),
                ("TIME(\"x\", 0, 0)", "#VALUE!"),
            ],
        );
    }

    #[test]
    fn weekdays_and_weeks() {
        // 2008-02-14 (serial 39492) was a Thursday.
        check(
            &mut Mock::new(),
            &[
                ("WEEKDAY(39492)", "5"),
                ("WEEKDAY(39492, 2)", "4"),
                ("WEEKDAY(39492, 3)", "3"),
                ("WEEKDAY(39492, 11)", "4"),
                ("WEEKDAY(39492, 12)", "3"),
                ("WEEKDAY(39492, 14)", "1"),
                ("WEEKDAY(39492, 16)", "6"),
                ("WEEKDAY(39492, 17)", "5"),
                ("WEEKDAY(1)", "1"),
                ("WEEKDAY(39492, 4)", "#NUM!"),
                ("WEEKDAY(-1)", "#NUM!"),
                ("WEEKDAY(\"2024-03-05\")", "3"),
                ("WEEKNUM(40977)", "10"),
                ("WEEKNUM(40977, 2)", "11"),
                ("WEEKNUM(DATE(2024, 1, 1))", "1"),
                ("WEEKNUM(DATE(2024, 1, 7))", "2"),
                ("WEEKNUM(DATE(2024, 1, 7), 2)", "1"),
                ("WEEKNUM(DATE(2024, 12, 31))", "53"),
                ("WEEKNUM(DATE(2021, 1, 1), 21)", "53"),
                ("WEEKNUM(DATE(2024, 12, 30), 21)", "1"),
                ("WEEKNUM(DATE(2024, 3, 5), 21)", "10"),
                ("WEEKNUM(DATE(2026, 1, 1), 21)", "1"),
                ("WEEKNUM(40977, 3)", "#NUM!"),
                ("WEEKNUM(\"x\")", "#VALUE!"),
            ],
        );
    }

    #[test]
    fn month_arithmetic() {
        check(
            &mut Mock::new(),
            &[
                ("EDATE(DATE(2011, 1, 15), 1)", "40589"),
                ("EDATE(DATE(2011, 1, 15), -1)", "40527"),
                ("EDATE(DATE(2011, 1, 15), 2)", "40617"),
                ("EDATE(DATE(2011, 1, 31), 1)", "40602"),
                ("EDATE(DATE(2024, 1, 31), 1)", "45351"),
                ("EDATE(\"2011-01-15\", 1.9)", "40589"),
                ("EDATE(-1, 1)", "#NUM!"),
                ("EDATE(DATE(1900, 1, 15), -1)", "#NUM!"),
                ("EOMONTH(DATE(2011, 1, 1), 1)", "40602"),
                ("EOMONTH(DATE(2011, 1, 1), -3)", "40482"),
                ("EOMONTH(DATE(2024, 1, 15), 1)", "45351"),
                ("EOMONTH(DATE(2024, 1, 15), 0)", "45322"),
                ("EOMONTH(\"x\", 1)", "#VALUE!"),
                ("EOMONTH(DATE(9999, 12, 1), 1)", "#NUM!"),
            ],
        );
    }

    #[test]
    fn differences() {
        check(
            &mut Mock::new(),
            &[
                ("DATEDIF(DATE(2001, 1, 1), DATE(2003, 1, 1), \"Y\")", "2"),
                ("DATEDIF(DATE(2001, 6, 1), DATE(2002, 8, 15), \"D\")", "440"),
                ("DATEDIF(DATE(2001, 6, 1), DATE(2002, 8, 15), \"YD\")", "75"),
                ("DATEDIF(DATE(2001, 6, 1), DATE(2002, 8, 15), \"M\")", "14"),
                ("DATEDIF(DATE(2001, 6, 1), DATE(2002, 8, 15), \"YM\")", "2"),
                ("DATEDIF(DATE(2001, 6, 1), DATE(2002, 8, 15), \"MD\")", "14"),
                ("DATEDIF(DATE(2015, 1, 31), DATE(2015, 3, 1), \"MD\")", "-2"),
                ("DATEDIF(DATE(2016, 1, 31), DATE(2016, 3, 1), \"MD\")", "-1"),
                ("DATEDIF(DATE(2015, 1, 31), DATE(2015, 3, 1), \"M\")", "1"),
                ("DATEDIF(DATE(2020, 2, 29), DATE(2021, 2, 28), \"Y\")", "0"),
                ("DATEDIF(DATE(2020, 2, 29), DATE(2021, 3, 1), \"YD\")", "0"),
                (
                    "DATEDIF(DATE(2020, 5, 10), DATE(2021, 5, 9), \"YD\")",
                    "364",
                ),
                (
                    "DATEDIF(DATE(2003, 1, 1), DATE(2001, 1, 1), \"Y\")",
                    "#NUM!",
                ),
                (
                    "DATEDIF(DATE(2001, 1, 1), DATE(2003, 1, 1), \"W\")",
                    "#NUM!",
                ),
                ("DATEDIF(\"2001-01-01\", \"2001-03-01\", \"md\")", "0"),
                ("DAYS(\"2021-03-15\", \"2021-02-01\")", "42"),
                ("DAYS(DATE(2021, 12, 31), DATE(2021, 1, 1))", "364"),
                ("DAYS(1, 10)", "-9"),
                ("DAYS(10.9, 1.1)", "9"),
                ("DAYS(\"x\", 1)", "#VALUE!"),
                ("DAYS(-1, 1)", "#NUM!"),
            ],
        );
    }

    #[test]
    fn working_days() {
        // Excel's sample: 2012-10-01 … 2013-03-01 with three holidays.
        let mut m = Mock::grid(&[&["2012-11-22"], &["2012-12-04"], &["2013-01-21"], &["x"]]);
        check(
            &mut m,
            &[
                ("NETWORKDAYS(DATE(2012, 10, 1), DATE(2013, 3, 1))", "110"),
                (
                    "NETWORKDAYS(DATE(2012, 10, 1), DATE(2013, 3, 1), A1)",
                    "109",
                ),
                (
                    "NETWORKDAYS(DATE(2012, 10, 1), DATE(2013, 3, 1), A1:A3)",
                    "107",
                ),
                (
                    "NETWORKDAYS(DATE(2013, 3, 1), DATE(2012, 10, 1), A1:A3)",
                    "-107",
                ),
                ("NETWORKDAYS(DATE(2024, 3, 2), DATE(2024, 3, 3))", "0"),
                ("NETWORKDAYS(DATE(2024, 3, 4), DATE(2024, 3, 4))", "1"),
                (
                    "NETWORKDAYS(DATE(2024, 3, 4), DATE(2024, 3, 4), {45355, 45355})",
                    "0",
                ),
                ("NETWORKDAYS(\"x\", 1)", "#VALUE!"),
                ("NETWORKDAYS(1, 10, A4)", "#VALUE!"),
                ("WORKDAY(DATE(2008, 10, 1), 151)", "39933"),
                (
                    "WORKDAY(DATE(2008, 10, 1), 151, {39778, 39786, 39834})",
                    "39938",
                ),
                ("WORKDAY(DATE(2024, 3, 8), 1)", "45362"),
                ("WORKDAY(DATE(2024, 3, 11), -1)", "45359"),
                ("WORKDAY(DATE(2024, 3, 9), 0)", "45360"),
                ("WORKDAY(\"x\", 1)", "#VALUE!"),
                ("WORKDAY(2958465, 5)", "#NUM!"),
            ],
        );
    }

    #[test]
    fn parsing_and_yearfrac() {
        check(
            &mut Mock::new(),
            &[
                ("DATEVALUE(\"8/22/2011\")", "40777"),
                ("DATEVALUE(\"22-MAY-2011\")", "40685"),
                ("DATEVALUE(\"2011/02/23\")", "40597"),
                ("DATEVALUE(\"5-JUL\")", "45478"),
                ("DATEVALUE(\"2024-03-01 18:00\")", "45352"),
                ("DATEVALUE(\"March 1, 2024\")", "45352"),
                ("DATEVALUE(\"2/30/2024\")", "#VALUE!"),
                ("DATEVALUE(45352)", "#VALUE!"),
                ("DATEVALUE(\"12:00\")", "#VALUE!"),
                ("DATEVALUE(#N/A)", "#N/A"),
                ("TIMEVALUE(\"2:24 AM\")", "0.1"),
                ("TIMEVALUE(\"22-Aug-2011 6:35 AM\")", "0.274305555555556"),
                ("TIMEVALUE(\"18:00\")", "0.75"),
                ("TIMEVALUE(\"2024-03-01\")", "0"),
                ("TIMEVALUE(\"25:00\")", "0.0416666666666667"),
                ("TIMEVALUE(\"x\")", "#VALUE!"),
                ("TIMEVALUE(0.5)", "#VALUE!"),
                (
                    "YEARFRAC(DATE(2012, 1, 1), DATE(2012, 7, 30))",
                    "0.580555555555556",
                ),
                (
                    "YEARFRAC(DATE(2012, 1, 1), DATE(2012, 7, 30), 1)",
                    "0.576502732240437",
                ),
                (
                    "YEARFRAC(DATE(2012, 1, 1), DATE(2012, 7, 30), 3)",
                    "0.578082191780822",
                ),
                (
                    "YEARFRAC(DATE(2012, 1, 1), DATE(2012, 7, 30), 2)",
                    "0.586111111111111",
                ),
                (
                    "YEARFRAC(DATE(2012, 1, 1), DATE(2012, 7, 30), 4)",
                    "0.580555555555556",
                ),
                (
                    "YEARFRAC(DATE(2012, 7, 30), DATE(2012, 1, 1))",
                    "0.580555555555556",
                ),
                ("YEARFRAC(DATE(2011, 2, 28), DATE(2012, 2, 29), 0)", "1"),
                (
                    "YEARFRAC(DATE(2011, 1, 31), DATE(2011, 3, 31), 0)",
                    "0.166666666666667",
                ),
                (
                    "YEARFRAC(DATE(2011, 1, 31), DATE(2011, 3, 31), 4)",
                    "0.166666666666667",
                ),
                (
                    "YEARFRAC(DATE(2010, 6, 1), DATE(2013, 6, 1), 1)",
                    "3.00068446269678",
                ),
                ("YEARFRAC(DATE(2011, 3, 1), DATE(2012, 3, 1), 1)", "1"),
                ("YEARFRAC(DATE(2012, 1, 1), DATE(2012, 7, 30), 5)", "#NUM!"),
                ("YEARFRAC(\"x\", 1)", "#VALUE!"),
            ],
        );
    }

    #[test]
    fn volatile_now() {
        let mut m = Mock::new();
        m.now = 45291.999;
        check(&mut m, &[("TODAY()", "45291"), ("NOW()", "45291.999")]);
        m.now = 0.25;
        check(
            &mut m,
            &[("TODAY()", "0"), ("NOW()", "0.25"), ("HOUR(NOW())", "6")],
        );
        m.now = 60.5;
        check(&mut m, &[("TODAY()", "60"), ("DAY(TODAY())", "29")]);
        assert!(super::super::lookup("NOW").unwrap().volatile);
        assert!(super::super::lookup("today").unwrap().volatile);
        assert!(!super::super::lookup("DATE").unwrap().volatile);
    }

    #[test]
    fn date1904_system() {
        let mut m = Mock::new();
        m.d1904 = true;
        check(
            &mut m,
            &[
                ("DATE(1904, 1, 1)", "0"),
                ("DATE(2008, 1, 1)", "37986"),
                ("DATE(1903, 12, 31)", "#NUM!"),
                ("YEAR(0)", "1904"),
                ("DAY(37986)", "1"),
                ("WEEKDAY(0)", "6"),
                ("DATEVALUE(\"2008-01-01\")", "37986"),
                ("EOMONTH(0, 1)", "59"),
            ],
        );
    }
}
