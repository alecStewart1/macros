;; dired-sidebar.scm — the bundled file-tree, à la Emacs `dired-sidebar` +
;; `dired-subtree`. A real dired buffer pinned to a side window: you navigate the
;; tree AND operate on files (mark, delete, rename, copy, mkdir), and directories
;; expand/collapse inline (the dired-subtree behavior). M-x dired-sidebar / C-c t.
;;
;; Two layers in one file:
;;   1. the dired-subtree ENGINE — `dired-subtree-build` / `-paint` and friends:
;;      a pure, reusable renderer that turns a root directory, a set of OPEN
;;      subdirectories, and a set of MARKED paths into an indented, icon-decorated,
;;      expandable listing. It owns no buffer and no keymap.
;;   2. the `dired-sidebar` consumer: state, the side-window lifecycle, the
;;      dired-style file operations, and the keymap.
;;
;; They live together because Macros evaluates bundled scheme/ files in sorted
;; order and a file cannot forward-reference a later one (see also the dired /
;; dired-subtree split note in steel-load-order): a consumer must be able to see
;; the engine at eval time, so the engine and its consumer share a file.
;;
;; The flat directory editor in dired.scm is untouched — this is an additive tree
;; layer, not a change to dired-mode. The file operations mirror dired.scm (rm /
;; mv / cp / mkdir via call-process), but addressed at the entry under point in the
;; tree rather than a single flat directory.
;;
;; No new Rust support: built entirely on existing primitives (list-directory,
;; render-buffer / render-buffer-quiet, set-line-icon, overlays, window splitting,
;; call-process). Live refresh rides the same "notify" path dired uses —
;; watch-directories declares the root + every expanded subdir, and
;; directory-changed-hook re-renders when one changes on disk, in place (never
;; stealing focus from the editing pane).
;;
;; Colors come from faces the active theme already defines, so the tree tracks the
;; current theme (and any `load-theme` switch) instead of hardcoding hex:
;;   directories  -> "keyword" (the theme's primary accent)
;;   dotfiles     -> "comment" (the theme's muted color)
;;   indent rails -> "dim"     (the theme's faint UI color)
;; A whole-line face also tints that line's icon (the renderer reuses it); the
;; renderer falls back to the foreground if a theme omits one of these names.

;; ===========================================================================
;; dired-subtree engine — pure builder + painter, no state, no keymap.
;; ===========================================================================

;; Drop a trailing "/" (list-directory marks dirs with one; the editor reports
;; changed directories without one — normalize before comparing/keying).
(define (dired-subtree-strip-slash p)
  (if (ends-with? p "/") (substring p 0 (- (string-length p) 1)) p))

;; Vertical indent guides: one "│ " per ancestor level. Each is 2 columns wide so
;; the icon/name columns line up under the 3 reserved icon columns below.
(define (dired-subtree-rails depth)
  (if (<= depth 0) "" (string-append "│ " (dired-subtree-rails (- depth 1)))))

;; Whole-line face for an entry, or #f to leave it the default foreground.
;; Dotfiles dim regardless of kind; other directories take the accent.
(define (dired-subtree-face name is-dir)
  (cond ((starts-with? name ".") "comment")
        (is-dir "keyword")
        (else #f)))

;; Extension → Lucide icon (assets/icons/<name>.svg). Customize freely.
(define (dired-subtree-icon name)
  (cond
    ((or (ends-with? name ".rs") (ends-with? name ".c") (ends-with? name ".h")
         (ends-with? name ".cpp") (ends-with? name ".go") (ends-with? name ".py")
         (ends-with? name ".js") (ends-with? name ".ts") (ends-with? name ".java")
         (ends-with? name ".rb") (ends-with? name ".lua") (ends-with? name ".scm")) "file-code")
    ((ends-with? name ".json") "braces")
    ((or (ends-with? name ".md") (ends-with? name ".txt") (ends-with? name ".org")) "file-text")
    ((or (ends-with? name ".toml") (ends-with? name ".yaml") (ends-with? name ".yml")
         (ends-with? name ".ini") (ends-with? name ".conf")) "settings-2")
    ((or (ends-with? name ".png") (ends-with? name ".jpg") (ends-with? name ".jpeg")
         (ends-with? name ".gif") (ends-with? name ".svg")) "image")
    ((ends-with? name ".lock") "lock")
    (else "file")))

;; Directories first, then files — each group keeps list-directory's
;; alphabetical order (filter is stable).
(define (dired-subtree-sort entries)
  (append (filter (lambda (e) (ends-with? e "/")) entries)
          (filter (lambda (e) (not (ends-with? e "/"))) entries)))

;; Build the tree for ROOT with EXPANDED (a list of directory paths, no trailing
;; slash, currently open) and MARKS (a list of full paths, as they appear in the
;; entry map, flagged for deletion). Returns a list of five lists, top-to-bottom:
;;   0 lines   — (string …)                 rendered text rows
;;   1 entries — ((line path is-dir) …)     hit-test map for the caller's keys
;;   2 icons   — ((line byte-col icon) …)   per-row icon placements
;;   3 faces   — ((line face) …)            whole-line color overrides
;;   4 guides  — ((start-byte end-byte face) …)  indent-rail overlay accents
;; Pure: no globals, no buffer side effects — the caller paints the result with
;; dired-subtree-paint. Each row opens with a 2-column mark gutter ("D " when the
;; path is marked, "  " otherwise), so icon and guide BYTE offsets are shifted by
;; that gutter width. Icons/guides are addressed by BYTE offset (a "│" is 3 bytes),
;; so the rail prefix is measured with utf8-length, not assumed 1 byte/column.
(define dired-subtree-mark-width 2)   ; bytes in the "D "/"  " gutter (ASCII)
(define (dired-subtree-build root expanded marks)
  (let ((lines '()) (entries '()) (icons '()) (faces '())
        (guides '()) (byte 0) (n 0))
    ;; Emit one row at DEPTH: mark gutter, indent rails, 3 reserved icon columns,
    ;; then NAME.
    (define (emit name path is-dir icon depth)
      (let* ((rails (dired-subtree-rails depth))
             (gb (utf8-length rails))                 ; byte width of the rail region
             (mark (if (member path marks) "D " "  "))
             (text (string-append mark rails "   " name))  ; icon in the 3 reserved cols
             ;; Byte offset of the rails (and so the icon) past the mark gutter.
             (rail-start (+ byte dired-subtree-mark-width))
             (face (dired-subtree-face name is-dir)))
        (set! lines (cons text lines))
        (set! entries (cons (list n path is-dir) entries))
        (set! icons (cons (list n (+ dired-subtree-mark-width gb) icon) icons))
        (when face (set! faces (cons (list n face) faces)))
        (when (> gb 0)
          (set! guides (cons (list rail-start (+ rail-start gb) "dim") guides)))
        (set! n (+ n 1))
        (set! byte (+ byte (utf8-length text) 1))))   ; +1 = newline
    ;; Walk one directory level; recurse into expanded subdirectories.
    (define (walk dir depth)
      (for-each
        (lambda (full)
          (let* ((is-dir (ends-with? full "/"))
                 (clean (dired-subtree-strip-slash full))
                 (name (basename clean))
                 (open (and is-dir (member clean expanded)))
                 (icon (if is-dir (if open "folder-open" "folder")
                           (dired-subtree-icon name))))
            (emit name full is-dir icon depth)
            (when open (walk clean (+ depth 1)))))
        (dired-subtree-sort (list-directory dir))))
    (emit (basename root) root #t "folder-open" 0)
    (walk root 1)
    (list (reverse lines) (reverse entries) (reverse icons)
          (reverse faces) (reverse guides))))

;; Paint a tree BUILT by dired-subtree-build into BUFFER (major mode MODE). When
;; QUIET? is true, render in place without stealing focus from the editing pane
;; (an FS auto-refresh); otherwise switch to the buffer. clear-overlays first so a
;; refresh doesn't stack stale indent rails (the whole-line face would otherwise
;; tint the rails too, hence painting them via overlays with the "dim" face).
(define (dired-subtree-paint built buffer mode quiet?)
  (let ((lines  (nth built 0))
        (icons  (nth built 2))
        (faces  (nth built 3))
        (guides (nth built 4)))
    (if quiet?
        (render-buffer-quiet buffer mode (join-lines lines) faces)
        (render-buffer buffer mode (join-lines lines) faces))
    (for-each (lambda (ic) (set-line-icon (nth ic 0) (nth ic 1) (nth ic 2))) icons)
    (clear-overlays)
    (for-each (lambda (g) (overlay-set-face (nth g 0) (nth g 1) (nth g 2))) guides)))

;; The (path is-dir) of the ENTRIES row at LINE, or #f. ENTRIES is element 1 of a
;; dired-subtree-build result; consumers keep it from the last render to resolve
;; clicks / point.
(define (dired-subtree-entry-at entries line)
  (let loop ((es entries))
    (cond ((null? es) #f)
          ((= (car (car es)) line) (cdr (car es)))
          (else (loop (cdr es))))))

;; Build → paint → re-declare the watch set, in one call. Returns the entries
;; list (element 1) for the consumer to stash for hit-testing. Replace-per-owner:
;; this single watch-directories call re-declares root + expanded each render, so
;; collapsed dirs drop off the watch list.
(define (dired-subtree-show root expanded marks buffer mode owner quiet?)
  (let ((built (dired-subtree-build root expanded marks)))
    (dired-subtree-paint built buffer mode quiet?)
    (watch-directories owner (cons root expanded))
    (nth built 1)))

;; The root to open a tree at: the project root of the current buffer's directory,
;; else that directory; slash stripped.
(define (dired-subtree-default-root)
  (dired-subtree-strip-slash
    (or (project-root (buffer-directory)) (buffer-directory))))

;; Directories to open so PATH (a file) becomes visible under ROOT: every ancestor
;; of PATH from just inside ROOT down to PATH's own parent. Returns '() when PATH
;; isn't under ROOT (nothing to reveal). Walks up parent-by-parent, so it stops at
;; the filesystem root even when the prefix never matches.
(define (dired-subtree-ancestors root path)
  (let ((root (dired-subtree-strip-slash root)))
    (let loop ((dir (parent-directory (dired-subtree-strip-slash path))) (acc '()))
      (cond ((equal? dir root) acc)
            ((or (equal? dir "")
                 (not (starts-with? (string-append dir "/")
                                    (string-append root "/")))) '())
            (else (loop (parent-directory dir) (cons dir acc)))))))

;; Toggle PATH in EXPANDED (a list of dir paths), returning the new list.
(define (dired-subtree-toggled expanded path)
  (if (member path expanded)
      (filter (lambda (p) (not (equal? p path))) expanded)
      (cons path expanded)))

;; ===========================================================================
;; dired-sidebar — a dired tree pinned to a side window (C-c t).
;; ===========================================================================

(define *dired-sidebar-root* "")         ; tree root (no trailing /)
(define *dired-sidebar-expanded* '())    ; dir paths (no trailing /) currently open
(define *dired-sidebar-marks* '())       ; full paths (as in entries) flagged "D"
(define *dired-sidebar-entries* '())     ; (line path is-dir) from the last render
(define *dired-sidebar-window* #f)       ; the pane id hosting the sidebar
(define *dired-sidebar-quiet* #f)        ; #t during an FS auto-refresh
;; Operands captured BEFORE a minibuffer prompt — the read-string callback runs
;; with the minibuffer current, so point is no longer in the tree; we can't read
;; the entry under point there.
(define *dired-sidebar-src* "")          ; rename/copy source path
(define *dired-sidebar-target-dir* "")   ; mkdir parent directory

(define (dired-sidebar-render)
  (set! *dired-sidebar-entries*
        (dired-subtree-show *dired-sidebar-root* *dired-sidebar-expanded*
                            *dired-sidebar-marks*
                            "*dired-sidebar*" "dired-sidebar-mode" "dired-sidebar"
                            *dired-sidebar-quiet*)))

;; Is the sidebar already on screen? Its buffer must still exist and its pane must
;; still be live (the user may have killed either).
(define (dired-sidebar-live?)
  (and (member "*dired-sidebar*" (buffer-names))
       *dired-sidebar-window*
       (member *dired-sidebar-window* (window-list))))

;; Open the tree in a 20%-wide sidebar on the left, pushing the current buffer into
;; an 80% pane on the right. Re-invoking just refocuses the sidebar, preserving the
;; expanded directories and any pending marks; only a fresh open resets them.
;; Move point onto the row showing full path PATH, if it's currently visible in
;; the tree. Entries are (line path is-dir); dirs carry a trailing slash, so both
;; sides are stripped before comparing.
(define (dired-sidebar-goto-path path)
  (let ((target (dired-subtree-strip-slash path)))
    (let loop ((es *dired-sidebar-entries*))
      (cond ((null? es) #f)
            ((equal? (dired-subtree-strip-slash (nth (car es) 1)) target)
             (goto-line (nth (car es) 0)))
            (else (loop (cdr es)))))))

(define (dired-sidebar)
  (if (dired-sidebar-live?)
      (begin (select-window *dired-sidebar-window*) (dired-sidebar-render))
      ;; Read the visited file BEFORE the split — afterward the tree buffer is
      ;; active and buffer-file-name no longer points at the file we came from.
      (let ((file (buffer-file-name)))
        (set! *dired-sidebar-root* (dired-subtree-default-root))
        ;; Reveal the current file: open its ancestor dirs so it shows in the tree
        ;; ('() when there's no file, or it's outside the root).
        (set! *dired-sidebar-expanded*
              (if (equal? file "")
                  '()
                  (dired-subtree-ancestors *dired-sidebar-root* file)))
        (set! *dired-sidebar-marks* '())
        ;; The split keeps the selected pane selected, so it (now the left 20%) is
        ;; where the tree renders; the right 80% keeps the original buffer.
        (set! *dired-sidebar-window* (selected-window))
        (split-window-right-ratio 0.2)
        (dired-sidebar-render)
        ;; Land point on the revealed file (no-op if it isn't under the root).
        (when (not (equal? file ""))
          (dired-sidebar-goto-path file)))))
(define (dired-sidebar-refresh) (dired-sidebar-render))

;; directory-changed-hook handler: re-render only when the sidebar buffer is alive
;; and the change is in a directory the tree shows (root or an expanded subdir).
(define (dired-sidebar-on-fs-change dir)
  (when (and (member "*dired-sidebar*" (buffer-names))
             (member (dired-subtree-strip-slash dir)
                     (cons *dired-sidebar-root* *dired-sidebar-expanded*)))
    (set! *dired-sidebar-quiet* #t)
    (dired-sidebar-render)
    (set! *dired-sidebar-quiet* #f)))
(add-hook "directory-changed-hook" "dired-sidebar-on-fs-change")

;; Close the sidebar: kill the tree buffer and collapse its window.
(define (dired-sidebar-quit)
  (set! *dired-sidebar-window* #f)
  (watch-directories "dired-sidebar" '())   ; stop watching; nothing to refresh
  (kill-buffer-and-window))

;; --- point helpers ----------------------------------------------------------
;; The (path is-dir) under point, or #f.
(define (dired-sidebar-entry) (dired-subtree-entry-at *dired-sidebar-entries* (buffer-line)))
;; The full path under point, or #f.
(define (dired-sidebar-path)
  (let ((e (dired-sidebar-entry))) (if e (car e) #f)))
;; The directory to operate IN for the entry under point: the entry itself if it's
;; a directory, else its parent; the root when point is off any entry.
(define (dired-sidebar-context-dir)
  (let ((e (dired-sidebar-entry)))
    (if e
        (let ((path (dired-subtree-strip-slash (car e))) (is-dir (car (cdr e))))
          (if is-dir path (parent-directory path)))
        *dired-sidebar-root*)))

;; --- navigation -------------------------------------------------------------
;; RET/TAB/click: toggle a directory, or open a file in the main editor window
;; (the tree stays put).
(define (dired-sidebar-visit)
  (let ((e (dired-sidebar-entry)))
    (when e
      (let ((path (car e)) (is-dir (car (cdr e))))
        (if is-dir
            (begin
              (set! *dired-sidebar-expanded*
                    (dired-subtree-toggled *dired-sidebar-expanded*
                                           (dired-subtree-strip-slash path)))
              (dired-sidebar-render))
            (open-file-other-window path))))))

;; --- marks + delete ---------------------------------------------------------
(define (dired-sidebar-mark)
  (let ((p (dired-sidebar-path)))
    ;; Never flag the root row — `x` would `rm -rf` the whole tree.
    (when (and p
               (not (equal? (dired-subtree-strip-slash p) *dired-sidebar-root*))
               (not (member p *dired-sidebar-marks*)))
      (set! *dired-sidebar-marks* (cons p *dired-sidebar-marks*))
      (dired-sidebar-render)
      (next-line))))
(define (dired-sidebar-unmark)
  (let ((p (dired-sidebar-path)))
    (when p
      (set! *dired-sidebar-marks* (filter (lambda (m) (not (equal? m p))) *dired-sidebar-marks*))
      (dired-sidebar-render)
      (next-line))))

(define (dired-sidebar-do-delete)
  (if (null? *dired-sidebar-marks*)
      (message "No files marked (mark with d)")
      (y-or-n-p (string-append "Delete " (int->string (length *dired-sidebar-marks*)) " marked file(s)?")
                "dired-sidebar-delete-confirmed")))
(define (dired-sidebar-delete-confirmed ans)
  (when (equal? ans "yes")
    (for-each (lambda (p) (call-process "rm" (list "-rf" (dired-subtree-strip-slash p))))
              *dired-sidebar-marks*)
    (set! *dired-sidebar-marks* '())
    (dired-sidebar-render)
    (message "Deleted")))

;; --- rename / copy / mkdir --------------------------------------------------
(define (dired-sidebar-rename)
  (let ((p (dired-sidebar-path)))
    (when p
      (set! *dired-sidebar-src* (dired-subtree-strip-slash p))
      (read-string (string-append "Rename " (basename *dired-sidebar-src*) " to: ")
                   "dired-sidebar-rename-to"))))
(define (dired-sidebar-rename-to name)
  (when (not (equal? (trim-end name) ""))
    (call-process "mv" (list *dired-sidebar-src*
                             (path-join (parent-directory *dired-sidebar-src*) name)))
    (dired-sidebar-render) (message "Renamed")))

(define (dired-sidebar-copy)
  (let ((p (dired-sidebar-path)))
    (when p
      (set! *dired-sidebar-src* (dired-subtree-strip-slash p))
      (read-string (string-append "Copy " (basename *dired-sidebar-src*) " to: ")
                   "dired-sidebar-copy-to"))))
(define (dired-sidebar-copy-to name)
  (when (not (equal? (trim-end name) ""))
    (call-process "cp" (list "-r" *dired-sidebar-src*
                             (path-join (parent-directory *dired-sidebar-src*) name)))
    (dired-sidebar-render) (message "Copied")))

(define (dired-sidebar-mkdir)
  (set! *dired-sidebar-target-dir* (dired-sidebar-context-dir))
  (read-string "Create directory: " "dired-sidebar-mkdir-finish"))
(define (dired-sidebar-mkdir-finish name)
  (when (not (equal? (trim-end name) ""))
    (call-process "mkdir" (list "-p" (path-join *dired-sidebar-target-dir* name)))
    (dired-sidebar-render) (message "Created directory")))

;; --- keymap -----------------------------------------------------------------
;; Single keys resolve against this mode keymap under both Emacs and evil: as a
;; special buffer, dired-sidebar-mode's only evil overlay is j/k (evil-collection),
;; so d/u/x/g/q/R/C/+ fall through here just like dired-mode.
(define-key "dired-sidebar-mode" "enter" "dired-sidebar-visit")
(define-key "dired-sidebar-mode" "tab" "dired-sidebar-visit")
;; Left click acts like RET: expand/collapse a dir, or open a file at point.
(define-key "dired-sidebar-mode" "mouse-1" "dired-sidebar-visit")
(define-key "dired-sidebar-mode" "n" "next-line")
(define-key "dired-sidebar-mode" "p" "previous-line")
(define-key "dired-sidebar-mode" "g" "dired-sidebar-refresh")
(define-key "dired-sidebar-mode" "d" "dired-sidebar-mark")
(define-key "dired-sidebar-mode" "u" "dired-sidebar-unmark")
(define-key "dired-sidebar-mode" "x" "dired-sidebar-do-delete")
(define-key "dired-sidebar-mode" "shift-r" "dired-sidebar-rename")
(define-key "dired-sidebar-mode" "shift-c" "dired-sidebar-copy")
(define-key "dired-sidebar-mode" "+" "dired-sidebar-mkdir")
;; q closes the sidebar outright — kills the tree buffer AND collapses its pane.
(define-key "dired-sidebar-mode" "q" "dired-sidebar-quit")

;; The tree is a listing, not code — no line-number gutter (on by default for
;; every other major mode); highlight the current row; truncate long names at the
;; pane edge instead of wrapping (ordinary buffers still soft-wrap).
(set-option "line-numbers:dired-sidebar-mode" "false")
(set-option "hl-line:dired-sidebar-mode" "true")
(set-option "word-wrap:dired-sidebar-mode" "false")

(define-key "fundamental" "ctrl-c t" "dired-sidebar")
(register-command "dired-sidebar")
