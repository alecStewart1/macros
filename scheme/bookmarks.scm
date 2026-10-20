;; requires: core helm persistence

;; bookmarks.scm — Emacs `bookmark.el`: named positions in files that survive a
;; restart. Set one with C-x r m (`bookmark-set`), jump back with C-x r b
;; (`bookmark-jump`), browse them all in Helm with C-x r l (`helm-bookmarks`),
;; and M-x `bookmark-delete` / `bookmark-rename` for upkeep.
;;
;; State is a single s-expression file — p-lists inside an a-list, like Emacs's
;; `~/.emacs.d/bookmarks` — alongside the other state files in the persistence
;; dir (`~/.config/macros/bookmarks`; saveplace, recentf, … live there too).
;; Every mutation rewrites the file, so there is no separate save step to
;; forget.
;;
;; File format (fields mirror bookmark.el):
;;   (("name"
;;     (filename . "path")
;;     (position . 123)          ; byte offset — kept for Emacs familiarity
;;     (line . 4) (column . 2)   ; what we actually jump by (survives edits
;;                               ; above the bookmark better than a raw byte)
;;     (front-context-string . "…after point…")
;;     (rear-context-string . "…before point…")))
;;
;; A reader/writer for that sexp format lives here (bm--parse*); there is no
;; `read` primitive in the engine yet, so it's a ~60-line Scheme reader.

;; Absolute path of the bookmarks state file (persistence dir + "bookmarks").
;; Read at load and rewritten on every mutation; see the file-format notes above.
(define (bookmarks-file)
  (string-append (persistence-dir) "/bookmarks"))

(define *bookmarks* '())

;; The value of field `k` (a string) of a bookmark's FIELDS list (what
;; `(alist-ref *bookmarks* name)` returns), or #f.
(define (bm-field e k) (alist-ref e k))

;; reading

