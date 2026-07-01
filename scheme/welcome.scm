;; welcome.scm — the startup splash. A read-only special buffer (welcome-mode)
;; shown on launch instead of *scratch*: a Macros ASCII banner over a
;; getting-started menu. Each menu line carries an action via per-line metadata;
;; RET or a left click runs it (the dired-sidebar/tabulated pattern):
;;
;;   * "Open the tutorial"  -> help-with-tutorial (the same as C-h t)
;;   * "Find a file"        -> helm-find-files
;;   * a recent-project row -> open a default file in that project
;;
;; Recent projects come from projectile's persisted ring (*projectile-recent*,
;; written to ~/.config/macros/projects and reloaded at startup), so the list is
;; populated the moment the app opens — even before any file is visited.
;;
;; Opening a project's default file is "just enough to register projectile":
;; find-file-hook runs projectile-update-modeline, which resolves and records the
;; project root. The default file is the first of *welcome-default-files* that
;; exists (README first), falling back to any file in the project so projectile
;; still registers when there is no README.

;; --- configuration ----------------------------------------------------------
;; Candidate "default" files to open when a project row is clicked, in order.
;; README first; later entries are the "something else" fallback so an opened
;; file always exists to register the project with projectile.
(define *welcome-default-files*
  (list "README.md" "README.org" "README.rst" "README.txt" "README"
        "Cargo.toml" "package.json" "go.mod" "Makefile" "main.rs" "index.js"))

;; How many recent projects to list.
(define *welcome-recent-count* 5)

