//! The `*anvil-repl*` buffer: SLIME-style input handling for the ANVIL REPL
//! (see [`crate::anvil`]).
//!
//! The buffer is a transcript followed by an editable *input region* that runs
//! from [`Document::repl_input`] to the end of the buffer. RET submits the input
//! once it reads as complete forms (otherwise it opens an indented continuation
//! line), and results are inserted just *before* the input region — so typing
//! the next form while a slow one runs never gets interleaved with its output.
//! Everything the REPL writes is a `;;` comment line, so pressing RET on old
//! transcript text can never evaluate a printed value by accident.

use gpui::Context;

use super::*;
use crate::sexp;

/// The REPL buffer's name.
pub(super) const ANVIL_REPL_BUFFER: &str = "*anvil-repl*";

/// The result callback whose outcomes render natively into the REPL transcript
/// (`(anvil-eval form "anvil-on-result")` from Scheme lands there too).
pub(super) const ANVIL_REPL_CALLBACK: &str = "anvil-on-result";

/// The REPL's major mode.
const ANVIL_REPL_MODE: &str = "anvil-repl-mode";

/// Indentation step for continuation lines in the REPL.
const REPL_INDENT: usize = 2;

const BANNER: &str = "\
;; ANVIL — independent Steel REPL.  RET evaluates once the input is complete.
;; C-c C-c interrupt · C-c C-k restart the image · M-p / M-n input history.

";

/// A REPL transcript entry for one evaluation: captured output as `;; ` lines,
/// then the value (`;; => …`) or the error (`;; error: …`). Multi-line values
/// keep every line commented.
pub(super) fn format_repl_result(
    result: &Result<Option<String>, String>,
    output: Option<&str>,
) -> String {
    let mut out = String::new();
    let output = output
        .map(|o| o.trim_end_matches('\n'))
        .filter(|o| !o.is_empty());
    if let Some(text) = output {
        for line in text.lines() {
            out.push_str(if line.is_empty() { ";;" } else { ";; " });
            out.push_str(line);
            out.push('\n');
        }
    }
    let mut lines = |first: &str, rest: &str, text: &str| {
        for (i, line) in text.lines().enumerate() {
            out.push_str(if i == 0 { first } else { rest });
            out.push_str(line);
            out.push('\n');
        }
        if text.is_empty() {
            out.push_str(first.trim_end());
            out.push('\n');
        }
    };
    match result {
        Ok(Some(value)) => lines(";; => ", ";;    ", value),
        Ok(None) if output.is_none() => lines(";; => ", ";;    ", "(no value)"),
        Ok(None) => {}
        Err(error) => lines(";; error: ", ";;        ", error),
    }
    out
}

/// Where the input region starts after an edit replaced `start..old_end` with
/// `new_len` bytes. Edits wholly before it shift it; an edit straddling it
/// pulls it back to the edit's start; text typed at or after it is input.
pub(super) fn shift_repl_input(
    input: usize,
    start: usize,
    old_end: usize,
    new_len: usize,
) -> usize {
    if start >= input {
        input
    } else if old_end <= input {
        input - (old_end - start) + new_len
    } else {
        start
    }
}

impl Editor {
    /// `anvil-repl`: switch to the REPL buffer, creating it (banner, mode, input
    /// region) on first use.
    pub(super) fn anvil_repl_open(&mut self, cx: &mut Context<Self>) {
        if self.active.name == ANVIL_REPL_BUFFER {
            return;
        }
        if let Some(index) = self.inactive_index(ANVIL_REPL_BUFFER) {
            self.activate(index, true, cx);
            return;
        }
        let current = self.snapshot_active();
        self.inactive.push(current);
        self.install_active(make_document(ANVIL_REPL_BUFFER.into(), None, BANNER));
        self.apply_command(EditorCommand::SetMajorMode(ANVIL_REPL_MODE.into()), cx);
        let end = self.active.buffer.len();
        self.active.repl_input = Some(end);
        self.active.modified = false;
        self.move_to(end, cx);
    }

    /// Start of the active buffer's input region. A REPL buffer that lost track
    /// of it (created some other way) falls back to point's line, the old
    /// line-at-a-time behaviour.
    fn repl_input_start(&self) -> usize {
        let len = self.active.buffer.len();
        self.active
            .repl_input
            .unwrap_or_else(|| self.active.buffer.line_range_at(self.point()).start)
            .min(len)
    }

    /// RET in the REPL. In the input region: submit it when it reads as
    /// complete forms, else open an indented continuation line. On earlier
    /// transcript text: copy the form under point down to the input (SLIME).
    pub(super) fn anvil_send_input(&mut self, cx: &mut Context<Self>) {
        let len = self.active.buffer.len();
        let start = self.repl_input_start();
        let point = self.point();
        if point < start {
            let text = self.active.buffer.full_text();
            let form = sexp::top_level_form_at(&text, point)
                .filter(|r| r.end <= start)
                .map(|r| text[r].to_string());
            if let Some(form) = form {
                self.edit(start..len, &form);
            }
            let end = self.active.buffer.len();
            self.move_to(end, cx);
            return;
        }
        let input = self.active.buffer.slice_to_string(start..len);
        let parsed = sexp::parse(&input);
        let complete = !parsed.ends_in_literal && parsed.nodes.iter().all(sexp::Node::is_complete);
        if !complete {
            let before = self.active.buffer.slice_to_string(start..point);
            let indent = if sexp::parse(&before).ends_in_literal {
                0 // string content: no indentation added to it
            } else {
                lisp_indent::lisp_indent_for(&before, REPL_INDENT)
            };
            self.insert_text(&format!("\n{}", " ".repeat(indent)), cx);
            return;
        }
        self.move_to(len, cx);
        self.insert_text("\n", cx);
        let new_start = self.active.buffer.len();
        self.active.repl_input = Some(new_start);
        self.anvil_history_pos = None;
        if parsed.nodes.is_empty() {
            return; // blank or comment-only input: just a fresh prompt line
        }
        let source = input.trim().to_string();
        if self.anvil_history.last() != Some(&source) {
            self.anvil_history.push(source.clone());
        }
        self.anvil.eval(&source, ANVIL_REPL_CALLBACK);
    }

