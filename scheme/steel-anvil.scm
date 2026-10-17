;; requires: core

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
;;   • `M-x anvil-repl` opens *anvil-repl*, where RET evaluates the input once
;;     it is a complete form (multi-line forms just keep going) and the result
;;     is written above the input as `;;` comments. M-p / M-n recall input.
;;   • From a Steel source buffer, the SLIME-style commands evaluate / introspect
;;     the live image. `(display …)` output is captured into the result, not the
;;     process stdout. Bindings (in `steel` and `macros-steel` modes):
;;       C-c C-z  pop to the REPL          C-c C-n  eval region
;;       C-M-x    eval top-level form      C-c C-l  load this file
;;       C-c C-d  doc for symbol at point  C-c C-x  macroexpand region/form
;;       C-c C-a  apropos the live image   C-c C-s  signature at point
;;     (In macros-steel, C-c C-d stays the editor-API `macros-eldoc`.)
;;
;; (Named after the anvil you hammer Steel into shape on.)

;; Open (or switch to) the REPL buffer. The buffer's input region, RET, result
;; rendering and history live in Rust (editor/anvil_repl.rs): RET submits once
;; the input reads as complete forms (an unfinished form gets an indented
;; continuation line instead), results land as `;;` comment lines just above
;; the input — even while you type the next form — and RET on an earlier form in
;; the transcript copies it down to the input. Evaluating with the callback
;; "anvil-on-result" renders into the transcript the same way.
(define (anvil-repl) (anvil-repl-open))

;; Interrupt a runaway evaluation (SLIME C-c C-c).
(define (anvil-repl-interrupt)
  (anvil-interrupt)
  (message "anvil: interrupt sent"))

;; Throw away the image and start fresh (SLIME restart-inferior-lisp). Works
;; even when an evaluation is stuck in a blocking call no interrupt can reach.
(define (anvil-repl-reset)
  (anvil-reset)
  (message "anvil: image restarted"))

(define-key "anvil-repl-mode" "enter" 'anvil-send-input)
(define-key "anvil-repl-mode" "ctrl-c ctrl-c" 'anvil-repl-interrupt)
(define-key "anvil-repl-mode" "ctrl-c ctrl-k" 'anvil-repl-reset)
(define-key "anvil-repl-mode" "alt-p" 'anvil-history-previous)
(define-key "anvil-repl-mode" "alt-n" 'anvil-history-next)

;; --- eldoc: live-image signatures in the echo area --------------------------
;; While you type in the REPL, the editor looks up the operator of the form
;; being typed (`(string-append |`) in the live image and echoes its one-line
;; signature here — the REPL's answer to LSP signature-help, aware of
;; everything you `define`d this session. (anvil-signature is the on-demand
;; variant for the symbol at point.)
(define (anvil-on-eldoc text) (message text))

;; On-demand variants too: C-c C-s for the one-line signature, C-c C-d for the
;; full image docstring (popped to *anvil*).
(define-key "anvil-repl-mode" "ctrl-c ctrl-s" 'anvil-signature)
(define-key "anvil-repl-mode" "ctrl-c ctrl-d" 'anvil-doc)

;; --- evaluate from a Steel source buffer into the image ---------------------
;; Echo the result in the minibuffer without stealing focus to the REPL buffer.
;; An error arrives as "error: …" followed by any output it printed first.
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
  (read-string "Anvil apropos: " 'anvil-apropos))

;; Available from both the plain Steel mode and the editor-aware macros-steel
;; mode. C-c C-z pops to the REPL; C-c C-n evals the region into the image.
(define-key "steel" "ctrl-c ctrl-z" 'anvil-repl)
(define-key "steel" "ctrl-c ctrl-n" 'anvil-eval-region)
(define-key "macros-steel" "ctrl-c ctrl-z" 'anvil-repl)
(define-key "macros-steel" "ctrl-c ctrl-n" 'anvil-eval-region)

;; SLIME-ish image commands (both Steel modes): C-c C-l load-file,
;; C-c C-x macroexpand, C-c C-a apropos, C-c C-s signature.
(define (anvil-bind-image mode)
  (define-key mode "ctrl-c ctrl-l" 'anvil-load-file)
  (define-key mode "ctrl-c ctrl-x" 'anvil-macroexpand)
  (define-key mode "ctrl-c ctrl-a" 'anvil-apropos-prompt)
  (define-key mode "ctrl-c ctrl-s" 'anvil-signature))
(anvil-bind-image "steel")
(anvil-bind-image "macros-steel")
;; C-c C-d is image documentation in plain Steel buffers. In macros-steel (the
;; editor's own config code) it stays `macros-eldoc`, the editor-API docs,
;; which is what that code calls; M-x anvil-doc still asks the image.
(define-key "steel" "ctrl-c ctrl-d" 'anvil-doc)
;; C-M-x evaluates the form at point in the image from plain Steel buffers; in
;; macros-steel it is the live `eval-defun` (consistent with that mode's C-x C-e).
(define-key "steel" "ctrl-alt-x" 'anvil-eval-defun)

(register-command 'anvil-repl)
(register-command 'anvil-eval-region)
(register-command 'anvil-eval-buffer)
(register-command 'anvil-repl-interrupt)
(register-command 'anvil-repl-reset)
(register-command 'anvil-history-previous)
(register-command 'anvil-history-next)
(register-command 'anvil-eval-defun)
(register-command 'anvil-eval-last-sexp)
(register-command 'anvil-load-file)
(register-command 'anvil-doc)
(register-command 'anvil-signature)
(register-command 'anvil-macroexpand)
(register-command 'anvil-apropos-prompt)
