;; requires: core dap lib-section lib-transient modeline

;; ui-dap.scm — the debugger's UI, on top of the dap.scm protocol core: the
;; *dap* panel (a lib-section buffer with threads / stack / scopes / watches /
;; breakpoints), the sticky debug transient (C-x C-a — the dap-hydra), the panel
;; keymap, and the mode-line segment. dap.scm doesn't require this file (or
;; lib-section / lib-transient), so it knows nothing about this UI — it calls
;; whatever was registered via `dap-on-change`, which is how the panel stays live.

(define *dap-panel-name* "*dap*")

;; --- variables tree state -----------------------------------------------------
;; A variable with children carries a variablesReference; expanding one fetches
;; its children once (per stop) and remembers the open set across re-renders.
(define *dap-var-open* '())      ; refs the user expanded
(define *dap-var-children* '())  ; (ref . vars) fetched children

;; --- navigation tables (rebuilt with every render) ------------------------------
(define *dap-ui--frame-lines* '())   ; (line . frame-idx)
(define *dap-ui--thread-lines* '())  ; (line . thread-id)
(define *dap-ui--var-lines* '())     ; (line . varref) — expandable vars only
(define *dap-ui--bp-lines* '())      ; (line path bp-line)

;; --- building the panel -----------------------------------------------------------
(define (dap-ui--state-label)
  (cond ((eq? *dap-state* 'stopped)
         (string-append "stopped"
                        (if (equal? *dap-stopped-reason* "") ""
                            (string-append ": " *dap-stopped-reason*))))
        ((eq? *dap-state* 'running)  "running")
        ((eq? *dap-state* 'starting) "starting…")
        (else "no session")))

(define (dap-ui--frame-label f)
  (let* ((src (dap--get f 'source))
         (file (basename (dap--str src 'path)))
         (line (dap--int (dap--get f 'line)))
         (name (dap--str f 'name)))
    (string-append (pad-right name 24) " " file
                   (if (> line 0) (string-append ":" (int->string line)) ""))))

(define (dap-ui--emit-var v depth)
  (let* ((name (dap--str v 'name))
         (val (dap--truncate (dap--str v 'value) 60))
         (ref (dap--int (dap--get v 'variablesReference)))
         (open (member ref *dap-var-open*))
         (marker (cond ((= ref 0) "  ") (open "v ") (else "> "))))
    (when (> ref 0)
      (set! *dap-ui--var-lines*
            (cons (cons (section-point-line) ref) *dap-ui--var-lines*)))
    (section-line (string-append "  " (string-repeat "  " depth) marker
                                 name " = " val))
    (when (and open (assoc ref *dap-var-children*))
      (for-each (lambda (child) (dap-ui--emit-var child (+ depth 1)))
                (cdr (assoc ref *dap-var-children*))))))

(define (dap-ui--build)
  (section-begin)
  (set! *dap-ui--frame-lines* '())
  (set! *dap-ui--thread-lines* '())
  (set! *dap-ui--var-lines* '())
  (set! *dap-ui--bp-lines* '())
  (section-line
    (string-append "Debug"
                   (if *dap-adapter* (string-append ": " (car *dap-adapter*)) "")
                   " — " (dap-ui--state-label))
    "dap-heading")
  (section-line "")

  ;; Threads — RET selects one (refetches its stack).
  (when (not (null? *dap-threads*))
    (section-open 'dap-threads
                  (string-append "Threads (" (int->string (length *dap-threads*)) ")")
                  "dap-heading")
    (for-each
      (lambda (t)
        (let ((tid (dap--int (dap--get t 'id))))
          (set! *dap-ui--thread-lines*
                (cons (cons (section-point-line) tid) *dap-ui--thread-lines*))
          (section-line (string-append (if (= tid *dap-thread*) "  ▶ " "    ")
                                       (int->string tid) " " (dap--str t 'name)))))
      *dap-threads*)
    (section-close 'dap-threads))

  ;; Stack — RET selects a frame (jump + rescope).
  (when (not (null? *dap-frames*))
    (section-open 'dap-stack "Stack" "dap-heading")
    (let loop ((fs *dap-frames*) (idx 0))
      (when (not (null? fs))
        (set! *dap-ui--frame-lines*
              (cons (cons (section-point-line) idx) *dap-ui--frame-lines*))
        (section-line (string-append (if (= idx *dap-frame-idx*) "  ▶ " "    ")
                                     (dap-ui--frame-label (car fs)))
                      (if (= idx *dap-frame-idx*) #f "dap-dim"))
        (loop (cdr fs) (+ idx 1))))
    (section-close 'dap-stack))

  ;; One section per scope (Locals, Globals, …); TAB on a `>` variable expands it.
  (for-each
    (lambda (entry)
      (let ((name (nth entry 0))
            (expensive (nth entry 2))
            (vars (nth entry 3)))
        (section-open (list 'dap-scope name) name "dap-heading")
        (cond
          (expensive (section-line "  (expensive scope — not fetched)" "dap-dim"))
          ((null? vars) (section-line "  (empty)" "dap-dim"))
          (else (for-each (lambda (v) (dap-ui--emit-var v 0)) vars)))
        (section-close (list 'dap-scope name))))
    *dap-scopes*)

  ;; Watches.
  (when (not (null? *dap-watches*))
    (section-open 'dap-watch
                  (string-append "Watch (" (int->string (length *dap-watches*)) ")")
                  "dap-heading")
    (for-each
      (lambda (expr)
        (let ((v (alist-ref *dap-watch-vals* expr)))
          (section-line (string-append "  " expr " ⇒ " (if v (dap--truncate v 60) "…")))))
      *dap-watches*)
    (section-close 'dap-watch))

  ;; Breakpoints — RET visits, x removes.
  (when (not (null? *dap-breakpoints*))
    (section-open 'dap-breakpoints
                  (string-append "Breakpoints ("
                                 (int->string (length *dap-breakpoints*)) ")")
                  "dap-heading")
    (for-each
      (lambda (bp)
        (let ((path (car bp)) (line (nth bp 1)) (condition (nth bp 2)))
          (set! *dap-ui--bp-lines*
                (cons (list (section-point-line) path line) *dap-ui--bp-lines*))
          (section-line
            (string-append "  "
                           (if (member (cons path line) *dap-bp-unverified*) "○ " "● ")
                           (basename path) ":" (int->string line)
                           (if (equal? condition "")
                               ""
                               (string-append "  if " condition)))
            (if (member (cons path line) *dap-bp-unverified*) "dap-dim" #f))))
      *dap-breakpoints*)
    (section-close 'dap-breakpoints))

  (section-line "")
  (section-line "RET visit/expand · TAB fold · c/s/i/o step · b/x breakpoints · ? menu"
                "dap-dim"))

;; --- showing / refreshing ------------------------------------------------------------
;; The panel opens in a right split (~1/3), focus staying put; refreshes are
;; quiet in-place re-renders wherever the panel already lives, with the fold /
;; accent overlays re-applied through the same-batch render redirect.
(define-command (dap-ui-show)
  (if (>= (get-buffer-window *dap-panel-name*) 0)
      (dap-ui-refresh)
      (let ((orig (selected-window)))
        (split-window-right-ratio 0.66)
        (other-window)
        (dap-ui--build)
        (section-render *dap-panel-name* "dap-ui-mode")
        (select-window orig))))

(define (dap-ui-refresh)
  (when (>= (get-buffer-window *dap-panel-name*) 0)
    (dap-ui--build)
    (render-buffer-quiet *dap-panel-name* "dap-ui-mode"
                         (section-text) (section-line-faces))
    (section-apply-overlays)))

;; The dap.scm change hook: refresh the panel; auto-open the debug layout when
;; a session starts; drop the variables-tree cache when execution resumes or
;; the session dies (its variablesReferences are stale after either).
(define *dap-ui--last-state* 'dead)
(define (dap-ui--on-change)
  (when (not (eq? *dap-state* *dap-ui--last-state*))
    (when (or (eq? *dap-state* 'running) (eq? *dap-state* 'dead))
      (set! *dap-var-open* '())
      (set! *dap-var-children* '()))
    (when (eq? *dap-state* 'starting) (dap-ui-show))
    (set! *dap-ui--last-state* *dap-state*))
  (dap-ui-refresh))
(dap-on-change dap-ui--on-change)

;; --- panel interaction ------------------------------------------------------------------
(define (dap-ui--toggle-var ref)
  (if (member ref *dap-var-open*)
      (begin
        (set! *dap-var-open* (filter (lambda (r) (not (= r ref))) *dap-var-open*))
        (dap-ui-refresh))
      (begin
        (set! *dap-var-open* (cons ref *dap-var-open*))
        (if (assoc ref *dap-var-children*)
            (dap-ui-refresh)
            (dap--fetch-variables ref
              (lambda (r vars)
                (set! *dap-var-children* (cons (cons r vars) *dap-var-children*))
                (dap-ui-refresh)))))))

;; RET: act on the line — select a frame, switch thread, expand a variable,
;; or visit a breakpoint's file.
(define (dap-ui-visit)
  (let* ((line (buffer-line))
         (frame (alist-ref *dap-ui--frame-lines* line))
         (tid (alist-ref *dap-ui--thread-lines* line))
         (ref (alist-ref *dap-ui--var-lines* line))
         (bp (let loop ((bs *dap-ui--bp-lines*))
               (cond ((null? bs) #f)
                     ((= (car (car bs)) line) (car bs))
                     (else (loop (cdr bs)))))))
    (cond
      (frame (dap--select-frame frame #t))
      (tid (dap-select-thread tid))
      (ref (dap-ui--toggle-var ref))
      (bp (dap--visit (nth bp 1) (nth bp 2) #f))
      (else (section-toggle-at-point)))))

;; TAB: expand/collapse a variable, else fold the section at point.
(define (dap-ui-tab)
  (let ((ref (alist-ref *dap-ui--var-lines* (buffer-line))))
    (if ref (dap-ui--toggle-var ref) (section-toggle-at-point))))

;; x on a breakpoint line removes it.
(define (dap-ui-remove)
  (let loop ((bs *dap-ui--bp-lines*))
    (cond ((null? bs) (message "dap: nothing to remove here"))
          ((= (car (car bs)) (buffer-line))
           (let ((path (nth (car bs) 1)) (line (nth (car bs) 2)))
             (set! *dap-breakpoints*
                   (filter (lambda (bp) (not (and (equal? (car bp) path)
                                                  (= (nth bp 1) line))))
                           *dap-breakpoints*))
             (dap--bp-mutate path)))
          (else (loop (cdr bs))))))

(define-key "dap-ui-mode" "enter" 'dap-ui-visit)
(define-key "dap-ui-mode" "tab"   'dap-ui-tab)
(define-key "dap-ui-mode" "n" 'next-line)
(define-key "dap-ui-mode" "p" 'previous-line)
(define-key "dap-ui-mode" "j" 'next-line)
(define-key "dap-ui-mode" "k" 'previous-line)
(define-key "dap-ui-mode" "g" 'dap-ui-show)
(define-key "dap-ui-mode" "c" 'dap-continue)
(define-key "dap-ui-mode" "s" 'dap-next)
(define-key "dap-ui-mode" "i" 'dap-step-in)
(define-key "dap-ui-mode" "o" 'dap-step-out)
(define-key "dap-ui-mode" "r" 'dap-restart)
(define-key "dap-ui-mode" "b" 'dap-toggle-breakpoint)
(define-key "dap-ui-mode" "x" 'dap-ui-remove)
(define-key "dap-ui-mode" "e" 'dap-eval)
(define-key "dap-ui-mode" "w" 'dap-watch-add)
(define-key "dap-ui-mode" "shift-w" 'dap-watch-clear)
(define-key "dap-ui-mode" "shift-q" 'dap-quit)
(define-key "dap-ui-mode" "q" 'kill-buffer)
(define-key "dap-ui-mode" "?" 'dap-hydra)
;; Modal states resolve movement against their own maps first — mirror the
;; essentials so the panel behaves under evil/helix too.
(define-key "evil-normal:dap-ui-mode" "enter" 'dap-ui-visit)
(define-key "evil-normal:dap-ui-mode" "tab" 'dap-ui-tab)
(define-key "helix-normal:dap-ui-mode" "enter" 'dap-ui-visit)
(define-key "helix-normal:dap-ui-mode" "tab" 'dap-ui-tab)

;; --- the debug transient (hydra) -----------------------------------------------------
;; Sticky: a stepping key runs its command and the popup stays up, so
;; n n n i c reads like dap-mode's hydra. Prompting rows (e/w/B) close into
;; the minibuffer as usual.
(define-command (dap-hydra)
  (transient-sticky "Debug"
    (list
      (list "Stepping"
            (list "n" "Next (step over)" 'dap-next)
            (list "i" "Step in"          'dap-step-in)
            (list "o" "Step out"         'dap-step-out)
            (list "c" "Continue"         'dap-continue)
            (list "P" "Pause"            'dap-pause))
      (list "Breakpoints"
            (list "b" "Toggle here"      'dap-toggle-breakpoint)
            (list "B" "Condition…"       'dap-breakpoint-condition)
            (list "X" "Clear all"        'dap-breakpoints-clear))
      (list "Session"
            (list "d" "Debug this file"  'dap-debug)
            (list "r" "Restart"          'dap-restart)
            (list "Q" "Quit session"     'dap-quit))
      (list "Inspect"
            (list "e" "Eval…"            'dap-eval)
            (list "w" "Add watch…"       'dap-watch-add)
            (list "u" "Frame up"         'dap-up-frame)
            (list "U" "Frame down"       'dap-down-frame)
            (list "p" "Show panel"       'dap-ui-show)
            (list "C" "Show console"     'dap-console)))))

;; C-x C-a is the GUD / dape debugger prefix in Emacs. (C-c d was taken: it's
;; lsp-hover's short key, lsp.scm.) Under evil it still works — unbound
;; Ctrl chords and Emacs prefixes fall through to this keymap; leader users
;; can add e.g. (evil-leader-set "d" 'dap-hydra) in init.scm.
(define-key "fundamental" "ctrl-x ctrl-a" 'dap-hydra)

;; --- mode line --------------------------------------------------------------------------
;; "⏸ breakpoint" / "▶ dbg" while a session lives (dap.scm writes the slot).
(mode-line-add-segment (ml-slot* "dap" "dap-stopped-line" "  " ""))
