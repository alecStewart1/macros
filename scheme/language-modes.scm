;; requires: core

;; language-modes.scm — an M-x major-mode command per bundled tree-sitter
;; language. Emacs exposes `rust-mode`, `python-mode`, … as interactive commands
;; you can invoke on any buffer; this generates the same set from the grammars
;; Rust actually ships (see `bundled-languages`), so `M-x scala-mode`,
;; `M-x clojure-mode`, etc. switch the current buffer's major mode without any
;; hand-written wiring per language.
;;
;; `set-major-mode` is highlighting-aware in Rust: switching to a mode whose name
;; matches a bundled language attaches that language's grammar (and, via the
;; `fundamental` `enable-lsp` hook, its language server) and re-runs the mode's
;; hooks. So `(add-hook "rust" my-setup)` fires whenever a buffer enters
;; rust-mode — by file detection OR by `M-x rust-mode` — which is the seam for
;; user per-mode configuration.
;;
;; Scheme is intentionally excluded: it has the richer, editor-aware `steel-mode`
;; / `macros-steel-mode` commands (see steel-mode.scm, macros-steel-mode.scm).
;;
;; Deliberately NOT `define-derived-mode`: that would add a keymap variable, a
;; `<lang>-mode-hook` variable, scoped-key wiring and a setup chain per
;; language, stop these commands echoing "<lang> mode", and — since it takes a
;; literal name — a user's `(define-derived-mode rust …)` would silently
;; replace the bundled mode instead of erroring as it does now.

;; The `<lang>-mode` command: a closure over LANG, so the language id is a
;; value rather than text spliced into source.
(define (language-mode-command lang)
  (lambda ()
    (set-major-mode lang)
    (message (string-append lang " mode"))))

;; Define a `<lang>-mode` command for `lang`: `eval` binds it globally (a macro
;; can't — `bundled-languages` doesn't exist at expansion time), then
;; `register-mode` hooks up the `<lang>-mode-hook` alias and the Rust registry.
(define (define-language-mode lang)
  (let ((cmd (string->symbol (string-append lang "-mode"))))
    (eval (list 'define cmd (list 'language-mode-command lang)))
    (register-command cmd)
    (register-mode lang "" lang)))

(for-each define-language-mode
          (filter (lambda (lang) (not (equal? lang "scheme")))
                  (bundled-languages)))
