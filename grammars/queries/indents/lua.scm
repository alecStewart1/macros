; Lua structural indentation. Keyword-delimited blocks use @extend; brackets and
; table constructors use @indent; `end` and bracket closers @outdent.
[
  (function_declaration)
  (function_definition)
  (if_statement)
  (while_statement)
  (for_statement)
  (repeat_statement)
  (do_statement)
] @extend

[
  (table_constructor)
  (arguments)
  (parameters)
] @indent

["}" ")" "]" "end"] @outdent
