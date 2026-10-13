//! Statistical functions and the conditional aggregates (`SUMIF(S)`,
//! `COUNTIF(S)`, `AVERAGEIF(S)`, `MAXIFS`, `MINIFS`).

use std::collections::HashMap;

use super::args::{Area, MANY, area, def, opt_num};
use super::{Arg, Ctx, FuncDef, num, numbers, scalar};
use crate::formula::criteria::Criterion;
use crate::formula::value::{ErrorKind, Value};

pub(super) const FUNCS: &[FuncDef] = &[
    def!("AVERAGE", 1, MANY, average),
    def!("AVERAGEA", 1, MANY, averagea),
    def!("COUNT", 1, MANY, count),
    def!("COUNTA", 1, MANY, counta),
    def!("COUNTBLANK", 1, 1, countblank),
    def!("MIN", 1, MANY, min),
    def!("MAX", 1, MANY, max),
    def!("MINA", 1, MANY, mina),
    def!("MAXA", 1, MANY, maxa),
    def!("MEDIAN", 1, MANY, median),
    def!("MODE", 1, MANY, mode),
    def!("MODE.SNGL", 1, MANY, mode),
    def!("STDEV", 1, MANY, stdev_s),
    def!("STDEV.S", 1, MANY, stdev_s),
    def!("STDEV.P", 1, MANY, stdev_p),
    def!("STDEVP", 1, MANY, stdev_p),
    def!("VAR", 1, MANY, var_s),
    def!("VAR.S", 1, MANY, var_s),
    def!("VAR.P", 1, MANY, var_p),
    def!("VARP", 1, MANY, var_p),
    def!("LARGE", 2, 2, large),
    def!("SMALL", 2, 2, small),
    def!("RANK", 2, 3, rank),
    def!("RANK.EQ", 2, 3, rank),
    def!("RANK.AVG", 2, 3, rank_avg),
    def!("PERCENTILE", 2, 2, percentile),
    def!("PERCENTILE.INC", 2, 2, percentile),
    def!("QUARTILE", 2, 2, quartile),
    def!("QUARTILE.INC", 2, 2, quartile),
    def!("SUMIF", 2, 3, sumif),
    def!("SUMIFS", 3, MANY, sumifs),
    def!("COUNTIF", 2, 2, countif),
    def!("COUNTIFS", 2, MANY, countifs),
    def!("AVERAGEIF", 2, 3, averageif),
    def!("AVERAGEIFS", 3, MANY, averageifs),
    def!("MAXIFS", 3, MANY, maxifs),
    def!("MINIFS", 3, MANY, minifs),
];

/// The `…A` functions' view: like [`numbers`], but inside ranges and arrays
/// text counts as 0 and booleans as 1/0 (empty cells are still skipped).
fn numbers_a(args: &[Arg], ctx: &mut dyn Ctx) -> Result<Vec<f64>, ErrorKind> {
    let mut out = Vec::new();
    for arg in args {
        match arg {
            Arg::Omitted => {}
            Arg::Value(Value::Empty) => {}
            Arg::Value(v) => out.push(v.to_number()?),
            _ => {
                let ar = area(arg, ctx)?;
                let (r, c) = ar.data_dims(ctx);
                for v in ar.window(ctx, r, c) {
                    match v {
                        Value::Number(n) => out.push(n),
                        Value::Bool(b) => out.push(if b { 1.0 } else { 0.0 }),
                        Value::Text(_) => out.push(0.0),
                        Value::Error(e) => return Err(e),
                        Value::Empty => {}
                    }
                }
            }
        }
    }
    Ok(out)
}

fn mean(ns: &[f64]) -> Result<f64, ErrorKind> {
    if ns.is_empty() {
        return Err(ErrorKind::Div0);
    }
    Ok(ns.iter().sum::<f64>() / ns.len() as f64)
}

fn average(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    numbers(args, ctx).and_then(|ns| mean(&ns))
}

fn averagea(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    numbers_a(args, ctx).and_then(|ns| mean(&ns))
}

