//! Overlays, folds, faces, inline images, and line scales for [`Editor`], extracted from the editor module for size.
//! These methods extend the same `Editor` type via an additional
//! `impl` block; cross-module entry points are `pub(super)`.

use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;

use gpui::{Context, RenderImage, SharedString};

use super::*;

impl Editor {
    /// Sub-line face ranges, for the element to layer over syntax highlighting.
    pub fn range_faces(&self) -> &[(Range<usize>, SharedString)] {
        &self.overlays.faces
    }

    /// Hidden ranges (`overlay-hide`) intersecting buffer line `line`, expressed
    /// as `(start, end)` byte offsets relative to the line start, sorted and
    /// merged. The renderer splices these out so the bytes collapse from display.
    pub fn line_holes(&self, line: usize) -> Vec<(usize, usize)> {
        // Out of range for the active buffer → no holes. Defensive: the caller
        // only asks for selected-pane rows (which index this buffer), but a stray
        // line index must never panic the renderer via `line_range`.
        if self.overlays.hidden.is_empty() || line >= self.buffer.line_count() {
            return Vec::new();
        }
        let lr = self.buffer.line_range(line);
        let mut holes: Vec<(usize, usize)> = self
            .overlays
            .hidden
            .iter()
            .filter_map(|r| {
                let s = r.start.max(lr.start);
                let e = r.end.min(lr.end);
                (s < e).then(|| (s - lr.start, e - lr.start))
            })
            .collect();
        if holes.len() > 1 {
            holes.sort_unstable();
            let mut merged: Vec<(usize, usize)> = Vec::with_capacity(holes.len());
            for (s, e) in holes {
                match merged.last_mut() {
                    Some(last) if s <= last.1 => last.1 = last.1.max(e),
                    _ => merged.push((s, e)),
                }
            }
            holes = merged;
        }
        holes
    }

    /// Whether any line is folded (the element's fast path skips fold handling).
    pub fn has_folds(&self) -> bool {
        !self.overlays.folds.is_empty()
    }

    /// Whether buffer line `line` is hidden by a fold.
    pub fn is_line_invisible(&self, line: usize) -> bool {
        self.overlays.folds.iter().any(|r| r.contains(&line))
    }

    /// Whether any gutter fold chevron is set (element's fast path skips them).
    pub fn has_fold_markers(&self) -> bool {
        !self.overlays.fold_markers.is_empty()
    }

    /// The fold chevron state for buffer `line`, if one is set: `Some(true)` for
    /// an open (expanded) chevron, `Some(false)` for a collapsed one.
    pub fn fold_marker_at(&self, line: usize) -> Option<bool> {
        self.overlays.fold_markers.get(&line).copied()
    }

    /// Number of visible (non-folded) lines — the element's screen-row count.
    pub fn display_line_count(&self) -> usize {
        let text = if self.overlays.folds.is_empty() {
            self.buffer.line_count()
        } else {
            (0..self.buffer.line_count())
                .filter(|i| !self.is_line_invisible(*i))
                .count()
        };
        text + self.image_rows_total() + self.scale_rows_total()
    }

    /// Screen row a buffer line maps to once folds are collapsed and inline
    /// images on earlier lines have reserved their rows.
    pub fn display_row_of_line(&self, line: usize) -> usize {
        let text = if self.overlays.folds.is_empty() {
            line
        } else {
            (0..line).filter(|i| !self.is_line_invisible(*i)).count()
        };
        text + self.image_rows_before(line) + self.scale_rows_before(line)
    }

    /// Virtual left indentation (in columns) for buffer `line`, 0 if none.
    /// Display-only — the Org `org-indent-mode` look.
    pub fn line_indent(&self, line: usize) -> usize {
        self.overlays.indents.get(&line).copied().unwrap_or(0)
    }

    /// Whether the active buffer is an Org buffer (set by `org-mode`). The
    /// renderer drops font ligatures here so prose markup like `**` / `***`
    /// doesn't ligate into a single shifted glyph.
    pub fn is_org(&self) -> bool {
        self.locals.get("org-active").is_some_and(|v| v == "t")
    }

    // --- Inline images (Org / LaTeX preview) ------------------------------
    /// Whether the active buffer displays any inline image.
    pub fn has_line_images(&self) -> bool {
        !self.line_images.is_empty()
    }

    /// Decoded image + its displayed (width, height) in px for `line`, or `None`
    /// if the line has no image / it hasn't decoded yet. The width is clamped to
    /// the line's own override (set via `set-line-image`'s optional width arg),
    /// or [`INLINE_IMAGE_MAX_WIDTH`] when the override is 0; height keeps aspect.
    pub(super) fn image_display(&self, line: usize) -> Option<(Arc<RenderImage>, f32, f32)> {
        let (path, override_w) = self.line_images.get(&line)?;
        let img = self.image_cache.get(path)?;
        if img.width == 0 {
            return None;
        }
        let cap = if *override_w > 0 {
            *override_w as f32
        } else {
            INLINE_IMAGE_MAX_WIDTH
        };
        let (w, h) = (img.width as f32, img.height as f32);
        let disp_w = w.min(cap);
        Some((img.data.clone(), disp_w, h * (disp_w / w)))
    }

    /// How many display rows the image on `line` reserves below it (0 if the
    /// line has no image, the image hasn't decoded yet, or the line is folded
    /// away). Width-independent of the window, so folds/scroll stay consistent.
    pub fn image_rows_at(&self, line: usize) -> usize {
        if self.is_line_invisible(line) {
            return 0;
        }
        let lh = f32::from(self.line_height);
        match self.image_display(line) {
            Some((_, _, disp_h)) if lh > 0.0 => (disp_h / lh).ceil().max(1.0) as usize,
            _ => 0,
        }
    }

