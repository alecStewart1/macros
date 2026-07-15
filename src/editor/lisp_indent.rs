//! The Lisp (paren-aligning) indent engine for [`Editor`], extracted from the
//! editor module for size: pure indent computations plus the thin `Editor`
//! methods that apply them. Column alignment under the enclosing form is what
//! the step-based tree-sitter indent model can't express, hence the dedicated
//! engine for Lisp dialects.

use gpui::Context;

use super::*;

/// Languages that get Lisp-style (paren-aware) auto-indentation rather than the
/// generic copy-previous-line-indent behaviour.
pub(super) fn is_lisp_language(lang: &str) -> bool {
    matches!(
        lang,
        "scheme"
            | "macros-scheme"
            | "racket"
            | "lisp"
            | "commonlisp"
            | "common-lisp"
            | "clojure"
            | "clojurescript"
            | "elisp"
            | "emacs-lisp"
            | "fennel"
            | "janet"
    )
}

/// Whether a form headed by `head` indents its body by two columns (special
/// forms / macros: `define`, `let`, `when`, …) rather than aligning
/// continuation lines under the first argument (ordinary function calls).
fn lisp_form_indents_body(head: &str) -> bool {
    if head.starts_with("define") || head.starts_with("with-") {
        return true;
    }
    matches!(
        head,
        "lambda"
            | "λ"
            | "named-lambda"
            | "case-lambda"
            | "let"
            | "let*"
            | "letrec"
            | "letrec*"
            | "let-values"
            | "let*-values"
            | "let-syntax"
            | "letrec-syntax"
            | "parameterize"
            | "fluid-let"
            | "when"
            | "unless"
            | "while"
            | "until"
            | "cond"
            | "case"
            | "match"
            | "match-let"
            | "match-let*"
            | "begin"
            | "do"
            | "dynamic-wind"
            | "guard"
            | "syntax-rules"
            | "syntax-case"
            | "for"
            | "delay"
            | "unwind-protect"
            | "module"
            | "library"
    )
}

/// Target indentation column for a line whose preceding text — from the start
/// of the enclosing top-level form (a line beginning at column 0) up to the
/// insertion point — is `text`. A pragmatic Lisp indenter: continuation lines
/// align under the enclosing form's first argument, while the body of a known
/// special form indents `tab_width` columns past its opening paren (the same
/// `tab-width` option the tree-sitter languages use). Strings, line and block
/// comments, and char literals are skipped so their parens don't count.
pub(super) fn lisp_indent_for(text: &str, tab_width: usize) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    // Open delimiters still unclosed at the end: (column, char-index just past
    // it, delimiter char).
    let mut stack: Vec<(usize, usize, char)> = Vec::new();
    let mut i = 0usize;
    let mut col = 0usize;
    while i < n {
        match chars[i] {
            '\n' => {
                col = 0;
                i += 1;
            }
            ';' => {
                while i < n && chars[i] != '\n' {
                    i += 1; // leave the '\n' to reset col on the next pass
                }
            }
            '"' => {
                i += 1;
                col += 1;
                while i < n {
                    match chars[i] {
                        '\\' => {
                            i += 1;
                            col += 1;
                            if i < n {
                                if chars[i] == '\n' {
                                    col = 0;
                                } else {
                                    col += 1;
                                }
                                i += 1;
                            }
                        }
                        '"' => {
                            i += 1;
                            col += 1;
                            break;
                        }
                        '\n' => {
                            col = 0;
                            i += 1;
                        }
                        _ => {
                            col += 1;
                            i += 1;
                        }
                    }
                }
            }
            '#' if i + 1 < n && chars[i + 1] == '\\' => {
                // Char literal `#\x` — the `#`, the `\`, and the literal char
                // are skipped (trailing letters of a named char like `#\space`
                // are harmless symbol chars that hold no parens).
                i += 2;
                col += 2;
                if i < n {
                    if chars[i] == '\n' {
                        col = 0;
                    } else {
                        col += 1;
                    }
                    i += 1;
                }
            }
            '#' if i + 1 < n && chars[i + 1] == '|' => {
                // Nestable block comment `#| ... |#`.
                i += 2;
                col += 2;
                let mut depth = 1usize;
                while i < n && depth > 0 {
                    if chars[i] == '#' && i + 1 < n && chars[i + 1] == '|' {
                        depth += 1;
                        i += 2;
                        col += 2;
                    } else if chars[i] == '|' && i + 1 < n && chars[i + 1] == '#' {
                        depth -= 1;
                        i += 2;
                        col += 2;
                    } else if chars[i] == '\n' {
                        col = 0;
                        i += 1;
                    } else {
                        col += 1;
                        i += 1;
                    }
                }
            }
            '(' | '[' | '{' => {
                stack.push((col, i + 1, chars[i]));
                i += 1;
                col += 1;
            }
            ')' | ']' | '}' => {
                stack.pop();
                i += 1;
                col += 1;
            }
            _ => {
                i += 1;
                col += 1;
            }
        }
    }

    let Some(&(open_col, after, open)) = stack.last() else {
        return 0; // top level
    };

    // Walk from just past the opener to the head token / first argument,
    // tracking columns. The char at `after` sits at column open_col + 1.
    let mut j = after;
    let mut c = open_col + 1;
    while j < n && (chars[j] == ' ' || chars[j] == '\t') {
        j += 1;
        c += 1;
    }
    // Nothing but a comment after the opener on its line → indent one past it.
    if lisp_line_end(&chars, j) {
        return open_col + 1;
    }
    // Data lists / vectors, or a list used as the head, align under the first
    // element rather than applying special-form rules.
    if open != '(' || matches!(chars[j], '(' | '[' | '{') {
        return c;
    }
    let head_start = j;
    while j < n
        && !chars[j].is_whitespace()
        && !matches!(chars[j], '(' | ')' | '[' | ']' | '{' | '}')
    {
        j += 1;
        c += 1;
    }
    if lisp_form_indents_body(&chars[head_start..j].iter().collect::<String>()) {
        return open_col + tab_width;
    }
    // Function call: align under the first argument when it shares the head's
    // line; otherwise one past the opener.
    while j < n && (chars[j] == ' ' || chars[j] == '\t') {
        j += 1;
        c += 1;
    }
    if lisp_line_end(&chars, j) {
        return open_col + 1;
    }
    c
}

