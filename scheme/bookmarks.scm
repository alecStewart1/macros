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
;;     (rear-context-string . "…before point…")
;;     (annotation . "free text"))) ; optional, shown in the Helm picker
;;
;; Like bookmark.el, a bookmark may carry an ANNOTATION (free-form text) —
;; shown dimmed next to the candidate in C-x r l / C-x r b. Edit it with
;; M-x `bookmark-edit-annotation`; `bookmark-annotation` /
;; `bookmark-set-annotation!` read and write it programmatically. Hooks
;; (`add-hook` / M-x describe): `bookmark-save-hook` (after every rewrite,
;; since saving is automatic), `bookmark-load-hook` (after the file loads at
;; startup), `bookmark-after-jump-hook` (after a jump's point has landed).
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
    ;; The annotation is optional: a bookmark without one gets no field line.
    (let ((ann (bm-field (cdr e) "annotation")))
      (if (string? ann) (string-append "\n" (bm-field-str "annotation" ann)) ""))
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
                        (car tops))))))
    ;; `bookmark-load-hook` fires on every load — startup and the reload an
    ;; M-x bookmark-revert-style caller does — whether or not the file existed.
    (run-hook "bookmark-load-hook")))

;; Rewrite the file. Cheap (a few dozen short lines), so callers just do it.
;; `bookmark-save-hook` fires after every rewrite — there is no separate save
;; step, so this is the hook's only trigger (Emacs fires it on an explicit
;; save; ours happens automatically on every mutation).
(define (bm-save)
  (write-file (bookmarks-file) (bm--serialize))
  (run-hook "bookmark-save-hook"))

;; Record bookmark NAME for PATH at byte offset POS (line LINE, column COL),
;; replacing any existing entry of the same name, and save. FRONT/REAR are the
;; text around the position — display context, and a way to spot a stale
;; bookmark. The primitive under bookmark-set-named and org-capture's
;; `org-capture-last-stored` tracking; an agent can call it directly. A
;; position-independent ANNOTATION (set with bookmark-set-annotation!) survives
;; the re-set: it describes the target, not the point.
(define (bookmark-store name path pos line col front rear)
  (let ((existing (alist-ref *bookmarks* name))
        (fields (list (cons "filename" path)
                      (cons "position" pos)
                      (cons "line" line)
                      (cons "column" col)
                      (cons "front-context-string" front)
                      (cons "rear-context-string" rear))))
    (set! *bookmarks*
          (alist-set *bookmarks* name
            (let ((ann (and existing (bm-field existing "annotation"))))
              ;; Annotation goes at the FRONT, matching bm--fields-with — the
              ;; parser is key-based, but the two writers keep one shape.
              (if (string? ann)
                  (cons (cons "annotation" ann) fields)
                  fields))))
    (bm-save)))

;; The annotation of bookmark NAME (a string), or #f when it has none.
(define (bookmark-annotation name)
  (let ((e (alist-ref *bookmarks* name)))
    (and e (bm-field e "annotation"))))

;; The FIELDS list `e` with field `k` (a string) replaced or added at the front.
(define (bm--fields-with e k v)
  (cons (cons k v)
        (filter (lambda (p) (not (and (pair? p) (equal? (car p) k)))) e)))

