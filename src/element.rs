use std::ops::Range;
use std::sync::Arc;

use gpui::{
    App, Bounds, ContentMask, Corners, CursorStyle, Element, ElementId, ElementInputHandler,
    Entity, Font, FontStyle, FontWeight, GlobalElementId, Hitbox, HitboxBehavior, Hsla,
    InspectorElementId, LayoutId, PaintQuad, Pixels, Point, RenderImage, ShapedLine, SharedString,
    Style, TextAlign, TextRun, TransformationMatrix, UnderlineStyle, Window, WrappedLine, fill,
    font, point, prelude::*, px, relative, rgb, rgba, size,
};

use crate::editor::{CompletionView, Editor};
use crate::highlight::{HlSpan, face_name};
use crate::lsp::Severity;
use crate::theme::Theme;

/// Horizontal padding on each side of the line-number gutter.
const GUTTER_PAD: Pixels = px(8.);
/// Width of the overlay scroll-bar track/thumb on the pane's right edge.
const SCROLLBAR_WIDTH: Pixels = px(6.);
/// Minimum height of the scroll-bar thumb so it stays grabbable on long buffers.
const SCROLLBAR_MIN_THUMB: f32 = 24.0;
/// Alpha multiplier applied to a block cursor so the glyph under it stays legible.
const BLOCK_CURSOR_ALPHA: f32 = 0.5;
/// File-tree icon size as a fraction of the line height.
const ICON_SCALE: f32 = 0.82;
/// Inner padding of the completion popup (and its doc side-panel).
const POPUP_PAD: Pixels = px(8.);
/// Column (in characters) at which the completion doc side-panel word-wraps.
const DOC_WRAP_COLS: usize = 64;
/// Maximum rows shown in the completion doc side-panel.
const DOC_MAX_ROWS: usize = 16;
/// Diameter of a diagnostic gutter dot.
const DIAG_DOT: f32 = 7.0;

/// A plain (unstyled) text run: a single font/color with no background,
/// underline, or strikethrough. The common case for shaped UI/text lines.
fn plain_run(len: usize, font: Font, color: Hsla) -> TextRun {
    TextRun {
        len,
        font,
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    }
}

/// The run for ghost (inline-suggestion) text: dim + italic. One constructor
/// so the spliced first line and the painted continuation lines can't drift
/// in style.
fn ghost_run(len: usize, font: &Font, color: Hsla) -> TextRun {
    let mut run = plain_run(len, font.clone(), color);
    run.font.style = FontStyle::Italic;
    run
}

/// Diagnostic colors, resolved from the theme once per prepaint. The faces are
/// `diagnostic-error` / `diagnostic-warning` / `diagnostic-info` /
/// `diagnostic-hint` (set by every bundled theme), with the previous fixed
/// hexes as fallbacks for themes that don't define them.
#[derive(Clone, Copy)]
struct DiagnosticColors {
    error: Hsla,
    warning: Hsla,
    info: Hsla,
    hint: Hsla,
}

impl DiagnosticColors {
    fn resolve(theme: &Theme) -> Self {
        let info = theme.color("diagnostic-info", rgb(0x3794ff).into());
        Self {
            error: theme.color("diagnostic-error", rgb(0xf14c4c).into()),
            warning: theme.color("diagnostic-warning", rgb(0xcca700).into()),
            info,
            hint: theme.color("diagnostic-hint", info),
        }
    }

    fn for_severity(&self, severity: Severity) -> Hsla {
        match severity {
            Severity::Error => self.error,
            Severity::Warning => self.warning,
            Severity::Info => self.info,
            Severity::Hint => self.hint,
        }
    }
}

/// A shaped, positioned line, cached after paint so the editor can do
/// hit-testing (mouse), IME bounds queries, and vertical cursor motion without
/// re-shaping. Only the currently visible lines are cached.
pub struct LineLayout {
    /// The shaped, soft-wrapped line. When word-wrap is off it has no wrap
    /// boundaries (a single visual row); when on it may span several rows.
    /// Shaped over the *display* text — `overlay-hide` byte ranges (`holes`) are
    /// spliced out before shaping, so indices into `wrapped` are display indices.
    pub wrapped: WrappedLine,
    /// Y offset of this line relative to the element's origin (already adjusted
    /// for the scroll position, so it may be negative for the top line).
    pub top: Pixels,
    /// Byte range of the line in the buffer (line break excluded).
    pub range: Range<usize>,
    /// Hidden byte ranges within this line, relative to `range.start`, sorted and
    /// non-overlapping (from `overlay-hide`). Empty for the common case, where the
    /// display text equals the buffer text and the maps below are the identity.
    pub holes: Vec<(usize, usize)>,
    /// Virtual left indent (px) the line text is shifted by (`set-line-indent`,
    /// the Org indent look). Subtracted from a click's x before hit-testing.
    pub indent: Pixels,
    /// Painted height of this line (rows it occupies × line height). Equals the
    /// shaped height for ordinary lines, but for a scaled heading it's the full
    /// reserved block — so click hit-testing matches what was painted.
    pub height: Pixels,
    /// Vertical offset from `top` at which the glyphs are painted, to center a
    /// scaled heading in its taller reserved block. Zero for ordinary lines.
    pub content_dy: Pixels,
    /// Ghost text spliced *into* the display text (the inverse of `holes`):
    /// `(display index it was inserted at, its byte length)`. The insertion
    /// point is where point sits, so the cursor renders before the ghost.
    pub ghost: Option<(usize, usize)>,
}

impl LineLayout {
    /// Buffer offset (relative to the line start) → index into the display text.
    pub fn to_display(&self, rel: usize) -> usize {
        ghost_to_display(to_display(rel, &self.holes), self.ghost)
    }
    /// Index into the display text → buffer offset (relative to the line start).
    // Named to pair with `to_display`, not a `From`-style constructor.
    #[allow(clippy::wrong_self_convention)]
    pub fn from_display(&self, disp: usize) -> usize {
        from_display(ghost_from_display(disp, self.ghost), &self.holes)
    }
}

/// Shift a display index past ghost text inserted at `(at, len)`. An index at
/// exactly `at` does NOT shift — that's point itself, which renders before the
/// ghost; everything after it moves right by the ghost's length.
fn ghost_to_display(disp: usize, ghost: Option<(usize, usize)>) -> usize {
    match ghost {
        Some((at, len)) if disp > at => disp + len,
        _ => disp,
    }
}

/// Inverse of [`ghost_to_display`]: an index inside the ghost text collapses to
/// its insertion point (a click on the suggestion lands at point).
fn ghost_from_display(disp: usize, ghost: Option<(usize, usize)>) -> usize {
    match ghost {
        Some((at, len)) if disp > at => disp.saturating_sub(len).max(at),
        _ => disp,
    }
}

/// Map a line-relative buffer offset to its index in the display text, where the
/// byte ranges in `holes` have been removed. An offset inside a hole clamps to
/// that hole's start. `holes` must be sorted and non-overlapping.
fn to_display(rel: usize, holes: &[(usize, usize)]) -> usize {
    let mut removed = 0;
    for &(hs, he) in holes {
        if rel <= hs {
            break;
        }
        if rel < he {
            return hs - removed;
        }
        removed += he - hs;
    }
    rel - removed
}

/// Inverse of [`to_display`]: a display-text index back to a line-relative buffer
/// offset (landing just past any hole it sits at).
fn from_display(disp: usize, holes: &[(usize, usize)]) -> usize {
    let mut removed = 0;
    for &(hs, he) in holes {
        if disp < hs - removed {
            break;
        }
        removed += he - hs;
    }
    disp + removed
}

/// The line text with `holes` (line-relative byte ranges) spliced out.
fn remove_holes(text: &str, holes: &[(usize, usize)]) -> String {
    if holes.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    for &(hs, he) in holes {
        if hs > pos {
            out.push_str(&text[pos..hs]);
        }
        pos = he.max(pos);
    }
    if pos < text.len() {
        out.push_str(&text[pos..]);
    }
    out
}

/// Shorten the per-line `TextRun`s so they cover the display text instead of the
/// full buffer line: each run loses the bytes it shares with a hole, and a run
/// fully inside a hole is dropped. Runs are in buffer-line order.
fn punch_runs(runs: Vec<TextRun>, holes: &[(usize, usize)]) -> Vec<TextRun> {
    if holes.is_empty() {
        return runs;
    }
    let mut out = Vec::with_capacity(runs.len());
    let mut pos = 0;
    for mut r in runs {
        let (start, end) = (pos, pos + r.len);
        pos = end;
        let mut overlap = 0;
        for &(hs, he) in holes {
            let (os, oe) = (hs.max(start), he.min(end));
            if oe > os {
                overlap += oe - os;
            }
        }
        if r.len > overlap {
            r.len -= overlap;
            out.push(r);
        }
    }
    out
}

/// Splice a run for ghost text into the display-text `runs` at display index
/// `at` (the inverse of `punch_runs`): the run covering `at` is split, and the
/// ghost's own run inserted between the halves. Runs must already cover the
/// ghost-less display text (holes punched).
fn inject_ghost_run(runs: Vec<TextRun>, at: usize, ghost_run: TextRun) -> Vec<TextRun> {
    let mut out = Vec::with_capacity(runs.len() + 2);
    let mut pos = 0;
    let mut inserted = false;
    for mut r in runs {
        let start = pos;
        pos += r.len;
        if !inserted && at <= start {
            out.push(ghost_run.clone());
            inserted = true;
        }
        if !inserted && at < pos {
            let mut head = r.clone();
            head.len = at - start;
            r.len -= head.len;
            out.push(head);
            out.push(ghost_run.clone());
            inserted = true;
        }
        out.push(r);
    }
    if !inserted {
        out.push(ghost_run); // at end of line (or an empty line)
    }
    out
}

/// The custom GPUI element that draws the buffer. GPUI ships no multi-line text
/// widget, so we shape and paint each visible line ourselves, register the
/// editor as the platform input handler, and clip to the viewport.
pub struct EditorElement {
    pub editor: Entity<Editor>,
    /// Which window pane this element draws.
    pub pane: usize,
}

pub struct PrepaintState {
    lines: Vec<LineLayout>,
    cursor: Option<PaintQuad>,
    /// Full-width background band behind the cursor's line (Emacs `hl-line-mode`).
    hl_line: Option<PaintQuad>,
    selections: Vec<PaintQuad>,
    /// Background highlight quads for the active search query's matches.
    search: Vec<PaintQuad>,
    /// Background highlight quads for the bracket at point and its match
    /// (Emacs `show-paren-mode`).
    paren: Vec<PaintQuad>,
    /// Scroll-bar track + thumb quads (Emacs scroll-bar-mode), right edge.
    scrollbar: Vec<PaintQuad>,
    /// Per-line SVG icons (file tree): (bounds, asset path, tint color).
    icons: Vec<(Bounds<Pixels>, SharedString, Hsla)>,
    /// Inline raster images (Org / LaTeX preview): (bounds, decoded bitmap).
    images: Vec<(Bounds<Pixels>, Arc<RenderImage>)>,
    /// Squiggle/underline quads for LSP diagnostics.
    diagnostics: Vec<PaintQuad>,
    /// git-gutter (diff-hl) change markers, painted at the gutter's far-left
    /// edge: full-height bars for added/modified lines, a short top-edge bar
    /// where lines were deleted.
    git_gutter: Vec<PaintQuad>,
    /// The completion popup overlay, if open.
    popup: Option<PopupLayout>,
    /// Left edge of the text area (past the line-number gutter, if any).
    text_left: Pixels,
    /// Where line text is painted: `text_left` shifted by the horizontal scroll
    /// (equal to `text_left` when word-wrap is on, since h-scroll is then 0).
    line_x: Pixels,
    /// One monospace character's advance, reported to the editor for h-scroll.
    char_width: Pixels,
    /// Shaped line-number labels + where to paint them (in the gutter).
    gutter: Vec<(ShapedLine, Point<Pixels>)>,
    /// Fold chevrons painted in the gutter: (bounds, asset path, tint color).
    fold_chevrons: Vec<(Bounds<Pixels>, SharedString, Hsla)>,
    /// Clickable region + line for each fold chevron, reported to the editor so a
    /// gutter click toggles the right fold.
    fold_hitboxes: Vec<crate::editor::FoldHitbox>,
    /// gpui hitboxes over the chevrons, so the pointer becomes a hand on hover
    /// (set during paint via `set_cursor_style`).
    fold_cursors: Vec<Hitbox>,
    /// Shaped virtual suffix text (inlay/blame) + where to paint it.
    virtual_text: Vec<(ShapedLine, Point<Pixels>)>,
    /// avy jump hints: a background quad + shaped label, painted on top of the
    /// text at a byte position (e.g. "a", "sd"). One per visible candidate.
    avy: Vec<(PaintQuad, ShapedLine, Point<Pixels>)>,
    /// Secondary cursor carets (multiple-cursors), painted like the primary one.
    extra_cursors: Vec<PaintQuad>,
    /// The primary cursor's top, relative to the viewport top (may be negative or
    /// past the bottom under wrapping). Fed back to the editor so it can follow the
    /// cursor against the real painted geometry. `None` when the cursor isn't laid
    /// out (e.g. scrolled fully off, or this isn't the selected pane).
    cursor_rel_y: Option<Pixels>,
}

