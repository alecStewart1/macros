;; requires: core

;; hooks.scm — friendlier `add-hook` / `remove-hook`.
;;
;; The raw builtins take a string (the editor calls hooks by name across the
;; engine-thread boundary). These wrappers add the same ergonomics `bind-key`
;; has: a 'symbol defers to its name, and an anonymous procedure is stashed in
;; the thunk table behind a generated, callable global stub. So all three forms
;; work — handy for per-major-mode setup:
;;
;;   (add-hook "rust" "my-setup")                  ; string  (raw form)
;;   (add-hook "rust" 'my-setup)                   ; 'symbol (resolved at run time)
;;   (add-hook "rust" (lambda () (message "hi")))  ; anonymous procedure
;;
;; Must be a separate file from core.scm (where `raw-add-hook`/`hook->name` live):
;; Steel compiles each file with letrec* scope, so redefining `add-hook` in the
;; same file as `(define raw-add-hook add-hook)` would shadow the builtin and turn
;; that capture into a forward reference. core.scm sorts first, so its bindings
;; are in scope here.

(define (add-hook hook proc)
  (raw-add-hook hook (hook->name proc)))

;; A 'symbol or string removes the matching registration; an anonymous procedure
;; can't be matched (each `add-hook` of a lambda gets a fresh stub name — the
;; same limitation Emacs has), so name it or use a 'symbol if you need removal.
(define (remove-hook hook proc)
  (cond
    ((string? proc) (raw-remove-hook hook proc))
    ((symbol? proc) (raw-remove-hook hook (symbol->string proc)))
    (else
     (message "remove-hook: can't remove an anonymous procedure; add it by name or 'symbol"))))
