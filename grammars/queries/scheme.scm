;; Macros — Steel/Scheme syntax highlighting.
;;
;; Replaces the stock R7RS query that ships in the `tree-sitter-scheme` crate,
;; which only knew a fixed R7RS keyword/builtin list and so left the bulk of a
;; Steel file (special forms, defined names, keyword args) rendered as plain
;; `@variable`. This query is Steel-aware: it recognizes Steel's special forms,
;; colors the *name* a `define` introduces, and tags `#:keyword` arguments.
;;
;; Capture names map to theme faces in `src/highlight.rs` (`face_for_capture`).
;; Pattern order matters: later patterns win for an identical node range, so the
;; generic rules come first and the specific ones (keywords, builtins, defined
;; names) override them.

;; --- Literals -------------------------------------------------------------
(number) @number
(character) @constant.builtin
(boolean) @constant.builtin
(string) @string
(escape_sequence) @escape

;; --- Comments / shebang directives ---------------------------------------
[(comment) (block_comment) (directive)] @comment

;; --- Keyword arguments: #:foo --------------------------------------------
(keyword) @constant.builtin

;; NB: ordinary symbols are deliberately *not* captured — an unhighlighted byte
;; already paints with the editor foreground (the variable color). Capturing
;; `(symbol) @variable` here would be counterproductive: tree-sitter emits
;; matches in root-node order, so a bare-symbol match (rooted at the symbol)
;; always lands *after* an enclosing `(list ...)` match and would win the tie,
;; clobbering the head/keyword coloring below. Leaving plain references uncaptured
;; lets the specific rules win.

;; The head of a list sits in call position -> function color. This also makes
;; arithmetic heads like (+ 1 2) read as calls rather than plain text.
;;
;; NB: the opening "(" is the list's first *child* in this grammar, so the head
;; symbol must be anchored after it — a bare `(list . (symbol))` anchors against
;; the "(" and never matches (this is one bug that left the stock query flat).
(list . "(" . (symbol) @function)

;; --- Special forms (Steel-aware) -----------------------------------------
(list . "(" . (symbol) @keyword
 (#any-of? @keyword
   "define" "define-syntax" "define-values" "define-syntax-rule"
   "define/contract" "define/public" "define/private"
   "lambda" "λ" "case-lambda"
   "let" "let*" "letrec" "letrec*" "let-values" "let*-values"
   "let-syntax" "letrec-syntax" "named-let" "let/cc" "let/ec"
   "begin" "if" "cond" "case" "when" "unless" "else" "=>"
   "and" "or" "not" "set!" "do" "delay" "force"
   "quote" "quasiquote" "unquote" "unquote-splicing"
   "syntax-rules" "syntax-case" "with-syntax" "quasisyntax"
   "dynamic-wind" "parameterize" "guard" "with-handlers"
   "require" "provide" "require-builtin" "include" "import" "export"
   "struct" "define-struct" "match" "match-define" "match-let" "match-let*"
   "for" "for-each" "for/list" "for/vector" "for/hash" "while"
   "define-command"))

;; --- Common Steel builtins / procedures ----------------------------------
(list . "(" . (symbol) @function.builtin
 (#any-of? @function.builtin
   "car" "cdr" "caar" "cadr" "cdar" "cddr" "cons" "list" "list-ref"
   "append" "map" "filter" "foldl" "foldr" "reduce" "flatten"
   "length" "reverse" "apply" "range" "take" "drop" "last"
   "first" "second" "third" "rest" "empty"
   "vector" "vector-ref" "vector-set!" "vector-push!" "vector->list"
   "list->vector" "list->string" "string->list"
   "hash" "hash-ref" "hash-get" "hash-set" "hash-insert" "hash-contains?"
   "hash-keys->list" "hash-values->list"
   "string-append" "string->symbol" "symbol->string" "string-length"
   "string-ref" "substring" "string-split" "string-join" "trim"
   "number->string" "string->number" "int->string"
   "displayln" "display" "newline" "write" "print" "println"
   "error" "raise-error" "error-object-message" "void"
   "null?" "empty?" "list?" "pair?" "string?" "number?" "symbol?"
   "hash?" "vector?" "boolean?" "function?" "void?"
   "equal?" "eq?" "eqv?" "zero?" "even?" "odd?"
   "abs" "min" "max" "modulo" "quotient" "remainder" "expt" "sqrt"))

;; --- Defined names: color what a `define` introduces ----------------------
;; (define NAME value)
((list . "(" . (symbol) @_def . (symbol) @function)
 (#any-of? @_def "define" "define-values" "define/contract"))
;; (define (NAME args...) body) and (define-command (NAME ...) ...)
((list . "(" . (symbol) @_def . (list . "(" . (symbol) @function))
 (#any-of? @_def "define" "define/contract" "define-command"))
;; (struct NAME (fields...))
((list . "(" . (symbol) @_def . (symbol) @type)
 (#any-of? @_def "struct" "define-struct"))

;; --- Quoted data ----------------------------------------------------------
;; 'atom and the immediate contents of a quoted list read as constants.
(quote (symbol) @constant)
(quote (list (symbol) @constant))

;; --- Structural delimiters -----------------------------------------------
;; Last so nothing overrides them; mapped to the dim `bracket` face.
["(" ")" "[" "]" "{" "}"] @punctuation.bracket
