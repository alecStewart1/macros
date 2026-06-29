; Elixir structural indentation. `do … end` blocks indent their body (@indent
; with an `end` outdent); collection/call brackets indent too.
[
  (do_block)
  (list)
  (tuple)
  (map)
  (bitstring)
  (arguments)
] @indent

["}" ")" "]" "end"] @outdent
