//! Undo/redo tree and undo grouping for [`Editor`], extracted from the editor module for size.
//! These methods extend the same `Editor` type via an additional
//! `impl` block; cross-module entry points are `pub(super)`.

use gpui::Context;

use super::*;

impl Document {
    /// Forget all undo history. Required whenever the text changes WITHOUT an
    /// undo record (a special buffer re-rendered wholesale, an unrecorded
    /// rewrite of an inactive buffer): the old records address text that no
    /// longer exists, and replaying one would corrupt the buffer.
    pub(super) fn reset_undo(&mut self) {
        self.undo_tree = new_undo_tree();
        self.undo_current = 0;
    }
}

/// Whether `expect` is exactly the text at byte `at` — i.e. an undo record
/// still describes the buffer. Every record is checked before it's replayed,
/// so a history gone stale through some unrecorded mutation is detected and
/// dropped instead of splicing text at wrong (possibly mid-char) offsets.
fn record_matches(buffer: &Buffer, at: usize, expect: &str) -> bool {
    let end = at + expect.len();
    end <= buffer.len()
        && buffer.snap(at) == at
        && buffer.snap(end) == end
        && buffer.slice_to_string(at..end) == expect
}

impl Editor {
    /// Replay one node's edit group — inverted (undo, last record first) or
    /// forward (redo) — keeping overlays, markers, and LSP in step exactly as
    /// `edit()` does. Returns the cursor after the last applied record, and
    /// whether every record applied (`false` = history was stale; replay
    /// stopped at the first record that no longer matched the text).
    fn replay_undo_group(&mut self, group: Vec<UndoRecord>, forward: bool) -> (usize, bool) {
        let mut cursor = self.point();
        let records: Box<dyn Iterator<Item = UndoRecord>> = if forward {
            Box::new(group.into_iter())
        } else {
            Box::new(group.into_iter().rev())
        };
        for record in records {
            let (old, new) = if forward {
                (&record.removed, &record.inserted)
            } else {
                (&record.inserted, &record.removed)
            };
            if !record_matches(&self.active.buffer, record.at, old) {
                return (cursor, false);
            }
            let old_end = record.at + old.len();
            // Capture the pre-change LSP positions BEFORE mutating the rope,
            // exactly as `edit()` does — an undo is an ordinary document change
            // as far as the server is concerned, and skipping it leaves an
            // incremental-sync server (rust-analyzer, gopls, …) permanently
            // out of step with the buffer for the rest of the session.
            let lsp_change = self.lsp_change_positions(record.at, old_end);
            self.active.buffer.replace(record.at..old_end, new);
            if let Some((language, start, end)) = lsp_change {
                self.lsp_did_change(&language, start, end, new);
            }
            // Overlays and markers track each replayed edit just like `edit()`
            // does — without this, org's hidden stars / faces / virtual indents
            // sit at stale positions after an undo, and (with no after-change
            // hook fired) nothing repaints them.
            let line_delta =
                new.matches('\n').count() as isize - old.matches('\n').count() as isize;
            let start_line = self.active.buffer.byte_to_position(record.at).0;
            self.adjust_overlays(record.at, old_end, new.len(), line_delta, start_line);
            self.script.adjust_markers(record.at, old_end, new.len());
            self.shift_vim_positions(record.at, old_end, new.len());
            cursor = record.at + new.len();
        }
        (cursor, true)
    }

    /// Shared tail of undo/redo: settle the buffer after a replay. On a stale
    /// history (`ok == false`) the tree is dropped — the text is intact, only
    /// the ability to step further is lost — and the user is told why.
    fn finish_undo_replay(&mut self, cursor: usize, ok: bool, cx: &mut Context<Self>) -> bool {
        self.undo_group_started = false; // next edit branches a new node
        self.active.modified = true;
        self.parse_full();
        self.collapse_to(self.active.buffer.snap(cursor));
        self.ensure_cursor_visible();
        // Emacs runs `after-change-functions` for undo like any other edit —
        // this is what schedules org/markdown's debounced refontify.
        self.run_hook("after-change-functions");
        if !ok {
            self.active.reset_undo();
            self.echo(
                "Undo history no longer matches the buffer; history cleared",
                cx,
            );
        }
        ok
    }

