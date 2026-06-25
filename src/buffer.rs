use std::ops::Range;

use ropey::Rope;
use unicode_segmentation::UnicodeSegmentation;

/// A "word" character for word-motion purposes: alphanumerics plus underscore.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// A Lisp symbol constituent: anything that isn't whitespace or a reader
/// delimiter. Unlike [`is_word_char`], this keeps `-`, `?`, `!`, `>`, `*`, … so
/// `string->number`, `set!`, and `*ring*` count as a single token — needed for
/// symbol completion in the Steel REPL.
fn is_symbol_char(c: char) -> bool {
    !c.is_whitespace()
        && !matches!(
            c,
            '(' | ')' | '[' | ']' | '{' | '}' | '"' | ';' | '\'' | '`' | ','
        )
}

/// Vim-style character classes for `w`/`b`-style word motion: a "word" is a run
/// of [`Word`](CharClass::Word) characters or a run of [`Punct`](CharClass::Punct)
/// characters, separated by [`Space`](CharClass::Space).
#[derive(PartialEq, Eq, Clone, Copy)]
enum CharClass {
    Space,
    Word,
    Punct,
}

fn char_class(c: char) -> CharClass {
    if c.is_whitespace() {
        CharClass::Space
    } else if is_word_char(c) {
        CharClass::Word
    } else {
        CharClass::Punct
    }
}

/// Text storage for a single editing buffer, backed by a [`Rope`] for O(log n)
/// edits, line indexing, and UTF-16 conversion.
///
/// The public API is byte-offset based (UTF-8). Internally rope edits are
/// char-indexed, so byte offsets are converted at the boundary; callers never
/// see chars.
#[derive(Debug, Default)]
pub struct Buffer {
    rope: Rope,
}

impl Buffer {
    pub fn new(initial: impl AsRef<str>) -> Self {
        Self {
            rope: Rope::from_str(initial.as_ref()),
        }
    }

    /// Total length in bytes.
    pub fn len(&self) -> usize {
        self.rope.len_bytes()
    }

    #[allow(dead_code)] // part of the buffer API.
    pub fn is_empty(&self) -> bool {
        self.rope.len_bytes() == 0
    }

    /// Number of lines (line breaks + 1; a trailing `\n` yields a final empty
    /// line, matching how editors render).
    pub fn line_count(&self) -> usize {
        self.rope.len_lines()
    }

    /// Replace byte `range` with `new_text`, returning the byte offset just past
    /// the inserted text — the natural new cursor position.
    pub fn replace(&mut self, range: Range<usize>, new_text: &str) -> usize {
        let start_char = self.rope.byte_to_char(range.start);
        let end_char = self.rope.byte_to_char(range.end);
        if start_char != end_char {
            self.rope.remove(start_char..end_char);
        }
        if !new_text.is_empty() {
            self.rope.insert(start_char, new_text);
        }
        range.start + new_text.len()
    }

    /// Index of the line containing byte `offset`.
    pub fn line_index_at_byte(&self, offset: usize) -> usize {
        self.rope.byte_to_line(offset.min(self.len()))
    }

    /// `(row, byte-column)` of byte `offset` — the position format tree-sitter
    /// wants for incremental edits.
    pub fn point_at(&self, offset: usize) -> (usize, usize) {
        let byte = offset.min(self.len());
        let row = self.rope.byte_to_line(byte);
        let column = byte - self.rope.line_to_byte(row);
        (row, column)
    }

    /// Byte offset for an LSP position (`line`, UTF-16 column from line start).
    pub fn position_to_byte(&self, line: usize, utf16_col: usize) -> usize {
        let line = line.min(self.line_count().saturating_sub(1));
        let line_start_char = self.rope.line_to_char(line);
        let line_start_u16 = self.rope.char_to_utf16_cu(line_start_char);
        let target_u16 = (line_start_u16 + utf16_col).min(self.rope.len_utf16_cu());
        self.rope
            .char_to_byte(self.rope.utf16_cu_to_char(target_u16))
    }

    /// LSP position (`line`, UTF-16 column) for a byte offset.
    pub fn byte_to_position(&self, offset: usize) -> (usize, usize) {
        let byte = offset.min(self.len());
        let line = self.rope.byte_to_line(byte);
        let line_start_char = self.rope.line_to_char(line);
        let char = self.rope.byte_to_char(byte);
        let col = self.rope.char_to_utf16_cu(char) - self.rope.char_to_utf16_cu(line_start_char);
        (line, col)
    }

