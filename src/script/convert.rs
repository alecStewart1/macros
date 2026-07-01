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

pub(crate) fn steel_string(value: &SteelVal) -> Option<String> {
    match value {
        SteelVal::StringV(s) => Some(s.to_string()),
        SteelVal::SymbolV(s) => Some(s.to_string()),
        _ => None,
    }
}

/// Convert a value passed to `(set-option …)` into an untyped [`OptionValue`].
/// Numbers, booleans, and strings map to the matching variant; anything else is
/// stringified. The real coercion to the option's declared type happens later in
/// `Settings::set`.
pub(crate) fn option_from_steelval(value: &SteelVal) -> crate::settings::OptionValue {
    use crate::settings::OptionValue;
    match value {
        SteelVal::BoolV(b) => OptionValue::Bool(*b),
        SteelVal::IntV(i) => OptionValue::Int(*i as i64),
        SteelVal::NumV(f) => OptionValue::Float(*f),
        SteelVal::StringV(s) => OptionValue::Str(s.to_string()),
        SteelVal::SymbolV(s) => OptionValue::Str(s.to_string()),
        other => OptionValue::Str(other.to_string()),
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