    /// The decoded image to paint below `line`, with its displayed pixel size.
    pub fn line_image_at(&self, line: usize) -> Option<(Arc<RenderImage>, f32, f32)> {
        self.image_display(line)
    }

    pub(super) fn image_rows_total(&self) -> usize {
        if self.line_images.is_empty() {
            return 0;
        }
        self.line_images
            .keys()
            .map(|&l| self.image_rows_at(l))
            .sum()
    }

    /// Whether any line is scaled (e.g. headings rendered larger).
    pub fn has_line_scales(&self) -> bool {
        !self.overlays.scales.is_empty()
    }

    /// The font-size multiplier for `line` (1.0 = unscaled).
    pub fn line_scale(&self, line: usize) -> f32 {
        self.overlays.scales.get(&line).copied().unwrap_or(1.0)
    }

    /// Extra display rows a scaled line reserves beyond its own — so a heading
    /// taller than one row claims that space, exactly like an inline image. A
    /// scale of S occupies `ceil(S)` rows total, hence `ceil(S) - 1` extra.
    pub fn scale_rows_at(&self, line: usize) -> usize {
        if self.is_line_invisible(line) {
            return 0;
        }
        let s = self.line_scale(line);
        if s > 1.0 {
            (s.ceil() as usize).saturating_sub(1)
        } else {
            0
        }
    }

    pub(super) fn scale_rows_total(&self) -> usize {
        self.overlays
            .scales
            .keys()
            .map(|&l| self.scale_rows_at(l))
            .sum()
    }

    pub(super) fn scale_rows_before(&self, line: usize) -> usize {
        self.overlays
            .scales
            .keys()
            .filter(|&&l| l < line)
            .map(|&l| self.scale_rows_at(l))
            .sum()
    }

    pub(super) fn image_rows_before(&self, line: usize) -> usize {
        if self.line_images.is_empty() {
            return 0;
        }
        self.line_images
            .keys()
            .filter(|&&l| l < line)
            .map(|&l| self.image_rows_at(l))
            .sum()
    }

    /// Record an inline image on `line` and ensure its bitmap is decoded (async,
    /// off the UI thread). Re-uses the cache so a re-scan never re-decodes.
    pub(super) fn set_line_image(
        &mut self,
        line: usize,
        path: String,
        max_width: usize,
        cx: &mut Context<Self>,
    ) {
        let path = PathBuf::from(expand_path(&path));
        self.line_images.insert(line, (path.clone(), max_width));
        if self.image_cache.contains_key(&path) {
            self.ensure_cursor_visible();
            cx.notify();
            return;
        }
        // Decode off the UI thread, then install on the foreground and repaint.
        let decode_path = path.clone();
        let decode = cx
            .background_executor()
            .spawn(async move { load_inline_image(&decode_path) });
        cx.spawn(async move |this, cx| {
            let decoded = decode.await;
            this.update(cx, |editor, cx| {
                if let Some(img) = decoded {
                    editor.image_cache.insert(path, img);
                    editor.ensure_cursor_visible();
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn overlay_set_face(
        &mut self,
        start: usize,
        end: usize,
        face: String,
        cx: &mut Context<Self>,
    ) {
        let len = self.buffer.len();
        let (start, end) = (start.min(len), end.min(len));
        if start < end {
            self.overlays
                .faces
                .push((start..end, SharedString::from(face)));
            cx.notify();
        }
    }

    pub(super) fn overlay_hide(&mut self, start: usize, end: usize, cx: &mut Context<Self>) {
        let len = self.buffer.len();
        let (start, end) = (start.min(len), end.min(len));
        if start < end {
            self.overlays.hidden.push(start..end);
            cx.notify();
        }
    }

    pub(super) fn overlay_fold_lines(
        &mut self,
        start_line: usize,
        end_line: usize,
        cx: &mut Context<Self>,
    ) {
        let last = self.buffer.line_count();
        let (start, end) = (start_line.min(last), end_line.min(last));
        if start < end {
            self.overlays.folds.push(start..end);
            self.ensure_cursor_visible();
            cx.notify();
        }
    }

    pub(super) fn overlay_unfold_all(&mut self, cx: &mut Context<Self>) {
        if !self.overlays.folds.is_empty() {
            self.overlays.folds.clear();
            self.ensure_cursor_visible();
            cx.notify();
        }
    }

    pub(super) fn overlay_set_fold_marker(
        &mut self,
        line: usize,
        open: bool,
        cx: &mut Context<Self>,
    ) {
        self.overlays.fold_markers.insert(line, open);
        cx.notify();
    }

    pub(super) fn overlay_clear_fold_markers(&mut self, cx: &mut Context<Self>) {
        if !self.overlays.fold_markers.is_empty() {
            self.overlays.fold_markers.clear();
            cx.notify();
        }
    }

    pub(super) fn overlay_clear(&mut self, cx: &mut Context<Self>) {
        self.overlays = Overlays::default();
        self.ensure_cursor_visible();
        cx.notify();
    }

    pub(super) fn put_text_property(
        &mut self,
        start: usize,
        end: usize,
        key: String,
        value: String,
    ) {
        let len = self.buffer.len();
        let (start, end) = (start.min(len), end.min(len));
        if start < end {
            self.overlays.props.push((start..end, key, value));
        }
    }

    // --- Async (subprocess streaming, timers) -----------------------------
}