/// A laid-out completion popup: background/border/selection quads plus the
/// shaped candidate lines and where to paint each.
struct PopupLayout {
    quads: Vec<PaintQuad>,
    labels: Vec<(ShapedLine, Point<Pixels>)>,
    hit: crate::editor::PopupHit,
}

/// Per-line geometry shared by the prepaint helpers: which buffer line it is,
/// where it sits, its byte range, and the display-text mapping (`holes`)
/// needed to position anything within it.
struct LineCtx<'a> {
    /// Buffer line index.
    i: usize,
    /// Byte range of the line in the buffer (line break excluded).
    range: Range<usize>,
    /// The shaped, soft-wrapped display text.
    wrapped: &'a WrappedLine,
    /// Hidden byte ranges spliced out of the display text (line-relative).
    holes: &'a [(usize, usize)],
    /// Y offset of the line's top relative to the element origin.
    top: Pixels,
    /// Painted height of the line (rows it occupies × line height).
    line_h: Pixels,
    /// Window-space top-left of the glyphs (indent, scroll, and any scaled-
    /// heading centering already applied); `.y` is the content top.
    line_origin: Point<Pixels>,
    /// Uniform row height used for positioning within the line.
    line_height: Pixels,
    /// Ghost text spliced into the display text at `(display index, byte len)`
    /// — see [`LineLayout::ghost`].
    ghost: Option<(usize, usize)>,
}

impl LineCtx<'_> {
    /// Display index of an absolute buffer offset at or after the line start.
    fn disp(&self, offset: usize) -> usize {
        ghost_to_display(
            to_display(offset.saturating_sub(self.range.start), self.holes),
            self.ghost,
        )
    }

    /// Emit background quads for the absolute byte range `[s, e)` where it
    /// overlaps this line — the shared body of the selection / search / paren
    /// / diagnostic highlights.
    fn push_range(
        &self,
        out: &mut Vec<PaintQuad>,
        s: usize,
        e: usize,
        color: Hsla,
        min_width: Pixels,
    ) {
        if s > self.range.end || e < self.range.start {
            return;
        }
        push_range_quads(
            out,
            self.wrapped,
            self.disp(s.max(self.range.start)),
            self.disp(e.min(self.range.end)),
            self.line_origin,
            self.line_height,
            color,
            min_width,
        );
    }
}

/// The per-frame accumulators the per-line helpers append into, drained into
/// [`PrepaintState`] once the line loop finishes.
#[derive(Default)]
struct Accum {
    selections: Vec<PaintQuad>,
    search: Vec<PaintQuad>,
    paren: Vec<PaintQuad>,
    diagnostics: Vec<PaintQuad>,
    git_gutter: Vec<PaintQuad>,
    icons: Vec<(Bounds<Pixels>, SharedString, Hsla)>,
    avy: Vec<(PaintQuad, ShapedLine, Point<Pixels>)>,
    extra_cursors: Vec<PaintQuad>,
    gutter: Vec<(ShapedLine, Point<Pixels>)>,
    fold_chevrons: Vec<(Bounds<Pixels>, SharedString, Hsla)>,
    fold_hitboxes: Vec<crate::editor::FoldHitbox>,
    fold_cursors: Vec<Hitbox>,
    virtual_text: Vec<(ShapedLine, Point<Pixels>)>,
}

/// Gutter configuration resolved once per frame: geometry, colors, and which
/// gutter features are enabled.
struct GutterCfg {
    bounds: Bounds<Pixels>,
    gutter_width: Pixels,
    gutter_pad: Pixels,
    /// Width of the fold-chevron column (0 when fold markers are off).
    fold_col: Pixels,
    chevron_sz: Pixels,
    fold_markers: bool,
    diag_gutter: bool,
    git_gutter: bool,
    git_gutter_w: Pixels,
    git_gutter_del: bool,
    gg_added: Hsla,
    gg_modified: Hsla,
    gg_removed: Hsla,
    number_color: Hsla,
    font: Font,
    font_size: Pixels,
    diag_colors: DiagnosticColors,
}

/// Cursor styling resolved once per frame, shared by the primary and
/// secondary (multiple-cursors) carets.
struct CursorCfg {
    block: bool,
    width: Pixels,
    shown: bool,
    /// One monospace advance — block-cursor width past EOL.
    char_advance: Pixels,
    color: Hsla,
    secondary_color: Hsla,
}

/// Lay out one line's gutter: the line-number label, its fold chevron, the
/// diagnostic dot, and the git-gutter change bar. Only called when the
/// line-number gutter is shown (the other gutter features are gated on it).
#[allow(clippy::too_many_arguments)]
fn layout_gutter(
    line: &LineCtx,
    cfg: &GutterCfg,
    editor: &Editor,
    theme: &Theme,
    diagnostics: &[crate::lsp::Diagnostic],
    diff_marks: &std::collections::HashMap<usize, crate::editor::DiffMark>,
    gutter_dots: &std::collections::HashMap<usize, SharedString>,
    window: &mut Window,
    acc: &mut Accum,
) {
    let i = line.i;
    let bounds = cfg.bounds;

    // Line-number label, right-aligned in the gutter. Shares the line's
    // content offset, so on a scaled line it centers to the same midpoint.
    let label = SharedString::from((i + 1).to_string());
    let n = label.len();
    let shaped_num = window.text_system().shape_line(
        label,
        cfg.font_size,
        &[plain_run(n, cfg.font.clone(), cfg.number_color)],
        None,
    );
    let nx = bounds.left() + cfg.gutter_width - cfg.gutter_pad - shaped_num.x_for_index(n);
    acc.gutter.push((shaped_num, point(nx, line.line_origin.y)));

    // Fold chevron, centered in its reserved gutter column for any line Scheme
    // marked foldable: down = open (expanded), right = collapsed. The whole
    // column is the clickable/hover region, which is reported back so a click
    // toggles the fold.
    let marker = cfg.fold_markers.then(|| editor.fold_marker_at(i)).flatten();
    if let Some(open) = marker {
        let glyph = Bounds::new(
            point(
                bounds.left() + (cfg.fold_col - cfg.chevron_sz) / 2.0,
                bounds.top() + line.top + (line.line_h - cfg.chevron_sz) / 2.0,
            ),
            size(cfg.chevron_sz, cfg.chevron_sz),
        );
        let name = if open {
            "chevron-down"
        } else {
            "chevron-right"
        };
        acc.fold_chevrons.push((
            glyph,
            SharedString::from(format!("icons/{name}.svg")),
            cfg.number_color,
        ));
        let hit = Bounds::new(
            point(bounds.left(), bounds.top() + line.top),
            size(cfg.fold_col, line.line_h),
        );
        acc.fold_hitboxes.push(crate::editor::FoldHitbox {
            bounds: hit,
            line: i,
        });
        acc.fold_cursors
            .push(window.insert_hitbox(hit, HitboxBehavior::Normal));
    }

    // Breakpoint gutter dot (`set-gutter-dot`, dap.scm): same shape and spot
    // as the diagnostic dot, and it wins when both are present — the user put
    // it there, and a diagnostic still shows via its text-area underline.
    if let Some(face) = gutter_dots.get(&i) {
        let color = theme.color(face, cfg.gg_removed);
        let dot = px(DIAG_DOT);
        let dx = bounds.left() + px(3.);
        let dy = bounds.top() + line.top + px((f32::from(line.line_h) - DIAG_DOT) / 2.0);
        let mut quad = fill(Bounds::new(point(dx, dy), size(dot, dot)), color);
        quad.corner_radii = Corners::all(dot / 2.0);
        acc.diagnostics.push(quad);
    }
    // Diagnostic gutter dot: a colored circle on the far-left of the gutter
    // for any line carrying a diagnostic, colored by the worst severity
    // present (error > warning > info/hint).
    else if cfg.diag_gutter && marker.is_none() {
        let worst = diagnostics
            .iter()
            .filter(|d| d.range.start <= line.range.end && d.range.end >= line.range.start)
            .map(|d| d.severity)
            .min();
        if let Some(severity) = worst {
            let color = cfg.diag_colors.for_severity(severity);
            let dot = px(DIAG_DOT);
            let dx = bounds.left() + px(3.);
            let dy = bounds.top() + line.top + px((f32::from(line.line_h) - DIAG_DOT) / 2.0);
            let mut quad = fill(Bounds::new(point(dx, dy), size(dot, dot)), color);
            quad.corner_radii = Corners::all(dot / 2.0);
            acc.diagnostics.push(quad);
        }
    }

    // git-gutter (diff-hl): a full-height change bar at the gutter's left edge
    // for added/modified lines; a short top-edge bar for a deletion that
    // happened just above this line. Sits left of the diagnostic dot column,
    // so the two can coexist on a line.
    if cfg.git_gutter {
        use crate::editor::DiffMark;
        let gx = bounds.left() + px(1.);
        let gy = bounds.top() + line.top;
        match diff_marks.get(&i) {
            Some(DiffMark::Removed) if cfg.git_gutter_del => {
                let w = (cfg.git_gutter_w * 2.0).min(px(8.));
                let bar = Bounds::new(point(gx, gy), size(w, px(2.)));
                acc.git_gutter.push(fill(bar, cfg.gg_removed));
            }
            Some(mark @ (DiffMark::Added | DiffMark::Modified)) => {
                let color = if *mark == DiffMark::Added {
                    cfg.gg_added
                } else {
                    cfg.gg_modified
                };
                let bar = Bounds::new(point(gx, gy), size(cfg.git_gutter_w, line.line_h));
                acc.git_gutter.push(fill(bar, color));
            }
            // A deletion mark with the deleted-wedge option off, or no mark.
            Some(DiffMark::Removed) | None => {}
        }
    }
}

