;; requires: core

;; steel-mode keymap — Steel (Scheme) editing, layered on fundamental.
;;
;; Plain `steel` buffers hold arbitrary Steel code, so their eval keys target
;; the isolated ANVIL image (steel-anvil.scm), never the live editor: running
;; a file that happens to `(define (insert …))` or `(define (message …))` must
;; not silently replace the editor's own functions. Code that is *meant* to
;; change the editor lives in `macros-steel` buffers, whose keys evaluate into
;; the live runtime (macros-steel-mode.scm). `M-x eval-last-sexp` /
;; `M-x eval-buffer` still evaluate live from anywhere.
(define-key "steel" "ctrl-c ctrl-e" 'anvil-eval-last-sexp)
(define-key "steel" "ctrl-x ctrl-e" 'anvil-eval-last-sexp)  ; Emacs muscle memory
(define-key "steel" "ctrl-c ctrl-b" 'anvil-eval-buffer)

;; LSP autostart lives on `fundamental` (see fundamental.scm) so it fires for
;; every language, not just Steel.

;; Activation: drop the active buffer into Steel mode (M-x steel-mode, or auto
;; for .scm by language detection). `set-major-mode` installs the keymap above;
;; for the editor-aware variant (live runtime eval, editor-API completion) use
;; `macros-steel-mode` instead. See macros-steel-mode.scm.
(define (steel-mode)
  (set-major-mode "steel")
  (message "Steel mode"))
(register-command 'steel-mode)