    /// M-p / M-n in the REPL: replace the input with an older / newer entry.
    pub(super) fn anvil_history(&mut self, older: bool, cx: &mut Context<Self>) {
        let n = self.anvil_history.len();
        if n == 0 {
            self.echo("No input history yet", cx);
            return;
        }
        let pos = match (self.anvil_history_pos, older) {
            (None, true) => Some(n - 1),
            (Some(i), true) => Some(i.saturating_sub(1)),
            (Some(i), false) if i + 1 < n => Some(i + 1),
            // Newer than the newest: back to an empty input.
            (Some(_), false) => None,
            (None, false) => {
                self.echo("End of history", cx);
                return;
            }
        };
        self.anvil_history_pos = pos;
        let entry = pos
            .map(|i| self.anvil_history[i].clone())
            .unwrap_or_default();
        let start = self.repl_input_start();
        let len = self.active.buffer.len();
        self.edit(start..len, &entry);
        let end = self.active.buffer.len();
        self.move_to(end, cx);
    }

    /// Insert a finished evaluation's transcript entry just before the REPL's
    /// input region — in place, whether or not the REPL is the active buffer,
    /// and without switching to it.
    pub(super) fn anvil_repl_render(&mut self, entry: &str, cx: &mut Context<Self>) {
        let active = self.active.name == ANVIL_REPL_BUFFER;
        let shift = |p: usize, at: usize| if p >= at { p + entry.len() } else { p };
        let doc = if active {
            &mut self.active
        } else {
            match self.inactive_doc_mut(ANVIL_REPL_BUFFER) {
                Some(doc) => doc,
                None => return, // the REPL buffer was killed
            }
        };
        let len = doc.buffer.len();
        let at = doc.repl_input.unwrap_or(len).min(len);
        doc.buffer.replace(at..at, entry);
        doc.repl_input = Some(at + entry.len());
        let sel = doc.selected_range.clone();
        doc.selected_range = shift(sel.start, at)..shift(sel.end, at);
        if active {
            self.lines.clear();
            self.parse_full();
            self.ensure_cursor_visible();
        } else {
            for pane in self
                .panes
                .iter_mut()
                .filter(|p| p.buffer == ANVIL_REPL_BUFFER)
            {
                pane.point = shift(pane.point, at);
            }
        }
        cx.notify();
    }

    /// eldoc for the REPL: after each keystroke, echo the live image's signature
    /// for the operator of the form being typed (`(string-append |` → its doc).
    /// Only re-queries when that operator changes, and never while an evaluation
    /// is running — the lookup would only queue behind it.
    pub(super) fn anvil_eldoc_tick(&mut self) {
        if self.active.modes.names().last().copied() != Some(ANVIL_REPL_MODE) {
            return;
        }
        let start = self.repl_input_start();
        let point = self.point();
        let operator = (point >= start)
            .then(|| sexp::enclosing_operator(&self.active.buffer.slice_to_string(start..point)))
            .flatten();
        if operator == self.anvil_eldoc_last {
            return;
        }
        self.anvil_eldoc_last = operator.clone();
        if let Some(sym) = operator
            && !self.anvil.is_busy()
        {
            self.anvil
                .query(crate::anvil::AnvilQuery::Signature(sym), "anvil-on-eldoc");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{format_repl_result, shift_repl_input};

    #[test]
    fn transcript_lines_are_all_comments() {
        assert_eq!(format_repl_result(&Ok(Some("3".into())), None), ";; => 3\n");
        assert_eq!(format_repl_result(&Ok(None), None), ";; => (no value)\n");
        // Output with a value: output first, both commented.
        assert_eq!(
            format_repl_result(&Ok(Some("5".into())), Some("computing\n")),
            ";; computing\n;; => 5\n"
        );
        // Output only (void result): no "(no value)" noise.
        assert_eq!(format_repl_result(&Ok(None), Some("hi\n")), ";; hi\n");
        // Errors keep their own prefix even after output.
        assert_eq!(
            format_repl_result(&Err("boom".into()), Some("x\n")),
            ";; x\n;; error: boom\n"
        );
        // Multi-line values stay commented.
        assert_eq!(
            format_repl_result(&Ok(Some("(a\n b)".into())), None),
            ";; => (a\n;;     b)\n"
        );
    }

    #[test]
    fn input_start_follows_edits() {
        // Insert before the input: shifts right.
        assert_eq!(shift_repl_input(10, 2, 2, 3), 13);
        // Delete before the input: shifts left.
        assert_eq!(shift_repl_input(10, 2, 5, 0), 7);
        // Typing at the input start is input: no shift.
        assert_eq!(shift_repl_input(10, 10, 10, 1), 10);
        // An edit straddling the boundary pulls it back.
        assert_eq!(shift_repl_input(10, 8, 12, 0), 8);
        // Edits after the input don't move it.
        assert_eq!(shift_repl_input(10, 12, 14, 0), 10);
    }
}
