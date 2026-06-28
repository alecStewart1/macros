//! Undo/redo tree and undo grouping for [`Editor`], extracted from the editor module for size.
//! These methods extend the same `Editor` type via an additional
//! `impl` block; cross-module entry points are `pub(super)`.

use gpui::Context;

use super::*;

impl Editor {
    pub(super) fn undo(&mut self, cx: &mut Context<Self>) {
        let node = self.undo_current;
        let Some(parent) = self.undo_tree[node].parent else {
            self.echo("No further undo information", cx);
            return;
        };
        // Invert the current node's edit group (last edit undone first):
        // restore `removed` over the inserted span.
        let group = self.undo_tree[node].edit.clone();
        let mut cursor = 0;
        for record in group.into_iter().rev() {
            let span = record.at..record.at + record.inserted.len();
            self.buffer.replace(span, &record.removed);
            cursor = record.at + record.removed.len();
        }
        self.undo_current = parent;
        self.undo_group_started = false; // next edit branches a new node
        self.modified = true;
        self.parse_full();
        self.collapse_to(cursor);
        self.ensure_cursor_visible();
        let branches = self.undo_tree[parent].children.len();
        if branches > 1 {
            self.echo(format!("Undo (branch point: {branches} redo branches)"), cx);
        } else {
            self.echo("Undo", cx);
        }
    }

    /// Redo: re-apply the most recently created child branch of the current node
    /// (undo-tree semantics — the last branch you came from / created).
    pub(super) fn redo(&mut self, cx: &mut Context<Self>) {
        let Some(&child) = self.undo_tree[self.undo_current].children.last() else {
            self.echo("No further redo information", cx);
            return;
        };
        // Re-apply the child's edit group forward: `inserted` over the removed span.
        let group = self.undo_tree[child].edit.clone();
        let mut cursor = 0;
        for record in &group {
            let span = record.at..record.at + record.removed.len();
            self.buffer.replace(span, &record.inserted);
            cursor = record.at + record.inserted.len();
        }
        self.undo_current = child;
        self.undo_group_started = false;
        self.modified = true;
        self.parse_full();
        self.collapse_to(cursor);
        self.ensure_cursor_visible();
        self.echo("Redo", cx);
    }

    /// A textual dump of the undo tree for the vundo visualizer: one
    /// "idx\tparent\tcurrent?\tdepth\tsize" line per node (depth = distance from
    /// root, size = edits in the node's group).
    pub(super) fn undo_tree_dump(&self) -> String {
        self.undo_tree
            .iter()
            .enumerate()
            .map(|(i, n)| {
                let mut depth = 0;
                let mut p = n.parent;
                while let Some(parent) = p {
                    depth += 1;
                    p = self.undo_tree[parent].parent;
                }
                let parent = n.parent.map_or(-1, |x| x as isize);
                let current = if i == self.undo_current { 1 } else { 0 };
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
