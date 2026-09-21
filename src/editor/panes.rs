//! Window/pane (split) methods for [`Editor`], plus the split-layout tree
//! ([`WindowLayout`]) and its free helper functions.

use super::*;

/// The split layout: a binary tree whose leaves are pane ids. `ratio` is the
/// fraction of the split's extent given to child `a` (the other gets `1 - ratio`);
/// resize commands nudge it.
pub(super) enum WindowLayout {
    Leaf(usize),
    Split {
        /// `true` = stacked (split below), `false` = side by side (split right).
        vertical: bool,
        /// Fraction `[0.1, 0.9]` of the area given to `a`.
        ratio: f32,
        a: Box<WindowLayout>,
        b: Box<WindowLayout>,
    },
}

/// Whether `layout`'s subtree contains pane `id`.
pub(super) fn subtree_contains(layout: &WindowLayout, id: usize) -> bool {
    let mut leaves = Vec::new();
    layout_leaves(layout, &mut leaves);
    leaves.contains(&id)
}

/// Nudge the ratio of the deepest `vertical`-matching split on the path to
/// `pane`, enlarging the side that contains it by `delta`. Returns whether a
/// matching split was found.
pub(super) fn layout_resize(
    layout: &mut WindowLayout,
    pane: usize,
    vertical: bool,
    delta: f32,
) -> bool {
    let WindowLayout::Split {
        vertical: v,
        ratio,
        a,
        b,
    } = layout
    else {
        return false;
    };
    let in_a = subtree_contains(a, pane);
    let child = if in_a { a } else { b };
    // Prefer a deeper matching split; otherwise adjust this one.
    if layout_resize(child, pane, vertical, delta) {
        return true;
    }
    if *v == vertical {
        let signed = if in_a { delta } else { -delta };
        *ratio = (*ratio + signed).clamp(0.1, 0.9);
        true
    } else {
        false
    }
}

/// The subtree at `path` (a sequence of `false`=a / `true`=b descents from the
/// root), or `None` if the path doesn't lead to a node.
pub(super) fn node_at<'a>(layout: &'a WindowLayout, path: &[bool]) -> Option<&'a WindowLayout> {
    match path.split_first() {
        None => Some(layout),
        Some((&go_b, rest)) => match layout {
            WindowLayout::Split { a, b, .. } => node_at(if go_b { b } else { a }, rest),
            WindowLayout::Leaf(_) => None,
        },
    }
}

/// Set the ratio of the Split node at `path` (clamped). Returns whether it hit a
/// Split there.
pub(super) fn set_ratio_at(layout: &mut WindowLayout, path: &[bool], new_ratio: f32) -> bool {
    match path.split_first() {
        None => match layout {
            WindowLayout::Split { ratio, .. } => {
                *ratio = new_ratio.clamp(0.1, 0.9);
                true
            }
            WindowLayout::Leaf(_) => false,
        },
        Some((&go_b, rest)) => match layout {
            WindowLayout::Split { a, b, .. } => {
                set_ratio_at(if go_b { b } else { a }, rest, new_ratio)
            }
            WindowLayout::Leaf(_) => false,
        },
    }
}

/// Bounding box of a set of rects, or `None` if empty.
pub(super) fn union_bounds(rects: &[Bounds<Pixels>]) -> Option<Bounds<Pixels>> {
    let first = *rects.first()?;
    let mut left = first.left();
    let mut top = first.top();
    let mut right = first.right();
    let mut bottom = first.bottom();
    for r in &rects[1..] {
        left = left.min(r.left());
        top = top.min(r.top());
        right = right.max(r.right());
        bottom = bottom.max(r.bottom());
    }
    Some(Bounds::from_corners(point(left, top), point(right, bottom)))
}

/// In-order list of pane ids (left-to-right / top-to-bottom) — the `other-window`
/// cycle order.
pub(super) fn layout_leaves(layout: &WindowLayout, out: &mut Vec<usize>) {
    match layout {
        WindowLayout::Leaf(id) => out.push(*id),
        WindowLayout::Split { a, b, .. } => {
            layout_leaves(a, out);
            layout_leaves(b, out);
        }
    }
}

