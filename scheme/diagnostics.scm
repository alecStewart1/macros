;; requires: core

;; diagnostics.scm — surface LSP diagnostics beyond the hover echo: a
;; flycheck/flymake-style *diagnostics* list (RET jumps to the source) and
;; next-error / previous-error navigation. Built on the `(diagnostics)` primitive
;; (active-buffer records "line\tcol\tseverity\tmessage", line/col 0-based).

;; Gutter dots: lines carrying a diagnostic get a colored dot beside the line
;; number (red=error, orange=warning, blue=info/hint; worst severity wins). On by
;; default — uncomment to disable, or set in ~/.config/macros/init.scm.
;; (set-option "diagnostics-gutter" "false")

(define (diag-line-of rec) (string->number (car (split-many rec "\t"))))

;; Which buffer `next-error` / `previous-error` walk. Emacs unifies these across
;; compilation, grep, and diagnostics: whichever was used most recently wins.
;; Defined here (shell.scm requires this file) so compile's dispatcher can `set!` it.
;; "diagnostics" = LSP list (below); "compile" = the *compilation* error list
;; (shell.scm). list-diagnostics / compile-run each claim the source.
(define *next-error-source* "diagnostics")

;; --- *diagnostics* list buffer ----------------------------------------------
(define *diag-buffer* "")    ; the source buffer these diagnostics belong to
(define *diag-targets* '())  ; (list-buffer-line . source-line)
(define *diag-lines* '())
(define *diag-n* 0)

(define (diag-emit text target)
  (set! *diag-lines* (cons text *diag-lines*))
  (when target (set! *diag-targets* (cons (cons *diag-n* target) *diag-targets*)))
  (set! *diag-n* (+ *diag-n* 1)))

(define (diag-emit-rec rec)
  (let* ((parts (split-many rec "\t"))
         (line (string->number (nth parts 0)))
         (col  (string->number (nth parts 1)))
         (sev  (nth parts 2))
         (msg  (nth parts 3)))
    ;; display 1-based for humans; jump uses the 0-based source line.
    (diag-emit (string-append "  " (int->string (+ line 1)) ":" (int->string (+ col 1))
                              "  " sev ": " msg)
               line)))

(define (list-diagnostics)
  (set! *next-error-source* "diagnostics")
  (set! *diag-buffer* (current-buffer))
  (set! *diag-lines* '())
  (set! *diag-targets* '())
  (set! *diag-n* 0)
  (let ((recs (diagnostics)))
    (diag-emit (string-append "Diagnostics in " *diag-buffer*
                              " (" (int->string (length recs)) ")") #f)
    (diag-emit "" #f)
    (if (null? recs)
        (diag-emit "  No diagnostics." #f)
        (for-each diag-emit-rec recs))
    (render-buffer "*diagnostics*" "diagnostics-mode"
                   (join-lines (reverse *diag-lines*)) '())))

(define (diag-target-at line)
  (let loop ((ts *diag-targets*))
    (cond ((null? ts) #f)
          ((= (car (car ts)) line) (cdr (car ts)))
          (else (loop (cdr ts))))))

;; RET: switch back to the source buffer and jump to the diagnostic's line.
(define (diag-visit)
  (let ((target (diag-target-at (buffer-line))))
    (when target
      (switch-to-buffer *diag-buffer*)
      (goto-line target))))

(define-key "diagnostics-mode" "enter" 'diag-visit)
(define-key "diagnostics-mode" "q" 'kill-buffer)
(define-key "diagnostics-mode" "n" 'next-line)
(define-key "diagnostics-mode" "p" 'previous-line)

;; --- diagnostic navigation (one `next-error` source) ------------------------
;; Move point to the nearest diagnostic after/before the current line. The list
;; isn't sorted, so scan for the closest qualifying line. These are the
;; diagnostics arm of the unified next-error/previous-error defined in shell.scm.
(define (diag-next-error)
  (let loop ((rs (diagnostics)) (cur (buffer-line)) (best -1))
    (if (null? rs)
        (if (>= best 0) (goto-line best) (message "No more diagnostics below"))
        (let ((ln (diag-line-of (car rs))))
          (loop (cdr rs) cur
                (if (and (> ln cur) (or (< best 0) (< ln best))) ln best))))))

(define (diag-previous-error)
  (let loop ((rs (diagnostics)) (cur (buffer-line)) (best -1))
    (if (null? rs)
        (if (>= best 0) (goto-line best) (message "No more diagnostics above"))
        (let ((ln (diag-line-of (car rs))))
          (loop (cdr rs) cur
                (if (and (< ln cur) (> ln best)) ln best))))))

;; --- keys + M-x -------------------------------------------------------------
;; `next-error` / `previous-error` are the unified dispatchers (shell.scm); they
;; route to diag-next-error/diag-previous-error or compile's nav per
;; *next-error-source*. Bound + registered here since diagnostics owns C-c e.
(define-key "fundamental" "ctrl-c e" 'list-diagnostics)
(define-key "fundamental" "alt-g n" 'next-error)
(define-key "fundamental" "alt-g p" 'previous-error)
(register-command 'list-diagnostics)
(register-command 'next-error)
(register-command 'previous-error)
