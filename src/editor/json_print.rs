//! Whole-buffer / region layout commands for [`Editor`]: the JSON
//! pretty-printer (`json-pretty-print-buffer`, `json-pretty-print`) and the
//! structural `reindent-region` (Emacs `C-M-\`).

use gpui::Context;

use super::*;

impl Editor {
    /// `json-pretty-print-buffer`: re-lay out the whole buffer as indented JSON
    /// (see [`crate::json_format`]), one undoable edit. Refuses — leaving the
    /// buffer alone — when a JSON buffer's parse has errors, so a half-typed
    /// document is never mangled; safe to run from `before-save-hook`.
    pub(super) fn json_pretty_print_buffer(&mut self, cx: &mut Context<Self>) {
        if self.active_language().as_deref() == Some("json")
            && let Some(tree) = self.active.highlighter.as_ref().and_then(|h| h.tree())
            && tree.root_node().has_error()
        {
            self.status = "JSON has syntax errors; not formatted".into();
            cx.notify();
            return;
        }
        let old = self
            .active
            .buffer
            .slice_to_string(0..self.active.buffer.len());
        let step = self.structural_indent_width(cx).max(1);
        match crate::json_format::pretty(&old, step) {
            Ok(new) if new == old => {}
            Ok(new) => {
                let p = map_point_by_content(&old, &new, self.point());
                self.replace_whole_buffer(&new, p, cx);
                self.ensure_cursor_visible();
            }
            Err(e) => self.status = format!("JSON not formatted: {e}").into(),
        }
        cx.notify();
    }

    /// `json-pretty-print`: pretty-print the JSON value in the region (the
    /// whole buffer with no region). Lines after the first keep the indentation
    /// of the region's first line, so a value embedded in a larger file stays
    /// nested where it was.
    pub(super) fn json_pretty_print_region(&mut self, cx: &mut Context<Self>) {
        let Some(region) = self.region() else {
            self.json_pretty_print_buffer(cx);
            return;
        };
        let old = self.active.buffer.slice_to_string(region.clone());
        let step = self.structural_indent_width(cx).max(1);
        let new = match crate::json_format::pretty(&old, step) {
            Ok(new) => new,
            Err(e) => {
                self.status = format!("JSON not formatted: {e}").into();
                cx.notify();
                return;
            }
        };
        let line = self.active.buffer.line_range_at(region.start);
        let base: String = self
            .active
            .buffer
            .slice_to_string(line.start..region.start)
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        let mut body = new
            .trim_end_matches('\n')
            .replace('\n', &format!("\n{base}"));
        // Blank lines carry no indentation.
        body = body.replace(&format!("\n{base}\n"), "\n\n");
        if old.ends_with('\n') {
            body.push('\n');
        }
        let cursor = self.edit(region, &body);
        self.collapse_to(cursor);
        self.ensure_cursor_visible();
        cx.notify();
    }

    /// `reindent-region` (Emacs `indent-region`, `C-M-\`): reindent every line
    /// of the region — the whole buffer with no region — from the language's
    /// structure, as one undo unit. Unlike `indent-region` (the vim `>` shift),
    /// this sets each line to its computed depth. Refused for buffers with no
    /// structural indenter, where it could only flatten the text.
    pub(super) fn reindent_region(&mut self, cx: &mut Context<Self>) {
        let structural = self
            .active_language()
            .as_deref()
            .is_some_and(is_lisp_language)
            || self
                .active
                .highlighter
                .as_ref()
                .is_some_and(|h| h.has_indent_query());
        if !structural {
            self.status = "No indentation rules for this buffer".into();
            cx.notify();
            return;
        }
        let (first, last) = match self.region() {
            Some(r) => (
                self.active.buffer.line_index_at_byte(r.start),
                self.active
                    .buffer
                    .line_index_at_byte(r.end.saturating_sub(1).max(r.start)),
            ),
            None => (0, self.active.buffer.line_count().saturating_sub(1)),
        };
        // Keep point on its line, the same distance past the first non-blank.
        let p = self.point();
        let row = self.active.buffer.line_index_at_byte(p);
        let fnb = self.first_non_blank(self.active.buffer.line_range(row).start);
        let past = p.saturating_sub(fnb);
        self.reindent_lines(first, last, cx);
        let lr = self.active.buffer.line_range(row);
        let fnb = self.first_non_blank(lr.start);
        self.collapse_to((fnb + past).min(lr.end));
        self.ensure_cursor_visible();
        cx.notify();
    }
}

/// Where `p` (a byte in `old`) lands in `new`, when the two differ only in
/// whitespace: the position just past the same count of non-whitespace chars.
fn map_point_by_content(old: &str, new: &str, p: usize) -> usize {
    let seen = old[..p.min(old.len())]
        .chars()
        .filter(|c| !c.is_whitespace())
        .count();
    if seen == 0 {
        return 0;
    }
    let mut n = 0;
    for (i, c) in new.char_indices() {
        if !c.is_whitespace() {
            n += 1;
            if n == seen {
                return i + c.len_utf8();
            }
        }
    }
    new.len()
}

#[cfg(test)]
mod tests {
    use super::map_point_by_content;

    #[test]
    fn point_follows_content_across_reformat() {
        let old = r#"{"a":1,"b":2}"#;
        let new = "{\n  \"a\": 1,\n  \"b\": 2\n}\n";
        // Just past `1` in the old text…
        let p = old.find('1').unwrap() + 1;
        // …is just past `1` in the new.
        assert_eq!(
            map_point_by_content(old, new, p),
            new.find('1').unwrap() + 1
        );
        assert_eq!(map_point_by_content(old, new, 0), 0);
        assert_eq!(
            map_point_by_content(old, new, old.len()),
            new.find('}').unwrap() + 1
        );
    }
}
