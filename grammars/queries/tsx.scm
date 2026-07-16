;; Macros — TSX-only highlighting rules (JSX elements).
;;
;; Appended to `typescript.scm` at compile time (see the `tsx` entry in
;; `src/highlight.rs`) — the JSX node kinds below exist only in the TSX
;; grammar, so they can't live in the shared base without breaking the plain
;; TypeScript query. Capitalized component names are handled by the base
;; query's identifier-rooted Capitalized→type rule, which out-ranks the @tag
;; rules here (it roots at the identifier), so `<div>` colors as a tag while
;; `<App>` colors as a type — matching Neovim/Helix.

(jsx_opening_element name: (identifier) @tag)
(jsx_closing_element name: (identifier) @tag)
(jsx_self_closing_element name: (identifier) @tag)

(jsx_attribute (property_identifier) @attribute)

(jsx_opening_element ["<" ">"] @punctuation.bracket)
(jsx_closing_element ["</" ">"] @punctuation.bracket)
(jsx_self_closing_element ["<" "/>"] @punctuation.bracket)
