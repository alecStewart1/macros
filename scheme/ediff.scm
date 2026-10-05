;; ediff — file diffing built on the Rust `diff-strings` primitive (the `similar`
;; crate), the Macros take on Emacs `ediff` / `diff-mode`.
;;
;;   M-x ediff               prompt for two files, show their diff
;;   M-x ediff-current-with  diff the current file against another
;;   q                       close the *ediff* buffer
;;
;; This is the *viewer* half: a read-only unified diff with added/removed lines
;; coloured. It reads both files from disk (synchronous `read-file`) and renders
;; a single buffer, which is all the execution model allows cleanly — Scheme
;; commands see a snapshot of the *active* buffer and their edits are queued, so
;; they can't read two buffers or read back their own writes in one call.
;;
;; INTERACTIVE-MERGE (future, needs Rust): the side-by-side "copy this hunk A->B"
;; flow needs cross-buffer primitives — read/replace a line range in a *named*
;; buffer regardless of which is active (e.g. `buffer-text`, `buffer-replace`),
;; or a `diff-buffers` primitive. With those, this file can grow the two-pane
;; navigator + copy commands.

(set-face "diff-added"   "#98c379")
(set-face "diff-removed" "#e06c75")

(define *ediff-tmp-a* "")

;; Parse "tag\told_start\told_end\tnew_start\tnew_end" into a list.
(define (ediff-parse-op rec)
  (let ((p (split-many rec "\t")))
    (list (nth p 0)
          (string->number (nth p 1)) (string->number (nth p 2))
          (string->number (nth p 3)) (string->number (nth p 4)))))

