//! Lookup and reference functions: `VLOOKUP`, `HLOOKUP`, `XLOOKUP`,
//! `MATCH`, `INDEX`, `ROW`, `COLUMN`, `ROWS`, `COLUMNS`. (`CHOOSE`
//! short-circuits, so the evaluator owns it.)
//!
//! Matching follows Excel: exact matches compare same-typed values, text
//! case-insensitively, with `*`/`?`/`~` wildcards where Excel allows them
//! (`VLOOKUP`/`HLOOKUP`/`MATCH` exact mode, `XLOOKUP` match mode 2); a
//! text `"1"` never matches the number 1. An empty lookup value looks for 0
//! (Excel passes a blank reference as 0); empty cells never match.
//! Approximate matches binary-search as Excel does, so unsorted data gives
//! Excel's (arbitrary but reproducible) answers; cells of another type than
//! the lookup value are stepped over.
//!
//! Exact matches over a range use a [`LookupIndex`] from
//! [`Ctx::lookup_index`] when the host provides one (cached per range), so
//! a column of `VLOOKUP`s over a big table is O(n), not O(n²).

use std::cmp::Ordering;
use std::collections::HashMap;

use super::args::{Area, area, def, given, int, opt_bool, opt_int, opt_num, positive};
use super::{Arg, Ctx, FuncDef, RangeRef, scalar};
use crate::a1::{MAX_COLS, MAX_ROWS};
use crate::formula::criteria::{Pattern, fold, has_wildcards};
use crate::formula::value::{ErrorKind, Value, cmp_text};

pub(super) const FUNCS: &[FuncDef] = &[
    def!(
        "VLOOKUP",
        3,
        4,
        vlookup,
        "lookup_value, table_array, col_index_num, [range_lookup]"
    ),
    def!(
        "HLOOKUP",
        3,
        4,
        hlookup,
        "lookup_value, table_array, row_index_num, [range_lookup]"
    ),
    def!(
        "XLOOKUP",
        3,
        6,
        xlookup,
        "lookup_value, lookup_array, return_array, [if_not_found], [match_mode], [search_mode]"
    ),
    def!(
        "MATCH",
        2,
        3,
        match_,
        "lookup_value, lookup_array, [match_type]"
    ),
    def!(
        "INDEX",
        2,
        4,
        index,
        "array, row_num, [column_num], [area_num]"
    ),
    def!("ROW", 0, 1, row, "[reference]"),
    def!("COLUMN", 0, 1, column, "[reference]"),
    def!("ROWS", 1, 1, rows, "array"),
    def!("COLUMNS", 1, 1, columns, "array"),
];

/// A normalised exact-match key: numbers by value (`-0` is `0`), text
/// case-folded, booleans. Empty cells and errors are not keys.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    Num(u64),
    Text(Box<str>),
    Bool(bool),
}

impl Key {
    pub fn of(v: &Value) -> Option<Key> {
        match v {
            Value::Number(n) => Some(Key::num(*n)),
            Value::Text(s) => Some(Key::Text(s.chars().map(fold).collect())),
            Value::Bool(b) => Some(Key::Bool(*b)),
            Value::Empty | Value::Error(_) => None,
        }
    }

    /// A number's key (`-0` and `0` are one key).
    pub fn num(n: f64) -> Key {
        Key::Num(if n == 0.0 { 0 } else { n.to_bits() })
    }
}

/// An exact-match index over one range: each distinct key's first and last
/// position (row-major offsets within the range).
#[derive(Debug, Default)]
pub struct LookupIndex {
    map: HashMap<Key, (usize, usize)>,
}

impl LookupIndex {
    /// Index `range` (clamped to the used area by the caller), reading only
    /// its cells that hold data.
    pub fn build(ctx: &dyn Ctx, range: &RangeRef) -> LookupIndex {
        let mut map: HashMap<Key, (usize, usize)> = HashMap::new();
        let cols = range.cols();
        if let Some(part) = ctx.data_part(range) {
            ctx.for_each(&part, &mut |r, c, v| {
                if let Some(k) = Key::of(&v) {
                    let i = (r - range.r0) * cols + (c - range.c0);
                    map.entry(k)
                        .and_modify(|e| {
                            e.0 = e.0.min(i);
                            e.1 = e.1.max(i);
                        })
                        .or_insert((i, i));
                }
            });
        }
        LookupIndex { map }
    }

