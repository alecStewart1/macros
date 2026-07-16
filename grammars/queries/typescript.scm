;; Macros — TypeScript syntax highlighting (also the base layer for TSX).
;;
;; Replaces the crate's stock `HIGHLIGHTS_QUERY`, which is only the ~36-line
;; TypeScript *addition*: upstream expects it to be concatenated onto the
;; JavaScript highlights (Neovim and Helix both inherit that way), so used
;; alone it left keywords, strings, functions, and punctuation uncolored.
;; This file is the full combined query — the JavaScript base adapted to the
;; TypeScript grammar, plus the TypeScript layer — tuned for this engine.
;;
;; Capture names map to theme faces in `src/highlight.rs` (`face_for_capture`).
;; Precedence (see `flatten`): the innermost capture wins at each byte; for an
;; identical node, the later-emitted match wins, and matches are emitted in
;; root-node order — so a rule rooted *at* an identifier always beats a rule
;; that reaches the same identifier from an enclosing node, regardless of file
;; order. Two consequences shape this file:
;;   * No blanket `(identifier) @variable` (the stock JS query has one): it
;;     would clobber every context rule below, and an uncaptured byte already
;;     paints with the editor foreground.
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

(type_arguments ["<" ">"] @punctuation.bracket)
(type_parameters ["<" ">"] @punctuation.bracket)

;; --- Operators --------------------------------------------------------------
;; `<` and `>` are deliberately absent from the blanket list: as bare tokens
;; they would out-rank the generic-bracket rules above (token-rooted matches
;; win ties), so they're captured only in comparison position.

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

;; TypeScript-only keywords.
[
  "abstract" "accessor" "asserts" "declare" "enum" "global" "implements"
  "infer" "interface" "is" "keyof" "module" "namespace" "override" "private"
  "protected" "public" "readonly" "require" "satisfies" "type" "using"
] @keyword

;; Labels (`outer: for (...) { break outer; }`).
(statement_identifier) @label

;; --- Functions & methods ----------------------------------------------------

(function_declaration name: (identifier) @function)
(function_expression name: (identifier) @function)
(function_signature name: (identifier) @function)
(generator_function_declaration name: (identifier) @function)
(generator_function name: (identifier) @function)

(method_definition
  name: [(property_identifier) (private_property_identifier)] @function.method)
(method_signature
  name: [(property_identifier) (private_property_identifier)] @function.method)
(abstract_method_signature
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

;; Decorators (`@Injectable()`).
(decorator "@" @function)
(decorator (identifier) @function)

;; --- Types (the TypeScript layer) -------------------------------------------

(type_identifier) @type
(predefined_type) @type.builtin
(this_type) @type.builtin
(new_expression constructor: (identifier) @type)

;; Namespace-ish names (`namespace Foo`, `import * as bar`, `Foo.Bar` types).
(internal_module name: (identifier) @namespace)
(module name: (identifier) @namespace)
(nested_type_identifier module: (identifier) @namespace)
(namespace_import (identifier) @namespace)

;; Enum members are constants.
(enum_body (property_identifier) @constant)
(enum_assignment name: (property_identifier) @constant)

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