/// COUNT never fails: errors and non-numeric text are simply not counted.
/// Values typed directly count when they are numbers, booleans or numeric
/// text; inside ranges and arrays only numbers count.
fn count(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    let mut n = 0usize;
    for arg in args {
        match arg {
            Arg::Omitted => {}
            Arg::Value(v) => {
                if !matches!(v, Value::Empty | Value::Error(_)) && v.to_number().is_ok() {
                    n += 1;
                }
            }
            _ => {
                let Ok(ar) = area(arg, ctx) else { continue };
                let (r, c) = ar.data_dims(ctx);
                n += ar
                    .window(ctx, r, c)
                    .iter()
                    .filter(|v| matches!(v, Value::Number(_)))
                    .count();
            }
        }
    }
    Value::Number(n as f64)
}

/// Every non-empty value counts, errors and empty text included.
fn counta(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    let mut n = 0usize;
    for arg in args {
        match arg {
            Arg::Omitted | Arg::Value(Value::Empty) => {}
            Arg::Value(_) => n += 1,
            _ => {
                let ar = match area(arg, ctx) {
                    Ok(a) => a,
                    Err(e) => return Value::Error(e),
                };
                let (r, c) = ar.data_dims(ctx);
                n += ar
                    .window(ctx, r, c)
                    .iter()
                    .filter(|v| **v != Value::Empty)
                    .count();
            }
        }
    }
    Value::Number(n as f64)
}

/// Empty cells and empty text in the range, counting the cells outside the
/// used area without visiting them. (An open-ended `A:A` is closed at the
/// used area, so it counts blanks down to the last used row, not Excel's
/// 1,048,576.)
fn countblank(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    let ar = match area(&args[0], ctx) {
        Ok(a) => a,
        Err(e) => return Value::Error(e),
    };
    let total = ar.len();
    let (r, c) = ar.data_dims(ctx);
    let filled = ar
        .window(ctx, r, c)
        .iter()
        .filter(|v| match v {
            Value::Empty => false,
            Value::Text(s) => !s.is_empty(),
            _ => true,
        })
        .count();
    Value::Number((total - filled) as f64)
}

fn fold_or_zero(ns: Vec<f64>, f: fn(f64, f64) -> f64) -> f64 {
    ns.into_iter().reduce(f).unwrap_or(0.0)
}

fn min(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    numbers(args, ctx).map(|ns| fold_or_zero(ns, f64::min))
}

fn max(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    numbers(args, ctx).map(|ns| fold_or_zero(ns, f64::max))
}

fn mina(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    numbers_a(args, ctx).map(|ns| fold_or_zero(ns, f64::min))
}

fn maxa(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    numbers_a(args, ctx).map(|ns| fold_or_zero(ns, f64::max))
}

fn sorted(mut ns: Vec<f64>) -> Vec<f64> {
    ns.sort_by(|a, b| a.total_cmp(b));
    ns
}

fn median(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    numbers(args, ctx).and_then(|ns| {
        if ns.is_empty() {
            return Err(ErrorKind::Num);
        }
        let s = sorted(ns);
        let m = s.len() / 2;
        Ok(if s.len() % 2 == 1 {
            s[m]
        } else {
            (s[m - 1] + s[m]) / 2.0
        })
    })
}

/// The most frequent number; ties go to the one that appears first. No
/// repeats at all is `#N/A`.
fn mode(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    numbers(args, ctx).and_then(|ns| {
        let mut counts: HashMap<u64, (usize, usize)> = HashMap::new();
        for (i, n) in ns.iter().enumerate() {
            let key = if *n == 0.0 { 0 } else { n.to_bits() };
            counts.entry(key).or_insert((0, i)).0 += 1;
        }
        let best = counts
            .values()
            .filter(|(c, _)| *c > 1)
            .max_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
        best.map(|&(_, i)| ns[i]).ok_or(ErrorKind::NA)
    })
}

fn variance(ns: &[f64], sample: bool) -> Result<f64, ErrorKind> {
    let n = ns.len();
    if n == 0 || (sample && n < 2) {
        return Err(ErrorKind::Div0);
    }
    let m = ns.iter().sum::<f64>() / n as f64;
    let ss: f64 = ns.iter().map(|x| (x - m) * (x - m)).sum();
    Ok(ss / if sample { (n - 1) as f64 } else { n as f64 })
}

fn var_s(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    numbers(args, ctx).and_then(|ns| variance(&ns, true))
}

fn var_p(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    numbers(args, ctx).and_then(|ns| variance(&ns, false))
}

fn stdev_s(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    numbers(args, ctx)
        .and_then(|ns| variance(&ns, true))
        .map(f64::sqrt)
}