    /// The first position holding `v`.
    pub fn first(&self, v: &Value) -> Option<usize> {
        self.map.get(&Key::of(v)?).map(|e| e.0)
    }

    /// The last position holding `v`.
    pub fn last(&self, v: &Value) -> Option<usize> {
        self.map.get(&Key::of(v)?).map(|e| e.1)
    }

    #[cfg(test)]
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.map.len()
    }
}

/// The lookup value: errors propagate, empty means 0.
fn needle(arg: &Arg, ctx: &mut dyn Ctx) -> Result<Value, ErrorKind> {
    match scalar(arg, ctx) {
        Value::Error(e) => Err(e),
        Value::Empty => Ok(Value::Number(0.0)),
        v => Ok(v),
    }
}

fn same_type(a: &Value, b: &Value) -> bool {
    matches!(
        (a, b),
        (Value::Number(_), Value::Number(_))
            | (Value::Text(_), Value::Text(_))
            | (Value::Bool(_), Value::Bool(_))
    )
}

/// Order of two same-typed values.
fn cmp_same(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        (Value::Text(x), Value::Text(y)) => cmp_text(x, y),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        _ => Ordering::Equal,
    }
}

fn exact_eq(cell: &Value, needle: &Value) -> bool {
    same_type(cell, needle) && cmp_same(cell, needle) == Ordering::Equal
}

/// First (or, `reverse`, last) position of `needle` in the vector `vec`.
/// `wildcards` turns on pattern matching for a text needle.
fn find_exact(
    ctx: &mut dyn Ctx,
    vec: &Area,
    needle: &Value,
    wildcards: bool,
    reverse: bool,
) -> Option<usize> {
    if let Value::Text(t) = needle
        && wildcards
        && has_wildcards(t)
    {
        let pat = Pattern::new(t);
        let hit = |v: &Value| matches!(v, Value::Text(s) if pat.matches(s));
        return scan(&*ctx, vec, reverse, hit);
    }
    if let Area::Range(r) = vec
        && let Some(index) = ctx.lookup_index(r)
    {
        return if reverse {
            index.last(needle)
        } else {
            index.first(needle)
        };
    }
    scan(&*ctx, vec, reverse, |v| exact_eq(v, needle))
}

/// Linear scan over the cells that hold data.
fn scan(ctx: &dyn Ctx, vec: &Area, reverse: bool, hit: impl Fn(&Value) -> bool) -> Option<usize> {
    let (r, c) = vec.data_dims(ctx);
    let vals = if vec.cols() == 1 {
        vec.window(ctx, r, 1)
    } else {
        vec.window(ctx, 1, c)
    };
    if reverse {
        vals.iter().rposition(hit)
    } else {
        vals.iter().position(hit)
    }
}

/// Excel's binary search over sorted data: the last position whose value is
/// ≤ `needle` (ascending) or ≥ `needle` (descending), comparing only cells
/// of the needle's type and stepping over the rest.
fn find_sorted(ctx: &dyn Ctx, vec: &Area, needle: &Value, descending: bool) -> Option<usize> {
    // Inclusive bounds with mid = (lo + hi) / 2, as Excel probes: on
    // unsorted data this decides which answer (or #N/A) comes out, e.g.
    // MATCH(40, {25,38,40,41}, -1) is #N/A in Excel.
    let n = vec.len() as i64;
    let (mut lo, mut hi) = (0i64, n - 1);
    let mut best = None;
    let typed = |i: i64| {
        let v = vec.vget(ctx, i as usize);
        same_type(&v, needle).then_some((i, v))
    };
    while lo <= hi {
        let mid = lo + (hi - lo) / 2;
        let found = (mid..=hi)
            .find_map(typed)
            .or_else(|| (lo..mid).rev().find_map(typed));
        let Some((j, v)) = found else { break };
        let o = cmp_same(&v, needle);
        let keep = if descending {
            o != Ordering::Less
        } else {
            o != Ordering::Greater
        };
        if keep {
            best = Some(j as usize);
            lo = j + 1;
        } else {
            hi = j - 1;
        }
    }
    best
}

