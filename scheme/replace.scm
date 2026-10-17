;; requires: core

;; replace.scm — Emacs's replace commands:
;;
;;   query-replace          (M-%)     ask at each match of a literal string
;;   query-replace-regexp   (C-M-%)   …of a regexp; `\1`…`\9` / `\&` in the
;;                                    replacement insert groups / the match
;;   replace-string / replace-regexp  replace every match, no questions
;;   project-query-replace  (C-c p r) query-replace through every project file
;;   project-query-replace-regexp (C-c p %)
;;                                    (aliases: projectile-replace / -regexp)
;;
;; All of them work from point to the end of the buffer, or inside the active
;; region (an evil Visual selection counts), and share one default: an empty
;; FROM reuses the previous pair, like Emacs's query-replace-defaults. Case
;; follows Emacs's defaults too: an all-lower-case FROM matches any case, and a
;; lower-case replacement then copies the match's case (FOO -> BAR, Foo -> Bar).
;;
;; During a query session (keys are read with the sticky `read-key-start`
;; capture, so it works the same under evil/helix):
;;   y / SPC  replace and move on       n / DEL  skip
;;   !        replace all remaining     .        replace this one and stop
;;   ,        replace, stay here        u / U    undo last / all replacements
;;   ^        back to previous match    E        edit the replacement string
;;   q / RET  stop (any other key stops too)    ? help
;;   Y / N    (project replace) replace everything in all remaining files /
;;            skip the rest of this file
;;
;; The current match is selected (region highlight) and every other match is
;; highlighted through the search-highlight layer. A whole session is ONE undo
;; step (undo-amalgamate-begin/-end).
;;
;; Offsets: matches come from `buffer-search-all` as BYTE offsets, and every
;; position the session computes is in bytes (`utf8-length` for replacement
;; lengths) — never a char index — so multibyte text stays intact. The session
;; never re-reads the buffer: it tracks how far each replacement shifted the
;; later matches itself, so keys typed faster than the editor applies edits
;; still land correctly.

