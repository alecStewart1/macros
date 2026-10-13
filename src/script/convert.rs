use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};
use steel::rvals::SteelVal;

use super::*;

/// Parse a list of `(line-number face-name)` pairs for `render-buffer`.
pub(crate) fn parse_line_faces(value: &SteelVal) -> Vec<(usize, String)> {
    steel_list(value)
        .unwrap_or_default()
        .iter()
        .filter_map(|item| {
            let pair = steel_list(item)?;
            let line = match pair.first()? {
                SteelVal::IntV(n) => (*n).max(0) as usize,
                _ => return None,
            };
            Some((line, steel_string(pair.get(1)?)?))
        })
        .collect()
}

/// Parse a list of `(key label command)` rows for `transient`.
pub(crate) fn parse_transient_item(item: &SteelVal) -> Option<TransientItem> {
    let row = steel_list(item)?;
    Some(TransientItem {
        key: steel_string(row.first()?)?,
        label: steel_string(row.get(1)?)?,
        command: steel_string(row.get(2)?)?,
    })
}

/// Parse `transient`'s rows argument into columns. Two accepted shapes:
///   flat:    `(list (k l c) (k l c) …)`            → one untitled column
///   grouped: `(list (title (k l c) …) (title …))`  → titled columns
/// A group is recognised by its second element being a list (a row) rather than
/// a string (a key), so the two forms are unambiguous.
pub(crate) fn parse_transient_columns(value: &SteelVal) -> Vec<TransientColumn> {
    let elements = steel_list(value).unwrap_or_default();
    let grouped = elements
        .first()
        .and_then(steel_list)
        .and_then(|first| first.get(1).map(|second| steel_list(second).is_some()))
        .unwrap_or(false);
    if !grouped {
        return vec![TransientColumn {
            title: None,
            rows: elements.iter().filter_map(parse_transient_item).collect(),
        }];
    }
    elements
        .iter()
        .filter_map(|group| {
            let group = steel_list(group)?;
            let title = steel_string(group.first()?);
            let rows = group
                .iter()
                .skip(1)
                .filter_map(parse_transient_item)
                .collect();
            Some(TransientColumn { title, rows })
        })
        .collect()
}

/// A callback argument: the *name* of a Scheme function, accepted as a
/// "name" string or a 'name symbol (the canonical config form) — the same
/// coercion as [`steel_string`]. Any other value coerces to `""` ("no
/// callback"), preserving the historical
/// `steel_string(&v).unwrap_or_default()` behavior at every call site.
pub(crate) struct Callback(pub(crate) String);

impl steel::rvals::FromSteelVal for Callback {
    fn from_steelval(val: &SteelVal) -> steel::rvals::Result<Self> {
        Ok(Callback(steel_string(val).unwrap_or_default()))
    }
}

/// Steel's own integer→`u16`/`usize` conversions are bare `as` casts, so a
/// Scheme `-1` or `70000` silently wraps. These argument types validate the
/// range instead and raise a Scheme conversion error when it's violated.
fn int_in_range(val: &SteelVal, what: &str, max: u64) -> steel::rvals::Result<u64> {
    match val {
        SteelVal::IntV(n) if *n >= 0 && (*n as u64) <= max => Ok(*n as u64),
        _ => Err(steel::SteelErr::new(
            steel::rerrs::ErrorKind::ConversionError,
            format!("expected {what} (0..={max}), found: {val}"),
        )),
    }
}

/// A TCP/UDP port argument: an integer in `0..=65535`.
pub(crate) struct Port(pub(crate) u16);

impl steel::rvals::FromSteelVal for Port {
    fn from_steelval(val: &SteelVal) -> steel::rvals::Result<Self> {
        int_in_range(val, "a port", u16::MAX as u64).map(|n| Port(n as u16))
    }
}

/// A non-negative integer argument (a count, size or timeout).
pub(crate) struct NonNeg(pub(crate) usize);

impl steel::rvals::FromSteelVal for NonNeg {
    fn from_steelval(val: &SteelVal) -> steel::rvals::Result<Self> {
        int_in_range(val, "a non-negative integer", isize::MAX as u64).map(|n| NonNeg(n as usize))
    }
}

