;; occur.scm — Emacs `occur` plus editable result buffers (occur-edit-mode /
;; wgrep). A "results buffer" is a read-only special buffer of
;;
;;     <header line>
;;     <prefix><text>          one line per hit, e.g. "     12:" + source line
;;
;; backed by a per-buffer SPEC kept here: the header, the major mode, the regexp
;; that was searched (to highlight matches), and a vector of RECORDS
;; `(bufname path line0 prefix text)`. `*occur*` (M-x occur) and projectile's
;; `*grep*` both use it, so RET/mouse visits, n/p, and the edit-and-apply cycle
;; are shared:
;;
;;   e / C-c C-p  (i under evil/helix)  make the result text editable. Only the
;;                text AFTER each prefix can change — the header and every
;;                `file:line:` prefix carry a `read-only` text property, so the
;;                editor refuses edits that touch them (or the newline before
;;                them, which would join two results).
;;   C-c C-c      apply every changed line back to its source:
;;                  * a source with an open buffer is edited IN that buffer and
;;                    left modified (unsaved) — review, then save as usual;
;;                  * a file that isn't open is edited directly ON DISK (read,
;;                    patch the line, write) — no buffer is created.
;;                A line is only patched when the source still holds the text
;;                the results showed; anything else is reported as skipped.
;;   C-c C-k      abort: throw the edits away and restore the results.
;;
;; Positions: the editor's overlay/property APIs take BYTE offsets while Steel
;; strings are CHAR-indexed (see core.scm's utf8-length). Every offset below is
;; built from byte lengths (`utf8-length`) or Rust-reported byte spans; text is
;; only ever sliced with char-based `substring` on char counts.

(set-face "occur-header" "#66d9ef")
(set-face "occur-prefix" "#75715e")
(set-face "occur-match" "#e6db74")
(set-face-bold "occur-match" #t)

;; --- result-buffer specs -----------------------------------------------------
(define *results-specs* (hash))

;; The spec of results buffer `name`, or #f.
(define (results-spec name)
  (if (hash-contains? *results-specs* name) (hash-ref *results-specs* name) #f))

;; A MUTABLE vector of `items` (list->vector builds an immutable one, which
;; vector-set! rejects).
(define (list->mutable-vector items)
  (let ((v (make-vector (length items) #f)))
    (let loop ((xs items) (i 0))
      (if (null? xs) v (begin (vector-set! v i (car xs)) (loop (cdr xs) (+ i 1)))))))

;; Register `records` (a list) as the content of results buffer `name`.
(define (results-set! name header mode pattern records)
  (set! *results-specs*
        (hash-insert *results-specs* name
                     (list header mode pattern (list->mutable-vector records)))))

(define (results-header spec) (list-ref spec 0))
(define (results-mode spec) (list-ref spec 1))
(define (results-pattern spec) (list-ref spec 2))
(define (results-records spec) (list-ref spec 3))

(define (results-record bufname path line0 prefix text)
  (list bufname path line0 prefix text))
(define (record-bufname r) (list-ref r 0))
(define (record-path r) (list-ref r 1))
(define (record-line r) (list-ref r 2))
(define (record-prefix r) (list-ref r 3))
(define (record-text r) (list-ref r 4))

;; Lay out a spec: `(text overlay-triples read-only-ranges)`. Offsets are BYTES.
;; The first read-only range spans the header, its newline and the first
;; prefix; each later one starts at its line start. The editor also refuses
;; deleting the char just before a read-only range, which protects the newline
;; ending the previous result (no joining two results into one line).
(define (results-layout spec)
  (let* ((header (results-header spec))
         (records (vector->list (results-records spec)))
         (spans (regexp-search-lines (results-pattern spec) (map record-text records)))
         (hb (+ (utf8-length header) 1)))
    (let loop ((rs records) (sp spans) (off hb) (lines (list header))
               (ovs '()) (ros '()) (first? #t))
      (if (null? rs)
          (list (string-join (reverse lines) "\n") ovs (reverse ros))
          (let* ((r (car rs))
                 (prefix (record-prefix r))
                 (text (record-text r))
                 (tstart (+ off (utf8-length prefix)))
                 (match-ovs (map (lambda (m) (list (+ tstart (car m)) (+ tstart (cadr m))
                                                   "occur-match"))
                                 (if (null? sp) '() (car sp)))))
            (loop (cdr rs) (if (null? sp) '() (cdr sp))
                  (+ tstart (utf8-length text) 1)
                  (cons (string-append prefix text) lines)
                  (cons (list off tstart "occur-prefix") (append match-ovs ovs))
                  (cons (if first? (list 0 tstart) (list off tstart)) ros)
                  #f))))))

;; (Re)draw results buffer `name` from its spec — read-only, header faced,
;; prefixes dimmed, matches highlighted. Stale overlays from a previous render
;; are dropped first (an in-place refresh keeps the buffer's overlay list).
(define (results-render name)
  (let* ((spec (results-spec name))
         (layout (results-layout spec)))
    (render-buffer name (results-mode spec) (car layout) (list (list 0 "occur-header")))
    (clear-overlays)
    (overlay-set-faces (cadr layout))))

;; Index of the record on the cursor line of the current results buffer, or #f
;; (header line / not a results buffer).
(define (results-current-index)
  (let ((spec (results-spec (current-buffer))))
    (and spec
         (let ((i (- (buffer-line) 1)))
           (and (>= i 0) (< i (vector-length (results-records spec))) i)))))

;; --- visiting a hit ------------------------------------------------------------
;; The open buffer behind a record: its recorded buffer if still open, else any
;; buffer visiting its path, else #f.
(define (results-live-buffer bufname path)
  (let ((names (buffer-names)))
    (cond ((and (not (equal? bufname "")) (member bufname names)) bufname)
          ((equal? path "") #f)
          (else (find-buffer-visiting path)))))

;; Show `bufname` (or the file `path`) in another window and put point on
;; 0-based `line0`, byte column `col`.
(define (visit-location bufname path line0 col)
  (let ((live (results-live-buffer bufname path)))
    (cond
      ((and live (>= (get-buffer-window live) 0)) (select-window (get-buffer-window live)))
      ((and (not live) (not (equal? path ""))) (open-file-other-window path))
      (live
       (when (< (length (window-list)) 2) (split-window-below))
       (other-window)
       (switch-to-buffer live))
      (else (message (string-append "Buffer " bufname " no longer exists"))))
    (when (or live (not (equal? path "")))
      (goto-when-visiting bufname path line0 col))))

;; RET / mouse-1 / o in a results buffer: jump to the hit (first match column).
(define-command (results-visit)
  (let ((i (results-current-index)))
    (if (not i)
        (message "No result on this line")
        (let* ((spec (results-spec (current-buffer)))
               (r (vector-ref (results-records spec) i))
               (spans (car (regexp-search-lines (results-pattern spec)
                                                (list (record-text r)))))
               (col (if (null? spans) 0 (car (car spans)))))
          (visit-location (record-bufname r) (record-path r) (record-line r) col)))))

;; n / p: step between result lines (never onto the header).
(define-command (results-next)
  (let ((spec (results-spec (current-buffer))))
    (when spec
      (let ((l (buffer-line)) (n (vector-length (results-records spec))))
        (cond ((< l 1) (goto-line 1))
              ((< l n) (goto-line (+ l 1)))
              (else (message "No more matches")))))))
(define-command (results-previous)
  (let ((l (buffer-line)))
    (if (> l 1) (goto-line (- l 1)) (message "No earlier matches"))))

;; --- occur ---------------------------------------------------------------------
;; List every line matching a regexp (case-insensitive when the regexp is all
;; lower case, like Emacs's case-fold-search) in *occur*, shown in a window
;; below the source. Searching runs in Rust over the buffer snapshot
;; (`buffer-search-all`), so no copy of the whole buffer crosses into Scheme —
;; only the matching lines do.
(define *occur-last* "")

(define-command (occur)
  (read-string (if (equal? *occur-last* "")
                   "List lines matching regexp: "
                   (string-append "List lines matching regexp (default " *occur-last* "): "))
               'occur-run))

;; Smart case: an all-lower-case regexp matches case-insensitively.
(define (occur-fold-pattern re)
  (if (equal? re (string-downcase re)) (string-append "(?i)" re) re))

;; `(match-count . records)` for every line of the active buffer holding a
;; non-empty match of `pat`, or the regexp error string.
(define (occur-collect pat)
  (let ((ms (buffer-search-all pat 0 (point-max)))
        (src (current-buffer))
        (path (buffer-file-name)))
    (if (string? ms)
        ms
        (let loop ((ms ms) (last-line -1) (acc '()) (count 0))
          (if (null? ms)
              (cons count (reverse acc))
              (let* ((m (car ms)) (s (car m)))
                (cond
                  ((= s (cadr m)) (loop (cdr ms) last-line acc count))
                  ((= (line-of-pos s) last-line) (loop (cdr ms) last-line acc (+ count 1)))
                  (else
                   (let ((line (line-of-pos s)))
                     (loop (cdr ms) line
                           (cons (results-record src path line
                                                 (string-append
                                                  (pad-left (int->string (+ line 1)) 7) ":")
                                                 (buffer-substring (line-start-pos s)
                                                                   (line-end-pos s)))
                                 acc)
                           (+ count 1)))))))))))

(define (occur-run input)
  (let ((re (if (equal? input "") *occur-last* input)))
    (if (equal? re "")
        (message "No regexp given")
        (let* ((pat (occur-fold-pattern re))
               (res (occur-collect pat)))
          (set! *occur-last* re)
          (cond
            ((string? res) (message (string-append "Invalid regexp: " res)))
            ((null? (cdr res)) (message (string-append "No matches for \"" re "\"")))
            (else
             (let* ((count (car res)) (records (cdr res)) (nl (length records))
                    (summary (string-append (int->string count) " match"
                                            (if (= count 1) "" "es") " in "
                                            (int->string nl) " line" (if (= nl 1) "" "s")
                                            " for \"" re "\"")))
               (results-set! "*occur*"
                             (string-append summary " in buffer: " (current-buffer))
                             "occur-mode" pat records)
               (occur-display)
               (message summary))))))))

;; Show *occur* in a window below the source (reusing it when already shown),
;; with point on the first hit, and select it so n/p/RET work at once.
(define (occur-display)
  (let ((w (get-buffer-window "*occur*")))
    (if (>= w 0)
        (select-window w)
        (begin (split-window-below) (other-window)))
    (results-render "*occur*")
    (goto-line 1)))

;; --- editing results (occur-edit-mode / wgrep) --------------------------------
;; Make the current results buffer editable in place.
(define-command (results-edit)
  (let* ((name (current-buffer)) (spec (results-spec name)))
    (if (not spec)
        (message "Not a results buffer")
        (let ((layout (results-layout spec)))
          (set-special-mode "")
          (set-major-mode "results-edit-mode")
          (for-each (lambda (r) (put-text-property (car r) (cadr r) "read-only" "t"))
                    (caddr layout))
          (message "Edit the results, then C-c C-c to apply them (C-c C-k aborts)")))))

;; Back to the read-only results view (after apply or abort). The edit-mode
;; buffer is killed and redrawn fresh rather than flipped back in place, so its
;; undo history (which could otherwise replay into the protected prefixes) and
;; read-only properties go with it. Point keeps its line.
(define (results-leave-edit name line)
  (begin
    (cond ((helix-enabled?) (helix-normal-state))
          ((evil-enabled?) (evil-normal-state)))
    (kill-buffer-named name)
    (results-render name)
    (goto-line line)))

(define-command (results-abort)
  (let ((name (current-buffer)))
    (when (results-spec name)
      (results-leave-edit name (buffer-line))
      (message "Edits discarded"))))

;; Parse the edited buffer text back against the records: a list of
;; `(index new-text)` for the changed lines, or an error string. Every line must
;; still start with its record's prefix (the read-only property guards this; an
;; inserted line break is the one way to break the shape).
(define (results-parse-edits spec text)
  (let* ((records (results-records spec))
         (n (vector-length records))
         (lines (cdr (split-many text "\n"))))
    (if (not (= (length lines) n))
        "The number of result lines changed (line breaks can't be added or removed) — fix it or C-c C-k"
        (let loop ((ls lines) (i 0) (acc '()))
          (if (null? ls)
              (reverse acc)
              (let* ((r (vector-ref records i)) (prefix (record-prefix r)) (line (car ls)))
                (if (not (starts-with? line prefix))
                    (string-append "Line " (int->string (+ i 2))
                                   " lost its prefix — fix it or C-c C-k")
                    (let ((new (substring line (string-length prefix) (string-length line))))
                      (loop (cdr ls) (+ i 1)
                            (if (equal? new (record-text r)) acc (cons (list i new) acc)))))))))))

;; How a source line `cur` relates to the text the results showed (`old`):
;; 'exact (occur shows the whole line), 'trimmed (grep shows it without
;; trailing whitespace, which is kept), or #f — changed since the search.
(define (results-line-match cur old)
  (cond ((equal? cur old) 'exact)
        ((equal? (trim-end cur) old) 'trimmed)
        (else #f)))

;; The new full line for `cur` given the edited text `new` (keeping trailing
;; whitespace / a CR that grep's trimmed view didn't show).
(define (results-patched-line cur old new)
  (if (equal? cur old)
      new
      (string-append new (substring cur (string-length old) (string-length cur)))))

;; Apply `edits` — each `(index line0 old new)` — to the file at `path` on
;; disk. Returns `(applied-indices . skipped)`. Writes only when something
;; applied; an unreadable / non-UTF-8 file reads as "" so every line is skipped
;; and the file is left untouched.
(define (results-apply-file path edits)
  (let* ((lines (list->mutable-vector (split-many (read-file path) "\n")))
         (n (vector-length lines)))
    (let loop ((es edits) (applied '()) (skipped 0))
      (if (null? es)
          (begin
            (when (not (null? applied))
              (write-file path (string-join (vector->list lines) "\n")))
            (cons applied skipped))
          (let* ((e (car es)) (line0 (list-ref e 1)) (old (list-ref e 2)) (new (list-ref e 3))
                 (cur (if (< line0 n) (vector-ref lines line0) #f)))
            (if (and cur (results-line-match cur old))
                (begin (vector-set! lines line0 (results-patched-line cur old new))
                       (loop (cdr es) (cons (car e) applied) skipped))
                (loop (cdr es) applied (+ skipped 1))))))))

;; Apply state while the chain runs: 0 results buffer, 1 queue of
;; (buffer . edits) still to visit, 2 applied record indices, 3 skipped count,
;; 4 files written, 5 buffers modified, 6 alist index -> new text, 7 the
;; results buffer's cursor line (restored after the redraw).
(define *results-apply* #f)
(define (results-apply-get k) (list-ref *results-apply* k))
(define (results-apply-put k v)
  (set! *results-apply*
        (let loop ((xs *results-apply*) (i 0))
          (if (null? xs) '() (cons (if (= i k) v (car xs)) (loop (cdr xs) (+ i 1)))))))

;; C-c C-c: write the edited lines back. Unopened files are patched on disk
;; immediately; open buffers are visited one by one (a switch and a read of the
;; switched-to buffer can't share one Scheme batch, so each goes through a 0ms
;; timer that sees that buffer's FRESH snapshot) and patched bottom-up so
;; earlier lines' byte offsets stay valid.
(define-command (results-apply)
  (let* ((name (current-buffer)) (spec (results-spec name)))
    (if (not spec)
        (message "Not a results buffer")
        (let ((parsed (results-parse-edits spec (buffer-string))))
          (cond
            ((string? parsed) (message parsed))
            ((null? parsed) (results-leave-edit name (buffer-line)) (message "No changes to apply"))
            (else (results-start-apply name spec parsed)))))))

(define (results-start-apply name spec parsed)
  (let ((records (results-records spec)))
    (let loop ((ps parsed) (buffers '()) (files '()) (gone 0))
      (if (null? ps)
          (begin
            (set! *results-apply* (list name buffers '() gone 0 0
                                        (map (lambda (p) (cons (car p) (cadr p))) parsed)
                                        (buffer-line)))
            (for-each (lambda (f)
                        (let ((res (results-apply-file (car f) (cdr f))))
                          (results-note-applied (car res) (cdr res)
                                                (if (null? (car res)) 0 1) 0)))
                      files)
            (results-apply-next))
          (let* ((p (car ps)) (i (car p))
                 (r (vector-ref records i))
                 (edit (list i (record-line r) (record-text r) (cadr p)))
                 (live (results-live-buffer (record-bufname r) (record-path r))))
            (cond
              (live (loop (cdr ps) (results-group-add buffers live edit) files gone))
              ((not (equal? (record-path r) ""))
               (loop (cdr ps) buffers (results-group-add files (record-path r) edit) gone))
              (else (loop (cdr ps) buffers files (+ gone 1)))))))))

;; Add `edit` under `key` in an alist of (key . edits).
(define (results-group-add groups key edit)
  (cond ((null? groups) (list (cons key (list edit))))
        ((equal? (car (car groups)) key)
         (cons (cons key (cons edit (cdr (car groups)))) (cdr groups)))
        (else (cons (car groups) (results-group-add (cdr groups) key edit)))))

(define (results-note-applied indices skipped files buffers)
  (results-apply-put 2 (append indices (results-apply-get 2)))
  (results-apply-put 3 (+ (results-apply-get 3) skipped))
  (results-apply-put 4 (+ (results-apply-get 4) files))
  (results-apply-put 5 (+ (results-apply-get 5) buffers)))

(define (results-apply-next)
  (let ((queue (results-apply-get 1)))
    (if (null? queue)
        (results-apply-finish)
        (begin (switch-to-buffer (car (car queue)))
               (run-with-timer 0 'results-apply-buffer-step)))))

;; Patch the line `e` = (index line0 old new) of the ACTIVE buffer, reading the
;; snapshot. Returns #t when applied, #f when the line changed since the search.
(define (results-patch-buffer-line e)
  (let ((line0 (list-ref e 1)) (old (list-ref e 2)) (new (list-ref e 3)))
    (and (< line0 (line-count))
         (let* ((ls (position->byte line0 0))
                (le (line-end-pos ls))
                (how (results-line-match (buffer-substring ls le) old)))
           (cond ((equal? how 'exact) (replace-region ls le new) #t)
                 ((equal? how 'trimmed) (replace-region ls (+ ls (utf8-length old)) new) #t)
                 (else #f))))))

;; Timer step: patch the source buffer that is now active with its queued edits.
(define (results-apply-buffer-step)
  (when *results-apply*
    (let* ((queue (results-apply-get 1)) (group (car queue))
           (edits (sort (cdr group) (lambda (a b) (> (list-ref a 1) (list-ref b 1))))))
      (results-apply-put 1 (cdr queue))
      (if (not (equal? (current-buffer) (car group)))
          (results-note-applied '() (length edits) 0 0)
          (let loop ((es edits) (applied '()) (skipped 0))
            (cond
              ((null? es) (results-note-applied applied skipped 0 (if (null? applied) 0 1)))
              ((results-patch-buffer-line (car es))
               (loop (cdr es) (cons (car (car es)) applied) skipped))
              (else (loop (cdr es) applied (+ skipped 1))))))
      (results-apply-next))))

;; Return to the results buffer, fold the applied texts into its records, and
;; redraw it read-only with a summary.
(define (results-apply-finish)
  (let* ((name (results-apply-get 0)) (applied (results-apply-get 2))
         (skipped (results-apply-get 3)) (files (results-apply-get 4))
         (buffers (results-apply-get 5)) (texts (results-apply-get 6))
         (line (results-apply-get 7))
         (records (results-records (results-spec name)))
         (n (length applied)))
    (set! *results-apply* #f)
    (for-each (lambda (i)
                (let ((r (vector-ref records i)))
                  (vector-set! records i
                               (results-record (record-bufname r) (record-path r) (record-line r)
                                               (record-prefix r) (alist-ref texts i)))))
              applied)
    (switch-to-buffer name)
    (results-leave-edit name line)
    (message (string-append "Applied " (int->string n) " change" (if (= n 1) "" "s")
                            " (" (int->string files) " file" (if (= files 1) "" "s")
                            " written on disk, " (int->string buffers) " open buffer"
                            (if (= buffers 1) "" "s") " modified — save to keep)"
                            (if (> skipped 0)
                                (string-append "; " (int->string skipped)
                                               " skipped: source changed since the search")
                                "")))))

;; --- keymaps ---------------------------------------------------------------------
;; Read-only view keys, shared by *occur* and *grep* (projectile.scm).
(define (results-bind-view-keys mode)
  (define-key mode "enter" 'results-visit)
  (define-key mode "mouse-1" 'results-visit)
  (define-key mode "o" 'results-visit)
  (define-key mode "n" 'results-next)
  (define-key mode "p" 'results-previous)
  (define-key mode "e" 'results-edit)
  (define-key mode "ctrl-c ctrl-p" 'results-edit)
  (define-key mode "q" 'kill-buffer)
  ;; Under evil/helix the special-buffer overlays put `e` on word-end, so `i`
  ;; (insert → edit, like evil-collection's wgrep) enters editing there.
  (define-key (string-append "evil-normal:" mode) "i" 'results-edit)
  (define-key (string-append "helix-normal:" mode) "i" 'results-edit))
(results-bind-view-keys "occur-mode")

(define-key "results-edit-mode" "ctrl-c ctrl-c" 'results-apply)
(define-key "results-edit-mode" "ctrl-c ctrl-k" 'results-abort)
(define-key "results-edit-mode" "ctrl-x ctrl-s" 'results-apply)

(define-key "fundamental" "ctrl-c o" 'occur)