fn stdev_p(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    numbers(args, ctx)
        .and_then(|ns| variance(&ns, false))
        .map(f64::sqrt)
}

/// The k-th largest (or smallest). A fractional k rounds up, as in Excel.
fn kth(args: &[Arg], ctx: &mut dyn Ctx, largest: bool) -> Result<f64, ErrorKind> {
    let ns = numbers(&args[..1], ctx)?;
    let k = num(&args[1], ctx)?.ceil();
    if k < 1.0 || k > ns.len() as f64 {
        return Err(ErrorKind::Num);
    }
    let s = sorted(ns);
    let i = k as usize - 1;
    Ok(if largest { s[s.len() - 1 - i] } else { s[i] })
}

fn large(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    kth(args, ctx, true)
}

fn small(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    kth(args, ctx, false)
}

/// `(count better, count equal)` of `x` in `ref` for RANK.
fn rank_counts(args: &[Arg], ctx: &mut dyn Ctx) -> Result<(usize, usize), ErrorKind> {
    let x = num(&args[0], ctx)?;
    let ns = numbers(&args[1..2], ctx)?;
    let ascending = opt_num(args.get(2), ctx, 0.0)? != 0.0;
    let better = ns
        .iter()
        .filter(|&&v| if ascending { v < x } else { v > x })
        .count();
    let equal = ns.iter().filter(|&&v| v == x).count();
    if equal == 0 {
        return Err(ErrorKind::NA);
    }
    Ok((better, equal))
}

fn rank(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    rank_counts(args, ctx).map(|(b, _)| (b + 1) as f64)
}

fn rank_avg(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    rank_counts(args, ctx).map(|(b, e)| b as f64 + (e as f64 + 1.0) / 2.0)
}

fn percentile_of(ns: Vec<f64>, k: f64) -> Result<f64, ErrorKind> {
    if ns.is_empty() || !(0.0..=1.0).contains(&k) {
        return Err(ErrorKind::Num);
    }
    let s = sorted(ns);
    let h = (s.len() - 1) as f64 * k;
    let lo = h.floor() as usize;
    let frac = h - lo as f64;
    Ok(if lo + 1 < s.len() {
        s[lo] + frac * (s[lo + 1] - s[lo])
    } else {
        s[lo]
    })
}

fn percentile(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    let ns = numbers(&args[..1], ctx)?;
    percentile_of(ns, num(&args[1], ctx)?)
}

fn quartile(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    let ns = numbers(&args[..1], ctx)?;
    let q = num(&args[1], ctx)?.trunc();
    if !(0.0..=4.0).contains(&q) {
        return Err(ErrorKind::Num);
    }
    percentile_of(ns, q / 4.0)
}

// ---- conditional aggregates --------------------------------------------------

/// The criterion argument: one value (a multi-cell range is `#VALUE!`).
fn criterion(arg: &Arg, ctx: &mut dyn Ctx) -> Result<Criterion, ErrorKind> {
    if let Arg::Range(r) = arg
        && !r.is_cell()
    {
        return Err(ErrorKind::Value);
    }
    let v = scalar(arg, ctx);
    Ok(Criterion::parse_with(&v, ctx.date1904()))
}

/// Which cells satisfy every `(range, criterion)` pair. All ranges share
/// the first one's shape (else `#VALUE!`). Only the window that holds data
/// in some range (or in `target`) is visited: the returned positions index
/// that `win`-wide window row-major, and `outside` counts the matching
/// cells beyond it (all empty).
struct Matches {
    positions: Vec<usize>,
    outside: usize,
    win: (usize, usize),
}

fn match_cells(
    pairs: &[(Area, Criterion)],
    target: Option<&Area>,
    ctx: &dyn Ctx,
) -> Result<Matches, ErrorKind> {
    let (rows, cols) = (pairs[0].0.rows(), pairs[0].0.cols());
    if pairs
        .iter()
        .any(|(a, _)| a.rows() != rows || a.cols() != cols)
    {
        return Err(ErrorKind::Value);
    }
    let (mut wr, mut wc) = (0, 0);
    for a in pairs.iter().map(|(a, _)| a).chain(target) {
        let (r, c) = a.data_dims(ctx);
        wr = wr.max(r.min(rows));
        wc = wc.max(c.min(cols));
    }
    let mut ok = vec![true; wr * wc];
    for (a, crit) in pairs {
        for (i, v) in a.window(ctx, wr, wc).iter().enumerate() {
            if ok[i] && !crit.matches(v) {
                ok[i] = false;
            }
        }
    }
    let outside = if pairs.iter().all(|(_, c)| c.matches_empty()) {
        rows * cols - wr * wc
    } else {
        0
    };
    Ok(Matches {
        positions: (0..ok.len()).filter(|&i| ok[i]).collect(),
        outside,
        win: (wr, wc),
    })
}