fn vlookup(args: &[Arg], ctx: &mut dyn Ctx) -> Result<Value, ErrorKind> {
    table_lookup(args, ctx, false)
}

fn hlookup(args: &[Arg], ctx: &mut dyn Ctx) -> Result<Value, ErrorKind> {
    table_lookup(args, ctx, true)
}

/// VLOOKUP (search the first column, return from column `k`) and HLOOKUP
/// (first row, row `k`).
fn table_lookup(args: &[Arg], ctx: &mut dyn Ctx, horizontal: bool) -> Result<Value, ErrorKind> {
    let needle = needle(&args[0], ctx)?;
    let table = area(&args[1], ctx)?;
    let k = int(&args[2], ctx)?;
    let approx = opt_bool(args.get(3), ctx, true)?;
    let span = if horizontal {
        table.rows()
    } else {
        table.cols()
    };
    let k = positive(k)?;
    if k > span {
        return Err(ErrorKind::Ref);
    }
    let keys = if horizontal {
        table.sub(0, 0, 1, table.cols())
    } else {
        table.sub(0, 0, table.rows(), 1)
    };
    let i = if approx {
        find_sorted(&*ctx, &keys, &needle, false)
    } else {
        find_exact(ctx, &keys, &needle, true, false)
    }
    .ok_or(ErrorKind::NA)?;
    let k = k - 1;
    Ok(if horizontal {
        table.get(&*ctx, k, i)
    } else {
        table.get(&*ctx, i, k)
    })
}

/// MATCH's `match_type`: its sign.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MatchType {
    /// Positive (the default 1): the last value ≤ in ascending data.
    LessOrEqual,
    /// 0: an exact match, with wildcards.
    Exact,
    /// Negative: the last value ≥ in descending data.
    GreaterOrEqual,
}

impl From<f64> for MatchType {
    fn from(t: f64) -> MatchType {
        if t == 0.0 {
            MatchType::Exact
        } else if t < 0.0 {
            MatchType::GreaterOrEqual
        } else {
            MatchType::LessOrEqual
        }
    }
}

/// `MATCH(value, vector, [type])` (see [`MatchType`]). A 2-D lookup array
/// is `#N/A`.
fn match_(args: &[Arg], ctx: &mut dyn Ctx) -> Result<Value, ErrorKind> {
    let needle = needle(&args[0], ctx)?;
    let vec = area(&args[1], ctx)?;
    let t = MatchType::from(opt_num(args.get(2), ctx, 1.0)?);
    if !vec.is_vector() {
        return Err(ErrorKind::NA);
    }
    let i = match t {
        MatchType::Exact => find_exact(ctx, &vec, &needle, true, false),
        MatchType::LessOrEqual => find_sorted(&*ctx, &vec, &needle, false),
        MatchType::GreaterOrEqual => find_sorted(&*ctx, &vec, &needle, true),
    };
    i.map(|i| Value::Number((i + 1) as f64))
        .ok_or(ErrorKind::NA)
}

/// XLOOKUP's `match_mode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MatchMode {
    /// 0 (the default): exact only.
    Exact,
    /// -1: exact, else the next smaller value.
    NextSmaller,
    /// 1: exact, else the next larger value.
    NextLarger,
    /// 2: `*`/`?`/`~` wildcards.
    Wildcard,
}

impl TryFrom<i64> for MatchMode {
    type Error = ErrorKind;

    fn try_from(n: i64) -> Result<MatchMode, ErrorKind> {
        match n {
            0 => Ok(MatchMode::Exact),
            -1 => Ok(MatchMode::NextSmaller),
            1 => Ok(MatchMode::NextLarger),
            2 => Ok(MatchMode::Wildcard),
            _ => Err(ErrorKind::Value),
        }
    }
}

/// XLOOKUP's `search_mode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SearchMode {
    /// 1 (the default): first to last.
    First,
    /// -1: last to first.
    Last,
    /// 2: binary search over ascending data.
    Ascending,
    /// -2: binary search over descending data.
    Descending,
}

impl TryFrom<i64> for SearchMode {
    type Error = ErrorKind;

    fn try_from(n: i64) -> Result<SearchMode, ErrorKind> {
        match n {
            1 => Ok(SearchMode::First),
            -1 => Ok(SearchMode::Last),
            2 => Ok(SearchMode::Ascending),
            -2 => Ok(SearchMode::Descending),
            _ => Err(ErrorKind::Value),
        }
    }
}

