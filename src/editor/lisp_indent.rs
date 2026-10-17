//! The Lisp (paren-aligning) indent engine for [`Editor`], extracted from the
//! editor module for size: pure indent computations plus the thin `Editor`
//! methods that apply them. Column alignment under the enclosing form is what
//! the step-based tree-sitter indent model can't express, hence the dedicated
//! engine for Lisp dialects.

use std::collections::HashMap;

use gpui::Context;

use super::*;
use crate::sexp::Kind;

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

/// How a form indents its continuation lines, by its head symbol — Emacs's
/// `lisp-indent-function` property. Built-in rules come from
/// [`builtin_rule`]; `(lisp-indent-rule "name" "spec")` overrides them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LispIndent {
    /// An ordinary call: continuation lines align under the first argument.
    Function,
    /// A definer (`define`, `with-…`): the body indents one step.
    Body,
    /// The first `n` arguments are "distinguished" (`let`'s bindings,
    /// `lambda`'s formals, `when`'s test): one still to come on its own line
    /// indents two steps, and the body after them one step.
    Special(usize),
    /// Clause lists (`cond`): align under the first clause when it shares the
    /// head's line, else indent one step.
    Clauses,
}

impl LispIndent {
    /// Parse a rule spec: `body`, `function`, `clauses`, or a count of
    /// distinguished arguments.
    pub fn parse(spec: &str) -> Option<Self> {
        match spec.trim() {
            "body" | "defun" => Some(Self::Body),
            "function" | "call" => Some(Self::Function),
            "clauses" => Some(Self::Clauses),
            n => n.parse().ok().map(Self::Special),
        }
    }
}

/// User indent rules (`lisp-indent-rule`), by head symbol.
pub type LispIndentRules = HashMap<String, LispIndent>;

/// The built-in rule for a form's head, after Emacs's `scheme-mode` /
/// `lisp-mode` conventions.
fn builtin_rule(head: &str) -> LispIndent {
    use LispIndent::*;
    if head.starts_with("define") || head.starts_with("with-") || head.starts_with("def") {
        return Body;
    }
    match head {
        "begin" | "dynamic-wind" | "unwind-protect" | "module" | "library" | "delay"
        | "delay-force" | "make-parameter" => Body,
        "lambda" | "λ" | "case-lambda" | "let" | "let*" | "letrec" | "letrec*" | "let-values"
        | "let*-values" | "let-syntax" | "letrec-syntax" | "parameterize" | "fluid-let"
        | "when" | "unless" | "while" | "until" | "case" | "match" | "match-lambda"
        | "match-lambda*" | "match-let" | "match-let*" | "match-define" | "syntax-rules"
        | "guard" | "for" | "for*" | "for/list" | "for/fold" | "with-handler"
        | "call-with-values" | "receive" | "struct" => Special(1),
        "named-lambda" | "do" | "syntax-case" | "destructuring-bind" => Special(2),
        "cond" => Clauses,
        _ => Function,
    }
}

/// The column of byte `pos` in `text` (chars since the line start).
fn column(text: &str, pos: usize) -> usize {
    let line_start = text[..pos].rfind('\n').map_or(0, |i| i + 1);
    text[line_start..pos].chars().count()
}

/// Target indentation column for a line whose preceding text — from the start
/// of the enclosing top-level form up to the insertion point — is `text`. The
/// innermost list still open at the end of `text` decides: data (quoted lists,
/// vectors, `[ ]` / `{ }`) and lists headed by a list align under their first
/// element; otherwise the head's [`LispIndent`] rule (user `rules` first, then
/// [`builtin_rule`]) picks between aligning under the first argument and a
/// body offset of `tab_width` (the same `tab-width` option tree-sitter
/// languages use). Strings, comments and char literals are skipped by the
/// reader ([`crate::sexp`]), so their parens never count.
pub(super) fn lisp_indent_for(text: &str, tab_width: usize, rules: &LispIndentRules) -> usize {
    let parsed = crate::sexp::parse(text);
    let mut list = None;
    let mut nodes = &parsed.nodes;
    while let Some(last) = nodes.last().filter(|n| !n.is_complete()) {
        if !matches!(last.kind, Kind::List { .. }) {
            break; // an unterminated string: the caller skips those lines
        }
        list = Some(last);
        nodes = &last.children;
    }
    let Some(list) = list else {
        return 0; // top level
    };
    let Kind::List { open, .. } = list.kind else {
        unreachable!()
    };
    let open_col = column(text, open);
    let Some(first) = list.children.first() else {
        return open_col + 1; // nothing after the opener yet
    };
    let quoted = list.start < open && matches!(text.as_bytes()[list.start], b'\'' | b'`' | b'#');
    if text.as_bytes()[open] != b'(' || quoted || !matches!(first.kind, Kind::Atom) {
        return column(text, first.start);
    }
    let head = &text[first.start..first.end];
    let args = &list.children[1..];
    let rule = rules
        .get(head)
        .copied()
        .unwrap_or_else(|| builtin_rule(head));
    let same_line = |pos: usize| !text[first.end..pos].contains('\n');
    match rule {
        LispIndent::Body => open_col + tab_width,
        LispIndent::Special(n) if args.len() < n => open_col + 2 * tab_width,
        LispIndent::Special(_) => open_col + tab_width,
        LispIndent::Clauses => match args.first() {
            Some(a) if same_line(a.start) => column(text, a.start),
            _ => open_col + tab_width,
        },
        LispIndent::Function => match args.first() {
            Some(a) => column(text, a.start),
            None => open_col + 1,
        },
    }
}

