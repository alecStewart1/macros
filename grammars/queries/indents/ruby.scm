; Ruby structural indentation. Keyword-delimited blocks (def … end) use @extend
; so an empty body still indents; their `end` token (and bracket closers)
; @outdent.
[
  (method)
  (singleton_method)
  (class)
  (module)
  (do_block)
  (block)
  (if)
  (unless)
  (while)
  (until)
  (for)
  (case)
  (begin)
] @extend

[
  (array)
  (hash)
  (argument_list)
  (method_parameters)
  (block_parameters)
] @indent

["}" "]" ")" "end"] @outdent