/// The numbers of `target` at the matched cells (non-numbers skipped; an
/// error in a matched cell is the result).
fn matched_numbers(m: &Matches, target: &Area, ctx: &dyn Ctx) -> Result<Vec<f64>, ErrorKind> {
    let vals = target.window(ctx, m.win.0, m.win.1);
    let mut out = Vec::new();
    for &i in &m.positions {
        match &vals[i] {
            Value::Number(n) => out.push(*n),
            Value::Error(e) => return Err(*e),
            _ => {}
        }
    }
    Ok(out)
}

/// `SUMIF`/`AVERAGEIF`: `(range, criterion, [values])`, where `values`
/// takes the shape of `range` from its own top-left cell, as Excel does
/// (`SUMIF(A1:A5, "x", B1)` sums B1:B5).
fn single_if(args: &[Arg], ctx: &mut dyn Ctx) -> Result<Vec<f64>, ErrorKind> {
    let range = area(&args[0], ctx)?;
    let crit = criterion(&args[1], ctx)?;
    let target = match args.get(2) {
        None | Some(Arg::Omitted) => range.clone(),
        Some(a) => area(a, ctx)?.sub(0, 0, range.rows(), range.cols()),
    };
    let pairs = [(range, crit)];
    let m = match_cells(&pairs, Some(&target), ctx)?;
    matched_numbers(&m, &target, ctx)
}

/// `…IFS`: `(values, range1, crit1, range2, crit2, …)`; `values` must have
/// the ranges' shape.
fn multi_if(args: &[Arg], ctx: &mut dyn Ctx) -> Result<Vec<f64>, ErrorKind> {
    if args.len() < 3 || !(args.len() - 1).is_multiple_of(2) {
        return Err(ErrorKind::Value);
    }
    let target = area(&args[0], ctx)?;
    let pairs = criteria_pairs(&args[1..], ctx)?;
    if target.rows() != pairs[0].0.rows() || target.cols() != pairs[0].0.cols() {
        return Err(ErrorKind::Value);
    }
    let m = match_cells(&pairs, Some(&target), ctx)?;
    matched_numbers(&m, &target, ctx)
}

fn criteria_pairs<'a>(
    args: &'a [Arg],
    ctx: &mut dyn Ctx,
) -> Result<Vec<(Area<'a>, Criterion)>, ErrorKind> {
    if args.is_empty() || !args.len().is_multiple_of(2) {
        return Err(ErrorKind::Value);
    }
    let mut pairs = Vec::new();
    for pair in args.chunks(2) {
        let a = area(&pair[0], ctx)?;
        pairs.push((a, criterion(&pair[1], ctx)?));
    }
    Ok(pairs)
}

fn sumif(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    single_if(args, ctx).map(|ns| ns.iter().sum())
}

fn averageif(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    single_if(args, ctx).and_then(|ns| mean(&ns))
}

fn sumifs(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    multi_if(args, ctx).map(|ns| ns.iter().sum())
}

fn averageifs(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    multi_if(args, ctx).and_then(|ns| mean(&ns))
}

fn maxifs(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    multi_if(args, ctx).map(|ns| fold_or_zero(ns, f64::max))
}

fn minifs(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    multi_if(args, ctx).map(|ns| fold_or_zero(ns, f64::min))
}

fn countif(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    countifs(args, ctx)
}

fn countifs(args: &[Arg], ctx: &mut dyn Ctx) -> Result<f64, ErrorKind> {
    let pairs = criteria_pairs(args, ctx)?;
    let m = match_cells(&pairs, None, ctx)?;
    Ok((m.positions.len() + m.outside) as f64)
}

#[cfg(test)]
mod tests {
    use super::super::testing::{Mock, check, check0};