/// `XLOOKUP(value, lookup, return, [if_not_found], [match_mode],
/// [search_mode])`. v1 returns one value: with a 2-D return array, the
/// first cell of the matched row (or column).
fn xlookup(args: &[Arg], ctx: &mut dyn Ctx) -> Result<Value, ErrorKind> {
    let needle = needle(&args[0], ctx)?;
    let look = area(&args[1], ctx)?;
    let ret = area(&args[2], ctx)?;
    let match_mode = opt_int(args.get(4), ctx, 0)?;
    let search_mode = opt_int(args.get(5), ctx, 1)?;
    if !look.is_vector() {
        return Err(ErrorKind::Value);
    }
    let match_mode = MatchMode::try_from(match_mode)?;
    let search_mode = SearchMode::try_from(search_mode)?;
    let vertical = look.cols() == 1;
    let n = look.len();
    if (vertical && ret.rows() != n) || (!vertical && ret.cols() != n) {
        return Err(ErrorKind::Value);
    }
    let found = match search_mode {
        SearchMode::First => xlinear(ctx, &look, &needle, match_mode, false),
        SearchMode::Last => xlinear(ctx, &look, &needle, match_mode, true),
        SearchMode::Ascending => xbinary(&*ctx, &look, &needle, match_mode, false),
        SearchMode::Descending => xbinary(&*ctx, &look, &needle, match_mode, true),
    };
    match found {
        Some(i) => Ok(if vertical {
            ret.get(&*ctx, i, 0)
        } else {
            ret.get(&*ctx, 0, i)
        }),
        None => match given(args.get(3)) {
            None => Err(ErrorKind::NA),
            Some(a) => Ok(scalar(a, ctx)),
        },
    }
}

fn xlinear(
    ctx: &mut dyn Ctx,
    look: &Area,
    needle: &Value,
    mode: MatchMode,
    reverse: bool,
) -> Option<usize> {
    let wildcards = mode == MatchMode::Wildcard;
    if let Some(i) = find_exact(ctx, look, needle, wildcards, reverse) {
        return Some(i);
    }
    let smaller = match mode {
        MatchMode::Exact | MatchMode::Wildcard => return None,
        MatchMode::NextSmaller => true,
        MatchMode::NextLarger => false,
    };
    // Next smaller or next larger: the closest same-typed value, the first
    // one met in search order on ties.
    let (r, c) = look.data_dims(&*ctx);
    let vals = if look.cols() == 1 {
        look.window(&*ctx, r, 1)
    } else {
        look.window(&*ctx, 1, c)
    };
    let order: Box<dyn Iterator<Item = usize>> = if reverse {
        Box::new((0..vals.len()).rev())
    } else {
        Box::new(0..vals.len())
    };
    let mut best: Option<usize> = None;
    for i in order {
        let v = &vals[i];
        if !same_type(v, needle) {
            continue;
        }
        let o = cmp_same(v, needle);
        let side_ok = if smaller {
            o == Ordering::Less
        } else {
            o == Ordering::Greater
        };
        if !side_ok {
            continue;
        }
        let better = match best {
            None => true,
            Some(b) => {
                let ob = cmp_same(v, &vals[b]);
                if smaller {
                    ob == Ordering::Greater
                } else {
                    ob == Ordering::Less
                }
            }
        };
        if better {
            best = Some(i);
        }
    }
    best
}

fn xbinary(
    ctx: &dyn Ctx,
    look: &Area,
    needle: &Value,
    mode: MatchMode,
    descending: bool,
) -> Option<usize> {
    let j = find_sorted(ctx, look, needle, descending);
    let exact = j.filter(|&j| exact_eq(&look.vget(ctx, j), needle));
    if exact.is_some() {
        return exact;
    }
    if mode == MatchMode::Wildcard {
        // Wildcards aren't valid with binary search: fall back to a scan.
        return if let Value::Text(t) = needle {
            let pat = Pattern::new(t);
            scan(
                ctx,
                look,
                false,
                |v| matches!(v, Value::Text(s) if pat.matches(s)),
            )
        } else {
            None
        };
    }
    let next_typed =
        |from: usize| (from..look.len()).find(|&i| same_type(&look.vget(ctx, i), needle));
    // Ascending: j is the last ≤ (next smaller); the next larger follows it.
    // Descending: j is the last ≥ (next larger); the next smaller follows it.
    let toward_j = if descending {
        mode == MatchMode::NextLarger
    } else {
        mode == MatchMode::NextSmaller
    };
    match (toward_j, j) {
        (_, _) if mode == MatchMode::Exact => None,
        (true, j) => j,
        (false, Some(j)) => next_typed(j + 1),
        (false, None) => next_typed(0),
    }
}

