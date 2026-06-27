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

;; Define `<lang>-mode` as a nullary interactive command and register it so M-x
;; and apropos list it. Built via eval-string because the name is data, not a
;; literal — the same mechanism `execute-command` uses to call commands by name.
(define (define-language-mode lang)
  (let ((cmd (string-append lang "-mode")))
    (eval-string
     (string-append "(define (" cmd ") "
                    "(set-major-mode \"" lang "\") "
                    "(message \"" lang " mode\"))"))
    (register-command cmd)))

(for-each define-language-mode
          (filter (lambda (lang) (not (equal? lang "scheme")))
                  (bundled-languages)))
