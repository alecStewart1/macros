;; folding.scm — generic code folding (Emacs hideshow). A gutter chevron on each
;; foldable header line toggles the visibility of its indented body. Rust renders
;; the chevron and hit-tests the click (calling `fold-toggle-at-line`); this file
;; is the policy: which lines are foldable, what each fold hides, and the
;; per-buffer fold state.
;;
;; Folding is indentation-based by default, so it works for any language without
;; a grammar: a line whose next non-blank line is more indented begins a block,
;; and folding it hides everything more-indented that follows. A major mode can
;; override by setting the buffer-local "fold-style" and supplying its own
;; toggle + marker refresh (see org.scm, which sets "fold-style" to "org").

;; --- per-buffer folded-header state ----------------------------------------
;; Buffer name -> list of header line numbers currently folded. (Buffer-locals
;; only hold strings, so the set lives in a Scheme alist like org's fold state.)
;; This drives the chevron direction; the actual hiding lives in Rust `folds`,
;; which the editor shifts to track edits on its own.
(define *fold-folded* '())
(define (fold--folded-get)
  (let ((e (alist-ref *fold-folded* (current-buffer)))) (if e e '())))
(define (fold--folded-set v)
  (set! *fold-folded*
        (cons (cons (current-buffer) v)
              (filter (lambda (p) (not (equal? (car p) (current-buffer)))) *fold-folded*))))
(define (fold--folded? line) (if (member line (fold--folded-get)) #t #f))

;; Don't auto-scan very large buffers on open (the marker pass is O(lines));
;; `M-x fold-refresh` still works on demand.
(define *fold-auto-limit* 20000)

;; --- indentation model ------------------------------------------------------
;; Next non-blank line at/after `line` (blank lines report indentation -1), or #f.
(define (fold--next-content line total)
  (let loop ((i line))
    (cond ((>= i total) #f)
          ((>= (line-indentation i) 0) i)
          (else (loop (+ i 1))))))

;; The indentation that makes `line` a fold header, or #f if it isn't one: a
;; non-blank line whose next non-blank line is more deeply indented.
(define (fold--header-indent line total)
  (let ((ind (line-indentation line)))
    (if (< ind 0) #f
        (let ((n (fold--next-content (+ line 1) total)))
          (if (and n (> (line-indentation n) ind)) ind #f)))))

;; Last body line (inclusive) of the block headed by `line` with indent `ind`:
;; the run of following lines that are blank or more indented than the header.
(define (fold--block-end line ind total)
  (let loop ((i (+ line 1)) (last line))
    (if (>= i total) last
        (let ((j (line-indentation i)))
          (cond ((< j 0) (loop (+ i 1) last))   ; blank: tentatively inside
                ((> j ind) (loop (+ i 1) i))     ; deeper: part of the body
                (else last))))))                 ; same/shallower: block ends

;; --- apply / refresh --------------------------------------------------------
;; Re-hide every still-valid folded block from the recorded state (folds are
;; rebuilt from scratch each time, like org--reapply-folds).
(define (fold--reapply)
  (unfold-all)
  (let ((total (line-count)))
    (for-each
     (lambda (line)
       (let ((ind (fold--header-indent line total)))
         (when ind
           (let ((e (fold--block-end line ind total)))
             (when (>= e (+ line 1)) (fold-lines (+ line 1) (+ e 1)))))))
     (fold--folded-get))))

;; Paint a chevron on every fold header (down = open, right = folded), and drop
;; recorded folded lines that are no longer headers so state can't go stale.
(define (fold-refresh-markers)
  (clear-fold-markers)
  (let ((total (line-count)) (still '()))
    (let loop ((i 0))
      (when (< i total)
        (when (fold--header-indent i total)
          (let ((folded (fold--folded? i)))
            (set-fold-marker i (not folded))
            (when folded (set! still (cons i still)))))
        (loop (+ i 1))))
    (fold--folded-set still)))

;; --- toggle (indentation style) --------------------------------------------
(define (fold--indent-toggle line)
  (let ((total (line-count)))
    (when (fold--header-indent line total)
      (fold--folded-set
       (if (fold--folded? line)
           (filter (lambda (x) (not (= x line))) (fold--folded-get))
           (cons line (fold--folded-get))))
      (fold--reapply)
      (fold-refresh-markers))))

;; --- public entry points ----------------------------------------------------
;; The buffer's fold style ("indent" by default; a mode may set "org", etc.).
(define (fold--style)
  (let ((s (buffer-local-get "fold-style")))
    (if (equal? s "") "indent" s)))

;; Per-style toggle handlers, registered at load by the owning mode. A registry
;; (not a direct call) so this file needn't name functions defined in later-
;; loading mode files — Steel resolves globals at compile time. Each handler is a
;; procedure of one argument (the header line).
(define *fold-handlers* '())
(define (fold-register-style style proc)
  (set! *fold-handlers*
        (cons (cons style proc)
              (filter (lambda (p) (not (equal? (car p) style))) *fold-handlers*))))

;; Toggle the fold whose header is `line`, dispatched on the buffer's style. This
;; is the exact name Rust's chevron click invokes (with an explicit line, since a
;; click can't move point first — queued motions don't apply mid-script).
(define (fold-toggle-at-line line)
  (let ((h (alist-ref *fold-handlers* (fold--style))))
    (if h (h line) (fold--indent-toggle line))))

;; Toggle the fold of the block at (or enclosing) point — the keyboard command.
(define (fold-toggle-at-point) (fold-toggle-at-line (buffer-line)))

;; The fold header at or enclosing `line` (indentation style): the line
;; itself when it heads a block, else the nearest header above whose block
;; reaches this line. #f when there is none.
(define (fold--header-for line)
  (let ((total (line-count)))
    (let loop ((i line))
      (cond ((< i 0) #f)
            ((let ((ind (fold--header-indent i total)))
               (and ind (or (= i line) (>= (fold--block-end i ind total) line))))
             i)
            (else (loop (- i 1)))))))

;; zc / zo — close / open the fold at (or around) point. Other fold styles
;; (org, markdown) only know toggling, so these toggle there.
(define (fold--set-at-point closed)
  (if (not (equal? (fold--style) "indent"))
      (fold-toggle-at-point)
      (let ((h (fold--header-for (buffer-line))))
        (when (and h (not (eq? (fold--folded? h) closed)))
          (fold--indent-toggle h)
          (when closed (goto-line h))))))
(define (fold-close-at-point) (fold--set-at-point #t))
(define (fold-open-at-point) (fold--set-at-point #f))

;; zj / zk — the next / previous fold header line.
(define (fold--header-scan from step)
  (let ((total (line-count)))
    (let loop ((i (+ from step)))
      (cond ((or (< i 0) (>= i total)) #f)
            ((fold--header-indent i total) i)
            (else (loop (+ i step)))))))
(define (fold-next-header)
  (let ((h (fold--header-scan (buffer-line) 1)))
    (if h (goto-line h) (message "No fold below"))))
(define (fold-previous-header)
  (let ((h (fold--header-scan (buffer-line) -1)))
    (if h (goto-line h) (message "No fold above"))))

;; Fold every block in the buffer (indentation style).
(define (fold-all)
  (let ((total (line-count)) (acc '()))
    (let loop ((i 0))
      (when (< i total)
        (when (fold--header-indent i total) (set! acc (cons i acc)))
        (loop (+ i 1))))
    (fold--folded-set acc)
    (fold--reapply)
    (fold-refresh-markers)))

;; Unfold everything and repaint the chevrons all-open.
(define (fold-show-all)
  (fold--folded-set '())
  (unfold-all)
  (fold-refresh-markers))

;; M-x alias.
(define (fold-refresh) (fold-refresh-markers))

;; --- enable on file visit ---------------------------------------------------
;; Mark fold headers when a file is opened — only for buffers using the default
;; indentation style (a mode that owns folding, e.g. org/markdown, sets its own
;; "fold-style" and installs its own chevrons) and not too large to scan. Special
;; generated buffers (magit/dired/dired-sidebar) never hit find-file-hook.
(define (fold--setup)
  (when (and (equal? (fold--style) "indent")
             (<= (line-count) *fold-auto-limit*))
    (fold-refresh-markers)))
(add-hook "find-file-hook" "fold--setup")
;; Re-mark after a save: cheap, and picks up structural edits made since open.
(add-hook "after-save-hook" "fold--setup")

;; `C-c f`, NOT `C-c C-f` — yasnippet.scm binds that for snippet-next-field and
;; loads later, so this binding was silently dead.
(define-key "fundamental" "ctrl-c f" 'fold-toggle-at-point)
(register-command 'fold-toggle-at-point)
(register-command 'fold-all)
(register-command 'fold-show-all)
(register-command 'fold-refresh)
(register-command 'fold-close-at-point)
(register-command 'fold-open-at-point)
