;; lib-section — a generic collapsible-section library, the reusable core
;; extracted from magit's status buffer (which builds on it — the "lib-" name
;; keeps this file sorting BEFORE magit.scm, since scheme/ files evaluate in
;; sorted filename order and a file may only use earlier definitions). It sits on the same special-buffer primitives
;; magit uses (`render-buffer`, `fold-lines`, `overlay-set-face`,
;; `clear-overlays`, `buffer-line`), but knows nothing about git: any package can
;; build a foldable, navigable, hierarchical read-only buffer on top of it
;; (think Emacs `magit-section.el` as a standalone dependency).
;;
;; Shape of a build:
;;   (section-begin)
;;   (section-line "Header" "section-heading")        ; a plain (non-foldable) line
;;   (section-open 'files "Files (3)" "section-heading")
;;     (section-line "  a.txt" "section-item")
;;     (section-line "  b.txt" "section-item")
;;   (section-close 'files)
;;   (section-render "*my-buffer*" "my-mode")
;;
;; Sections nest: open another section between an open/close pair and folding the
;; outer one hides the inner one too. Each section has an id (any value), used to
;; remember the user's fold toggles across re-renders (a refresh rebuilds the
;; text; folds persist by id, exactly like magit).

;; --- build state (reset each render) ----------------------------------------
(define *section-lines* '())    ; rendered lines, reversed while building
(define *section-faces* '())    ; (line face) whole-line colors
(define *section-n* 0)          ; next line number
(define *section-byte* 0)       ; byte offset of the next line (for accents)
(define *section-accents* '())  ; (start end face) sub-line overlay accents
(define *section-records* '())  ; finished sections (see record shape below)
(define *section-stack* '())    ; sections currently open (being built)

;; --- persistent fold state (survives re-renders) ----------------------------
;; Ids the user has toggled away from the default. A section renders folded when
;; (xor default-collapsed (toggled? id)).
(define *section-toggled* '())
(define *section-default-collapsed* #f)

;; Whether new sections collapse by default. Set before rendering.
(define (section-default-collapsed! flag)
  (set! *section-default-collapsed* flag))

;; A section record: (id label face heading body-start body-end value).
(define (make-section-record id label face heading body-start body-end value)
  (list id label face heading body-start body-end value))
(define (section-id s)         (nth s 0))
(define (section-label s)      (nth s 1))
(define (section-face s)       (nth s 2))
(define (section-heading s)    (nth s 3))
(define (section-body-start s) (nth s 4))
(define (section-body-end s)   (nth s 5))
(define (section-value s)      (nth s 6))

;; --- emitting lines ---------------------------------------------------------
(define (section-begin)
  (set! *section-lines* '())
  (set! *section-faces* '())
  (set! *section-n* 0)
  (set! *section-byte* 0)
  (set! *section-accents* '())
  (set! *section-records* '())
  (set! *section-stack* '()))

;; Emit one line with an optional whole-line face (pass #f or omit for none).
(define (section-line text . face)
  (let ((f (if (null? face) #f (car face))))
    (set! *section-lines* (cons text *section-lines*))
    (when f (set! *section-faces* (cons (list *section-n* f) *section-faces*))))
  (set! *section-n* (+ *section-n* 1))
  (set! *section-byte* (+ *section-byte* (utf8-length text) 1))) ; +1 newline

;; Sub-line accent: color bytes [start,end) via an overlay (e.g. a status label
;; inside a heading). `section-byte` before an emit is that line's start byte.
(define (section-accent start end face)
  (set! *section-accents* (cons (list start end face) *section-accents*)))

;; The byte offset at which the NEXT emitted line begins (for computing accents).
(define (section-point-byte) *section-byte*)

;; The line number the NEXT emitted line will get (for recording headings).
(define (section-point-line) *section-n*)

;; The accumulated buffer text / per-line faces, in emit order — what a custom
;; renderer (e.g. magit's quiet refresh) passes to render-buffer itself.
(define (section-text) (join-lines (reverse *section-lines*)))
(define (section-line-faces) (reverse *section-faces*))

;; Open a collapsible section: emit its heading line, then push it on the stack.
;; Body lines (and nested sections) emitted until the matching `section-close`
;; become its foldable body. `value` (optional) is arbitrary data carried on the
;; section — e.g. a file path or row id — retrievable via `section-value`.
(define (section-open id label face . value)
  (let ((heading *section-n*)
        (v (if (null? value) id (car value))))
    (section-line label face)
    (set! *section-stack*
          (cons (list id label face heading *section-n* v) *section-stack*))))

;; Close the most recently opened section (id is accepted for readability and to
;; catch mismatches, but the stack is authoritative).
(define (section-close . id)
  (when (not (null? *section-stack*))
    (let* ((open (car *section-stack*))
           (rec (make-section-record (nth open 0) (nth open 1) (nth open 2)
                                     (nth open 3) (nth open 4) *section-n* (nth open 5))))
      (set! *section-records* (cons rec *section-records*))
      (set! *section-stack* (cdr *section-stack*)))))

;; --- rendering --------------------------------------------------------------
(define (section-folded? id)
  (let ((toggled (member id *section-toggled*)))
    (if toggled (not *section-default-collapsed*) *section-default-collapsed*)))

;; Paint the accumulated sub-line accents onto the CURRENT buffer. Shared by
;; section-apply-overlays and by consumers with their own fold model (magit).
;; NOTE: like all the build state, accents belong to the LAST build — callers
;; that re-apply later (fold toggles) assume no other section build ran since.
(define (section-apply-accents)
  (for-each (lambda (a) (overlay-set-face (nth a 0) (nth a 1) (nth a 2)))
            (reverse *section-accents*)))

;; Re-apply accents + folds to the CURRENT buffer (after a render, or after a
;; toggle that didn't rebuild the text). clear-overlays first so stale folds and
;; accents don't accumulate.
(define (section-apply-overlays)
  (clear-overlays)
  (section-apply-accents)
  (for-each (lambda (sec)
              (when (and (section-folded? (section-id sec))
                         (> (section-body-end sec) (section-body-start sec)))
                (fold-lines (section-body-start sec) (section-body-end sec))))
            *section-records*))

;; Draw the accumulated lines into read-only buffer `name` with major mode
;; `mode`, then apply folds/accents. Any sections left open are auto-closed.
(define (section-render name mode)
  (let loop () (when (not (null? *section-stack*)) (section-close) (loop)))
  (render-buffer name mode
                 (join-lines (reverse *section-lines*))
                 (reverse *section-faces*))
  (section-apply-overlays))

;; --- navigation / interaction -----------------------------------------------
;; The innermost section whose heading or body contains `line`, or #f. "Innermost"
;; = the matching section with the greatest heading line (nested sections open
;; after their parent, so they have larger heading numbers).
(define (section-at-line line)
  (let loop ((recs *section-records*) (best #f))
    (cond
      ((null? recs) best)
      (else
       (let ((s (car recs)))
         (loop (cdr recs)
               (if (and (or (= (section-heading s) line)
                            (and (>= line (section-body-start s))
                                 (< line (section-body-end s))))
                        (or (not best) (> (section-heading s) (section-heading best))))
                   s best)))))))

;; The section at point (the line the cursor is on), or #f.
(define (section-current) (section-at-line (buffer-line)))

;; Toggle the fold of the section at point, then re-apply folds in place (no
;; rebuild — the text and records are unchanged, only which sections are folded).
(define (section-toggle-at-point)
  (let ((s (section-current)))
    (when s
      (let ((id (section-id s)))
        (set! *section-toggled*
              (if (member id *section-toggled*)
                  (filter (lambda (x) (not (equal? x id))) *section-toggled*)
                  (cons id *section-toggled*))))
      (section-apply-overlays))))