    pub(super) fn undo(&mut self, cx: &mut Context<Self>) {
        let node = self.active.undo_current;
        let Some(parent) = self.active.undo_tree.get(node).and_then(|n| n.parent) else {
            self.echo("No further undo information", cx);
            return;
        };
        // Undo mutates the buffer directly (below), bypassing `edit()` — so the
        // ghost suggestion must be dropped here too, or a stale one could
        // re-anchor over rewritten content when a later move/redo puts point
        // back on its byte.
        self.active.ghost = None;
        // Invert the current node's edit group (last edit undone first):
        // restore `removed` over the inserted span.
        let group = self.active.undo_tree[node].edit.clone();
        let (cursor, ok) = self.replay_undo_group(group, false);
        if ok {
            self.active.undo_current = parent;
        }
        if !self.finish_undo_replay(cursor, ok, cx) {
            return;
        }
        let branches = self.active.undo_tree[parent].children.len();
        if branches > 1 {
            self.echo(format!("Undo (branch point: {branches} redo branches)"), cx);
        } else {
            self.echo("Undo", cx);
        }
    }

    /// Redo: re-apply the most recently created child branch of the current node
    /// (undo-tree semantics — the last branch you came from / created).
    pub(super) fn redo(&mut self, cx: &mut Context<Self>) {
        let Some(child) = self
            .active
            .undo_tree
            .get(self.active.undo_current)
            .and_then(|n| n.children.last().copied())
            .filter(|&c| c < self.active.undo_tree.len())
        else {
            self.echo("No further redo information", cx);
            return;
        };
        self.active.ghost = None; // direct buffer mutation, like undo() above
        // Re-apply the child's edit group forward: `inserted` over the removed
        // span.
        let group = self.active.undo_tree[child].edit.clone();
        let (cursor, ok) = self.replay_undo_group(group, true);
        if ok {
            self.active.undo_current = child;
        }
        if self.finish_undo_replay(cursor, ok, cx) {
            self.echo("Redo", cx);
        }
    }

    /// A textual dump of the undo tree for the vundo visualizer: one
    /// "idx\tparent\tcurrent?\tdepth\tsize" line per node (depth = distance from
    /// root, size = edits in the node's group).
    pub(super) fn undo_tree_dump(&self) -> String {
        self.active
            .undo_tree
            .iter()
            .enumerate()
            .map(|(i, n)| {
                let mut depth = 0;
                let mut p = n.parent;
                while let Some(parent) = p {
                    depth += 1;
                    p = self.active.undo_tree[parent].parent;
                }
                let parent = n.parent.map_or(-1, |x| x as isize);
                let current = if i == self.active.undo_current { 1 } else { 0 };
                format!("{i}\t{parent}\t{current}\t{depth}\t{}", n.edit.len())
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Group all edits made by one Scheme command batch into a single undo unit.
    pub(super) fn begin_undo_group(&mut self) {
        self.undo_grouping = true;
        self.undo_group_started = false;
    }

    pub(super) fn end_undo_group(&mut self) {
        self.undo_grouping = false;
        self.undo_group_started = false;
    }

    /// `(undo-boundary)`: close the current group so the next edit starts a new
    /// one even within the same command batch.
    pub(super) fn undo_boundary(&mut self) {
        self.undo_group_started = false;
    }

    // --- Programmatic range editing ---------------------------------------
}

#[cfg(test)]
mod tests {
    use super::record_matches;
    use crate::buffer::Buffer;

    #[test]
    fn stale_undo_records_are_detected_not_replayed() {
        // A record replays only when its text is still exactly where it says.
        let b = Buffer::new("hello wörld");
        assert!(record_matches(&b, 6, "wörld"));
        assert!(record_matches(&b, 3, "")); // pure insertion point
        // The special-buffer re-render case: same offsets, different text.
        assert!(!record_matches(&b, 6, "world"));
        // Past the end (the buffer shrank under the history).
        assert!(!record_matches(&b, 10, "abc"));
        assert!(!record_matches(&b, 99, ""));
        // Mid-codepoint (`ö` is bytes 7..9) — never splice there.
        assert!(!record_matches(&b, 8, ""));
    }
}
