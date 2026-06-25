;; steel-mode keymap — Steel (Scheme) editing, layered on fundamental.
(define-key "steel" "ctrl-c ctrl-e" "eval-last-sexp")
(define-key "steel" "ctrl-c ctrl-b" "eval-buffer")

;; LSP autostart lives on `fundamental` (see fundamental.scm) so it fires for
;; every language, not just Steel.
