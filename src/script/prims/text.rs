//! Pure string utilities: line and word diffs, regexp quoting and fuzzy
//! ranking.

use super::*;

pub(in crate::script) fn register(reg: &mut Registrar, _ctx: &EngineShared) {
    reg.section(Section::Text);
    {
        // `(diff-strings old new)` → line-level diff ops, one per line as
        // "tag\told_start\told_end\tnew_start\tnew_end" with 0-based,
        // end-exclusive LINE indices. tag ∈ equal|delete|insert|replace.
        // Pure function (no editor state); powers scheme/ediff.scm — Scheme
        // merges adjacent non-equal ops into navigable hunks.
        reg.register_fn(
            "diff-strings",
            move |old: String, new: String| -> Vec<String> {
                let diff = similar::TextDiff::from_lines(&old, &new);
                diff.ops()
                    .iter()
                    .map(|op| {
                        let (tag, old_range, new_range) = op.as_tag_tuple();
                        let tag = match tag {
                            similar::DiffTag::Equal => "equal",
                            similar::DiffTag::Delete => "delete",
                            similar::DiffTag::Insert => "insert",
                            similar::DiffTag::Replace => "replace",
                        };
                        format!(
                            "{tag}\t{}\t{}\t{}\t{}",
                            old_range.start, old_range.end, new_range.start, new_range.end
                        )
                    })
                    .collect()
            },
        );
    }
    {
        // `(diff-inline old new)` → word-level refinement between two single
        // lines, one change per line as "tag\ttext" (tag ∈ equal|delete|insert).
        // Used to highlight intra-line changes within an ediff hunk.
        reg.register_fn(
            "diff-inline",
            move |old: String, new: String| -> Vec<String> {
                let diff = similar::TextDiff::from_words(&old, &new);
                diff.iter_all_changes()
                    .map(|c| {
                        let tag = match c.tag() {
                            similar::ChangeTag::Equal => "equal",
                            similar::ChangeTag::Delete => "delete",
                            similar::ChangeTag::Insert => "insert",
                        };
                        format!("{tag}\t{}", c.value().replace('\n', " "))
                    })
                    .collect()
            },
        );
    }
    // Escape a string for literal use in a regexp (Emacs `regexp-quote`).
    reg.register_fn("regexp-quote", |s: String| -> String { regex::escape(&s) });
    // `(fuzzy-match pattern candidates)` — rank `candidates` against `pattern`
    // with the same nucleo matcher Helm uses, best-match first; non-matches are
    // dropped. An empty pattern returns the list unchanged. Lets a Scheme-driven
    // Helm filter do compiled-speed scoring instead of reimplementing it.
    reg.register_fn(
        "fuzzy-match",
        |pattern: String, StringList(candidates): StringList| -> Vec<String> {
            fuzzy_rank(&pattern, &candidates)
        },
    );
}
