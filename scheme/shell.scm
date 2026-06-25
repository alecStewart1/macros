;; shell.scm — process integration: an Emacs-style `compile` (run a command into
;; a read-only *compilation* buffer, parse errors, walk them with next-error /
;; previous-error and jump to the source), grep navigation, and a basic shell.

;; --- compile (M-x compile / C-c c) ------------------------------------------
;; Error faces (whole-line, applied on the final faced re-render in compile-done).
(set-face "compilation-error"   "#f92672")  ; red — errors
(set-face "compilation-warning" "#e6db74")  ; yellow — warnings
(set-face "compilation-info"    "#a6e22e")  ; green — "finished" status

(define *compile-dir* "")     ; cwd the command ran in (relative paths resolve here)
(define *compile-last* "")     ; last command, for the prompt default + recompile
(define *compile-out* '())     ; reversed list of raw output lines (for faced re-render)
(define *compile-errors* '())  ; reversed list of (comp-line path src-line src-col face)
(define *compile-line* 2)      ; *compilation* line the next output line lands on
(define *compile-idx* -1)      ; cursor into the (forward) error list for nav

;; Parse one output line into (path src-line src-col) or #f. Tries the common
;; compiler/grep formats; first match wins (Emacs's compilation-error-regexp-alist).
(define (compile-match line)
  (define (cap pat path-i line-i col-i)
    (let ((m (regexp-captures pat line)))
      (if (and (not (null? m)) (not (equal? (nth m path-i) "")))
          (list (nth m path-i)
                (string->number (nth m line-i))
                (let ((c (if (>= col-i (length m)) "" (nth m col-i))))
                  (if (or (equal? c "") (not (string->number c))) 1 (string->number c))))
          #f)))
  (or
   ;; rust / cargo: `  --> src/main.rs:10:5`
   (cap "-->[ \t]+([^: \t]+):([0-9]+):([0-9]+)" 1 2 3)
   ;; python traceback: `  File "foo.py", line 42`
   (cap "File \"([^\"]+)\", line ([0-9]+)" 1 2 99)
   ;; gcc / clang / grep -n: `path:line[:col]:` at the start of the line
   (cap "^([^ \t:][^:]*):([0-9]+):(?:([0-9]+):)?" 1 2 3)))

;; Resolve a (possibly relative) error path against the command's cwd.
(define (compile-resolve file)
  (if (starts-with? file "/") file (path-join *compile-dir* file)))

(define (compile) (read-string "Compile command: " "compile-run" *compile-last*))
(define (compile-run cmd)
  (when (not (equal? (trim-end cmd) ""))
    (set! *compile-dir* (buffer-directory))
    (set! *compile-last* cmd)
    (set! *compile-out* '())
    (set! *compile-errors* '())
    (set! *compile-line* 2)
    (set! *compile-idx* -1)
    (set! *next-error-source* "compile")
    (render-buffer "*compilation*" "compile-mode" (string-append "$ " cmd "\n\n") '())
    ;; `2>&1`: make-process captures stdout only, but compilers report on stderr.
    (make-process "/bin/sh" (list "-c" (string-append "cd " *compile-dir* " && " cmd " 2>&1"))
                  "compile-output" "compile-done")))

;; Stream each line live; record any error with the *compilation* line it sits on.
(define (compile-output line)
  (append-to-buffer "*compilation*" (string-append line "\n"))
  (set! *compile-out* (cons line *compile-out*))
  (let ((m (compile-match line)))
    (when m
      (let ((face (if (string-contains? line "warning") "compilation-warning" "compilation-error")))
        (set! *compile-errors*
              (cons (cons *compile-line* (append m (list face))) *compile-errors*)))))
  (set! *compile-line* (+ *compile-line* 1)))

;; On exit, re-render the whole buffer with error lines faced + a status footer.
(define (compile-done code)
  (let* ((errs (reverse *compile-errors*))
         (ok (equal? code "0"))
         (status (if ok "Compilation finished"
                     (string-append "Compilation exited abnormally with code " code)))
         (lines (append (list (string-append "$ " *compile-last*) "")
                        (reverse *compile-out*)
                        (list "" status)))
         (status-line (- (length lines) 1))
         (faces (cons (list status-line (if ok "compilation-info" "compilation-error"))
                      (map (lambda (e) (list (car e) (nth e 4))) errs))))
    (render-buffer "*compilation*" "compile-mode" (join-lines lines) faces)
    (message (string-append status " — " (int->string (length errs)) " error(s)"))))

(define (recompile) (when (not (equal? *compile-last* "")) (compile-run *compile-last*)))

;; --- error navigation (the compile arm of next-error / previous-error) ------
(define (compile-error-list) (reverse *compile-errors*))
(define (compile-show idx)
  (let ((errs (compile-error-list)))
    (cond
      ((null? errs) (message "No errors"))
      ((< idx 0) (message "Before first error"))
      ((>= idx (length errs)) (message "No more errors"))
      (else
       (set! *compile-idx* idx)
       (let* ((e (nth errs idx))
              (path (compile-resolve (nth e 1)))
              (ln (nth e 2)))
         (if (file-exists? path)
             (begin (open-file-other-window path)
                    (goto-line (max 0 (- ln 1)))
                    (message (string-append "Error " (int->string (+ idx 1)) "/"
                                            (int->string (length errs)) ": "
                                            (nth e 1) ":" (int->string ln))))
             (message (string-append "File not found: " path))))))))
(define (compile-next-error) (compile-show (+ *compile-idx* 1)))
(define (compile-previous-error) (compile-show (- *compile-idx* 1)))
(define (compile-first-error) (compile-show 0))

;; Unified next-error / previous-error: route to compile or diagnostics by which
;; source was used last (Emacs's single next-error command). *next-error-source*
;; and diag-next-error/diag-previous-error come from diagnostics.scm (loads first).
(define (next-error)
  (if (equal? *next-error-source* "compile") (compile-next-error) (diag-next-error)))
(define (previous-error)
  (if (equal? *next-error-source* "compile") (compile-previous-error) (diag-previous-error)))

;; RET in a result buffer: parse the current line and visit it in another window.
;; Shared by compile-mode and grep-mode.
(define (compile-visit)
  (let* ((b (line-bounds))
         (line (buffer-substring (car b) (cadr b)))
         (m (compile-match line)))
    (if m
        (let ((path (compile-resolve (nth m 0)))
              (ln (nth m 1)))
          (if (file-exists? path)
              (begin (open-file-other-window path) (goto-line (max 0 (- ln 1))))
              (message (string-append "No such file: " path))))
        (message "No error on this line"))))

(define-key "compile-mode" "enter" "compile-visit")
(define-key "compile-mode" "g" "recompile")
(define-key "compile-mode" "q" "kill-buffer")
(define-key "compile-mode" "n" "next-line")
(define-key "compile-mode" "p" "previous-line")
;; grep-mode buffers (projectile-grep / occur-style) get the same RET jump.
(define-key "grep-mode" "enter" "compile-visit")

(define-key "fundamental" "ctrl-c c" "compile")
(register-command "compile")
(register-command "recompile")
(register-command "compile-first-error")

;; --- a basic interactive shell (*shell*) ------------------------------------
;; A normal (editable) buffer running /bin/sh on its stdin: type a command, RET
;; sends the current line; stdout streams back. No prompt/tty/job-control — a
;; simple command runner, not a terminal emulator.
(define *shell-proc* -1)
(define (shell)
  (append-to-buffer "*shell*" "")          ; create/switch to a normal buffer
  (set-major-mode "shell-mode")            ; so RET is rebound below
  (set! *shell-proc* (make-process "/bin/sh" (list) "shell-output" "shell-exit")))
(define (shell-output line) (append-to-buffer "*shell*" (string-append line "\n")))
(define (shell-exit code) (append-to-buffer "*shell*" (string-append "[exited " code "]\n")))
(define (shell-send-input)
  (let* ((b (line-bounds)) (line (buffer-substring (car b) (cadr b))))
    (insert "\n")
    (process-send-string *shell-proc* (string-append line "\n"))))

(define-key "shell-mode" "enter" "shell-send-input")
(define-key "fundamental" "ctrl-c s" "shell")
(register-command "shell")