    fn data() -> Mock {
        // A: 10, 7, 9, 27, 2   B: text/bool/empty mix   C: 4,5,5,5,8
        Mock::grid(&[
            &["10", "x", "4"],
            &["7", "TRUE", "5"],
            &["9", "", "5"],
            &["27", "'5", "5"],
            &["2", "#N/A", "8"],
        ])
    }

    #[test]
    fn averages_and_counts() {
        let mut m = data();
        check(
            &mut m,
            &[
                ("AVERAGE(A1:A5)", "11"),
                ("AVERAGE(A1:A5, 5)", "10"),
                ("AVERAGE(C1:C5, B1:B4)", "5.4"),
                ("AVERAGE(B1:B4)", "#DIV/0!"),
                ("AVERAGE(\"x\")", "#VALUE!"),
                ("AVERAGE(B1:B5)", "#N/A"),
                ("AVERAGE(1, TRUE, \"3\")", "1.66666666666667"),
                ("AVERAGEA(A1:A2, B1:B4)", "3.6"),
                ("AVERAGEA(B1:B4)", "0.333333333333333"),
                ("AVERAGEA(10, TRUE)", "5.5"),
                ("AVERAGEA(B3)", "#DIV/0!"),
                ("AVERAGEA(B1:B5)", "#N/A"),
                ("COUNT(A1:C5)", "10"),
                ("COUNT(B1:B5)", "0"),
                ("COUNT(1, \"2\", TRUE, \"x\", #N/A)", "3"),
                ("COUNT({1,\"2\",TRUE})", "1"),
                ("COUNT(A:A)", "5"),
                ("COUNTA(A1:C5)", "14"),
                ("COUNTA(B1:B5)", "4"),
                ("COUNTA(1, \"\", #N/A)", "3"),
                ("COUNTA(D1:D9)", "0"),
                ("COUNTA(A1, B3)", "1"),
                ("COUNTBLANK(B1:B5)", "1"),
                ("COUNTBLANK(A1:D10)", "26"),
                ("COUNTBLANK(D1:D3)", "3"),
                ("COUNTBLANK({1,\"\"})", "1"),
                ("COUNTBLANK(A1:A5)", "0"),
            ],
        );
    }

    #[test]
    fn min_max_median_mode() {
        let mut m = data();
        check(
            &mut m,
            &[
                ("MIN(A1:A5)", "2"),
                ("MIN(A1:A5, 0)", "0"),
                ("MIN(B1:B4)", "0"),
                ("MIN(B1:B5)", "#N/A"),
                ("MIN(\"1\", 5)", "1"),
                ("MAX(A1:A5)", "27"),
                ("MAX(A1:A5, 30)", "30"),
                ("MAX(B1:B4)", "0"),
                ("MAX(-1, -5)", "-1"),
                ("MAX(\"x\")", "#VALUE!"),
                ("MINA(B1:B4)", "0"),
                ("MINA(A1:A5, B1:B4)", "0"),
                ("MAXA(B1:B4)", "1"),
                ("MAXA(-1, FALSE)", "0"),
                ("MAXA(B1:B5)", "#N/A"),
                ("MEDIAN(1, 2, 3, 4, 5)", "3"),
                ("MEDIAN(1, 2, 3, 4, 5, 6)", "3.5"),
                ("MEDIAN(A1:A5)", "9"),
                ("MEDIAN(B1:B4)", "#NUM!"),
                ("MEDIAN(1, #DIV/0!)", "#DIV/0!"),
                ("MODE(5.6, 4, 4, 3, 2, 4)", "4"),
                ("MODE(C1:C5)", "5"),
                ("MODE(1, 2, 3)", "#N/A"),
                ("MODE(1, 2, 2, 1)", "1"),
                ("MODE.SNGL(3, 3, 1)", "3"),
                ("MODE(B1:B4)", "#N/A"),
            ],
        );
    }

