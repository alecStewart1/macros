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

/// Pretty-print `src` with `indent` spaces per nesting level. Errors name the
/// 0-based byte offset of the problem.
pub fn pretty(src: &str, indent: usize) -> Result<String, String> {
    let b = src.as_bytes();
    let pad = " ".repeat(indent);
    let mut out = String::with_capacity(src.len() + src.len() / 2);
    // Open containers, as their closing byte.
    let mut stack: Vec<u8> = Vec::new();
    // A line break is owed before the next token (after `{`, `[`, `,`, a line
    // comment, or a completed top-level value).
    let mut pending = false;
    // A trailing block comment wants a space before the token after it.
    let mut space_next = false;
    // The last token completed a value inside a container, so only `,`, `:`
    // or a closer may follow — another value there means a missing comma.
    let mut after_value = false;
    let mut i = 0;

    let newline = |out: &mut String, depth: usize, blank: bool| {
        // Nothing to break away from at the top of the document.
        if out.is_empty() {
            return;
        }
        // A blank line never opens a container's body.
        if blank && !out.ends_with(['{', '[']) {
            out.push('\n');
        }
        out.push('\n');
        for _ in 0..depth {
            out.push_str(&pad);
        }
    };

    loop {
        // Skip whitespace, noting how many newlines it held.
        let mut newlines = 0;
        while i < b.len() && b[i].is_ascii_whitespace() {
            if b[i] == b'\n' {
                newlines += 1;
            }
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        // Whether this token shares a source line with the previous one, and
        // whether a blank line preceded it.
        let same_line = newlines == 0 && !out.is_empty();
        let blank = newlines >= 2 && !out.is_empty();
        if std::mem::take(&mut space_next) && !pending && !matches!(b[i], b',' | b'}' | b']') {
            out.push(' ');
        }
        let depth = stack.len();
        let start = i;
        let starts_value = !matches!(b[i], b',' | b':' | b'}' | b']')
            && !(b[i] == b'/' && matches!(b.get(i + 1), Some(b'/' | b'*')));
        if starts_value && after_value && depth > 0 {
            return Err(format!("missing `,` before byte {i}"));
        }
        if starts_value || matches!(b[i], b',' | b':') {
            // Reset here; the arms below set it again once a value completes.
            after_value = false;
        }
        match b[i] {
            b'{' | b'[' => {
                if pending {
                    newline(&mut out, depth, blank);
                }
                let close = if b[i] == b'{' { b'}' } else { b']' };
                i += 1;
                // An empty container (nothing but whitespace inside) stays `{}`.
                let mut j = i;
                while j < b.len() && b[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j < b.len() && b[j] == close {
                    out.push(b[start] as char);
                    out.push(close as char);
                    i = j + 1;
                    pending = depth == 0;
                    after_value = true;
                } else {
                    out.push(b[start] as char);
                    stack.push(close);
                    pending = true;
                }
            }
            b'}' | b']' => {
                match stack.pop() {
                    Some(c) if c == b[i] => {}
                    Some(c) => {
                        return Err(format!(
                            "mismatched `{}` at byte {i} (expected `{}`)",
                            b[i] as char, c as char
                        ));
                    }
                    None => return Err(format!("unmatched `{}` at byte {i}", b[i] as char)),
                }
                newline(&mut out, stack.len(), false);
                out.push(b[i] as char);
                i += 1;
                pending = stack.is_empty();
                after_value = true;
            }
            b',' => {
                out.push(',');
                i += 1;
                pending = true;
            }
            b':' => {
                out.push_str(": ");
                i += 1;
                pending = false;
            }
            b'"' => {
                i += 1;
                loop {
                    match b.get(i) {
                        None => return Err(format!("unterminated string at byte {start}")),
                        Some(b'\\') => i += 2,
                        Some(b'"') => {
                            i += 1;
                            break;
                        }
                        Some(_) => i += 1,
                    }
                }
                if pending {
                    newline(&mut out, depth, blank);
                }
                out.push_str(&src[start..i]);
                pending = depth == 0;
                after_value = true;
            }
            b'/' if b.get(i + 1) == Some(&b'/') || b.get(i + 1) == Some(&b'*') => {
                let line = b[i + 1] == b'/';
                if line {
                    while i < b.len() && b[i] != b'\n' {
                        i += 1;
                    }
                } else {
                    let Some(end) = src[i + 2..].find("*/") else {
                        return Err(format!("unterminated comment at byte {start}"));
                    };
                    i += 2 + end + 2;
                }
                let text = src[start..i].trim_end();
                if same_line {
                    if !out.ends_with(' ') {
                        out.push(' ');
                    }
                    out.push_str(text);
                    // A trailing line comment ends the line; a trailing block
                    // comment leaves any owed break owed.
                    space_next = !line;
                    pending = pending || line;
                } else {
                    // Own line: a closer is next at most one level out, but the
                    // comment belongs with the members, so use the current depth.
                    newline(&mut out, depth, blank);
                    out.push_str(text);
                    pending = true;
                }
            }
            _ => {
                // A bare scalar (number, true/false/null): copy up to the next
                // delimiter.
                while i < b.len()
                    && !b[i].is_ascii_whitespace()
                    && !matches!(b[i], b',' | b':' | b'{' | b'}' | b'[' | b']' | b'"')
                    && !(b[i] == b'/' && matches!(b.get(i + 1), Some(b'/' | b'*')))
                {
                    i += 1;
                }
                if pending {
                    newline(&mut out, depth, blank);
                }
                out.push_str(&src[start..i]);
                pending = depth == 0;
                after_value = true;
            }
        }
    }
    if let Some(c) = stack.last() {
        return Err(format!("unclosed container (missing `{}`)", *c as char));
    }
    if !out.is_empty() {
        out.push('\n');
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::pretty;

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
        assert!(pretty("{\"a\": 1", 2).unwrap_err().contains("unclosed"));
        assert!(pretty("[1}", 2).unwrap_err().contains("mismatched"));
        assert!(pretty("1]", 2).unwrap_err().contains("unmatched"));
        assert!(
            pretty("\"abc", 2)
                .unwrap_err()
                .contains("unterminated string")
        );
        assert!(
            pretty("[1 /* x", 2)
                .unwrap_err()
                .contains("unterminated comment")
        );
        assert!(
            pretty(r#"{"a": 1 "b": 2}"#, 2)
                .unwrap_err()
                .contains("missing `,`")
        );
        assert!(pretty("[1 2]", 2).unwrap_err().contains("missing `,`"));
        assert!(pretty("[{} []]", 2).unwrap_err().contains("missing `,`"));
        // A comment between values is not a value.
        assert!(pretty("[1 /* c */, 2]", 2).is_ok());
    }
}
