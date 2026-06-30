;; Macros — Racket syntax highlighting (.rkt).
;;
;; A focused replacement for the upstream 6cdh/tree-sitter-racket highlights
;; query, which leans on a blanket `(symbol) @variable` plus a ~40 KB keyword
;; regex. That structure renders flat in this editor: tree-sitter emits matches
;; in root-node order and `flatten` (src/highlight.rs) lets the last-emitted
;; capture win, so a bare-symbol match (rooted at the symbol) always clobbers an
;; enclosing `(list ...)` match. So, like grammars/queries/scheme.scm, we follow
;; the classic Emacs/DrRacket convention — color *roles* (special forms, defined
;; names, known builtins, literals), and leave ordinary call heads and plain
;; references uncaptured so they paint with the editor foreground.
;;
;; NB: a list's opening "(" is its first *child*, so head symbols must be
;; anchored after it — `(list . (symbol))` anchors against "(" and never matches.

;; --- Literals -------------------------------------------------------------
(number) @number
(decimal) @number
(character) @constant.builtin
(boolean) @constant.builtin
[(string) (here_string) (byte_string)] @string
(regex) @string
(escape_sequence) @escape

;; --- Comments -------------------------------------------------------------
[(comment) (block_comment) (sexp_comment)] @comment

;; --- #lang / #reader line -------------------------------------------------
(lang_name) @keyword

;; --- Keyword arguments: #:foo --------------------------------------------
(keyword) @constant.builtin

;; --- Special forms --------------------------------------------------------
(list . "(" . (symbol) @keyword
 (#any-of? @keyword
   "define" "define-values" "define-syntax" "define-syntaxes"
   "define-syntax-rule" "define-for-syntax" "define/public" "define/private"
   "define/override" "define/augment" "define/contract" "define-struct"
   "define-signature" "define-values-for-syntax"
   "lambda" "λ" "case-lambda" "thunk"
   "let" "let*" "letrec" "letrec*" "let-values" "let*-values"
   "let-syntax" "letrec-syntax" "let/cc" "let/ec" "named-let" "let-struct"
   "begin" "begin0" "begin-for-syntax" "when" "unless" "cond" "case" "else"
   "if" "and" "or" "not" "set!" "set!-values" "=>" "delay" "force"
   "quote" "quasiquote" "unquote" "unquote-splicing" "syntax" "quasisyntax"
   "syntax-rules" "syntax-case" "syntax-id-rules" "with-syntax" "datum->syntax"
   "for" "for*" "for/list" "for*/list" "for/vector" "for/hash" "for/fold"
   "for/and" "for/or" "for/sum" "for/product" "for-each" "in-range" "in-list"
   "in-vector" "in-hash" "in-string" "in-naturals"
   "match" "match*" "match-define" "match-let" "match-let*" "match-lambda"
   "struct" "require" "provide" "module" "module+" "module*" "submod"
   "all-defined-out" "all-from-out" "rename-out" "rename-in" "only-in" "except-in"
   "prefix-in" "prefix-out" "contract-out" "for-syntax" "for-template"
   "parameterize" "with-handlers" "dynamic-wind" "call/cc" "call-with-values"
   "class" "class*" "interface" "mixin" "send" "new" "super-new" "define-class"
   "with-output-to-string" "with-input-from-string" "values"))

;; --- Common builtins / procedures ----------------------------------------
(list . "(" . (symbol) @function.builtin
 (#any-of? @function.builtin
   "car" "cdr" "caar" "cadr" "cdar" "cddr" "cons" "list" "list*" "list-ref"
   "append" "map" "filter" "foldl" "foldr" "for-each" "apply" "reverse"
   "length" "range" "build-list" "take" "drop" "first" "second" "third" "last"
   "rest" "empty?" "null?" "pair?" "list?" "member" "memq" "assoc" "assq"
   "vector" "vector-ref" "vector-set!" "vector->list" "list->vector"
   "make-vector" "vector-length" "vector-map" "vector-for-each"
   "hash" "hash-ref" "hash-set" "hash-set!" "hash-has-key?" "hash-keys"
   "hash-values" "hash-map" "hash-for-each" "make-hash" "make-hasheq"
   "string-append" "string->symbol" "symbol->string" "string-length"
   "string-ref" "substring" "string-split" "string-join" "string-trim"
   "string->number" "number->string" "string->list" "list->string"
   "format" "printf" "fprintf" "display" "displayln" "print" "println"
   "write" "newline" "void" "error" "raise" "raise-argument-error"
   "string?" "number?" "symbol?" "boolean?" "procedure?" "hash?" "vector?"
   "equal?" "eq?" "eqv?" "zero?" "even?" "odd?" "positive?" "negative?"
   "add1" "sub1" "abs" "min" "max" "modulo" "quotient" "remainder"
   "expt" "sqrt" "floor" "ceiling" "round" "exact->inexact" "inexact->exact"))

;; --- Defined names: color what a definition introduces --------------------
;; (define NAME value) / (define-syntax NAME ...) etc.
((list . "(" . (symbol) @_def . (symbol) @function)
 (#any-of? @_def
   "define" "define-values" "define-syntax" "define-syntaxes"
   "define-for-syntax" "define/public" "define/private" "define/contract"))
;; (define (NAME args...) body) — curried/procedure form
((list . "(" . (symbol) @_def . (list . "(" . (symbol) @function))
 (#any-of? @_def
   "define" "define/public" "define/private" "define/override"
   "define/augment" "define/contract" "define-for-syntax"))
;; (struct NAME (fields...)) / (define-struct NAME ...)
((list . "(" . (symbol) @_def . (symbol) @type)
 (#any-of? @_def "struct" "define-struct" "define-signature" "class" "interface"))

;; --- Quoted data ----------------------------------------------------------
(quote (symbol) @constant)
(quote (list (symbol) @constant))

;; --- Structural delimiters -----------------------------------------------
;; Last so nothing overrides them; mapped to the dim `bracket` face.
["(" ")" "[" "]" "{" "}"] @punctuation.bracket