/// Lay out the primary cursor on this line: its pixel origin (the completion-
/// popup anchor, tracked even when blinked off) plus the paint quad when the
/// cursor is currently shown. `None` when the cursor isn't on this line.
fn layout_cursor(
    line: &LineCtx,
    buffer: &crate::buffer::Buffer,
    cursor: usize,
    cfg: &CursorCfg,
) -> Option<(Point<Pixels>, Option<PaintQuad>)> {
    if cursor < line.range.start || cursor > line.range.end {
        return None;
    }
    let cpos = line
        .wrapped
        .position_for_index(line.disp(cursor), line.line_height)
        .unwrap_or_default();
    let origin = point(line.line_origin.x + cpos.x, line.line_origin.y + cpos.y);
    let (width, color) = if cfg.block {
        // Width of the grapheme under the cursor (or one char at EOL), but
        // only if it sits on the same visual row. With ghost text at point the
        // next buffer grapheme renders past the whole suggestion — measuring
        // to it would stretch the block across the ghost, so fall back to one
        // character's advance (the block sits over the ghost's first char).
        let ghost_at_cursor = line.ghost.is_some_and(|(at, _)| line.disp(cursor) == at);
        let next = buffer.next_grapheme(cursor);
        let npos = (!ghost_at_cursor && next > cursor && next <= line.range.end)
            .then(|| {
                line.wrapped
                    .position_for_index(line.disp(next), line.line_height)
            })
            .flatten();
        let width = match npos {
            Some(np) if np.y == cpos.y && np.x > cpos.x => np.x - cpos.x,
            _ => cfg.char_advance,
        };
        // Translucent so the character under the block stays readable.
        let mut color = cfg.color;
        color.a *= BLOCK_CURSOR_ALPHA;
        (width, color)
    } else {
        (cfg.width, cfg.color)
    };
    let quad = cfg
        .shown
        .then(|| fill(Bounds::new(origin, size(width, line.line_height)), color));
    Some((origin, quad))
}

/// Secondary cursors (multiple-cursors): a thin caret at each position that
/// falls on this line.
fn layout_extra_cursors(
    line: &LineCtx,
    positions: &[usize],
    cfg: &CursorCfg,
    out: &mut Vec<PaintQuad>,
) {
    for &ec in positions {
        if ec >= line.range.start && ec <= line.range.end {
            let cpos = line
                .wrapped
                .position_for_index(line.disp(ec), line.line_height)
                .unwrap_or_default();
            let origin = point(line.line_origin.x + cpos.x, line.line_origin.y + cpos.y);
            out.push(fill(
                Bounds::new(origin, size(cfg.width, line.line_height)),
                cfg.secondary_color,
            ));
        }
    }
}

/// Lay out one line's decorations: the file-tree icon, diagnostic underlines,
/// virtual suffix text (inlay hints / blame), and avy jump hints. The latter
/// two only exist on the selected pane.
#[allow(clippy::too_many_arguments)] // one call site; bundles a paint pass
fn layout_decorations(
    line: &LineCtx,
    editor: &Editor,
    pane: usize,
    selected: bool,
    theme: &Theme,
    text_color: Hsla,
    font: &Font,
    font_size: Pixels,
    em: Pixels,
    diagnostics: &[crate::lsp::Diagnostic],
    diag_colors: DiagnosticColors,
    window: &mut Window,
    acc: &mut Accum,
) {
    let i = line.i;
    let range = &line.range;

    // File-tree icon, painted in a reserved column (the Scheme renders leading
    // space at `col` so it doesn't overlap the label).
    if let Some((col, name)) = editor.pane_line_icon(pane, i) {
        let ipos = line
            .wrapped
            .position_for_index(
                to_display(col.min(range.end - range.start), line.holes),
                line.line_height,
            )
            .unwrap_or_default();
        let lh_f = f32::from(line.line_height);
        let sz = lh_f * ICON_SCALE;
        let pad = (lh_f - sz) / 2.0;
        let origin = point(
            line.line_origin.x + ipos.x,
            line.line_origin.y + ipos.y + px(pad),
        );
        let color = editor
            .pane_line_face(pane, i)
            .map_or(text_color, |f| theme.color(f, text_color));
        acc.icons.push((
            Bounds::new(origin, size(px(sz), px(sz))),
            SharedString::from(format!("icons/{name}.svg")),
            color,
        ));
    }

    // Diagnostic underlines: a 2px bar along each diagnostic's span. Drawn as
    // a thin band at the bottom of each visual row the span covers.
    for diagnostic in diagnostics {
        let (d0, d1) = (diagnostic.range.start, diagnostic.range.end);
        if d0 <= range.end && d1 >= range.start {
            let color = diag_colors.for_severity(diagnostic.severity);
            let mut bars = Vec::new();
            line.push_range(&mut bars, d0, d1, color, px(4.));
            // Keep only the bottom 2px of each row's quad.
            for quad in bars {
                let b = quad.bounds;
                acc.diagnostics.push(fill(
                    Bounds::from_corners(
                        point(b.left(), b.bottom() - px(2.)),
                        point(b.right(), b.bottom()),
                    ),
                    color,
                ));
            }
        }
    }

    // Virtual suffix text (inlay hint / blame), after the line content.
    if selected && let Some((suffix, face)) = editor.line_suffix(i) {
        let color = theme.color(face, theme.color("dim", text_color));
        // Inlay-hint / code-lens text is LSP-sourced; collapse any newline so the
        // single-line `shape_line` below can't panic and abort the editor.
        let suffix = if suffix.contains(['\n', '\r']) {
            crate::util::single_line(suffix)
        } else {
            suffix.to_string()
        };
        let label = SharedString::from(format!("  {suffix}"));
        let n = label.len();
        let shaped_suffix = window.text_system().shape_line(
            label,
            font_size,
            &[plain_run(n, font.clone(), color)],
            None,
        );
        let epos = line
            .wrapped
            .position_for_index(line.disp(range.end), line.line_height)
            .unwrap_or_default();
        acc.virtual_text.push((
            shaped_suffix,
            point(line.line_origin.x + epos.x, line.line_origin.y + epos.y),
        ));
    }

    // avy hints: paint each candidate's label over the buffer at its byte
    // position (e.g. "a", "sd"). Only the focused pane carries hints.
    if selected {
        let hint_bg = theme.color("avy-hint-bg", gpui::hsla(0.13, 0.95, 0.55, 1.0));
        let hint_fg = theme.color("avy-hint-fg", gpui::hsla(0.0, 0.0, 0.05, 1.0));
        for (byte, label) in editor.avy_hints() {
            if *byte >= range.start && *byte <= range.end {
                let pos = line
                    .wrapped
                    .position_for_index(line.disp(*byte), line.line_height)
                    .unwrap_or_default();
                let origin = point(line.line_origin.x + pos.x, line.line_origin.y + pos.y);
                let shaped = window.text_system().shape_line(
                    label.clone(),
                    font_size,
                    &[plain_run(label.len(), font.clone(), hint_fg)],
                    None,
                );
                let w = shaped.width.max(em);
                let bg = fill(Bounds::new(origin, size(w, line.line_height)), hint_bg);
                acc.avy.push((bg, shaped, origin));
            }
        }
    }
}