pub(crate) fn steel_string(value: &SteelVal) -> Option<String> {
    match value {
        SteelVal::StringV(s) => Some(s.to_string()),
        SteelVal::SymbolV(s) => Some(s.to_string()),
        _ => None,
    }
}

// --- grid argument converters (the `grid-*` primitives) ---------------------

/// A value Scheme writes into a cell, as the cell's text: a string as is,
/// a number in its shortest form, a boolean as `TRUE`/`FALSE`; anything
/// else can't go in a cell.
pub(crate) fn steel_cell_text(v: &SteelVal) -> Option<String> {
    match v {
        SteelVal::StringV(s) => Some(s.to_string()),
        SteelVal::IntV(i) => Some(i.to_string()),
        SteelVal::NumV(n) if n.is_finite() => Some(n.to_string()),
        SteelVal::BoolV(b) => Some((if *b { "TRUE" } else { "FALSE" }).to_string()),
        _ => None,
    }
}

/// A rule/filter value from Scheme as text: numbers print plainly, strings
/// and symbols as themselves.
pub(crate) fn steel_value_text(v: &SteelVal) -> String {
    match v {
        SteelVal::IntV(i) => i.to_string(),
        SteelVal::NumV(n) => n.to_string(),
        other => steel_name(other),
    }
}

/// A 0-based row/column index from Scheme (a non-negative integer).
pub(crate) fn steel_index(v: &SteelVal) -> Option<usize> {
    match v {
        SteelVal::IntV(i) if *i >= 0 => Some(*i as usize),
        _ => None,
    }
}

/// A column from Scheme: a 0-based index or a column name (`"C"`, `'AA`).
/// `#f` (and anything else) is `None` — "every column" where that's allowed.
pub(crate) fn steel_col(v: &SteelVal) -> Option<usize> {
    match v {
        SteelVal::StringV(s) | SteelVal::SymbolV(s) => crate::grid::view::parse_col_name(s),
        other => steel_index(other),
    }
}

/// A palette color argument: `#f`, `""` or `none` is no color; an unknown
/// name comes back as the second value, to be reported.
pub(crate) fn steel_palette_color(v: &SteelVal) -> (Option<u8>, Option<String>) {
    if matches!(v, SteelVal::BoolV(false)) {
        return (None, None);
    }
    let name = steel_name(v);
    if name.is_empty() || name.eq_ignore_ascii_case("none") {
        return (None, None);
    }
    match crate::grid::cellfmt::color_index(&name) {
        Some(i) => (Some(i), None),
        None => (None, Some(name)),
    }
}

/// A face, operator, or function name given as a string or a symbol.
pub(crate) fn steel_name(v: &SteelVal) -> String {
    steel_string(v).unwrap_or_default()
}

/// Convert a value passed to `(set-option …)` into an untyped [`OptionValue`].
/// Numbers, booleans, strings, and symbols map to the matching variant; lists
/// and hashes are preserved structurally (recursing into their contents) so
/// packages can store an array/object of config; any other value stringifies.
/// The real coercion to the option's declared type happens later in
/// `Settings::set`.
pub(crate) fn option_from_steelval(value: &SteelVal) -> crate::settings::OptionValue {
    use crate::settings::OptionValue;
    match value {
        SteelVal::BoolV(b) => OptionValue::Bool(*b),
        SteelVal::IntV(i) => OptionValue::Int(*i as i64),
        SteelVal::NumV(f) => OptionValue::Float(*f),
        SteelVal::StringV(s) => OptionValue::Str(s.to_string()),
        SteelVal::SymbolV(s) => OptionValue::Symbol(s.to_string()),
        // Structured values round-trip: a list stays a list, a hash stays a
        // hash. Elements/keys/values recurse, so nested config (an array of
        // account objects, say) survives set-option → get-option intact.
        SteelVal::ListV(l) => OptionValue::List(l.iter().map(option_from_steelval).collect()),
        SteelVal::HashMapV(h) => OptionValue::Map(
            h.iter()
                .map(|(k, v)| (option_from_steelval(k), option_from_steelval(v)))
                .collect(),
        ),
        other => match crate::authinfo::secret_ref_of(other) {
            Some(r) => OptionValue::Secret(r),
            None => OptionValue::Str(other.to_string()),
        },
    }
}

