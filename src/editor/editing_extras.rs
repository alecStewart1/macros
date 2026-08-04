//! Snippets, multiple cursors, and avy hints for [`Editor`], extracted from the editor module for size.
//! These methods extend the same `Editor` type via an additional
//! `impl` block; cross-module entry points are `pub(super)`.

use gpui::{Context, SharedString};

use super::*;

impl Editor {
    /// Expand `template` at point: insert its plain text, create a marker per
    /// tab-stop field, and jump to the first field (selecting its placeholder so
    /// typing overwrites it). Field markers ride along on edits via the marker
    /// store, so navigation stays correct as the user types.
    pub(super) fn snippet_expand(&mut self, template: String, cx: &mut Context<Self>) {
        self.snippet_clear();
        let (text, fields) = parse_snippet(&template);
        let origin = self
            .active
            .selected_range
            .start
            .min(self.active.buffer.len());
        let cursor = self.edit(origin..origin, &text);
        self.snippet_fields = fields
            .iter()
            .map(|&(_, off, len)| (self.script.add_marker(origin + off), len))
            .collect();
        self.snippet_index = 0;
        if self.snippet_fields.is_empty() {
            self.collapse_to(cursor);
        } else {
            self.snippet_goto_field(0);
        }
        self.ensure_cursor_visible();
        cx.notify();
    }

    /// Place point at field `idx`, selecting its placeholder when it has one.
    pub(super) fn snippet_goto_field(&mut self, idx: usize) {
        let Some(&(mid, len)) = self.snippet_fields.get(idx) else {
            return;
        };
        let Some(pos) = self.script.marker_pos(mid) else {
            return;
        };
        let pos = pos.min(self.active.buffer.len());
        if len > 0 {
            self.active.selected_range = pos..(pos + len).min(self.active.buffer.len());
            self.active.selection_reversed = false;
            self.active.marked_range = None;
            self.active.mark_active = true;
        } else {
            self.collapse_to(pos);
        }
        self.snippet_index = idx;
    }

    /// Move to the next (`dir` +1) or previous (`dir` -1) field; stepping past
    /// the last field ends the session.
    pub(super) fn snippet_step_field(&mut self, dir: isize, cx: &mut Context<Self>) {
        if self.snippet_fields.is_empty() {
            return;
        }
        let next = self.snippet_index as isize + dir;
        if next < 0 {
            return;
        }
        if next as usize >= self.snippet_fields.len() {
            self.snippet_clear();
            self.echo("snippet: done", cx);
            return;
        }
        self.snippet_goto_field(next as usize);
        self.ensure_cursor_visible();
        cx.notify();
    }

    /// End the snippet session, dropping its field markers.
    pub(super) fn snippet_clear(&mut self) {
        let ids: Vec<u64> = self.snippet_fields.iter().map(|&(id, _)| id).collect();
        for id in ids {
            self.script.remove_marker(id);
        }
        self.snippet_fields.clear();
        self.snippet_index = 0;
    }

    // --- avy jump hints ---------------------------------------------------

    /// The active avy hints (byte offset + label), read by the renderer.
    pub fn avy_hints(&self) -> &[(usize, SharedString)] {
        &self.avy_hints
    }

    /// Set avy hints from "byte\tlabel" records (one per line).
    pub(super) fn avy_show_hints(&mut self, records: &str, cx: &mut Context<Self>) {
        self.avy_hints = records
            .lines()
            .filter_map(|line| {
                let (b, label) = line.split_once('\t')?;
                Some((
                    b.trim().parse::<usize>().ok()?,
                    SharedString::from(label.to_string()),
                ))
            })
            .collect();
        cx.notify();
    }

    pub(super) fn avy_clear_hints(&mut self, cx: &mut Context<Self>) {
        if !self.avy_hints.is_empty() {
            self.avy_hints.clear();
            cx.notify();
        }
    }

    // --- multiple cursors -------------------------------------------------

    /// Byte positions of the secondary cursors (resolved from their markers).
    pub fn extra_cursor_positions(&self) -> Vec<usize> {
        self.extra_cursors
            .iter()
            .filter_map(|s| self.script.marker_pos(s.head))
            .collect()
    }