/// `INDEX(array, row, [col], [area])`: the value at a position. A row or
/// column of 0 asks for a whole column or row, which would spill — v1 has
/// no spilling, so that's `#VALUE!` unless the dimension is a single cell.
/// Returns a value, not a reference (`INDEX(…):INDEX(…)` isn't supported).
fn index(args: &[Arg], ctx: &mut dyn Ctx) -> Result<Value, ErrorKind> {
    if let Some(a) = given(args.get(3))
        && int(a, ctx)? != 1
    {
        return Err(ErrorKind::Ref);
    }
    // Whole-column references keep Excel's dimensions here.
    let (rows, cols, base) = match &args[0] {
        Arg::Range(rg) => {
            ctx.extent(rg.sheet.as_deref()).ok_or(ErrorKind::Ref)?;
            let rows = if rg.r1 == usize::MAX {
                MAX_ROWS - rg.r0.min(MAX_ROWS - 1)
            } else {
                rg.rows()
            };
            let cols = if rg.c1 == usize::MAX {
                MAX_COLS - rg.c0.min(MAX_COLS - 1)
            } else {
                rg.cols()
            };
            (rows, cols, None)
        }
        a => {
            let ar = area(a, ctx)?;
            (ar.rows(), ar.cols(), Some(ar))
        }
    };
    let row_arg = opt_int(args.get(1), ctx, 0)?;
    let col_given = given(args.get(2)).is_some();
    let col_arg = opt_int(args.get(2), ctx, 0)?;
    let (mut r, mut c) = if !col_given && rows == 1 && cols > 1 {
        // A single row: the one number picks the column.
        (1, row_arg)
    } else if !col_given && cols == 1 {
        (row_arg, 1)
    } else {
        (row_arg, col_arg)
    };
    if r < 0 || c < 0 {
        return Err(ErrorKind::Value);
    }
    if r as usize > rows || c as usize > cols {
        return Err(ErrorKind::Ref);
    }
    if r == 0 {
        if rows != 1 {
            return Err(ErrorKind::Value);
        }
        r = 1;
    }
    if c == 0 {
        if cols != 1 {
            return Err(ErrorKind::Value);
        }
        c = 1;
    }
    let (r, c) = (r as usize - 1, c as usize - 1);
    Ok(match (base, &args[0]) {
        (Some(ar), _) => ar.get(&*ctx, r, c),
        (None, Arg::Range(rg)) => ctx.cell(rg.sheet.as_deref(), rg.r0 + r, rg.c0 + c),
        _ => Value::Error(ErrorKind::Value),
    })
}

/// `ROW([ref])`: the 1-based row of the reference's top-left cell, or of
/// the formula's own cell.
fn row(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    match given(args.first()) {
        None => Value::Number((ctx.here().0 + 1) as f64),
        Some(Arg::Range(r)) => Value::Number((r.r0 + 1) as f64),
        Some(_) => Value::Error(ErrorKind::Value),
    }
}

fn column(args: &[Arg], ctx: &mut dyn Ctx) -> Value {
    match given(args.first()) {
        None => Value::Number((ctx.here().1 + 1) as f64),
        Some(Arg::Range(r)) => Value::Number((r.c0 + 1) as f64),
        Some(_) => Value::Error(ErrorKind::Value),
    }
}

/// `ROWS`: open-ended references count to Excel's sheet size (`ROWS(A:A)`
/// is 1,048,576).
fn rows(args: &[Arg], _: &mut dyn Ctx) -> Value {
    Value::Number(match &args[0] {
        Arg::Range(r) if r.r1 == usize::MAX => MAX_ROWS.saturating_sub(r.r0),
        Arg::Range(r) => r.rows(),
        Arg::Array(a) => a.rows,
        _ => 1,
    } as f64)
}

