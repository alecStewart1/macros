//! JSON pretty-printer behind `json-pretty-print-buffer` / `json-pretty-print`.
//!
//! A token-level re-printer, deliberately NOT a parse into `serde_json::Value`
//! and back: that round-trip reorders keys, rewrites numbers (`1.0` → `1`,
//! `1e3` → `1000`), and drops duplicate keys. Here every string and scalar is
//! copied byte-for-byte; only the whitespace between tokens is rewritten.
//!
//! Layout matches Prettier / `jq`: one member or element per line, `": "` after
//! keys, empty containers stay `{}` / `[]`. JSONC comments (as in tsconfig or
//! editor settings files) are kept: a comment that sat on the same line as the
//! token before it stays trailing; any other comment gets its own line. A single
//! blank line between members is preserved, runs of them collapse to one.
//!
//! The scanner checks bracket balance and string/comment termination, not the
//! full grammar (a missing comma passes through untouched) — the editor gates on
//! the tree-sitter parse for that.

use std::fmt;

/// Why `src` couldn't be formatted, at a 0-based byte offset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JsonError {
    pub offset: usize,
    pub kind: JsonErrorKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsonErrorKind {
    /// A value right after another one inside a container.
    MissingComma,
    /// A closer that doesn't match the open container.
    Mismatched {
        found: char,
        expected: char,
    },
    /// A closer with no container open.
    Unmatched(char),
    UnterminatedString,
    UnterminatedComment,
    /// The input ended inside a container (missing this closer); the
    /// offset is the end of the input.
    Unclosed(char),
}

impl fmt::Display for JsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let at = self.offset;
        match self.kind {
            JsonErrorKind::MissingComma => write!(f, "missing `,` before byte {at}"),
            JsonErrorKind::Mismatched { found, expected } => {
                write!(
                    f,
                    "mismatched `{found}` at byte {at} (expected `{expected}`)"
                )
            }
            JsonErrorKind::Unmatched(c) => write!(f, "unmatched `{c}` at byte {at}"),
            JsonErrorKind::UnterminatedString => write!(f, "unterminated string at byte {at}"),
            JsonErrorKind::UnterminatedComment => write!(f, "unterminated comment at byte {at}"),
            JsonErrorKind::Unclosed(c) => write!(f, "unclosed container (missing `{c}`)"),
        }
    }
}

impl std::error::Error for JsonError {}

/// Pretty-print `src` with `indent` spaces per nesting level. Errors name the
/// 0-based byte offset of the problem.
pub fn pretty(src: &str, indent: usize) -> Result<String, JsonError> {
    Printer {
        src,
        b: src.as_bytes(),
        pad: " ".repeat(indent),
        out: String::with_capacity(src.len() + src.len() / 2),
        stack: Vec::new(),
        pending: false,
        space_next: false,
        after_value: false,
        i: 0,
    }
    .run()
}

/// The re-printer's state while it walks the tokens.
struct Printer<'a> {
    src: &'a str,
    b: &'a [u8],
    /// One level of indentation.
    pad: String,
    out: String,
    /// Open containers, as their closing byte.
    stack: Vec<u8>,
    /// A line break is owed before the next token (after `{`, `[`, `,`, a
    /// line comment, or a completed top-level value).
    pending: bool,
    /// A trailing block comment wants a space before the token after it.
    space_next: bool,
    /// The last token completed a value inside a container, so only `,`,
    /// `:` or a closer may follow — another value there means a missing
    /// comma.
    after_value: bool,
    /// The read position.
    i: usize,
}

/// Where a token sits relative to the one before it.
#[derive(Clone, Copy)]
struct Spot {
    /// The token's first byte.
    start: usize,
    /// Containers open around it.
    depth: usize,
    /// It shares a source line with the previous token.
    same_line: bool,
    /// A blank line preceded it.
    blank: bool,
}

