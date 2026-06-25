;; dired.scm — a directory editor (file manager), on list-directory + the
;; special-buffer model + call-process for fs operations. C-x d opens it.

(define *dired-dir* "")
(define *dired-lines* '())    ; rendered lines (reversed while building)
(define *dired-entries* '())  ; (line . fullpath)
(define *dired-marks* '())    ; fullpaths marked for deletion
(define *dired-n* 0)

(define (dired-emit text path)
  (set! *dired-lines* (cons text *dired-lines*))
  (when path (set! *dired-entries* (cons (cons *dired-n* path) *dired-entries*)))
  (set! *dired-n* (+ *dired-n* 1)))

;; basename, keeping a trailing "/" for directories.
(define (dired-display full)
  (if (ends-with? full "/") (string-append (basename full) "/") (basename full)))

(define (dired-render)
  (set! *dired-lines* '())
  (set! *dired-entries* '())
  (set! *dired-n* 0)
  (dired-emit (string-append *dired-dir* ":") #f)
  (dired-emit "" #f)
  (for-each (lambda (full)
              (let ((mark (if (member full *dired-marks*) "D " "  ")))
                (dired-emit (string-append mark (dired-display full)) full)))
            (list-directory *dired-dir*))
  (render-buffer "*dired*" "dired-mode" (join-lines (reverse *dired-lines*)) '()))

(define (dired)
  (set! *dired-dir* (buffer-directory))
  (set! *dired-marks* '())
  (dired-render))
(define (dired-in dir)
  (set! *dired-dir* dir)
  (set! *dired-marks* '())
  (dired-render))

;; The fullpath on the current line, or #f.
(define (dired-at-point)
  (let loop ((es *dired-entries*) (line (buffer-line)))
    (cond ((null? es) #f)
          ((= (car (car es)) line) (cdr (car es)))
          (else (loop (cdr es) line)))))

;; --- commands ---------------------------------------------------------------
(define (dired-visit)
  (let ((p (dired-at-point)))
    (when p (if (ends-with? p "/") (dired-in p) (open-file p)))))
(define (dired-up) (dired-in (parent-directory *dired-dir*)))
(define (dired-refresh) (dired-render))

(define (dired-mark)
  (let ((p (dired-at-point)))
    (when (and p (not (member p *dired-marks*)))
      (set! *dired-marks* (cons p *dired-marks*))
      (dired-render)
      (next-line))))
(define (dired-unmark)
  (let ((p (dired-at-point)))
    (when p
      (set! *dired-marks* (filter (lambda (m) (not (equal? m p))) *dired-marks*))
      (dired-render)
      (next-line))))

(define (dired-do-delete)
  (if (null? *dired-marks*)
      (message "No files marked (mark with d)")
      (y-or-n-p (string-append "Delete " (int->string (length *dired-marks*)) " marked file(s)?")
                "dired-delete-confirmed")))
(define (dired-delete-confirmed ans)
  (when (equal? ans "yes")
    (for-each (lambda (p) (call-process "rm" (list "-rf" p))) *dired-marks*)
    (set! *dired-marks* '())
    (dired-render)
    (message "Deleted")))

(define *dired-src* "")
(define (dired-rename)
  (let ((p (dired-at-point)))
    (when p
      (set! *dired-src* p)
      (read-string (string-append "Rename " (basename p) " to: ") "dired-rename-to"))))
(define (dired-rename-to name)
  (when (not (equal? (trim-end name) ""))
    (call-process "mv" (list *dired-src* (path-join *dired-dir* name)))
    (dired-render) (message "Renamed")))
(define (dired-copy)
  (let ((p (dired-at-point)))
    (when p
      (set! *dired-src* p)
      (read-string (string-append "Copy " (basename p) " to: ") "dired-copy-to"))))
(define (dired-copy-to name)
  (when (not (equal? (trim-end name) ""))
    (call-process "cp" (list "-r" *dired-src* (path-join *dired-dir* name)))
    (dired-render) (message "Copied")))
(define (dired-mkdir) (read-string "Create directory: " "dired-mkdir-finish"))
(define (dired-mkdir-finish name)
  (when (not (equal? (trim-end name) ""))
    (call-process "mkdir" (list "-p" (path-join *dired-dir* name)))
    (dired-render) (message "Created directory")))

;; A directory listing, not code — no line-number gutter.
(set-option "line-numbers:dired-mode" "false")

;; --- keymap -----------------------------------------------------------------
(define-key "dired-mode" "enter" "dired-visit")
(define-key "dired-mode" "^" "dired-up")
(define-key "dired-mode" "g" "dired-refresh")
(define-key "dired-mode" "d" "dired-mark")
(define-key "dired-mode" "u" "dired-unmark")
(define-key "dired-mode" "x" "dired-do-delete")
(define-key "dired-mode" "shift-r" "dired-rename")
(define-key "dired-mode" "shift-c" "dired-copy")
(define-key "dired-mode" "+" "dired-mkdir")
(define-key "dired-mode" "n" "next-line")
(define-key "dired-mode" "p" "previous-line")
(define-key "dired-mode" "q" "kill-buffer")

(define-key "fundamental" "ctrl-x d" "dired")
(register-command "dired")
