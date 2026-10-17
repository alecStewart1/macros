;; requires: core

;; rainbow-delimiters: color nested brackets by their nesting depth, using the
;; live tree-sitter parse tree (the `tree-sitter-query` bridge). Because we ask
;; the grammar for bracket *nodes* — not raw characters — brackets that sit
;; inside strings or comments are skipped automatically, so this is correct for
;; non-S-expr languages (C, JS, Rust, …), not just Lisp.
;;
;; OFF BY DEFAULT. Once enabled it paints when a file is opened and repaints
;; (debounced) as you type. It uses overlay faces, so it shares the overlay-face layer with LSP
;; semantic tokens — if you enable semantic tokens for a language, disable rainbow
;; for that buffer (or vice versa) so they don't fight over the layer.
;;
;; Configurable from ~/.config/macros/init.scm (loaded after this file):
;;   (rainbow-set-colors '("#ff5555" "#50fa7b" "#bd93f9"))  ; palette, any length
;;   (rainbow-set-delay 150)                                 ; live-refresh ms
;;   (rainbow-set-skip-extensions '(".org"))                 ; where to suppress it
;;   (rainbow-enable)                                        ; turn it on
;; Interactively: `M-x rainbow-delimiters` toggles, `M-x rainbow-refresh` repaints.

;; --- configuration (mutable; change via the setters below) ------------------
(define *rainbow-colors*
  (list "#e6194b" "#f58231" "#ffe119" "#3cb44b" "#4363d8" "#911eb4" "#f032e6"))
(define *rainbow-count* (length *rainbow-colors*))
(define *rainbow-enabled* #f)       ; off by default
(define *rainbow-delay* 250)        ; debounce (ms) for live refresh

;; Register the palette as theme faces "rainbow-1".."rainbow-N" so
;; `overlay-set-face` can name them.
(define (rainbow--register-faces)
  (let loop ((cs *rainbow-colors*) (i 1))
    (when (not (null? cs))
      (set-face (string-append "rainbow-" (number->string i)) (car cs))
      (loop (cdr cs) (+ i 1)))))
(rainbow--register-faces)

;; Face name for nesting depth `d` (0-based), cycling through the palette.
(define (rainbow--face d)
  (string-append "rainbow-" (number->string (+ 1 (modulo d *rainbow-count*)))))

(define (rainbow--open? kind)
  (or (equal? kind "(") (equal? kind "[") (equal? kind "{")))
(define (rainbow--close? kind)
  (or (equal? kind ")") (equal? kind "]") (equal? kind "}")))

;; Every bracket token, captured as @b. The anonymous "(" … nodes match the
;; literal delimiters in any grammar that has them.
(define *rainbow-query* "[\"(\" \")\" \"[\" \"]\" \"{\" \"}\"] @b")

;; Parse one "capture\tstart\tend\tkind" record into (start end kind), else #f.
(define (rainbow--parse rec)
  (let ((parts (split-many rec "\t")))
    (if (>= (length parts) 4)
        (list (string->number (nth parts 1))
              (string->number (nth parts 2))
              (nth parts 3))
        #f)))

;; Sort bracket records by start offset — depth counting needs strict left-to-
;; right order (query match order isn't guaranteed across patterns).
(define (rainbow--sort lst) (sort-by car < lst))

;; Remove only the rainbow overlays: clearing every overlay face also wiped
;; spell squiggles, LSP highlights and the debugger's stopped line.
(define (rainbow--clear)
  (let loop ((i 1))
    (when (<= i *rainbow-count*)
      (clear-overlay-faces-matching (string-append "rainbow-" (number->string i)))
      (loop (+ i 1)))))

;; Callback for `tree-sitter-query`: `records` is newline-separated bracket
;; captures. Sweep left to right, tracking depth, and overlay each bracket with
;; its depth's face (an opener and its matching closer get the same color).
(define (rainbow--apply records)
  (rainbow--clear)
  (let ((brackets (rainbow--sort
                   (filter (lambda (x) x)
                           (map rainbow--parse (non-empty-lines records))))))
    ;; Accumulate the spans and paint them with ONE overlay-set-faces call — a
    ;; bracket-heavy buffer has thousands, and per-span calls dominated.
    (let loop ((bs brackets) (depth 0) (spans '()))
      (if (null? bs)
          (overlay-set-faces (reverse spans))
          (let* ((b (car bs)) (start (nth b 0)) (end (nth b 1)) (kind (nth b 2)))
            (cond
              ((rainbow--open? kind)
               (loop (cdr bs) (+ depth 1)
                     (cons (list start end (rainbow--face depth)) spans)))
              ((rainbow--close? kind)
               (let ((d (max 0 (- depth 1))))
                 (loop (cdr bs) d
                       (cons (list start end (rainbow--face d)) spans))))
              (else (loop (cdr bs) depth spans))))))))

;; Buffers where rainbow-delimiters is unwanted, matched by extension. Two
;; reasons land here:
;;   * Org (scheme/org.scm) paints its own faces onto this same shared
;;     overlay-face layer and has no tree-sitter grammar — a rainbow pass would
;;     clear org's fontification and paint nothing.
;;   * Prose languages like Markdown DO have a grammar, so the bracket query
;;     matches their link/reference brackets (`[text](url)`) and colors them as
;;     if they were code nesting. That's noise — rainbow only earns its keep in
;;     bracket-heavy code (Lisp, C, JS, …), so skip prose too.
;; The check is by name so it holds synchronously at find-file time, before a
;; mode's buffer-local state is set. Edit via the setters below.
(define *rainbow-skip-extensions*
  (list ".org" ".md" ".markdown" ".mdown" ".mkd" ".mkdn" ".txt"))
(define (rainbow--skip?)
  ;; The visited path, not the (possibly uniquified `x.md<dir>`) buffer name;
  ;; fall back to the name for fileless buffers.
  (ends-with-any? (buffer-path-or-name) *rainbow-skip-extensions*))

;; Recolor now (no-op while disabled): fire the query; the callback paints when
;; the result arrives.
(define (rainbow-refresh)
  (when (and *rainbow-enabled* (not (rainbow--skip?)))
    (tree-sitter-query *rainbow-query* 'rainbow--apply)))
(register-command 'rainbow-refresh)

;; --- public configuration API -----------------------------------------------
(define (rainbow-set-colors colors)
  (when (> (length colors) 0)
    (rainbow--clear) ; the old palette's faces, before the count changes
    (set! *rainbow-colors* colors)
    (set! *rainbow-count* (length colors))
    (rainbow--register-faces)
    (rainbow-refresh)))

(define (rainbow-set-delay ms)
  (set! *rainbow-delay* ms))

;; Replace the set of extensions where rainbow is suppressed, e.g.
;;   (rainbow-set-skip-extensions '(".org" ".md"))   ; re-enable for .txt
(define (rainbow-set-skip-extensions exts)
  (set! *rainbow-skip-extensions* exts))

(define (rainbow-enable)
  (set! *rainbow-enabled* #t)
  (rainbow-refresh))

(define (rainbow-disable)
  (set! *rainbow-enabled* #f)
  (rainbow--clear))

;; M-x rainbow-delimiters — toggle for the current view.
(define (rainbow-delimiters)
  (if *rainbow-enabled* (rainbow-disable) (rainbow-enable))
  (message (if *rainbow-enabled* "rainbow-delimiters on" "rainbow-delimiters off")))
(register-command 'rainbow-delimiters)

;; --- triggers ---------------------------------------------------------------
;; Paint on open so an opened file is colored immediately (guarded by the flag).
(add-hook "find-file-hook" "rainbow-refresh")

;; Debounced live refresh (core's make-debounce): each edit schedules a timer;
;; only the last to fire repaints. Cheap no-op while disabled. The delay is
;; passed per call so a config `(set! *rainbow-delay* …)` keeps applying.
(define rainbow--debounce
  (make-debounce "rainbow" 150
                 (lambda () (when *rainbow-enabled* (rainbow-refresh)))))
(define (rainbow--schedule)
  (when *rainbow-enabled* (rainbow--debounce *rainbow-delay*)))
(add-hook "after-change-functions" "rainbow--schedule")
