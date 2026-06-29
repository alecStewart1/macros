; Python structural indentation. Compound statements use @extend so the first
; body line indents even before any body exists (there is no brace to open a
; block); brackets use @indent and their closers @outdent.
[
  (if_statement)
  (for_statement)
  (while_statement)
  (with_statement)
  (try_statement)
  (function_definition)
  (class_definition)
  (match_statement)
] @extend

[
  (parenthesized_expression)
  (list)
  (dictionary)
  (set)
  (tuple)
  (argument_list)
  (parameters)
  (list_comprehension)
  (dictionary_comprehension)
  (set_comprehension)
] @indent

[")" "]" "}"] @outdent