impl IntoElement for EditorElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for EditorElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        // Fill the viewport; scrolling is handled internally, so the element is
        // viewport-sized rather than content-sized.
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let editor = self.editor.read(cx);
        let pane = self.pane;
        let selected = editor.pane_is_selected(pane);
        let buffer = editor.pane_buffer(pane);
        let theme = cx.global::<Theme>();
        let cursor_color = theme.color("cursor", rgb(0x528bff).into());
        let region_color = theme.color("region", rgba(0x3a5fcd55).into());
        // Helix multiple selections: each secondary selection paints a dimmer
        // span, and its caret a dimmer cursor, so the primary stays distinct.
        // Themes can set these explicitly; otherwise derive them from the primary
        // region/cursor by knocking the alpha down.
        let secondary_region_color = theme.color("secondary-region", {
            let mut c = region_color;
            c.a *= 0.6;
            c
        });
        let secondary_cursor_color = theme.color("secondary-cursor", {
            let mut c = cursor_color;
            c.a *= 0.7;
            c
        });
        let search_color = theme.color("search", rgba(0xffc83d66).into());
        let paren_color = theme.color("show-paren", rgba(0x5fafef66).into());
        let hl_line_color = theme.color("hl-line", rgba(0xffffff10).into());
        let settings = cx.global::<crate::settings::Settings>();
        let popup_max_rows = settings.completion_max_rows();
        let popup_show_detail = settings.completion_show_detail();
        let popup_show_kind = settings.completion_show_kind();
        // Cursor shape: "bar" (cursor-width px), "block" (char width), or "auto"
        // (block in evil Normal/Visual, otherwise a bar).
        let block_cursor = match settings.cursor_style() {
            "block" => true,
            "bar" => false,
            _ => editor.wants_block_cursor(),
        };
        let cursor_width = px(settings.cursor_width());
        // When blinking is on, only paint the cursor in the "shown" half of the
        // cycle; a steady cursor (blink off) is always shown.
        let cursor_shown = !settings.cursor_blink() || editor.cursor_blink_visible();

        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();
        let text_color = style.color;
        // The custom element doesn't inherit the div's font_family, so resolve the
        // configured `font-family` ourselves (defaults to the bundled "Lilex"). A
        // family GPUI can't resolve falls back to the platform default at shape
        // time. In Org buffers drop ligatures so prose markup like `**`/`***`
        // renders as plain asterisks instead of ligating into one vertically-
        // shifted glyph.
        let editor_font = {
            let mut f = font(settings.font_family().to_string());
            if editor.is_org() {
                f.features = gpui::FontFeatures::disable_ligatures();
            }
            f
        };

        // One monospace character's advance (block-cursor width past EOL, and the
        // line-number gutter's column width).
        let mk_run = |len: usize, color: Hsla| plain_run(len, editor_font.clone(), color);
        let em = window
            .text_system()
            .shape_line(
                SharedString::from("0"),
                font_size,
                &[mk_run(1, text_color)],
                None,
            )
            .x_for_index(1);
        let char_advance = if block_cursor { em } else { px(0.) };

        let lh = f32::from(line_height);
        let scroll = f32::from(editor.pane_scroll(pane));
        let total_lines = buffer.line_count();
        let viewport_h = f32::from(bounds.size.height);

        // Line-number gutter (Emacs display-line-numbers-mode). Per major mode:
        // on by default for every pane's buffer, off for modes that opt out
        // (e.g. the dired-sidebar tree). Resolved against this pane's own major
        // mode so a code pane shows numbers while the tree next to it doesn't.
        let line_numbers = settings.line_numbers_for(editor.pane_major_mode(pane));
        // hl-line: a background band on the cursor's row, selected pane only.
        let hl_line = selected && settings.hl_line_for(editor.pane_major_mode(pane));
        let gutter_digits = total_lines.max(1).to_string().len().max(3);
        let gutter_pad = GUTTER_PAD;
        // Gutter fold chevrons: selected pane only, and only when the gutter
        // exists (line numbers on) so they have somewhere to live. When shown they
        // claim a dedicated column on the left of the gutter so they never overlap
        // the line numbers; `chevron_sz` is the glyph size, `fold_col` its column.
        let fold_markers_ui = selected && line_numbers && editor.has_fold_markers();
        let chevron_sz = px((lh * 0.7).min(14.0));
        let fold_col = if fold_markers_ui {
            chevron_sz + px(6.)
        } else {
            px(0.)
        };
        let gutter_width = if line_numbers {
            fold_col + em * gutter_digits as f32 + gutter_pad * 2.0
        } else {
            px(0.)
        };
        let text_left = bounds.left() + gutter_width;
        let line_number_color = theme.color("line-number", theme.color("dim", text_color));
        // Diagnostic gutter dots ride alongside the line numbers, so they need the
        // gutter to exist; gate on both the option and line numbers being shown.
        let diag_gutter = line_numbers && settings.diagnostics_gutter();
        // git-gutter (diff-hl): change bars at the gutter's far-left edge. Like
        // the diagnostic dots, gated on the line-number gutter existing.
        let git_gutter_on = line_numbers && settings.magit_gutter();
        let git_gutter_w = px(settings.magit_gutter_width());
        let git_gutter_del = settings.magit_gutter_deleted();
        let gg_added = theme.color("magit-gutter-added", rgb(0x6cae3e).into());
        let gg_modified = theme.color("magit-gutter-modified", rgb(0xc7a23a).into());
        let gg_removed = theme.color("magit-gutter-removed", rgb(0xc1574b).into());
        // Diagnostic colors (gutter dots + underlines), resolved once per frame.
        let diag_colors = DiagnosticColors::resolve(theme);

        // Copilot-style ghost text (selected pane only — it lives on the active
        // buffer, like `line_holes`). `ghost_at` validates at read time: the
        // suggestion is only visible while point still sits on its anchor byte.
        // The first line is spliced into the anchor line's display text; the
        // continuation lines paint on display rows reserved below it. The split
        // and face lookup only run while a ghost is showing — this path is
        // per-frame, and no ghost is the overwhelmingly common case.
        let ghost = if selected { editor.ghost_at() } else { None };
        let (ghost_first, ghost_tail, ghost_color) = match ghost {
            Some((_, t)) => {
                let mut parts = t.split('\n');
                let first = parts.next().unwrap_or("");
                // The `ghost` face, falling back to the theme's dim shade,
                // then to a faded foreground so the suggestion reads as
                // tentative even on a theme that defines neither.
                let mut faded = text_color;
                faded.a *= 0.55;
                let color = theme.color("ghost", theme.color("dim", faded));
                (first, parts.collect::<Vec<_>>(), color)
            }
            None => ("", Vec::new(), text_color),
        };
        // The ghost's anchor line + reserved tail rows, resolved once per frame
        // (folded-away anchors contribute nothing) — the row loop below must
        // not re-derive this per line.
        let ghost_rows = if ghost.is_some() {
            editor.ghost_rows()
        } else {
            None
        };

        // Folds collapse lines into fewer rows; inline images expand a line into
        // extra rows below it. Either way `rows[k]` is the buffer line drawn
        // starting at a known screen row. Without either, this is the identity
        // (the fast path).
        let folds = selected && editor.has_folds();
        let images = selected && editor.has_line_images();
        let scales = selected && editor.has_line_scales();
        let dynamic = folds || images || scales || ghost_rows.is_some();
        let display_count = if dynamic {
            editor.display_line_count()
        } else {
            total_lines
        };
        let first_row = (scroll / lh).floor().max(0.0) as usize;
        let visible = (viewport_h / lh).ceil() as usize + 1;
        let last_row = (first_row + visible).min(display_count);

        // Cumulative display row of the first painted line — the base for `y`
        // when folds/images make display rows differ from buffer-line indices.
        let mut first_top_row = first_row;
        let rows: Vec<usize> = if dynamic {
            let mut v = Vec::new();
            let mut r = 0usize;
            for i in 0..total_lines {
                if folds && editor.is_line_invisible(i) {
                    continue;
                }
                // 1 row for the line's text + N reserved rows for its image, +
                // any extra rows a scaled (larger) line or a multi-line ghost
                // suggestion claims.
                let line_rows = 1
                    + if images { editor.image_rows_at(i) } else { 0 }
                    + if scales { editor.scale_rows_at(i) } else { 0 }
                    + match ghost_rows {
                        Some((g, rows)) if g == i => rows,
                        _ => 0,
                    };
                if r < last_row && r + line_rows > first_row {
                    if v.is_empty() {
                        first_top_row = r;
                    }
                    v.push(i);
                }
                r += line_rows;
                if r >= last_row {
                    break;
                }
            }
            v
        } else {
            (first_row..last_row).collect()
        };

        // Highlight only the visible byte range, queried on demand.
        let highlights = match (rows.first(), rows.last()) {
            (Some(&f), Some(&l)) => {
                let start = buffer.line_range(f).start;
                let end = buffer.line_range(l).end;
                editor.pane_highlights(pane, start..end)
            }
            _ => Vec::new(),
        };

        // Sub-line overlay faces (selected pane only).
        let empty_overlays: &[(std::ops::Range<usize>, SharedString)] = &[];
        let overlay_faces = if selected {
            editor.range_faces()
        } else {
            empty_overlays
        };

        let cursor = editor.pane_point(pane);
        let selection = editor.pane_selection(pane);
        // Block-wise Visual (`C-v`): a rectangle (top_row, bottom_row, left_col,
        // right_col) highlighted per line as a column strip rather than the
        // contiguous `selection` range. `None` unless in block-wise Visual.
        let block_sel = editor.pane_visual_block(pane);
        let diagnostics = editor.pane_diagnostics(pane);
        let diff_marks = editor.pane_diff_marks(pane);
        let gutter_dots = editor.pane_gutter_dots(pane);
        // Search matches highlight only in the selected pane (the active buffer),
        // and only over the visible line span — O(viewport), recomputed each
        // frame so it can't go stale.
        let search = match (selected, rows.first(), rows.last()) {
            (true, Some(&top), Some(&bot)) => {
                let vis_start = buffer.line_range(top).start;
                let vis_end = buffer.line_range(bot).end;
                editor.search_match_ranges_in(vis_start, vis_end)
            }
            _ => Vec::new(),
        };
        // Matching-delimiter highlight (Emacs show-paren), selected pane only,
        // recomputed each frame so it tracks the cursor.
        let paren = if selected && settings.show_paren_for(editor.pane_major_mode(pane)) {
            editor.show_paren_ranges()
        } else {
            Vec::new()
        };

        // Word-wrap (Emacs visual-line-mode): when on, soft-wrap each line at the
        // text-area width; when off, leave lines unwrapped and shift them left by
        // the pane's horizontal scroll so off-screen text is reachable.
        let word_wrap = settings.word_wrap_for(editor.pane_major_mode(pane));
        let text_area_w = (f32::from(bounds.size.width) - f32::from(gutter_width)).max(1.0);
        let wrap_width = if word_wrap {
            Some(px(text_area_w))
        } else {
            None
        };
        let h_scroll = if selected && !word_wrap {
            editor.h_scroll()
        } else {
            px(0.)
        };
        let text_x = text_left - h_scroll;

        let mut lines = Vec::new();
        let mut cursor_quad = None;
        let mut hl_line_quad: Option<PaintQuad> = None;
        let mut cursor_px: Option<Point<Pixels>> = None;
        let mut acc = Accum::default();
        let extra_cursor_positions = editor.extra_cursor_positions();
        // Secondary selection spans (Helix multiple selections); selected pane
        // only, like the carets. Each is highlighted with `secondary_region_color`.
        let extra_selections = if selected {
            editor.extra_selections()
        } else {
            Vec::new()
        };
        let mut image_quads: Vec<(Bounds<Pixels>, Arc<RenderImage>)> = Vec::new();

        let gutter_cfg = GutterCfg {
            bounds,
            gutter_width,
            gutter_pad,
            fold_col,
            chevron_sz,
            fold_markers: fold_markers_ui,
            diag_gutter,
            git_gutter: git_gutter_on,
            git_gutter_w,
            git_gutter_del,
            gg_added,
            gg_modified,
            gg_removed,
            number_color: line_number_color,
            font: editor_font.clone(),
            font_size,
            diag_colors,
        };
        let cursor_cfg = CursorCfg {
            block: block_cursor,
            width: cursor_width,
            shown: cursor_shown,
            char_advance,
            color: cursor_color,
            secondary_color: secondary_cursor_color,
        };

        // Lines have variable height under wrapping (and under inline images), so
        // accumulate pixel offsets rather than multiplying a row index. Start at
        // the top of the first visible line (which may be partially scrolled off
        // the top); `first_top_row` is its cumulative display row.
        let mut y = px(first_top_row as f32 * lh - scroll);
        let viewport_bottom = px(viewport_h);
        for &i in &rows {
            let (range, text) = buffer.line(i);
            // Stop laying out once below the viewport — but keep going until the
            // cursor's own line has been laid out, so `reconcile_cursor_scroll`
            // learns its real (wrapped) pixel position and can follow it. Under
            // word-wrap the lines above the cursor can fill the viewport before
            // the loop reaches the cursor's line (e.g. jumping to end-of-buffer),
            // so breaking at the first off-screen line would leave `cursor_px`
            // unset and the scroll a few rows short of revealing the cursor. Once
            // the cursor has been located — or in a non-selected pane, which has
            // no cursor to follow — the early-out stands. Off-screen lines laid
            // out in the interim are clipped to `bounds` anyway.
            if y >= viewport_bottom && (!selected || cursor_px.is_some()) {
                break;
            }
            let top = y;

            // `overlay-hide` ranges intersecting this line, made line-relative and
            // restricted to char boundaries; spliced out before shaping so the
            // stars (etc.) collapse. Empty for almost every line. The line the
            // cursor sits on is revealed in full (Emacs-style) so hidden markup —
            // e.g. Org's leading stars — is visible and editable while point is
            // there; moving off the line re-hides it on the next frame.
            //
            // Selected pane only: `overlay-hide` ranges live on the *active*
            // buffer (`line_holes` reads `self.buffer`/`self.overlays`), so a
            // non-selected pane must not consult them — its rows index a different,
            // possibly longer buffer (e.g. a tall dired-sidebar tree beside a short
            // file), which would index the active buffer past its end. Matches the
            // `selected` gating on `overlay_faces` and the cursor-line reveal below.
            let mut holes = if selected {
                editor.line_holes(i)
            } else {
                Vec::new()
            };
            holes.retain(|&(s, e)| s < e && text.is_char_boundary(s) && text.is_char_boundary(e));
            // The cursor's own line is revealed in full so hidden markup — e.g.
            // Org's leading stars — is visible and editable while point is there.
            // The width those stars reclaim is taken back out of the indent below,
            // so the visible title stays put instead of jumping right as the
            // cursor arrives. Moving off the line re-hides it next frame.
            let revealed = selected && cursor >= range.start && cursor <= range.end;
            let reveal_cols: usize = if revealed {
                holes.iter().map(|&(s, e)| e - s).sum()
            } else {
                0
            };
            if revealed {
                holes.clear();
            }

            // Virtual left indent (Org `org-indent-mode`): shift this line's text
            // right by N columns. Display-only — the buffer text is untouched, so
            // there is no real whitespace to navigate (matches Emacs).
            let indent_px =
                px(editor.line_indent(i).saturating_sub(reveal_cols) as f32 * f32::from(em));
            let lx = text_x + indent_px;
            // Whole-line scale (markdown/org headings rendered larger). A scaled
            // line is shaped at `scale`× the base font and doesn't wrap; it
            // reserves `1 + scale_rows_at` display rows (claimed above), so the
            // bigger text sits in that taller block and the row math stays exact.
            let scale = if scales { editor.line_scale(i) } else { 1.0 };
            let line_font_size = font_size * scale;
            // Narrow the wrap width by the indent so wrapped rows stay in the pane.
            // Scaled heading lines never wrap (keeps them a single, taller block).
            let line_wrap_width = if scale > 1.0 {
                None
            } else {
                wrap_width.map(|w| px((f32::from(w) - f32::from(indent_px)).max(1.0)))
            };

            // Compute the styled runs, then shape once (the branches only
            // differ in which spans and base color tile the line).
            let runs = if text.is_empty() {
                Vec::new()
            } else if let Some(face) = editor.pane_line_face(pane, i) {
                // Special buffers (magit-style) paint whole lines by face name —
                // but sub-line overlay accents (magit's status labels, diff +/-
                // markers) still override their byte range, layered on top of the
                // whole-line color as the base (no syntax spans here). The face's
                // bold attribute rides along as the line's base weight.
                let color = theme.color(face, text_color);
                line_runs(
                    &[],
                    overlay_faces,
                    range.clone(),
                    &editor_font,
                    color,
                    theme.is_bold(face),
                    theme,
                )
            } else {
                line_runs(
                    &highlights,
                    overlay_faces,
                    range.clone(),
                    &editor_font,
                    text_color,
                    false,
                    theme,
                )
            };
            // Ghost anchored on this line (point's line): splice the
            // suggestion's first line into the display text at point — the
            // inverse of the `holes` splice-out — so the rest of the real line
            // is pushed right and soft-wrap measures the suggestion like real
            // text. Point's line is `revealed` above, so `holes` is empty here
            // and display indices equal line-relative buffer offsets.
            let ghost_here = ghost.filter(|(at, _)| *at >= range.start && *at <= range.end);
            let line_ghost = ghost_here
                .filter(|_| !ghost_first.is_empty())
                .map(|(at, _)| (to_display(at - range.start, &holes), ghost_first.len()));
            let mut display_text = remove_holes(&text, &holes);
            let mut display_runs = punch_runs(runs, &holes);
            if let Some((at, len)) = line_ghost {
                display_text.insert_str(at, ghost_first);
                display_runs =
                    inject_ghost_run(display_runs, at, ghost_run(len, &editor_font, ghost_color));
            }
            let wrapped = window.text_system().shape_text(
                SharedString::from(display_text),
                line_font_size,
                &display_runs,
                line_wrap_width,
                None,
            );
            let wrapped = wrapped.ok().and_then(|mut v| v.pop()).unwrap_or_default();
            // A scaled line occupies the whole reserved block (its claimed rows ×
            // line height) so `y` advances by the rows it reserved above; an
            // unscaled line is just its shaped (possibly wrapped) height.
            let line_h = if scale > 1.0 {
                px((1 + editor.scale_rows_at(i)) as f32 * lh)
            } else {
                wrapped.size(line_height).height
            };

            // A scaled (heading) line reserves whole rows (`ceil(scale)`), taller
            // than the one base row a line normally occupies. `WrappedLine::paint`
            // vertically centers the glyph run within the `line_height` it's given
            // (here the base `lh`), so the glyphs' center sits one half-row below
            // `content_top`. To center them in the reserved block, offset the whole
            // line's content (glyphs, cursor, selection, inlays — and the line
            // number) down by half the slack: `(block − one base row) / 2`. Zero for
            // ordinary lines, which keeps wrapped multi-row lines top-aligned.
            let content_dy = if scale > 1.0 {
                (line_h - px(lh)) / 2.0
            } else {
                px(0.)
            };
            let content_top = bounds.top() + top + content_dy;
            let line_origin = point(lx, content_top);
            let lc = LineCtx {
                i,
                range: range.clone(),
                wrapped: &wrapped,
                holes: &holes,
                top,
                line_h,
                line_origin,
                line_height,
                ghost: line_ghost,
            };

            // Gutter: line number, fold chevron, diagnostic dot, git bar.
            if line_numbers {
                layout_gutter(
                    &lc,
                    &gutter_cfg,
                    editor,
                    theme,
                    &diagnostics,
                    diff_marks,
                    gutter_dots,
                    window,
                    &mut acc,
                );
            }

            if hl_line && cursor >= range.start && cursor <= range.end && hl_line_quad.is_none() {
                // Span the full pane width (gutter included), one line tall.
                let origin = point(bounds.left(), bounds.top() + top);
                hl_line_quad = Some(fill(
                    Bounds::new(origin, size(bounds.size.width, line_h)),
                    hl_line_color,
                ));
            }

            // Cursors: the primary caret (also the completion-popup anchor,
            // tracked even when blinked off) and any secondary carets.
            if selected {
                if let Some((origin, quad)) = layout_cursor(&lc, buffer, cursor, &cursor_cfg) {
                    cursor_px = Some(origin);
                    cursor_quad = quad;
                }
                layout_extra_cursors(
                    &lc,
                    &extra_cursor_positions,
                    &cursor_cfg,
                    &mut acc.extra_cursors,
                );
            }

            // Range highlights, all through the same per-row quad helper.
            if let Some((top_row, bottom_row, left_col, right_col)) = block_sel {
                // Block-wise Visual: highlight only the `[left_col, right_col]`
                // column strip on each row within the rectangle (rows shorter
                // than the strip contribute nothing).
                if i >= top_row && i <= bottom_row {
                    let s = char_col_byte_in(&text, left_col);
                    let e = char_col_byte_in(&text, right_col + 1);
                    if e > s {
                        lc.push_range(
                            &mut acc.selections,
                            range.start + s,
                            range.start + e,
                            region_color,
                            px(0.),
                        );
                    }
                }
            } else if !selection.is_empty() {
                lc.push_range(
                    &mut acc.selections,
                    selection.start,
                    selection.end,
                    region_color,
                    px(0.),
                );
            }
            // Secondary selections (Helix multiple selections): one dimmer span
            // per selection that overlaps this line. Each carries its own color,
            // so they coexist with the primary highlight in the same quad list.
            for &(ss, se) in &extra_selections {
                if se > ss {
                    lc.push_range(&mut acc.selections, ss, se, secondary_region_color, px(0.));
                }
            }
            // Search matches: a translucent background highlight per match.
            for m in &search {
                lc.push_range(&mut acc.search, m.start, m.end, search_color, px(0.));
            }
            // Matching-delimiter highlight: the bracket pair (each one char).
            for m in &paren {
                lc.push_range(&mut acc.paren, m.start, m.end, paren_color, px(0.));
            }

            // Decorations: file-tree icon, diagnostic underlines, virtual
            // suffix text, avy hints.
            layout_decorations(
                &lc,
                editor,
                pane,
                selected,
                theme,
                text_color,
                &editor_font,
                font_size,
                em,
                &diagnostics,
                diag_colors,
                window,
                &mut acc,
            );

            lines.push(LineLayout {
                wrapped,
                top,
                range,
                holes,
                indent: indent_px,
                height: line_h,
                content_dy,
                ghost: line_ghost,
            });
            y += line_h;

            // Ghost continuation lines: painted on the display rows reserved
            // below point's line (the inline-image pattern), one per `\n` in
            // the suggestion. Not hit-testable — a click below the anchor line
            // maps to the following buffer line, and moving point dismisses
            // the ghost anyway.
            if ghost_here.is_some() {
                for t in &ghost_tail {
                    if !t.is_empty() {
                        let shaped = window.text_system().shape_line(
                            SharedString::from(t.to_string()),
                            font_size,
                            &[ghost_run(t.len(), &editor_font, ghost_color)],
                            None,
                        );
                        acc.virtual_text.push((shaped, point(lx, bounds.top() + y)));
                    }
                    y += px(lh);
                }
            }

            // Inline image: paint it in the rows reserved below this line, and
            // advance past them. `image_rows_at` and the displayed size agree on
            // the fixed max width, so the reserved height matches the bitmap.
            if images && let Some((data, disp_w, disp_h)) = editor.line_image_at(i) {
                let img_origin = point(text_left + indent_px, bounds.top() + y);
                image_quads.push((Bounds::new(img_origin, size(px(disp_w), px(disp_h))), data));
                y += px(editor.image_rows_at(i) as f32 * lh);
            }
        }

        // The completion popup, anchored just below the cursor.
        let popup = cursor_px
            .zip(editor.completion_view())
            .map(|(anchor, view)| {
                build_popup(
                    anchor,
                    view,
                    popup_max_rows,
                    popup_show_detail,
                    popup_show_kind,
                    &editor_font,
                    font_size,
                    line_height,
                    bounds,
                    theme,
                    window,
                )
            });

        // Scroll bar (Emacs scroll-bar-mode): a thumb on the pane's right edge,
        // sized/positioned by the visible fraction. Overlay — doesn't reserve
        // gutter space. Only when content overflows the viewport.
        let mut scrollbar = Vec::new();
        if settings.scroll_bar() {
            let content_h = display_count as f32 * lh;
            if content_h > viewport_h + 1.0 {
                let sb_w = SCROLLBAR_WIDTH;
                let x1 = bounds.right();
                let x0 = x1 - sb_w;
                let track_color = theme.color("scroll-bar-track", rgba(0x88888822).into());
                scrollbar.push(fill(
                    Bounds::from_corners(
                        point(x0, bounds.top()),
                        point(x1, bounds.top() + px(viewport_h)),
                    ),
                    track_color,
                ));
                let thumb_h = (viewport_h / content_h * viewport_h)
                    .max(SCROLLBAR_MIN_THUMB)
                    .min(viewport_h);
                let max_scroll = (content_h - viewport_h).max(1.0);
                let t = (scroll / max_scroll).clamp(0.0, 1.0);
                let thumb_top = bounds.top() + px(t * (viewport_h - thumb_h));
                let thumb_color = theme.color("scroll-bar", rgba(0x888888aa).into());
                scrollbar.push(fill(
                    Bounds::from_corners(point(x0, thumb_top), point(x1, thumb_top + px(thumb_h))),
                    thumb_color,
                ));
            }
        }

        PrepaintState {
            lines,
            cursor: cursor_quad,
            hl_line: hl_line_quad,
            selections: acc.selections,
            search: acc.search,
            paren: acc.paren,
            scrollbar,
            icons: acc.icons,
            images: image_quads,
            diagnostics: acc.diagnostics,
            git_gutter: acc.git_gutter,
            popup,
            text_left,
            line_x: text_x,
            char_width: em,
            gutter: acc.gutter,
            fold_chevrons: acc.fold_chevrons,
            fold_hitboxes: acc.fold_hitboxes,
            fold_cursors: acc.fold_cursors,
            virtual_text: acc.virtual_text,
            avy: acc.avy,
            extra_cursors: acc.extra_cursors,
            cursor_rel_y: cursor_px.map(|p| p.y - bounds.top()),
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let selected = self.editor.read(cx).pane_is_selected(self.pane);
        let focus_handle = self.editor.read(cx).focus_handle.clone();

        // Route IME / typed text to the editor for this region — only the
        // selected pane is live, so only it claims the input handler.
        if selected {
            window.handle_input(
                &focus_handle,
                ElementInputHandler::new(bounds, self.editor.clone()),
                cx,
            );
        }

        let line_height = window.line_height();
        // Captured before the paint closure consumes the popup, so we can report
        // its clickable geometry back to the editor afterwards.
        let popup_hit = prepaint.popup.as_ref().map(|p| p.hit);

        // Clip painting to the viewport so partially-scrolled top/bottom lines
        // don't bleed past the editor (e.g. into the status line).
        let text_left = prepaint.text_left;
        let line_x = prepaint.line_x;
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            // hl-line band sits behind everything else on its row.
            if let Some(quad) = prepaint.hl_line.take() {
                window.paint_quad(quad);
            }
            // Search highlights sit behind the selection and the text.
            for quad in prepaint.search.drain(..) {
                window.paint_quad(quad);
            }
            // Matching-delimiter highlights, likewise behind selection and text.
            for quad in prepaint.paren.drain(..) {
                window.paint_quad(quad);
            }
            for quad in prepaint.selections.drain(..) {
                window.paint_quad(quad);
            }
            // Line-number gutter labels.
            for (label, origin) in prepaint.gutter.drain(..) {
                label.paint(origin, line_height, window, cx).ok();
            }
            for line in &prepaint.lines {
                let origin = point(
                    line_x + line.indent,
                    bounds.top() + line.top + line.content_dy,
                );
                line.wrapped
                    .paint(origin, line_height, TextAlign::Left, None, window, cx)
                    .ok();
            }
            // Virtual suffix text (inlay/blame), after the line content.
            for (label, origin) in prepaint.virtual_text.drain(..) {
                label.paint(origin, line_height, window, cx).ok();
            }
            for quad in prepaint.diagnostics.drain(..) {
                window.paint_quad(quad);
            }
            // git-gutter change bars + deletion marks, in the gutter's left edge.
            for quad in prepaint.git_gutter.drain(..) {
                window.paint_quad(quad);
            }
            // Scroll bar sits above the text on the right edge.
            for quad in prepaint.scrollbar.drain(..) {
                window.paint_quad(quad);
            }
            // File-tree icons (monochrome SVGs tinted by the line color).
            for (icon_bounds, path, color) in prepaint.icons.drain(..) {
                window
                    .paint_svg(icon_bounds, path, TransformationMatrix::unit(), color, cx)
                    .ok();
            }
            // Fold chevrons in the gutter (monochrome SVGs, tinted like numbers).
            for (chevron_bounds, path, color) in prepaint.fold_chevrons.drain(..) {
                window
                    .paint_svg(
                        chevron_bounds,
                        path,
                        TransformationMatrix::unit(),
                        color,
                        cx,
                    )
                    .ok();
            }
            // A hand pointer over each chevron (overrides the editor's IBeam).
            for hitbox in &prepaint.fold_cursors {
                window.set_cursor_style(CursorStyle::PointingHand, hitbox);
            }
            // Inline raster images (Org inline images / LaTeX previews), painted
            // in the rows reserved below their line.
            for (img_bounds, data) in prepaint.images.drain(..) {
                window
                    .paint_image(img_bounds, Corners::default(), data, 0, false)
                    .ok();
            }
            if focus_handle.is_focused(window)
                && let Some(cursor) = prepaint.cursor.take()
            {
                window.paint_quad(cursor);
            }
            if let Some(popup) = prepaint.popup.take() {
                for quad in popup.quads {
                    window.paint_quad(quad);
                }
                for (line, origin) in popup.labels {
                    line.paint(origin, line_height, window, cx).ok();
                }
            }
            // Secondary cursors (multiple-cursors), drawn whenever focused.
            if focus_handle.is_focused(window) {
                for caret in prepaint.extra_cursors.drain(..) {
                    window.paint_quad(caret);
                }
            }
            // avy hints sit on top of everything (text, cursor, popup).
            for (bg, label, origin) in prepaint.avy.drain(..) {
                window.paint_quad(bg);
                label.paint(origin, line_height, window, cx).ok();
            }
        });

        // Report this pane's bounds for click-to-focus, and (for the selected
        // pane only) hand back the visible layout for hit-testing / IME / cursor.
        let lines = if selected {
            std::mem::take(&mut prepaint.lines)
        } else {
            Vec::new()
        };
        let pane = self.pane;
        let char_width = prepaint.char_width;
        let major_mode = self
            .editor
            .read(cx)
            .pane_major_mode(pane)
            .map(str::to_string);
        let word_wrap = cx
            .global::<crate::settings::Settings>()
            .word_wrap_for(major_mode.as_deref());
        // The text region starts past the gutter; mouse hit-testing uses it so
        // clicks map to the right column when line numbers are on.
        let text_bounds = gpui::Bounds {
            origin: point(text_left, bounds.top()),
            size: gpui::size(
                bounds.size.width - (text_left - bounds.left()),
                bounds.size.height,
            ),
        };
        let cursor_rel_y = prepaint.cursor_rel_y;
        let fold_hitboxes = std::mem::take(&mut prepaint.fold_hitboxes);
        self.editor.update(cx, |editor, cx| {
            editor.report_pane_bounds(pane, bounds);
            if selected {
                editor.set_layout(lines, text_bounds, line_height, char_width, word_wrap);
                editor.report_completion_popup(popup_hit);
                editor.report_fold_hitboxes(fold_hitboxes);
                // Follow the cursor against the real (wrapped) geometry; corrects
                // the buffer-line estimate `ensure_cursor_visible` works from.
                editor.reconcile_cursor_scroll(cursor_rel_y, cx);
            }
        });
    }
}

