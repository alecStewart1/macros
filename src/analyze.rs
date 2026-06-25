//! Lightweight static analysis of Steel/Scheme source over Steel's own AST.
//!
//! Two products, both built on one scope model:
//!   * [`bindings_in_scope_at`] — local bindings visible at a byte offset, used
//!     to rank in-scope names first in completion, and
//!   * [`unbound_operators`] — identifiers used in call position that resolve to
//!     no binding or known global (likely typos), used for diagnostics.
//!
//! Both work on the *pre-expansion* AST, so they are deliberately conservative:
//! quoted data is skipped, and the unbound pass bails entirely when the file
//! uses macro-defining or binding constructs it can't model (see [`BAIL_HEADS`]),
//! and only ever flags *alphabetic operator names* — never symbolic operators,
//! never argument-position references. The bias is firmly toward zero false
//! positives: a missed binding only costs a missed warning, never a wrong one.

use std::collections::HashSet;

use steel::parser::ast::ExprKind;
use steel::parser::parser::Parser;

/// List heads whose presence means the file uses syntax we can't model without
/// macro expansion; the unbound pass bails rather than risk false positives.
const BAIL_HEADS: &[&str] = &[
    "define-syntax",
    "syntax-rules",
    "quasiquote",
    "case",
    "do",
    "match",
    "match-define",
    "define-values",
    "let-values",
    "letrec-values",
    "struct",
    "define-struct",
    "parameterize",
];

/// Identifiers that appear in operator position inside common macros but aren't
/// calls (e.g. `(cond (else …))`).
const CLAUSE_KEYWORDS: &[&str] = &["else"];

fn parse(text: &str) -> Option<Vec<ExprKind>> {
    let mut exprs = Vec::new();
    for result in Parser::new(text, None) {
        exprs.push(result.ok()?);
    }
    Some(exprs)
}

fn ident(expr: &ExprKind) -> Option<&str> {
    expr.atom_identifier().map(|i| i.resolve())
}

/// Byte `(start, end)` of an expression.
fn byte_span(expr: &ExprKind) -> (usize, usize) {
    let span = expr.span();
    (span.start as usize, span.end as usize)
}

/// Only alphabetic-leading names are flaggable, so symbolic operators (`+`, `=>`,
/// `->foo`) and literals are never reported.
fn is_word(name: &str) -> bool {
    name.chars().next().is_some_and(char::is_alphabetic)
}

// --- Unbound operator detection --------------------------------------------

/// Identifiers used as the operator of a call that resolve to neither a binding
/// in the file nor a `known` global. Returns `(name, start_byte, end_byte)`.
/// Empty if the source doesn't parse or uses constructs we can't model safely.
pub fn unbound_operators(text: &str, known: &HashSet<String>) -> Vec<(String, usize, usize)> {
    let Some(exprs) = parse(text) else {
        return Vec::new();
    };
    if exprs.iter().any(should_bail) {
        return Vec::new();
    }
    let mut bound = HashSet::new();
    for expr in &exprs {
        collect_bound(expr, &mut bound);
    }
    let mut out = Vec::new();
    for expr in &exprs {
        check_operators(expr, known, &bound, &mut out);
    }
    out
}

/// True if the tree uses a bail-listed head or a macro/require form.
fn should_bail(expr: &ExprKind) -> bool {
    match expr {
        ExprKind::Quote(_) => false,
        ExprKind::Macro(_) | ExprKind::SyntaxRules(_) | ExprKind::Require(_) => true,
        ExprKind::List(list) => {
            let head_bails = list
                .args
                .first()
                .and_then(ident)
                .is_some_and(|h| BAIL_HEADS.contains(&h));
            head_bails || list.args.iter().any(should_bail)
        }
        _ => children(expr).iter().any(|c| should_bail(c)),
    }
}