    #[test]
    fn spread() {
        // Excel's documentation sample (tensile strengths).
        let mut m = Mock::grid(&[
            &["1345"],
            &["1301"],
            &["1368"],
            &["1322"],
            &["1310"],
            &["1370"],
            &["1318"],
            &["1350"],
            &["1303"],
            &["1299"],
        ]);
        check(
            &mut m,
            &[
                ("STDEV(A1:A10)", "27.4639157198435"),
                ("STDEV.S(A1:A10)", "27.4639157198435"),
                ("STDEV.P(A1:A10)", "26.0545581424825"),
                ("STDEVP(A1:A10)", "26.0545581424825"),
                ("VAR(A1:A10)", "754.266666666667"),
                ("VAR.S(A1:A10)", "754.266666666667"),
                ("VAR.P(A1:A10)", "678.84"),
                ("VARP(A1:A10)", "678.84"),
                ("STDEV(1)", "#DIV/0!"),
                ("VAR(5)", "#DIV/0!"),
                ("VAR.P(5)", "0"),
                ("STDEV.P(B1:B3)", "#DIV/0!"),
                ("VAR(1, #N/A)", "#N/A"),
                ("STDEV(2, 4, 4, 4, 5, 5, 7, 9)", "2.1380899352994"),
                ("STDEV.P(2, 4, 4, 4, 5, 5, 7, 9)", "2"),
            ],
        );
    }

    #[test]
    fn aliases_and_a_variants() {
        let mut m = data();
        check(
            &mut m,
            &[
                ("MINA(B1:B4)", "0"),
                ("MINA(A1:A5)", "2"),
                ("MINA(5, TRUE)", "1"),
                ("MINA(D1:D3)", "0"),
                ("MINA(\"x\")", "#VALUE!"),
                ("MAXA(A1:A5)", "27"),
                ("MAXA(\"2\", 1)", "2"),
                ("MAXA(D1:D3)", "0"),
                ("MODE.SNGL(C1:C5)", "5"),
                ("MODE.SNGL(1, 2)", "#N/A"),
                ("MODE.SNGL(B1:B5)", "#N/A"),
                ("MODE.SNGL(\"x\", 1)", "#VALUE!"),
                ("STDEV.S(A1:A5)", "9.46044396421225"),
                ("STDEV.S(1)", "#DIV/0!"),
                ("STDEV.S(1, 3)", "1.4142135623731"),
                ("STDEV.S(B1:B5)", "#N/A"),
                ("STDEV.P(A1:A5)", "8.46167832052247"),
                ("STDEV.P(4)", "0"),
                ("STDEVP(1, 3)", "1"),
                ("STDEVP(\"x\")", "#VALUE!"),
                ("STDEVP(D1:D2)", "#DIV/0!"),
                ("VAR(1, 2, 3, 4)", "1.66666666666667"),
                ("VAR.S(1, 3)", "2"),
                ("VAR.S(A1:A5)", "89.5"),
                ("VAR.S(1)", "#DIV/0!"),
                ("VAR.S(\"x\", 1)", "#VALUE!"),
                ("VAR.P(A1:A5)", "71.6"),
                ("VAR.P(1, 3)", "1"),
                ("VAR.P(D1:D2)", "#DIV/0!"),
                ("VARP(1, 2, 3, 4)", "1.25"),
                ("VARP(7)", "0"),
                ("VARP(B1:B5)", "#N/A"),
                ("VARP(D1:D2)", "#DIV/0!"),
                ("RANK.EQ(10, A1:A5)", "2"),
                ("RANK.EQ(10, A1:A5, 1)", "4"),
                ("RANK.EQ(11, A1:A5)", "#N/A"),
                ("RANK.EQ(2, A1:A5, 0)", "5"),
                ("RANK.AVG(5, C1:C5)", "3"),
                ("RANK.AVG(5, C1:C5, 1)", "3"),
                ("RANK.AVG(8, C1:C5)", "1"),
                ("RANK.AVG(9, C1:C5)", "#N/A"),
                ("PERCENTILE(A1:A5, 0.5)", "9"),
                ("PERCENTILE.INC(A1:A5, 0.25)", "7"),
                ("PERCENTILE.INC(A1:A5, 0.1)", "4"),
                ("PERCENTILE.INC(A1:A5, -0.1)", "#NUM!"),
                ("PERCENTILE.INC(B1:B5, 0.5)", "#N/A"),
                ("QUARTILE.INC(A1:A5, 3)", "10"),
                ("QUARTILE.INC(A1:A5, 1.9)", "7"),
                ("QUARTILE.INC(B1:B4, 1)", "#NUM!"),
                ("QUARTILE.INC(A1:A5, \"x\")", "#VALUE!"),
            ],
        );
    }

