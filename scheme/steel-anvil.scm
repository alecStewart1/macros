;; steel-anvil.scm — ANVIL: an interactive Steel REPL, the SLIME-style way.
;;
;; ANVIL evaluates Steel in a SECOND, fully independent interpreter (see
;; src/anvil.rs), separate from the editor's config engine. Forms you run here
;; live in their own image: `define`s don't leak into the running editor, the
;; image can be restarted from scratch, and a runaway loop is *interruptible*
;; (C-c C-c) — Steel checks an interrupt flag on every opcode, so even
;; `(define (spin) (spin))` unwinds instead of freezing.
;;
;; Two ways to drive it:
;;   • `M-x anvil-repl` opens *anvil-repl*, a scratch buffer where RET evaluates
;;     the current line and the result is appended below.
;;   • From a Steel source buffer, the SLIME-style commands evaluate / introspect
;;     the live image. `(display …)` output is captured into the result, not the
;;     process stdout. Bindings (in `steel` and `macros-steel` modes):
;;       C-c C-z  pop to the REPL          C-c C-n  eval region
;;       C-M-x    eval top-level form      C-c C-l  load this file
;;       C-c C-d  doc for symbol at point  C-c C-x  macroexpand region/form
;;       C-c C-a  apropos the live image
;;
;; (Named after the anvil you hammer Steel into shape on.)

(define anvil-repl-buffer "*anvil-repl*")

;; Open (or switch to) the REPL scratch buffer.
(define (anvil-repl)
  (append-to-buffer anvil-repl-buffer
    (string-append
     ";; ANVIL — independent Steel REPL.  RET evaluates the current line.\n"
     ";; C-c C-c interrupt a runaway eval · C-c C-k restart the image.\n\n"))
  (set-major-mode "anvil-repl-mode"))

;; Render a result line into the REPL buffer. Errors keep their prefix; values
;; are shown as `;; => …`.
(define (anvil-on-result text)
  (append-to-buffer anvil-repl-buffer
    (cond
     ((equal? text "") ";; => (no value)\n")
     ((starts-with? text "error: ") (string-append ";; " text "\n"))
     (else (string-append ";; => " text "\n")))))

;; RET in the REPL: send the current line to the ANVIL image. Mirrors the shell
;; buffer — the typed line is the form; the result streams back to
;; `anvil-on-result`, which appends it (and leaves the cursor at the end, ready
;; for the next form).
(define (anvil-send-input)
  (let* ((b (line-bounds))
         (form (buffer-substring (car b) (cadr b))))
    (insert "\n")
    (when (not (equal? (trim-end form) ""))
      (anvil-eval form "anvil-on-result"))))

;; Interrupt a runaway evaluation (SLIME C-c C-c).
(define (anvil-repl-interrupt)
  (anvil-interrupt)
  (message "anvil: interrupt sent"))

;; Throw away the image and start fresh (SLIME restart-inferior-lisp).
(define (anvil-repl-reset)
  (anvil-reset)
  (append-to-buffer anvil-repl-buffer ";; --- image restarted ---\n"))

(define-key "anvil-repl-mode" "enter" "anvil-send-input")
(define-key "anvil-repl-mode" "ctrl-c ctrl-c" "anvil-repl-interrupt")
(define-key "anvil-repl-mode" "ctrl-c ctrl-k" "anvil-repl-reset")

;; --- evaluate from a Steel source buffer into the image ---------------------
;; Echo the result in the minibuffer without stealing focus to the REPL buffer.
(define (anvil-on-result-echo text)
  (message
   (cond
    ((equal? text "") "anvil => (no value)")
    ((starts-with? text "error: ") (string-append "anvil: " text))
    (else (string-append "anvil => " text)))))

(define (anvil-eval-region)
  (let ((form (region-text)))
    (if (equal? form "")
        (message "anvil: no active region")
        (anvil-eval form "anvil-on-result-echo"))))

(define (anvil-eval-buffer)
  (anvil-eval (buffer-string) "anvil-on-result-echo"))

;; --- SLIME-style image interaction -----------------------------------------
;; anvil-eval-defun / anvil-load-file / anvil-doc / anvil-macroexpand are Rust
;; bridges (the editor computes the form / symbol / file path from the buffer,
;; then talks to the live image). Their results stream back here.

;; Render a multi-line query result (doc, macroexpand, apropos) into a shared
;; *anvil* buffer and pop to it — these are too long for the minibuffer.
(define (anvil-on-describe text)
  (render-buffer "*anvil*" "fundamental"
                 (if (equal? text "") ";; (no result)" text)
                 '()))

;; apropos needs a pattern: prompt, then the Rust bridge queries the image.
(define (anvil-apropos-prompt)
  (read-string "Anvil apropos: " "anvil-apropos"))

;; Available from both the plain Steel mode and the editor-aware macros-steel
;; mode. C-c C-z pops to the REPL; C-c C-n evals the region into the image.
(define-key "steel" "ctrl-c ctrl-z" "anvil-repl")
(define-key "steel" "ctrl-c ctrl-n" "anvil-eval-region")
(define-key "macros-steel" "ctrl-c ctrl-z" "anvil-repl")
(define-key "macros-steel" "ctrl-c ctrl-n" "anvil-eval-region")

;; SLIME-ish image commands (both Steel modes): C-M-x eval-defun,
;; C-c C-l load-file, C-c C-d doc, C-c C-x macroexpand, C-c C-a apropos.
(define (anvil-bind-image mode)
  (define-key mode "ctrl-alt-x" "anvil-eval-defun")
  (define-key mode "ctrl-c ctrl-l" "anvil-load-file")
  (define-key mode "ctrl-c ctrl-d" "anvil-doc")
  (define-key mode "ctrl-c ctrl-x" "anvil-macroexpand")
  (define-key mode "ctrl-c ctrl-a" "anvil-apropos-prompt"))
(anvil-bind-image "steel")
(anvil-bind-image "macros-steel")

(register-command "anvil-repl")
(register-command "anvil-eval-region")
(register-command "anvil-eval-buffer")
(register-command "anvil-repl-interrupt")
(register-command "anvil-repl-reset")
(register-command "anvil-eval-defun")
(register-command "anvil-load-file")
(register-command "anvil-doc")
(register-command "anvil-macroexpand")
(register-command "anvil-apropos-prompt")