/// Every name bound anywhere in the tree (defines, lambda/let params, let-likes).
/// Intentionally flat and permissive: a name bound in one scope suppresses
/// warnings everywhere, trading recall for a guarantee of no scope-based false
/// positives.
fn collect_bound(expr: &ExprKind, set: &mut HashSet<String>) {
    match expr {
        ExprKind::Quote(_) => {}
        ExprKind::Define(define) => {
            match &define.name {
                // (define (f a b) …) binds f and the params.
                ExprKind::List(sig) => {
                    for arg in &sig.args {
                        insert_ident(arg, set);
                    }
                }
                other => insert_ident(other, set),
            }
            collect_bound(&define.body, set);
        }
        ExprKind::LambdaFunction(lambda) => {
            for arg in &lambda.args {
                insert_ident(arg, set);
            }
            collect_bound(&lambda.body, set);
        }
        ExprKind::Let(let_expr) => {
            for (name, value) in &let_expr.bindings {
                insert_ident(name, set);
                collect_bound(value, set);
            }
            collect_bound(&let_expr.body_expr, set);
        }
        ExprKind::List(list) => {
            // let* / letrec / named let are parsed as plain lists pre-expansion.
            if let Some(head) = list.args.first().and_then(ident)
                && matches!(head, "let" | "let*" | "letrec" | "letrec*")
            {
                collect_let_like_bindings(&list.args[1..], set);
            }
            for arg in &list.args {
                collect_bound(arg, set);
            }
        }
        _ => {
            for child in children(expr) {
                collect_bound(child, set);
            }
        }
    }
}

/// Extract the binding names of a `let`-family form's arguments (after the head),
/// handling the optional leading name of a named `let`.
fn collect_let_like_bindings(args: &[ExprKind], set: &mut HashSet<String>) {
    let mut index = 0;
    // Named let: `(let loop (bindings) body)`.
    if let Some(first) = args.first()
        && let Some(name) = ident(first)
    {
        set.insert(name.to_string());
        index = 1;
    }
    if let Some(ExprKind::List(bindings)) = args.get(index) {
        for pair in &bindings.args {
            if let ExprKind::List(pair) = pair
                && let Some(name) = pair.args.first()
            {
                insert_ident(name, set);
            }
        }
    }
}

fn check_operators(
    expr: &ExprKind,
    known: &HashSet<String>,
    bound: &HashSet<String>,
    out: &mut Vec<(String, usize, usize)>,
) {
    match expr {
        ExprKind::Quote(_) => {}
        ExprKind::List(list) => {
            if let Some(head) = list.args.first()
                && let Some(name) = ident(head)
                && is_word(name)
                && !known.contains(name)
                && !bound.contains(name)
                && !CLAUSE_KEYWORDS.contains(&name)
            {
                let (start, end) = byte_span(head);
                out.push((name.to_string(), start, end));
            }
            for arg in &list.args {
                check_operators(arg, known, bound, out);
            }
        }
        _ => {
            for child in children(expr) {
                check_operators(child, known, bound, out);
            }
        }
    }
}

// --- Scope-aware completion -------------------------------------------------

/// Names bound and visible at byte offset `cursor`: every top-level define, plus
/// the params / let-bindings of any enclosing form whose body covers the cursor.
pub fn bindings_in_scope_at(text: &str, cursor: usize) -> HashSet<String> {
    let Some(exprs) = parse(text) else {
        return HashSet::new();
    };
    let mut out = HashSet::new();
    for expr in &exprs {
        if let Some(name) = top_define_name(expr) {
            out.insert(name);
        }
        scope_at(expr, cursor, &mut out);
    }
    out
}

fn top_define_name(expr: &ExprKind) -> Option<String> {
    if let ExprKind::Define(define) = expr {
        return match &define.name {
            ExprKind::List(sig) => sig.args.first().and_then(ident).map(str::to_string),
            other => ident(other).map(str::to_string),
        };
    }
    None
}

fn covers((start, end): (usize, usize), cursor: usize) -> bool {
    cursor >= start && cursor <= end
}

