;; treemacs.scm — a file-tree sidebar: a recursive, expandable directory view in
;; a special buffer, with Lucide icons. Built on render-buffer + set-line-icon +
;; open-file-other-window (RET on a file shows it in the main editor; the tree
;; stays). M-x treemacs (or C-c t). Expand/collapse dirs with RET/TAB.

(define *treemacs-root* "")
(define *treemacs-expanded* '())   ; dir paths (no trailing /) currently open
(define *treemacs-lines* '())
(define *treemacs-entries* '())    ; (line path is-dir)
(define *treemacs-icons* '())      ; (line col icon-name)
(define *treemacs-faces* '())      ; (line face) — whole-line color (dir/dotfile)
(define *treemacs-guides* '())     ; (start end face) — indent-guide overlay accents
(define *treemacs-byte* 0)         ; byte offset of the next line's start
(define *treemacs-n* 0)

;; Colors come from faces the active theme already defines, so the tree tracks
;; the current theme (and any `load-theme` switch) instead of hardcoding hex:
;;   directories  -> "keyword" (the theme's primary accent)
;;   dotfiles     -> "comment" (the theme's muted color)
;;   indent rails -> "dim"     (the theme's faint UI color)
;; A whole-line face also tints that line's icon (the renderer reuses it). The
;; renderer falls back to the foreground if a theme omits one of these names.

(define (strip-slash p)
  (if (ends-with? p "/") (substring p 0 (- (string-length p) 1)) p))

;; Vertical indent guides: one "│ " per ancestor level. Each is 2 columns wide,
;; matching the old 2-space indent so icon/name columns still line up.
(define (treemacs-rails depth)
  (if (<= depth 0) "" (string-append "│ " (treemacs-rails (- depth 1)))))

;; Whole-line face for an entry, or #f to leave it the default foreground.
;; Dotfiles dim regardless of kind; other directories take the accent.
(define (treemacs-face name is-dir)
  (cond ((starts-with? name ".") "comment")
        (is-dir "keyword")
        (else #f)))

;; Extension → Lucide icon (asset icons/<name>.svg). Customize freely.
(define (file-icon name)
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

;; Emit one row at `depth`: indent rails, then 3 columns reserved for the icon,
;; then the name. The icon and the guide overlay are addressed by BYTE offset
;; (a "│" is 3 bytes), so we measure the rail prefix with utf8-length rather
;; than assume 1 byte per column. Byte offsets are tracked here because writes
;; are queued — we can't read them back from the buffer after render.
(define (treemacs-emit name path is-dir icon depth)
  (let* ((rails (treemacs-rails depth))
         (gb (utf8-length rails))                  ; byte width of the rail region
         (text (string-append rails "   " name))   ; icon sits in the 3 reserved cols
         (byte *treemacs-byte*)
         (face (treemacs-face name is-dir)))
    (set! *treemacs-lines* (cons text *treemacs-lines*))
    (set! *treemacs-entries* (cons (list *treemacs-n* path is-dir) *treemacs-entries*))
    (set! *treemacs-icons* (cons (list *treemacs-n* gb icon) *treemacs-icons*))
    (when face (set! *treemacs-faces* (cons (list *treemacs-n* face) *treemacs-faces*)))
    (when (> gb 0)
      (set! *treemacs-guides*
            (cons (list byte (+ byte gb) "dim") *treemacs-guides*)))
    (set! *treemacs-n* (+ *treemacs-n* 1))
    (set! *treemacs-byte* (+ *treemacs-byte* (utf8-length text) 1)))) ; +1 = newline

;; Directories first, then files — each group keeps list-directory's
;; alphabetical order (filter is stable).
(define (treemacs-sort-entries entries)
  (append (filter (lambda (e) (ends-with? e "/")) entries)
          (filter (lambda (e) (not (ends-with? e "/"))) entries)))

;; Walk one directory level; recurse into expanded subdirectories.
(define (treemacs-walk dir depth)
  (for-each
    (lambda (full)
      (let* ((is-dir (ends-with? full "/"))
             (clean (strip-slash full))
             (name (basename clean))
             (open (and is-dir (member clean *treemacs-expanded*)))
             (icon (if is-dir (if open "folder-open" "folder") (file-icon name))))
        (treemacs-emit name full is-dir icon depth)
        (when open (treemacs-walk clean (+ depth 1)))))
    (treemacs-sort-entries (list-directory dir))))

(define (treemacs-render)
  (set! *treemacs-lines* '())
  (set! *treemacs-entries* '())
  (set! *treemacs-icons* '())
  (set! *treemacs-faces* '())
  (set! *treemacs-guides* '())
  (set! *treemacs-byte* 0)
  (set! *treemacs-n* 0)
  (treemacs-emit (basename *treemacs-root*) *treemacs-root* #t "folder-open" 0)
  (treemacs-walk *treemacs-root* 1)
  (render-buffer "*treemacs*" "treemacs-mode"
                 (join-lines (reverse *treemacs-lines*))
                 (reverse *treemacs-faces*))
  (for-each (lambda (ic) (set-line-icon (nth ic 0) (nth ic 1) (nth ic 2)))
            (reverse *treemacs-icons*))
  ;; Indent rails: paint the leading "│" columns with the theme's "dim" face via
  ;; overlays so they stay faint even on accented directory rows (the whole-line
  ;; face would otherwise tint them too). clear-overlays first so a refresh
  ;; doesn't stack stale rails.
  (clear-overlays)
  (for-each (lambda (g) (overlay-set-face (nth g 0) (nth g 1) (nth g 2)))
            (reverse *treemacs-guides*)))

;; The pane id hosting the sidebar, so a repeat invocation focuses it instead of
;; splitting again.
(define *treemacs-window* #f)

;; Is the sidebar already on screen? Its buffer must still exist and its pane
;; must still be live (the user may have killed either).
(define (treemacs-live?)
  (and (member "*treemacs*" (buffer-names))
       *treemacs-window*
       (member *treemacs-window* (window-list))))

;; Open the file tree in a 20%-wide sidebar on the left, pushing the current
;; buffer into an 80% pane on the right. Re-invoking just refocuses the sidebar.
(define (treemacs)
  (set! *treemacs-root* (strip-slash (or (project-root (buffer-directory)) (buffer-directory))))
  (set! *treemacs-expanded* '())
  (if (treemacs-live?)
      (begin (select-window *treemacs-window*) (treemacs-render))
      (begin
        ;; The split keeps the selected pane selected, so it (now the left 20%)
        ;; is where the tree renders; the right 80% keeps the original buffer.
        (set! *treemacs-window* (selected-window))
        (split-window-right-ratio 0.2)
        (treemacs-render))))
(define (treemacs-refresh) (treemacs-render))

;; Close the sidebar: kill the tree buffer and collapse its window.
(define (treemacs-quit)
  (set! *treemacs-window* #f)
  (kill-buffer-and-window))

;; (path is-dir) of the entry on the current line, or #f.
(define (treemacs-entry-at line)
  (let loop ((es *treemacs-entries*))
    (cond ((null? es) #f)
          ((= (car (car es)) line) (cdr (car es)))
          (else (loop (cdr es))))))

(define (treemacs-toggle-dir path)
  (if (member path *treemacs-expanded*)
      (set! *treemacs-expanded* (filter (lambda (p) (not (equal? p path))) *treemacs-expanded*))
      (set! *treemacs-expanded* (cons path *treemacs-expanded*)))
  (treemacs-render))

;; RET/TAB: toggle a directory, or open a file in the main editor window.
(define (treemacs-visit)
  (let ((e (treemacs-entry-at (buffer-line))))
    (when e
      (let ((path (car e)) (is-dir (car (cdr e))))
        (if is-dir
            (treemacs-toggle-dir (strip-slash path))
            (open-file-other-window path))))))

(define-key "treemacs-mode" "enter" "treemacs-visit")
(define-key "treemacs-mode" "tab" "treemacs-visit")
;; Left click acts like RET: expand/collapse a dir, or open a file at point.
(define-key "treemacs-mode" "mouse-1" "treemacs-visit")
(define-key "treemacs-mode" "n" "next-line")
(define-key "treemacs-mode" "p" "previous-line")
(define-key "treemacs-mode" "g" "treemacs-refresh")
;; q closes the sidebar outright — kills the tree buffer AND collapses the pane
;; it popped up in (Emacs treemacs `q`), not just the buffer.
(define-key "treemacs-mode" "q" "treemacs-quit")

;; The sidebar is a tree, not code — no line-number gutter (line numbers are on
;; by default for every other major mode).
(set-option "line-numbers:treemacs-mode" "false")
;; Highlight the row the cursor is on, so the selection is obvious.
(set-option "hl-line:treemacs-mode" "true")
;; Long names truncate at the pane edge instead of wrapping onto a second row
;; (ordinary buffers still soft-wrap).
(set-option "word-wrap:treemacs-mode" "false")

(define-key "fundamental" "ctrl-c t" "treemacs")
(register-command "treemacs")
