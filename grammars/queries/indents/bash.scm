; Bash structural indentation. Compound bodies indent (@indent); block keywords
; (then/do/else/elif) and terminators (fi/done/esac) sit on their own dedented
; lines, so they are @outdent even while their enclosing statement stays open.
[
  (if_statement)
  (do_group)
  (case_statement)
  (compound_statement)
  (subshell)
  (function_definition)
] @indent

["}" ")" "then" "do" "else" "elif" "fi" "done" "esac"] @outdent