    pub(super) fn has_extra_cursors(&self) -> bool {
        !self.extra_cursors.is_empty()
    }

    /// All cursor byte positions (primary + extras), unique and ascending.
    pub(super) fn all_cursor_positions(&self) -> Vec<usize> {
        let mut v: Vec<usize> = self.extra_cursor_positions();
        v.push(self.active.selected_range.start);
        v.sort_unstable();
        v.dedup();
        v
    }

    /// Add a secondary cursor at byte `pos` (ignored if it coincides with an
    /// existing cursor). Returns whether one was added. The paired anchor marker
    /// sits on the head, so a bare cursor is an empty selection (no highlight).
    pub(super) fn mc_add_cursor_at(&mut self, pos: usize) -> bool {
        let pos = pos.min(self.active.buffer.len());
        if pos == self.active.selected_range.start || self.extra_cursor_positions().contains(&pos) {
            return false;
        }
        self.extra_cursors.push(SelectionMarkers {
            head: self.script.add_marker(pos),
            anchor: self.script.add_marker(pos),
        });
        true
    }

    /// Drop the marker pair (head + anchor) backing every secondary selection,
    /// leaving only the primary. Shared by `mc-clear` and `set-selections`.
    pub(super) fn clear_extra_selections(&mut self) {
        for s in std::mem::take(&mut self.extra_cursors) {
            self.script.remove_marker(s.head);
            self.script.remove_marker(s.anchor);
        }
    }

    /// Drop all secondary cursors.
    pub(super) fn mc_clear(&mut self, cx: &mut Context<Self>) {
        self.clear_extra_selections();
        cx.notify();
    }

    /// The secondary selections as resolved `(start, end)` byte ranges, dropping
    /// any whose anchor and head coincide (a bare cursor highlights nothing). The
    /// renderer paints one `secondary-region` span per entry.
    pub fn extra_selections(&self) -> Vec<(usize, usize)> {
        self.extra_cursors
            .iter()
            .filter_map(|s| {
                let a = self.script.marker_pos(s.anchor)?;
                let h = self.script.marker_pos(s.head)?;
                (a != h).then_some((a.min(h), a.max(h)))
            })
            .collect()
    }

    /// Every selection as an `(anchor, head)` pair — the primary first, then the
    /// secondaries in insertion order. Published in the snapshot for Scheme's
    /// `(selections)`; `head` is the moving cursor end, `anchor` the fixed end.
    pub(super) fn all_selections(&self) -> Vec<(usize, usize)> {
        let head = self.cursor_offset();
        let anchor = if self.active.mark_active {
            if self.active.selection_reversed {
                self.active.selected_range.end
            } else {
                self.active.selected_range.start
            }
        } else {
            head
        };
        let mut sels = vec![(anchor, head)];
        for s in &self.extra_cursors {
            if let (Some(a), Some(h)) = (
                self.script.marker_pos(s.anchor),
                self.script.marker_pos(s.head),
            ) {
                sels.push((a, h));
            }
        }
        sels
    }

    /// Replace the whole selection set from a list of `(anchor, head)` pairs
    /// (Scheme's `(set-selections …)`). The first pair becomes the primary
    /// selection; the rest become marker-backed secondaries. An empty `anchor ==
    /// head` pair is a bare cursor. The selection-first motions in
    /// scheme/helix.scm drive every Normal-mode move through this.
    pub(super) fn set_selections(&mut self, sels: Vec<(usize, usize)>, cx: &mut Context<Self>) {
        self.clear_extra_selections();
        let len = self.active.buffer.len();
        let clamp = |x: usize| x.min(len);
        let mut it = sels.into_iter();
        let Some((anchor, head)) = it.next() else {
            cx.notify();
            return;
        };
        let (anchor, head) = (clamp(anchor), clamp(head));
        if anchor == head {
            self.collapse_to(head);
        } else {
            self.active.selected_range = anchor.min(head)..anchor.max(head);
            self.active.selection_reversed = head < anchor;
            self.active.marked_range = None;
            self.active.mark_active = true;
        }
        for (anchor, head) in it {
            let (anchor, head) = (clamp(anchor), clamp(head));
            self.extra_cursors.push(SelectionMarkers {
                head: self.script.add_marker(head),
                anchor: self.script.add_marker(anchor),
            });
        }
        self.ensure_cursor_visible();
        cx.notify();
    }