/// Largest prefix (bytes) the Lisp indenter reads from the buffer start.
const LISP_READ_LIMIT: usize = 1024 * 1024;

impl Editor {
    /// Byte offset of the enclosing top-level form at or before `pos` (the
    /// beginning-of-defun the indent scan starts from), or `pos` itself when it
    /// sits at top level. Read from the buffer start, so a `(` at column 0
    /// inside a docstring is never mistaken for a form. A line opening with `(`
    /// at column 0 after the reader's form start still wins, as Emacs's
    /// `beginning-of-defun` does, so one unclosed form earlier in the buffer
    /// doesn't skew the indentation of everything after it. Past
    /// [`LISP_READ_LIMIT`] only that column-0 heuristic is used.
    pub(super) fn lisp_defun_start(&self, pos: usize) -> usize {
        let candidate = self.column0_paren_line_before(pos);
        if pos > LISP_READ_LIMIT {
            return candidate.unwrap_or(0);
        }
        let text = self.active.buffer.slice_to_string(0..pos);
        match (candidate, crate::sexp::open_form_start(&text)) {
            (Some(c), Some(open))
                if c > open && !crate::sexp::parse(&text[..c]).ends_in_literal =>
            {
                c
            }
            (_, Some(open)) => open,
            (_, None) => pos,
        }
    }

    /// Start of the nearest line at or before `pos` whose first byte is `(`.
    fn column0_paren_line_before(&self, pos: usize) -> Option<usize> {
        let mut i = self.active.buffer.line_index_at_byte(pos);
        loop {
            let range = self.active.buffer.line_range(i);
            // `(` is ASCII: test the line's first byte, no string needed.
            if !range.is_empty()
                && range.start <= pos
                && self.active.buffer.rope().byte(range.start) == b'('
            {
                return Some(range.start);
            }
            if i == 0 {
                return None;
            }
            i -= 1;
        }
    }

    /// The text the Lisp indenter scans for a line starting at `pos`: from the
    /// enclosing top-level form up to `pos`. `None` when `pos` is inside a string
    /// or block comment, whose content must not be reindented.
    pub(super) fn lisp_indent_context(&self, pos: usize) -> Option<String> {
        // From the start of the form's *line*: columns are measured from the
        // start of the text, so a form opening mid-line (`x (foo`) must not
        // have its own `(` taken for column 0.
        let defun = self.lisp_defun_start(pos);
        let from = self.active.buffer.line_range_at(defun).start;
        let text = self.active.buffer.slice_to_string(from..pos);
        (!crate::sexp::parse(&text).ends_in_literal).then_some(text)
    }

    /// Reindent the current line to its Lisp indentation (Emacs TAB in a Lisp
    /// buffer). A line that starts inside a multi-line string is left alone.
    pub(super) fn lisp_reindent_line(&mut self, cx: &mut Context<Self>) {
        let tab = self.structural_indent_width(cx);
        let line = self.active.buffer.line_range_at(self.point());
        if let Some(text) = self.lisp_indent_context(line.start) {
            let target = lisp_indent_for(&text, tab, &self.lisp_indent_rules);
            self.reindent_current_line(target, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{LispIndent, LispIndentRules, is_lisp_language};

    fn lisp_indent_for(text: &str, tab: usize) -> usize {
        super::lisp_indent_for(text, tab, &LispIndentRules::new())
    }

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

    #[test]
    fn lisp_indent_follows_emacs_conventions() {
        let ind = |s| lisp_indent_for(s, 2);
        // A distinguished argument still to come gets two steps.
        assert_eq!(ind("(let\n"), 4);
        assert_eq!(ind("(when\n"), 4);
        assert_eq!(ind("(do ((i 0 (+ i 1)))\n"), 4);
        assert_eq!(ind("(do ((i 0 (+ i 1)))\n    ((= i 3))\n"), 2);
        // cond clauses align under the first clause on the head's line…
        assert_eq!(ind("(cond ((a) 1)\n"), 6);
        // …else one step.
        assert_eq!(ind("(cond\n"), 2);
        // Quoted data and vectors align under their first element.
        assert_eq!(ind("'(alpha beta\n"), 2);
        assert_eq!(ind("#(1 2\n"), 2);
        // Unquoted code inside a quasiquote is code again.
        assert_eq!(ind("`(a ,(foo bar\n"), 10);
        // Later arguments follow the first argument's column.
        assert_eq!(ind("(foo\n bar\n"), 1);
        // A string holding a paren on a continuation line.
        assert_eq!(ind("(foo \"a)\"\n"), 5);
    }

    #[test]
    fn lisp_indent_user_rules_override_builtins() {
        let mut rules = LispIndentRules::new();
        rules.insert("with-db".into(), LispIndent::Function);
        rules.insert("my-let".into(), LispIndent::Special(1));
        rules.insert("frob".into(), LispIndent::Body);
        let ind = |s| super::lisp_indent_for(s, 2, &rules);
        assert_eq!(ind("(with-db conn\n"), 9);
        assert_eq!(ind("(my-let ((a 1))\n"), 2);
        assert_eq!(ind("(my-let\n"), 4);
        assert_eq!(ind("(frob a b\n"), 2);
        assert_eq!(LispIndent::parse("body"), Some(LispIndent::Body));
        assert_eq!(LispIndent::parse("2"), Some(LispIndent::Special(2)));
        assert_eq!(LispIndent::parse("nope"), None);
    }
}
