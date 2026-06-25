//! Window/pane (split) methods for [`Editor`].

use super::*;

impl Editor {
    pub(super) fn pane(&self, id: usize) -> Option<&Pane> {
        self.panes.iter().find(|p| p.id == id)
    }

    /// Resolve a per-buffer value for pane `id`: the live value (via `live`) for
    /// the selected/active buffer, else the pane's inactive [`Document`] (via
    /// `pick`), falling back to `live` if the document can't be found. Works for
    /// both borrowed (`&T`) and optional (`Option<&T>`) results.
    fn pane_field<'a, R>(
        &'a self,
        id: usize,
        live: impl Fn() -> R,
        pick: impl Fn(&'a Document) -> R,
    ) -> R {
        match self.pane(id) {
            Some(p) if p.buffer != self.active_name => self
                .inactive
                .iter()
                .find(|d| d.name == p.buffer)
                .map_or_else(live, pick),
            _ => live(),
        }
    }

    /// Number of panes — the element renders one per leaf.
    pub fn pane_is_selected(&self, id: usize) -> bool {
        id == self.selected_pane
    }

    /// The buffer a pane displays: the live active buffer for the selected pane
    /// (and any pane showing the active buffer), else the inactive [`Document`].
    pub fn pane_buffer(&self, id: usize) -> &Buffer {
        self.pane_field(id, || &self.buffer, |d| &d.buffer)
    }

    /// The pane's major (topmost) mode name, resolved like [`pane_buffer`] so an
    /// inactive pane reports its own buffer's mode. Drives per-major-mode display
    /// options such as line numbers (`Self::line_numbers_for`).
    pub fn pane_major_mode(&self, id: usize) -> Option<&str> {
        self.pane_field(id, || &self.modes, |d| &d.modes)
            .names()
            .last()
            .copied()
    }

    pub fn pane_point(&self, id: usize) -> usize {
        if id == self.selected_pane {
            self.cursor_offset()
        } else {
            self.pane(id).map_or(0, |p| p.point)
        }
    }

    pub fn pane_selection(&self, id: usize) -> Range<usize> {
        if id == self.selected_pane {
            // Inclusive of the grapheme under the block cursor in evil Visual.
            self.visual_region()
        } else {
            0..0
        }
    }

    pub fn pane_scroll(&self, id: usize) -> Pixels {
        if id == self.selected_pane {
            self.scroll_offset
        } else {
            self.pane(id).map_or(px(0.), |p| p.scroll)
        }
    }

    pub fn pane_highlights(&self, id: usize, range: Range<usize>) -> Vec<HlSpan> {
        let highlighter =
            self.pane_field(id, || self.highlighter.as_ref(), |d| d.highlighter.as_ref());
        match highlighter {
            Some(h) => h.spans_in(self.pane_buffer(id).rope(), range),
            None => Vec::new(),
        }
    }

    /// Virtual suffix text + face for `line` (selected/active buffer only).
    pub fn line_suffix(&self, line: usize) -> Option<(&str, &str)> {
        self.overlays
            .suffixes
            .get(&line)
            .map(|(t, f)| (t.as_ref(), f.as_ref()))
    }

    /// Whole-line face override for a pane (only the selected/active buffer
    /// carries Scheme-set line faces today).
    pub fn pane_line_face(&self, id: usize, line: usize) -> Option<&str> {
        self.pane_line_faces(id)
            .get(&line)
            .map(SharedString::as_ref)
    }

    /// The line-face map for pane `id` — the live one if selected, else the
    /// pane's inactive document (so a special buffer keeps its colors when it's
    /// not the focused window — e.g. a tree sidebar).
    pub(super) fn pane_line_faces(&self, id: usize) -> &HashMap<usize, SharedString> {
        self.pane_field(id, || &self.line_faces, |d| &d.line_faces)
    }

    /// Per-line icon `(char_col, asset_name)` for pane `id`, if any — same
    /// active/inactive resolution as the faces.
    pub fn pane_line_icon(&self, id: usize, line: usize) -> Option<(usize, &str)> {
        let icons = self.pane_field(id, || &self.line_icons, |d| &d.line_icons);
        icons.get(&line).map(|(col, name)| (*col, name.as_ref()))
    }

    pub fn pane_diagnostics(&self, id: usize) -> Vec<Diagnostic> {
        if id == self.selected_pane {
            self.active_diagnostics()
        } else {
            Vec::new()
        }
    }

