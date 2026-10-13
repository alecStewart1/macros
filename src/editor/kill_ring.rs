//! Kill ring, clipboard, and yank operations for [`Editor`], extracted from the editor module for size.
//! These methods extend the same `Editor` type via an additional
//! `impl` block; cross-module entry points are `pub(super)`.

use gpui::Context;

use super::*;

/// Upper bound on kill-ring entries (the spirit of Emacs `kill-ring-max`).
/// Pushing past the cap evicts the oldest entry so the ring can't grow without
/// limit over a long session.
const KILL_RING_MAX: usize = 120;

impl Editor {
    /// Record `text` as the newest kill and point the yank index at it,
    /// evicting the oldest entry when the ring is full. Whether a kill also
    /// syncs to the OS clipboard stays per-site policy — callers that mirror it
    /// call `set_clipboard` themselves.
    pub(super) fn push_kill(&mut self, text: String) {
        // Any new kill/copy is charwise until an evil op says otherwise (evil's
        // linewise ops queue `evil-set-linewise` AFTER their kill), so a later
        // cmd-c / C-k can't be pasted with a stale `yy` line-wise flag.
        self.evil_yank_linewise = false;
        if self.kill_append
            && let Some(last) = self.kill_ring.last_mut()
        {
            last.push_str(&text);
            self.kill_appended = true;
            self.kill_index = self.kill_ring.len() - 1;
            return;
        }
        self.kill_ring.push(text);
        if self.kill_ring.len() > KILL_RING_MAX {
            self.kill_ring.remove(0);
        }
        self.kill_index = self.kill_ring.len() - 1;
    }

    /// File a yank/delete's `text` everywhere a kill goes: the system
    /// clipboard, the Vim registers (`reg` is the `"x` prefix, already taken)
    /// and the kill ring, then mark it line-wise for `p` when `linewise`.
    /// The black-hole register `"_` swallows it: nothing is filed and this
    /// returns false.
    pub(super) fn file_kill(
        &mut self,
        kind: super::registers::KillKind,
        reg: Option<char>,
        text: String,
        linewise: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        if reg == Some('_') {
            return false;
        }
        self.set_clipboard(&text, cx);
        self.note_register_kill(kind, reg, &text);
        // Resets the line-wise flag; set it again after for a line-wise kill.
        self.push_kill(text);
        if linewise {
            self.evil_yank_linewise = true;
            self.vim.registers.set_linewise_last(true);
        }
        true
    }

    pub(super) fn kill_line(&mut self, cx: &mut Context<Self>) {
        let p = self.point();
        let line = self.active.buffer.line_range_at(p);
        let range = if p < line.end {
            p..line.end
        } else if p < self.active.buffer.len() {
            p..self.active.buffer.next_grapheme(p) // the line break
        } else {
            return;
        };
        self.kill_range(range, cx);
    }

    pub(super) fn kill_word(&mut self, cx: &mut Context<Self>) {
        let p = self.point();
        self.kill_range(p..self.active.buffer.forward_word(p), cx);
    }

    pub(super) fn backward_kill_word(&mut self, cx: &mut Context<Self>) {
        let p = self.point();
        self.kill_range(self.active.buffer.backward_word(p)..p, cx);
    }

    pub(super) fn kill_region(&mut self, cx: &mut Context<Self>) {
        match self.region() {
            Some(range) => self.kill_range(range, cx),
            None => self.echo("No region selected", cx),
        }
    }

    pub(super) fn kill_ring_save(&mut self, cx: &mut Context<Self>) {
        if self.terminal_copy(cx) {
            return;
        }
        let reg = self.take_kill_register();
        if reg == Some('_') {
            self.active.mark_active = false;
            return;
        }
        match self.region() {
            Some(range) => {
                self.set_auto_mark('[', range.start);
                self.set_auto_mark(']', range.end.saturating_sub(1).max(range.start));
                let text = self.active.buffer.slice_to_string(range);
                self.file_kill(super::registers::KillKind::Yank, reg, text, false, cx);
                self.active.mark_active = false;
                self.echo("Saved to kill ring", cx);
            }
            None => self.echo("No region selected", cx),
        }
    }

    // --- System clipboard (macOS cmd-c/x/v; bindable as clipboard-*) ----------

