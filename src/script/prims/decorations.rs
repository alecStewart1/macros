//! Per-line decorations (icons, gutter dots, images, virtual indent, scale,
//! ghost text, avy hints) and the staleness guards that keep a repaint from
//! landing on edited text.

use super::*;

pub(in crate::script) fn register(reg: &mut Registrar, ctx: &EngineShared) {
    let EngineShared {
        queue, snapshot, ..
    } = ctx;
    reg.section(Section::Decorations);
    {
        // `(set-line-icon line col "folder")` attaches a bundled icon (asset
        // `icons/<name>.svg`) at character column `col` of `line` in the active
        // buffer; `(clear-line-icons)` removes them all. Used by the file tree.
        register_ternary(reg, queue, "set-line-icon", EditorCommand::SetLineIcon);
        register_nullary(
            reg,
            queue,
            "clear-line-icons",
            EditorCommand::ClearLineIcons,
        );
    }
    {
        // `(set-gutter-dot line face)` puts a colored dot (the diagnostic-dot
        // shape) in the gutter of 0-based `line` of the active buffer, tinted
        // by theme face `face`; `(clear-gutter-dots)` removes them all.
        // Breakpoint signs (dap.scm).
        register_binary(reg, queue, "set-gutter-dot", EditorCommand::SetGutterDot);
        register_nullary(
            reg,
            queue,
            "clear-gutter-dots",
            EditorCommand::ClearGutterDots,
        );
    }
    {
        // `(set-line-image line "/abs/path.png" [max-width])` displays a decoded
        // raster image on rows reserved below `line` of the active buffer;
        // `(clear-line-images)` removes them all. `max-width` (px) caps the
        // displayed width — 0 uses the default cap. The optional arg is provided
        // by the `set-line-image` Scheme wrapper (core.scm); this is the impl.
        // Drives Org's inline-image / LaTeX-preview display and the roam graph.
        let q = queue.clone();
        reg.register_fn(
            "set-line-image-impl",
            move |line: usize, path: String, max_width: usize| {
                q.push(EditorCommand::SetLineImage {
                    line,
                    path,
                    max_width,
                });
            },
        );
        register_nullary(
            reg,
            queue,
            "clear-line-images",
            EditorCommand::ClearLineImages,
        );
        // `(guard-buffer-unchanged on-stale)` — captures the snapshot's buffer
        // name and edit tick NOW, before the caller reads any text; when the
        // batch is applied, everything after this command is dropped if the
        // target buffer was edited (or switched) in the meantime. A debounced
        // repaint racing live typing would otherwise paint decorations at stale
        // byte offsets. `on-stale` (a global fn name, or "") is dispatched on a
        // drop so the caller can reschedule the repaint it just lost.
        {
            let q = queue.clone();
            let shared = snapshot.clone();
            reg.register_fn("guard-buffer-unchanged", move |on_stale: String| {
                let s = shared.load();
                q.push(EditorCommand::GuardBufferUnchanged {
                    name: s.name.clone(),
                    tick: s.tick,
                    on_stale,
                });
            });
        }
        // `(buffer-tick)` → the published buffer's edit counter, and
        // `(guard-buffer-tick name tick on-stale)` — the explicit-identity form
        // of `guard-buffer-unchanged`, for repaints whose byte offsets were
        // computed in an EARLIER eval than the one that paints them: an async
        // backend reply (spell.scm's aspell / NSSpellChecker callbacks) must
        // guard with the name+tick captured when the text was sent for
        // checking, not with whatever the snapshot says when the reply lands.
        register_snapshot_read(reg, snapshot, "buffer-tick", |s| s.tick as usize);
        {
            let q = queue.clone();
            reg.register_fn(
                "guard-buffer-tick",
                move |name: String, tick: usize, on_stale: String| {
                    q.push(EditorCommand::GuardBufferUnchanged {
                        name,
                        tick: tick as u64,
                        on_stale,
                    });
                },
            );
        }
        // `(set-line-indent line cols)` gives buffer `line` a virtual left indent
        // of `cols` columns (display-only); `(clear-line-indents)` removes them
        // all. Drives Org's org-indent-mode look.
        register_binary(reg, queue, "set-line-indent", |line, cols| {
            EditorCommand::SetLineIndent { line, cols }
        });
        register_nullary(
            reg,
            queue,
            "clear-line-indents",
            EditorCommand::ClearLineIndents,
        );
        // `(set-line-scale line scale)` renders buffer `line` at `scale`× the
        // base font size (e.g. a heading); `(clear-line-scales)` resets all.
        let q = queue.clone();
        reg.register_fn("set-line-scale", move |line: usize, scale: f64| {
            q.push(EditorCommand::SetLineScale {
                line,
                scale: scale as f32,
            });
        });
        register_nullary(
            reg,
            queue,
            "clear-line-scales",
            EditorCommand::ClearLineScales,
        );
        // `(ghost-text-show text)` renders `text` as a dim inline suggestion at
        // point (copilot-style "ghost text"): display-only, occupying no buffer
        // bytes; a multi-line suggestion pushes the following display lines
        // down. It is deliberately ephemeral — any edit or cursor motion (or
        // `(ghost-text-clear)` / C-g) dismisses it, so a completion package
        // re-shows after each debounced `after-change-functions` fire.
        // `(ghost-text-accept)` inserts the visible suggestion as one edit;
        // scheme/ghost.scm binds it to TAB via the `ghost` keymap.
        register_unary(reg, queue, "ghost-text-show", |text| {
            EditorCommand::GhostShow { text }
        });
        register_nullary(reg, queue, "ghost-text-clear", EditorCommand::GhostClear);
        register_nullary(reg, queue, "ghost-text-accept", EditorCommand::GhostAccept);
    }
    register_unary(reg, queue, "avy-show-hints", EditorCommand::AvyShowHints);
    register_nullary(reg, queue, "avy-clear-hints", EditorCommand::AvyClearHints);
    register_unary(
        reg,
        queue,
        "fontify-org-buffer",
        EditorCommand::FontifyOrgBuffer,
    );
}
