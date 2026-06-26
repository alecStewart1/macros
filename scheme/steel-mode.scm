;; steel-mode keymap — Steel (Scheme) editing, layered on fundamental.
(define-key "steel" "ctrl-c ctrl-e" "eval-last-sexp")
(define-key "steel" "ctrl-c ctrl-b" "eval-buffer")

;; LSP autostart lives on `fundamental` (see fundamental.scm) so it fires for
;; every language, not just Steel.

;; Activation: drop the active buffer into Steel mode (M-x steel-mode, or auto
;; for .scm by language detection). `set-major-mode` installs the keymap above;
;; for the editor-aware variant (live runtime eval, editor-API completion) use
;; `macros-steel-mode` instead. See macros-steel-mode.scm.
(define (steel-mode)
  (set-major-mode "steel")
  (message "Steel mode"))
(register-command "steel-mode")
