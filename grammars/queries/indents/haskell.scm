; Haskell structural indentation. Layout blocks (do / case-of / where / let)
; have no closing token, so they use @extend; bracketed literals use @indent.
[
  (do)
  (alternatives)
  (local_binds)
] @extend

[
  (list)
  (tuple)
] @indent

[")" "]"] @outdent