    /// Record a pane's painted bounds (called by every pane's element each
    /// frame) so clicks can be routed to the pane under the cursor.
    pub fn report_pane_bounds(&mut self, id: usize, bounds: Bounds<Pixels>) {
        match self.pane_bounds.iter_mut().find(|(pid, _)| *pid == id) {
            Some(entry) => entry.1 = bounds,
            None => self.pane_bounds.push((id, bounds)),
        }
    }

    /// The pane whose painted bounds contain `pos` (existing panes only).
    pub(super) fn pane_at_position(&self, pos: Point<Pixels>) -> Option<usize> {
        self.pane_bounds
            .iter()
            .find(|(id, b)| b.contains(&pos) && self.panes.iter().any(|p| p.id == *id))
            .map(|(id, _)| *id)
    }

    /// Save the live point/scroll into the selected pane before leaving it.
    pub(super) fn save_selected_pane(&mut self) {
        let (point, scroll) = (self.cursor_offset(), self.scroll_offset);
        let selected = self.selected_pane;
        if let Some(pane) = self.panes.iter_mut().find(|p| p.id == selected) {
            pane.point = point;
            pane.scroll = scroll;
        }
    }

    /// `C-x 2` / `C-x 3`: split the selected window, the new pane cloning the
    /// current buffer/point/scroll.
    pub(super) fn split_window(&mut self, vertical: bool, cx: &mut Context<Self>) {
        self.split_window_ratio(vertical, 0.5, cx);
    }

    /// Like `split_window`, but the existing (selected) pane keeps `ratio` of the
    /// extent — e.g. 0.25 for a narrow sidebar created by `open-file-other-window`.
    pub(super) fn split_window_ratio(
        &mut self,
        vertical: bool,
        ratio: f32,
        cx: &mut Context<Self>,
    ) {
        self.save_selected_pane();
        let new_id = self.next_pane_id;
        self.next_pane_id += 1;
        let buffer = self
            .pane(self.selected_pane)
            .map_or_else(|| self.active_name.clone(), |p| p.buffer.clone());
        self.panes.push(Pane {
            id: new_id,
            buffer,
            point: self.cursor_offset(),
            scroll: self.scroll_offset,
        });
        let layout = std::mem::replace(&mut self.layout, WindowLayout::Leaf(self.selected_pane));
        self.layout = layout_split(layout, self.selected_pane, vertical, new_id, ratio);
        cx.notify();
    }

    /// `C-x o`: select the next pane in cycle order, activating its buffer.
    pub(super) fn other_window(&mut self, cx: &mut Context<Self>) {
        let mut leaves = Vec::new();
        layout_leaves(&self.layout, &mut leaves);
        if leaves.len() < 2 {
            return;
        }
        let pos = leaves
            .iter()
            .position(|&id| id == self.selected_pane)
            .unwrap_or(0);
        let next = leaves[(pos + 1) % leaves.len()];
        self.select_pane(next, cx);
    }

    /// Make pane `id` the selected window: save the old pane, activate the
    /// target's buffer if different, and restore the target's point/scroll.
    pub(super) fn select_pane(&mut self, id: usize, cx: &mut Context<Self>) {
        if id == self.selected_pane {
            return;
        }
        self.save_selected_pane();
        let target_buffer = self.pane(id).map(|p| p.buffer.clone()).unwrap_or_default();
        let restore = self.pane(id).map(|p| (p.point, p.scroll));
        self.selected_pane = id;
        if target_buffer != self.active_name && !target_buffer.is_empty() {
            self.switch_to_buffer(&target_buffer, cx);
        }
        if let Some((point, scroll)) = restore {
            self.move_to(point.min(self.buffer.len()), cx);
            self.scroll_offset = scroll;
        }
        cx.notify();
    }

    /// `C-x 0`: delete the selected window, selecting a neighbor.
    pub(super) fn delete_window(&mut self, cx: &mut Context<Self>) {
        let mut leaves = Vec::new();
        layout_leaves(&self.layout, &mut leaves);
        if leaves.len() < 2 {
            self.echo("Can't delete the sole window", cx);
            return;
        }
        let removed = self.selected_pane;
        let pos = leaves.iter().position(|&id| id == removed).unwrap_or(0);
        let neighbor = leaves[if pos == 0 { 1 } else { pos - 1 }];
        let layout = std::mem::replace(&mut self.layout, WindowLayout::Leaf(neighbor));
        if let Some(l) = layout_remove(layout, removed) {
            self.layout = l;
        }
        self.panes.retain(|p| p.id != removed);
        self.pane_bounds.retain(|(id, _)| *id != removed);
        self.select_pane(neighbor, cx);
    }