;; Set the annotation of bookmark NAME to TEXT (saved immediately); TEXT ""
;; clears it. No-op when there is no such bookmark or TEXT isn't a string.
(define (bookmark-set-annotation! name text)
  (let ((e (alist-ref *bookmarks* name)))
    (when (and e (string? text))
      (set! *bookmarks*
            (alist-set *bookmarks* name
              (if (equal? text "")
                  (filter (lambda (p) (not (and (pair? p)
                                                (equal? (car p) "annotation")))) e)
                  (bm--fields-with e "annotation" text))))
      (bm-save))))

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
;; remembered line/column once the buffer is current, and fire
;; `bookmark-after-jump-hook` once point has landed (the goto parks —
;; goto-when-visiting — until the file is loaded, so the hook must wait for
;; that, not fire straight after the call).
;; *bm-jumps-owed*: how many jumps are still owed a hook fire. Core's
;; *visit-pending* is a single slot — a jump issued while another is pending
;; overwrites it — so landing is indistinguishable between the outstanding
;; jumps; when the pending visit lands, every owed jump fires (Emacs fires
;; after each jump; the clobbered ones' targets were superseded anyway). A
;; TIMED-OUT visit (the file never became current — core flags it in
;; *visit-result*) pays nothing: the hook must not fire on a failed jump.
(define *bm-jumps-owed* 0)

;; Poll until the pending visit has landed (core's *visit-pending* clears in
;; the same step that moves point, and *visit-result* says 'landed), then fire
;; the hook once per owed jump. A timeout gives up instead: no hook, and any
;; owed jumps are dropped with a message.
(define (bm--after-jump-step)
  (cond (*visit-pending* (run-with-timer 20 'bm--after-jump-step))
        ((equal? *visit-result* 'landed)
         (when (> *bm-jumps-owed* 0)
           (let ((n *bm-jumps-owed*))
             (set! *bm-jumps-owed* 0)
             (let loop ((i 0))
               (when (< i n)
                 (run-hook "bookmark-after-jump-hook")
                 (loop (+ i 1)))))))
        (else
         (when (> *bm-jumps-owed* 0)
           (set! *bm-jumps-owed* 0)
           (message "Bookmark jump failed: the file never opened")))))

(define (bm-jump-entry e)
  (let ((path (bm-field e "filename")))
    (open-file path)
    (set! *bm-jumps-owed* (+ *bm-jumps-owed* 1))
    (goto-when-visiting "" path (or (bm-field e "line") 0) (or (bm-field e "column") 0))
    (bm--after-jump-step)))

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

;; Candidates for the Helm sources: a bare string normally, or a
;; `(candidate annotation)` pair (Helm marginalia) for a bookmark with an
;; annotation — the annotation renders dimmed to the right of the row, and
;; only the candidate string is matched on and passed to the action.
(define (bm-candidates)
  (map (lambda (e)
         (let ((ann (bm-field (cdr e) "annotation")))
           (if (and (string? ann) (not (equal? ann "")))
               (list (bm-candidate e) ann)
               (bm-candidate e))))
       *bookmarks*))

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
;; there is no such bookmark or NEW is empty/same; refuses with a message when
;; NEW is already taken (renaming over it would silently delete that entry).
;; Exactly one rewrite, so bookmark-save-hook fires once.
(define (bm-rename! old new)
  (when (and (not (equal? new "")) (not (equal? new old)))
    (let ((e (alist-ref *bookmarks* old)))
      (cond ((not e))
            ((alist-ref *bookmarks* new)
             (message (string-append "Bookmark name already in use: " new)))
            (else
             ;; e is the FIELDS list (alist-ref returns the entry's cdr); re-key it.
             (set! *bookmarks* (alist-set (alist-remove *bookmarks* old) new e))
             (bm-save)
             (message (string-append "Renamed bookmark: " old " → " new)))))))

;; Edit the annotation of the bookmark NAME: a minibuffer prompt prefilled
;; with the current annotation; empty input clears it. Single-line, unlike
;; Emacs's full editing buffer — the text lives in the file and the Helm
;; picker either way.
(define (bm-annotation-prompt name)
  (read-string (string-append "Annotation for " name ": ")
    (lambda (text) (bookmark-set-annotation! name (string-trim text)))
    (or (bookmark-annotation name) "")))

;; M-x bookmark-edit-annotation — pick a bookmark, then an annotation for it.
(define-command (bookmark-edit-annotation)
  (completing-read "Edit annotation: " (map car *bookmarks*) 'bm-annotation-prompt))

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
