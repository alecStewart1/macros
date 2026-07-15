;;; indent.scm — per-language indentation width defaults.
;;;
;;; Each structural indent level is `tab-width` columns (global default 4). A
;;; `tab-width:<language>` override sets the step for one language, so OCaml can
;;; be 2 while Rust stays 4 — matching each ecosystem's formatter, so live
;;; editing agrees with format-on-save instead of fighting it.
;;;
;;; These are DEFAULTS: your init.scm loads after this file, so
;;;   (set-option "tab-width:ocaml" 4)
;;; there wins. The <language> key is the editor's language id (the same name
;;; used in `lsp-language` / syntax highlighting: "ocaml", "ruby", "tsx", …).
;;;
;;; Only languages whose indentation is computed here (a tree-sitter indent
;;; query or the Lisp aligner) are listed — setting a width for a language that
;;; just copies the previous line's indent would have no effect.

;; --- 2-space ---------------------------------------------------------------
;; Lisps: body indented two columns past the special form.
(set-option "tab-width:scheme" 2)
(set-option "tab-width:clojure" 2)
(set-option "tab-width:racket" 2)
;; ML / functional: formatter-enforced 2 (ocamlformat, scalafmt, mix format).
(set-option "tab-width:ocaml" 2)
(set-option "tab-width:elixir" 2)
(set-option "tab-width:scala" 2)
(set-option "tab-width:haskell" 2)
;; Ruby: rubocop / standardrb.
(set-option "tab-width:ruby" 2)
;; Web / Prettier ecosystem defaults to 2.
(set-option "tab-width:javascript" 2)
(set-option "tab-width:typescript" 2)
(set-option "tab-width:tsx" 2)
(set-option "tab-width:json" 2)
(set-option "tab-width:css" 2)
(set-option "tab-width:html" 2)
;; Data / misc conventions.
(set-option "tab-width:yaml" 2)
(set-option "tab-width:lua" 2)
(set-option "tab-width:r" 2)          ; tidyverse style
(set-option "tab-width:bash" 2)       ; Google shell style

;; --- 4-space ---------------------------------------------------------------
;; These match the global default, but are stated explicitly so each language's
;; convention is visible and independently editable.
(set-option "tab-width:rust" 4)       ; rustfmt
(set-option "tab-width:python" 4)     ; PEP 8
(set-option "tab-width:c" 4)
(set-option "tab-width:cpp" 4)
(set-option "tab-width:java" 4)
(set-option "tab-width:zig" 4)        ; zig fmt
;; Go indents with TABS (gofmt). Auto-indent still inserts spaces here; a save
;; under gofmt normalizes them back to tabs. Width is the tab display size.
(set-option "tab-width:go" 4)

;; --- trailing-token indent rules -------------------------------------------
;; A token-level layer on top of the structural tree-sitter indent, for the
;; distinctions the syntax tree can't express (a dangling `->` vs a satisfied
;; one). `indent-after` = tokens that, when they END a line, open the next line
;; one step deeper; `align-after` = tokens that keep the next line at the current
;; column. Whitespace-separated, per language.
;;
;; OCaml: a match/function arm whose `->` dangles at end of line indents its body
;; (`| A ->` ⏎ → body one step in), while `let … in` and `match … with` keep the
;; body at the current column. (`with` aligns `match`/`try` arms with the keyword;
;; a record update `{ r with` ⏎ can't reach the fields' column pre-parse either
;; way, so it lands at the current column too — a save reformats it.)
(set-option "indent-after:ocaml" "->")
(set-option "align-after:ocaml" "in with")