fn columns(args: &[Arg], _: &mut dyn Ctx) -> Value {
    Value::Number(match &args[0] {
        Arg::Range(r) if r.c1 == usize::MAX => MAX_COLS.saturating_sub(r.c0),
        Arg::Range(r) => r.cols(),
        Arg::Array(a) => a.cols,
        _ => 1,
    } as f64)
}

#[cfg(test)]
mod tests {
    use super::super::testing::{Mock, check, check0, eval};
    use super::*;

    /// Excel's VLOOKUP sample: density, viscosity, temperature.
    fn density() -> Mock {
        Mock::grid(&[
            &["0.457", "3.55", "500"],
            &["0.525", "3.25", "400"],
            &["0.606", "2.93", "300"],
            &["0.675", "2.75", "250"],
            &["0.746", "2.57", "200"],
            &["0.835", "2.38", "150"],
            &["0.946", "2.17", "100"],
            &["1.09", "1.95", "50"],
            &["1.29", "1.71", "0"],
        ])
    }

    #[test]
    fn vlookup_hlookup() {
        let mut m = density();
        let cases: &[(&str, &str)] = &[
            ("VLOOKUP(1, A1:C9, 2)", "2.17"),
            ("VLOOKUP(1, A1:C9, 3, TRUE)", "100"),
            ("VLOOKUP(0.7, A1:C9, 3, FALSE)", "#N/A"),
            ("VLOOKUP(0.1, A1:C9, 2, TRUE)", "#N/A"),
            ("VLOOKUP(2, A1:C9, 2, TRUE)", "1.71"),
            ("VLOOKUP(0.746, A1:C9, 3, FALSE)", "200"),
            ("VLOOKUP(0.746, A:C, 3, FALSE)", "200"),
            ("VLOOKUP(1, A1:C9, 4)", "#REF!"),
            ("VLOOKUP(1, A1:C9, 0)", "#VALUE!"),
            ("VLOOKUP(#N/A, A1:C9, 2)", "#N/A"),
            ("VLOOKUP(\"x\", A1:C9, 2)", "#N/A"),
        ];
        check(&mut m, cases);
        m.use_index = true;
        check(&mut m, cases);
        assert!(m.index_builds > 0);

        let mut t = Mock::grid(&[
            &["Axles", "Bearings", "Bolts"],
            &["4", "4", "9"],
            &["5", "7", "10"],
            &["6", "8", "11"],
        ]);
        check(
            &mut t,
            &[
                ("HLOOKUP(\"Axles\", A1:C4, 2, TRUE)", "4"),
                ("HLOOKUP(\"Bearings\", A1:C4, 3, FALSE)", "7"),
                ("HLOOKUP(\"B\", A1:C4, 3, TRUE)", "5"),
                ("HLOOKUP(\"Bolts\", A1:C4, 4)", "11"),
                (
                    "HLOOKUP(3, {1,2,3;\"a\",\"b\",\"c\";\"d\",\"e\",\"f\"}, 2, TRUE)",
                    "\"c\"",
                ),
                ("HLOOKUP(\"bo*\", A1:C4, 2, FALSE)", "9"),
                ("HLOOKUP(\"Bolts\", A1:C4, 5)", "#REF!"),
                ("HLOOKUP(\"Nuts\", A1:C4, 2, FALSE)", "#N/A"),
            ],
        );
    }

    #[test]
    fn exact_matching_rules() {
        let mut m = Mock::grid(&[
            &["Apple", "1"],
            &["'1", "2"],
            &["1", "3"],
            &["TRUE", "4"],
            &["", "5"],
            &["a*c", "6"],
            &["apple", "7"],
            &["0", "8"],
        ]);
        let cases: &[(&str, &str)] = &[
            ("VLOOKUP(\"APPLE\", A1:B8, 2, FALSE)", "1"),
            ("VLOOKUP(1, A1:B8, 2, FALSE)", "3"),
            ("VLOOKUP(\"1\", A1:B8, 2, FALSE)", "2"),
            ("VLOOKUP(TRUE, A1:B8, 2, FALSE)", "4"),
            ("VLOOKUP(\"a~*c\", A1:B8, 2, FALSE)", "6"),
            ("VLOOKUP(\"a*\", A1:B8, 2, FALSE)", "1"),
            ("VLOOKUP(D1, A1:B8, 2, FALSE)", "8"),
            ("MATCH(\"apple\", A1:A8, 0)", "1"),
            ("MATCH(\"?pple\", A1:A8, 0)", "1"),
            ("XLOOKUP(\"apple\", A1:A8, B1:B8, , 0, -1)", "7"),
            ("XLOOKUP(\"a*\", A1:A8, B1:B8)", "#N/A"),
            ("XLOOKUP(\"a*\", A1:A8, B1:B8, , 2, -1)", "7"),
        ];
        check(&mut m, cases);
        m.use_index = true;
        check(&mut m, cases);
    }

