; OCaml structural indentation. Bracketed expressions indent their body; the
; `do … done` / `struct … end` / `sig … end` forms indent via @extend (no
; bracket token to open on the first body line) with `done`/`end` outdents.
[
  (record_expression)
  (list_expression)
  (array_expression)
  (parenthesized_expression)
] @indent

[
  (do_clause)
  (then_clause)
  (else_clause)
  (structure)
  (signature)
  (object_expression)
] @extend

["}" ")" "]" "done" "end"] @outdent