/// Convert a stored [`OptionValue`] back into a Steel value for `(get-option …)`.
pub(crate) fn option_to_steelval(v: &crate::settings::OptionValue) -> SteelVal {
    use crate::settings::OptionValue;
    match v {
        OptionValue::Bool(b) => SteelVal::BoolV(*b),
        OptionValue::Int(i) => SteelVal::IntV(*i as isize),
        OptionValue::Float(f) => SteelVal::NumV(*f),
        OptionValue::Str(s) => SteelVal::StringV(s.clone().into()),
        OptionValue::Symbol(s) => SteelVal::SymbolV(s.clone().into()),
        OptionValue::List(items) => SteelVal::ListV(items.iter().map(option_to_steelval).collect()),
        OptionValue::Map(pairs) => {
            let map: steel::HashMap<SteelVal, SteelVal> = pairs
                .iter()
                .map(|(k, v)| (option_to_steelval(k), option_to_steelval(v)))
                .collect();
            SteelVal::HashMapV(steel::gc::Gc::new(map).into())
        }
        // Nested references stay references; only a top-level `get-option`
        // resolves (see `get-option`).
        OptionValue::Secret(r) => {
            steel::rvals::IntoSteelVal::into_steelval(r.clone()).unwrap_or(SteelVal::BoolV(false))
        }
    }
}

/// A number from Scheme as `f64`: ints, floats, and exact rationals alike.
pub(crate) fn steel_f64(value: &SteelVal) -> Option<f64> {
    match value {
        SteelVal::IntV(n) => Some(*n as f64),
        SteelVal::NumV(f) => Some(*f),
        SteelVal::Rational(r) => Some(*r.numer() as f64 / *r.denom() as f64),
        _ => None,
    }
}

pub(crate) fn steel_list(value: &SteelVal) -> Option<Vec<SteelVal>> {
    match value {
        SteelVal::ListV(list) => Some(list.iter().cloned().collect()),
        _ => None,
    }
}

/// Rank `candidates` against `pattern` with nucleo (best first), dropping
/// non-matches. An empty pattern keeps the list as-is. The matcher matches Helm's
/// own (`helm_filter`), so a Scheme filter ranks identically to a native source.
pub(crate) fn fuzzy_rank(pattern: &str, candidates: &[String]) -> Vec<String> {
    if pattern.is_empty() {
        return candidates.to_vec();
    }
    let mut matcher = Matcher::new(Config::DEFAULT);
    let parsed = Pattern::parse(pattern, CaseMatching::Smart, Normalization::Smart);
    parsed
        .match_list(candidates.iter(), &mut matcher)
        .into_iter()
        .map(|(c, _score)| c.clone())
        .collect()
}

/// A list of strings (anything that isn't a string is dropped).
pub(crate) fn steel_string_list(value: &SteelVal) -> Vec<String> {
    steel_list(value)
        .map(|items| items.iter().filter_map(steel_string).collect())
        .unwrap_or_default()
}

/// A buffer position from Scheme. Position primitives take `isize`, not
/// `usize`: Steel converts a negative int to `usize` by wrapping (`-1` →
/// `usize::MAX`), which the editor would then clamp to the END of the buffer —
/// so `(delete-region -5 2)` deleted from 2 to the end. Negative clamps to 0
/// (the high side is clamped against the live buffer by the editor).
pub(crate) fn scheme_pos(n: isize) -> usize {
    n.max(0) as usize
}