    #[test]
    fn order_statistics() {
        let mut m = Mock::grid(&[
            &["3", "4"],
            &["5", "2"],
            &["3", "4"],
            &["5", "6"],
            &["4", "7"],
        ]);
        check(
            &mut m,
            &[
                ("LARGE(A1:B5, 3)", "5"),
                ("LARGE(A1:B5, 7)", "4"),
                ("LARGE(A1:B5, 1)", "7"),
                ("LARGE(A1:B5, 0)", "#NUM!"),
                ("LARGE(A1:B5, 11)", "#NUM!"),
                ("LARGE({1,2,3}, 1.5)", "2"),
                ("SMALL(A1:A5, 4)", "5"),
                ("SMALL(B1:B5, 2)", "4"),
                ("SMALL(A1:B5, 1)", "2"),
                ("SMALL({1}, 2)", "#NUM!"),
                ("SMALL(C1:C5, 1)", "#NUM!"),
                ("RANK(A3, A1:A5, 1)", "1"),
                ("RANK(7, B1:B5)", "1"),
                ("RANK(2, B1:B5)", "5"),
                ("RANK(4, B1:B5)", "3"),
                ("RANK(9, B1:B5)", "#N/A"),
                ("RANK.EQ(5, A1:A5)", "1"),
                ("RANK.AVG(4, B1:B5)", "3.5"),
                ("PERCENTILE({1,3,2,4}, 0.3)", "1.9"),
                ("PERCENTILE.INC({1,2,3,4}, 0)", "1"),
                ("PERCENTILE({1,2,3,4}, 1)", "4"),
                ("PERCENTILE({1,2,3,4}, 1.5)", "#NUM!"),
                ("PERCENTILE(C1:C3, 0.5)", "#NUM!"),
                ("QUARTILE({1,2,4,7,8,9,10,12}, 1)", "3.5"),
                ("QUARTILE({1,2,4,7,8,9,10,12}, 0)", "1"),
                ("QUARTILE({1,2,4,7,8,9,10,12}, 2)", "7.5"),
                ("QUARTILE.INC({1,2,4,7,8,9,10,12}, 4.7)", "12"),
                ("QUARTILE({1,2}, 5)", "#NUM!"),
                ("QUARTILE({1,2}, -1)", "#NUM!"),
            ],
        );
    }

