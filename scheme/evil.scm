;; evil-mode — Vim-style modal editing, built on Macros' keymaps. The Rust side
;; only decides *when* plain keys are commands (Normal/Visual) vs. text (Insert);
;; every binding below is Scheme and fully overridable. Emacs keybindings are
;; the default; opt in to modal editing with (evil-mode) in your config (and
;; (evil-disable) to turn it back off). This mirrors GNU Emacs's evil-mode:
;; plain Vim modal editing, no leader key. Add your own leader in init.scm if
;; you want a Doom/Spacemacs-style menu.

;; --- composite operators (commands enqueue in order when a fn is called) ----
;; a — insert after the char under the cursor, but never past the line end: on
;; an empty line `forward-char` would step over the break onto the next line.
(define (evil-append)
  (when (< (point) (line-end-pos (point))) (forward-char))
  (evil-insert-state))
(define (evil-append-line) (move-end-of-line) (evil-insert-state))
(define (evil-insert-line) (evil-first-non-blank) (evil-insert-state)) ; I — first non-blank
(define (evil-open-below)  (move-end-of-line) (newline) (evil-insert-state))
(define (evil-open-above)  (move-beginning-of-line) (open-line) (evil-insert-state))
;; The typed count for a count-aware command (see `set-count-aware!` below),
;; at least 1. `(prefix-count)` itself is 0 when no count was typed.
(define (evil-count) (max 1 (prefix-count)))

;; The last line a linewise op can reach: a buffer ending in "\n" has a phantom
;; empty line after it that Vim doesn't count.
(define (evil-last-line)
  (let ((mx (point-max)) (n (line-count)))
    (if (and (> mx 0) (equal? (buffer-substring (- mx 1) mx) "\n") (> n 1))
        (- n 2)
        (- n 1))))