impl Printer<'_> {
    fn err(offset: usize, kind: JsonErrorKind) -> JsonError {
        JsonError { offset, kind }
    }

    fn run(mut self) -> Result<String, JsonError> {
        while let Some(spot) = self.next_token() {
            let b = self.b;
            let i = spot.start;
            if std::mem::take(&mut self.space_next)
                && !self.pending
                && !matches!(b[i], b',' | b'}' | b']')
            {
                self.out.push(' ');
            }
            let starts_value = !matches!(b[i], b',' | b':' | b'}' | b']') && !self.at_comment(i);
            if starts_value && self.after_value && spot.depth > 0 {
                return Err(Self::err(i, JsonErrorKind::MissingComma));
            }
            if starts_value || matches!(b[i], b',' | b':') {
                // Reset here; the token's handler sets it again once a value
                // completes.
                self.after_value = false;
            }
            match b[i] {
                b'{' | b'[' => self.open(spot),
                b'}' | b']' => self.close(spot)?,
                b',' => {
                    self.out.push(',');
                    self.i += 1;
                    self.pending = true;
                }
                b':' => {
                    self.out.push_str(": ");
                    self.i += 1;
                    self.pending = false;
                }
                b'"' => self.string(spot)?,
                _ if self.at_comment(i) => self.comment(spot)?,
                _ => self.scalar(spot),
            }
        }
        if let Some(&c) = self.stack.last() {
            return Err(Self::err(self.b.len(), JsonErrorKind::Unclosed(c as char)));
        }
        if !self.out.is_empty() {
            self.out.push('\n');
        }
        Ok(self.out)
    }

    /// Skip whitespace to the next token; `None` at the end of the input.
    fn next_token(&mut self) -> Option<Spot> {
        let b = self.b;
        let mut newlines = 0;
        while self.i < b.len() && b[self.i].is_ascii_whitespace() {
            if b[self.i] == b'\n' {
                newlines += 1;
            }
            self.i += 1;
        }
        if self.i >= b.len() {
            return None;
        }
        Some(Spot {
            start: self.i,
            depth: self.stack.len(),
            same_line: newlines == 0 && !self.out.is_empty(),
            blank: newlines >= 2 && !self.out.is_empty(),
        })
    }

    /// A `//` or `/*` comment starts at `i`.
    fn at_comment(&self, i: usize) -> bool {
        self.b[i] == b'/' && matches!(self.b.get(i + 1), Some(b'/' | b'*'))
    }

    /// Break the line and indent to `depth`.
    fn newline(&mut self, depth: usize, blank: bool) {
        // Nothing to break away from at the top of the document.
        if self.out.is_empty() {
            return;
        }
        // A blank line never opens a container's body.
        if blank && !self.out.ends_with(['{', '[']) {
            self.out.push('\n');
        }
        self.out.push('\n');
        for _ in 0..depth {
            self.out.push_str(&self.pad);
        }
    }

    /// Break the line first if one is owed.
    fn owed_newline(&mut self, spot: Spot) {
        if self.pending {
            self.newline(spot.depth, spot.blank);
        }
    }

    fn open(&mut self, spot: Spot) {
        let b = self.b;
        self.owed_newline(spot);
        let open = b[spot.start];
        let close = if open == b'{' { b'}' } else { b']' };
        self.i += 1;
        // An empty container (nothing but whitespace inside) stays `{}`.
        let mut j = self.i;
        while j < b.len() && b[j].is_ascii_whitespace() {
            j += 1;
        }
        self.out.push(open as char);
        if j < b.len() && b[j] == close {
            self.out.push(close as char);
            self.i = j + 1;
            self.pending = spot.depth == 0;
            self.after_value = true;
        } else {
            self.stack.push(close);
            self.pending = true;
        }
    }

    fn close(&mut self, spot: Spot) -> Result<(), JsonError> {
        let found = self.b[spot.start];
        match self.stack.pop() {
            Some(c) if c == found => {}
            Some(c) => {
                return Err(Self::err(
                    spot.start,
                    JsonErrorKind::Mismatched {
                        found: found as char,
                        expected: c as char,
                    },
                ));
            }
            None => {
                return Err(Self::err(
                    spot.start,
                    JsonErrorKind::Unmatched(found as char),
                ));
            }
        }
        self.newline(self.stack.len(), false);
        self.out.push(found as char);
        self.i += 1;
        self.pending = self.stack.is_empty();
        self.after_value = true;
        Ok(())
    }

    fn string(&mut self, spot: Spot) -> Result<(), JsonError> {
        self.i += 1;
        loop {
            match self.b.get(self.i) {
                None => {
                    return Err(Self::err(spot.start, JsonErrorKind::UnterminatedString));
                }
                Some(b'\\') => self.i += 2,
                Some(b'"') => {
                    self.i += 1;
                    break;
                }
                Some(_) => self.i += 1,
            }
        }
        self.owed_newline(spot);
        self.out.push_str(&self.src[spot.start..self.i]);
        self.pending = spot.depth == 0;
        self.after_value = true;
        Ok(())
    }

    fn comment(&mut self, spot: Spot) -> Result<(), JsonError> {
        let b = self.b;
        let start = spot.start;
        let line = b[start + 1] == b'/';
        if line {
            while self.i < b.len() && b[self.i] != b'\n' {
                self.i += 1;
            }
        } else {
            let Some(end) = self.src[start + 2..].find("*/") else {
                return Err(Self::err(start, JsonErrorKind::UnterminatedComment));
            };
            self.i += 2 + end + 2;
        }
        let text = self.src[start..self.i].trim_end();
        if spot.same_line {
            if !self.out.ends_with(' ') {
                self.out.push(' ');
            }
            self.out.push_str(text);
            // A trailing line comment ends the line; a trailing block
            // comment leaves any owed break owed.
            self.space_next = !line;
            self.pending = self.pending || line;
        } else {
            // Own line: a closer is next at most one level out, but the
            // comment belongs with the members, so use the current depth.
            self.newline(spot.depth, spot.blank);
            self.out.push_str(text);
            self.pending = true;
        }
        Ok(())
    }

    /// A bare scalar (number, true/false/null): copied up to the next
    /// delimiter.
    fn scalar(&mut self, spot: Spot) {
        let b = self.b;
        while self.i < b.len()
            && !b[self.i].is_ascii_whitespace()
            && !matches!(b[self.i], b',' | b':' | b'{' | b'}' | b'[' | b']' | b'"')
            && !self.at_comment(self.i)
        {
            self.i += 1;
        }
        self.owed_newline(spot);
        self.out.push_str(&self.src[spot.start..self.i]);
        self.pending = spot.depth == 0;
        self.after_value = true;
    }
}

