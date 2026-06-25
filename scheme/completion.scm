;; Completion-popup keymap (corfu-style). These bindings are consulted only
;; while the in-buffer completion popup is open; otherwise the keys behave
;; normally. Rebind freely — the popup is driven entirely from here.
(define-key "completion" "ctrl-n" "completion-next")
(define-key "completion" "ctrl-p" "completion-previous")
(define-key "completion" "down" "completion-next")
(define-key "completion" "up" "completion-previous")
(define-key "completion" "tab" "completion-accept")
(define-key "completion" "enter" "completion-accept")
(define-key "completion" "ctrl-g" "completion-abort")

;; --- completion-at-point-functions (CAPF) -----------------------------------
;; `completion-at-point` (M-i) tries each registered Scheme CAPF in order before
;; falling back to the language server. A CAPF is a function of the prefix word
;; that returns either:
;;   * a flat candidate list — the editor replaces the word before point; or
;;   * `(start end candidate-list)` — to control the replaced byte range; or
;;   * `'()` / `#f` — to decline (the next CAPF, then LSP, is tried).
;; Each candidate is a bare string, or a `(text annotation)` pair whose
;; annotation shows dimmed in the popup (marginalia-style).
;;
;; Register with `(add-completion-at-point-function "name")`. To disable the CAPF
;; path entirely (strictly LSP-driven completion), set
;;   (set-option "completion-capf" "false")

;; A word character for completion purposes: [A-Za-z0-9_-] (by codepoint, so we
;; don't depend on locale char predicates).
(define (capf-word-char? cp)
  (or (and (>= cp 48) (<= cp 57))     ; 0-9
      (and (>= cp 65) (<= cp 90))     ; A-Z
      (and (>= cp 97) (<= cp 122))    ; a-z
      (= cp 95) (= cp 45)))           ; _ -

;; Every maximal word token in `text`, in order of appearance.
(define (capf-buffer-words text)
  (let loop ((chars (string->list text)) (cur '()) (acc '()))
    (define (flush acc) (if (null? cur) acc (cons (list->string (reverse cur)) acc)))
    (cond
      ((null? chars) (reverse (flush acc)))
      ((capf-word-char? (char->integer (car chars)))
       (loop (cdr chars) (cons (car chars) cur) acc))
      (else (loop (cdr chars) '() (flush acc))))))

;; Distinct elements of `lst`, preserving first-seen order.
(define (capf-dedup lst)
  (let loop ((lst lst) (seen '()) (acc '()))
    (cond ((null? lst) (reverse acc))
          ((member (car lst) seen) (loop (cdr lst) seen acc))
          (else (loop (cdr lst) (cons (car lst) seen) (cons (car lst) acc))))))

;; dabbrev: complete `prefix` from words already in the current buffer. Declines
;; (returns '()) when a language server is ready — so in code buffers LSP wins,
;; and dabbrev only kicks in for plain-text / config / scratch buffers.
(define (dabbrev-capf prefix)
  (if (or (lsp-server-ready?) (< (string-length prefix) 2))
      '()
      (capf-dedup
        (filter (lambda (w) (and (starts-with? w prefix) (not (equal? w prefix))))
                (capf-buffer-words (buffer-string))))))

(add-completion-at-point-function "dabbrev-capf")