;; Lines of `lst` with 0-based indices in [s,e).
(define (ediff-list-slice lst s e)
  (let loop ((i 0) (xs lst) (acc '()))
    (cond ((or (null? xs) (>= i e)) (reverse acc))
          ((>= i s) (loop (+ i 1) (cdr xs) (cons (car xs) acc)))
          (else (loop (+ i 1) (cdr xs) acc)))))

;; Turn the diff ops into a list of (display-line . face-or-#f) pairs: context
;; lines are prefixed "  ", removals "- " (A side), additions "+ " (B side).
(define (ediff-pairs la lb ops)
  (if (null? ops)
      '()
      (let* ((op (car ops)) (tag (nth op 0))
             (os (nth op 1)) (oe (nth op 2)) (ns (nth op 3)) (ne (nth op 4)))
        (append
          (cond
            ((equal? tag "equal")
             (map (lambda (l) (cons (string-append "  " l) #f))
                  (ediff-list-slice la os oe)))
            ((equal? tag "delete")
             (map (lambda (l) (cons (string-append "- " l) "diff-removed"))
                  (ediff-list-slice la os oe)))
            ((equal? tag "insert")
             (map (lambda (l) (cons (string-append "+ " l) "diff-added"))
                  (ediff-list-slice lb ns ne)))
            ((equal? tag "replace")
             (append
               (map (lambda (l) (cons (string-append "- " l) "diff-removed"))
                    (ediff-list-slice la os oe))
               (map (lambda (l) (cons (string-append "+ " l) "diff-added"))
                    (ediff-list-slice lb ns ne))))
            (else '()))
          (ediff-pairs la lb (cdr ops))))))

;; (line . face) pairs -> the per-line face list render-buffer expects.
;; Tail-recursive so a large diff can't blow the stack.
(define (ediff-faces pairs n)
  (let loop ((pairs pairs) (n n) (acc '()))
    (cond ((null? pairs) (reverse acc))
          ((cdr (car pairs))
           (loop (cdr pairs) (+ n 1) (cons (list n (cdr (car pairs))) acc)))
          (else (loop (cdr pairs) (+ n 1) acc)))))

;; Render the diff of files `a` and `b` into the read-only *ediff* buffer.
(define (ediff-render a b)
  (if (not (and (file-exists? a) (file-exists? b)))
      (message "ediff: both files must exist")
      (let* ((ta (read-file a)) (tb (read-file b))
             (pairs (ediff-pairs (split-many ta "\n") (split-many tb "\n")
                                 (map ediff-parse-op (diff-strings ta tb)))))
        (if (null? pairs)
            (message "ediff: files are identical")
            (begin
              (render-buffer "*ediff*" "diff-mode"
                             (join-lines (map car pairs))
                             (ediff-faces pairs 0))
              (set-option "line-numbers:diff-mode" "false")
              (define-key "diff-mode" "q" 'kill-buffer))))))

;; M-x ediff: prompt for the two files in turn.
(define-command (ediff)
  (read-file-name "diff file A: " 'ediff--got-a))
(define (ediff--got-a a)
  (set! *ediff-tmp-a* a)
  (read-file-name "diff file B: " 'ediff--got-b))
(define (ediff--got-b b)
  (ediff-render *ediff-tmp-a* b))

;; M-x ediff-current-with: diff the file in the current buffer against another.
(define-command (ediff-current-with)
  (let ((cur (buffer-file-name)))
    (if (and (not (equal? cur "")) (file-exists? cur))
        (begin (set! *ediff-tmp-a* cur)
               (read-file-name "diff against: " 'ediff--got-b))
        (message "ediff: current buffer is not a file"))))


;; ============================================================================
;; ediff-buffers — interactive merge between two OPEN buffers (Emacs
;; `ediff-buffers`). Renders an actionable hunk list in a *ediff* control buffer;
;; from there you copy hunks between the two buffers. Built on the cross-buffer
;; Rust primitives `diff-buffers` (async, returns hunks with text + line ranges)
;; and `buffer-copy-lines` (copy a line range between named buffers).
;;
;;   M-x ediff-buffers   pick buffer A, then B
;;   n / p               next / previous hunk
;;   a                   copy the hunk at point  A -> B
;;   b                   copy the hunk at point  B -> A
;;   g                   refresh        q   quit
;;
;; Edits land in the buffers (not on disk) — save them yourself afterwards.
;; Note: the two buffers must have distinct names (the editor names file buffers
;; by basename); for same-named files on disk use `M-x ediff` (the viewer).

(set-face "ediff-header" "#61afef")

(define *ediff-a* "")
(define *ediff-b* "")
(define *ediff-hunks* '())          ; list of (tag as ae bs be a-lines b-lines)
(define *ediff-hunk-lines* '())     ; (display-line . hunk-index), ascending
(define *ediff-us* (string (integer->char 31)))   ; the U+001F text separator

(define (ediff--decode s) (split-many s *ediff-us*))

;; One "tag\tas\tae\tbs\tbe\tabs\tabe\tbbs\tbbe\tatext\tbtext" record -> hunk.
(define (ediff--parse-hunk rec)
  (let* ((p (split-many rec "\t"))
         (as (string->number (nth p 1))) (ae (string->number (nth p 2)))
         (bs (string->number (nth p 3))) (be (string->number (nth p 4))))
    (list (nth p 0) as ae bs be
          (if (< as ae) (ediff--decode (if (>= (length p) 10) (nth p 9) "")) '())
          (if (< bs be) (ediff--decode (if (>= (length p) 11) (nth p 10) "")) '()))))

(define (ediff--face-range start count face)
  (let loop ((n 0) (acc '()))
    (if (>= n count) (reverse acc)
        (loop (+ n 1) (cons (list (+ start n) face) acc)))))

;; Render the hunk blocks into *ediff*, recording each block's header line so we
;; can map point back to a hunk.
(define (ediff--render-hunks hunks)
  (let loop ((hs hunks) (i 0) (line 0) (out '()) (faces '()) (hlines '()))
    (if (null? hs)
        (begin
          (set! *ediff-hunks* hunks)
          (set! *ediff-hunk-lines* (reverse hlines))
          (render-buffer "*ediff*" "ediff-mode"
                         (join-lines (reverse out)) (reverse faces))
          (set-option "line-numbers:ediff-mode" "false")
          (ediff--setup-keys)
          (message (string-append (int->string (length hunks))
                                  " hunks — a:A→B  b:B→A  n/p:nav  g:refresh  q:quit")))
        (let* ((h (car hs)) (tag (nth h 0))
               (as (nth h 1)) (ae (nth h 2)) (bs (nth h 3)) (be (nth h 4))
               (a-disp (map (lambda (l) (string-append "- " l)) (nth h 5)))
               (b-disp (map (lambda (l) (string-append "+ " l)) (nth h 6)))
               (hdr (string-append "── hunk " (int->string (+ i 1)) " (" tag ")  A:"
                                   (int->string as) "-" (int->string ae) "  B:"
                                   (int->string bs) "-" (int->string be)))
               (block (cons hdr (append a-disp b-disp)))
               (a-start (+ line 1))
               (b-start (+ a-start (length a-disp)))
               (new-faces (append
                            (list (list line "ediff-header"))
                            (ediff--face-range a-start (length a-disp) "diff-removed")
                            (ediff--face-range b-start (length b-disp) "diff-added"))))
          (loop (cdr hs) (+ i 1) (+ line (length block))
                (append (reverse block) out)
                (append (reverse new-faces) faces)
                (cons (cons line i) hlines))))))

(define (ediff--on-hunks records)
  (let ((hunks (map ediff--parse-hunk (non-empty-lines records))))
    (if (null? hunks)
        (begin
          (render-buffer "*ediff*" "ediff-mode" "No differences." '())
          (set-option "line-numbers:ediff-mode" "false")
          (set! *ediff-hunks* '())
          (set! *ediff-hunk-lines* '())
          (ediff--setup-keys))
        (ediff--render-hunks hunks))))

;; Hunk index whose header is the last one at or above point, or -1.
(define (ediff--hunk-at-point)
  (let ((line (buffer-line)))
    (let loop ((hl *ediff-hunk-lines*) (best -1))
      (cond ((null? hl) best)
            ((<= (car (car hl)) line) (loop (cdr hl) (cdr (car hl))))
            (else best)))))

(define-command (ediff-next-hunk)
  (let ((line (buffer-line)))
    (let loop ((hl *ediff-hunk-lines*))
      (cond ((null? hl) (message "No next hunk"))
            ((> (car (car hl)) line) (goto-line (car (car hl))))
            (else (loop (cdr hl)))))))

(define-command (ediff-prev-hunk)
  (let ((line (buffer-line)))
    (let loop ((hl (reverse *ediff-hunk-lines*)))
      (cond ((null? hl) (message "No previous hunk"))
            ((< (car (car hl)) line) (goto-line (car (car hl))))
            (else (loop (cdr hl)))))))

(define-command (ediff-copy-a->b)
  (let ((i (ediff--hunk-at-point)))
    (if (< i 0)
        (message "No hunk at point")
        (let ((h (nth *ediff-hunks* i)))
          (buffer-copy-lines *ediff-a* (nth h 1) (nth h 2)
                             *ediff-b* (nth h 3) (nth h 4))
          (ediff--refresh)))))

(define-command (ediff-copy-b->a)
  (let ((i (ediff--hunk-at-point)))
    (if (< i 0)
        (message "No hunk at point")
        (let ((h (nth *ediff-hunks* i)))
          (buffer-copy-lines *ediff-b* (nth h 3) (nth h 4)
                             *ediff-a* (nth h 1) (nth h 2))
          (ediff--refresh)))))

(define (ediff--setup-keys)
  (define-key "ediff-mode" "n" 'ediff-next-hunk)
  (define-key "ediff-mode" "p" 'ediff-prev-hunk)
  (define-key "ediff-mode" "a" 'ediff-copy-a->b)
  (define-key "ediff-mode" "b" 'ediff-copy-b->a)
  (define-key "ediff-mode" "g" 'ediff--refresh)
  (define-key "ediff-mode" "q" 'kill-buffer))

(define (ediff--refresh)
  (if (and (not (equal? *ediff-a* "")) (not (equal? *ediff-b* "")))
      (diff-buffers *ediff-a* *ediff-b* 'ediff--on-hunks)
      (message "ediff: pick two buffers first")))

(define-command (ediff-buffers)
  (completing-read "ediff buffer A: " (buffer-names) 'ediff--pick-a))
(define (ediff--pick-a a)
  (set! *ediff-a* a)
  (completing-read "ediff buffer B: " (buffer-names) 'ediff--pick-b))
(define (ediff--pick-b b)
  (set! *ediff-b* b)
  (ediff--refresh))

