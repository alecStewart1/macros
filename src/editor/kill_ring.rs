//! Kill ring, clipboard, and yank operations for [`Editor`], extracted from the editor module for size.
//! These methods extend the same `Editor` type via an additional
//! `impl` block; cross-module entry points are `pub(super)`.

use gpui::Context;

use super::*;

impl Editor {
    pub(super) fn kill_line(&mut self, cx: &mut Context<Self>) {
        let p = self.point();
        let line = self.buffer.line_range_at(p);
        let range = if p < line.end {
            p..line.end
        } else if p < self.buffer.len() {
            p..self.buffer.next_grapheme(p) // the line break
        } else {
            return;
        };
        self.kill_range(range, cx);
    }

    pub(super) fn kill_word(&mut self, cx: &mut Context<Self>) {
        let p = self.point();
        self.kill_range(p..self.buffer.forward_word(p), cx);
    }

    pub(super) fn backward_kill_word(&mut self, cx: &mut Context<Self>) {
        let p = self.point();
        self.kill_range(self.buffer.backward_word(p)..p, cx);
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
        match self.region() {
            Some(range) => {
                let text = self.buffer.slice_to_string(range);
                self.set_clipboard(&text, cx); // sync the system clipboard
                self.kill_ring.push(text);
                self.kill_index = self.kill_ring.len() - 1;
                self.mark_active = false;
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
                let text = self.buffer.slice_to_string(range);
                self.set_clipboard(&text, cx);
                self.kill_ring.push(text);
                self.kill_index = self.kill_ring.len() - 1;
                self.mark_active = false;
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
                let text = self.buffer.slice_to_string(range.clone());
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
        let start = self.selected_range.start;
        let cursor = self.edit(self.selected_range.clone(), &text);
        self.collapse_to(cursor);
        self.last_yank = Some(start..cursor);
        self.kill_ring.push(text);
        self.kill_index = self.kill_ring.len() - 1;
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
            self.kill_ring.push(clip);
            self.kill_index = self.kill_ring.len() - 1;
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
        let start = self.selected_range.start;
        let cursor = self.edit(self.selected_range.clone(), &text);
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
        let (left, mid, right) = if p < self.buffer.len() && p > 0 {
            (
                self.buffer.prev_grapheme(p),
                p,
                self.buffer.next_grapheme(p),
            )
        } else if p == self.buffer.len() && p > 0 {
            let mid = self.buffer.prev_grapheme(p);
            if mid == 0 {
                return;
            }
            (self.buffer.prev_grapheme(mid), mid, p)
        } else {
            return;
        };
        let a = self.buffer.slice_to_string(left..mid);
        let b = self.buffer.slice_to_string(mid..right);
        let swapped = format!("{b}{a}");
        self.edit(left..right, &swapped);
        self.collapse_to(right);
        cx.notify();
    }
}