/// Byte offset within `text` of character column `col`, clamped to the text's
/// length (a column past the end lands at the end). Maps a block-Visual
/// rectangle's columns to per-line byte ranges for highlighting.
fn char_col_byte_in(text: &str, col: usize) -> usize {
    text.char_indices()
        .nth(col)
        .map(|(b, _)| b)
        .unwrap_or(text.len())
}

/// Emit one background-fill quad per visual row that the display-text byte
/// range `[disp_start, disp_end)` covers within a (possibly wrapped) line.
/// Indices are relative to the line's display text (holes already spliced
/// out — see [`LineCtx::disp`]). `origin` is the line's top-left in window
/// space (text x already shifted for horizontal scroll). `min_width` widens a
/// zero/near-zero span so it stays visible (used for diagnostics).
#[allow(clippy::too_many_arguments)] // a focused per-row highlight helper
fn push_range_quads(
    out: &mut Vec<PaintQuad>,
    wrapped: &WrappedLine,
    disp_start: usize,
    disp_end: usize,
    origin: Point<Pixels>,
    line_height: Pixels,
    color: Hsla,
    min_width: Pixels,
) {
    if disp_end <= disp_start {
        return;
    }
    let local_s = disp_start;
    let local_e = disp_end;
    let len = wrapped.len();
    // Byte index where each visual row (after the first) begins.
    let boundaries: Vec<usize> = wrapped
        .wrap_boundaries()
        .iter()
        .map(|wb| wrapped.unwrapped_layout.runs[wb.run_ix].glyphs[wb.glyph_ix].index)
        .collect();
    let mut row_start = 0usize;
    for row in 0..=boundaries.len() {
        let row_end = boundaries.get(row).copied().unwrap_or(len);
        let a = local_s.max(row_start);
        let b = local_e.min(row_end);
        if b > a {
            let row_x0 = wrapped.unwrapped_layout.x_for_index(row_start);
            let x0 = wrapped.unwrapped_layout.x_for_index(a) - row_x0;
            let mut x1 = wrapped.unwrapped_layout.x_for_index(b) - row_x0;
            if x1 - x0 < min_width {
                x1 = x0 + min_width;
            }
            let y = origin.y + px(row as f32 * f32::from(line_height));
            out.push(fill(
                Bounds::from_corners(
                    point(origin.x + x0, y),
                    point(origin.x + x1, y + line_height),
                ),
                color,
            ));
        }
        row_start = row_end;
    }
}