    #[test]
    fn conditional_aggregates() {
        // Excel's SUMIF sample: property values and commissions.
        let mut m = Mock::grid(&[
            &["100000", "7000", "Vegetables", "Tomatoes", "2300"],
            &["200000", "14000", "Vegetables", "Celery", "5500"],
            &["300000", "21000", "Fruits", "Oranges", "800"],
            &["400000", "28000", "", "Butter", "400"],
            &["", "", "Vegetables", "Carrots", "4200"],
            &["", "", "Fruits", "Apples", "1200"],
        ]);
        check(
            &mut m,
            &[
                ("SUMIF(A1:A4, \">160000\", B1:B4)", "63000"),
                ("SUMIF(A1:A4, \">160000\")", "900000"),
                ("SUMIF(A1:A4, 300000, B1:B4)", "21000"),
                ("SUMIF(C1:C6, \"Fruits\", E1:E6)", "2000"),
                ("SUMIF(C1:C6, \"Vegetables\", E1:E6)", "12000"),
                ("SUMIF(D1:D6, \"*es\", E1:E6)", "4300"),
                ("SUMIF(C1:C6, \"\", E1:E6)", "400"),
                ("SUMIF(C1:C6, \"fruits\", E1)", "2000"),
                ("SUMIF(A1:A4, \">160000\", B1)", "63000"),
                ("SUMIF(C1:C6, \"x\", Nope!A1)", "#REF!"),
                ("SUMIF(C1:C6, C1:C2, E1:E6)", "#VALUE!"),
                ("COUNTIF(C1:C6, \"Vegetables\")", "3"),
                ("COUNTIF(D1:D6, \"?????s\")", "1"),
                ("COUNTIF(A1:A6, \">=200000\")", "3"),
                ("COUNTIF(A1:A6, \"<>\")", "4"),
                ("COUNTIF(A1:A6, \"\")", "2"),
                ("COUNTIF(A1:A10, \"\")", "6"),
                ("COUNTIF(A:A, \"=\")", "2"),
                ("COUNTIF(C1:C6, \"<>Fruits\")", "4"),
                ("COUNTIF(E1:E6, \">1000\")", "4"),
                ("COUNTIFS(C1:C6, \"Vegetables\", E1:E6, \">3000\")", "2"),
                (
                    "COUNTIFS(C1:C6, \"Vegetables\", E1:E5, \">3000\")",
                    "#VALUE!",
                ),
                ("COUNTIFS(A1:A6, \"<>\", B1:B6, \"<>\")", "4"),
                ("COUNTIFS(F1:F20, \"\", G1:G20, \"\")", "20"),
                ("COUNTIFS(C1:C6, \"*\")", "5"),
                (
                    "SUMIFS(E1:E6, C1:C6, \"Vegetables\", D1:D6, \"C*\")",
                    "9700",
                ),
                ("SUMIFS(E1:E6, C1:C6, \"Fruits\")", "2000"),
                ("SUMIFS(E1:E6, C1:C6, \"Nope\")", "0"),
                ("SUMIFS(E1:E5, C1:C6, \"Fruits\")", "#VALUE!"),
                (
                    "SUMIFS(E1:E6, C1:C6, \"<>Fruits\", E1:E6, \"<5000\")",
                    "6900",
                ),
                ("AVERAGEIF(B1:B4, \"<23000\")", "14000"),
                ("AVERAGEIF(A1:A4, \"<95000\")", "#DIV/0!"),
                ("AVERAGEIF(A1:A4, \">250000\", B1:B4)", "24500"),
                ("AVERAGEIF(D1:D6, \"*es\", E1:E6)", "1433.33333333333"),
                ("AVERAGEIF(C1:C6, \"Fruits\", E1:E6)", "1000"),
                (
                    "AVERAGEIFS(E1:E6, C1:C6, \"Vegetables\", E1:E6, \">2500\")",
                    "4850",
                ),
                ("AVERAGEIFS(E1:E6, C1:C6, \"none\")", "#DIV/0!"),
                ("AVERAGEIFS(E1:E6, C1:C6, \"Fruits\")", "1000"),
                ("AVERAGEIFS(E1:E6, C1:C5, \"Fruits\")", "#VALUE!"),
                ("AVERAGEIFS(E1:E6, D1:D6, \"?a*\")", "4200"),
                ("MAXIFS(E1:E6, C1:C6, \"Vegetables\")", "5500"),
                ("MAXIFS(E1:E6, C1:C6, \"Fruits\", E1:E6, \"<1000\")", "800"),
                ("MAXIFS(E1:E6, C1:C6, \"none\")", "0"),
                ("MAXIFS(E1:E6, C1:C3, \"none\")", "#VALUE!"),
                ("MAXIFS(B1:B6, A1:A6, \">0\")", "28000"),
                ("MINIFS(E1:E6, C1:C6, \"Vegetables\")", "2300"),
                ("MINIFS(E1:E6, C1:C6, \"Fruits\")", "800"),
                ("MINIFS(E1:E6, C1:C6, \"none\")", "0"),
                ("MINIFS(E1:E6, C1:C6, \"<>\", E1:E6, \">1000\")", "1200"),
                ("MINIFS(E1:E6, C1:C3, \"none\")", "#VALUE!"),
            ],
        );
    }

    #[test]
    fn criteria_edge_cases() {
        let mut m = Mock::grid(&[
            &["1", "#DIV/0!"],
            &["'1", "2"],
            &["TRUE", "3"],
            &["apple", "4"],
            &["", "5"],
        ]);
        check(
            &mut m,
            &[
                ("COUNTIF(A1:A5, 1)", "2"),
                ("COUNTIF(A1:A5, \"1\")", "2"),
                ("COUNTIF(A1:A5, \">0\")", "1"),
                ("COUNTIF(A1:A5, TRUE)", "1"),
                ("COUNTIF(A1:A5, \"APPLE\")", "1"),
                ("COUNTIF(A1:A5, D1)", "0"),
                ("COUNTIF(B1:B5, \"#DIV/0!\")", "1"),
                ("SUMIF(A1:A5, \"apple\", B1:B5)", "4"),
                ("SUMIF(A1:A5, 1, B1:B5)", "#DIV/0!"),
                ("SUMIF(A1:A5, \"<>1\", B1:B5)", "12"),
                ("COUNTIF({1,2,3}, \">1\")", "2"),
            ],
        );
        check0(&[("COUNTIF(A1:A3, \"x\")", "0"), ("SUMIF(A1:A3, \"\")", "0")]);
    }
}