fn scope_at(expr: &ExprKind, cursor: usize, out: &mut HashSet<String>) {
    match expr {
        ExprKind::Quote(_) => {}
        ExprKind::LambdaFunction(lambda) => {
            if covers(byte_span(&lambda.body), cursor) {
                for arg in &lambda.args {
                    insert_ident(arg, out);
                }
            }
            scope_at(&lambda.body, cursor, out);
        }
        ExprKind::Define(define) => {
            // A nested define is visible to its siblings; surface it.
            if let Some(name) = top_define_name(expr) {
                out.insert(name);
            }
            if let ExprKind::List(sig) = &define.name
                && covers(byte_span(&define.body), cursor)
            {
                for param in sig.args.iter().skip(1) {
                    insert_ident(param, out);
                }
            }
            scope_at(&define.body, cursor, out);
        }
        ExprKind::Let(let_expr) => {
            if covers(byte_span(&let_expr.body_expr), cursor) {
                for (name, _) in &let_expr.bindings {
                    insert_ident(name, out);
                }
            }
            for (_, value) in &let_expr.bindings {
                scope_at(value, cursor, out);
            }
            scope_at(&let_expr.body_expr, cursor, out);
        }
        ExprKind::List(list) => {
            if covers(byte_span(expr), cursor)
                && let Some(head) = list.args.first().and_then(ident)
                && matches!(head, "let" | "let*" | "letrec" | "letrec*")
            {
                collect_let_like_bindings(&list.args[1..], out);
            }
            for arg in &list.args {
                scope_at(arg, cursor, out);
            }
        }
        _ => {
            for child in children(expr) {
                scope_at(child, cursor, out);
            }
        }
    }
}

// --- Shared helpers ---------------------------------------------------------

fn insert_ident(expr: &ExprKind, set: &mut HashSet<String>) {
    if let Some(name) = ident(expr) {
        set.insert(name.to_string());
    }
}

/// Direct sub-expressions of `expr` (Quote intentionally excluded).
fn children(expr: &ExprKind) -> Vec<&ExprKind> {
    match expr {
        ExprKind::If(x) => vec![&x.test_expr, &x.then_expr, &x.else_expr],
        ExprKind::Begin(x) => x.exprs.iter().collect(),
        ExprKind::Return(x) => vec![&x.expr],
        ExprKind::Set(x) => vec![&x.variable, &x.expr],
        ExprKind::Vector(x) => x.args.iter().collect(),
        ExprKind::Define(x) => vec![&x.name, &x.body],
        ExprKind::LambdaFunction(x) => {
            let mut c: Vec<&ExprKind> = x.args.iter().collect();
            c.push(&x.body);
            c
        }
        ExprKind::Let(x) => {
            let mut c = Vec::new();
            for (n, v) in &x.bindings {
                c.push(n);
                c.push(v);
            }
            c.push(&x.body_expr);
            c
        }
        ExprKind::List(x) => x.args.iter().collect(),
        // Atom (leaf), Quote (skipped), Macro/SyntaxRules/Require (bailed): no
        // children to descend for our purposes.
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known(words: &[&str]) -> HashSet<String> {
        words.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn flags_unknown_operator_only() {
        // `frobnicate` is neither bound nor known → flagged; `+`, `x`, `f` are not.
        let found = unbound_operators("(define (f x) (frobnicate (+ x 1)))", &known(&["+"]));
        let names: Vec<&str> = found.iter().map(|(n, _, _)| n.as_str()).collect();
        assert_eq!(names, vec!["frobnicate"]);
    }

    #[test]
    fn respects_bindings_and_forward_refs() {
        // `helper` is defined later in the file; using it earlier is fine.
        let src = "(define (a) (helper))\n(define (helper) 1)";
        assert!(unbound_operators(src, &HashSet::new()).is_empty());
        // let-bound operators aren't flagged.
        assert!(unbound_operators("(let ((g (lambda () 1))) (g))", &HashSet::new()).is_empty());
        // cond's `else` is not treated as a call.
        assert!(unbound_operators("(define (f x) (cond (else x)))", &known(&["cond"])).is_empty());
    }

    #[test]
    fn bails_on_unmodelable_constructs() {
        // `case` uses datum syntax we don't model — bail rather than misreport.
        assert!(unbound_operators("(case x (1 (mystery)))", &HashSet::new()).is_empty());
        // Quoted data is never analyzed.
        assert!(unbound_operators("(define x '(a b c))", &HashSet::new()).is_empty());
    }

    #[test]
    fn scope_contains_enclosing_bindings() {
        // Cursor inside the body of `(define (f x) (g x))` — byte 16 is within `x)`.
        let src = "(define (f x) (g x))";
        let cursor = src.find("(g x)").unwrap() + 3;
        let scope = bindings_in_scope_at(src, cursor);
        assert!(scope.contains("x"), "params in scope: {scope:?}");
        assert!(scope.contains("f"), "top-level define in scope: {scope:?}");
    }
}