/// Lay out the completion popup below `anchor` (the cursor cell's top-left):
/// shape each visible candidate, size the box to the widest, and produce the
/// background/border/selection quads plus positioned label lines.
#[allow(clippy::too_many_arguments)]
fn build_popup(
    anchor: Point<Pixels>,
    view: CompletionView,
    max_rows: usize,
    show_detail: bool,
    show_kind: bool,
    font: &Font,
    font_size: Pixels,
    line_height: Pixels,
    editor_bounds: Bounds<Pixels>,
    theme: &Theme,
    window: &mut Window,
) -> PopupLayout {
    let total = view.items.len();
    // Scroll the window so the selected row stays visible.
    let start = if view.selected >= max_rows {
        view.selected + 1 - max_rows
    } else {
        0
    };
    let end = (start + max_rows).min(total);
    let visible = &view.items[start..end];

    let fg = theme.color("completion-fg", theme.foreground());
    let bg = theme.color(
        "completion-bg",
        theme.color("mode-line-bg", theme.background()),
    );
    let selected_bg = theme.color(
        "completion-selected-bg",
        theme.color("region", theme.foreground()),
    );
    let selected_fg = theme.color("completion-selected-fg", fg);
    let border = theme.color("completion-border", theme.color("dim", theme.foreground()));
    let dim = theme.color("dim", fg);
    let kind_color = theme.color("completion-kind", theme.color("type", dim));

    let run = |len: usize, color: Hsla| plain_run(len, font.clone(), color);

    let mut shaped = Vec::new();
    let mut max_width = px(0.);
    for (i, row) in visible.iter().enumerate() {
        // Each row is `label  detail  kind`, with the label in the foreground and
        // the (optional, Scheme-gated) signature and kind dimmed.
        let label_color = if start + i == view.selected {
            selected_fg
        } else {
            fg
        };
        let mut text = row.label.clone();
        let mut runs = vec![run(row.label.len(), label_color)];
        if show_detail && let Some(detail) = &row.detail {
            let segment = format!("  {detail}");
            runs.push(run(segment.len(), dim));
            text.push_str(&segment);
        }
        if show_kind && let Some(kind) = &row.kind {
            let segment = format!("  {kind}");
            runs.push(run(segment.len(), kind_color));
            text.push_str(&segment);
        }
        let length = text.len();
        // `shape_line` panics (aborting the editor) on an embedded newline. Items
        // are sanitized at parse time, but guard here too so no future text source
        // can crash the popup. Same byte length, so `runs` stay valid.
        debug_assert!(!text.contains('\n'), "completion row text has a newline");
        let text: String = text
            .chars()
            .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
            .collect();
        let line =
            window
                .text_system()
                .shape_line(SharedString::from(text), font_size, &runs, None);
        let width = line.x_for_index(length);
        if width > max_width {
            max_width = width;
        }
        shaped.push(line);
    }

    let padding = POPUP_PAD;
    let width = max_width + padding + padding;
    let rows = shaped.len();
    let height = line_height * rows as f32;
    // Default below the cursor line. Flip ABOVE it when the box would spill past
    // the pane's bottom edge (off the window) AND there's more room above the
    // cursor than below — so it stays on-screen instead of being clipped. When
    // flipped, the box's bottom rests just above the cursor line (`anchor.y`).
    let below_top = anchor.y + line_height;
    let space_below = editor_bounds.bottom() - below_top;
    let space_above = anchor.y - editor_bounds.top();
    let top = if height > space_below && space_above > space_below {
        anchor.y - height
    } else {
        below_top
    };
    // Keep the box within the editor horizontally.
    let max_left = editor_bounds.right() - width;
    let mut left = anchor.x;
    if left > max_left {
        left = max_left;
    }
    if left < editor_bounds.left() {
        left = editor_bounds.left();
    }

    let mut quads = Vec::new();
    // Border, then the background inset by 1px.
    quads.push(fill(
        Bounds::from_corners(
            point(left - px(1.), top - px(1.)),
            point(left + width + px(1.), top + height + px(1.)),
        ),
        border,
    ));
    quads.push(fill(
        Bounds::from_corners(point(left, top), point(left + width, top + height)),
        bg,
    ));
    // Selected-row highlight.
    let selected_row = view.selected - start;
    let sy = top + line_height * selected_row as f32;
    quads.push(fill(
        Bounds::from_corners(point(left, sy), point(left + width, sy + line_height)),
        selected_bg,
    ));

    let hit = crate::editor::PopupHit {
        bounds: Bounds::from_corners(point(left, top), point(left + width, top + height)),
        row_height: line_height,
        start,
        rows,
    };

    let mut labels: Vec<(ShapedLine, Point<Pixels>)> = shaped
        .into_iter()
        .enumerate()
        .map(|(i, line)| (line, point(left + padding, top + line_height * i as f32)))
        .collect();

    // Documentation side-panel for the selected item (resolved lazily). Sits to
    // the right of the popup, or below it when there isn't room.
    if let Some(doc) = view.doc.as_ref() {
        let doc_fg = theme.color("completion-doc-fg", fg);
        let doc_bg = theme.color("completion-doc-bg", bg);
        let mut doc_lines: Vec<String> = Vec::new();
        for raw in doc.lines() {
            if raw.trim().is_empty() {
                doc_lines.push(String::new());
            } else {
                let mut cur = String::new();
                let mut cur_cols = 0usize;
                for word in raw.split_whitespace() {
                    // Wrap by character columns, not UTF-8 bytes, so non-ASCII
                    // docs don't wrap early.
                    let word_cols = word.chars().count();
                    if cur.is_empty() {
                        cur = word.to_string();
                        cur_cols = word_cols;
                    } else if cur_cols + 1 + word_cols <= DOC_WRAP_COLS {
                        cur.push(' ');
                        cur.push_str(word);
                        cur_cols += 1 + word_cols;
                    } else {
                        doc_lines.push(std::mem::take(&mut cur));
                        cur = word.to_string();
                        cur_cols = word_cols;
                    }
                }
                if !cur.is_empty() {
                    doc_lines.push(cur);
                }
            }
            if doc_lines.len() >= DOC_MAX_ROWS {
                break;
            }
        }
        doc_lines.truncate(DOC_MAX_ROWS);
        if !doc_lines.is_empty() {
            let mut shaped_doc = Vec::new();
            let mut doc_w = px(0.);
            for dl in &doc_lines {
                let line = window.text_system().shape_line(
                    SharedString::from(dl.clone()),
                    font_size,
                    &[run(dl.len(), doc_fg)],
                    None,
                );
                let w = line.x_for_index(dl.len());
                if w > doc_w {
                    doc_w = w;
                }
                shaped_doc.push(line);
            }
            let doc_width = doc_w + padding + padding;
            let doc_height = line_height * doc_lines.len() as f32;
            let gap = px(6.);
            let (dleft, dtop) = if left + width + gap + doc_width <= editor_bounds.right() {
                (left + width + gap, top)
            } else {
                (left, top + height + gap)
            };
            quads.push(fill(
                Bounds::from_corners(
                    point(dleft - px(1.), dtop - px(1.)),
                    point(dleft + doc_width + px(1.), dtop + doc_height + px(1.)),
                ),
                border,
            ));
            quads.push(fill(
                Bounds::from_corners(
                    point(dleft, dtop),
                    point(dleft + doc_width, dtop + doc_height),
                ),
                doc_bg,
            ));
            for (i, line) in shaped_doc.into_iter().enumerate() {
                labels.push((line, point(dleft + padding, dtop + line_height * i as f32)));
            }
        }
    }

    PopupLayout { quads, labels, hit }
}