    /// Write `text` to the OS clipboard (kept in sync with the kill ring so the
    /// Emacs and platform copy/paste worlds interoperate).
    pub(super) fn set_clipboard(&self, text: &str, cx: &mut Context<Self>) {
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.to_string()));
    }

    /// Copy the region to the system clipboard (and the kill ring).
    pub(super) fn clipboard_copy(&mut self, cx: &mut Context<Self>) {
        if self.terminal_copy(cx) {
            return;
        }
        match self.region() {
            Some(range) => {
                let text = self.active.buffer.slice_to_string(range);
                self.set_clipboard(&text, cx);
                self.push_kill(text);
                self.active.mark_active = false;
                self.echo("Copied", cx);
                cx.notify();
            }
            None => self.echo("No region selected", cx),
        }
    }

    /// Cut the region to the system clipboard (and the kill ring).
    pub(super) fn clipboard_cut(&mut self, cx: &mut Context<Self>) {
        // A terminal's grid is read-only; "cut" just copies the selection.
        if self.terminal_copy(cx) {
            return;
        }
        match self.region() {
            Some(range) => {
                let text = self.active.buffer.slice_to_string(range.clone());
                self.set_clipboard(&text, cx);
                self.kill_range(range, cx); // deletes + pushes to the kill ring
                self.echo("Cut", cx);
            }
            None => self.echo("No region selected", cx),
        }
    }

    /// Paste the system clipboard at point (replacing the region), also pushing
    /// it onto the kill ring so `C-y` / `yank-pop` see it.
    pub(super) fn clipboard_paste(&mut self, cx: &mut Context<Self>) {
        if self.terminal_paste(cx) {
            return;
        }
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            self.echo("Clipboard is empty", cx);
            return;
        };
        if text.is_empty() {
            self.echo("Clipboard is empty", cx);
            return;
        }
        let start = self.active.selected_range.start;
        let cursor = self.edit(self.active.selected_range.clone(), &text);
        self.collapse_to(cursor);
        self.last_yank = Some(start..cursor);
        self.push_kill(text);
        cx.notify();
    }

    /// Pull the system clipboard into the kill ring when it holds text that isn't
    /// already the ring's top — i.e. it was copied in another app, or via a path
    /// that bypassed the ring. This is Emacs's `interprogram-paste-function`: any
    /// yank/paste then reflects what was last copied *anywhere*, not just inside
    /// the editor. Returns true when an external value was adopted, so callers
    /// tracking register metadata (evil's line-wise flag) can reset it.
    pub(super) fn sync_clipboard_into_kill_ring(&mut self, cx: &mut Context<Self>) -> bool {
        if let Some(clip) = cx.read_from_clipboard().and_then(|item| item.text())
            && !clip.is_empty()
            && self.kill_ring.last() != Some(&clip)
        {
            self.push_kill(clip);
            return true;
        }
        false
    }

    pub(super) fn yank(&mut self, cx: &mut Context<Self>) {
        // C-y reflects the system clipboard too (Emacs `select-enable-clipboard`),
        // so a copy from another app yanks here without needing cmd-v.
        self.sync_clipboard_into_kill_ring(cx);
        let Some(text) = self.kill_ring.last().cloned() else {
            self.echo("Kill ring is empty", cx);
            return;
        };
        let start = self.active.selected_range.start;
        let cursor = self.edit(self.active.selected_range.clone(), &text);
        self.collapse_to(cursor);
        self.last_yank = Some(start..cursor);
        self.kill_index = self.kill_ring.len() - 1;
        cx.notify();
    }

    pub(super) fn yank_pop(&mut self, cx: &mut Context<Self>) {
        if !self.last_was_yank {
            self.echo("Previous command was not a yank", cx);
            return;
        }
        let (Some(range), false) = (self.last_yank.clone(), self.kill_ring.is_empty()) else {
            return;
        };
        // The range was recorded by the previous yank; anything that rewrote the
        // buffer without going through a command (auto-revert, an async LSP
        // workspace edit) can have shrunk it since, and splicing past the end
        // aborts the editor inside ropey.
        let range = clamp_range(range, self.active.buffer.len());
        self.kill_index = (self.kill_index + self.kill_ring.len() - 1) % self.kill_ring.len();
        let text = self.kill_ring[self.kill_index].clone();
        let cursor = self.edit(range.clone(), &text);
        self.collapse_to(cursor);
        self.last_yank = Some(range.start..cursor);
        cx.notify();
    }

    pub(super) fn transpose_chars(&mut self, cx: &mut Context<Self>) {
        let p = self.point();
        // Need a character on each side; at buffer end, transpose the two before.
        let (left, mid, right) = if p < self.active.buffer.len() && p > 0 {
            (
                self.active.buffer.prev_grapheme(p),
                p,
                self.active.buffer.next_grapheme(p),
            )
        } else if p == self.active.buffer.len() && p > 0 {
            let mid = self.active.buffer.prev_grapheme(p);
            if mid == 0 {
                return;
            }
            (self.active.buffer.prev_grapheme(mid), mid, p)
        } else {
            return;
        };
        let a = self.active.buffer.slice_to_string(left..mid);
        let b = self.active.buffer.slice_to_string(mid..right);
        let swapped = format!("{b}{a}");
        self.edit(left..right, &swapped);
        self.collapse_to(right);
        cx.notify();
    }
}