;; --- replacement text ----------------------------------------------------------
(define (qr-digit-value ch)
  (let ((n (- (char->integer ch) 48)))
    (if (and (>= n 0) (<= n 9)) n #f)))

;; Push the chars of `s` onto the reversed char list `acc`.
(define (qr-push-chars s acc)
  (let loop ((cs (string->list s)) (acc acc))
    (if (null? cs) acc (loop (cdr cs) (cons (car cs) acc)))))

(define (qr-group groups n)
  (if (< n (length groups)) (list-ref groups n) ""))

;; Expand Emacs replacement syntax against `groups` ("whole" "g1" "g2" …):
;; `\&` the whole match, `\N` group N (an unmatched/missing group is ""),
;; `\\` a backslash, `\n` / `\t` a newline / tab. Any other `\x` stays as is.
;; Walks chars, so multibyte text is safe.
(define (qr-expand-replacement rep groups)
  (let loop ((cs (string->list rep)) (acc '()))
    (cond
      ((null? cs) (list->string (reverse acc)))
      ((and (char=? (car cs) #\\) (not (null? (cdr cs))))
       (let* ((c (cadr cs)) (d (qr-digit-value c)))
         (cond
           ((char=? c #\&) (loop (cddr cs) (qr-push-chars (qr-group groups 0) acc)))
           (d (loop (cddr cs) (qr-push-chars (qr-group groups d) acc)))
           ((char=? c #\\) (loop (cddr cs) (cons #\\ acc)))
           ((char=? c #\n) (loop (cddr cs) (cons #\newline acc)))
           ((char=? c #\t) (loop (cddr cs) (cons #\tab acc)))
           (else (loop (cddr cs) (cons c (cons #\\ acc)))))))
      (else (loop (cdr cs) (cons (car cs) acc))))))

;; --- case handling (Emacs case-fold-search + case-replace) ------------------------
(define (qr-has-upper? s) (not (equal? s (string-downcase s))))

(define (qr-char-cased? ch)
  (let ((s (list->string (list ch))))
    (not (equal? (string-upcase s) (string-downcase s)))))

(define (qr-char-upper? ch)
  (let ((s (list->string (list ch))))
    (and (qr-char-cased? ch) (equal? s (string-upcase s)))))

(define (qr-capitalize s)
  (if (equal? s "")
      s
      (let ((cs (string->list s)))
        (string-append (string-upcase (list->string (list (car cs))))
                       (list->string (cdr cs))))))

;; Give `rep` the case pattern of `match`: ALL CAPS (two or more cased letters,
;; all upper) upcases it, an initial capital capitalizes it, else unchanged.
(define (qr-case-adjust rep match)
  (let ((cased (filter qr-char-cased? (string->list match))))
    (cond ((null? cased) rep)
          ((and (> (length cased) 1) (equal? match (string-upcase match))) (string-upcase rep))
          ((qr-char-upper? (car (string->list match))) (qr-capitalize rep))
          (else rep))))

;; Smart case: a FROM without upper-case letters matches case-insensitively.
(define (qr-fold? from) (not (qr-has-upper? from)))

(define (qr-pattern from regexp? fold?)
  (let ((base (if regexp? from (regexp-quote from))))
    (if fold? (string-append "(?i)" base) base)))

;; The text replacing match record `m` = (start end whole g1 …).
(define (qr-replacement-for m to regexp? fold?)
  (let ((rep (if regexp? (qr-expand-replacement to (cddr m)) to)))
    (if (and fold? (not (qr-has-upper? to)))
        (qr-case-adjust rep (list-ref m 2))
        rep)))

;; --- the query session ----------------------------------------------------------
(define *qr-active?* #f)
(define *qr-buffer* "")
(define *qr-from* "")
(define *qr-to* "")
(define *qr-regexp?* #f)
(define *qr-fold?* #f)
(define *qr-matches* (list->vector '()))  ; match records, ORIGINAL byte offsets
(define *qr-n* 0)
(define *qr-reps* (list->vector '()))     ; per match: #f or (replacement . byte-delta)
(define *qr-done* '())                    ; replaced indices, newest first (undo order)
(define *qr-idx* 0)                       ; current match
(define *qr-history* '())                 ; previously visited indices (for ^)
(define *qr-count* 0)                     ; replacements made in this session
(define *qr-point* 0)                     ; where point goes when the session ends
(define *qr-on-done* #f)                  ; procedure run after the session (project replace)
(define *qr-multi?* #f)                   ; a project (multi-file) session: Y / N keys
(define *qr-all-files?* #f)               ; Y pressed: replace the rest without asking
(define *qr-quit-all?* #f)                ; q / RET / …: stop the whole (multi-file) run
(define *qr-exit-visual?* #f)             ; started from evil Visual: back to Normal at the end

(define (qr-match i) (vector-ref *qr-matches* i))
(define (qr-rep i) (vector-ref *qr-reps* i))
(define (qr-orig-len i) (let ((m (qr-match i))) (- (cadr m) (car m))))

;; Bytes the replacements made before match `i` shifted it by.
(define (qr-delta-before i)
  (let loop ((ds *qr-done*) (acc 0))
    (if (null? ds)
        acc
        (loop (cdr ds) (if (< (car ds) i) (+ acc (cdr (qr-rep (car ds)))) acc)))))

;; Current byte position of match `i`.
(define (qr-pos i) (+ (car (qr-match i)) (qr-delta-before i)))

;; Start a session over bytes [beg, end) of the active buffer. `on-done` (a
;; procedure or #f) runs after it ends; `multi?` enables the project keys;
;; `auto?` replaces everything without asking.
(define (qr-start from to regexp? beg end on-done multi? auto?)
  (let* ((fold? (qr-fold? from))
         (pat (qr-pattern from regexp? fold?))
         (ms (buffer-search-all pat beg end)))
    (set! *qr-on-done* on-done)
    (cond
      ((string? ms)
       (set! *qr-quit-all?* #t)
       (set! *qr-count* 0)
       (set! *qr-exit-visual?* #f)
       (qr-run-on-done (string-append "Invalid regexp: " ms)))
      ((null? ms)
       (set! *qr-count* 0)
       (set! *qr-exit-visual?* #f)
       (qr-run-on-done (string-append "No matches for \"" from "\"")))
      (else
       (set! *qr-active?* #t)
       (set! *qr-buffer* (current-buffer))
       (set! *qr-from* from)
       (set! *qr-to* to)
       (set! *qr-regexp?* regexp?)
       (set! *qr-fold?* fold?)
       (set! *qr-matches* (list->vector ms))
       (set! *qr-n* (length ms))
       (set! *qr-reps* (make-vector (length ms) #f))
       (set! *qr-done* '())
       (set! *qr-idx* 0)
       (set! *qr-history* '())
       (set! *qr-count* 0)
       (set! *qr-point* (car (car ms)))
       (set! *qr-multi?* multi?)
       (undo-amalgamate-begin)
       (if auto?
           (begin (qr-replace-rest!) (qr-finish #f))
           (begin (set-search-query pat #t) (qr-show "")))))))

;; Message, then hand over to the session's continuation (if any).
(define (qr-run-on-done note)
  (message note)
  (let ((k *qr-on-done*))
    (set! *qr-on-done* #f)
    (when k (k))))

(define (qr-prompt)
  (string-append (if *qr-regexp?* "Query replacing regexp " "Query replacing ")
                 *qr-from* " with " *qr-to*
                 " (" (int->string (+ *qr-idx* 1)) "/" (int->string *qr-n*) "): (? for help)"))

;; Select the current match (or its replacement, after `,`) and wait for a key.
;; `note` is shown ahead of the prompt.
(define (qr-show note)
  (if (>= *qr-idx* *qr-n*)
      (qr-finish #f)
      (let* ((i *qr-idx*) (pos (qr-pos i)) (r (qr-rep i))
             (end (+ pos (if r (utf8-length (car r)) (qr-orig-len i)))))
        (set! *qr-point* end)
        (set-region pos end)
        (read-key-start (string-append note (if (equal? note "") "" "  ") (qr-prompt))
                        'qr-key))))

;; Replace match `i` (no-op when already replaced).
(define (qr-replace! i)
  (when (not (qr-rep i))
    (let* ((m (qr-match i))
           (rep (qr-replacement-for m *qr-to* *qr-regexp?* *qr-fold?*))
           (pos (qr-pos i))
           (len (qr-orig-len i))
           (rl (utf8-length rep)))
      (replace-region pos (+ pos len) rep)
      (vector-set! *qr-reps* i (cons rep (- rl len)))
      (set! *qr-done* (cons i *qr-done*))
      (set! *qr-count* (+ *qr-count* 1))
      (set! *qr-point* (+ pos rl)))))

;; Replace every not-yet-replaced match from the current one on, in ONE pass
;; with a running byte delta (O(n), unlike n calls of qr-pos).
(define (qr-replace-rest!)
  (let loop ((i *qr-idx*) (delta (qr-delta-before *qr-idx*)))
    (when (< i *qr-n*)
      (let ((r (qr-rep i)) (m (qr-match i)))
        (if r
            (loop (+ i 1) (+ delta (cdr r)))
            (let* ((rep (qr-replacement-for m *qr-to* *qr-regexp?* *qr-fold?*))
                   (pos (+ (car m) delta))
                   (len (- (cadr m) (car m)))
                   (d (- (utf8-length rep) len)))
              (replace-region pos (+ pos len) rep)
              (vector-set! *qr-reps* i (cons rep d))
              (set! *qr-done* (cons i *qr-done*))
              (set! *qr-count* (+ *qr-count* 1))
              (set! *qr-point* (+ pos len d))
              (loop (+ i 1) (+ delta d))))))))

;; The next match after the current one that hasn't been replaced (or n).
(define (qr-next-index i)
  (let loop ((j (+ i 1)))
    (cond ((>= j *qr-n*) *qr-n*)
          ((qr-rep j) (loop (+ j 1)))
          (else j))))

(define (qr-advance)
  (set! *qr-history* (cons *qr-idx* *qr-history*))
  (set! *qr-idx* (qr-next-index *qr-idx*))
  (qr-show ""))

;; u: put the most recent replacement back and return to that match.
(define (qr-undo-last)
  (if (null? *qr-done*)
      (qr-show "Nothing to undo.")
      (let* ((i (car *qr-done*)) (r (qr-rep i)))
        (set! *qr-done* (cdr *qr-done*))
        (vector-set! *qr-reps* i #f)
        (let ((pos (qr-pos i)))
          (replace-region pos (+ pos (utf8-length (car r))) (list-ref (qr-match i) 2)))
        (set! *qr-count* (- *qr-count* 1))
        (set! *qr-idx* i)
        (qr-show "Undid one replacement."))))

;; U: undo every replacement of this session, back to the first match.
(define (qr-undo-all)
  (if (null? *qr-done*)
      (qr-show "Nothing to undo.")
      (begin
        (let loop ()
          (when (not (null? *qr-done*))
            (let* ((i (car *qr-done*)) (r (qr-rep i)))
              (set! *qr-done* (cdr *qr-done*))
              (vector-set! *qr-reps* i #f)
              (let ((pos (qr-pos i)))
                (replace-region pos (+ pos (utf8-length (car r))) (list-ref (qr-match i) 2))))
            (loop)))
        (set! *qr-count* 0)
        (set! *qr-idx* 0)
        (set! *qr-history* '())
        (qr-show "Undid all replacements."))))

;; ^: back to the previously visited match.
(define (qr-back)
  (if (null? *qr-history*)
      (qr-show "No previous match.")
      (begin (set! *qr-idx* (car *qr-history*))
             (set! *qr-history* (cdr *qr-history*))
             (qr-show ""))))

;; E: edit the replacement string; the current match is then replaced with it
;; and the session continues (the new string applies to later matches too).
(define (qr-edit)
  (read-string "Edit replacement string: " 'qr-edit-done *qr-to*))
(define (qr-edit-done s)
  (when *qr-active?*
    (set! *qr-to* s)
    (set! *qr-defaults* (cons *qr-from* s))
    (qr-replace! *qr-idx*)
    (qr-advance)))

(define (qr-help)
  (string-append
   "y/SPC replace, n/DEL skip, ! all, . this & stop, , replace & stay,"
   " u/U undo last/all, ^ back, E edit replacement, q/RET stop"
   (if *qr-multi?* ", Y all files, N next file" "")
   "."))

;; The read-key callback: one keystroke of the session.
(define (qr-key k)
  (cond
    ((not *qr-active?*) (read-key-stop))
    ((not (equal? (current-buffer) *qr-buffer*))
     (set! *qr-quit-all?* #t)
     (qr-finish "Query replace stopped: the buffer changed."))
    ((or (equal? k "y") (equal? k " ")) (qr-replace! *qr-idx*) (qr-advance))
    ((member k (list "n" "backspace" "delete")) (qr-advance))
    ((equal? k "!") (qr-replace-rest!) (qr-finish #f))
    ((equal? k ".") (qr-replace! *qr-idx*) (qr-quit))
    ((equal? k ",") (qr-replace! *qr-idx*) (qr-show ""))
    ((equal? k "u") (qr-undo-last))
    ((equal? k "U") (qr-undo-all))
    ((equal? k "^") (qr-back))
    ((equal? k "E") (qr-edit))
    ((and *qr-multi?* (equal? k "Y"))
     (set! *qr-all-files?* #t)
     (qr-replace-rest!)
     (qr-finish #f))
    ((and *qr-multi?* (equal? k "N")) (qr-finish #f))
    ((member k (list "?" "h" "f1")) (qr-show (qr-help)))
    ;; q, RET, Escape, C-g — and like Emacs, any other key — end the session.
    (else (qr-quit))))

(define (qr-quit)
  (set! *qr-quit-all?* #t)
  (qr-finish #f))

(define (qr-summary)
  (string-append "Replaced " (int->string *qr-count*) " occurrence"
                 (if (= *qr-count* 1) "" "s")))

;; End the session: release the key capture and the match highlight, leave
;; point after the last match handled, close the undo group.
(define (qr-finish note)
  (read-key-stop)
  (set! *qr-active?* #f)
  (set-search-query "" #t)
  (when *qr-exit-visual?*
    (set! *qr-exit-visual?* #f)
    (evil-normal-state))
  (goto-char *qr-point*)
  (undo-amalgamate-end)
  (qr-run-on-done (or note (qr-summary))))

;; Replace every match in [beg, end) at once (replace-string / replace-regexp),
;; in ascending order with a running byte delta. One command = one undo step.
(define (qr-replace-all-in from to regexp? beg end)
  (let* ((fold? (qr-fold? from))
         (ms (buffer-search-all (qr-pattern from regexp? fold?) beg end)))
    (if (string? ms)
        (message (string-append "Invalid regexp: " ms))
        (let loop ((ms ms) (delta 0) (count 0) (last -1))
          (if (null? ms)
              (begin
                (when (>= last 0) (goto-char last))
                (message (string-append "Replaced " (int->string count) " occurrence"
                                        (if (= count 1) "" "s"))))
              (let* ((m (car ms))
                     (rep (qr-replacement-for m to regexp? fold?))
                     (pos (+ (car m) delta))
                     (len (- (cadr m) (car m)))
                     (rl (utf8-length rep)))
                (replace-region pos (+ pos len) rep)
                (loop (cdr ms) (+ delta (- rl len)) (+ count 1) (+ pos rl))))))))

;; --- prompting -------------------------------------------------------------------
(define *qr-defaults* #f)   ; (from . to) of the last replace, for an empty FROM
(define *qr-kind* 'query)
(define *qr-beg* 0)
(define *qr-end* 0)
(define *qr-pending-from* "")

(define (qr-kind-label kind)
  (cond ((equal? kind 'query) "Query replace")
        ((equal? kind 'query-regexp) "Query replace regexp")
        ((equal? kind 'string) "Replace string")
        ((equal? kind 'regexp) "Replace regexp")
        ((equal? kind 'project) "Project query replace")
        (else "Project query replace regexp")))

(define (qr-kind-regexp? kind)
  (if (member kind '(query-regexp regexp project-regexp)) #t #f))

;; Capture the scope (region, else point..end) now — before the minibuffer
;; opens — then read FROM.
(define (qr-read kind)
  (set! *qr-kind* kind)
  (if (region-active?)
      (begin (set! *qr-beg* (region-beginning)) (set! *qr-end* (region-end)))
      (begin (set! *qr-beg* (point)) (set! *qr-end* (point-max))))
  (set! *qr-exit-visual?* (and (region-active?) (evil-enabled?) (not (helix-enabled?))))
  (read-string (string-append (qr-kind-label kind)
                              (if *qr-defaults*
                                  (string-append " (default " (car *qr-defaults*)
                                                 " -> " (cdr *qr-defaults*) ")")
                                  "")
                              ": ")
               'qr-read-from))

(define (qr-read-from s)
  (cond ((and (equal? s "") *qr-defaults*)
         (qr-dispatch (car *qr-defaults*) (cdr *qr-defaults*)))
        ((equal? s "") (message "Nothing to replace"))
        (else (set! *qr-pending-from* s)
              (read-string (string-append (qr-kind-label *qr-kind*) " " s " with: ")
                           'qr-read-to))))

(define (qr-read-to s)
  (set! *qr-defaults* (cons *qr-pending-from* s))
  (qr-dispatch *qr-pending-from* s))

(define (qr-dispatch from to)
  (let* ((kind *qr-kind*) (rx (qr-kind-regexp? kind)))
    (set! *qr-all-files?* #f)
    (set! *qr-quit-all?* #f)
    (cond ((member kind '(query query-regexp))
           (qr-start from to rx *qr-beg* *qr-end* #f #f #f))
          ((member kind '(string regexp))
           (set! *qr-exit-visual?* #f)
           (qr-replace-all-in from to rx *qr-beg* *qr-end*))
          (else
           (set! *qr-exit-visual?* #f)
           (pqr-start from to rx)))))

(define-command (query-replace) (qr-read 'query))
(define-command (query-replace-regexp) (qr-read 'query-regexp))
(define-command (replace-string) (qr-read 'string))
(define-command (replace-regexp) (qr-read 'regexp))

;; --- project-wide query-replace --------------------------------------------------
;; ripgrep (`grep-files`, gitignore-aware) finds the project files containing a
;; match; each is then visited in turn for a query session over the whole file.
;; `!` finishes a file, `Y` does all remaining files without asking, `N` skips
;; to the next file, `q` stops. At the end you're asked whether to save the
;; files that changed (unsaved ones stay modified in their buffers).
(define *pqr-files* '())
(define *pqr-current* "")
(define *pqr-tries* 0)
(define *pqr-from* "")
(define *pqr-to* "")
(define *pqr-regexp?* #f)
(define *pqr-touched* '())
(define *pqr-total* 0)

;; The distinct file paths of `path:line:text` grep hits, in order.
(define (qr-unique-paths hits)
  (let loop ((hs hits) (seen (hash)) (acc '()))
    (if (null? hs)
        (reverse acc)
        (let ((p (let ((parts (grep-hit-parts (car hs))))
                   (if parts (car parts) (car hs)))))
          (if (hash-contains? seen p)
              (loop (cdr hs) seen acc)
              (loop (cdr hs) (hash-insert seen p #t) (cons p acc)))))))

(define (pqr-start from to regexp?)
  (let ((root (project-root (buffer-directory))))
    (if (not root)
        (message "Not in a project")
        (let* ((pat (qr-pattern from regexp? (qr-fold? from)))
               (check (buffer-search-all pat 0 0)))
          (if (string? check)
              (message (string-append "Invalid regexp: " check))
              (let ((files (qr-unique-paths (grep-files root pat))))
                (if (null? files)
                    (message (string-append "No matches for \"" from "\" in " root))
                    (begin
                      (set! *pqr-files* files)
                      (set! *pqr-from* from)
                      (set! *pqr-to* to)
                      (set! *pqr-regexp?* regexp?)
                      (set! *pqr-touched* '())
                      (set! *pqr-total* 0)
                      (pqr-next)))))))))

;; Visit the next file. Opening an unvisited file is asynchronous, so the
;; session starts from a timer that waits for it to become the active buffer.
(define (pqr-next)
  (if (or *qr-quit-all?* (null? *pqr-files*))
      (pqr-finish)
      (begin
        (set! *pqr-current* (car *pqr-files*))
        (set! *pqr-files* (cdr *pqr-files*))
        (set! *pqr-tries* 0)
        (open-file *pqr-current*)
        (run-with-timer 0 'pqr-visit))))

(define (pqr-visit)
  (cond
    ;; The same file however it's spelled (grep's path vs the opened buffer's
    ;; may differ by a symlink / `/private` prefix).
    ((same-file? (buffer-file-name) *pqr-current*)
     (message (string-append "Replacing in " *pqr-current*))
     (qr-start *pqr-from* *pqr-to* *pqr-regexp?* 0 (point-max) pqr-file-done #t *qr-all-files?*))
    ((< *pqr-tries* 150)
     (set! *pqr-tries* (+ *pqr-tries* 1))
     (run-with-timer 20 'pqr-visit))
    (else
     (message (string-append "Could not open " *pqr-current* "; skipped"))
     (pqr-next))))

(define (pqr-file-done)
  (when (> *qr-count* 0)
    (set! *pqr-touched* (cons *pqr-current* *pqr-touched*))
    (set! *pqr-total* (+ *pqr-total* *qr-count*)))
  (pqr-next))

(define (pqr-finish)
  (let ((n (length *pqr-touched*))
        (total (string-append "Replaced " (int->string *pqr-total*) " occurrence"
                              (if (= *pqr-total* 1) "" "s"))))
    (if (= n 0)
        (message (string-append total "; no files changed"))
        (y-or-n-p (string-append total " in " (int->string n) " file" (if (= n 1) "" "s")
                                 ". Save " (if (= n 1) "it" "them")
                                 "? (n leaves the buffers modified)")
                  'pqr-save))))

;; Save every file the run changed, then come back to the current buffer.
(define (pqr-save ans)
  (when (equal? ans "yes")
    (let ((here (current-buffer)))
      (for-each (lambda (p) (open-file p) (save-buffer)) (reverse *pqr-touched*))
      (switch-to-buffer here)
      (message (string-append "Saved " (int->string (length *pqr-touched*)) " file"
                              (if (= (length *pqr-touched*) 1) "" "s"))))))

(define-command (project-query-replace) (qr-read 'project))
(define-command (project-query-replace-regexp) (qr-read 'project-regexp))
(define-command (projectile-replace) (qr-read 'project))
(define-command (projectile-replace-regexp) (qr-read 'project-regexp))

;; --- keymap ------------------------------------------------------------------------
;; M-% / C-M-% fall through the evil/helix keymaps (unbound Alt chords reach the
;; Emacs map), so modal users get them too.
(define-key "fundamental" "alt-%" 'query-replace)
(define-key "fundamental" "ctrl-alt-%" 'query-replace-regexp)
(define-key "fundamental" "ctrl-c p r" 'projectile-replace)
(define-key "fundamental" "ctrl-c p %" 'projectile-replace-regexp)