    #[test]
    fn match_function() {
        let mut m = Mock::grid(&[
            &["Bananas", "25"],
            &["Oranges", "38"],
            &["Apples", "40"],
            &["Pears", "41"],
        ]);
        check(
            &mut m,
            &[
                ("MATCH(39, B1:B4, 1)", "2"),
                ("MATCH(41, B1:B4, 0)", "4"),
                ("MATCH(40, B1:B4, -1)", "#N/A"),
                ("MATCH(40, B1:B4)", "3"),
                ("MATCH(5, B1:B4)", "#N/A"),
                ("MATCH(100, B1:B4)", "4"),
                ("MATCH(\"or*\", A1:A4, 0)", "2"),
                ("MATCH(\"x\", A1:B4, 0)", "#N/A"),
                ("MATCH(3, {5,4,3,2,1}, -1)", "3"),
                ("MATCH(3.5, {5,4,3,2,1}, -1)", "2"),
                ("MATCH(6, {5,4,3,2,1}, -1)", "#N/A"),
                ("MATCH(\"b\", {\"a\",\"b\",\"c\"}, 0)", "2"),
                ("MATCH(#DIV/0!, B1:B4, 0)", "#DIV/0!"),
            ],
        );
    }

    #[test]
    fn xlookup_modes() {
        let mut m = Mock::grid(&[
            &["China", "CN", "10"],
            &["India", "IN", "20"],
            &["United States", "US", "30"],
            &["Indonesia", "ID", "40"],
            &["Pakistan", "PK", "50"],
        ]);
        check(
            &mut m,
            &[
                ("XLOOKUP(\"India\", A1:A5, B1:B5)", "\"IN\""),
                ("XLOOKUP(\"Brazil\", A1:A5, B1:B5)", "#N/A"),
                (
                    "XLOOKUP(\"Brazil\", A1:A5, B1:B5, \"Not found\")",
                    "\"Not found\"",
                ),
                ("XLOOKUP(\"In*\", A1:A5, B1:B5, , 2)", "\"IN\""),
                ("XLOOKUP(\"In*\", A1:A5, B1:B5, , 2, -1)", "\"ID\""),
                ("XLOOKUP(35, C1:C5, B1:B5, , -1)", "\"US\""),
                ("XLOOKUP(35, C1:C5, B1:B5, , 1)", "\"ID\""),
                ("XLOOKUP(5, C1:C5, B1:B5, , -1)", "#N/A"),
                ("XLOOKUP(55, C1:C5, B1:B5, , 1)", "#N/A"),
                ("XLOOKUP(30, C1:C5, A1:A5, , 0, 2)", "\"United States\""),
                ("XLOOKUP(35, C1:C5, B1:B5, , -1, 2)", "\"US\""),
                ("XLOOKUP(35, C1:C5, B1:B5, , 1, 2)", "\"ID\""),
                ("XLOOKUP(35, {50,40,30,20,10}, {1,2,3,4,5}, , 1, -2)", "2"),
                ("XLOOKUP(35, {50,40,30,20,10}, {1,2,3,4,5}, , -1, -2)", "3"),
                ("XLOOKUP(\"CN\", B1:B5, A1:C5)", "\"China\""),
                ("XLOOKUP(\"IN\", B1:B5, A1:A4)", "#VALUE!"),
                ("XLOOKUP(\"IN\", B1:B5, A1:A5, , 3)", "#VALUE!"),
                ("XLOOKUP(\"IN\", B1:B5, A1:A5, , 0, 0)", "#VALUE!"),
                ("XLOOKUP(\"IN\", A1:B5, A1:A5)", "#VALUE!"),
                ("XLOOKUP(20, {10,20,30}, {\"a\",\"b\",\"c\"})", "\"b\""),
            ],
        );
    }

