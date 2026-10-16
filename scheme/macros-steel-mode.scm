;; requires: core completion help

;; macros-steel-mode — the editor-aware major mode for *Macros* config and
;; extension code (your init.scm, anything under ~/.config/macros, or a file
;; named *.macros.scm). It is deliberately distinct from plain `steel` mode:
;;
;;   * steel        — editing arbitrary Scheme/Steel. The built-in language
;;                    server is a vanilla interpreter; editor functions are not
;;                    in scope, so a reference to `buffer-line` is "unknown".
;;   * macros-steel — editing code that EXTENDS this editor. Rust routes these
;;                    files to the `macros-scheme` language, whose built-in server
;;                    is seeded with the whole editor API, so completion and
;;                    diagnostics know every editor function and command.
;;
;; Both share Scheme highlighting and indentation; this file layers the extra,
;; editor-aware affordances on top: eval into the live runtime, and one-key
;; documentation for the symbol at point.
;;
;; Activation is automatic by path (see `is_macros_scheme_path` in Rust): files
;; named *.macros.scm or living under the config dir. For editing this editor's
;; OWN sources (the macros repo's scheme/*.scm), which match neither rule, run
;; `M-x macros-steel-mode` — the Rust builtin re-attaches the `macros-scheme`
;; language (highlighter + editor-API server), not just this keymap.
;;
;; Requires completion.scm and help.scm, whose `capf-word-char?`,
;; `doc-summary`, and `describe-function-show` it reuses.

;; --- keymap -----------------------------------------------------------------
;; Evaluation targets the LIVE editor runtime (it can define commands, rebind
;; keys, mutate buffers) — that is the whole point of an extension file, and the
;; reason this is its own mode rather than a flag on `steel`.
(define-key "macros-steel" "ctrl-c ctrl-e" 'eval-last-sexp)
(define-key "macros-steel" "ctrl-x ctrl-e" 'eval-last-sexp)  ; Emacs muscle memory
(define-key "macros-steel" "ctrl-c ctrl-b" 'eval-buffer)
;; Editor-API documentation for the symbol at point: a one-line echo (eldoc), or
;; the full doc buffer.
(define-key "macros-steel" "ctrl-c ctrl-d" 'macros-eldoc)
(define-key "macros-steel" "ctrl-c ctrl-f" 'macros-describe-point)

;; --- symbol at point --------------------------------------------------------
;; The codepoint of a one-character string (its first char).
(define (macros--cp s) (char->integer (car (string->list s))))

;; Is the byte at offset `i` part of an identifier? (Single-byte / ASCII scan —
;; identifiers in editor code are ASCII, and `buffer-substring` is byte-indexed.)
(define (macros--word-at? i)
  (let ((c (buffer-substring i (+ i 1))))
    (and (= (string-length c) 1) (capf-word-char? (macros--cp c)))))

;; The identifier surrounding point ("" if point isn't on one), found by
;; extending left and right over word characters from the cursor.
(define (macros-symbol-at-point)
  (let ((p (point)) (end-max (point-max)))
    (let ((start (let loop ((i p))
                   (if (and (> i 0) (macros--word-at? (- i 1))) (loop (- i 1)) i)))
          (stop  (let loop ((i p))
                   (if (and (< i end-max) (macros--word-at? i)) (loop (+ i 1)) i))))
      (buffer-substring start stop))))

;; --- documentation commands -------------------------------------------------
;; eldoc: echo the one-line summary of the editor function/command at point.
(define (macros-eldoc)
  (let ((sym (macros-symbol-at-point)))
    (if (equal? sym "")
        (message "")
        (let ((summary (doc-summary sym)))
          (message (if (equal? summary "")
                       (string-append sym " — (no documentation)")
                       (string-append sym " — " summary)))))))

;; describe-function for the symbol at point, straight to the *help* buffer.
(define (macros-describe-point)
  (let ((sym (macros-symbol-at-point)))
    (when (not (equal? sym "")) (describe-function-show sym))))

(register-command 'macros-eldoc)
(register-command 'macros-describe-point)
