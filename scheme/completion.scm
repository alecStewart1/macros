;; Completion-popup keymap (corfu-style). These bindings are consulted only
;; while the in-buffer completion popup is open; otherwise the keys behave
;; normally. Rebind freely — the popup is driven entirely from here.
(define-key "completion" "ctrl-n" 'completion-next)
(define-key "completion" "ctrl-p" 'completion-previous)
(define-key "completion" "down" 'completion-next)
(define-key "completion" "up" 'completion-previous)
(define-key "completion" "tab" 'completion-next)     ; TAB cycles the candidates…
(define-key "completion" "enter" 'completion-accept) ; …ENTER accepts the selected one
(define-key "completion" "ctrl-g" 'completion-abort)

;; --- per-language completion trigger characters -----------------------------
;; Characters that auto-open the completion popup with no word prefix (member
;; access), ADDED to whatever the language server advertises. The built-in
;; default is "." for every language, so `Stdlib.` auto-completes even under a
;; server that under-advertises (ocamllsp lists no trigger characters). Override
;; globally or per language in init.scm (loaded last), e.g.
;;   (set-option "completion-trigger-chars:rust" ".:")   ; add `:` for paths
;;   (set-option "completion-trigger-chars" "")          ; server-advertised only

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

;; --- argument-aware completion ----------------------------------------------
;; A general CAPF that completes the *argument* of a known call by its position.
;; Register a producer for a (head, arg-index) pair and, when point sits in that
;; argument of a `(head …)` form, its candidates are offered. The producer is a
;; function of the partial symbol returning a candidate list (a thunk-style
;; producer that ignores the prefix is fine — the CAPF filters by prefix itself).
;;
;;   (define-arg-completion 'load-theme 0 (lambda (_) (theme-names)))
;;
;; This is how `(load-theme 'mac…)` offers theme names in a source buffer; add
;; entries for any command whose argument draws from a known set.

(define *arg-completions* '())   ; ((head-string . index) . producer)

(define (arg-completion-key head index)
  (cons (if (symbol? head) (symbol->string head) head) index))

(define (define-arg-completion head index producer)
  (set! *arg-completions*
        (cons (cons (arg-completion-key head index) producer) *arg-completions*)))

(define (arg-completion-lookup head index)
  (let ((hit (assoc (cons head index) *arg-completions*)))
    (and hit (cdr hit))))

;; The trailing run of word characters ending `text` — the symbol being typed,
;; minus any leading non-word punctuation like the quote in `'macros-dark`. Used
;; as both the filter prefix and (by its length) the replacement start, since the
;; editor's default word boundary stops at `-` and would mis-scope hyphenated ids.
(define (capf-symbol-prefix text)
  (let loop ((chars (reverse (string->list text))) (acc '()))
    (if (and (pair? chars) (capf-word-char? (char->integer (car chars))))
        (loop (cdr chars) (cons (car chars) acc))
        (list->string acc))))

;; Scan `text` (buffer contents ending at point) and return (head . arg-index)
;; for the innermost enclosing list: head is its first symbol, arg-index the
;; 0-based position of the argument under point (the head is not an argument).
;; Returns #f at top level or while typing the head itself. A single forward pass
;; tracks a stack of frames — (completed-atom-count . head) — plus the in-progress
;; atom and string state, so quotes and nested parens don't fool the arg counter.
(define (capf-arg-context text)
  (define (close-atom stack cur)
    (if (null? stack)
        stack
        (let* ((frame (car stack))
               (count (car frame))
               (head  (cdr frame))
               (atom  (list->string (reverse cur))))
          (cons (cons (+ count 1)
                      (if (and (= count 0) (> (string-length atom) 0)) atom head))
                (cdr stack)))))
  ;; in-str / esc track string literals; cmt tracks a `;` line comment (so stray
  ;; parens/quotes/apostrophes in comments don't fool the counter).
  (let loop ((chars (string->list text)) (stack '()) (cur '()) (in-str #f) (esc #f) (cmt #f))
    (cond
      ((null? chars)
       (if (or cmt (null? stack))
           #f                                   ; in a comment at point → no context
           (let ((count (car (car stack))) (head (cdr (car stack))))
             (if (and head (> count 0)) (cons head (- count 1)) #f))))
      (in-str
       (let ((c (car chars)))
         (cond
           (esc            (loop (cdr chars) stack (cons c cur) #t #f #f))
           ((char=? c #\\) (loop (cdr chars) stack (cons c cur) #t #t #f))
           ((char=? c #\") (loop (cdr chars) stack (cons c cur) #f #f #f))
           (else           (loop (cdr chars) stack (cons c cur) #t #f #f)))))
      (cmt
       ;; skip to end of line
       (if (char=? (car chars) #\newline)
           (loop (cdr chars) stack '() #f #f #f)
           (loop (cdr chars) stack '() #f #f #t)))
      (else
       (let ((c (car chars)))
         (cond
           ((char=? c #\;)
            (let ((s (if (null? cur) stack (close-atom stack cur))))
              (loop (cdr chars) s '() #f #f #t)))
           ((char=? c #\")
            (loop (cdr chars) stack (cons c cur) #t #f #f))
           ((char=? c #\()
            (let ((s (if (null? cur) stack (close-atom stack cur))))
              (loop (cdr chars) (cons (cons 0 #f) s) '() #f #f #f)))
           ((char=? c #\))
            (let* ((s1 (if (null? cur) stack (close-atom stack cur)))
                   (s2 (if (null? s1) '() (cdr s1)))   ; pop the closed frame
                   ;; the closed sub-form counts as one completed atom in the parent
                   (s3 (if (null? s2)
                           s2
                           (let ((pc (car (car s2))) (ph (cdr (car s2))))
                             (cons (cons (+ pc 1) (if (= pc 0) "" ph)) (cdr s2))))))
              (loop (cdr chars) s3 '() #f #f #f)))
           ((char-whitespace? c)
            (let ((s (if (null? cur) stack (close-atom stack cur))))
              (loop (cdr chars) s '() #f #f #f)))
           (else
            (loop (cdr chars) stack (cons c cur) #f #f #f))))))))

;; CAPF: if point is in a registered argument position, offer that producer's
;; candidates filtered by the symbol under point, replacing the symbol itself
;; (an explicit (start end cands) range, so hyphenated ids replace whole).
(define (arg-completion-capf prefix)
  (let* ((p (point))
         (text (buffer-substring (max 0 (- p 2048)) p))
         (ctx (capf-arg-context text)))
    (if (not ctx)
        '()
        (let ((producer (arg-completion-lookup (car ctx) (cdr ctx))))
          (if (not producer)
              '()
              (let* ((sym (capf-symbol-prefix text))
                     (start (- p (string-length sym)))
                     (cands (filter (lambda (c) (starts-with? c sym)) (producer sym))))
                (if (null? cands) '() (list start p cands))))))))

(add-completion-at-point-function "arg-completion-capf")

;; Built-in argument completion: `(load-theme 'mac…)` offers the bundled theme
;; names (the producer ignores the prefix — the CAPF filters). The matching M-x
;; `load-theme` command lives in core.scm.
(define-arg-completion 'load-theme 0 (lambda (_) (theme-names)))

;; dabbrev: complete `prefix` from words already in the current buffer. Declines
;; (returns '()) when a language server is ready — so in code buffers LSP wins,
;; and dabbrev only kicks in for plain-text / config / scratch buffers.
(define (dabbrev-capf prefix)
  (if (or (lsp-server-ready?) (< (string-length prefix) 1))
      '()
      (capf-dedup
        (filter (lambda (w) (and (starts-with? w prefix) (not (equal? w prefix))))
                (capf-buffer-words (buffer-string))))))

(add-completion-at-point-function "dabbrev-capf")
