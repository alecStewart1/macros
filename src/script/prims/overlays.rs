//! Overlays, text properties, folds and line suffixes.

use steel::rvals::SteelVal;

use super::*;

pub(in crate::script) fn register(reg: &mut Registrar, ctx: &EngineShared) {
    let EngineShared {
        queue, snapshot, ..
    } = ctx;
    reg.section(Section::Overlays);
    // --- Overlays / text properties / folds -------------------------------
    register_ternary(reg, queue, "overlay-set-face", |start, end, face| {
        EditorCommand::OverlayFace { start, end, face }
    });
    {
        // `(overlay-set-faces triples)` — batch overlay-set-face: applies a
        // whole list of (start end face) accents in ONE Steel→Rust call. A big
        // magit render carries thousands of accents; the per-call dispatch of
        // the unary form dominated it. Malformed entries are skipped.
        let q = queue.clone();
        reg.register_fn("overlay-set-faces", move |triples: SteelVal| {
            let faces = steel_list(&triples).unwrap_or_default();
            q.extend(faces.iter().filter_map(|t| {
                let items = steel_list(t)?;
                let usize_at = |i: usize| match items.get(i) {
                    Some(SteelVal::IntV(n)) if *n >= 0 => Some(*n as usize),
                    _ => None,
                };
                Some(EditorCommand::OverlayFace {
                    start: usize_at(0)?,
                    end: usize_at(1)?,
                    face: items.get(2).and_then(steel_string)?,
                })
            }));
        });
    }
    register_binary(reg, queue, "overlay-hide", |start, end| {
        EditorCommand::OverlayHide { start, end }
    });
    register_binary(reg, queue, "fold-lines", |start_line, end_line| {
        EditorCommand::FoldLines {
            start_line,
            end_line,
        }
    });
    register_nullary(reg, queue, "unfold-all", EditorCommand::UnfoldAll);
    // `(set-fold-marker line open?)` paints a gutter chevron on `line`; the click
    // is hit-tested back to `(fold-toggle-at-line line)`. `(clear-fold-markers)`
    // removes them all (modes re-declare markers on refresh).
    register_binary(reg, queue, "set-fold-marker", |line, open| {
        EditorCommand::SetFoldMarker { line, open }
    });
    register_nullary(
        reg,
        queue,
        "clear-fold-markers",
        EditorCommand::ClearFoldMarkers,
    );
    register_nullary(reg, queue, "clear-overlays", EditorCommand::ClearOverlays);
    // `(clear-overlay-faces-matching face)` — drop only the overlays painted
    // with `face`, leaving other features' overlays (org, rainbow, LSP) alone.
    register_unary(
        reg,
        queue,
        "clear-overlay-faces-matching",
        EditorCommand::ClearOverlayFacesMatching,
    );
    {
        let q = queue.clone();
        reg.register_fn(
            "put-text-property",
            move |NonNeg(start): NonNeg, NonNeg(end): NonNeg, key: String, value: String| {
                q.push(EditorCommand::PutTextProperty {
                    start,
                    end,
                    key,
                    value,
                });
            },
        );
    }
    register_ternary(reg, queue, "set-line-suffix", |line, text, face| {
        EditorCommand::SetLineSuffix { line, text, face }
    });
    {
        // Value of text property `key` at byte `pos`, or "" — read over the
        // published snapshot (Scheme writes via put-text-property, reads next cmd).
        let shared = snapshot.clone();
        reg.register_fn(
            "get-text-property",
            move |NonNeg(pos): NonNeg, key: String| -> String {
                let snap = shared.load();
                snap.props
                    .iter()
                    .find(|(s, e, k, _)| *k == key && pos >= *s && pos < *e)
                    .map(|(_, _, _, v)| v.clone())
                    .unwrap_or_default()
            },
        );
    }
}