/// Build the `TextRun`s for one line: tile it with the buffer's tree-sitter
/// highlight spans, then let sub-line overlay faces override on top (the Emacs
/// overlay model). With no overlays this is exactly the old syntax-only path.
fn line_runs(
    spans: &[HlSpan],
    overlays: &[(std::ops::Range<usize>, SharedString)],
    line: std::ops::Range<usize>,
    font: &Font,
    default: Hsla,
    default_bold: bool,
    theme: &Theme,
) -> Vec<TextRun> {
    // 1. Base segments `(start, end, color, underline, bold)` from syntax
    //    spans. The underline slot is `None` or `Some((wavy, color))` — the
    //    underline carries its own color so a squiggle can be tinted without
    //    recoloring the text under it. `default_bold` styles the face-less
    //    base — a whole-line face is resolved to `default` by the caller, so
    //    its bold flag arrives here.
    type Seg = (usize, usize, Hsla, Option<(bool, Hsla)>, bool);
    let mut segs: Vec<Seg> = Vec::new();
    let mut covered = line.start;
    for span in spans {
        if span.range.end <= line.start {
            continue;
        }
        if span.range.start >= line.end {
            break;
        }
        let start = span.range.start.max(line.start);
        let end = span.range.end.min(line.end);
        if start > covered {
            segs.push((covered, start, default, None, default_bold));
        }
        let (color, ul, bold) = span.face.map_or((default, None, default_bold), |i| {
            let name = face_name(i);
            let color = theme.color(name, default);
            (
                color,
                theme.underline(name).map(|wavy| (wavy, color)),
                theme.is_bold(name),
            )
        });
        segs.push((start, end, color, ul, bold));
        covered = end;
    }
    if covered < line.end {
        segs.push((covered, line.end, default, None, default_bold));
    }
    if segs.is_empty() {
        segs.push((line.start, line.end, default, None, default_bold));
    }

    // 2. Overlay faces override their sub-range, splitting base segments. An
    //    overlay face may also carry underline (e.g. web-reader link markers)
    //    or bold. A WAVY overlay face colors only its squiggle (spellcheck
    //    style: the word keeps the color it already had); a straight-underline
    //    or plain face recolors the text as before.
    for (range, face) in overlays {
        let os = range.start.max(line.start);
        let oe = range.end.min(line.end);
        if os >= oe {
            continue;
        }
        let fcolor = theme.color(face, default);
        let ul = theme.underline(face).map(|wavy| (wavy, fcolor));
        let wavy_only = matches!(ul, Some((true, _)));
        let bold = theme.is_bold(face);
        let mut next = Vec::with_capacity(segs.len() + 2);
        for (s, e, c, u, b) in segs.drain(..) {
            if e <= os || s >= oe {
                next.push((s, e, c, u, b));
                continue;
            }
            if s < os {
                next.push((s, os, c, u, b));
            }
            let text_color = if wavy_only { c } else { fcolor };
            next.push((s.max(os), e.min(oe), text_color, ul, bold));
            if e > oe {
                next.push((oe, e, c, u, b));
            }
        }
        segs = next;
    }

    // 3. Emit runs (skip any empty segment), underlining/bolding where the
    //    face asked.
    segs.into_iter()
        .filter(|(s, e, _, _, _)| e > s)
        .map(|(s, e, color, ul, bold)| {
            let mut run = plain_run(e - s, font.clone(), color);
            if let Some((wavy, ul_color)) = ul {
                run.underline = Some(UnderlineStyle {
                    thickness: px(1.),
                    color: Some(ul_color),
                    wavy,
                });
            }
            if bold {
                run.font.weight = FontWeight::BOLD;
            }
            run
        })
        .collect()
}

/// The custom GPUI element that draws an embedded terminal grid. Like
/// [`EditorElement`] it shapes and paints text itself, but over a fixed
/// `cols × rows` cell grid read from the alacritty backend (via a plain-data
/// [`TermSnapshot`](crate::terminal::TermSnapshot)) rather than a rope.
pub struct TerminalElement {
    pub editor: Entity<Editor>,
    pub pane: usize,
    /// The backing terminal id.
    pub id: u64,
}

/// What `TerminalElement::prepaint` hands to `paint`: background fills, shaped
/// glyph rows, and the cursor quad.
pub struct TerminalPrepaint {
    background: PaintQuad,
    cell_backgrounds: Vec<PaintQuad>,
    /// Selection highlight quads, painted over cell backgrounds, under glyphs.
    selection: Vec<PaintQuad>,
    rows: Vec<(ShapedLine, Point<Pixels>)>,
    cursor: Option<PaintQuad>,
}

impl IntoElement for TerminalElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = TerminalPrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let theme = cx.global::<Theme>();
        // Terminal default colors: dedicated `terminal-*` faces, else the editor's
        // foreground/background.
        let default_fg = theme.color("terminal-foreground", theme.foreground());
        let default_bg = theme.color("terminal-background", theme.background());
        let cursor_color = theme.color("cursor", rgb(0x528bff).into());
        let selection_color = theme.color(
            "terminal-selection",
            theme.color("region", rgba(0x3a5fcd55).into()),
        );
        let dfg = hsla_to_rgb_u32(default_fg);
        let dbg = hsla_to_rgb_u32(default_bg);

        let settings = cx.global::<crate::settings::Settings>();
        let cursor_width = px(settings.cursor_width());

        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();
        let term_font = font(settings.font_family().to_string());
        let mk_run = |len: usize, color: Hsla| plain_run(len, term_font.clone(), color);
        // One monospace cell advance.
        let em = window
            .text_system()
            .shape_line(
                SharedString::from("0"),
                font_size,
                &[mk_run(1, default_fg)],
                None,
            )
            .x_for_index(1);

        // Cell grid that fits the viewport, and re-fit the backend to it.
        let cols = if em > px(0.) {
            (bounds.size.width / em) as usize
        } else {
            0
        };
        let rows = if line_height > px(0.) {
            (bounds.size.height / line_height) as usize
        } else {
            0
        };
        let id = self.id;
        self.editor.update(cx, |editor, _| {
            editor.terminal_resize_fit(
                id,
                cols.max(1),
                rows.max(1),
                f32::from(em).max(1.0) as u16,
                f32::from(line_height).max(1.0) as u16,
            );
        });

        let snapshot = self.editor.read(cx).terminal_snapshot(id, dfg, dbg);
        let background = fill(bounds, default_bg);
        let mut cell_backgrounds = Vec::new();
        let mut selection: Vec<PaintQuad> = Vec::new();
        let mut shaped_rows = Vec::new();
        let mut cursor = None;

        if let Some(snapshot) = snapshot {
            // Reassemble the sparse cell list into dense rows so column positions
            // line up (gaps are spaces over the default background).
            let cols = snapshot.cols;
            let rows = snapshot.rows;
            // Single flat grid (len rows*cols), indexed `r*cols + c`, instead of a
            // per-row Vec to avoid the per-row allocation each frame.
            let mut grid: Vec<Option<&crate::terminal::TermCell>> = vec![None; rows * cols];
            for cell in &snapshot.cells {
                if cell.row < rows && cell.col < cols {
                    grid[cell.row * cols + cell.col] = Some(cell);
                }
            }

            for (r, row_cells) in grid.chunks(cols).enumerate() {
                let y = bounds.top() + line_height * r as f32;
                // Background fills first.
                for (c, cell) in row_cells.iter().enumerate() {
                    if let Some(cell) = cell
                        && let Some(bg) = cell.bg
                    {
                        let x = bounds.left() + em * c as f32;
                        cell_backgrounds.push(fill(
                            Bounds::new(point(x, y), size(em, line_height)),
                            Hsla::from(rgb(bg)),
                        ));
                    }
                }
                // Build the row string + per-cell styled runs.
                let mut text = String::with_capacity(cols);
                let mut runs: Vec<TextRun> = Vec::with_capacity(cols);
                for cell in row_cells {
                    let (ch, color, bold, italic, underline) = match cell {
                        Some(c) => {
                            let ch = if c.ch == '\0' { ' ' } else { c.ch };
                            (ch, Hsla::from(rgb(c.fg)), c.bold, c.italic, c.underline)
                        }
                        None => (' ', default_fg, false, false, false),
                    };
                    text.push(ch);
                    let mut f = term_font.clone();
                    if bold {
                        f.weight = FontWeight::BOLD;
                    }
                    if italic {
                        f.style = FontStyle::Italic;
                    }
                    runs.push(TextRun {
                        len: ch.len_utf8(),
                        font: f,
                        color,
                        background_color: None,
                        underline: underline.then(|| UnderlineStyle {
                            thickness: px(1.),
                            color: Some(color),
                            wavy: false,
                        }),
                        strikethrough: None,
                    });
                }
                let shaped = window.text_system().shape_line(
                    SharedString::from(text),
                    font_size,
                    &runs,
                    None,
                );
                shaped_rows.push((shaped, point(bounds.left(), y)));
            }

            // Selection highlight: one quad per visible row the range covers.
            if let Some(sel) = &snapshot.selection {
                let first = sel.start_row.max(0) as usize;
                let last = (sel.end_row.max(0) as usize).min(rows.saturating_sub(1));
                for r in first..=last.min(rows.saturating_sub(1)) {
                    if r >= rows {
                        break;
                    }
                    // Column span for this row (block = same cols every row; simple =
                    // start..eol on the first row, full middle rows, 0..end on the last).
                    let (c0, c1) = if sel.is_block {
                        (sel.start_col, sel.end_col)
                    } else {
                        let c0 = if sel.start_row == r as i32 {
                            sel.start_col
                        } else {
                            0
                        };
                        let c1 = if sel.end_row == r as i32 {
                            sel.end_col
                        } else {
                            cols.saturating_sub(1)
                        };
                        (c0, c1)
                    };
                    if c1 < c0 {
                        continue;
                    }
                    let x0 = bounds.left() + em * c0 as f32;
                    let x1 = bounds.left() + em * (c1 + 1) as f32;
                    let y = bounds.top() + line_height * r as f32;
                    selection.push(fill(
                        Bounds::from_corners(point(x0, y), point(x1, y + line_height)),
                        selection_color,
                    ));
                }
            }

            if let Some(cur) = snapshot.cursor {
                let x = bounds.left() + em * cur.col as f32;
                let y = bounds.top() + line_height * cur.row as f32;
                let (w, mut color) = if cur.block {
                    (em, cursor_color)
                } else {
                    (cursor_width, cursor_color)
                };
                // A block cursor is translucent so the glyph under it stays legible.
                if cur.block {
                    color.a *= BLOCK_CURSOR_ALPHA;
                }
                cursor = Some(fill(Bounds::new(point(x, y), size(w, line_height)), color));
            }
        }

        // Report the painted geometry so the editor can map mouse positions to
        // grid cells for selection.
        self.editor.update(cx, |editor, _| {
            editor.set_terminal_geometry(
                id,
                point(bounds.left(), bounds.top()),
                em,
                line_height,
                cols.max(1),
                rows.max(1),
            );
        });

        TerminalPrepaint {
            background,
            cell_backgrounds,
            selection,
            rows: shaped_rows,
            cursor,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.editor.read(cx).focus_handle.clone();

        // Claim the IME/typed-text input handler for the selected terminal pane,
        // just like `EditorElement`. Live terminal keys are grabbed in
        // `on_key_down` (which sends PTY bytes and stops propagation before the
        // IME), so this only matters when an overlay is open over the terminal —
        // e.g. helm/minibuffer, which let plain text fall through to the IME.
        // Without a handler here that text would be dropped, so typing to filter
        // M-x wouldn't work after the shell exits and the terminal is still shown.
        if self.editor.read(cx).pane_is_selected(self.pane) {
            window.handle_input(
                &focus_handle,
                ElementInputHandler::new(bounds, self.editor.clone()),
                cx,
            );
        }

        let line_height = window.line_height();
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            window.paint_quad(prepaint.background.clone());
            for quad in prepaint.cell_backgrounds.drain(..) {
                window.paint_quad(quad);
            }
            for quad in prepaint.selection.drain(..) {
                window.paint_quad(quad);
            }
            if focus_handle.is_focused(window)
                && let Some(cursor) = prepaint.cursor.take()
            {
                window.paint_quad(cursor);
            }
            for (line, origin) in prepaint.rows.drain(..) {
                line.paint(origin, line_height, window, cx).ok();
            }
        });
        // Report bounds so click-to-focus routes to this pane like any other.
        let pane = self.pane;
        self.editor.update(cx, |editor, _| {
            editor.report_pane_bounds(pane, bounds);
        });
    }
}