    /// Replicate an insertion of `text` at every cursor (primary + extras),
    /// keeping all of them after their inserted text. One undo group.
    pub(super) fn multi_insert(&mut self, text: &str, cx: &mut Context<Self>) {
        let primary = self.active.selected_range.start;
        let pm = self.script.add_marker(primary);
        let positions = self.all_cursor_positions();
        self.begin_undo_group();
        for &pos in positions.iter().rev() {
            self.edit(pos..pos, text);
        }
        self.end_undo_group();
        let np = self.script.marker_pos(pm).unwrap_or(primary);
        self.script.remove_marker(pm);
        self.collapse_to(np);
        self.parse_full();
        self.ensure_cursor_visible();
        cx.notify();
    }

    /// Replicate a backspace (delete the grapheme before each cursor).
    pub(super) fn multi_delete_back(&mut self, cx: &mut Context<Self>) {
        let primary = self.active.selected_range.start;
        let pm = self.script.add_marker(primary);
        let positions = self.all_cursor_positions();
        self.begin_undo_group();
        for &pos in positions.iter().rev() {
            let prev = self.active.buffer.prev_grapheme(pos);
            if prev < pos {
                self.edit(prev..pos, "");
            }
        }
        self.end_undo_group();
        let np = self.script.marker_pos(pm).unwrap_or(primary);
        self.script.remove_marker(pm);
        self.collapse_to(np);
        self.parse_full();
        self.ensure_cursor_visible();
        cx.notify();
    }

    /// Add a cursor on the line above/below the lowest/highest cursor, at the
    /// primary cursor's column (`dir` -1 = above, +1 = below).
    pub(super) fn mc_add_line(&mut self, dir: isize, cx: &mut Context<Self>) {
        let primary = self.active.selected_range.start;
        let line = self.active.buffer.line_index_at_byte(primary);
        let target = line as isize + dir;
        if target < 0 || target as usize >= self.active.buffer.line_count() {
            self.echo("multiple-cursors: no line there", cx);
            return;
        }
        // Track the CHARACTER column, not the byte column (same rule as
        // `line_offset`). A byte column carried onto a line with multibyte text
        // lands mid-codepoint, and the marker it seeds then hands `edit()` an
        // offset the rope refuses to slice at.
        let line_start = self.active.buffer.line_range(line).start;
        let goal =
            self.active.buffer.byte_to_char(primary) - self.active.buffer.byte_to_char(line_start);
        let lr = self.active.buffer.line_range(target as usize);
        let ts = self.active.buffer.byte_to_char(lr.start);
        let tlen = self.active.buffer.byte_to_char(lr.end) - ts;
        let pos = self.active.buffer.char_to_byte(ts + goal.min(tlen));
        if self.mc_add_cursor_at(pos) {
            cx.notify();
        }
    }

    /// Add a cursor at the next occurrence of the word at point (mc/mark-next).
    pub(super) fn mc_mark_next(&mut self, cx: &mut Context<Self>) {
        let primary = self.active.selected_range.start;
        // The word under the primary cursor.
        let start = self
            .active
            .buffer
            .backward_word(self.active.buffer.forward_word(primary));
        let end = self.active.buffer.forward_word(start);
        let word = self.active.buffer.slice_to_string(start..end);
        if word.trim().is_empty() {
            self.echo("multiple-cursors: no word at point", cx);
            return;
        }
        // Search after the furthest existing cursor, wrapping once.
        let from = self
            .all_cursor_positions()
            .into_iter()
            .max()
            .unwrap_or(primary);
        let text = self.active.buffer.full_text();
        let found = text[from..]
            .find(&word)
            .map(|i| from + i)
            .or_else(|| text.find(&word));
        match found {
            Some(at) if self.mc_add_cursor_at(at) => {
                self.collapse_to(at + word.len());
                self.ensure_cursor_visible();
                cx.notify();
            }
            _ => self.echo("multiple-cursors: no more matches", cx),
        }
    }
}