    /// Byte range of line `i`, excluding any trailing line break.
    pub fn line_range(&self, i: usize) -> Range<usize> {
        let (range, _) = self.line(i);
        range
    }

    /// Byte range of the line containing `offset`, excluding the line break.
    pub fn line_range_at(&self, offset: usize) -> Range<usize> {
        self.line_range(self.line_index_at_byte(offset))
    }

    /// Line `i` as `(byte_range_excluding_break, text)`.
    pub fn line(&self, i: usize) -> (Range<usize>, String) {
        let start = self.rope.line_to_byte(i);
        let raw = self.rope.line(i).to_string();
        let trimmed = raw.strip_suffix('\n').unwrap_or(&raw);
        let trimmed = trimmed.strip_suffix('\r').unwrap_or(trimmed);
        let text = trimmed.to_string();
        (start..start + text.len(), text)
    }

    /// The bytes in `range` as an owned string.
    pub fn slice_to_string(&self, range: Range<usize>) -> String {
        self.rope.byte_slice(range).to_string()
    }

    /// The entire buffer as an owned string.
    #[allow(dead_code)] // handy for debugging / future serialization.
    pub fn full_text(&self) -> String {
        self.rope.to_string()
    }

    /// The underlying rope, for the highlighter to parse/query without copying.
    pub fn rope(&self) -> &Rope {
        &self.rope
    }

    pub fn byte_to_char(&self, byte: usize) -> usize {
        self.rope.byte_to_char(byte.min(self.len()))
    }

    pub fn char_to_byte(&self, char_idx: usize) -> usize {
        self.rope.char_to_byte(char_idx)
    }

    // --- Word motion ------------------------------------------------------

    /// The byte offset at the end of the next word at or after `from` — skips
    /// any leading non-word characters, then the word itself (Emacs `M-f`).
    pub fn forward_word(&self, from: usize) -> usize {
        let total = self.rope.len_chars();
        let mut ci = self.rope.byte_to_char(from.min(self.len()));
        while ci < total && !is_word_char(self.rope.char(ci)) {
            ci += 1;
        }
        while ci < total && is_word_char(self.rope.char(ci)) {
            ci += 1;
        }
        self.rope.char_to_byte(ci)
    }

    /// The byte offset at the start of the next word at or after `from`, using
    /// Vim `w` semantics: skip the rest of the word under the cursor (a run of
    /// word characters or a run of punctuation), then skip whitespace, landing
    /// on the first character of the next word. Unlike [`forward_word`], which
    /// stops at the *end* of a word (Emacs `M-f`), this stops at the *start*.
    pub fn forward_word_start(&self, from: usize) -> usize {
        let total = self.rope.len_chars();
        let mut ci = self.rope.byte_to_char(from.min(self.len()));
        if ci >= total {
            return self.rope.char_to_byte(ci);
        }
        // Skip the current word/punctuation run. If we're already on
        // whitespace there's nothing to skip — we only need to skip below.
        let start = char_class(self.rope.char(ci));
        if start != CharClass::Space {
            while ci < total && char_class(self.rope.char(ci)) == start {
                ci += 1;
            }
        }
        // Skip the whitespace separating us from the next word.
        while ci < total && char_class(self.rope.char(ci)) == CharClass::Space {
            ci += 1;
        }
        self.rope.char_to_byte(ci)
    }

    /// The byte offset at the start of the word at or before `from` (Emacs `M-b`).
    pub fn backward_word(&self, from: usize) -> usize {
        let mut ci = self.rope.byte_to_char(from.min(self.len()));
        while ci > 0 && !is_word_char(self.rope.char(ci - 1)) {
            ci -= 1;
        }
        while ci > 0 && is_word_char(self.rope.char(ci - 1)) {
            ci -= 1;
        }
        self.rope.char_to_byte(ci)
    }

    /// The start of the Lisp symbol ending at `from` — walks back over symbol
    /// constituents (see [`is_symbol_char`]) so a whole token like
    /// `string->number` is returned, not just its last hyphen-delimited piece.
    /// Returns `from` when the preceding char isn't a symbol char (empty prefix).
    pub fn backward_symbol(&self, from: usize) -> usize {
        let mut ci = self.rope.byte_to_char(from.min(self.len()));
        while ci > 0 && is_symbol_char(self.rope.char(ci - 1)) {
            ci -= 1;
        }
        self.rope.char_to_byte(ci)
    }

    // --- Grapheme-aware cursor movement -----------------------------------

