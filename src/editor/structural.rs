//! Structural (s-expression) motion and editing for Lisp buffers — Emacs's
//! `forward-sexp` family plus the core paredit edits — on the reader-aware
//! boundaries in [`crate::sexp`], so strings, comments and char literals never
//! confuse them.

use gpui::Context;

use super::*;
use crate::sexp::{self, StructEdit};

/// A structural command (see `EditorCommand::Structural`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuralOp {
    ForwardSexp,
    BackwardSexp,
    UpList,
    BackwardUpList,
    DownList,
    KillSexp,
    SlurpForward,
    BarfForward,
    Raise,
    Splice,
    WrapRound,
}

impl Editor {
    pub(super) fn structural(&mut self, op: StructuralOp, cx: &mut Context<Self>) {
        use StructuralOp::*;
        let text = self.active.buffer.full_text();
        let pos = self.point();
        let result: Result<Option<usize>, StructEdit> = match op {
            ForwardSexp => Ok(sexp::forward_sexp(&text, pos)),
            BackwardSexp => Ok(sexp::backward_sexp(&text, pos)),
            UpList => Ok(sexp::up_list(&text, pos)),
            BackwardUpList => Ok(sexp::backward_up_list(&text, pos)),
            DownList => Ok(sexp::down_list(&text, pos)),
            KillSexp => {
                match sexp::forward_sexp(&text, pos) {
                    Some(end) => {
                        // Through the kill ring, like any kill.
                        self.kill_range(pos..end, cx);
                    }
                    None => self.echo("No sexp to kill", cx),
                }
                return;
            }
            SlurpForward => sexp::slurp_forward(&text, pos).map_or(Ok(None), Err),
            BarfForward => sexp::barf_forward(&text, pos).map_or(Ok(None), Err),
            Raise => sexp::raise_sexp(&text, pos).map_or(Ok(None), Err),
            Splice => sexp::splice_sexp(&text, pos).map_or(Ok(None), Err),
            WrapRound => Err(sexp::wrap_round(&text, pos)),
        };
        match result {
            Ok(Some(to)) => self.move_to(to, cx),
            Ok(None) => self.echo("Scan error: no sexp there", cx),
            Err(edit) => self.apply_struct_edit(edit, cx),
        }
    }

    /// Apply a [`StructEdit`] as one undo step.
    fn apply_struct_edit(&mut self, mut edit: StructEdit, cx: &mut Context<Self>) {
        edit.edits
            .sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
        self.begin_undo_group();
        for (range, text) in edit.edits {
            self.edit(range, &text);
        }
        self.end_undo_group();
        self.move_to(edit.point, cx);
    }
}
