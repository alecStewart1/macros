;; Editing commands written in Scheme on the new primitives — comment-dwim,
;; replace-string, and occur. Each command's edits coalesce into one undo group
;; (the editor wraps a command batch), so they undo as a unit.

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

;; --- replace-string (literal, whole buffer) ---------------------------------
(define *replace-from* "")

(define (replace-string)
  (read-string "Replace string: " "replace-string-from"))
(define (replace-string-from from)
  (set! *replace-from* from)
  (read-string (string-append "Replace \"" from "\" with: ") "replace-string-to"))
(define (replace-string-to to)
  (let* ((text (buffer-string))
         (new (replace-regexp-in-string (regexp-quote *replace-from*) to text)))
    (replace-region 0 (point-max) new)
    (message (string-append "Replaced \"" *replace-from* "\""))))

;; --- occur ------------------------------------------------------------------
;; List lines matching a regexp in a read-only *occur* buffer.
(define (occur)
  (read-string "Occur regexp: " "occur-run"))
(define (occur-run re)
  (let ((matched (filter (lambda (l) (>= (string-match re l) 0))
                         (split-many (buffer-string) "\n"))))
    (render-buffer "*occur*" "occur-mode"
                   (if (null? matched)
                       (string-append "No matches for " re)
                       (join-lines matched))
                   '())))

;; --- keymap -----------------------------------------------------------------
(define-key "fundamental" "alt-;" "comment-dwim")
(define-key "fundamental" "alt-%" "replace-string")  ; (Emacs M-% is query-replace)
(define-key "fundamental" "ctrl-c o" "occur")
(define-key "occur-mode" "q" "kill-buffer")

;; steel-mode uses ";; " line comments.
(add-hook "steel" "edit-steel-comment-string")
(define (edit-steel-comment-string) (buffer-local-set "comment-string" ";; "))

;; Make the editing entry points M-x discoverable.
(register-command "comment-dwim")
(register-command "occur")
(register-command "replace-string")