;; Colors come from faces the active theme already defines, so the splash tracks
;; the current theme (and any `load-theme` switch) instead of hardcoding hex —
;; the same approach dired-sidebar uses. Hardcoded colors looked wrong on light
;; themes (e.g. near-white action rows vanished on a light background):
;;   ASCII logo     -> "keyword"    (the theme's primary accent)
;;   section titles -> "function"   (a second accent, distinct from the logo)
;;   action rows    -> "foreground" (normal text — always legible)
;;   paths / hints  -> "comment"    (the theme's muted color)
;; The renderer falls back to the foreground if a theme omits one of these.

;; The Macros wordmark (figlet "standard"). Literal backslashes are doubled for
;; the Steel string reader.
(define *welcome-banner*
  (list
    "    __  __                          "
    "   |  \\/  | __ _  ___ _ __ ___  ___ "
    "   | |\\/| |/ _` |/ __| '__/ _ \\/ __|"
    "   | |  | | (_| | (__| | | (_) \\__ \\"
    "   |_|  |_|\\__,_|\\___|_|  \\___/|___/"))

;; --- per-render state -------------------------------------------------------
(define *welcome-lines*   '())   ; rendered text, reversed
(define *welcome-faces*   '())   ; (line face), reversed
(define *welcome-actions* '())   ; (line . thunk), reversed
(define *welcome-n* 0)

;; Record an action thunk for `line`, active over byte-columns [lo,hi); lo/hi #f
;; means the whole line. (Columns are byte offsets from the line start, matching
;; `point` / `line-bounds`, so a click or RET on a multi-byte line still lands on
;; the right segment.) Newest-first, like the other welcome state.
(define (welcome-add-action line lo hi action)
  (set! *welcome-actions* (cons (list line lo hi action) *welcome-actions*)))

;; Emit one line, optionally tagging it with a face and/or a whole-line action.
(define (welcome-emit text face action)
  (set! *welcome-lines* (cons text *welcome-lines*))
  (when face
    (set! *welcome-faces* (cons (list *welcome-n* face) *welcome-faces*)))
  (when action
    (welcome-add-action *welcome-n* #f #f action))
  (set! *welcome-n* (+ *welcome-n* 1)))

;; Emit a line with several independently clickable targets. `segments` is a list
;; of (lo hi thunk): byte-column ranges relative to the line start (or #f #f for a
;; whole-line fallback) and the action to run when RET/click lands in that range.
;; A matching segment wins over the whole-line fallback in `welcome-action-at`.
(define (welcome-emit-segmented text face segments)
  (set! *welcome-lines* (cons text *welcome-lines*))
  (when face
    (set! *welcome-faces* (cons (list *welcome-n* face) *welcome-faces*)))
  (for-each
    (lambda (seg) (welcome-add-action *welcome-n* (car seg) (cadr seg) (caddr seg)))
    segments)
  (set! *welcome-n* (+ *welcome-n* 1)))

;; --- recent projects --------------------------------------------------------
;; The first `n` of projectile's persisted recent-projects ring, newest-first,
;; skipping any root that has since been deleted from disk.
(define (welcome-recent-projects n)
  (helm-take (filter (lambda (root) (file-exists? root)) *projectile-recent*) n))

;; --- default-file resolution ------------------------------------------------
;; First of `names` that exists under `root`, as an absolute path, or #f.
(define (welcome-first-existing root names)
  (cond ((null? names) #f)
        ((file-exists? (path-join root (car names))) (path-join root (car names)))
        (else (welcome-first-existing root (cdr names)))))

;; First regular file at the top level of `root` (skip subdirectories), or #f.
(define (welcome-first-file root)
  (let loop ((entries (list-directory root)))
    (cond ((null? entries) #f)
          ((ends-with? (car entries) "/") (loop (cdr entries)))
          (else (car entries)))))

;; The file to open for a project: a configured default (README first), else any
;; top-level file, else any file the project walker finds (deep fallback). #f for
;; a genuinely empty project.
(define (welcome-project-file root)
  (or (welcome-first-existing root *welcome-default-files*)
      (welcome-first-file root)
      (let ((fs (project-files root)))
        (if (null? fs) #f (car fs)))))

(define (welcome-open-project root)
  (let ((file (welcome-project-file root)))
    (if file
        (open-file file)          ; find-file-hook -> projectile registers the root
        (message (string-append "No file to open in " (basename root))))))

;; --- rendering --------------------------------------------------------------
;; Pad `s` with spaces to at least `width` columns (core's pad-right).
(define (welcome-pad s width) (pad-right s width))

;; Replace a leading $HOME with "~" for compact paths.
(define (welcome-abbrev path)
  (let ((home (getenv "HOME")))
    (if (and home (not (equal? home "")) (starts-with? path home))
        (string-append "~" (substring path (string-length home) (string-length path)))
        path)))

(define (welcome-render)
  (set! *welcome-lines*   '())
  (set! *welcome-faces*   '())
  (set! *welcome-actions* '())
  (set! *welcome-n* 0)

  (welcome-emit "" #f #f)
  (for-each (lambda (l) (welcome-emit l "keyword" #f)) *welcome-banner*)
  (welcome-emit "" #f #f)
  (welcome-emit "  The Emacs you know, with a modern engine" "comment" #f)
  (welcome-emit "" #f #f)
  (welcome-emit "" #f #f)

  (welcome-emit "   Getting started" "function" #f)
  (welcome-emit "" #f #f)
  ;; Two clickable targets on one line. RET (or a click anywhere else on the row)
  ;; opens the tutorial for your *current* keybindings via the whole-line
  ;; fallback; clicking a specific label opens that one and switches to its mode.
  (let* ((prefix "     ‣  Open the tutorial            ")
         (emacs-label "[emacs]")
         (gap "  ")
         (vim-label "[vim]")
         (helix-label "[helix]")
         (emacs-lo (utf8-length prefix))
         (emacs-hi (+ emacs-lo (utf8-length emacs-label)))
         (vim-lo   (+ emacs-hi (utf8-length gap)))
         (vim-hi   (+ vim-lo (utf8-length vim-label)))
         (helix-lo (+ vim-hi (utf8-length gap)))
         (helix-hi (+ helix-lo (utf8-length helix-label))))
    (welcome-emit-segmented
      (string-append prefix emacs-label gap vim-label gap helix-label "   C-h t")
      "foreground"
      (list (list emacs-lo emacs-hi (lambda () (help-with-emacs-tutorial)))
            (list vim-lo   vim-hi   (lambda () (help-with-vim-tutorial)))
            (list helix-lo helix-hi (lambda () (help-with-helix-tutorial)))
            (list #f #f (lambda () (help-with-tutorial))))))
  (welcome-emit "     ‣  Find a file                  C-x C-f"
                "foreground" (lambda () (helm-find-files)))
  (welcome-emit "     ‣  Switch project               C-c p p"
                "foreground" (lambda () (projectile-switch-project)))
  (welcome-emit "" #f #f)
  (welcome-emit "" #f #f)

  (welcome-emit "   Recent projects" "function" #f)
  (welcome-emit "" #f #f)
  (let ((projects (welcome-recent-projects *welcome-recent-count*)))
    (if (null? projects)
        (welcome-emit "     (none yet — open a file in a project to populate this)"
                      "comment" #f)
        (for-each
          (lambda (root)
            (welcome-emit
              (string-append "     ‣  " (welcome-pad (basename root) 22)
                             (welcome-abbrev root))
              "foreground"
              (lambda () (welcome-open-project root))))
          projects)))
  (welcome-emit "" #f #f)
  (welcome-emit "" #f #f)
  (welcome-emit "   RET or click to open  ·  q for scratch  ·  M-x for commands"
                "comment" #f)

  (render-buffer "*welcome*" "welcome-mode"
                 (join-lines (reverse *welcome-lines*))
                 (reverse *welcome-faces*)))

;; --- interaction ------------------------------------------------------------
;; The action thunk for (line, col): the first segment whose [lo,hi) byte range
;; contains col wins; otherwise a whole-line action (lo/hi #f) for that line, if
;; any; otherwise #f. A precise label hit (mouse, or RET with point on it) thus
;; beats the row's fallback regardless of record order.
(define (welcome-action-at line col)
  (let loop ((as *welcome-actions*) (whole #f))
    (if (null? as)
        whole
        (let* ((rec (car as)) (rl (car rec)) (lo (cadr rec)) (hi (caddr rec))
               (thunk (cadddr rec)))
          (cond ((not (= rl line)) (loop (cdr as) whole))
                ((and lo hi)
                 (if (and (>= col lo) (< col hi)) thunk (loop (cdr as) whole)))
                (else (loop (cdr as) thunk)))))))

;; RET / left click: run the action under point on the current line, if any.
;; `col` is the byte offset of point within the line (point minus line start),
;; matching the byte-column ranges stored by `welcome-emit-segmented`. Clicks on
;; the banner or blank lines do nothing.
(define (welcome-visit)
  (let* ((line (buffer-line))
         (col  (- (point) (car (line-bounds))))
         (act  (welcome-action-at line col)))
    (when act (act))))

;; Show (or refresh) the welcome buffer and switch to it.
(define (welcome) (welcome-render))
(define (welcome-refresh) (welcome-render))

;; q dismisses the splash to the scratch buffer.
(define (welcome-dismiss)
  (kill-buffer)
  (switch-to-buffer "*scratch*"))

(define-list-mode-keys "welcome-mode" 'welcome-visit)
(define-key "welcome-mode" "mouse-1" 'welcome-visit)
(define-key "welcome-mode" "g"       'welcome-refresh)
(define-key "welcome-mode" "q"       'welcome-dismiss)  ; splash q ≠ kill-buffer

;; Under evil-mode, the collection layer (evil.scm) gives this read-only buffer
;; only j/k. Add horizontal motion so the cursor can reach a specific target on a
;; multi-target row — e.g. the [emacs] vs [evil/vim] labels on the tutorial line,
;; which RET then opens. Inert under plain Emacs (this keymap is consulted only
;; when evil is active); `g`/`G` are deliberately left to welcome-refresh / vim.
(define-key "evil-normal:welcome-mode" "h" 'backward-char)
(define-key "evil-normal:welcome-mode" "l" 'forward-char)
(define-key "evil-normal:welcome-mode" "w" 'forward-word-start)
(define-key "evil-normal:welcome-mode" "b" 'backward-word)
(define-key "evil-normal:welcome-mode" "0" 'move-beginning-of-line)
(define-key "evil-normal:welcome-mode" "$" 'move-end-of-line)

;; Same horizontal motion under helix-mode (its collection layer also gives this
;; buffer only j/k), so a Helix user can reach the [helix] label and press RET.
(define-key "helix-normal:welcome-mode" "h" 'backward-char)
(define-key "helix-normal:welcome-mode" "l" 'forward-char)
(define-key "helix-normal:welcome-mode" "w" 'forward-word-start)
(define-key "helix-normal:welcome-mode" "b" 'backward-word)
(define-key "helix-normal:welcome-mode" "0" 'move-beginning-of-line)
(define-key "helix-normal:welcome-mode" "$" 'move-end-of-line)

(register-command 'welcome)

;; Show it on launch, replacing the initial *scratch* view. To disable, add
;; `(switch-to-buffer "*scratch*")` to your ~/.config/macros/init.scm (loaded
;; after this file).
(welcome)