    /// `C-x 1`: delete all other windows, keeping the selected one.
    pub(super) fn delete_other_windows(&mut self, cx: &mut Context<Self>) {
        self.save_selected_pane();
        let keep = self.selected_pane;
        self.panes.retain(|p| p.id == keep);
        self.layout = WindowLayout::Leaf(keep);
        cx.notify();
    }

    /// Enlarge/shrink the selected window along an axis (Emacs `enlarge-window`
    /// etc.): nudge the nearest matching split's ratio toward/away from this pane.
    pub(super) fn resize_window(&mut self, vertical: bool, delta: f32, cx: &mut Context<Self>) {
        if layout_resize(&mut self.layout, self.selected_pane, vertical, delta) {
            cx.notify();
        }
    }

    /// Recursively render the split layout into nested flex containers. `path`
    /// (a/b descents from the root) identifies each split so its divider can
    /// start a resize drag.
    pub(super) fn render_layout(
        &self,
        layout: &WindowLayout,
        path: &[bool],
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = cx.global::<Theme>();
        let divider = theme.color("window-divider", theme.color("dim", theme.foreground()));
        let divider_w = px(cx.global::<crate::settings::Settings>().divider_width());
        match layout {
            WindowLayout::Leaf(id) => {
                // A terminal pane draws the grid via `TerminalElement`; everything
                // else uses the text `EditorElement`.
                let content = if let Some(tid) = self.pane_terminal_id(*id) {
                    crate::element::TerminalElement {
                        editor: cx.entity(),
                        pane: *id,
                        id: tid,
                    }
                    .into_any_element()
                } else {
                    EditorElement {
                        editor: cx.entity(),
                        pane: *id,
                    }
                    .into_any_element()
                };
                div().size_full().child(content).into_any_element()
            }
            WindowLayout::Split {
                vertical,
                ratio,
                a,
                b,
            } => {
                let vertical = *vertical;
                let a = self.render_layout(a, &[path, &[false]].concat(), cx);
                let b = self.render_layout(b, &[path, &[true]].concat(), cx);
                // A grabbable divider (4px hit area) that begins a drag-resize of
                // this split; the editor's mouse-move then tracks the ratio.
                let drag_path = path.to_vec();
                let divider_bar = {
                    let bar = if vertical {
                        div()
                            .h(divider_w)
                            .w_full()
                            .cursor(CursorStyle::ResizeUpDown)
                    } else {
                        div()
                            .w(divider_w)
                            .h_full()
                            .cursor(CursorStyle::ResizeLeftRight)
                    };
                    bar.bg(divider).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, _window, cx| {
                            this.resizing = Some(ResizeDrag {
                                path: drag_path.clone(),
                                vertical,
                            });
                            cx.stop_propagation();
                        }),
                    )
                };
                let container = if vertical {
                    div().flex().flex_col()
                } else {
                    div().flex().flex_row()
                };
                // Child `a` takes `ratio` of the extent; `b` fills the remainder.
                let first = if vertical {
                    div().w_full().h(gpui::relative(*ratio))
                } else {
                    div().h_full().w(gpui::relative(*ratio))
                };
                container
                    .size_full()
                    .child(first.min_w(px(0.)).min_h(px(0.)).child(a))
                    .child(divider_bar)
                    .child(div().flex_1().min_w(px(0.)).min_h(px(0.)).child(b))
                    .into_any_element()
            }
        }
    }

    /// Handle a non-text key (Enter/Tab/Backspace/Delete) in a special buffer:
    /// resolve it against the buffer's keymap and run any binding. Returns
    /// whether the active buffer is special (so callers skip the edit fallback).
    pub(super) fn special_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        if self.special.is_none() {
            return false;
        }
        if let Some(expr) = self.keymap.resolve(&self.modes, key).map(str::to_string) {
            self.run_script(&expr, cx);
        }
        true // read-only: the key is consumed whether or not it was bound
    }

    /// In a NORMAL (editable) buffer, let an active mode rebind a special key
    /// (Enter/Tab) — Emacs's major modes do this (comint RET, etc.). Returns
    /// whether a binding ran; if not, the caller does the default edit.
    pub(super) fn mode_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        match self.keymap.resolve(&self.modes, key).map(str::to_string) {
            Some(expr) => {
                self.run_script(&expr, cx);
                true
            }
            None => false,
        }
    }
}
