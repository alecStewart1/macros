;; Macros — JavaScript/JSX syntax highlighting.
;;
;; Replaces the crate's stock `HIGHLIGHT_QUERY`, whose blanket
;; `(identifier) @variable` / `(property_identifier) @property` rules out-rank
;; every context rule in this engine (identifier-rooted matches win ties — see
;; below), leaving function names, calls, and method calls painted as plain
;; variables. This is the sibling of `typescript.scm` (the same JavaScript
;; base adapted to this engine) minus the TypeScript layer, plus JSX rules —
;; the JavaScript grammar parses JSX natively.
;;
;; Capture names map to theme faces in `src/highlight.rs` (`face_for_capture`).
;; Precedence (see `flatten`): the innermost capture wins at each byte; for an
;; identical node the later-emitted match wins, and matches emit in
;; *completion* order — so a bare `(identifier)` rule beats a rule that
;; reaches the same identifier from an enclosing node, while a multi-step
;; pattern that completes deeper (e.g. a function-valued binding, whose match
;; finishes at the arrow body) beats both. Two consequences shape this file:
;;   * No blanket `(identifier) @variable` (the stock query's bug): an
;;     uncaptured byte already paints with the editor foreground.
;;   * The identifier-rooted specials (Capitalized → type, SCREAMING_CASE →
;;     constant) sit last, so their mutual tie resolves constant-over-type.

;; --- Comments ---------------------------------------------------------------

(comment) @comment
(hash_bang_line) @comment

;; --- Literals ---------------------------------------------------------------

(string) @string
(template_string) @string
;; Substitutions render as plain code, not string: reset the region to the
;; foreground face, then let inner captures (calls, numbers, …) win by nesting.
(template_substitution) @variable
(template_substitution
  "${" @punctuation.special
  "}" @punctuation.special)
(regex) @string.special
(number) @number

[
  (true)
  (false)
  (null)
  (undefined)
] @constant.builtin

(this) @constant.builtin
(super) @constant.builtin

;; --- Punctuation ------------------------------------------------------------

[
  ";"
  ","
  "."
  ":"
  "?"
  (optional_chain)
] @punctuation.delimiter

"..." @punctuation.special

[
  "("
  ")"
  "["
  "]"
  "{"
  "}"
] @punctuation.bracket

;; --- Operators --------------------------------------------------------------
;; `<` and `>` are deliberately absent from the blanket list: as bare tokens
;; they would out-rank the JSX-bracket rules below (token-rooted matches win
;; ties), so they're captured only in comparison position.

(binary_expression operator: ["<" ">"] @operator)

[
  "-" "--" "-="
  "+" "++" "+="
  "*" "*=" "**" "**="
  "/" "/=" "%" "%="
  "<=" "<<" "<<="
  "=" "==" "===" "!" "!=" "!=="
  "=>" ">=" ">>" ">>=" ">>>" ">>>="
  "~" "^" "&" "|" "^=" "&=" "|="
  "&&" "||" "??" "&&=" "||=" "??="
] @operator

;; --- Keywords ---------------------------------------------------------------

[
  "as" "async" "await" "break" "case" "catch" "class" "const" "continue"
  "debugger" "default" "delete" "do" "else" "export" "extends" "finally"
  "for" "from" "function" "get" "if" "import" "in" "instanceof" "let" "new"
  "of" "return" "set" "static" "switch" "target" "throw" "try" "typeof"
  "var" "void" "while" "with" "yield"
] @keyword

;; Labels (`outer: for (...) { break outer; }`).
(statement_identifier) @label

;; --- Functions & methods ----------------------------------------------------

(function_declaration name: (identifier) @function)
(function_expression name: (identifier) @function)
(generator_function_declaration name: (identifier) @function)
(generator_function name: (identifier) @function)

(method_definition
  name: [(property_identifier) (private_property_identifier)] @function.method)

;; A function-valued binding or property is a function name.
(pair
  key: (property_identifier) @function.method
  value: [(function_expression) (arrow_function)])
(variable_declarator
  name: (identifier) @function
  value: [(function_expression) (arrow_function)])
(assignment_expression
  left: (identifier) @function
  right: [(function_expression) (arrow_function)])
(assignment_expression
  left: (member_expression property: (property_identifier) @function.method)
  right: [(function_expression) (arrow_function)])

;; Calls.
(call_expression function: (identifier) @function)
(call_expression
  function: (member_expression property: (property_identifier) @function.method))

;; Decorators (`@observable`).
(decorator "@" @function)
(decorator (identifier) @function)

;; Constructors and namespace-ish imports.
(new_expression constructor: (identifier) @type)
(namespace_import (identifier) @namespace)

;; --- JSX --------------------------------------------------------------------
;; Capitalized component names are handled by the identifier-rooted
;; Capitalized→type rule below, which out-ranks the @tag rules here (it roots
;; at the identifier), so `<div>` colors as a tag while `<App>` colors as a
;; type — matching Neovim/Helix.

(jsx_opening_element name: (identifier) @tag)
(jsx_closing_element name: (identifier) @tag)
(jsx_self_closing_element name: (identifier) @tag)

(jsx_attribute (property_identifier) @attribute)

(jsx_opening_element ["<" ">"] @punctuation.bracket)
(jsx_closing_element ["</" ">"] @punctuation.bracket)
(jsx_self_closing_element ["<" "/>"] @punctuation.bracket)

;; --- Identifier-rooted specials (keep last — they win every tie) ------------

;; Capitalized identifiers: constructors, imported classes/components.
((identifier) @type
 (#match? @type "^[A-Z]"))

;; SCREAMING_CASE identifiers: constants. After the rule above so it wins
;; their shared tie (`MAX_N` matches both).
([
  (identifier)
  (shorthand_property_identifier)
  (shorthand_property_identifier_pattern)
 ] @constant
 (#match? @constant "^[A-Z_][A-Z\\d_]+$"))
