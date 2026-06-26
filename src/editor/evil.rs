//! Evil (modal editing) + search/ex-command methods for [`Editor`].

use regex::Regex;

use super::*;

impl Editor {
    /// True in Normal/Visual, where plain keys are commands (not self-insert).
    pub(super) fn evil_intercepts(&self) -> bool {
        matches!(
            self.evil,
            EvilState::Normal
                | EvilState::Visual
                | EvilState::VisualLine
                | EvilState::VisualBlock
        )
    }

    /// The keymap name plain keys resolve against in the current evil state.
    pub(super) fn evil_keymap(&self) -> &'static str {
        match self.evil {
            EvilState::Visual => "evil-visual",
            EvilState::VisualLine => "evil-visual-line",
            EvilState::VisualBlock => "evil-visual-block",
            _ => "evil-normal",
        }
    }

    /// Whether the `"auto"` cursor style should be a block: always, except in
    /// evil insert mode (a bar signals insertion, the vim convention). With evil
    /// disabled this is a plain block — matching the default cursor shape.
    pub fn wants_block_cursor(&self) -> bool {
        !matches!(self.evil, EvilState::Insert)
    }

    /// Short mode-line tag for the current state (None when disabled).
    pub fn evil_label(&self) -> Option<&'static str> {
        match self.evil {
            EvilState::Disabled => None,
            EvilState::Normal => Some("<N>"),
            EvilState::Insert => Some("<I>"),
            EvilState::Visual => Some("<V>"),
            EvilState::VisualLine => Some("<VL>"),
            EvilState::VisualBlock => Some("<VB>"),
        }
    }

    pub(super) fn evil_enable(&mut self, cx: &mut Context<Self>) {
        self.evil = EvilState::Normal;
        self.mark_active = false;
        cx.notify();
    }

    pub(super) fn evil_normal_state(&mut self, cx: &mut Context<Self>) {
        let was_insert = matches!(self.evil, EvilState::Insert);
        self.evil = EvilState::Normal;
        // Collapse any block-insert cursors (vim leaves a single cursor after a
        // blockwise `I`/`A` once you press Escape). No-op without extras.
        if self.has_extra_cursors() {
            self.mc_clear(cx);
        }
        // Leaving visual/insert clears any selection (vim's Escape behaviour).
        self.mark_active = false;
        // Vim nudges the cursor one column left when leaving Insert (the caret
        // sat *after* the last typed char), but not past the line's start.
        let mut point = self.point();
        if was_insert {
            let line_start = self.buffer.line_range_at(point).start;
            if point > line_start {
                point = self.buffer.prev_grapheme(point);
            }
        }
        self.collapse_to(point);
        cx.notify();
    }

    pub(super) fn evil_insert_state(&mut self, cx: &mut Context<Self>) {
        self.evil = EvilState::Insert;
        self.mark_active = false;
        cx.notify();
    }

    pub(super) fn evil_visual_state(&mut self, cx: &mut Context<Self>) {
        self.evil = EvilState::Visual;
        // Anchor the selection at point; motions extend it (mark is active).
        self.mark_active = true;
        cx.notify();
    }

    /// Vim line-wise Visual (`V`). Like [`Self::evil_visual_state`] but the
    /// selection is always rounded out to whole lines by [`Self::visual_region`],
    /// so `d`/`y` operate line-wise and `j`/`k` extend the selection a line at a
    /// time.
    pub(super) fn evil_visual_line_state(&mut self, cx: &mut Context<Self>) {
        self.evil = EvilState::VisualLine;
        self.mark_active = true;
        cx.notify();
    }

    /// Block insert (`I`, `at_eol` false) / append (`A`, `at_eol` true) over a
    /// Visual selection: drop a cursor at the start (or end) of every selected
    /// line and switch to Insert. Typed text, Enter, and Backspace replicate to
    /// every cursor via the multiple-cursors machinery, so one edit lands on all
    /// lines — vim's blockwise `I`/`A`. Escape (→ [`Self::evil_normal_state`])
    /// collapses the cursors back to one.
    pub(super) fn evil_visual_insert(&mut self, at_eol: bool, cx: &mut Context<Self>) {
        let region = self.visual_region();
        let first = self.buffer.line_index_at_byte(region.start);
        // `region.end` is the start of the line after the last selected line
        // (line-wise); step back one byte to land on the last selected line.
        let last_byte = region.end.saturating_sub(1).max(region.start);
        let last = self.buffer.line_index_at_byte(last_byte);

        let target = |ed: &Self, line: usize| -> usize {
            let lr = ed.buffer.line_range(line);
            if at_eol { lr.end } else { lr.start }
        };
        // Primary cursor on the first line; a secondary cursor on each of the
        // rest. `collapse_to` clears the selection so the insert path replicates.
        let primary = target(self, first);
        self.collapse_to(primary);
        for line in (first + 1)..=last {
            let pos = target(self, line);
            self.mc_add_cursor_at(pos);
        }
        self.evil_insert_state(cx);
    }

    /// Enter block-wise Visual (`C-v`). The two corners ride on
    /// `selected_range.start`/`.end` like the other visual states, so the usual
    /// motions move the cursor corner; the rectangle is derived on demand by
    /// [`Self::visual_block_rect`] and the renderer highlights only that column
    /// strip (not whole lines).
    pub(super) fn evil_visual_block_state(&mut self, cx: &mut Context<Self>) {
        self.evil = EvilState::VisualBlock;
        self.mark_active = true;
        cx.notify();
    }

    /// Block `I` (`append` false) / `A` (`append` true): drop a cursor at the
    /// block's left column (or just past its right column) on every row and
    /// switch to Insert, so typed text replicates down the column via the
    /// multiple-cursors machinery — vim's block-wise insert. Escape collapses the
    /// cursors back to one.
    pub(super) fn evil_block_insert(&mut self, append: bool, cx: &mut Context<Self>) {
        let Some((top, bottom, left, right)) = self.visual_block_rect() else {
            return;
        };
        let col = if append { right + 1 } else { left };
        let mut positions = Vec::new();
        for row in top..=bottom {
            let lr = self.buffer.line_range(row);
            let line = self.buffer.slice_to_string(lr.clone());
            positions.push(lr.start + char_col_byte(&line, col));
        }
        if let Some((&first, rest)) = positions.split_first() {
            self.collapse_to(first);
            for &p in rest {
                self.mc_add_cursor_at(p);
            }
        }
        self.evil_insert_state(cx);
    }

    /// Block `d`/`x`: delete the rectangle (columns `[left, right]` on every row),
    /// copy it to the kill ring + system clipboard, and return to Normal with the
    /// cursor at the block's top-left.
    pub(super) fn evil_block_delete(&mut self, cx: &mut Context<Self>) {
        let Some((top, bottom, left, right)) = self.visual_block_rect() else {
            return;
        };
        let killed = self.block_text(top, bottom, left, right);
        self.set_clipboard(&killed, cx);
        self.kill_ring.push(killed);
        self.kill_index = self.kill_ring.len() - 1;
        // Delete bottom-up so earlier edits don't shift later rows' offsets.
        self.begin_undo_group();
        for row in (top..=bottom).rev() {
            let lr = self.buffer.line_range(row);
            let line = self.buffer.slice_to_string(lr.clone());
            let s = char_col_byte(&line, left);
            let e = char_col_byte(&line, right + 1);
            if e > s {
                self.edit(lr.start + s..lr.start + e, "");
            }
        }
        self.end_undo_group();
        let top_line = self.buffer.line_range(top);
        let line = self.buffer.slice_to_string(top_line.clone());
        let cursor = top_line.start + char_col_byte(&line, left);
        self.evil_yank_linewise = false;
        self.collapse_to(cursor);
        self.evil_normal_state(cx);
    }

    /// Block `y`: copy the rectangle to the kill ring + system clipboard without
    /// removing it; the cursor returns to the block's top-left.
    pub(super) fn evil_block_yank(&mut self, cx: &mut Context<Self>) {
        let Some((top, bottom, left, right)) = self.visual_block_rect() else {
            return;
        };
        let text = self.block_text(top, bottom, left, right);
        self.set_clipboard(&text, cx);
        self.kill_ring.push(text);
        self.kill_index = self.kill_ring.len() - 1;
        self.evil_yank_linewise = false;
        let top_line = self.buffer.line_range(top);
        let line = self.buffer.slice_to_string(top_line.clone());
        let cursor = top_line.start + char_col_byte(&line, left);
        self.collapse_to(cursor);
        self.evil_normal_state(cx);
    }

    /// The text inside a block rectangle: columns `[left, right]` of rows
    /// `[top, bottom]`, one buffer line per row joined by newlines (rows shorter
    /// than `left` contribute an empty string).
    fn block_text(&self, top: usize, bottom: usize, left: usize, right: usize) -> String {
        (top..=bottom)
            .map(|row| {
                let line = self.buffer.slice_to_string(self.buffer.line_range(row));
                let s = char_col_byte(&line, left);
                let e = char_col_byte(&line, right + 1).max(s);
                line[s..e].to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub(super) fn evil_disable(&mut self, cx: &mut Context<Self>) {
        self.evil = EvilState::Disabled;
        cx.notify();
    }

    // --- evil paste (`p` / `P`) ------------------------------------------

    /// Vim `p` (after) / `P` (before). Pastes the most recent kill, choosing
    /// line-wise or character-wise placement from the flag set by the last
    /// yank/delete (`evil_yank_linewise`).
    pub(super) fn evil_paste(&mut self, after: bool, cx: &mut Context<Self>) {
        let Some(text) = self.kill_ring.last().cloned() else {
            self.echo("Kill ring is empty", cx);
            return;
        };
        if self.evil_yank_linewise {
            self.evil_paste_linewise(&text, after, cx);
        } else {
            self.evil_paste_charwise(&text, after, cx);
        }
    }

    /// Character-wise paste: insert after (vim `p`) or before (`P`) the grapheme
    /// under the cursor, leaving the cursor on the last grapheme pasted.
    fn evil_paste_charwise(&mut self, text: &str, after: bool, cx: &mut Context<Self>) {
        let p = self.point();
        let at = if after {
            self.buffer.next_grapheme(p).min(self.buffer.len())
        } else {
            p
        };
        let end = self.edit(at..at, text);
        // Vim leaves the cursor on the last character of the inserted text.
        let cursor = if end > at {
            self.buffer.prev_grapheme(end)
        } else {
            end
        };
        self.collapse_to(cursor);
        cx.notify();
    }

    /// Line-wise paste: drop the killed line(s) on a fresh line below (vim `p`)
    /// or above (`P`) the current one, cursor at the first non-blank.
    fn evil_paste_linewise(&mut self, text: &str, after: bool, cx: &mut Context<Self>) {
        let line = self.buffer.line_range_at(self.point());
        let line_start = if after {
            if line.end < self.buffer.len() {
                // Insert at the start of the following line; `text` ends in '\n'.
                let at = self.buffer.next_grapheme(line.end);
                self.edit(at..at, text);
                at
            } else {
                // Last line (no trailing newline): add the break ourselves and
                // drop the trailing one from `text` so we don't leave a blank.
                let body = text.strip_suffix('\n').unwrap_or(text);
                let at = line.end;
                self.edit(at..at, &format!("\n{body}"));
                self.buffer.next_grapheme(at)
            }
        } else {
            let at = line.start;
            self.edit(at..at, text);
            at
        };
        self.collapse_to(self.first_non_blank(line_start));
        cx.notify();
    }

    /// The first non-whitespace byte on the line containing `offset` (or the
    /// line's end if it's all blank) — where vim parks the cursor after a
    /// line-wise paste or `^`.
    fn first_non_blank(&self, offset: usize) -> usize {
        let line = self.buffer.line_range_at(offset);
        let text = self.buffer.slice_to_string(line.clone());
        match text.find(|c: char| !c.is_whitespace()) {
            Some(i) => line.start + i,
            None => line.start,
        }
    }

    // --- evil search (`/` `?` `n` `N`) and ex commands (`:`) --------------

    pub(super) fn evil_search_forward(&mut self, cx: &mut Context<Self>) {
        self.open_minibuffer("/", MinibufferPurpose::SearchForward, "", cx);
    }

    pub(super) fn evil_search_backward(&mut self, cx: &mut Context<Self>) {
        self.open_minibuffer("?", MinibufferPurpose::SearchBackward, "", cx);
    }

    /// `n` — repeat the last search in its original direction (vim semantics:
    /// after `?`, `n` keeps going backward).
    pub(super) fn evil_search_next(&mut self, cx: &mut Context<Self>) {
        self.do_search(self.evil_search_forward, cx);
    }

    /// `N` — repeat the last search in the opposite direction.
    pub(super) fn evil_search_previous(&mut self, cx: &mut Context<Self>) {
        self.do_search(!self.evil_search_forward, cx);
    }

    /// Move point to the next/previous regex match of the stored query, wrapping
    /// around the buffer. The cursor lands on the start of the match.
    pub(super) fn do_search(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(query) = self.search_query.clone() else {
            return;
        };
        let regex = match Regex::new(&query) {
            Ok(r) => r,
            Err(err) => {
                self.echo(format!("Invalid regex: {err}"), cx);
                return;
            }
        };
        let text = self.buffer.full_text();
        let point = self.point();
        let starts: Vec<usize> = regex.find_iter(&text).map(|m| m.start()).collect();
        if starts.is_empty() {
            self.echo(format!("No match for /{query}/"), cx);
            return;
        }
        let target = if forward {
            starts
                .iter()
                .find(|&&s| s > point)
                .copied()
                .unwrap_or(starts[0]) // wrap to first
        } else {
            starts
                .iter()
                .rev()
                .find(|&&s| s < point)
                .copied()
                .unwrap_or(*starts.last().unwrap()) // wrap to last
        };
        self.move_to(target, cx);
    }

    /// Run an ex command line (the text after `:`): `%s/…/…/`, `s/…/…/`, `w`,
    /// `q`, `wq`/`x`, `q!`.
    pub(super) fn run_ex_command(&mut self, input: &str, cx: &mut Context<Self>) {
        let cmd = input.trim();
        if let Some(rest) = cmd.strip_prefix("%s") {
            self.ex_substitute(true, rest, cx);
        } else if let Some(rest) = cmd.strip_prefix('s').filter(|r| r.starts_with('/')) {
            self.ex_substitute(false, rest, cx);
        } else if let Ok(n) = cmd.parse::<usize>() {
            // `:N` — jump to (1-based) line N.
            let last = self.buffer.line_count().saturating_sub(1);
            let offset = self.buffer.line_range(n.saturating_sub(1).min(last)).start;
            self.move_to(offset, cx);
        } else {
            // Split a verb from its optional argument (`:e file`, `:b name`, …).
            let (verb, arg) = match cmd.split_once(char::is_whitespace) {
                Some((v, a)) => (v, a.trim()),
                None => (cmd, ""),
            };
            match verb {
                // Write / quit.
                "w" | "write" if !arg.is_empty() => self.write_buffer_to(arg, cx),
                "w" | "write" => self.save_buffer(cx),
                "q" | "quit" => self.quit(cx),
                "q!" | "quit!" => cx.quit(),
                "wq" | "wq!" | "x" | "xit" => {
                    self.save_buffer(cx);
                    cx.quit();
                }
                "qa" | "qall" | "quitall" | "qa!" | "qall!" => cx.quit(),
                "wqa" | "wqall" | "xa" | "xall" => {
                    self.save_buffer(cx);
                    cx.quit();
                }
                // Files.
                "e!" | "edit!" => self.revert_buffer(cx),
                "e" | "edit" if !arg.is_empty() => self.find_file(arg, cx),
                "e" | "edit" => self.echo("Usage: :e <file>", cx),
                // Clear search highlighting.
                "noh" | "nohl" | "nohlsearch" => {
                    self.search_query = None;
                    cx.notify();
                }
                // Buffers.
                "b" | "bu" | "buf" | "buffer" if !arg.is_empty() => self.switch_to_buffer(arg, cx),
                "bn" | "bnext" => self.next_buffer(cx),
                "bp" | "bN" | "bprev" | "bprevious" | "bNext" => self.previous_buffer(cx),
                "bd" | "bdel" | "bdelete" => self.kill_buffer(cx),
                // Windows (Vim abbreviations): :split / :vsplit and friends.
                "split" | "sp" | "spl" => self.split_window(true, cx),
                "vsplit" | "vs" | "vsp" | "vspl" => self.split_window(false, cx),
                "only" | "on" => self.delete_other_windows(cx),
                "close" | "clo" => self.delete_window(cx),
                other => self.echo(format!("Unknown ex command: {other}"), cx),
            }
        }
    }

    /// `:%s/pat/rep/[g]` (whole buffer) or `:s/pat/rep/[g]` (current line).
    /// `g` replaces every match on a line; otherwise the first per line. Vim
    /// backreferences (`\1`) are translated to the regex crate's `${1}`.
    pub(super) fn ex_substitute(&mut self, whole_buffer: bool, spec: &str, cx: &mut Context<Self>) {
        let body = spec.strip_prefix('/').unwrap_or(spec);
        let mut parts = body.splitn(3, '/');
        let pattern = parts.next().unwrap_or("");
        let replacement = translate_backrefs(parts.next().unwrap_or(""));
        let global = parts.next().unwrap_or("").contains('g');
        if pattern.is_empty() {
            self.echo("Empty pattern", cx);
            return;
        }
        let regex = match Regex::new(pattern) {
            Ok(r) => r,
            Err(err) => {
                self.echo(format!("Invalid regex: {err}"), cx);
                return;
            }
        };

        let substitute = |line: &str| -> String {
            if global {
                regex.replace_all(line, replacement.as_str()).into_owned()
            } else {
                regex.replace(line, replacement.as_str()).into_owned()
            }
        };

        if whole_buffer {
            let text = self.buffer.full_text();
            // split('\n') preserves the line structure (incl. a trailing empty).
            let new: Vec<String> = text.split('\n').map(substitute).collect();
            let new = new.join("\n");
            let len = self.buffer.len();
            let cursor = self.edit(0..len, &new);
            self.collapse_to(cursor.min(self.buffer.len()));
        } else {
            let range = self.buffer.line_range_at(self.point());
            let line = self.buffer.slice_to_string(range.clone());
            let new = substitute(&line);
            let cursor = self.edit(range, &new);
            self.collapse_to(cursor);
        }
        cx.notify();
    }
}