/// A list of non-negative integers (anything else dropped). Used by
/// `(set-selections …)` to read its flat anchor/head offset list.
pub(crate) fn steel_usize_list(value: &SteelVal) -> Vec<usize> {
    steel_list(value)
        .map(|items| {
            items
                .iter()
                .filter_map(|v| match v {
                    SteelVal::IntV(i) if *i >= 0 => Some(*i as usize),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A list of completion candidates, each either a bare string or a
/// `(candidate annotation)` pair (the marginalia form). Non-conforming entries
/// are dropped.
pub(crate) fn steel_candidate_list(value: &SteelVal) -> Vec<(String, Option<String>)> {
    steel_list(value)
        .map(|items| items.iter().filter_map(steel_candidate).collect())
        .unwrap_or_default()
}

/// Recognize the `(raw "fn")` source form: a list whose head is the symbol
/// `raw` and whose second element is a Scheme fn name. Returns that name. Used to
/// tag a self-filtered dynamic Helm source; anything else parses as static
/// candidates. The head must be a *symbol* so a candidate list whose first entry
/// happens to be the string `"raw"` is not misread.
pub(crate) fn steel_raw_dynamic(value: &SteelVal) -> Option<String> {
    let items = steel_list(value)?;
    match items.first()? {
        SteelVal::SymbolV(s) if s.as_str() == "raw" => steel_string(items.get(1)?),
        _ => None,
    }
}

/// One candidate: a string `"cand"`, or a `(cand)` / `(cand annotation)` list.
pub(crate) fn steel_candidate(value: &SteelVal) -> Option<(String, Option<String>)> {
    if let Some(s) = steel_string(value) {
        return Some((s, None));
    }
    let pair = steel_list(value)?;
    let cand = steel_string(pair.first()?)?;
    let annotation = pair.get(1).and_then(steel_string);
    Some((cand, annotation))
}

/// Parse a CAPF function's return value into `(start, end, items)`. Accepted
/// shapes: a flat candidate list `((cand …))` (keeping the default range), or
/// `(start end candidate-list)` to also override the replaced byte range.
pub(crate) fn parse_capf_result(
    value: &SteelVal,
    default_start: usize,
    default_end: usize,
) -> (usize, usize, Vec<(String, Option<String>)>) {
    // `(start end candidate-list)` — integers in the first two slots.
    if let Some(items) = steel_list(value)
        && let (Some(SteelVal::IntV(s)), Some(SteelVal::IntV(e)), Some(cands)) =
            (items.first(), items.get(1), items.get(2))
    {
        return (
            (*s).max(0) as usize,
            (*e).max(0) as usize,
            steel_candidate_list(cands),
        );
    }
    // Otherwise treat the whole value as a flat candidate list.
    (default_start, default_end, steel_candidate_list(value))
}

/// Parse `(helm …)` / `(helm-from …)`'s sources argument: a list of specs.
pub(crate) fn parse_helm_sources(sources: &SteelVal) -> Vec<HelmSourceSpec> {
    steel_list(sources)
        .unwrap_or_default()
        .iter()
        .filter_map(parse_helm_source)
        .collect()
}

/// Parse one `(name candidates actions)` source spec.
///   * `candidates` — a list (static) or a string/symbol (a dynamic fn name).
///   * `actions` — a string/symbol (single action) or a list of `(label fn)`.
pub(crate) fn parse_helm_source(value: &SteelVal) -> Option<HelmSourceSpec> {
    let items = steel_list(value)?;
    let name = steel_string(items.first()?)?;

    let candidates = match items.get(1)? {
        // `(raw "fn")` — a self-filtered dynamic source (leading `raw` symbol);
        // otherwise a plain list is static candidates.
        list @ SteelVal::ListV(_) => match steel_raw_dynamic(list) {
            Some(function) => HelmCandidates::DynamicRaw(function),
            None => HelmCandidates::Static(steel_candidate_list(list)),
        },
        other => HelmCandidates::Dynamic(steel_string(other)?),
    };

    let actions = match items.get(2)? {
        SteelVal::ListV(_) => steel_list(items.get(2)?)?
            .iter()
            .filter_map(|pair| {
                let pair = steel_list(pair)?;
                Some((steel_string(pair.first()?)?, steel_string(pair.get(1)?)?))
            })
            .collect(),
        single => vec![("default".to_string(), steel_string(single)?)],
    };
    if actions.is_empty() {
        return None;
    }

    // Optional 4th element: a preview fn name.
    let preview = items.get(3).and_then(steel_string);

    Some(HelmSourceSpec {
        name,
        candidates,
        actions,
        preview,
    })
}

/// Format a Steel value for the echo area, treating void as "no value".
pub(crate) fn format_value(value: &SteelVal) -> Option<String> {
    match value {
        SteelVal::Void => None,
        other => Some(other.to_string()),
    }
}

/// A Steel string literal for `s` (quotes, backslashes, newlines and tabs
/// escaped), for splicing text into generated Scheme source.
pub(crate) fn steel_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// SteelErr's Display is "Error: <Kind>: <message>". Drop the redundant
/// "Error: " label and the internal "<Kind>: " category token (e.g.
/// `FreeIdentifier`, `ConversionError`) so callers can apply their own single
/// "error: " prefix without it stacking up into
/// "error: Error: FreeIdentifier: ...".
pub(crate) fn steel_error_string(err: &impl std::fmt::Display) -> String {
    let s = err.to_string();
    let s = s.strip_prefix("Error: ").unwrap_or(&s);
    // The kind is a single token with no spaces; only strip it when it matches
    // that shape so a real message containing a colon is never truncated.
    if let Some((kind, rest)) = s.split_once(": ")
        && !kind.is_empty()
        && kind.chars().all(|c| c.is_alphanumeric())
    {
        return rest.to_string();
    }
    s.to_string()
}

#[cfg(test)]
mod option_roundtrip_tests {
    use super::*;
    use crate::settings::OptionValue;

    // A list of hashes with symbol keys — the email-accounts shape — survives
    // option_from_steelval → option_to_steelval byte-for-byte.
    #[test]
    fn list_of_hashes_roundtrips() {
        let mut hm: steel::HashMap<SteelVal, SteelVal> = steel::HashMap::new();
        hm.insert(
            SteelVal::SymbolV("name".into()),
            SteelVal::StringV("kyle@x".into()),
        );
        hm.insert(
            SteelVal::SymbolV("method".into()),
            SteelVal::StringV("oauth".into()),
        );
        let original = SteelVal::ListV(
            [SteelVal::HashMapV(steel::gc::Gc::new(hm).into())]
                .into_iter()
                .collect(),
        );

        // → OptionValue (what set-option stores) → SteelVal (what get-option returns)
        let stored = option_from_steelval(&original);
        assert!(
            matches!(stored, OptionValue::List(_)),
            "not a list: {stored:?}"
        );
        let back = option_to_steelval(&stored);

        // The element is still a hash with the SYMBOL keys intact (not decayed
        // to strings) and the right values.
        let SteelVal::ListV(items) = back else {
            panic!("not a list")
        };
        let Some(SteelVal::HashMapV(h)) = items.iter().next() else {
            panic!("no hash")
        };
        let name = h.get(&SteelVal::SymbolV("name".into()));
        assert!(
            matches!(name, Some(SteelVal::StringV(s)) if s.as_str() == "kyle@x"),
            "name key lost: {name:?}"
        );
        let method = h.get(&SteelVal::SymbolV("method".into()));
        assert!(matches!(method, Some(SteelVal::StringV(s)) if s.as_str() == "oauth"));
    }

    #[test]
    fn scalars_unchanged() {
        // Existing scalar options behave exactly as before.
        assert!(matches!(
            option_from_steelval(&SteelVal::BoolV(true)),
            OptionValue::Bool(true)
        ));
        assert!(matches!(
            option_from_steelval(&SteelVal::StringV("x".into())),
            OptionValue::Str(s) if s == "x"
        ));
        // A bare symbol now round-trips as a symbol (was: coerced to string).
        let sym = option_from_steelval(&SteelVal::SymbolV("foo".into()));
        assert!(matches!(option_to_steelval(&sym), SteelVal::SymbolV(s) if s.as_str() == "foo"));
    }
}