/// Pack a [`Hsla`] into a `0xRRGGBB` value for the terminal color resolver.
fn hsla_to_rgb_u32(color: Hsla) -> u32 {
    let rgba = gpui::Rgba::from(color);
    let to_u8 = |v: f32| (v.clamp(0., 1.) * 255.0).round() as u32;
    (to_u8(rgba.r) << 16) | (to_u8(rgba.g) << 8) | to_u8(rgba.b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_face_overrides_subrange() {
        let mut theme = Theme::default();
        let red: Hsla = rgb(0xff0000).into();
        theme.set("hl", red);
        let font = font("Lilex");
        let default: Hsla = rgb(0x000000).into();
        // Line [0,10), no syntax spans, one overlay [2,5) with face "hl".
        let overlays = vec![(2usize..5usize, SharedString::from("hl"))];
        let runs = line_runs(&[], &overlays, 0..10, &font, default, false, &theme);
        assert_eq!(
            runs.iter().map(|r| r.len).collect::<Vec<_>>(),
            vec![2, 3, 5]
        );
        assert_eq!(runs[1].color, red, "overlay colors its sub-range");
        assert_eq!(runs[0].color, default);
        assert_eq!(runs[2].color, default);
    }

    #[test]
    fn overlay_face_underlines_when_set() {
        let mut theme = Theme::default();
        theme.set("link", rgb(0x4ea1f3).into());
        theme.set_underline("link", true);
        let font = font("Lilex");
        let default: Hsla = rgb(0x000000).into();
        // One overlay [2,5) with an underlined face; the rest of the line is not.
        let overlays = vec![(2usize..5usize, SharedString::from("link"))];
        let runs = line_runs(&[], &overlays, 0..10, &font, default, false, &theme);
        assert!(
            runs[1].underline.is_some(),
            "underlined face underlines its run"
        );
        assert!(runs[0].underline.is_none());
        assert!(runs[2].underline.is_none());
    }

    #[test]
    fn wavy_face_colors_only_the_squiggle_not_the_text() {
        let red: Hsla = rgb(0xff0000).into();
        let mut theme = Theme::default();
        theme.set("spell-error", red);
        theme.set_wavy("spell-error", true);
        let font = font("Lilex");
        let default: Hsla = rgb(0x000000).into();
        let overlays = vec![(2usize..5usize, SharedString::from("spell-error"))];
        let runs = line_runs(&[], &overlays, 0..10, &font, default, false, &theme);
        let ul = runs[1].underline.as_ref().expect("wavy implies underline");
        assert!(ul.wavy, "wavy face squiggles its run");
        assert_eq!(ul.color, Some(red), "the squiggle takes the face color");
        assert_eq!(
            runs[1].color, default,
            "the text keeps its color — only the squiggle is red"
        );
        assert!(runs[0].underline.is_none());
        assert!(runs[2].underline.is_none());
    }

    #[test]
    fn no_overlays_tiles_whole_line() {
        let theme = Theme::default();
        let font = font("Lilex");
        let default: Hsla = rgb(0x111111).into();
        let runs = line_runs(&[], &[], 0..7, &font, default, false, &theme);
        assert_eq!(runs.iter().map(|r| r.len).sum::<usize>(), 7);
    }

    #[test]
    fn ghost_display_shift_and_inverse() {
        let ghost = Some((4usize, 3usize)); // 3 ghost bytes injected at index 4
        // Point itself (== insertion index) does NOT shift: the cursor renders
        // before the ghost.
        assert_eq!(ghost_to_display(4, ghost), 4);
        // Everything after point shifts right by the ghost length.
        assert_eq!(ghost_to_display(5, ghost), 8);
        assert_eq!(ghost_to_display(3, ghost), 3);
        assert_eq!(ghost_to_display(9, None), 9);
        // Inverse: real positions round-trip…
        for rel in [0, 3, 4, 5, 9] {
            assert_eq!(ghost_from_display(ghost_to_display(rel, ghost), ghost), rel);
        }
        // …and a display index inside the ghost (a click on the suggestion)
        // collapses to the insertion point.
        assert_eq!(ghost_from_display(5, ghost), 4);
        assert_eq!(ghost_from_display(7, ghost), 4);
    }

    #[test]
    fn inject_ghost_run_splits_covering_run() {
        let font = font("Lilex");
        let c: Hsla = rgb(0x111111).into();
        let g: Hsla = rgb(0x888888).into();
        let ghost = plain_run(3, font.clone(), g);
        // Mid-run: [6] at index 4 → [4, 3(ghost), 2].
        let runs = inject_ghost_run(vec![plain_run(6, font.clone(), c)], 4, ghost.clone());
        assert_eq!(
            runs.iter().map(|r| r.len).collect::<Vec<_>>(),
            vec![4, 3, 2]
        );
        assert_eq!(runs[1].color, g);
        // On a run boundary: [2, 4] at index 2 → the ghost slots between them.
        let runs = inject_ghost_run(
            vec![plain_run(2, font.clone(), c), plain_run(4, font.clone(), c)],
            2,
            ghost.clone(),
        );
        assert_eq!(
            runs.iter().map(|r| r.len).collect::<Vec<_>>(),
            vec![2, 3, 4]
        );
        assert_eq!(runs[1].color, g);
        // At the start and past the end (EOL), and on an empty line.
        let runs = inject_ghost_run(vec![plain_run(5, font.clone(), c)], 0, ghost.clone());
        assert_eq!(runs.iter().map(|r| r.len).collect::<Vec<_>>(), vec![3, 5]);
        let runs = inject_ghost_run(vec![plain_run(5, font.clone(), c)], 5, ghost.clone());
        assert_eq!(runs.iter().map(|r| r.len).collect::<Vec<_>>(), vec![5, 3]);
        let runs = inject_ghost_run(Vec::new(), 0, ghost);
        assert_eq!(runs.iter().map(|r| r.len).collect::<Vec<_>>(), vec![3]);
    }

    #[test]
    fn bold_face_bolds_runs_without_needing_a_color() {
        let mut theme = Theme::default();
        theme.set_bold("feeds-unread", true); // NO color entry on purpose
        let font = font("Lilex");
        let default: Hsla = rgb(0x222222).into();
        // Whole-line-face path: caller resolves the face to `default` (fallback
        // — the face has no color) and passes its bold flag as the base.
        let runs = line_runs(
            &[],
            &[],
            0..7,
            &font,
            theme.color("feeds-unread", default),
            theme.is_bold("feeds-unread"),
            &theme,
        );
        assert_eq!(runs[0].font.weight, FontWeight::BOLD);
        assert_eq!(runs[0].color, default, "colorless bold face keeps default");
        // Overlay path: a bold overlay bolds only its sub-range.
        let overlays = vec![(2usize..5usize, SharedString::from("feeds-unread"))];
        let runs = line_runs(&[], &overlays, 0..10, &font, default, false, &theme);
        assert_eq!(runs[0].font.weight, FontWeight::NORMAL);
        assert_eq!(runs[1].font.weight, FontWeight::BOLD);
        assert_eq!(runs[2].font.weight, FontWeight::NORMAL);
    }

    #[test]
    fn hidden_range_maps_are_inverse() {
        // "*** Title": collapse the first two stars (bytes [0,2)).
        let holes = [(0usize, 2usize)];
        // Buffer offset -> display index: hidden bytes map to the hole start (0),
        // the surviving star (offset 2) lands at display 0, then 1:1.
        assert_eq!(to_display(0, &holes), 0);
        assert_eq!(to_display(1, &holes), 0);
        assert_eq!(to_display(2, &holes), 0); // surviving '*'
        assert_eq!(to_display(3, &holes), 1); // ' '
        assert_eq!(to_display(4, &holes), 2); // 'T'
        // Inverse lands just past the hole, and round-trips for visible offsets.
        assert_eq!(from_display(0, &holes), 2);
        assert_eq!(from_display(1, &holes), 3);
        assert_eq!(from_display(2, &holes), 4);
        for rel in 2..=8 {
            assert_eq!(from_display(to_display(rel, &holes), &holes), rel);
        }
        // Identity when there are no holes.
        assert_eq!(to_display(5, &[]), 5);
        assert_eq!(from_display(5, &[]), 5);
    }

    #[test]
    fn hidden_range_splices_text_and_runs() {
        assert_eq!(remove_holes("*** Title", &[(0, 2)]), "* Title");
        assert_eq!(remove_holes("no holes", &[]), "no holes");
        // Two disjoint holes.
        assert_eq!(remove_holes("abcdef", &[(1, 2), (4, 5)]), "acdf");

        let font = font("Lilex");
        let c: Hsla = rgb(0).into();
        // One run of len 9 ("*** Title"), collapse [0,2) -> run shrinks to 7.
        let runs = punch_runs(vec![plain_run(9, font.clone(), c)], &[(0, 2)]);
        assert_eq!(runs.iter().map(|r| r.len).collect::<Vec<_>>(), vec![7]);
        // A run fully inside a hole is dropped.
        let runs = punch_runs(
            vec![plain_run(2, font.clone(), c), plain_run(5, font.clone(), c)],
            &[(0, 2)],
        );
        assert_eq!(runs.iter().map(|r| r.len).collect::<Vec<_>>(), vec![5]);
    }
}