    #[test]
    fn index_and_references() {
        let mut m = Mock::grid(&[&["Apples", "Lemons"], &["Bananas", "Pears"]]);
        m.here = (4, 2); // C5
        check(
            &mut m,
            &[
                ("INDEX(A1:B2, 2, 2)", "\"Pears\""),
                ("INDEX(A1:B2, 2, 1)", "\"Bananas\""),
                ("INDEX({1,2;3,4}, 1, 2)", "2"),
                ("INDEX({1,2,3}, 2)", "2"),
                ("INDEX(A1:A2, 2)", "\"Bananas\""),
                ("INDEX(A1:A2, 2, 0)", "\"Bananas\""),
                ("INDEX(A1:B2, 3, 1)", "#REF!"),
                ("INDEX(A1:B2, 0, 1)", "#VALUE!"),
                ("INDEX(A1:B2, -1, 1)", "#VALUE!"),
                ("INDEX(A:A, 100)", ""),
                ("INDEX(A:B, 2, 2)", "\"Pears\""),
                ("INDEX(A1:B2, 1, 1, 2)", "#REF!"),
                ("ROW()", "5"),
                ("ROW(C10)", "10"),
                ("ROW(C10:D20)", "10"),
                ("ROW(A:A)", "1"),
                ("ROW({1,2})", "#VALUE!"),
                ("COLUMN()", "3"),
                ("COLUMN(D10)", "4"),
                ("COLUMN(B:C)", "2"),
                ("COLUMN(5:5)", "1"),
                ("COLUMN(5)", "#VALUE!"),
                ("ROWS(C1:E4)", "4"),
                ("ROWS({1,2,3;4,5,6})", "2"),
                ("ROWS(A:A)", "1048576"),
                ("ROWS(3:4)", "2"),
                ("ROWS(7)", "1"),
                ("COLUMNS(C1:E4)", "3"),
                ("COLUMNS({1,2,3;4,5,6})", "3"),
                ("COLUMNS(1:1)", "16384"),
                ("COLUMNS(A:B)", "2"),
                ("COLUMNS(7)", "1"),
            ],
        );
        check0(&[("INDEX(Nope!A1:B2, 1, 1)", "#REF!")]);
    }

    #[test]
    fn index_built_once_per_call_and_correct_positions() {
        let mut m = Mock::new();
        for i in 0..1000 {
            m.put(None, i, 0, &format!("k{}", i % 500));
            m.put(None, i, 1, &i.to_string());
        }
        m.use_index = true;
        assert_eq!(
            eval(&mut m, "VLOOKUP(\"K499\", A1:B1000, 2, FALSE)"),
            Value::Number(499.0)
        );
        assert_eq!(
            eval(&mut m, "XLOOKUP(\"k7\", A1:A1000, B1:B1000, , 0, -1)"),
            Value::Number(507.0)
        );
        assert_eq!(
            eval(&mut m, "MATCH(\"k0\", A1:A1000, 0)"),
            Value::Number(1.0)
        );
        let r = RangeRef {
            sheet: None,
            r0: 0,
            c0: 0,
            r1: 999,
            c1: 0,
        };
        let idx = LookupIndex::build(&m, &r);
        assert_eq!(idx.len(), 500);
        assert_eq!(idx.first(&Value::text("K3")), Some(3));
        assert_eq!(idx.last(&Value::text("k3")), Some(503));
        assert_eq!(idx.first(&Value::Number(3.0)), None);
    }

    #[test]
    fn approximate_skips_other_types() {
        let mut m = Mock::grid(&[&["1"], &["x"], &["3"], &[""], &["5"], &["y"], &["7"]]);
        check(
            &mut m,
            &[
                ("MATCH(4, A1:A7)", "3"),
                ("MATCH(7, A1:A7)", "7"),
                ("MATCH(0, A1:A7)", "#N/A"),
                ("MATCH(\"z\", A1:A7)", "6"),
                ("MATCH(\"a\", A1:A7)", "#N/A"),
            ],
        );
    }
}