#[cfg(test)]
mod tests {
    use super::{JsonError, JsonErrorKind, pretty};

    #[test]
    fn minified_object_expands() {
        assert_eq!(
            pretty(r#"{"a":1,"b":[true,null,{"c":"x"}]}"#, 2).unwrap(),
            "{\n  \"a\": 1,\n  \"b\": [\n    true,\n    null,\n    {\n      \"c\": \"x\"\n    }\n  ]\n}\n"
        );
    }

    #[test]
    fn indent_width_is_honored() {
        assert_eq!(pretty("[1,2]", 4).unwrap(), "[\n    1,\n    2\n]\n");
    }

    #[test]
    fn already_pretty_is_a_fixed_point() {
        let src = "{\n  \"a\": {\n    \"b\": [\n      1\n    ]\n  },\n  \"c\": {}\n}\n";
        assert_eq!(pretty(src, 2).unwrap(), src);
        let again = pretty(&pretty("{\"x\" : [ ] ,\"y\":{ }}", 2).unwrap(), 2).unwrap();
        assert_eq!(again, "{\n  \"x\": [],\n  \"y\": {}\n}\n");
    }

    #[test]
    fn scalars_and_strings_are_verbatim() {
        // Numbers keep their spelling, keys keep their order (and duplicates),
        // escapes and brackets inside strings are untouched.
        let src = r#"{"z":1.0,"a":1e3,"a":-0.50,"s":"q\"{[,:]}\\","u":"\u00e9"}"#;
        assert_eq!(
            pretty(src, 2).unwrap(),
            "{\n  \"z\": 1.0,\n  \"a\": 1e3,\n  \"a\": -0.50,\n  \"s\": \"q\\\"{[,:]}\\\\\",\n  \"u\": \"\\u00e9\"\n}\n"
        );
    }

    #[test]
    fn scalar_document_and_badly_indented_input() {
        assert_eq!(pretty("  42  ", 2).unwrap(), "42\n");
        assert_eq!(pretty("", 2).unwrap(), "");
        assert_eq!(
            pretty("{\n\"a\":\n      [1,\n2]\n   }", 2).unwrap(),
            "{\n  \"a\": [\n    1,\n    2\n  ]\n}\n"
        );
    }

    #[test]
    fn comments_are_kept() {
        let src = "{\n// head\n\"a\": 1, // trailing\n/* block */\n\"b\": [2 /* inline */]\n}";
        assert_eq!(
            pretty(src, 2).unwrap(),
            "{\n  // head\n  \"a\": 1, // trailing\n  /* block */\n  \"b\": [\n    2 /* inline */\n  ]\n}\n"
        );
        // A trailing line comment never swallows the token after it.
        assert_eq!(
            pretty("{\"a\": // why\n1}", 2).unwrap(),
            "{\n  \"a\": // why\n  1\n}\n"
        );
        assert_eq!(
            pretty("{\"a\": /* c */ 1}", 2).unwrap(),
            "{\n  \"a\": /* c */ 1\n}\n"
        );
    }

    #[test]
    fn leading_comment_starts_the_document() {
        assert_eq!(
            pretty("// settings\n{\"a\":1}", 2).unwrap(),
            "// settings\n{\n  \"a\": 1\n}\n"
        );
    }

    #[test]
    fn single_blank_lines_between_members_survive() {
        assert_eq!(
            pretty("{\n\n\"a\": 1,\n\n\n\"b\": 2}", 2).unwrap(),
            "{\n  \"a\": 1,\n\n  \"b\": 2\n}\n"
        );
    }

    #[test]
    fn structural_errors_are_reported() {
        let err = |src: &str| pretty(src, 2).unwrap_err().to_string();
        assert_eq!(err(r#"{"a": 1"#), "unclosed container (missing `}`)");
        assert_eq!(err("[1}"), "mismatched `}` at byte 2 (expected `]`)");
        assert_eq!(err("1]"), "unmatched `]` at byte 1");
        assert_eq!(err(r#""abc"#), "unterminated string at byte 0");
        assert_eq!(err("[1 /* x"), "unterminated comment at byte 3");
        assert_eq!(err(r#"{"a": 1 "b": 2}"#), "missing `,` before byte 8");
        assert_eq!(
            pretty("[1 2]", 2).unwrap_err(),
            JsonError {
                offset: 3,
                kind: JsonErrorKind::MissingComma
            }
        );
        assert!(err("[{} []]").contains("missing `,`"));
        // A comment between values is not a value.
        assert!(pretty("[1 /* c */, 2]", 2).is_ok());
    }
}
