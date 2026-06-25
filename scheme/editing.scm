;; editing.scm — core Emacs editing commands, on the range-edit / regexp / read
;; primitives. Region-aware where Emacs is.

;; The active region as (start end), or the fallback range [a b).
(define (region-or a b)
  (if (region-active?) (list (region-beginning) (region-end)) (list a b)))

;; Byte offset of the start of the line containing `pos`.
(define (line-start-before pos)
  (let loop ((i pos))
    (if (or (= i 0) (equal? (buffer-substring (- i 1) i) "\n")) i (loop (- i 1)))))

;; --- sort-lines (region, or whole buffer) -----------------------------------
(define (sort-lines)
  (let* ((r (region-or 0 (point-max))) (s (car r)) (e (cadr r)))
    (replace-region s e (join-lines (sort-strings (split-many (buffer-substring s e) "\n"))))
    (message "Sorted lines")))

;; --- case commands (region) -------------------------------------------------
(define (upcase-region)
  (let* ((r (region-or (point) (point))) (s (car r)) (e (cadr r)))
    (when (< s e)
      (replace-region s e (string-upcase (buffer-substring s e)))
      (message "Upcased region"))))
(define (downcase-region)
  (let* ((r (region-or (point) (point))) (s (car r)) (e (cadr r)))
    (when (< s e)
      (replace-region s e (string-downcase (buffer-substring s e)))
      (message "Downcased region"))))
(define (capitalize-token tok)
  (if (equal? tok "")
      tok
      (string-append (string-upcase (substring tok 0 1))
                     (string-downcase (substring tok 1 (string-length tok))))))
(define (capitalize-region)
  (let* ((r (region-or (point) (point))) (s (car r)) (e (cadr r)))
    (when (< s e)
      (replace-region s e (string-join (map capitalize-token
                                            (split-many (buffer-substring s e) " ")) " "))
      (message "Capitalized region"))))

;; --- goto-line (M-g g) ------------------------------------------------------
(define (goto-line-cmd) (read-string "Goto line: " "goto-line-finish"))
(define (goto-line-finish s)
  (let ((n (string->number (trim-end s))))
    (when n (goto-line (max 0 (- n 1))))))   ; user enters 1-based; goto-line is 0-based

;; --- join-line (M-^): pull the next line up, collapsing indentation to a space.
(define (join-line)
  (let* ((b (line-bounds)) (e (cadr b)) (mx (point-max)))
    (when (< e mx)
      (let loop ((i (+ e 1)))
        (if (and (< i mx)
                 (let ((c (buffer-substring i (+ i 1)))) (or (equal? c " ") (equal? c "\t"))))
            (loop (+ i 1))
            (replace-region e i " "))))))

;; --- zap-to-char (M-z): delete from point up to and including a character ----
(define (zap-to-char) (read-string "Zap to char: " "zap-to-char-finish"))
(define (zap-to-char-finish s)
  (when (> (string-length s) 0)
    (let ((idx (search-forward (substring s 0 1))))
      (when (>= idx 0) (delete-region (point) (+ idx 1))))))

;; --- transpose-lines (C-x C-t): swap this line with the previous -------------
(define (transpose-lines)
  (let* ((b (line-bounds)) (cur-s (car b)) (cur-e (cadr b)))
    (when (> cur-s 0)
      (let* ((prev-e (- cur-s 1))                 ; the newline before this line
             (prev-s (line-start-before prev-e)))
        (replace-region prev-s cur-e
                        (string-append (buffer-substring cur-s cur-e) "\n"
                                       (buffer-substring prev-s prev-e)))))))

;; --- keymap + M-x registration ----------------------------------------------
(define-key "fundamental" "alt-g g" "goto-line-cmd")
(define-key "fundamental" "alt-^" "join-line")
(define-key "fundamental" "alt-z" "zap-to-char")
(define-key "fundamental" "ctrl-x ctrl-t" "transpose-lines")

(register-command "sort-lines")
(register-command "upcase-region")
(register-command "downcase-region")
(register-command "capitalize-region")
(register-command "goto-line-cmd")
(register-command "join-line")
(register-command "zap-to-char")
(register-command "transpose-lines")