/// Whether position `j` in `chars` is the end of meaningful content on its line:
/// end of input, a newline, or the start of a line (`;`) or block (`#|`)
/// comment. Used to decide there's no argument left to align under.
fn lisp_line_end(chars: &[char], j: usize) -> bool {
    j >= chars.len()
        || chars[j] == '\n'
        || chars[j] == ';'
        || (chars[j] == '#' && j + 1 < chars.len() && chars[j + 1] == '|')
}

impl Editor {
    /// Byte offset of the nearest enclosing top-level form at or before `pos` —
    /// the start of the last line whose first character is `(`. Falls back to
    /// the buffer start. This bounds the Lisp indent scan to one defun, the
    /// same heuristic Emacs `beginning-of-defun` uses for Lisp.
    pub(super) fn lisp_defun_start(&self, pos: usize) -> usize {
        let mut i = self.active.buffer.line_index_at_byte(pos);
        loop {
            let range = self.active.buffer.line_range(i);
            if self
                .active
                .buffer
                .slice_to_string(range.clone())
                .starts_with('(')
            {
                return range.start;
            }
            if i == 0 {
                return 0;
            }
            i -= 1;
        }
    }

    /// Reindent the current line to its Lisp indentation (Emacs TAB in a Lisp
    /// buffer).
    pub(super) fn lisp_reindent_line(&mut self, cx: &mut Context<Self>) {
        let tab = self.structural_indent_width(cx);
        let line = self.active.buffer.line_range_at(self.point());
        let target = lisp_indent_for(
            &self
                .active
                .buffer
                .slice_to_string(self.lisp_defun_start(line.start)..line.start),
            tab,
        );
        self.reindent_current_line(target, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::{is_lisp_language, lisp_indent_for};

    #[test]
    fn lisp_indent_aligns_and_bodies() {
        // Body offset tracks `tab-width`; alignment cases are width-independent.
        // Here we pass the classic 2-column width.
        let ind = |s| lisp_indent_for(s, 2);
        // Top level: no enclosing form.
        assert_eq!(ind(""), 0);
        assert_eq!(ind("(define x 1)\n"), 0);
        // Function call: continuation aligns under the first argument.
        assert_eq!(ind("(foo bar\n"), 5);
        // Operator alone on its line → one past the opener.
        assert_eq!(ind("(foo\n"), 1);
        // Special form indents its body `tab-width` columns past the opener.
        assert_eq!(ind("(define (f x)\n"), 2);
        assert_eq!(ind("(let ((x 1))\n"), 2);
        assert_eq!(ind("(when ready\n"), 2);
        // Nesting: indent follows the innermost open form, offset by its column.
        assert_eq!(ind("(define (f x)\n  (when y\n"), 4);
        // A list used as the head aligns under it (let-binding lists).
        assert_eq!(ind("(let ((a 1)\n"), 6);
    }

    #[test]
    fn lisp_indent_body_follows_tab_width() {
        // The body offset is `tab-width`, configurable from init.scm. Alignment
        // under the first argument stays column-based regardless of the width.
        assert_eq!(lisp_indent_for("(define (f x)\n", 4), 4);
        assert_eq!(lisp_indent_for("(when ready\n", 8), 8);
        assert_eq!(lisp_indent_for("(define (f x)\n    (when y\n", 4), 8);
        // Function-call alignment is unaffected by the width setting.
        assert_eq!(lisp_indent_for("(foo bar\n", 4), 5);
        assert_eq!(lisp_indent_for("(foo bar\n", 2), 5);
    }

    #[test]
    fn lisp_indent_skips_strings_and_comments() {
        // None of these hit the body branch, so the width is irrelevant.
        let ind = |s| lisp_indent_for(s, 4);
        // A paren inside a string is not an open form.
        assert_eq!(ind("(foo \"(\"\n"), 5);
        // A paren inside a line comment is ignored.
        assert_eq!(ind("(foo ; (\n"), 1);
        // Char literal `#\(` is not an opener; aligns under the first arg `c`.
        assert_eq!(ind("(char=? c #\\(\n"), 8);
        // Block comment hides its parens.
        assert_eq!(ind("(foo #| ( |#\n"), 1);
    }

    #[test]
    fn lisp_language_detection() {
        assert!(is_lisp_language("scheme"));
        assert!(is_lisp_language("macros-scheme"));
        assert!(is_lisp_language("clojure"));
        assert!(!is_lisp_language("rust"));
        assert!(!is_lisp_language("python"));
    }
}
