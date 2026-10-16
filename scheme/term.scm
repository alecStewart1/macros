;; requires: core

;; term.scm — embedded terminal emulator (alacritty-backed).
;;
;; `M-x term` (or `ansi-term`) opens a full VT/ANSI terminal running your login
;; shell in a read-only `term-mode` buffer. While that buffer is focused, almost
;; every key — including C-c, C-r, arrows, Tab — is sent straight to the shell;
;; the one exception is the editor prefix `C-x`, so window/buffer commands still
;; work (`C-x o` to switch panes, `C-x k` to kill the terminal, etc.). The mouse
;; wheel scrolls the terminal's scrollback.

;; Distinct buffer names so multiple terminals can run at once. `make-terminal`
;; reuses an existing terminal buffer if the name is already taken.
(define *term-count* 0)

(define (term-open program)
  (set! *term-count* (+ *term-count* 1))
  (let ((name (if (= *term-count* 1)
                  "*terminal*"
                  (string-append "*terminal*<" (number->string *term-count*) ">"))))
    ;; Launch in the SELECTED project (title-bar perspective), not whatever
    ;; buffer is current, so switching projects repoints new terminals too.
    (make-terminal name program (list) (project-directory))
    (message (string-append "Terminal " name " — keys go to the shell; C-x is the editor prefix"))))

;; The login shell ("" = the backend picks $SHELL / the default shell).
(define (term) (term-open ""))
(define (ansi-term) (term-open ""))

;; Open a terminal running a specific program (M-x term-run).
(define (term-run) (read-string "Run program: " 'term-open))

(register-command 'term)
(register-command 'ansi-term)
(register-command 'term-run)

;; When the shell exits (`exit`, C-d), kill its buffer rather than leave a dead
;; terminal behind. Opt out in init.scm with
;; (remove-hook "terminal-exit-hook" "term-kill-on-exit").
(define (term-kill-on-exit name) (kill-buffer-named name))
(add-hook "terminal-exit-hook" "term-kill-on-exit")

;; A convenient default binding (Emacs uses no global key for term; pick the
;; project leader prefix). Users can rebind in init.scm.
(define-key "fundamental" "ctrl-c ctrl-t" 'term)