    /// The grapheme boundary immediately before `offset` (crossing line breaks).
    pub fn prev_grapheme(&self, offset: usize) -> usize {
        if offset == 0 {
            return 0;
        }
        let line = self.line_range_at(offset);
        if offset <= line.start {
            // At a line start: the preceding byte is the line break.
            return offset - 1;
        }
        let text = self.slice_to_string(line.start..offset);
        let prev = text
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(i, _)| i);
        line.start + prev
    }

    /// The grapheme boundary immediately after `offset` (crossing line breaks).
    pub fn next_grapheme(&self, offset: usize) -> usize {
        let len = self.len();
        if offset >= len {
            return len;
        }
        let line = self.line_range_at(offset);
        if offset >= line.end {
            // At end of line content: step past the line break to the next line.
            return (offset + 1).min(len);
        }
        let text = self.slice_to_string(offset..line.end);
        let next = text.graphemes(true).next().map_or(1, str::len);
        offset + next
    }

    // --- UTF-16 <-> UTF-8 offset conversion, required by the platform IME ---

    pub fn offset_to_utf16(&self, byte: usize) -> usize {
        let ch = self.rope.byte_to_char(byte.min(self.len()));
        self.rope.char_to_utf16_cu(ch)
    }

    pub fn offset_from_utf16(&self, utf16: usize) -> usize {
        let ch = self
            .rope
            .utf16_cu_to_char(utf16.min(self.rope.len_utf16_cu()));
        self.rope.char_to_byte(ch)
    }

    pub fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }

    pub fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lsp_position_conversion_handles_utf16() {
        // "é" (U+00E9) is 2 UTF-8 bytes but a single UTF-16 code unit — the unit
        // LSP positions count. Line 1 begins at byte 4 (after "abc\n").
        let buffer = Buffer::new("abc\ndéf\nghi");

        assert_eq!(buffer.byte_to_position(0), (0, 0)); // 'a'
        assert_eq!(buffer.byte_to_position(4), (1, 0)); // 'd'
        assert_eq!(buffer.byte_to_position(5), (1, 1)); // 'é'
        assert_eq!(buffer.byte_to_position(7), (1, 2)); // 'f' — é counted as 1
        assert_eq!(buffer.byte_to_position(9), (2, 0)); // 'g' (byte 8 is '\n')

        // Round-trips back to the same byte offsets.
        assert_eq!(buffer.position_to_byte(1, 0), 4);
        assert_eq!(buffer.position_to_byte(1, 1), 5);
        assert_eq!(buffer.position_to_byte(1, 2), 7);
        assert_eq!(buffer.position_to_byte(2, 0), 9);
    }

    #[test]
    fn backward_symbol_keeps_whole_lisp_tokens() {
        // `backward_word` splits on '-'/'>' ; `backward_symbol` must not, so REPL
        // completion gets the full token to match and replace.
        let buffer = Buffer::new("(foo string->number");
        let end = buffer.len(); // just past "number"
        assert_eq!(
            &buffer.full_text()[buffer.backward_symbol(end)..end],
            "string->number"
        );
        // Stops at the opening paren (a delimiter), not inside the previous atom.
        let foo_end = 4; // just past "foo"
        assert_eq!(
            &buffer.full_text()[buffer.backward_symbol(foo_end)..foo_end],
            "foo"
        );
        // Right after a delimiter → empty prefix (complete everything).
        assert_eq!(buffer.backward_symbol(1), 1); // just after '('
    }

    #[test]
    fn forward_word_start_lands_on_next_word() {
        // Vim `w`: from the start of "foo", jump to the 'b' of "bar" (byte 4),
        // not to the end of "foo" the way Emacs `M-f` (`forward_word`) would.
        let buffer = Buffer::new("foo bar baz");
        assert_eq!(buffer.forward_word_start(0), 4); // 'b' of "bar"
        assert_eq!(buffer.forward_word_start(4), 8); // 'b' of "baz"
        assert_eq!(buffer.forward_word_start(8), 11); // end of buffer

        // From mid-word it still lands on the start of the following word.
        assert_eq!(buffer.forward_word_start(1), 4);

        // Punctuation is its own word, and runs cross newlines via whitespace.
        let p = Buffer::new("foo.bar\nbaz");
        assert_eq!(p.forward_word_start(0), 3); // '.' starts a punctuation word
        assert_eq!(p.forward_word_start(3), 4); // 'b' of "bar"
        assert_eq!(p.forward_word_start(4), 8); // 'b' of "baz" (skips '\n')
    }
}
