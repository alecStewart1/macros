;; markdown-mode — major mode for Markdown, the Macros analogue of markdown-mode.el.
;; Tree-sitter (tree-sitter-md) already fontifies once the mode is set, so this
;; layer adds the editing affordances: heading navigation, fold-by-heading, and
;; emphasis insert helpers. Files with a Markdown extension open here.
;;
;; Live HTML preview is intentionally out of scope — rendering Markdown to a
;; webview needs Rust/GPUI support beyond the current primitives.

(for-each (lambda (e) (set-extension e "markdown"))
          (list "md" "markdown" "mdown" "mkd" "mkdn"))

(define (md-lines) (split-many (buffer-string) "\n"))

(define (md-heading-line? line)
  (>= (string-match "^#+ " line) 0))

;; Heading level of a line ("###" -> 3), or 0 if the line isn't a heading.
(define (md-heading-level line)
  (let ((m (regexp-captures "^(#+) " line)))
    (if (null? m) 0 (string-length (nth m 1)))))

;; --- heading display: hide the `#` markers and enlarge headings ------------
;; Like Org hides leading stars, markdown-mode hides the `### ` markers from
;; display and can render headings larger. Both are display-only and refresh on
;; edit. Set these in init.scm, or flip them live with the toggle commands below.

;; Hide the leading `#` markers (and the following space) on heading lines —
;; display-only, and revealed while point is on the line. Set `#f` in init.scm
;; to keep the markers visible.
(define *markdown-hide-heading-markers* #t)

;; Render heading lines in a larger font, tapering by level (the multipliers are
;; in `md--heading-scale`). Set `#f` in init.scm to keep headings at body size.
(define *markdown-scale-headings* #t)

(define (md--active?)
  (let ((b (current-buffer)))
    (or (ends-with? b ".md") (ends-with? b ".markdown")
        (ends-with? b ".mdown") (ends-with? b ".mkd") (ends-with? b ".mkdn"))))

;; Font multiplier per heading level — h1 biggest, tapering toward body size.
(define (md--heading-scale level)
  (cond ((<= level 1) 1.6)
        ((= level 2) 1.4)
        ((= level 3) 1.25)
        ((= level 4) 1.15)
        (else 1.1)))

;; A compact signature of the heading LEVELS in order (e.g. "1;2;2;"). It changes
;; when a heading is added, removed, or re-levelled — but NOT when title text is
;; edited or lines are merely inserted/deleted (pressing Enter). The editor shifts
;; existing hide/scale overlays automatically on every edit, so whenever this is
;; unchanged we skip the clear-and-repaint — which is what kept the document from
;; blinking on each keystroke AND on Enter (line indices shift, levels don't).
(define *md-heading-signature* #f)
(define (md--heading-signature lines)
  (let loop ((ls lines) (acc ""))
    (if (null? ls)
        acc
        (let ((lvl (md-heading-level (car ls))))
          (loop (cdr ls)
                (if (> lvl 0)
                    (string-append acc (int->string lvl) ";")
                    acc))))))

;; Whole-buffer fontification walks every line twice and rebuilds the fold
;; markers — O(lines) work in interpreted Scheme, run synchronously on the engine
;; thread. On a very large file (e.g. a multi-MB THIRD-PARTY-LICENSES.md) that
;; blocks the engine long enough that M-x and every other Scheme command stop
;; responding, even though cursor movement (handled in Rust) still works. Past a
;; line threshold we skip it — the Emacs `so-long` move. tree-sitter still colors
;; the visible region; only the heading hide/scale and fold chevrons are dropped.
;; Mirrors folding.scm's *fold-auto-limit*.
(define *markdown-fontify-line-limit* 20000)
(define (md--fontify-ok?)
  (and (md--active?) (<= (line-count) *markdown-fontify-line-limit*)))

;; Repaint heading overlays across the buffer, but only when the heading
;; structure actually changed (see the signature note above). Clear our own hides
;; and scales (not overlay FACES — markdown's colors come from tree-sitter, and
;; clearing faces would fight rainbow-delimiters / LSP highlight, which repaint
;; faces on every edit).
(define (md--fontify)
  (when (md--fontify-ok?)
    (let* ((lines (md-lines))
           (sig (md--heading-signature lines)))
      (when (not (equal? sig *md-heading-signature*))
        (set! *md-heading-signature* sig)
        (clear-overlay-hides)
        (clear-line-scales)
        (let loop ((ls lines) (off 0) (i 0))
          (when (not (null? ls))
            (let* ((line (car ls))
                   (lvl (md-heading-level line))
                   (lend (+ off (utf8-length line))))
              (when (> lvl 0)
                ;; The `#`s plus the one required space are ASCII (1 byte each).
                (when *markdown-hide-heading-markers*
                  (overlay-hide off (+ off lvl 1)))
                (when *markdown-scale-headings*
                  (set-line-scale i (md--heading-scale lvl))))
              (loop (cdr ls) (+ lend 1) (+ i 1)))))
        ;; Repaint the fold chevrons whenever the heading structure changes.
        (md--refresh-fold-markers)))))

;; Force the next `md--fontify` to repaint even if the structure is unchanged —
;; used after a toggle, where the desired overlays changed but the headings
;; didn't.
(define (md--fontify-force)
  (set! *md-heading-signature* #f)
  (md--fontify))

;; Debounced live refresh (core's make-debounce): each edit schedules a timer;
;; only the last to fire repaints.
(define md--fontify-debounce
  (make-debounce "md-fontify" 200
                 (lambda () (when (md--active?) (md--fontify)))))
(define (md--fontify-schedule)
  (when (md--fontify-ok?) (md--fontify-debounce)))

;; On open, always repaint (the global signature may carry over from another
;; markdown buffer with the same heading shape); edits use the signature guard.
(add-hook "find-file-hook" "md--fontify-force")
(add-hook "after-change-functions" "md--fontify-schedule")

;; Toggle hiding of the `#` heading markers in this buffer (the
;; `*markdown-hide-heading-markers*` variable) and repaint.
(define-command (markdown-toggle-heading-markers)
  (set! *markdown-hide-heading-markers* (not *markdown-hide-heading-markers*))
  (md--fontify-force)
  (message (if *markdown-hide-heading-markers*
               "Heading markers hidden" "Heading markers shown")))

;; Toggle the larger heading font in this buffer (the
;; `*markdown-scale-headings*` variable) and repaint.
(define-command (markdown-toggle-heading-scale)
  (set! *markdown-scale-headings* (not *markdown-scale-headings*))
  (md--fontify-force)
  (message (if *markdown-scale-headings*
               "Headings enlarged" "Headings at body size")))

;; --- navigation -------------------------------------------------------------
(define-command (md-next-heading)
  (let ((lines (md-lines)))
    (let loop ((n (+ (buffer-line) 1)))
      (cond ((>= n (length lines)) (message "No next heading"))
            ((md-heading-line? (nth lines n)) (goto-line n))
            (else (loop (+ n 1)))))))

(define-command (md-previous-heading)
  (let ((lines (md-lines)))
    (let loop ((n (- (buffer-line) 1)))
      (cond ((< n 0) (message "No previous heading"))
            ((md-heading-line? (nth lines n)) (goto-line n))
            (else (loop (- n 1)))))))

;; --- folding ----------------------------------------------------------------
;; Markdown folds by heading, the same model Org uses: a heading's section is
;; everything below it down to the next same-or-higher-level heading. A gutter
;; chevron on each foldable heading toggles it (registered with folding.scm), and
;; a per-buffer set tracks which headings are folded so the chevron direction and
;; re-application stay correct.

;; Nearest heading at or above `line`, or #f.
(define (md-section-heading-line lines line)
  (let loop ((n line))
    (cond ((< n 0) #f)
          ((md-heading-line? (nth lines n)) n)
          (else (loop (- n 1))))))

;; First line index after the section starting at `start` (of level `level`):
;; the next heading whose level is the same or higher, else end of buffer. This
;; is the section body's exclusive end, so the body is the half-open
;; `[start+1, end)`.
(define (md-section-end lines start level)
  (let ((total (length lines)))
    (let loop ((n (+ start 1)))
      (cond ((>= n total) total)
            ((and (md-heading-line? (nth lines n))
                  (<= (md-heading-level (nth lines n)) level))
             n)
            (else (loop (+ n 1)))))))

;; Exclusive body end of the section headed by `start`.
(define (md--section-body-end lines start)
  (md-section-end lines start (md-heading-level (nth lines start))))

;; Heading line indices that have a non-empty body (so are actually foldable).
(define (md--foldable-headings lines)
  (let loop ((ls lines) (i 0) (acc '()))
    (cond ((null? ls) (reverse acc))
          ((and (md-heading-line? (car ls))
                (> (md--section-body-end lines i) (+ i 1)))
           (loop (cdr ls) (+ i 1) (cons i acc)))
          (else (loop (cdr ls) (+ i 1) acc)))))

;; Per-buffer set of folded heading line numbers (mirrors org's fold state).
(define *md-folded* '())
(define (md--folded-get) (buffer-state-get *md-folded* '()))
(define (md--folded-set v)
  (set! *md-folded* (buffer-state-set *md-folded* v)))
(define (md--folded? line) (if (member line (md--folded-get)) #t #f))

;; Re-hide every folded section from scratch (the only unfold primitive is
;; unfold-all, so we clear and re-apply — same as org--reapply-folds).
(define (md--reapply-folds lines)
  (unfold-all)
  (for-each
   (lambda (start)
     (when (md-heading-line? (nth lines start))
       (let ((end (md--section-body-end lines start)))
         (when (> end (+ start 1)) (fold-lines (+ start 1) end)))))
   (md--folded-get)))

;; Paint a chevron on every foldable heading (down = expanded, right = folded),
;; and drop folded entries that are no longer foldable headings. Also claims the
;; buffer's fold style so chevron clicks route here and the generic indentation
;; folder leaves this buffer alone.
(define (md--refresh-fold-markers)
  (when (md--active?)
    (buffer-local-set "fold-style" "md")
    (let ((lines (md-lines)) (still '()))
      (clear-fold-markers)
      (for-each (lambda (h)
                  (let ((folded (md--folded? h)))
                    (set-fold-marker h (not folded))
                    (when folded (set! still (cons h still)))))
                (md--foldable-headings lines))
      (md--folded-set still))))

;; Toggle the fold of the heading on `line` — the chevron click entry point.
(define (md--fold-toggle-at-line line)
  (let ((lines (md-lines)))
    (when (and (md-heading-line? (nth lines line))
               (> (md--section-body-end lines line) (+ line 1)))
      (md--folded-set
       (if (md--folded? line)
           (filter (lambda (x) (not (= x line))) (md--folded-get))
           (cons line (md--folded-get))))
      (md--reapply-folds lines)
      (md--refresh-fold-markers))))

;; Route chevron clicks in Markdown buffers (fold-style "md") to the toggle.
(fold-register-style "md" md--fold-toggle-at-line)

;; C-c TAB — toggle the fold of the section at point.
(define-command (md-fold-section)
  (let* ((lines (md-lines))
         (start (md-section-heading-line lines (buffer-line))))
    (if (not start)
        (message "Not in a section")
        (md--fold-toggle-at-line start))))

;; C-c C-a — unfold every section and repaint the chevrons all-open.
(define-command (md-unfold-all)
  (md--folded-set '())
  (unfold-all)
  (md--refresh-fold-markers))

;; --- emphasis helpers -------------------------------------------------------
(define (md-wrap-region left right)
  (if (region-active?)
      (let ((s (region-beginning)) (e (region-end)))
        (replace-region s e (string-append left (buffer-substring s e) right)))
      (begin (insert (string-append left right))
             (goto-char (- (point) (string-length right))))))

(define-command (md-bold)   (md-wrap-region "**" "**"))
(define-command (md-italic) (md-wrap-region "*" "*"))
(define-command (md-code)   (md-wrap-region "`" "`"))

;; --- keymap (consulted for buffers in markdown-mode) ------------------------
(define-key "markdown" "ctrl-c ctrl-n" 'md-next-heading)
(define-key "markdown" "ctrl-c ctrl-p" 'md-previous-heading)
(define-key "markdown" "ctrl-c tab" 'md-fold-section)
(define-key "markdown" "ctrl-c ctrl-a" 'md-unfold-all)
(define-key "markdown" "ctrl-c b" 'md-bold)
(define-key "markdown" "ctrl-c i" 'md-italic)
(define-key "markdown" "ctrl-c e" 'md-code)

