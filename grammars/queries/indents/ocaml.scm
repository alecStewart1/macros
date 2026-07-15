; OCaml structural indentation. Bracketed expressions indent their body and the
; closing token outdents; the keyword-delimited forms (`let … =`, `match … with`,
; `struct … end`, `do … done`, `fun … ->`, …) have no bracket token on the body's
; first line, so they indent via @extend, with `done`/`end` outdenting.
[
  (record_expression)
  (list_expression)
  (array_expression)
  (parenthesized_expression)
  (record_declaration)
] @indent

; NOTE: `match_expression` / `try_expression` / `match_case` are deliberately
; NOT indented. ocamlformat aligns the `| arm` lines with the `match`/`try`
; keyword (not one step in), and after a complete one-line arm (`| 0 -> a`) RET
; should start the next arm at that same column — extending `match_case` would
; instead drop the caret to the arm *body* column. `function` arms DO indent one
; step under their binding, and `function_expression` shares the binding's row so
; the `used_rows` dedup keeps it at exactly one step.
[
  (do_clause)
  (then_clause)
  (else_clause)
  (structure)
  (signature)
  (object_expression)
  (let_binding)
  (function_expression)
  (fun_expression)
] @extend

; Opener keywords, so pressing RET into a still-empty body indents before the
; closing token has been typed (the node hasn't formed yet, so the clause rules
; above can't fire). `used_rows` dedup keeps these from double-counting when the
; formed node covers the same line. `with` is intentionally absent: `match`/`try`
; arms align with the keyword, and the not-yet-typed nested-match case is handled
; by the editor's "RET never falls to column 0 inside an indented block" rule.
["struct" "sig" "do" "begin" "{"] @extend

["}" ")" "]" "done" "end"] @outdent
