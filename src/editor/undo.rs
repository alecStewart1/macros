//! Undo/redo tree and undo grouping for [`Editor`], extracted from the editor module for size.
//! These methods extend the same `Editor` type via an additional
//! `impl` block; cross-module entry points are `pub(super)`.

use gpui::Context;

use super::*;

impl Editor {
    pub(super) fn undo(&mut self, cx: &mut Context<Self>) {
        let node = self.active.undo_current;
        let Some(parent) = self.active.undo_tree[node].parent else {
            self.echo("No further undo information", cx);
            return;
        };
        // Undo mutates the buffer directly (below), bypassing `edit()` — so the
        // ghost suggestion must be dropped here too, or a stale one could
        // re-anchor over rewritten content when a later move/redo puts point
        // back on its byte.
        self.active.ghost = None;
        // Invert the current node's edit group (last edit undone first):
        // restore `removed` over the inserted span. Overlays and markers track
        // each inverse edit just like `edit()` does — without this, org's
        // hidden stars / faces / virtual indents sit at stale positions after
        // an undo, and (with no after-change hook fired) nothing repaints them.
        let group = self.active.undo_tree[node].edit.clone();
        let mut cursor = 0;
        for record in group.into_iter().rev() {
            let old_end = record.at + record.inserted.len();
            // Capture the pre-change LSP positions BEFORE mutating the rope,
            // exactly as `edit()` does — an undo is an ordinary document change
            // as far as the server is concerned, and skipping it leaves an
            // incremental-sync server (rust-analyzer, gopls, …) permanently
            // out of step with the buffer for the rest of the session.
            let lsp_change = self.lsp_change_positions(record.at, old_end);
            self.active
                .buffer
                .replace(record.at..old_end, &record.removed);
            if let Some((language, start, end)) = lsp_change {
                self.lsp_did_change(&language, start, end, &record.removed);
            }
            let line_delta = record.removed.matches('\n').count() as isize
                - record.inserted.matches('\n').count() as isize;
            let start_line = self.active.buffer.byte_to_position(record.at).0;
            self.adjust_overlays(
                record.at,
                old_end,
                record.removed.len(),
                line_delta,
                start_line,
            );
            self.script
                .adjust_markers(record.at, old_end, record.removed.len());
            cursor = record.at + record.removed.len();
        }
        self.active.undo_current = parent;
        self.undo_group_started = false; // next edit branches a new node
        self.active.modified = true;
        self.parse_full();
        self.collapse_to(cursor);
        self.ensure_cursor_visible();
        // Emacs runs `after-change-functions` for undo like any other edit —
        // this is what schedules org/markdown's debounced refontify.
        self.run_hook("after-change-functions");
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
        let Some(&child) = self.active.undo_tree[self.active.undo_current]
            .children
            .last()
        else {
            self.echo("No further redo information", cx);
            return;
        };
        self.active.ghost = None; // direct buffer mutation, like undo() above
        // Re-apply the child's edit group forward: `inserted` over the removed
        // span. Overlays/markers track each edit and the after-change hook
        // fires, mirroring `undo` above.
        let group = self.active.undo_tree[child].edit.clone();
        let mut cursor = 0;
        for record in &group {
            let old_end = record.at + record.removed.len();
            let lsp_change = self.lsp_change_positions(record.at, old_end); // see `undo`
            self.active
                .buffer
                .replace(record.at..old_end, &record.inserted);
            if let Some((language, start, end)) = lsp_change {
                self.lsp_did_change(&language, start, end, &record.inserted);
            }
            let line_delta = record.inserted.matches('\n').count() as isize
                - record.removed.matches('\n').count() as isize;
            let start_line = self.active.buffer.byte_to_position(record.at).0;
            self.adjust_overlays(
                record.at,
                old_end,
                record.inserted.len(),
                line_delta,
                start_line,
            );
            self.script
                .adjust_markers(record.at, old_end, record.inserted.len());
            cursor = record.at + record.inserted.len();
        }
        self.active.undo_current = child;
        self.undo_group_started = false;
        self.active.modified = true;
        self.parse_full();
        self.collapse_to(cursor);
        self.ensure_cursor_visible();
        self.run_hook("after-change-functions");
        self.echo("Redo", cx);
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