;; Skip whitespace and `;` comment lines; returns the next live index.
(define (bm--skip-ws s i n)
  (if (>= i n) i
      (let ((c (string-ref s i)))
        (cond ((or (char=? c #\space) (char=? c #\newline) (char=? c #\tab) (char=? c #\return))
               (bm--skip-ws s (+ i 1) n))
              ((char=? c #\;) (bm--skip-ws s (bm--skip-comment s i n) n))
              (else i)))))
(define (bm--skip-comment s i n)
  (if (or (>= i n) (char=? (string-ref s i) #\newline))
      i
      (bm--skip-comment s (+ i 1) n)))

;; The char at `i`, or a space past the end (never a delimiter by accident).
(define (bm--char-at s i n) (if (< i n) (string-ref s i) #\space))

(define (bm--delim? c)
  (or (char=? c #\space) (char=? c #\newline) (char=? c #\tab) (char=? c #\return)
      (char=? c #\() (char=? c #\)) (char=? c #\") (char=? c #\;)))

;; Parse one datum at `i`; returns (list VALUE NEXT-I). VALUE is a string,
;; number, or list — atoms come back as STRINGS (we only ever parse field
;; names, and that keeps the reader free of string->symbol). #f at end of
;; input. Unterminated strings/lists run to EOF (never fails).
(define (bm--parse s i n)
  (let ((i (bm--skip-ws s i n)))
    (cond ((>= i n) (list #f i))
          ((char=? (string-ref s i) #\() (bm--parse-list s (+ i 1) n '()))
          ((char=? (string-ref s i) #\") (bm--parse-string s (+ i 1) n '()))
          (else (bm--parse-atom s i n)))))

;; Elements until the matching `)`; a bare `.` makes the list improper.
(define (bm--parse-list s i n acc)
  (let ((i (bm--skip-ws s i n)))
    (cond ((>= i n) (list (reverse acc) i))
          ((char=? (string-ref s i) #\)) (list (reverse acc) (+ i 1)))
          ((and (char=? (string-ref s i) #\.)
                (bm--delim? (bm--char-at s (+ i 1) n)))
           (let* ((r (bm--parse s (+ i 1) n))
                  (j (bm--skip-ws s (cadr r) n)))
             (if (and (< j n) (char=? (string-ref s j) #\)))
                 (list (bm--dot (reverse acc) (car r)) (+ j 1))
                 (list (reverse acc) j))))          ; malformed → treat as proper
          (else (let ((r (bm--parse s i n)))
                  (bm--parse-list s (cadr r) n (cons (car r) acc)))))))

;; (a b …) plus a dotted `tail` → (a b … . tail) (append when tail is a list).
(define (bm--dot proper tail)
  (if (list? tail)
      (append proper tail)
      (let loop ((l proper))
        (cond ((null? l) tail)
              ((null? (cdr l)) (cons (car l) tail))
              (else (cons (car l) (loop (cdr l))))))))

(define (bm--parse-string s i n acc)
  (cond ((>= i n) (list (list->string (reverse acc)) i))
        (else
         (let ((c (string-ref s i)))
           (cond ((char=? c #\") (list (list->string (reverse acc)) (+ i 1)))
                 ((char=? c #\\)
                  (let ((c2 (bm--char-at s (+ i 1) n)))
                    (bm--parse-string s (+ i 2) n
                      (cons (cond ((char=? c2 #\n) #\newline)
                                  ((char=? c2 #\t) #\tab)
                                  ((char=? c2 #\r) #\return)
                                  (else c2))
                            acc))))
                 (else (bm--parse-string s (+ i 1) n (cons c acc))))))))

(define (bm--parse-atom s i n)
  (let loop ((j i) (acc '()))
    (if (or (>= j n) (bm--delim? (string-ref s j)))
        (let* ((tok (list->string (reverse acc)))
               (num (string->number tok)))
          (list (or num tok) j))
        (loop (+ j 1) (cons (string-ref s j) acc)))))

;; Every top-level datum of `s`, in order.
(define (bm--read-all s)
  (let ((n (string-length s)))
    (let loop ((i 0) (acc '()))
      (let ((r (bm--parse s i n)))
        (if (equal? (car r) #f)
            (reverse acc)
            (loop (cadr r) (cons (car r) acc)))))))

;; writing

;; `s` as a Scheme string literal body: quotes and backslashes escaped, control
;; characters spelled out.
(define (bm-escape s)
  (list->string
    (let loop ((cs (string->list s)) (acc '()))
      (cond ((null? cs) (reverse acc))
            ((or (char=? (car cs) #\") (char=? (car cs) #\\))
             (loop (cdr cs) (cons (car cs) (cons #\\ acc))))
            ((char=? (car cs) #\newline) (loop (cdr cs) (cons #\n (cons #\\ acc))))
            ((char=? (car cs) #\tab)     (loop (cdr cs) (cons #\t (cons #\\ acc))))
            ((char=? (car cs) #\return)  (loop (cdr cs) (cons #\r (cons #\\ acc))))
            (else (loop (cdr cs) (cons (car cs) acc)))))))

(define (bm-field-str name v)
  (cond ((string? v) (string-append "  (" name " . \"" (bm-escape v) "\")"))
        ((number? v) (string-append "  (" name " . " (int->string v) ")"))
        (else (string-append "  (" name ")"))))

(define (bm--serialize-entry e)
  (string-append "(\"" (bm-escape (car e)) "\"\n"
    (bm-field-str "filename"              (bm-field (cdr e) "filename")) "\n"
    (bm-field-str "position"              (bm-field (cdr e) "position")) "\n"
    (bm-field-str "line"                  (bm-field (cdr e) "line")) "\n"
    (bm-field-str "column"                (bm-field (cdr e) "column")) "\n"
    (bm-field-str "front-context-string"  (bm-field (cdr e) "front-context-string")) "\n"
    (bm-field-str "rear-context-string"   (bm-field (cdr e) "rear-context-string"))
    ")"))

(define (bm--serialize)
  (string-append
    ";;;; Macros Bookmark Format Version 1 ;;;; -*- mode: lisp-data -*-\n"
    ";;; This format is meant to be slightly human-readable;\n"
    ";;; nevertheless, you probably don't want to edit it.\n"
    ";;; -*- End Of Bookmark File Format Version Stamp -*-\n"
    "(\n"
    (join-lines (map bm--serialize-entry *bookmarks*))
    ")\n"))

;; load / save

(define (bm-load)
  (let ((f (bookmarks-file)))
    (when (file-exists? f)
      (let ((tops (bm--read-all (read-file f))))
        ;; First top-level list is the bookmark a-list; entries missing the
        ;; filename field (hand-edited badly) are dropped.
        (when (and (pair? tops) (list? (car tops)))
          (set! *bookmarks*
                (filter (lambda (e) (and (pair? e) (string? (car e))
                                         (string? (bm-field (cdr e) "filename"))))
                        (car tops))))))))

;; Rewrite the file. Cheap (a few dozen short lines), so callers just do it.
(define (bm-save)
  (write-file (bookmarks-file) (bm--serialize)))

;; Record bookmark NAME for PATH at byte offset POS (line LINE, column COL),
;; replacing any existing entry of the same name, and save. FRONT/REAR are the
;; text around the position — display context, and a way to spot a stale
;; bookmark. The primitive under bookmark-set-named and org-capture's
;; `org-capture-last-stored` tracking; an agent can call it directly.
(define (bookmark-store name path pos line col front rear)
  (set! *bookmarks*
        (alist-set *bookmarks* name
          (list (cons "filename" path)
                (cons "position" pos)
                (cons "line" line)
                (cons "column" col)
                (cons "front-context-string" front)
                (cons "rear-context-string" rear))))
  (bm-save))

(define (bm-delete-named! name)
  (set! *bookmarks* (alist-remove *bookmarks* name))
  (bm-save))

;; set (C-x r m)

;; Context strings are capped so a bookmark on a minified line can't bloat the
;; file (Emacs caps them too).
(define (bookmark-clip s)
  (if (<= (string-length s) 96) s (substring s 0 96)))

;; Set a bookmark called NAME on the current buffer at point (a file-visiting
;; buffer only). The programmatic half of the C-x r m command; what an MCP
;; agent normally calls instead of driving the minibuffer.
(define (bookmark-set-named name)
  (let* ((f (buffer-file-name)))
    (when (and (string? f) (not (equal? f "")))
      (let ((line (buffer-line)))
        (bookmark-store name f (point) line (- (point) (position->byte line 0))
                        (bookmark-clip (buffer-substring (point)
                                                         (min (point-max) (+ (point) 96))))
                        (bookmark-clip (buffer-substring (max 0 (- (point) 96)) (point))))
        (message (string-append "Bookmark set: " name))))))

;; C-x r m — set a bookmark on the current buffer, prompting for its name.
(define-command (bookmark-set)
  (read-string "Set bookmark (name): "
    (lambda (nm)
      (let ((nm (string-trim nm)))
        (when (not (equal? nm ""))
          (bookmark-set-named nm))))))

;; jump (C-x r b)

;; Jump to bookmark entry `e`: open its file if needed, then land on the
;; remembered line/column once the buffer is current.
(define (bm-jump-entry e)
  (let ((path (bm-field e "filename")))
    (open-file path)
    (goto-when-visiting "" path (or (bm-field e "line") 0) (or (bm-field e "column") 0))))

;; Jump to the bookmark NAME: open its file (if not already visiting it) and
;; land on the remembered line/column. Message when there is no such bookmark.
(define (bm-jump name)
  (let ((e (alist-ref *bookmarks* name)))
    (if e (bm-jump-entry e) (message "No such bookmark"))))

;; the Helm source

;; The Helm candidate line for bookmark entry `e`: "NAME — PATH". The exact
;; same string is displayed and matched back on selection (as Emacs selects by
;; the bookmark record, never by parsing the display), so a name may contain
;; anything, including the separator.
(define (bm-candidate e)
  (string-append (car e) " — " (or (bm-field (cdr e) "filename") "")))

(define (bm-candidates)
  (map bm-candidate *bookmarks*))

(define (bm-jump-selected sel)
  (let loop ((ls *bookmarks*))
    (cond ((null? ls) (message "No such bookmark"))
          ((equal? (bm-candidate (car ls)) sel)
           (bm-jump-entry (cdr (car ls))))
          (else (loop (cdr ls))))))

;; C-x r l — browse every bookmark in a Helm picker; selecting one jumps to it.
(define-command (helm-bookmarks)
  (helm (list (list "Bookmarks" (bm-candidates) 'bm-jump-selected))))

;; C-x r b — pick a bookmark to jump to (same picker as helm-bookmarks).
(define-command (bookmark-jump)
  (helm (list (list "Jump to bookmark" (bm-candidates) 'bm-jump-selected))))

;; delete / rename

;; M-x bookmark-delete — pick a bookmark and remove it from the file.
(define-command (bookmark-delete)
  (completing-read "Delete bookmark: " (map car *bookmarks*) 'bm-delete-selected))
(define (bm-delete-selected name) (bm-delete-named! name))

;; Rename the bookmark OLD to NEW (saving), keeping its fields. No-op when
;; there is no such bookmark or NEW is empty/same.
(define (bm-rename! old new)
  (when (and (not (equal? new "")) (not (equal? new old)))
    (let ((e (alist-ref *bookmarks* old)))
      (when e
        ;; e is the FIELDS list (alist-ref returns the entry's cdr); re-key it.
        (bm-delete-named! old)
        (set! *bookmarks* (alist-set *bookmarks* new e))
        (bm-save)
        (message (string-append "Renamed bookmark: " old " → " new))))))

;; M-x bookmark-rename — pick a bookmark, then a new name for it (no-op when
;; the name is unchanged or empty).
(define-command (bookmark-rename)
  (completing-read "Rename bookmark: " (map car *bookmarks*) 'bm-rename-prompt))
(define (bm-rename-prompt old)
  (read-string (string-append "Rename " old " to: ")
    (lambda (new) (bm-rename! old (string-trim new)))))

;; keys
;; (C-x r k/y/p stay rectangle commands in grid mode; m/b/l are unclaimed.)
(define-key "fundamental" "ctrl-x r m" 'bookmark-set)
(define-key "fundamental" "ctrl-x r b" 'bookmark-jump)
(define-key "fundamental" "ctrl-x r l" 'helm-bookmarks)

(bm-load)