/// Replace the leaf for `target` with a split of `target` (ratio `split_ratio`)
/// and `new_id`.
pub(super) fn layout_split(
    layout: WindowLayout,
    target: usize,
    vertical: bool,
    new_id: usize,
    split_ratio: f32,
) -> WindowLayout {
    match layout {
        WindowLayout::Leaf(id) if id == target => WindowLayout::Split {
            vertical,
            ratio: split_ratio,
            a: Box::new(WindowLayout::Leaf(id)),
            b: Box::new(WindowLayout::Leaf(new_id)),
        },
        WindowLayout::Leaf(id) => WindowLayout::Leaf(id),
        WindowLayout::Split {
            vertical: v,
            ratio,
            a,
            b,
        } => WindowLayout::Split {
            vertical: v,
            ratio,
            a: Box::new(layout_split(*a, target, vertical, new_id, split_ratio)),
            b: Box::new(layout_split(*b, target, vertical, new_id, split_ratio)),
        },
    }
}

/// Remove the leaf `id`, collapsing its parent split to the sibling. `None` if
/// `id` was the whole layout.
pub(super) fn layout_remove(layout: WindowLayout, id: usize) -> Option<WindowLayout> {
    match layout {
        WindowLayout::Leaf(x) if x == id => None,
        WindowLayout::Leaf(x) => Some(WindowLayout::Leaf(x)),
        WindowLayout::Split {
            vertical,
            ratio,
            a,
            b,
        } => match (layout_remove(*a, id), layout_remove(*b, id)) {
            (Some(a), Some(b)) => Some(WindowLayout::Split {
                vertical,
                ratio,
                a: Box::new(a),
                b: Box::new(b),
            }),
            (Some(only), None) | (None, Some(only)) => Some(only),
            (None, None) => None,
        },
    }
}

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
            Some(p) if p.buffer != self.active.name => self
                .inactive
                .iter()
                .find(|d| d.name == p.buffer)
                .map_or_else(live, pick),
            _ => live(),
        }
    }

    /// Whether pane `id` is the selected window (where editing/cursor live).
    pub fn pane_is_selected(&self, id: usize) -> bool {
        id == self.selected_pane
    }

    /// The buffer a pane displays: the live active buffer for the selected pane
    /// (and any pane showing the active buffer), else the inactive [`Document`].
    pub fn pane_buffer(&self, id: usize) -> &Buffer {
        self.pane_field(id, || &self.active.buffer, |d| &d.buffer)
    }

    /// The pane's major (topmost) mode name, resolved like [`pane_buffer`] so an
    /// inactive pane reports its own buffer's mode. Drives per-major-mode display
    /// options such as line numbers (`Self::line_numbers_for`).
    pub fn pane_major_mode(&self, id: usize) -> Option<&str> {
        self.pane_field(id, || &self.active.modes, |d| &d.modes)
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
            // Block-wise Visual draws its own rectangular highlight
            // (`pane_visual_block`), so suppress the contiguous range here.
            if matches!(self.evil, EvilState::VisualBlock) {
                return 0..0;
            }
            // Inclusive of the grapheme under the block cursor in evil Visual.
            self.visual_region()
        } else {
            0..0
        }
    }

    pub fn pane_scroll(&self, id: usize) -> Pixels {
        if id == self.selected_pane {
            self.active.scroll_offset
        } else {
            self.pane(id).map_or(px(0.), |p| p.scroll)
        }
    }

    pub fn pane_highlights(&self, id: usize, range: Range<usize>) -> Vec<HlSpan> {
        let highlighter = self.pane_field(
            id,
            || self.active.highlighter.as_ref(),
            |d| d.highlighter.as_ref(),
        );
        match highlighter {
            Some(h) => h.spans_in(self.pane_buffer(id).rope(), range),
            None => Vec::new(),
        }
    }

    /// Virtual suffix text + face for `line` (selected/active buffer only).
    pub fn line_suffix(&self, line: usize) -> Option<(&str, &str)> {
        self.active
            .overlays
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
        self.pane_field(id, || &self.active.line_faces, |d| &d.line_faces)
    }

    /// Per-line icon `(char_col, asset_name)` for pane `id`, if any — same
    /// active/inactive resolution as the faces.
    pub fn pane_line_icon(&self, id: usize, line: usize) -> Option<(usize, &str)> {
        let icons = self.pane_field(id, || &self.active.line_icons, |d| &d.line_icons);
        icons.get(&line).map(|(col, name)| (*col, name.as_ref()))
    }

    /// Working-tree-vs-HEAD gutter marks for pane `id` (git-gutter / diff-hl),
    /// resolved active/inactive like the line faces so an unfocused window keeps
    /// its file's marks.
    pub fn pane_diff_marks(&self, id: usize) -> &HashMap<usize, DiffMark> {
        self.pane_field(id, || &self.active.diff_marks, |d| &d.diff_marks)
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
        let (point, scroll) = (self.cursor_offset(), self.active.scroll_offset);
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
            .map_or_else(|| self.active.name.clone(), |p| p.buffer.clone());
        self.panes.push(Pane {
            id: new_id,
            buffer,
            point: self.cursor_offset(),
            scroll: self.active.scroll_offset,
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
        if target_buffer != self.active.name && !target_buffer.is_empty() {
            // Pane focus moving is not a deliberate buffer switch: the
            // perspectives title bar must not follow a window dance (e.g.
            // compile re-selecting panes) into another project.
            self.switch_to_buffer_ex(&target_buffer, false, cx);
        }
        if let Some((point, scroll)) = restore {
            self.move_to(point.min(self.active.buffer.len()), cx);
            self.active.scroll_offset = scroll;
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
        if self.active.special.is_none() {
            return false;
        }
        if let Some(expr) = self
            .keymap
            .resolve(&self.active.modes, key)
            .map(str::to_string)
        {
            self.run_script(&expr, cx);
        }
        true // read-only: the key is consumed whether or not it was bound
    }

    /// In a NORMAL (editable) buffer, let an active mode rebind a special key
    /// (Enter/Tab) — Emacs's major modes do this (comint RET, etc.). Returns
    /// whether a binding ran; if not, the caller does the default edit.
    pub(super) fn mode_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        match self
            .keymap
            .resolve(&self.active.modes, key)
            .map(str::to_string)
        {
            Some(expr) => {
                self.run_script(&expr, cx);
                true
            }
            None => false,
        }
    }

    /// Let the active modal keymap claim an OS-special key (Tab, Enter, the
    /// arrows, Home/End, Backspace, Delete).
    ///
    /// Those keys are bound as gpui actions, which dispatch BEFORE the raw
    /// key-down listener, so they never reach [`Editor::on_key_down`] and never
    /// reach the evil/helix keymaps. Without this hook `(define-key
    /// "evil-normal" "tab" …)` is silently dead: it resolves against a keymap
    /// nothing consults. Called first from each `act_*` handler, so a modal
    /// binding wins over the built-in behaviour; unbound falls through
    /// untouched, which keeps every default intact.
    ///
    /// Only consulted while the modal layer is intercepting, so this is inert
    /// under plain Emacs keys.
    pub(super) fn modal_action_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        if !self.evil_intercepts() || self.active.special.is_some() {
            return false;
        }
        let state = self.evil_keymap();
        let major: Option<String> = self.active.modes.names().last().map(|s| s.to_string());
        match self
            .keymap
            .resolve_evil(state, major.as_deref(), key)
            .map(str::to_string)
        {
            Some(expr) => {
                self.dispatch_recorded(&expr, cx);
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        WindowLayout, layout_leaves, layout_remove, layout_resize, layout_split, node_at,
        set_ratio_at, union_bounds,
    };
    use gpui::{Bounds, point, px};

    #[test]
    fn window_layout_resize_nudges_nearest_matching_split() {
        // [0 | 1] horizontal split (ratio 0.5).
        let mut l = layout_split(WindowLayout::Leaf(0), 0, false, 1, 0.5);
        // Enlarge pane 0 horizontally: it's in `a`, so ratio grows.
        assert!(layout_resize(&mut l, 0, false, 0.1));
        if let WindowLayout::Split { ratio, .. } = &l {
            assert!((*ratio - 0.6).abs() < 1e-6, "ratio grew toward pane 0");
        } else {
            panic!("expected split");
        }
        // Enlarging pane 1 (in `b`) shrinks the ratio.
        assert!(layout_resize(&mut l, 1, false, 0.1));
        if let WindowLayout::Split { ratio, .. } = &l {
            assert!((*ratio - 0.5).abs() < 1e-6);
        }
        // A vertical resize finds no vertical split here.
        assert!(!layout_resize(&mut l, 0, true, 0.1));
    }

    #[test]
    fn window_layout_split_leaves_and_remove() {
        // One pane.
        let l = WindowLayout::Leaf(0);
        let mut leaves = Vec::new();
        layout_leaves(&l, &mut leaves);
        assert_eq!(leaves, vec![0]);

        // Split 0 below into [0, 1], then split 1 right into [0, [1, 2]].
        let l = layout_split(l, 0, true, 1, 0.5);
        let l = layout_split(l, 1, false, 2, 0.5);
        let mut leaves = Vec::new();
        layout_leaves(&l, &mut leaves);
        assert_eq!(
            leaves,
            vec![0, 1, 2],
            "in-order traversal is the cycle order"
        );

        // Remove pane 1 — its split collapses to sibling 2.
        let l = layout_remove(l, 1).expect("not the whole layout");
        let mut leaves = Vec::new();
        layout_leaves(&l, &mut leaves);
        assert_eq!(leaves, vec![0, 2]);

        // Removing the last leaf yields None.
        let single = WindowLayout::Leaf(7);
        assert!(layout_remove(single, 7).is_none());
    }

    #[test]
    fn layout_split_ratio_and_path_helpers() {
        // A narrow split: the existing pane keeps 0.22.
        let l = layout_split(WindowLayout::Leaf(0), 0, false, 1, 0.22);
        if let WindowLayout::Split { ratio, .. } = &l {
            assert!((*ratio - 0.22).abs() < 1e-6);
        } else {
            panic!("expected split");
        }
        // Nest: [0, [1, 2]] (split 1 below). The inner split is at path [true].
        let mut l = layout_split(l, 1, true, 2, 0.5);
        // node_at: root is a split; [true] is the inner split; [false] is leaf 0.
        assert!(matches!(node_at(&l, &[]), Some(WindowLayout::Split { .. })));
        assert!(matches!(node_at(&l, &[false]), Some(WindowLayout::Leaf(0))));
        assert!(matches!(
            node_at(&l, &[true]),
            Some(WindowLayout::Split { .. })
        ));
        // set_ratio_at the inner split; clamps to [0.1, 0.9].
        assert!(set_ratio_at(&mut l, &[true], 2.0));
        if let Some(WindowLayout::Split { ratio, .. }) = node_at(&l, &[true]) {
            assert!((*ratio - 0.9).abs() < 1e-6, "clamped to 0.9");
        } else {
            panic!("expected inner split");
        }
        // A path into a leaf isn't a split.
        assert!(!set_ratio_at(&mut l, &[false], 0.5));
    }

    #[test]
    fn union_bounds_covers_all_rects() {
        let a = Bounds::from_corners(point(px(0.), px(0.)), point(px(10.), px(10.)));
        let b = Bounds::from_corners(point(px(20.), px(5.)), point(px(30.), px(40.)));
        let u = union_bounds(&[a, b]).unwrap();
        assert_eq!(u.left(), px(0.));
        assert_eq!(u.top(), px(0.));
        assert_eq!(u.right(), px(30.));
        assert_eq!(u.bottom(), px(40.));
        assert!(union_bounds(&[]).is_none());
    }
}
