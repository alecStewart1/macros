;; Editing commands written in Scheme on the new primitives — comment-dwim.
;; Each command's edits coalesce into one undo group (the editor wraps a
;; command batch), so they undo as a unit.

;; --- comment-dwim -----------------------------------------------------------
;; The comment prefix is a buffer-local ("comment-string"); modes set it (e.g.
;; steel-mode below). Default "// ".
(define (comment-string)
  (let ((c (buffer-local-get "comment-string")))
    (if (equal? c "") "// " c)))

;; Toggle a line comment on the current line.
(define (comment-dwim)
  (let* ((b (line-bounds))
         (s (car b))
         (e (cadr b))
         (line (buffer-substring s e))
         (cs (comment-string)))
    (if (starts-with? (trim-start line) cs)
        ;; Uncomment: drop the first `cs` (keeping leading indentation).
        (replace-region s e (uncomment-line line cs))
        ;; Comment: prepend `cs`.
        (replace-region s e (string-append cs line)))))

;; Remove the first occurrence of `cs` from `line` (after leading whitespace).
(define (uncomment-line line cs)
  (let loop ((i 0))
    (cond ((> (+ i (string-length cs)) (string-length line)) line)
          ((equal? (substring line i (+ i (string-length cs))) cs)
           (string-append (substring line 0 i)
                          (substring line (+ i (string-length cs)) (string-length line))))
          (else (loop (+ i 1))))))

;; replace-string / query-replace live in replace.scm; occur (and its
;; editable occur-edit mode) in occur.scm.

;; --- keymap -----------------------------------------------------------------
(define-key "fundamental" "alt-;" 'comment-dwim)

;; steel-mode uses ";; " line comments.
(add-hook "steel" "edit-steel-comment-string")
(define (edit-steel-comment-string) (buffer-local-set "comment-string" ";; "))

;; Make the editing entry points M-x discoverable.
(register-command 'comment-dwim)