;; The `n` whole lines from point's line down (clamped at the last line), as
;; (start end eof?): `end` is past the last line's newline, or — when that line
;; is the buffer's last and has no newline — the buffer end, with eof? #t.
(define (evil-lines-range n)
  (let* ((p (point))
         (ln2 (min (evil-last-line) (+ (line-of-pos p) (- n 1))))
         (s (line-start-pos p))
         (le (line-end-pos (pos-at-char-column ln2 0))))
    (if (< le (point-max))
        (list s (+ le 1) #f)
        (list s le #t))))

;; dd / 3dd — delete whole lines linewise (with their newlines) into the
;; register. On the buffer's last line (no newline after it) the newline BEFORE
;; the lines goes instead, so no phantom empty line is left behind; the
;; register still gets plain "line" text and linewise paste adds the break.
(define (evil-delete-line)
  (let ((r (evil-lines-range (evil-count))))
    (evil-delete-lines (car r) (cadr r))))
;; Visual-mode delete/yank are character-wise (the selection's exact range).
(define (evil-visual-delete) (kill-region) (evil-set-charwise) (evil-normal-state))
(define (evil-visual-yank)
  ;; Vim parks the cursor at the START of the yanked text, not at the end where
  ;; the motion left it — `goto-char` before leaving visual state (the region is
  ;; read from the pre-command snapshot, so `region-beginning` is still live).
  (let ((rb (region-beginning)))
    (kill-ring-save) (evil-set-charwise)
    (when (>= rb 0) (goto-char rb)))
  (evil-normal-state))
;; Visual-LINE delete/yank are line-wise: the visual region already covers whole
;; lines (incl. the trailing newline), so a following `p`/`P` pastes the vim way.
;; On the buffer's last line (no newline after it) the region stops at the
;; buffer end; drop the newline BEFORE it too, like `dd` there, so no empty
;; line is left behind.
(define (evil-visual-line-eof-start)   ; region start when the region hits EOF, else #f
  (let ((rb (region-beginning)) (re (region-end)) (mx (point-max)))
    (and (>= rb 0) (= re mx) (> rb 0)
         (not (equal? (buffer-substring (- re 1) re) "\n"))
         rb)))
(define (evil-visual-line-delete)
  (let ((eof (evil-visual-line-eof-start)))
    (kill-region) (evil-set-linewise)
    (when eof (delete-region (- eof 1) eof)))
  (evil-normal-state))
(define (evil-visual-line-yank)
  (let ((rb (region-beginning)))
    (kill-ring-save) (evil-set-linewise)
    (when (>= rb 0) (goto-char rb)))
  (evil-normal-state))
;; Normal-mode yanks (copy → kill ring AND the system clipboard). `kill-ring-save`
;; syncs the OS clipboard, so these reach cmd-v in other apps. Cursor is restored
;; to where the yank started, vim-style. Each marks the yank line- or char-wise
;; so a following `p`/`P` pastes the vim way.
(define (evil-yank-line)
  ;; yy / 3yy — line-wise: grab whole lines *including* their trailing newline,
  ;; so `p` drops them on fresh lines below. The cursor stays put (Vim).
  (let* ((p (point)) (r (evil-lines-range (evil-count))))
    (set-region (car r) (cadr r)) (kill-ring-save) (evil-set-linewise)
    (goto-char p)))
(define (evil-yank-to-eol)
  (let ((p (point)))
    (set-mark) (move-end-of-line) (kill-ring-save) (evil-set-charwise)
    (goto-char p)))
(define (evil-yank-word)
  ;; yw — character-wise, using the vim `w` motion (to the start of the next
  ;; word) so the trailing whitespace is included, matching `w`.
  (let ((p (point)) (e (evil-word-op-end (point))))
    (when (< p e)
      (set-region p e) (kill-ring-save) (evil-set-charwise))
    (goto-char p)))

;; --- more operators & motions (the rest of the common Vim verbs) -------------
;; NOTE on the snapshot model: a command script reads buffer state (point,
;; line-bounds, buffer-substring, …) from a snapshot taken BEFORE it runs; the
;; motion/edit commands it calls are queued and applied afterwards. So a helper
;; must compute every position from synchronous reads up front — it can't move
;; then read the new point. `goto-char` also COLLAPSES any selection (it's
;; move-to), so the helpers that use it are bound in normal state only; visual
;; motions go through the extend-aware command procs (backward-char, forward-word…).

;; ^ — first non-blank of the current line. Where `^` and `I` land (Vim leaves
;; the cursor on the first non-blank, not column 0).
(define (evil-skip-blanks-to from end)   ; first non-blank byte in [from end), else end
  (let loop ((i from))
    (if (and (< i end)
             (let ((c (buffer-substring i (+ i 1)))) (or (equal? c " ") (equal? c "\t"))))
        (loop (+ i 1))
        i)))
(define (evil-first-non-blank)
  (let* ((b (line-bounds)) (s (car b)) (e (cadr b)))
    (goto-char (evil-skip-blanks-to s e))))

;; gg / G — first / last line, on its first non-blank. Scanned directly: point
;; reads are a pre-command snapshot, so we can't jump-then-read-the-new-line.
;; `5G` / `5gg` — line 5 (1-based), on its first non-blank.
(define (evil-goto-line-count n)
  (let* ((ln (min (evil-last-line) (max 0 (- n 1))))
         (s (pos-at-char-column ln 0)))
    (goto-char (evil-skip-blanks-to s (line-end-pos s)))))
(define (evil-goto-first-line)
  (if (> (prefix-count) 0) (evil-goto-line-count (prefix-count)) (evil-goto-first-line-0)))
(define (evil-goto-last-line)
  (if (> (prefix-count) 0) (evil-goto-line-count (prefix-count)) (evil-goto-last-line-0)))
(define (evil-goto-first-line-0)
  (let* ((mx (point-max))
         (end (let loop ((i 0))
                (cond ((>= i mx) mx)
                      ((equal? (buffer-substring i (+ i 1)) "\n") i)
                      (else (loop (+ i 1)))))))
    (goto-char (evil-skip-blanks-to 0 end))))
(define (evil-goto-last-line-0)
  (let* ((mx (point-max))
         ;; ignore one trailing newline so a file ending in "\n" doesn't land us
         ;; on a phantom empty line below the text.
         (end (if (and (> mx 0) (equal? (buffer-substring (- mx 1) mx) "\n")) (- mx 1) mx))
         (start (let loop ((i end))
                  (cond ((<= i 0) 0)
                        ((equal? (buffer-substring (- i 1) i) "\n") i)
                        (else (loop (- i 1)))))))
    (goto-char (evil-skip-blanks-to start end))))

;; e — end of word: land ON the last char of the current/next word. Backed by
;; the class-aware `forward-word-end` primitive (Vim `e`), NOT the old
;; forward-char + Emacs `forward-word` + backward-char dance: Emacs's motion
;; treats punctuation as a separator, so that form walked straight past a `);`
;; run instead of stopping on it, and `e` disagreed with `w` about where words
;; are. Kept as a wrapper so the visual maps can bind the same name.
(define (evil-forward-word-end) (forward-word-end))

;; x — delete the char under the cursor INTO the register (Vim `x` = `dl`), so a
;; following `p` can put it back. Char-wise; a no-op on an empty line.
(define (evil-delete-char)
  ;; 3x deletes up to 3 chars, never past the line end.
  (let* ((b (line-bounds)) (eol (cadr b)) (p (point))
         (e (let loop ((i p) (n (evil-count)))
              (if (or (<= n 0) (>= i eol)) i (loop (grapheme-forward i) (- n 1))))))
    (when (< p e)
      (kill-region-between p e) (evil-set-charwise))))

;; D / d$ — delete to end of line.  C / c$ — change to end of line.
(define (evil-delete-to-eol)
  (set-mark) (move-end-of-line) (kill-region) (evil-set-charwise))
(define (evil-change-to-eol)
  (set-mark) (move-end-of-line) (kill-region) (evil-insert-state))

;; cc / S / 3cc — change whole lines: clear their text (keeping one line) and
;; insert on it.
(define (evil-change-line)
  (let* ((r (evil-lines-range (evil-count))) (s (car r))
         (e (if (list-ref r 2) (cadr r) (- (cadr r) 1))))
    (when (< s e) (kill-region-between s e) (evil-set-linewise))
    (goto-char s)
    (evil-insert-state)))

;; s — substitute the char under the cursor (delete it, then insert).
(define (evil-substitute-char) (evil-delete-char) (evil-insert-state))

;; dw / db, cw / cb — word operators. `dw` runs to the next word start (Vim `w`,
;; trailing space included); `cw` stops at the word end (Vim treats `cw` as `ce`).
;; Where a `w` operator stops: the next word start, but clamped to the line end
;; when that start is on a later line (Vim: `dw` on a line's last word deletes
;; to the end of the line; it never joins the next one).
(define (evil-word-op-end p)
  (let ((nx (word-forward-start p)) (le (line-end-pos p)))
    (if (and (> nx le) (< p le)) le nx)))
(define (evil-delete-word)
  (let ((p (point)))
    (kill-region-between p (evil-word-op-end p)) (evil-set-charwise)))
(define (evil-delete-word-back) (set-mark) (backward-word-start) (kill-region) (evil-set-charwise))
;; cw — Vim treats it as `ce`, but from INSIDE a word it stops at that word's
;; end (on its last char, `ce` would jump to the next word). So: kill to the end
;; of the char run under point (word, punctuation, or blanks).
(define (evil-change-word)
  (let ((p (point)) (r (evil-object-word #t)))
    (when (and r (< p (cdr r)))
      (kill-region-between p (cdr r)) (evil-set-charwise))
    (evil-insert-state)))
(define (evil-change-word-back) (set-mark) (backward-word-start) (kill-region) (evil-insert-state))

;; ~ — toggle the case of the char under the cursor; the replace leaves the
;; cursor on the next char, matching Vim's advance. Only cased letters change.
(define (evil-toggle-case)
  (let* ((b (line-bounds)) (eol (cadr b)) (p (point)))
    (when (< p eol)
      (let* ((c (buffer-substring p (+ p 1))) (up (string-upcase c)) (dn (string-downcase c)))
        (when (not (equal? up dn))
          (replace-region p (+ p 1) (if (equal? c up) dn up)))))))

;; J — join the next line onto this one, collapsing its leading indentation to a
;; single space (Vim `J`). Kept self-contained here (evil.scm must not depend on
;; editing.scm: load order isn't guaranteed and the tests load evil.scm alone).
;; With a count, `3J` joins 3 lines (2 joins). Like Vim, no space is added when
;; the next line is empty (after its indent), starts with `)`, or the line
;; already ends in a blank. Edits run bottom-up so earlier offsets stay valid.
(define (evil-join-sep e i mx)
  (let ((next (if (< i mx) (buffer-substring i (+ i 1)) "\n")))
    (if (or (equal? next "\n") (equal? next ")")
            (and (> e (line-start-pos e)) (evil-blank-pos? (- e 1))))
        ""
        " ")))
(define (evil-join-line)
  (let* ((mx (point-max))
         (joins (max 1 (- (evil-count) 1)))
         ;; each join, top-down: (eol first-non-blank-of-next-line separator)
         (spots (let loop ((pos (point)) (k joins) (acc '()))
                  (let ((e (line-end-pos pos)))
                    (if (or (<= k 0) (>= e mx))
                        (reverse acc)
                        (let ((i (let skip ((i (+ e 1)))
                                   (if (and (< i mx) (evil-blank-pos? i)) (skip (+ i 1)) i))))
                          (loop (+ e 1) (- k 1)
                                (cons (list e i (evil-join-sep e i mx)) acc))))))))
    (when (not (null? spots))
      (for-each (lambda (sp) (replace-region (car sp) (cadr sp) (list-ref sp 2)))
                (reverse spots))
      ;; Vim leaves the cursor at the last join point: its eol, shifted left by
      ;; what the earlier joins removed.
      (let loop ((ss spots) (removed 0))
        (let ((sp (car ss)))
          (if (null? (cdr ss))
              (goto-char (- (car sp) removed))
              (loop (cdr ss)
                    (+ removed (- (- (cadr sp) (car sp))
                                (string-length (list-ref sp 2)))))))))))

;; gq / gw — Vim's format operator: re-wrap text at *fill-column* (80 by
;; default), continuing indentation and comment leaders. The wrap engine
;; (fill-text) and paragraph-bounds live in core.scm — loaded before this file,
;; and by the tests too. The evil layer binds concrete chords, so the common
;; forms are bound below: gqq / gqgq (current line), gqip / gqap (paragraph at
;; point), gq on a visual selection; gww is gqq with the cursor kept in place.
;; Snapshot model: read the range, wrap it, issue ONE replace-region.
(define (evil-fill-range s e keep-point)
  (when (< s e)
    (let ((new (fill-text (buffer-substring s e) *fill-column*)))
      (replace-region s e new)
      ;; gq leaves the cursor at the start of the formatted text; gw clamps the
      ;; old point into the new text (Vim restores it exactly).
      (goto-char (if keep-point (min (point) (+ s (utf8-length new))) s)))))
(define (evil-fill-line)      (let ((b (line-bounds))) (evil-fill-range (car b) (cadr b) #f)))
(define (evil-fill-line-keep) (let ((b (line-bounds))) (evil-fill-range (car b) (cadr b) #t)))
(define (evil-fill-paragraph)
  (let ((b (paragraph-bounds (point))))
    (when b (evil-fill-range (car b) (cadr b) #f))))
;; Visual gq: format the selected lines (the selection is rounded out to whole
;; lines; region-end is exclusive, so end-1 finds the last selected line).
(define (evil-visual-fill)
  (let ((rb (region-beginning)) (re (region-end)))
    (when (and (>= rb 0) (< rb re))
      (evil-fill-range (line-start-pos rb) (line-end-pos (max rb (- re 1))) #f)))
  (evil-normal-state))

;; --- paragraph motions ({ / }) ------------------------------------------------
;; Boundary positions live in core.scm (shared with Emacs M-{ / M-}). The
;; visual variants extend via `select-to` (goto-char would collapse).
(define (evil-forward-paragraph)         (goto-char  (paragraph-forward-pos (point))))
(define (evil-backward-paragraph)        (goto-char  (paragraph-backward-pos (point))))
(define (evil-forward-paragraph-visual)  (select-to  (paragraph-forward-pos (point))))
(define (evil-backward-paragraph-visual) (select-to  (paragraph-backward-pos (point))))

;; --- gJ and line case operators ------------------------------------------------
;; gJ — join the next line up with NO space inserted (vim gJ): remove just the
;; newline, keeping the next line's indentation.
(define (evil-join-line-no-space)
  (let* ((b (line-bounds)) (e (cadr b)))
    (when (< e (point-max)) (delete-region e (+ e 1)))))
;; guu / gUU / g~~ — case-fold the whole current line.
(define (evil-fold-line folder)
  (let* ((b (line-bounds)) (s (car b)) (e (cadr b)))
    (when (< s e) (replace-region s e (folder (buffer-substring s e))))))
(define (evil-downcase-line)    (evil-fold-line string-downcase))
(define (evil-upcase-line)      (evil-fold-line string-upcase))
(define (evil-toggle-case-line) (evil-fold-line string-toggle-case))

;; --- visual p — paste over the selection ---------------------------------------
;; Replace the selection with the register: delete WITHOUT touching the kill
;; ring (delete-region, not kill-region), then paste before the collapsed
;; cursor. (Vim also yanks the replaced text; that nuance is skipped.)
(define (evil-visual-paste)
  (let ((rb (region-beginning)) (re (region-end)))
    (when (and (>= rb 0) (< rb re))
      (delete-region rb re)
      (evil-paste-before)))
  (evil-normal-state))

;; --- q / @ — keyboard macros in registers -----------------------------------
;; Vim-style recording on the kbd-macro engine (C-x ( / C-x ) / C-x e): `qa`
;; records into register a (`qA` appends), `q` again stops, `@a` replays it
;; (`3@a` three times), `@@` replays the last one, `@:` the last ex command.
;; The toggle's name contains "kbd-macro" so the recorder skips it (exprs
;; mentioning kbd-macro are never recorded into the macro itself). The
;; recording flag is READ FROM THE EDITOR, not tracked here, so `q` stays right
;; when the Emacs chords (C-x ( / C-x )) are used instead.
(define (evil-kbd-macro-toggle)
  (if (kbd-macro-recording?) (end-kbd-macro) (read-char "q-" 'evil-macro-record)))

;; --- f / F / t / T char find (+ ; and , repeat) ---------------------------------
;; Built on the core `read-char` primitive: pressing `f` arms a one-keystroke
;; capture; the next key arrives in the dispatch callback as a string. The
;; motion is line-local, vim-style. `;` repeats the last find, `,` reverses it;
;; the visual variants extend the selection instead of moving.
(define *evil-find-char* #f)     ; last sought char, for ; and ,
(define *evil-find-till* #f)     ; t/T (stop short) vs f/F (land on it)
(define *evil-find-forward* #t)
(define *evil-find-count* 1)     ; `2fx` — the count, captured when armed

;; Byte offset the find lands on, scanning the line of `pos`, or #f. `till`
;; stops one grapheme before (after, when backward) the match. Steps by
;; GRAPHEMES, not bytes: buffer-substring floors a mid-codepoint offset to the
;; char's start, so a byte-stepping backward scan could "match" a multibyte
;; `ch` at a mid-codepoint offset and hand goto-char an unaligned position.
(define (evil-find-target-at pos ch till forward)
  (let* ((ls (line-start-pos pos)) (le (line-end-pos pos)) (w (utf8-length ch)))
    (if forward
        (let loop ((i (grapheme-forward pos)))
          (cond ((or (<= i pos) (>= i le)) #f)
                ((equal? (buffer-substring i (+ i w)) ch)
                 (if till (grapheme-backward i) i))
                (else (loop (grapheme-forward i)))))
        (let loop ((i (grapheme-backward pos)))
          (cond ((or (>= i pos) (< i ls)) #f)
                ((equal? (buffer-substring i (+ i w)) ch)
                 (if till (+ i w) i))
                ((<= i ls) #f)
                (else (loop (grapheme-backward i))))))))

;; The `n`th match from `pos`, or #f when there are fewer (Vim then doesn't
;; move). A repeated `t` would re-find the char it's parked before, so each
;; further step searches from one grapheme on.
(define (evil-find-nth pos ch till forward n)
  (let loop ((p pos) (k n))
    (let ((tgt (evil-find-target-at p ch till forward)))
      (cond ((not tgt) #f)
            ((<= k 1) tgt)
            (else (loop (if till (if forward (grapheme-forward tgt) (grapheme-backward tgt)) tgt)
                        (- k 1)))))))
(define (evil-find-go tgt extend)
  (when tgt (if extend (select-to tgt) (goto-char tgt))))
(define (evil-find-dispatch ch)         ; armed from normal state
  (set! *evil-find-char* ch)
  (evil-find-go (evil-find-nth (point) ch *evil-find-till* *evil-find-forward* *evil-find-count*) #f))
(define (evil-find-dispatch-visual ch)  ; armed from visual states — extends
  (set! *evil-find-char* ch)
  (evil-find-go (evil-find-nth (point) ch *evil-find-till* *evil-find-forward* *evil-find-count*) #t))
(define (evil-find-arm till forward callback prompt)
  (set! *evil-find-till* till)
  (set! *evil-find-forward* forward)
  (set! *evil-find-count* (evil-count))
  (read-char prompt callback))
(define (evil-find-char-forward)         (evil-find-arm #f #t 'evil-find-dispatch "f-"))
(define (evil-find-char-backward)        (evil-find-arm #f #f 'evil-find-dispatch "F-"))
(define (evil-till-char-forward)         (evil-find-arm #t #t 'evil-find-dispatch "t-"))
(define (evil-till-char-backward)        (evil-find-arm #t #f 'evil-find-dispatch "T-"))
(define (evil-find-char-forward-visual)  (evil-find-arm #f #t 'evil-find-dispatch-visual "f-"))
(define (evil-find-char-backward-visual) (evil-find-arm #f #f 'evil-find-dispatch-visual "F-"))
(define (evil-till-char-forward-visual)  (evil-find-arm #t #t 'evil-find-dispatch-visual "t-"))
(define (evil-till-char-backward-visual) (evil-find-arm #t #f 'evil-find-dispatch-visual "T-"))
;; ; and , — repeat in the stored / opposite direction. `,` does not flip the
;; stored direction (vim: two `,` in a row go the same way, not back and forth).
(define (evil-find-repeat-dir forward extend)
  (when *evil-find-char*
    (evil-find-go (evil-find-nth (point) *evil-find-char* *evil-find-till* forward (evil-count))
                  extend)))
(define (evil-find-repeat)                (evil-find-repeat-dir *evil-find-forward* #f))
(define (evil-find-repeat-reverse)        (evil-find-repeat-dir (not *evil-find-forward*) #f))
(define (evil-find-repeat-visual)         (evil-find-repeat-dir *evil-find-forward* #t))
(define (evil-find-repeat-reverse-visual) (evil-find-repeat-dir (not *evil-find-forward*) #t))

;; --- * / # — search the word under the cursor -----------------------------------
;; Builds a whole-word regex from the word at (or after) point, stores it as
;; the active search via `set-search-query` (so n/N repeat it), and jumps.
(define (evil-word-char-pos? i)   ; byte at i is a word char [A-Za-z0-9_]
  ;; Plain codepoint ranges, not a regex: this runs per BYTE in the word scans
  ;; (`*`, iw/aw), and a regex match per byte is measurably slow on long lines.
  (let ((c (buffer-substring i (+ i 1))))
    (and (> (string-length c) 0)
         (let ((n (char->integer (car (string->list c)))))
           (or (and (>= n 97) (<= n 122))   ; a-z
               (and (>= n 65) (<= n 90))    ; A-Z
               (and (>= n 48) (<= n 57))    ; 0-9
               (= n 95))))))                ; _
(define (evil-word-at-point)      ; (start . end) of the word at/after point, or #f
  (let* ((b (line-bounds)) (ls (car b)) (le (cadr b))
         (start (let loop ((i (point)))   ; scan right to the next word char
                  (cond ((>= i le) #f)
                        ((evil-word-char-pos? i) i)
                        (else (loop (+ i 1)))))))
    (and start
         (let ((s (let loop ((i start))
                    (if (and (> i ls) (evil-word-char-pos? (- i 1))) (loop (- i 1)) i)))
               (e (let loop ((i start))
                    (if (and (< i le) (evil-word-char-pos? i)) (loop (+ i 1)) i))))
           (cons s e)))))
(define (evil-search-word forward)
  (let ((w (evil-word-at-point)))
    (when w
      (set-search-query
       (string-append "\\b" (regexp-quote (buffer-substring (car w) (cdr w))) "\\b")
       forward)
      ;; `#` searches back from the word's START, or mid-word it would just
      ;; find the word under the cursor.
      (when (not forward) (goto-char (car w)))
      (evil-search-next))))
(define (evil-search-word-forward)  (evil-search-word #t))
(define (evil-search-word-backward) (evil-search-word #f))

;; --- % — matching bracket --------------------------------------------------------
;; Jump between a bracket and its match, nesting-aware. The bracket is the char
;; under the cursor, or the first one to the right on the current line (vim).
;; A plain char scan: strings and comments are not understood.
(define (evil-matchit)        (let ((tg (evil-matchit-target))) (when tg (goto-char tg))))
(define (evil-matchit-visual) (let ((tg (evil-matchit-target))) (when tg (select-to tg))))
(define (evil-matchit-target)
  (let* ((b (line-bounds)) (le (cadr b)) (mx (point-max))
         (brackets (list "(" ")" "[" "]" "{" "}"))
         (start (let loop ((i (point)))
                  (cond ((>= i le) #f)
                        ((member (buffer-substring i (+ i 1)) brackets) i)
                        (else (loop (+ i 1)))))))
    (when start
      ;; One buffer read + a local char walk (byte offsets tracked through
      ;; multibyte chars via char-bytes) — not one buffer-substring FFI call
      ;; per byte, which is what a naive scan costs on a large file.
      (let* ((c (buffer-substring start (+ start 1)))
             (pair (cond ((or (equal? c "(") (equal? c ")")) (cons "(" ")"))
                         ((or (equal? c "[") (equal? c "]")) (cons "[" "]"))
                         (else (cons "{" "}"))))
             (open (car pair)) (close (cdr pair))
             (tgt (if (equal? c open)
                      ;; forward to the matching close
                      (let loop ((chars (cdr (string->list (buffer-substring start mx))))
                                 (i (+ start 1)) (d 1))
                        (if (null? chars)
                            #f
                            (let ((cs (list->string (list (car chars))))
                                  (w (char-bytes (car chars))))
                              (cond ((equal? cs open) (loop (cdr chars) (+ i w) (+ d 1)))
                                    ((equal? cs close)
                                     (if (= d 1) i (loop (cdr chars) (+ i w) (- d 1))))
                                    (else (loop (cdr chars) (+ i w) d))))))
                      ;; backward to the matching open (reversed walk, offsets
                      ;; descending: each char occupies [i-w, i))
                      (let loop ((chars (reverse (string->list (buffer-substring 0 start))))
                                 (i start) (d 1))
                        (if (null? chars)
                            #f
                            (let* ((cs (list->string (list (car chars))))
                                   (w (char-bytes (car chars)))
                                   (j (- i w)))
                              (cond ((equal? cs close) (loop (cdr chars) j (+ d 1)))
                                    ((equal? cs open)
                                     (if (= d 1) j (loop (cdr chars) j (- d 1))))
                                    (else (loop (cdr chars) j d)))))))))
        tgt))))

;; --- text objects (iw/aw, quotes, pairs, ip/ap) ----------------------------------
;; `d i w`, `c a (`, `y i "` , visual `i w`, … — each object computes a byte
;; range from the snapshot; the operator issues the edit. ASCII word class and
;; plain char scans (escapes/strings are not understood) — the vim 90% case.

;; Blank / word-char byte predicates (word = [A-Za-z0-9_], vim's default-ish
;; iskeyword). A mid-codepoint byte reads as "" — treated as neither.
(define (evil-blank-pos? i)
  (let ((c (buffer-substring i (+ i 1)))) (or (equal? c " ") (equal? c "\t"))))

;; iw/aw — the run of word chars (or of non-blank punctuation) under point;
;; `a` adds trailing blanks, or leading ones when there are none trailing.
(define (evil-object-word inner)
  (let* ((b (line-bounds)) (ls (car b)) (le (cadr b)) (p (point)))
    (and (< p le)
         (let* ((pred (cond ((evil-blank-pos? p) evil-blank-pos?)
                            ((evil-word-char-pos? p) evil-word-char-pos?)
                            (else (lambda (i) (and (not (evil-blank-pos? i))
                                                   (not (evil-word-char-pos? i)))))))
                (s (let loop ((i p)) (if (and (> i ls) (pred (- i 1))) (loop (- i 1)) i)))
                (e (let loop ((i p)) (if (and (< i le) (pred i)) (loop (+ i 1)) i))))
           (if inner
               (cons s e)
               (let ((e2 (let loop ((i e))
                           (if (and (< i le) (evil-blank-pos? i)) (loop (+ i 1)) i))))
                 (if (> e2 e)
                     (cons s e2)
                     (cons (let loop ((i s))
                             (if (and (> i ls) (evil-blank-pos? (- i 1))) (loop (- i 1)) i))
                           e))))))))

;; i"/a" (and '/`) — the quote pair on the current line containing point, else
;; the next pair to the right; quotes pair up left to right (escapes ignored).
(define (evil-object-quote qch inner)
  (let* ((b (line-bounds)) (ls (car b)) (le (cadr b)) (p (point))
         (positions (let loop ((i ls) (acc '()))
                      (cond ((>= i le) (reverse acc))
                            ((equal? (buffer-substring i (+ i 1)) qch) (loop (+ i 1) (cons i acc)))
                            (else (loop (+ i 1) acc))))))
    (let loop ((ps positions))
      (cond ((or (null? ps) (null? (cdr ps))) #f)
            ((or (and (<= (car ps) p) (<= p (cadr ps)))  ; contains point,
                 (> (car ps) p))                          ; or first pair past it
             (if inner (cons (+ (car ps) 1) (cadr ps)) (cons (car ps) (+ (cadr ps) 1))))
            (else (loop (cddr ps)))))))

;; i(/a( and friends — the innermost open/close pair containing point,
;; nesting-aware, scanning the whole buffer once (byte offsets tracked through
;; multibyte chars via char-bytes).
(define (evil-object-pair open close inner)
  (let ((p (point)))
    (let loop ((chars (string->list (buffer-substring 0 (point-max))))
               (i 0) (stack '()) (best #f))
      (if (null? chars)
          (and best
               (if inner (cons (+ (car best) 1) (cdr best))
                   (cons (car best) (+ (cdr best) 1))))
          (let* ((ch (car chars)) (w (char-bytes ch)) (cs (list->string (list ch))))
            (cond ((equal? cs open) (loop (cdr chars) (+ i w) (cons i stack) best))
                  ((equal? cs close)
                   (if (null? stack)
                       (loop (cdr chars) (+ i w) stack best)
                       (let* ((o (car stack))
                              (hit (and (<= o p) (<= p i)
                                        (or (not best) (> o (car best))))))
                         (loop (cdr chars) (+ i w) (cdr stack)
                               (if hit (cons o i) best)))))
                  (else (loop (cdr chars) (+ i w) stack best))))))))

;; ip/ap — the paragraph as lines (linewise, incl. trailing newline); `a` also
;; takes the blank lines after it.
(define (evil-object-paragraph inner)
  (let ((b (paragraph-bounds (point))) (mx (point-max)))
    (and b
         (let* ((s (car b)) (e (min mx (+ (cadr b) 1))))
           (if inner
               (cons s e)
               (cons s (let loop ((i e))
                         (cond ((>= i mx) mx)
                               ((blank-line-at? i) (loop (min mx (+ (line-end-pos i) 1))))
                               (else i)))))))))

(define (evil-object-range kind obj)
  (let ((inner (equal? kind "i")))
    (cond ((equal? obj "word")    (evil-object-word inner))
          ((equal? obj "para")    (evil-object-paragraph inner))
          ((equal? obj "dquote")  (evil-object-quote "\"" inner))
          ((equal? obj "squote")  (evil-object-quote "'" inner))
          ((equal? obj "btick")   (evil-object-quote "`" inner))
          ((equal? obj "paren")   (evil-object-pair "(" ")" inner))
          ((equal? obj "brace")   (evil-object-pair "{" "}" inner))
          ((equal? obj "bracket") (evil-object-pair "[" "]" inner))
          ((equal? obj "angle")   (evil-object-pair "<" ">" inner))
          (else #f))))

;; The operator half: op is "change" / "delete" / "yank" / "select" (visual).
;; Deletes go through kill-region-between (register + clipboard, like d/x);
;; yanks select-then-save so the kill ring and OS clipboard both get it.
(define (evil-object-cmd op kind obj)
  (let ((r (evil-object-range kind obj)))
    (when (and r (< (car r) (cdr r)))
      (let ((s (car r)) (e (cdr r)))
        (if (equal? obj "para") (evil-set-linewise) (evil-set-charwise))
        (cond ((equal? op "delete") (kill-region-between s e))
              ((equal? op "change") (kill-region-between s e) (evil-insert-state))
              ((equal? op "yank")   (set-region s e) (kill-ring-save) (goto-char s))
              ;; Visual is inclusive of the char under the cursor, so park the
              ;; cursor ON the object's last char, not just past it.
              ((equal? op "select") (set-region s (grapheme-backward e))))))))

;; Object keys → object names; every (c/d/y) × (i/a) × key chord is generated
;; below, plus visual-mode `i <key>` / `a <key>` selection. b/B are the vim
;; aliases for (/{.
(define *evil-text-objects*
  (list (cons "w" "word") (cons "p" "para")
        (cons "\"" "dquote") (cons "'" "squote") (cons "`" "btick")
        (cons "(" "paren") (cons ")" "paren") (cons "b" "paren")
        (cons "{" "brace") (cons "}" "brace") (cons "shift-b" "brace")
        (cons "[" "bracket") (cons "]" "bracket")
        (cons "<" "angle") (cons ">" "angle")))
(for-each
 (lambda (to)
   (for-each
    (lambda (kind)
      (define-key "evil-visual"
        (string-append kind " " (car to))
        (string-append "evil-object-cmd \"select\" \"" kind "\" \"" (cdr to) "\"")))
    (list "i" "a")))
 *evil-text-objects*)

;; --- the operator grammar (d/c/y/>/</=/gu/gU/g~/gq/gw/g?/gc × any motion) -----
;; Vim's `{count}{operator}{count}{motion}`: an operator key is bound to
;; `(evil-operator "name")`, which the editor turns into OPERATOR-PENDING state;
;; the next keys resolve against the "evil-operator" keymap (motions + text
;; objects, below), with digits as the motion count and `v`/`V` forcing the
;; motion charwise/linewise. The doubled operator (`dd`, `>>`, `gUU`) is "the
;; current line(s)". The editor then dispatches ONE self-contained call:
;;
;;   (evil-operate "delete" 6 "" (lambda () (evil-motion "w")))   ; 2d3w
;;
;; so a macro or `.` replays it with no pending state. `evil-operate` runs the
;; motion thunk (which returns `(type from to)` computed from the snapshot),
;; turns it into a byte range per Vim's exclusive/inclusive/linewise rules, and
;; applies the operator. Add your own operator with `evil-define-operator` and
;; bind a key to `evil-operator "name"`; add a motion with `evil-define-motion`.

(define *evil-motion-count* 0)        ; count for the running motion (0 = none)
(define *evil-op-name* "")            ; the operator the motion runs under
(define (evil-mcount) (max 1 *evil-motion-count*))

;; Motion results: (list type from to). `type` is "exclusive", "inclusive",
;; "line", "exclusive!" (exclusive, but skip Vim's end-of-line adjustment), or
;; "obj-char" / "obj-line" (a text object: an absolute range).
(define (evil-mres type to) (list type (point) to))

;; Apply `f` (pos → pos) `n` times, stopping early once it stops moving.
(define (evil-repeat-pos f p n)
  (let loop ((p p) (k n))
    (if (<= k 0)
        p
        (let ((q (f p)))
          (if (= q p) p (loop q (- k 1)))))))

;; First non-blank byte of the line holding `pos`.
(define (evil-first-nonblank-pos pos)
  (evil-skip-blanks-to (line-start-pos pos) (line-end-pos pos)))

;; Byte offset at char column `col` (clamped) on line `ln`.
(define (evil-line-col-pos ln col) (pos-at-char-column ln col))

;; Target of a vertical motion to line `ln`, keeping point's column.
(define (evil-vertical-target ln)
  (evil-line-col-pos (max 0 (min ln (evil-last-line))) (char-column (point))))

;; `w`/`W` under an operator: the n-th next word start, except that when the
;; last word moved over ends its line the range stops at that line's end (Vim:
;; `dw` on a line's last word never joins the next line).
(define (evil-word-op-target p n step)
  (let* ((q (evil-repeat-pos step p (- n 1)))
         (nx (step q))
         (le (line-end-pos q)))
    (if (and (> nx le) (< q le)) le nx)))

;; `cw` / `cW`: Vim's special case — like `ce`, but from inside a word only
;; to that word's end (the char run under point).
(define (evil-change-word-target p n bigword)
  (let* ((run-end (if bigword
                      (let ((le (line-end-pos p)))
                        (let loop ((i p))
                          (if (and (< i le) (not (evil-blank-pos? i))) (loop (+ i 1)) i)))
                      (cdr (or (evil-object-word #t) (cons p (grapheme-forward p))))))
         (end-step (if bigword bigword-forward-end word-forward-end)))
    (let loop ((r run-end) (k (- n 1)))
      (if (<= k 0)
          r
          (loop (grapheme-forward (end-step (grapheme-backward r))) (- k 1))))))

;; --- sentences ( ) -------------------------------------------------------------
;; A sentence ends at . ! or ? (plus any closing )]"' chars) followed by
;; whitespace; a blank line also starts one. The next sentence starts at the
;; first non-blank after that.
(define *evil-sentence-re* "[.!?][)\\]\"']*[ \t\n]+[^ \t\n]|\n[ \t]*\n[ \t\n]*[^ \t\n]")
(define (evil-sentence-starts from to)
  (let loop ((m (buffer-regex-matches *evil-sentence-re* from to)) (acc '()))
    (if (or (null? m) (null? (cdr m)))
        (reverse acc)
        (loop (cddr m) (cons (grapheme-backward (cadr m)) acc)))))
(define (evil-sentence-forward-pos p)
  (let* ((mx (point-max))
         (starts (evil-sentence-starts p (min mx (+ p 20000)))))
    (let loop ((ss starts))
      (cond ((null? ss) mx)
            ((> (car ss) p) (car ss))
            (else (loop (cdr ss)))))))
(define (evil-sentence-backward-pos p)
  (let* ((from (max 0 (- p 20000)))
         (starts (cons from (evil-sentence-starts from p))))
    (let loop ((ss starts) (best 0))
      (cond ((null? ss) best)
            ((< (car ss) p) (loop (cdr ss) (car ss)))
            (else best)))))
(define (evil-forward-sentence)
  (goto-char (evil-repeat-pos evil-sentence-forward-pos (point) (evil-count))))
(define (evil-backward-sentence)
  (goto-char (evil-repeat-pos evil-sentence-backward-pos (point) (evil-count))))
(define (evil-forward-sentence-visual)
  (select-to (evil-repeat-pos evil-sentence-forward-pos (point) (evil-count))))
(define (evil-backward-sentence-visual)
  (select-to (evil-repeat-pos evil-sentence-backward-pos (point) (evil-count))))

;; H / M / L — screen lines (top / middle / bottom of the window).
(define (evil-screen-line which n)
  (let* ((vl (visible-lines)) (top (car vl)) (bot (min (cadr vl) (evil-last-line))))
    (cond ((equal? which "H") (min bot (+ top (- n 1))))
          ((equal? which "L") (max top (- bot (- n 1))))
          (else (quotient (+ top bot) 2)))))

;; User-defined motions: name → thunk returning a motion result.
(define *evil-user-motions* (hash))
(define (evil-define-motion name thunk)
  (set! *evil-user-motions* (hash-insert *evil-user-motions* name thunk)))

;; The motion table. Every motion computes from the snapshot (point, buffer)
;; and the motion count; #f means "fails" (Vim beeps; the operator is dropped).
(define (evil-motion name)
  (let* ((p (point)) (n (evil-mcount)) (raw *evil-motion-count*))
    (cond
      ((equal? name "line")                    ; dd / cc / yy / >> …
       (let ((ln (min (evil-last-line) (+ (line-of-pos p) (- n 1)))))
         (list "line" p (pos-at-char-column ln 0))))
      ((equal? name "h")
       (let ((ls (line-start-pos p)))
         (and (> p ls)
              (evil-mres "exclusive"
                         (evil-repeat-pos (lambda (q) (if (> q ls) (grapheme-backward q) q)) p n)))))
      ((equal? name "l")
       (let ((le (line-end-pos p)))
         (and (< p le)
              (evil-mres "exclusive"
                         (evil-repeat-pos (lambda (q) (if (< q le) (grapheme-forward q) q)) p n)))))
      ((equal? name "j")
       (let ((ln (line-of-pos p)))
         (and (< ln (evil-last-line)) (list "line" p (evil-vertical-target (+ ln n))))))
      ((equal? name "k")
       (let ((ln (line-of-pos p)))
         (and (> ln 0) (list "line" p (evil-vertical-target (- ln n))))))
      ((equal? name "+")
       (let ((ln (line-of-pos p)))
         (and (< ln (evil-last-line))
              (list "line" p (evil-first-nonblank-pos (evil-vertical-target (+ ln n)))))))
      ((equal? name "-")
       (let ((ln (line-of-pos p)))
         (and (> ln 0) (list "line" p (evil-first-nonblank-pos (evil-vertical-target (- ln n)))))))
      ((equal? name "_")
       (list "line" p (pos-at-char-column (min (evil-last-line) (+ (line-of-pos p) (- n 1))) 0)))
      ((equal? name "w")
       (if (and (equal? *evil-op-name* "change") (not (evil-blank-pos? p)) (< p (line-end-pos p)))
           (evil-mres "exclusive!" (evil-change-word-target p n #f))
           (evil-mres "exclusive" (evil-word-op-target p n word-forward-start))))
      ((equal? name "W")
       (if (and (equal? *evil-op-name* "change") (not (evil-blank-pos? p)) (< p (line-end-pos p)))
           (evil-mres "exclusive!" (evil-change-word-target p n #t))
           (evil-mres "exclusive" (evil-word-op-target p n bigword-forward-start))))
      ((equal? name "b")  (evil-mres "exclusive" (evil-repeat-pos word-backward-start p n)))
      ((equal? name "B")  (evil-mres "exclusive" (evil-repeat-pos bigword-backward-start p n)))
      ((equal? name "e")  (evil-mres "inclusive" (evil-repeat-pos word-forward-end p n)))
      ((equal? name "E")  (evil-mres "inclusive" (evil-repeat-pos bigword-forward-end p n)))
      ((equal? name "ge") (evil-mres "inclusive" (evil-repeat-pos word-backward-end p n)))
      ((equal? name "gE") (evil-mres "inclusive" (evil-repeat-pos bigword-backward-end p n)))
      ((equal? name "0")  (evil-mres "exclusive" (line-start-pos p)))
      ((equal? name "^")  (evil-mres "exclusive" (evil-first-nonblank-pos p)))
      ((equal? name "$")
       (evil-mres "exclusive!"
                  (line-end-pos (pos-at-char-column (min (evil-last-line) (+ (line-of-pos p) (- n 1))) 0))))
      ((equal? name "g_")
       (let* ((ls (pos-at-char-column (min (evil-last-line) (+ (line-of-pos p) (- n 1))) 0))
              (le (line-end-pos ls))
              (last (let loop ((i le))
                      (if (and (> i ls) (evil-blank-pos? (- i 1))) (loop (- i 1)) i))))
         (evil-mres "exclusive!" last)))
      ((equal? name "|")
       (evil-mres "exclusive" (evil-line-col-pos (line-of-pos p) (- n 1))))
      ((equal? name "gg")
       (list "line" p (evil-first-nonblank-pos
                       (pos-at-char-column (if (> raw 0) (min (evil-last-line) (- raw 1)) 0) 0))))
      ((equal? name "G")
       (list "line" p (evil-first-nonblank-pos
                       (pos-at-char-column (if (> raw 0) (min (evil-last-line) (- raw 1)) (evil-last-line)) 0))))
      ((equal? name "}")  (evil-mres "exclusive" (evil-repeat-pos paragraph-forward-pos p n)))
      ((equal? name "{")  (evil-mres "exclusive" (evil-repeat-pos paragraph-backward-pos p n)))
      ((equal? name ")")  (evil-mres "exclusive" (evil-repeat-pos evil-sentence-forward-pos p n)))
      ((equal? name "(")  (evil-mres "exclusive" (evil-repeat-pos evil-sentence-backward-pos p n)))
      ((equal? name "%")
       (if (> raw 0)
           (let ((ln (min (evil-last-line) (quotient (+ (* raw (line-count)) 99) 100))))
             (list "line" p (pos-at-char-column (max 0 (- ln 1)) 0)))
           (let ((tg (evil-matchit-target))) (and tg (evil-mres "inclusive" tg)))))
      ((or (equal? name "H") (equal? name "M") (equal? name "L"))
       (list "line" p (evil-first-nonblank-pos (pos-at-char-column (evil-screen-line name n) 0))))
      ((equal? name ";") (evil-find-motion *evil-find-forward* n))
      ((equal? name ",") (evil-find-motion (not *evil-find-forward*) n))
      ((equal? name "n") (evil-search-motion (search-query) (search-forward?) n))
      ((equal? name "N") (evil-search-motion (search-query) (not (search-forward?)) n))
      ((equal? name "*") (evil-search-word-motion #t n))
      ((equal? name "#") (evil-search-word-motion #f n))
      (else
       (let ((user (hash-try-get *evil-user-motions* name)))
         (and user (user)))))))

;; f/F/t/T as operator motions: `(evil-motion-find "f" "x")` (the editor fills
;; in the char). f/t are inclusive, F/T exclusive — as in Vim.
(define (evil-motion-find kind ch)
  (set! *evil-find-char* ch)
  (set! *evil-find-till* (or (equal? kind "t") (equal? kind "T")))
  (set! *evil-find-forward* (or (equal? kind "f") (equal? kind "t")))
  (evil-find-motion *evil-find-forward* (evil-mcount)))
(define (evil-find-motion forward n)
  (and *evil-find-char*
       (let ((tg (evil-find-nth (point) *evil-find-char* *evil-find-till* forward n)))
         (and tg (evil-mres (if forward "inclusive" "exclusive") tg)))))

;; Mark motions under an operator: `d'a` (linewise) / `` d`a `` (exclusive).
(define (evil-motion-mark m exact)
  (let ((pos (mark-position (if (equal? m "`") "'" m))))
    (and (>= pos 0)
         (if exact (evil-mres "exclusive" pos) (list "line" (point) pos)))))

;; H / M / L in Normal / Visual: the top / middle / bottom screen line (with a
;; count, that many lines in from the top / bottom), on its first non-blank.
(define (evil-screen-goto which extend)
  (let ((pos (evil-first-nonblank-pos (pos-at-char-column (evil-screen-line which (evil-count)) 0))))
    (if extend (select-to pos) (goto-char pos))))
(define (evil-window-top)           (evil-screen-goto "H" #f))
(define (evil-window-middle)        (evil-screen-goto "M" #f))
(define (evil-window-bottom)        (evil-screen-goto "L" #f))
(define (evil-window-top-visual)    (evil-screen-goto "H" #t))
(define (evil-window-middle-visual) (evil-screen-goto "M" #t))
(define (evil-window-bottom-visual) (evil-screen-goto "L" #t))

;; Search motions (`d/foo`, `dn`): exclusive, to the n-th match.
(define (evil-search-motion pattern forward n)
  (and (not (equal? pattern ""))
       (let loop ((from (point)) (k n))
         (let ((hit (regex-search-pos pattern from forward)))
           (cond ((< hit 0) #f)
                 ((<= k 1) (evil-mres "exclusive" hit))
                 (else (loop hit (- k 1))))))))
(define (evil-motion-search pattern forward)
  (set-search-query pattern forward)
  (evil-search-motion pattern forward (evil-mcount)))
(define (evil-search-word-motion forward n)
  (let ((w (evil-word-at-point)))
    (and w
         (let ((pat (string-append "\\b" (regexp-quote (buffer-substring (car w) (cdr w))) "\\b")))
           (set-search-query pat forward)
           (evil-search-motion pat forward n)))))

;; Text objects under an operator: `(evil-object "i" "word")`.
(define (evil-object kind obj)
  (let ((r (evil-object-range kind obj)))
    (and r (< (car r) (cdr r))
         (list (if (equal? obj "para") "obj-line" "obj-char") (car r) (cdr r)))))

;; The range of a motion result: (list kind start end), kind "char" or
;; "line" (a line range runs from a line start to past the last line's
;; newline, or to the buffer end). `force` is "", "v" or "V".
(define (evil-line-range a b)
  (let ((s (line-start-pos a)) (le (line-end-pos b)))
    (list "line" s (if (< le (point-max)) (+ le 1) le))))
(define (evil-motion->range m force)
  (let* ((a0 (cadr m)) (b0 (caddr m))
         (a (min a0 b0)) (b (max a0 b0))
         (type (car m))
         (type (cond ((equal? force "V") (if (equal? type "obj-char") "obj-line!" "line"))
                     ((equal? force "v")
                      (cond ((equal? type "line") "exclusive!")
                            ((equal? type "inclusive") "exclusive!")
                            ((equal? type "obj-line") "obj-char")
                            ((equal? type "obj-char") "obj-char")
                            (else "inclusive")))
                     (else type))))
    (cond
      ((equal? type "obj-char") (list "char" a b))
      ((equal? type "obj-line") (list "line" a b))
      ((equal? type "obj-line!") (evil-line-range a (max a (- b 1))))
      ((equal? type "line") (evil-line-range a b))
      ((equal? type "inclusive")
       (list "char" a (if (< b (point-max)) (grapheme-forward b) b)))
      ((equal? type "exclusive!") (list "char" a b))
      ;; Exclusive, with Vim's `:help exclusive` rules: an end in column 0 of
      ;; a later line moves back to the end of the previous line; if the start
      ;; is also at/before its line's first non-blank, the motion turns
      ;; linewise (`d}` from a paragraph's first column deletes whole lines).
      ((and (> b a) (= b (line-start-pos b)) (> (line-of-pos b) (line-of-pos a)))
       (let ((b2 (line-end-pos (- b 1))))
         (if (<= a (evil-first-nonblank-pos a))
             (evil-line-range a b2)
             (list "char" a b2))))
      (else (list "char" a b)))))

;; The linewise delete shared by `dd`, `dj`, `dip`, … over [s, e): on the
;; buffer's last line (no newline after it) the newline BEFORE the lines goes
;; too, so no phantom empty line is left; the cursor lands on the first
;; non-blank of the line that slides into place.
(define (evil-delete-lines s e)
  (let ((eof (and (= e (point-max))
                  (or (= e s) (not (equal? (buffer-substring (- e 1) e) "\n"))))))
    (kill-region-between s e)
    (evil-set-linewise)
    (if (and eof (> s 0))
        (let ((ls (line-start-pos (- s 1))))
          (delete-region (- s 1) s)
          (goto-char (evil-skip-blanks-to ls (- s 1))))
        (if (and (= e (point-max)) (> s 0))
            (goto-char (evil-skip-blanks-to (line-start-pos (- s 1)) (- s 1)))
            (goto-char (+ s (- (evil-skip-blanks-to e (line-end-pos e)) e)))))))

;; Case / rot13 folds over a string.
(define (evil-rot13-char c)
  (let ((n (char->integer c)))
    (cond ((and (>= n 97) (<= n 122)) (integer->char (+ 97 (modulo (+ (- n 97) 13) 26))))
          ((and (>= n 65) (<= n 90))  (integer->char (+ 65 (modulo (+ (- n 65) 13) 26))))
          (else c))))
(define (evil-rot13 s) (list->string (map evil-rot13-char (string->list s))))

;; Replace [s, e) with (folder text), leaving point at `goto`.
(define (evil-fold-range s e folder goto)
  (when (< s e)
    (let* ((old (buffer-substring s e)) (new (folder old)))
      (when (not (equal? old new)) (replace-region s e new))))
  (goto-char goto))

;; Shift the lines of [s, e) one step right (dir > 0) or left: the region-
;; aware indent commands do the work (they skip blank lines, honor the
;; buffer's indent width and land on the first non-blank).
(define (evil-shift-lines s e dir)
  (set-region s (max s (- e 1)))
  (if (> dir 0) (indent-region) (outdent-region))
  (goto-char (evil-first-nonblank-pos s)))

;; User-defined operators: name → (lambda (kind start end) …).
(define *evil-user-operators* (hash))
(define (evil-define-operator name proc)
  (set! *evil-user-operators* (hash-insert *evil-user-operators* name proc)))

;; Apply operator `op` to [s, e) of kind "char"/"line".
(define (evil-apply-operator op kind s e)
  (let ((line (equal? kind "line")) (p (point)))
    (cond
      ((equal? op "delete")
       (if line
           (evil-delete-lines s e)
           (when (< s e) (kill-region-between s e) (evil-set-charwise))))
      ((equal? op "change")
       (if line
           (let* ((eof (and (= e (point-max))
                            (or (= e s) (not (equal? (buffer-substring (- e 1) e) "\n")))))
                  (e2 (if eof e (- e 1))))
             (when (< s e2) (kill-region-between s e2) (evil-set-linewise))
             (goto-char s))
           (when (< s e) (kill-region-between s e) (evil-set-charwise)))
       (evil-insert-state))
      ((equal? op "yank")
       (when (< s e)
         (set-region s e) (kill-ring-save)
         (if line (evil-set-linewise) (evil-set-charwise))
         (goto-char (if (and line (= (line-of-pos p) (line-of-pos s))) p s))))
      ((equal? op "indent")  (evil-shift-lines s e 1))
      ((equal? op "outdent") (evil-shift-lines s e -1))
      ((equal? op "reindent") (evil-reindent-lines s e))
      ((equal? op "downcase") (evil-fold-range s e string-downcase (if line p s)))
      ((equal? op "upcase")   (evil-fold-range s e string-upcase (if line p s)))
      ((equal? op "togglecase") (evil-fold-range s e string-toggle-case (if line p s)))
      ((equal? op "rot13")    (evil-fold-range s e evil-rot13 (if line p s)))
      ((equal? op "fill")
       (evil-fill-range (line-start-pos s) (line-end-pos (max s (- e 1))) #f))
      ((equal? op "fill-keep")
       (evil-fill-range (line-start-pos s) (line-end-pos (max s (- e 1))) #t))
      ((equal? op "comment") (evil-comment-lines s e))
      (else
       (let ((user (hash-try-get *evil-user-operators* op)))
         (if user
             (user kind s e)
             (message (string-append "Unknown operator: " op))))))))

;; The entry point the editor dispatches for `{count}{op}{count}{motion}`.
(define (evil-operate op count force thunk)
  (set! *evil-motion-count* count)
  (set! *evil-op-name* op)
  (let ((m (thunk)))
    (set! *evil-motion-count* 0)
    ;; A failed motion (#f) aborts the whole command — `c` doesn't even enter
    ;; Insert — as in Vim.
    (when m
      (let ((r (evil-motion->range m force)))
        (evil-apply-operator op (car r) (cadr r) (caddr r))))))

;; An operator key in a Visual state: apply it to the selection, then leave
;; Visual (change enters Insert instead).
(define (evil-visual-operator op)
  (let* ((rb (region-beginning)) (re (region-end))
         (st (evil-state))
         (kind (if (equal? st "visual-line") "line" "char")))
    (when (and (>= rb 0) (< rb re))
      (if (equal? op "yank")
          ;; the live selection IS the range (Visual is inclusive in the core)
          (begin (kill-ring-save)
                 (if (equal? kind "line") (evil-set-linewise) (evil-set-charwise))
                 (goto-char rb))
          (evil-apply-operator op kind rb re)))
    (when (not (equal? op "change")) (evil-normal-state))))

;; `=` — re-indent lines. With no language indenter wired into Scheme, this
;; normalizes each line's indentation to its first non-blank line's above's
;; (copying the previous line's indent), the `autoindent` behavior.
(define (evil-reindent-lines s e)
  (let* ((first (line-of-pos s))
         (last (line-of-pos (max s (- e 1))))
         (indent-of (lambda (pos)
                      (buffer-substring (line-start-pos pos) (evil-first-nonblank-pos pos))))
         (base (if (> first 0) (indent-of (pos-at-char-column (- first 1) 0)) "")))
    (let loop ((ln last))
      (when (>= ln first)
        (let* ((ls (pos-at-char-column ln 0)) (fnb (evil-first-nonblank-pos ls)))
          (when (and (< fnb (line-end-pos ls))
                     (not (equal? (buffer-substring ls fnb) base)))
            (replace-region ls fnb base)))
        (loop (- ln 1))))
    (goto-char (evil-first-nonblank-pos s))))

;; Placeholder until the comment operator lands (gc).
(define (evil-comment-lines s e) (message "gc: no comment operator"))

;; --- the operator-pending keymap -------------------------------------------------
(define (evil-omap chord expr) (define-key "evil-operator" chord expr))
(for-each
 (lambda (b) (evil-omap (car b) (string-append "evil-motion \"" (cdr b) "\"")))
 (list (cons "h" "h") (cons "l" "l") (cons "space" "l") (cons "j" "j") (cons "k" "k")
       (cons "+" "+") (cons "-" "-") (cons "_" "_")
       (cons "w" "w") (cons "shift-w" "W") (cons "b" "b") (cons "shift-b" "B")
       (cons "e" "e") (cons "shift-e" "E") (cons "g e" "ge") (cons "g shift-e" "gE")
       (cons "0" "0") (cons "^" "^") (cons "$" "$") (cons "g _" "g_") (cons "|" "|")
       (cons "g g" "gg") (cons "shift-g" "G")
       (cons "{" "{") (cons "}" "}") (cons "(" "(") (cons ")" ")") (cons "%" "%")
       (cons "shift-h" "H") (cons "shift-m" "M") (cons "shift-l" "L")
       (cons ";" ";") (cons "," ",") (cons "n" "n") (cons "shift-n" "N")
       (cons "*" "*") (cons "#" "#")))
(evil-omap "f" "evil-motion-find \"f\" %c")
(evil-omap "shift-f" "evil-motion-find \"F\" %c")
(evil-omap "t" "evil-motion-find \"t\" %c")
(evil-omap "shift-t" "evil-motion-find \"T\" %c")
(evil-omap "/" "evil-op-search \"/\"")
(evil-omap "'" "evil-motion-mark %c #f")
(evil-omap "`" "evil-motion-mark %c #t")
(evil-omap "?" "evil-op-search \"?\"")
;; Text objects: `d i w`, `c a (`, … (and the visual `i w` selections below).
(for-each
 (lambda (to)
   (for-each
    (lambda (kind)
      (evil-omap (string-append kind " " (car to))
                 (string-append "evil-object \"" kind "\" \"" (cdr to) "\"")))
    (list "i" "a")))
 *evil-text-objects*)

;; Operator keys (normal state → operator-pending; visual → apply to selection).
(define (evil-bind-operator chord op)
  (define-key "evil-normal" chord (string-append "evil-operator \"" op "\"")))
(evil-bind-operator "d" "delete")
(evil-bind-operator "c" "change")
(evil-bind-operator "y" "yank")
(evil-bind-operator ">" "indent")
(evil-bind-operator "<" "outdent")
(evil-bind-operator "=" "reindent")
(evil-bind-operator "g u" "downcase")
(evil-bind-operator "g shift-u" "upcase")
(evil-bind-operator "g ~" "togglecase")
(evil-bind-operator "g ?" "rot13")
(evil-bind-operator "g q" "fill")
(evil-bind-operator "g w" "fill-keep")
(evil-bind-operator "g c" "comment")
(for-each
 (lambda (km)
   (define-key km "=" "evil-visual-operator \"reindent\"")
   (define-key km "g u" "evil-visual-operator \"downcase\"")
   (define-key km "g shift-u" "evil-visual-operator \"upcase\"")
   (define-key km "g ~" "evil-visual-operator \"togglecase\"")
   (define-key km "g ?" "evil-visual-operator \"rot13\"")
   (define-key km "g c" "evil-visual-operator \"comment\""))
 (list "evil-visual" "evil-visual-line"))

;; Visual-mode change (charwise selection's exact range) and case folding. Case
;; ops work the region text directly (builtins only) rather than via editing.scm.
(define (evil-visual-change) (kill-region) (evil-set-charwise) (evil-insert-state))
(define (evil-visual-fold-case folder)
  (let ((s (region-beginning)) (e (region-end)))
    (when (and (>= s 0) (< s e))
      (replace-region s e (folder (buffer-substring s e)))))
  (evil-normal-state))
(define (evil-visual-downcase)    (evil-visual-fold-case string-downcase))    ; u
(define (evil-visual-upcase)      (evil-visual-fold-case string-upcase))      ; U
(define (evil-visual-toggle-case) (evil-visual-fold-case string-toggle-case)) ; ~
;; Visual-LINE change: drop the whole-line selection, then open a fresh line in
;; its place and insert (the selection already spans entire lines).
(define (evil-visual-line-change)
  ;; At EOF the kill already leaves an empty last line: insert there instead of
  ;; opening another one above it.
  (let ((eof (evil-visual-line-eof-start)))
    (kill-region) (evil-set-linewise)
    (if eof (evil-insert-state) (evil-open-above))))

;; Visual `>` / `<` — shift the selected lines one indent step. Vim drops back to
;; Normal after a single shift (`3>` repeats instead, which the count prefix now
;; handles), so these end in normal state.
(define (evil-visual-indent)  (indent-region)  (evil-normal-state))
(define (evil-visual-outdent) (outdent-region) (evil-normal-state))

;; Visual `J` — join every selected line into one, collapsing each newline plus
;; the following indentation to a single space (vim `J` over a selection). One
;; replace-region over the whole selected line span, computed from the snapshot.
(define (evil-visual-join)
  (let ((rb (region-beginning)) (re (region-end)))
    (when (and (>= rb 0) (< rb re))
      (let* ((s (line-start-pos rb))
             (e (line-end-pos (max rb (- re 1)))))
        (when (< s e)
          (replace-region s e
            (replace-regexp-in-string "\n[ \t]*" " " (buffer-substring s e)))
          (goto-char s)))))
  (evil-normal-state))

;; Visual `r<char>` — overwrite EVERY character of the selection with the typed
;; char (vim). Newlines are preserved, so a multi-line selection keeps its
;; shape. Armed through the same `read-char` capture as normal-state `r`.
(define (evil-visual-replace-dispatch ch)
  (let ((rb (region-beginning)) (re (region-end)))
    (when (and (>= rb 0) (< rb re))
      (replace-region rb re
        (list->string
         (map (lambda (c) (if (equal? c #\newline) c (car (string->list ch))))
              (string->list (buffer-substring rb re)))))
      (goto-char rb)))
  (evil-normal-state))
(define (evil-visual-replace-char) (read-char "r-" 'evil-visual-replace-dispatch))
;; Block-wise `r` overwrites only the rectangle, not the stream between its
;; corners (the core walks the block's columns).
(define (evil-block-replace-dispatch ch) (evil-block-replace ch))
(define (evil-block-replace-char) (read-char "r-" 'evil-block-replace-dispatch))

;; Visual `l` stops on the line's last char (Vim), so repeated `l` can't creep
;; onto the line break and make `d` join lines. `$` still reaches it.
(define (evil-visual-forward-char)
  (let ((p (point)))
    (when (< (grapheme-forward p) (line-end-pos p)) (forward-char))))

;; --- normal state -----------------------------------------------------------
;; motion
(define-key "evil-normal" "h" 'backward-char)
(define-key "evil-normal" "l" 'forward-char)
(define-key "evil-normal" "j" 'next-line)
(define-key "evil-normal" "k" 'previous-line)
(define-key "evil-normal" "w" 'forward-word-start)
;; `b` is the exact mirror of `w` (class-aware), NOT Emacs `backward-word` —
;; that one skips punctuation as a separator, so it disagreed with `w` about
;; where a word starts and `b` could never land on a `).` run.
(define-key "evil-normal" "b" 'backward-word-start)
;; W / B / E — whitespace-delimited WORD motions (vim's shifted trio).
(define-key "evil-normal" "shift-w" 'forward-bigword-start)
(define-key "evil-normal" "shift-b" 'backward-bigword-start)
(define-key "evil-normal" "shift-e" 'forward-bigword-end)
(define-key "evil-normal" "0" 'move-beginning-of-line)
;; macOS clears shift for non-letter keys and reports the produced char, so
;; bind the literal symbol ($, :, ?) — NOT shift-4 / shift-; / shift-/.
(define-key "evil-normal" "$" 'move-end-of-line)
(define-key "evil-normal" "e" 'evil-forward-word-end)    ; e — end of word
(define-key "evil-normal" "^" 'evil-first-non-blank)     ; ^ — first non-blank
(define-key "evil-normal" "g g" 'evil-goto-first-line)   ; gg — first line, first non-blank
(define-key "evil-normal" "shift-g" 'evil-goto-last-line); G  — last line, first non-blank
(define-key "evil-normal" "{" 'evil-backward-paragraph)  ; { — previous paragraph boundary
(define-key "evil-normal" "}" 'evil-forward-paragraph)   ; } — next paragraph boundary
(define-key "evil-normal" "(" 'evil-backward-sentence)   ; ( — previous sentence start
(define-key "evil-normal" ")" 'evil-forward-sentence)    ; ) — next sentence start
(define-key "evil-normal" "%" 'evil-matchit)             ; % — matching bracket
(define-key "evil-normal" "f" 'evil-find-char-forward)   ; f<char> — find char on line
(define-key "evil-normal" "shift-f" 'evil-find-char-backward) ; F<char> — find backward
(define-key "evil-normal" "t" 'evil-till-char-forward)   ; t<char> — till char
(define-key "evil-normal" "shift-t" 'evil-till-char-backward) ; T<char>
(define-key "evil-normal" ";" 'evil-find-repeat)         ; ; — repeat last f/t
(define-key "evil-normal" "," 'evil-find-repeat-reverse) ; , — repeat, reversed
(define-key "evil-normal" "*" 'evil-search-word-forward) ; * — search word under cursor
(define-key "evil-normal" "#" 'evil-search-word-backward); # — same, backward
;; enter insert
(define-key "evil-normal" "i" 'evil-insert-state)
(define-key "evil-normal" "shift-i" 'evil-insert-line)   ; I
(define-key "evil-normal" "a" 'evil-append)
(define-key "evil-normal" "shift-a" 'evil-append-line)   ; A
(define-key "evil-normal" "o" 'evil-open-below)
(define-key "evil-normal" "shift-o" 'evil-open-above)    ; O
;; edit
(define-key "evil-normal" "x" 'evil-delete-char)         ; x — delete char (into register)
(define-key "evil-normal" "r" 'evil-replace-char)        ; r<char> — replace char under cursor
(define-key "evil-normal" "s" 'evil-substitute-char)     ; s — substitute char
(define-key "evil-normal" "shift-s" 'evil-change-line)   ; S — substitute line (= cc)
(define-key "evil-normal" "~" 'evil-toggle-case)         ; ~ — toggle case of char
(define-key "evil-normal" "shift-j" 'evil-join-line)     ; J — join next line up
(define-key "evil-normal" "g shift-j" 'evil-join-line-no-space) ; gJ — join, no space
(define-key "evil-normal" "g d" 'lsp-find-definition)    ; gd — go to definition (LSP)
(define-key "evil-normal" "shift-k" 'lsp-hover)          ; K  — doc at point (LSP)
(define-key "evil-normal" "q" 'evil-kbd-macro-toggle)    ; q — record macro (toggle)
(define-key "evil-normal" "@" "evil-macro-play %c")     ; @a — replay macro a (@@ last, @: ex)
;; "x — use register x for the next yank / delete / put ("_ black hole, "+ / "*
;; clipboard, "0 last yank, "1-"9 deletes, "- small delete, ". : / read-only).
(define-key "evil-normal" "\"" "evil-use-register %c")
(define-key "evil-visual" "\"" "evil-use-register %c")
(define-key "evil-visual-line" "\"" "evil-use-register %c")
(define-key "evil-visual-block" "\"" "evil-use-register %c")
(define-key "evil-normal" "shift-d" 'evil-delete-to-eol) ; D  (= d$)
(define-key "evil-normal" "shift-c" 'evil-change-to-eol) ; C  (= c$)
(define-key "evil-normal" "shift-y" 'evil-yank-line)     ; Y (= yy)
(define-key "evil-normal" "p" 'evil-paste-after)        ; p  (line-wise → below)
(define-key "evil-normal" "shift-p" 'evil-paste-before) ; P  (line-wise → above)
;; marks: ma sets (mA = file mark), 'a jumps to its line, `a to the exact spot;
;; '' / `` return to before the last jump. gv reselects the last Visual area.
(define-key "evil-normal" "m" "evil-set-mark %c")
(define-key "evil-normal" "'" "evil-goto-mark-line %c")
(define-key "evil-normal" "`" "evil-goto-mark %c")
(define-key "evil-normal" "g v" 'evil-reselect-visual)
(define-key "evil-normal" "shift-h" 'evil-window-top)     ; H / M / L — screen lines
(define-key "evil-normal" "shift-m" 'evil-window-middle)
(define-key "evil-normal" "shift-l" 'evil-window-bottom)
(define-key "evil-normal" "u" 'undo)
(define-key "evil-normal" "." 'evil-repeat)              ; . — repeat the last change (3. = new count)
(define-key "evil-normal" "ctrl-r" 'redo)               ; C-r — redo (else it falls
                                                        ; through to isearch-backward)
(define-key "evil-normal" "v" 'evil-visual-state)
(define-key "evil-normal" "shift-v" 'evil-visual-line-state) ; V (line-wise visual)
(define-key "evil-normal" "ctrl-v" 'evil-visual-block-state) ; C-v (block-wise)
(define-key "evil-normal" ":" 'evil-ex-command)          ; :  (e.g. :%s/a/b/g)
(define-key "evil-normal" "escape" 'keyboard-quit)
;; search
(define-key "evil-normal" "/" 'evil-search-forward)
(define-key "evil-normal" "?" 'evil-search-backward)
(define-key "evil-normal" "n" 'evil-search-next)
(define-key "evil-normal" "shift-n" 'evil-search-previous) ; N

;; Scrolling — vanilla evil's half-page scroll on C-d / C-u. (No SPC leader by
;; default: that's a Spacemacs/Doom addition, not part of Emacs's evil-mode.
;; Define your own in init.scm, e.g. (define-key "evil-normal" "space space"
;; "helm-M-x").)
(define-key "evil-normal" "ctrl-d" 'evil-scroll-down)   ; half page down
(define-key "evil-normal" "ctrl-u" 'evil-scroll-up)     ; half page up
(define-key "evil-normal" "ctrl-f" 'scroll-up)          ; full page forward
(define-key "evil-normal" "ctrl-b" 'scroll-down)        ; full page back
(define-key "evil-normal" "z z" 'recenter)              ; zz — center cursor line

;; (`>>` / `<<` and every other operator + motion pair live in the operator
;; grammar above: `>` / `<` are operators, doubled for the current line.)

;; --- Emacs chords vim owns in Normal state ----------------------------------
;; Unbound Ctrl chords fall through to the Emacs dispatch, which is what keeps
;; C-x C-s / M-x / C-g working under evil. But four of those Emacs defaults
;; EDIT the buffer, and in Normal state a vim user pressing them expects
;; something else entirely — `C-o` above all, which is vim's jump-back and Emacs's
;; `open-line`, so it silently inserted a newline. Shadow them:
;;   C-o  → pop the xref marker stack (the nearest thing to vim's jump list)
;;   C-k / C-w / C-y / C-t → undefined, like vim (beep instead of edit)
(define (evil-undefined)
  (message "Not a Normal-state command (Emacs binding shadowed by evil)"))
(define-key "evil-normal" "ctrl-o" 'xref-pop-marker)    ; jump back (≈ vim C-o)
(define-key "evil-normal" "ctrl-k" 'evil-undefined)     ; was kill-line
(define-key "evil-normal" "ctrl-w" 'evil-undefined)     ; was kill-region
(define-key "evil-normal" "ctrl-y" 'evil-undefined)     ; was yank
(define-key "evil-normal" "ctrl-t" 'evil-undefined)     ; was transpose-chars

;; --- insert state -----------------------------------------------------------
(define-key "evil-insert" "escape" 'evil-normal-state)
(define-key "evil-insert" "ctrl-r" "evil-insert-register %c") ; C-r x — insert register x

;; --- visual state (motions extend; the mark is active) ----------------------
(define-key "evil-visual" "h" 'backward-char)
(define-key "evil-visual" "l" 'evil-visual-forward-char)
(define-key "evil-visual" "j" 'next-line)
(define-key "evil-visual" "k" 'previous-line)
(define-key "evil-visual" "w" 'forward-word-start)
(define-key "evil-visual" "b" 'backward-word-start)
(define-key "evil-visual" "0" 'move-beginning-of-line)
(define-key "evil-visual" "$" 'move-end-of-line)
(define-key "evil-visual" "g g" 'beginning-of-buffer)
(define-key "evil-visual" "shift-g" 'end-of-buffer)
(define-key "evil-visual" "e" 'evil-forward-word-end)          ; e — end of word (extends)
(define-key "evil-visual" "{" 'evil-backward-paragraph-visual) ; { / } — extend by paragraph
(define-key "evil-visual" "}" 'evil-forward-paragraph-visual)
(define-key "evil-visual" "(" 'evil-backward-sentence-visual)
(define-key "evil-visual" ")" 'evil-forward-sentence-visual)
(define-key "evil-visual" "f" 'evil-find-char-forward-visual)  ; f/F/t/T — extend to char
(define-key "evil-visual" "shift-f" 'evil-find-char-backward-visual)
(define-key "evil-visual" "t" 'evil-till-char-forward-visual)
(define-key "evil-visual" "shift-t" 'evil-till-char-backward-visual)
(define-key "evil-visual" ";" 'evil-find-repeat-visual)        ; ; , — repeat f/t (extends)
(define-key "evil-visual" "," 'evil-find-repeat-reverse-visual)
(define-key "evil-visual" "d" 'evil-visual-delete)
(define-key "evil-visual" "x" 'evil-visual-delete)
(define-key "evil-visual" "y" 'evil-visual-yank)
(define-key "evil-visual" "p" 'evil-visual-paste)             ; p — paste over selection
(define-key "evil-visual" "c" 'evil-visual-change)            ; c/s — change selection
(define-key "evil-visual" "s" 'evil-visual-change)
(define-key "evil-visual" "o" 'exchange-point-and-mark)       ; o — swap selection ends
(define-key "evil-visual" "u" 'evil-visual-downcase)          ; u — lowercase selection
(define-key "evil-visual" "shift-u" 'evil-visual-upcase)      ; U — uppercase selection
(define-key "evil-visual" "~" 'evil-visual-toggle-case)       ; ~ — toggle case
(define-key "evil-visual" "g q" 'evil-visual-fill)            ; gq — format selected lines
(define-key "evil-visual" "g w" 'evil-visual-fill)
(define-key "evil-visual" ">" 'evil-visual-indent)            ; > / < — shift selected lines
(define-key "evil-visual" "<" 'evil-visual-outdent)
(define-key "evil-visual" "shift-j" 'evil-visual-join)        ; J — join selected lines
(define-key "evil-visual" "r" 'evil-visual-replace-char)      ; r<char> — replace every char
(define-key "evil-visual" "shift-w" 'forward-bigword-start)   ; W / B / E — WORD motions
(define-key "evil-visual" "shift-b" 'backward-bigword-start)
(define-key "evil-visual" "shift-e" 'forward-bigword-end)
(define-key "evil-visual" "%" 'evil-matchit-visual)           ; % — extend to matching bracket
(define-key "evil-visual" "ctrl-d" 'evil-scroll-down)         ; half-page, extending
(define-key "evil-visual" "ctrl-u" 'evil-scroll-up)
(define-key "evil-visual" "v" 'evil-normal-state)             ; v again exits
(define-key "evil-visual" "shift-v" 'evil-visual-line-state)  ; switch to line-wise
(define-key "evil-visual" "ctrl-v" 'evil-visual-block-state)  ; switch to block-wise
(define-key "evil-visual" "escape" 'evil-normal-state)
(for-each
 (lambda (km)
   (define-key km "'" "evil-goto-mark-line %c")
   (define-key km "`" "evil-goto-mark %c")
   (define-key km "shift-h" 'evil-window-top-visual)
   (define-key km "shift-m" 'evil-window-middle-visual)
   (define-key km "shift-l" 'evil-window-bottom-visual))
 (list "evil-visual" "evil-visual-line"))

;; --- visual line state (`V`: whole-line selection; j/k extend by line) -------
;; The selection is rounded out to whole lines in the core, so the motions here
;; just move point; `d`/`y` act line-wise. `I`/`A` are vim's blockwise insert /
;; append: they drop a cursor at the start / end of every selected line, so the
;; text you type (then Escape) lands on all of them at once.
(define-key "evil-visual-line" "h" 'backward-char)
(define-key "evil-visual-line" "l" 'forward-char)
(define-key "evil-visual-line" "j" 'next-line)
(define-key "evil-visual-line" "k" 'previous-line)
(define-key "evil-visual-line" "w" 'forward-word-start)
(define-key "evil-visual-line" "b" 'backward-word-start)
(define-key "evil-visual-line" "0" 'move-beginning-of-line)
(define-key "evil-visual-line" "$" 'move-end-of-line)
(define-key "evil-visual-line" "g g" 'beginning-of-buffer)
(define-key "evil-visual-line" "shift-g" 'end-of-buffer)
(define-key "evil-visual-line" "d" 'evil-visual-line-delete)
(define-key "evil-visual-line" "x" 'evil-visual-line-delete)
(define-key "evil-visual-line" "y" 'evil-visual-line-yank)
(define-key "evil-visual-line" "c" 'evil-visual-line-change)   ; c/s — change selected lines
(define-key "evil-visual-line" "s" 'evil-visual-line-change)
(define-key "evil-visual-line" "o" 'exchange-point-and-mark)   ; o — swap selection ends
(define-key "evil-visual-line" "u" 'evil-visual-downcase)      ; u — lowercase selection
(define-key "evil-visual-line" "shift-u" 'evil-visual-upcase)  ; U — uppercase selection
(define-key "evil-visual-line" "~" 'evil-visual-toggle-case)   ; ~ — toggle case
(define-key "evil-visual-line" "p" 'evil-visual-paste)         ; p — paste over selection
(define-key "evil-visual-line" "{" 'evil-backward-paragraph-visual) ; { / } — by paragraph
(define-key "evil-visual-line" "}" 'evil-forward-paragraph-visual)
(define-key "evil-visual-line" "g q" 'evil-visual-fill)        ; gq — format selected lines
(define-key "evil-visual-line" "g w" 'evil-visual-fill)
(define-key "evil-visual-line" ">" 'evil-visual-indent)        ; > / < — shift selected lines
(define-key "evil-visual-line" "<" 'evil-visual-outdent)
(define-key "evil-visual-line" "shift-j" 'evil-visual-join)    ; J — join selected lines
(define-key "evil-visual-line" "r" 'evil-visual-replace-char)  ; r<char> — replace every char
(define-key "evil-visual-line" "shift-w" 'forward-bigword-start)
(define-key "evil-visual-line" "shift-e" 'forward-bigword-end)
(define-key "evil-visual-line" "ctrl-d" 'evil-scroll-down)
(define-key "evil-visual-line" "ctrl-u" 'evil-scroll-up)
(define-key "evil-visual-line" "shift-i" 'evil-visual-insert)  ; I — insert at each line start
(define-key "evil-visual-line" "shift-a" 'evil-visual-append)  ; A — append at each line end
(define-key "evil-visual-line" "v" 'evil-visual-state)         ; switch to charwise visual
(define-key "evil-visual-line" "shift-v" 'evil-normal-state)   ; V again exits
(define-key "evil-visual-line" "ctrl-v" 'evil-visual-block-state) ; switch to block-wise
(define-key "evil-visual-line" "escape" 'evil-normal-state)

;; --- visual block state (`C-v`: rectangular column selection) ----------------
;; The selection is a rectangle between two corners; the renderer highlights only
;; the column strip (not whole lines). Motions move the cursor corner. `I`/`A`
;; insert/append down the block's left/right column on every row at once (vim's
;; block-insert); `d`/`x` delete the rectangle and `y` yanks it.
(define-key "evil-visual-block" "h" 'backward-char)
(define-key "evil-visual-block" "l" 'forward-char)
(define-key "evil-visual-block" "j" 'next-line)
(define-key "evil-visual-block" "k" 'previous-line)
(define-key "evil-visual-block" "w" 'forward-word-start)
(define-key "evil-visual-block" "b" 'backward-word-start)
(define-key "evil-visual-block" "0" 'move-beginning-of-line)
(define-key "evil-visual-block" "$" 'move-end-of-line)
(define-key "evil-visual-block" "g g" 'beginning-of-buffer)
(define-key "evil-visual-block" "shift-g" 'end-of-buffer)
(define-key "evil-visual-block" "d" 'evil-block-delete)
(define-key "evil-visual-block" "x" 'evil-block-delete)
(define-key "evil-visual-block" "y" 'evil-block-yank)
(define-key "evil-visual-block" "c" 'evil-block-change)        ; c — change the rectangle
(define-key "evil-visual-block" "s" 'evil-block-change)
(define-key "evil-visual-block" "o" 'exchange-point-and-mark)  ; o — swap block corners
(define-key "evil-visual-block" "r" 'evil-block-replace-char)  ; r<char> — replace the rectangle
(define-key "evil-visual-block" "shift-w" 'forward-bigword-start)
(define-key "evil-visual-block" "shift-e" 'forward-bigword-end)
(define-key "evil-visual-block" "shift-i" 'evil-block-insert)  ; I — insert down the left column
(define-key "evil-visual-block" "shift-a" 'evil-block-append)  ; A — append down the right column
(define-key "evil-visual-block" "v" 'evil-visual-state)        ; switch to charwise visual
(define-key "evil-visual-block" "shift-v" 'evil-visual-line-state) ; switch to line-wise
(define-key "evil-visual-block" "ctrl-v" 'evil-normal-state)   ; C-v again exits
(define-key "evil-visual-block" "escape" 'evil-normal-state)

;; --- binding commands by identity (not by name string) ----------------------
;; A keymap stores a NAME string and evals "(name)" on the keypress. To let you
;; bind the *command itself* — `lsp-find-references`, not "lsp-find-references" —
;; so editor tooling (go-to-def, find-refs, typo/unbound warnings) sees a real
;; reference, we keep a table of bound procedures and dispatch through it.
;; Every command (builtins included) is an ordinary Steel procedure, so a bare
;; identifier is a value we can store here.
(define *evil-cmd-table* (hash))
(define (evil-run-key key)
  (let ((cmd (hash-try-get *evil-cmd-table* key)))
    (when cmd (cmd))))
;; Bind `cmd` under `chord` in keymap `km`. `cmd` is a 'name symbol (the canonical
;; config form), a procedure (a bare identifier — lets editor tooling resolve the
;; reference), or a "name" string for back-compat.
(define (evil-bind-key km chord cmd)
  (cond
    ((symbol? cmd) (define-key km chord (symbol->string cmd))) ; 'name → (name)
    ((string? cmd) (define-key km chord cmd))                  ; "name" → (name), back-compat
    (else                                                      ; identity → table dispatch
     (let ((key (string-append km " " chord)))
       (set! *evil-cmd-table* (hash-insert *evil-cmd-table* key cmd))
       (define-key km chord (string-append "evil-run-key \"" key "\""))))))

;; --- per-major-mode evil bindings (Emacs `evil-define-key` / evil-leader) ----
;; Bindings in the keymap "evil-<state>:<major-mode>" shadow the global
;; "evil-<state>" keymap, but only in buffers of that major mode. The major mode
;; is the language name for code buffers ("rust", "go", "typescript", "steel",
;; "python", …) — the same name the highlighter uses.
;;
;;   (evil-define-key "normal" "rust" "space g g" 'lsp-find-definition)
;;
;; binds SPC g g to go-to-definition in Rust buffers only (pass a 'name symbol; a
;; bare command identifier or a "name" string also work). Elsewhere SPC g g keeps
;; whatever the global evil-normal keymap says.

;; Bind one key (procedure or name) in a major-mode-scoped evil keymap.
(define (evil-define-key state mode chord command)
  (evil-bind-key (string-append "evil-" state ":" mode) chord command))

;; Bind many (chord . command) pairs across several major modes for one state —
;; the analogue of Emacs's `(dolist (mode …) (evil-leader/set-key-for-mode …))`.
(define (evil-define-keys-for-modes state modes bindings)
  (for-each
   (lambda (mode)
     (for-each
      (lambda (b) (evil-define-key state mode (car b) (cdr b)))
      bindings))
   modes))

;; --- leader key -------------------------------------------------------------
;; Doom/Spacemacs-style leader (Emacs `evil-leader`). Plain evil has no leader;
;; this opt-in layer lets you bind chords *relative* to a leader key and swap
;; that key in one place. The leader is a normal config variable — `SPC` by
;; default; override it in init.scm with `evil-set-leader` BEFORE defining your
;; leader bindings, since each chord is expanded when it's bound (changing the
;; leader afterward does not move bindings already made):
;;
;;   (evil-set-leader "comma")                    ; or "space" (default), "space space", …
;;   (evil-set-leader-alt "ctrl-space")           ; or "alt-space" (default), "" for none
;;   (evil-leader-set "n n" 'next-buffer)         ; <leader> n n   (global, normal state)
;;   (evil-leader-set-for-modes '("rust" "go")    ; <leader> g g / g r in those modes only
;;     (list (cons "g g" 'lsp-find-definition)
;;           (cons "g r" 'lsp-rename)))
;;
;; Commands may be a 'name symbol (canonical), a "name" string, or a bare
;; procedure identifier (the latter lets editor tooling resolve the reference),
;; same as `evil-define-key`.
;;
;; Global leader chords work EVERYWHERE, not just in file buffers: each one is
;; also bound in the "evil-leader" keymap, which the editor consults in special
;; buffers (magit, dired, package dashboards — whose own keymaps otherwise hide
;; the global evil-normal map) and terminals. Where a plain key must type — a
;; terminal, Insert state — the ALT leader (Doom's `doom-leader-alt-key`, M-SPC
;; by default) opens the same chords: `M-SPC n o` in a pod shell works like
;; `SPC n o` in a file.
(define *evil-leader* "space")
(define *evil-leader-alt* "alt-space")

;; Set the leader to a chord string ("space", "comma", "space space", …).
(define (evil-set-leader key) (set! *evil-leader* key))

;; Set the alt leader (a Ctrl/Alt chord, so it can't collide with typing), or
;; "" to disable it. Like the leader, set it BEFORE your bindings.
(define (evil-set-leader-alt key) (set! *evil-leader-alt* key))

;; Prefix a chord with the current leader; "" yields the bare leader key.
(define (evil-leader-chord chord)
  (if (equal? chord "") *evil-leader* (string-append *evil-leader* " " chord)))

;; Every full sequence a leader chord is reachable by: the leader, plus the alt
;; leader when one is set.
(define (evil-leader-sequences chord)
  (let ((main (evil-leader-chord chord)))
    (if (equal? *evil-leader-alt* "")
        (list main)
        (list main
              (if (equal? chord "")
                  *evil-leader-alt*
                  (string-append *evil-leader-alt* " " chord))))))

;; Bind one leader chord in the global normal-state map (`evil-leader/set-key`)
;; and in the "evil-leader" map special buffers / terminals / Insert consult.
(define (evil-leader-set chord command)
  (for-each
   (lambda (seq)
     (evil-bind-key "evil-normal" seq command)
     (evil-bind-key "evil-leader" seq command))
   (evil-leader-sequences chord)))

;; Bind many (chord . command) leader pairs across major modes for normal state
;; (`evil-leader/set-key-for-mode` over a list of modes). Works for special
;; buffer modes too ("k8s-mode", "magit-status", …): their evil-normal:<mode>
;; overlay shadows the global leader there.
(define (evil-leader-set-for-modes modes bindings)
  (evil-define-keys-for-modes "normal" modes
    (apply append
           (map (lambda (b)
                  (map (lambda (seq) (cons seq (cdr b)))
                       (evil-leader-sequences (car b))))
                bindings))))

;; --- evil-collection: special read-only buffers -----------------------------
;; Macros' "special" buffers (magit, dired, dired-sidebar, grep, …) drive their own
;; keymaps, so the global evil keymaps don't apply there. Like Emacs's
;; `evil-collection`, this layer overlays vim-style keys onto those buffers.
;;
;; The editor consults an `evil-normal:<major>` keymap for a special buffer ONLY
;; when evil-mode is active, and BEFORE the buffer's own keymap. So whatever we
;; bind here is (a) inert under plain Emacs — the buffer's Emacs-default keys
;; show through untouched — and (b) able to shadow a default key under evil
;; (e.g. magit's `k`=discard becomes `k`=up; magit then re-exposes discard on
;; `x` in its own `evil-normal:magit-status` overlay). It's all just keymaps:
;; override any of this in init.scm with `(define-key "evil-normal:<mode>" …)`.

;; Read-only special modes that get vim motion. Extend in init.scm by
;; `(set! *evil-collection-modes* (cons "my-mode" *evil-collection-modes*))`.
;; (Editable special buffers like `git-commit-mode` are intentionally absent —
;; you type into them, so they use the normal insert/normal states.)
(define *evil-collection-modes*
  (list "magit-status" "magit-log-mode" "dired-mode" "dired-sidebar-mode"
        "ibuffer-mode" "grep-mode" "occur-mode" "compile-mode"
        "diagnostics-mode" "help-mode" "diff-mode" "welcome-mode"
        "org-roam-backlinks-mode" "org-roam-unlinked-mode"
        "org-roam-graph-mode"))

;; Vim motion shared by every special buffer. Only `j`/`k` by default: single
;; `g` is `refresh` in several of these modes, so a `g g` top-motion would
;; shadow it — left out deliberately. `n`/`p` keep working from the base keymap.
;; Parameterized over the overlay prefix so the helix collection (helix.scm,
;; which loads after this file) shares the exact same binding set.
(define (modal-collection-bind-mode prefix mode)
  (let ((km (string-append prefix mode)))
    (define-key km "j" 'next-line)
    (define-key km "k" 'previous-line)))
(define (evil-collection-bind-mode mode)
  (modal-collection-bind-mode "evil-normal:" mode))

;; Install the overlay across every mode in `*evil-collection-modes*`. Called
;; once below as part of loading the evil layer; safe to re-run after extending
;; the list.
(define (evil-collection-init)
  (for-each evil-collection-bind-mode *evil-collection-modes*))
(evil-collection-init)

;; Plain-text result buffers (compile / grep / occur output) are read-only text
;; you read and scroll like a file, so they earn the full read-only motion set on
;; top of the shared `j`/`k`: `h`/`l`, word motions, line ends, `G`, and half-page
;; `C-d`/`C-u` (the editor routes those ctrl chords through this overlay too).
;; `g g` is deliberately left off so a single `g` stays free for each mode's own
;; key (compile's recompile, grep's refresh); jump to the top with `C-u` or `k`.
;; Not folded into the shared `evil-collection-bind-mode` because the modal
;; buffers (magit/dired) already bind several of these keys (`b`, `l`, `$`, `^`).
(define (modal-collection-bind-readonly-motion prefix mode)
  (let ((km (string-append prefix mode)))
    (define-key km "h" 'backward-char)
    (define-key km "l" 'forward-char)
    (define-key km "w" 'forward-word-start)
    (define-key km "b" 'backward-word-start)
    (define-key km "e" 'evil-forward-word-end)
    (define-key km "0" 'move-beginning-of-line)
    (define-key km "$" 'move-end-of-line)
    (define-key km "^" 'evil-first-non-blank)
    (define-key km "shift-g" 'evil-goto-last-line)
    (define-key km "ctrl-d" 'evil-scroll-down)
    (define-key km "ctrl-u" 'evil-scroll-up)))
(define (evil-collection-bind-readonly-motion mode)
  (modal-collection-bind-readonly-motion "evil-normal:" mode))

(for-each evil-collection-bind-readonly-motion
          (list "compile-mode" "grep-mode" "occur-mode"))

;; --- counts ------------------------------------------------------------------
;; These take their count as `(prefix-count)` and run once (`5G` = line 5,
;; `3yy` = three lines, `2fx` = second x). Everything else under a count simply
;; repeats (`3w`, `3dw`, …), squashed into one undo step by the core.
(for-each set-count-aware!
          '(evil-delete-line evil-yank-line evil-change-line evil-delete-char
            evil-join-line evil-goto-first-line evil-goto-last-line
            evil-replace-char indent-region outdent-region
            evil-find-char-forward evil-find-char-backward
            evil-till-char-forward evil-till-char-backward
            evil-find-char-forward-visual evil-find-char-backward-visual
            evil-till-char-forward-visual evil-till-char-backward-visual
            evil-find-repeat evil-find-repeat-reverse
            evil-find-repeat-visual evil-find-repeat-reverse-visual
            evil-forward-sentence evil-backward-sentence
            evil-forward-sentence-visual evil-backward-sentence-visual
            evil-repeat evil-macro-play
            evil-window-top evil-window-bottom
            evil-window-top-visual evil-window-bottom-visual))

;; --- jumps ---------------------------------------------------------------------
;; Commands that are "jumps" in Vim's sense: run from a key, they record the
;; jump list (C-o / C-i) and the '' mark first. Extend with your own
;; `(set-jump-command! 'my-command)`.
(for-each set-jump-command!
          '(evil-goto-first-line evil-goto-last-line beginning-of-buffer end-of-buffer
            evil-search-forward evil-search-backward evil-search-next evil-search-previous
            evil-search-word-forward evil-search-word-backward
            evil-matchit evil-forward-paragraph evil-backward-paragraph
            evil-forward-sentence evil-backward-sentence
            evil-window-top evil-window-middle evil-window-bottom
            lsp-find-definition lsp-find-references lsp-find-implementation
            lsp-find-type-definition imenu))
