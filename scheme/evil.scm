;; evil-mode — Vim-style modal editing, built on Macros' keymaps. The Rust side
;; only decides *when* plain keys are commands (Normal/Visual) vs. text (Insert);
;; every binding below is Scheme and fully overridable. Emacs keybindings are
;; the default; opt in to modal editing with (evil-mode) in your config (and
;; (evil-disable) to turn it back off). This mirrors GNU Emacs's evil-mode:
;; plain Vim modal editing, no leader key. Add your own leader in init.scm if
;; you want a Doom/Spacemacs-style menu.

;; --- composite operators (commands enqueue in order when a fn is called) ----
(define (evil-append)      (forward-char) (evil-insert-state))
(define (evil-append-line) (move-end-of-line) (evil-insert-state))
(define (evil-insert-line) (evil-first-non-blank) (evil-insert-state)) ; I — first non-blank
(define (evil-open-below)  (move-end-of-line) (newline) (evil-insert-state))
(define (evil-open-above)  (move-beginning-of-line) (open-line) (evil-insert-state))
;; dd — delete the whole line linewise (including its trailing newline) and
;; copy it to the kill ring. Selecting BOL→start-of-next-line and killing the
;; region handles empty lines correctly; the old `kill-line` + `delete-char`
;; form swallowed the first character of the *next* line on an empty line.
(define (evil-delete-line)
  (move-beginning-of-line) (set-mark) (move-end-of-line) (forward-char)
  (kill-region) (evil-set-linewise))
;; Visual-mode delete/yank are character-wise (the selection's exact range).
(define (evil-visual-delete) (kill-region) (evil-set-charwise) (evil-normal-state))
(define (evil-visual-yank)   (kill-ring-save) (evil-set-charwise) (evil-normal-state))
;; Visual-LINE delete/yank are line-wise: the visual region already covers whole
;; lines (incl. the trailing newline), so a following `p`/`P` pastes the vim way.
(define (evil-visual-line-delete) (kill-region) (evil-set-linewise) (evil-normal-state))
(define (evil-visual-line-yank)   (kill-ring-save) (evil-set-linewise) (evil-normal-state))
;; Normal-mode yanks (copy → kill ring AND the system clipboard). `kill-ring-save`
;; syncs the OS clipboard, so these reach cmd-v in other apps. Cursor is restored
;; to where the yank started, vim-style. Each marks the yank line- or char-wise
;; so a following `p`/`P` pastes the vim way.
(define (evil-yank-line)
  ;; yy — line-wise: grab the whole line *including* its trailing newline, so
  ;; `p` drops it on a fresh line below.
  (move-beginning-of-line) (set-mark) (move-end-of-line) (forward-char)
  (kill-ring-save) (evil-set-linewise) (move-beginning-of-line))
(define (evil-yank-to-eol)
  (let ((p (point)))
    (set-mark) (move-end-of-line) (kill-ring-save) (evil-set-charwise)
    (goto-char p)))
(define (evil-yank-word)
  ;; yw — character-wise, using the vim `w` motion (to the start of the next
  ;; word) so the trailing whitespace is included, matching `w`.
  (let ((p (point)))
    (set-mark) (forward-word-start) (kill-ring-save) (evil-set-charwise)
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
(define (evil-goto-first-line)
  (let* ((mx (point-max))
         (end (let loop ((i 0))
                (cond ((>= i mx) mx)
                      ((equal? (buffer-substring i (+ i 1)) "\n") i)
                      (else (loop (+ i 1)))))))
    (goto-char (evil-skip-blanks-to 0 end))))
(define (evil-goto-last-line)
  (let* ((mx (point-max))
         ;; ignore one trailing newline so a file ending in "\n" doesn't land us
         ;; on a phantom empty line below the text.
         (end (if (and (> mx 0) (equal? (buffer-substring (- mx 1) mx) "\n")) (- mx 1) mx))
         (start (let loop ((i end))
                  (cond ((<= i 0) 0)
                        ((equal? (buffer-substring (- i 1) i) "\n") i)
                        (else (loop (- i 1)))))))
    (goto-char (evil-skip-blanks-to start end))))

;; e — end of word: land ON the last char of the current/next word. Emacs
;; `forward-word` stops just AFTER a word, so nudge in by one then step back; the
;; leading forward-char also makes `e` advance when already sitting on a word end.
(define (evil-forward-word-end) (forward-char) (forward-word) (backward-char))

;; x — delete the char under the cursor INTO the register (Vim `x` = `dl`), so a
;; following `p` can put it back. Char-wise; a no-op on an empty line.
(define (evil-delete-char)
  (let* ((b (line-bounds)) (eol (cadr b)) (p (point)))
    (when (< p eol)
      (set-mark) (forward-char) (kill-region) (evil-set-charwise))))

;; D / d$ — delete to end of line.  C / c$ — change to end of line.
(define (evil-delete-to-eol)
  (set-mark) (move-end-of-line) (kill-region) (evil-set-charwise))
(define (evil-change-to-eol)
  (set-mark) (move-end-of-line) (kill-region) (evil-insert-state))

;; cc / S — change the whole line: clear its text and insert (the line stays).
(define (evil-change-line)
  (move-beginning-of-line) (set-mark) (move-end-of-line) (kill-region) (evil-insert-state))

;; s — substitute the char under the cursor (delete it, then insert).
(define (evil-substitute-char) (evil-delete-char) (evil-insert-state))

;; dw / db, cw / cb — word operators. `dw` runs to the next word start (Vim `w`,
;; trailing space included); `cw` stops at the word end (Vim treats `cw` as `ce`).
(define (evil-delete-word)      (set-mark) (forward-word-start) (kill-region) (evil-set-charwise))
(define (evil-delete-word-back) (set-mark) (backward-word)      (kill-region) (evil-set-charwise))
(define (evil-change-word)      (set-mark) (forward-word)       (kill-region) (evil-insert-state))
(define (evil-change-word-back) (set-mark) (backward-word)      (kill-region) (evil-insert-state))

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
(define (evil-join-line)
  (let* ((b (line-bounds)) (e (cadr b)) (mx (point-max)))
    (when (< e mx)
      (let loop ((i (+ e 1)))
        (if (and (< i mx)
                 (let ((c (buffer-substring i (+ i 1)))) (or (equal? c " ") (equal? c "\t"))))
            (loop (+ i 1))
            (replace-region e i " "))))))

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

;; --- q / @@ — keyboard macros ---------------------------------------------------
;; Vim-style recording on the Emacs kbd-macro engine (C-x ( / C-x ) / C-x e):
;; one anonymous register, `q` toggles recording, `@@` (or `@q`) replays. The
;; name contains "kbd-macro" so the key recorder skips the closing toggle
;; (exprs mentioning kbd-macro are never recorded into the macro itself).
(define *evil-kbd-macro-recording* #f)
(define (evil-kbd-macro-toggle)
  (if *evil-kbd-macro-recording* (end-kbd-macro) (start-kbd-macro))
  (set! *evil-kbd-macro-recording* (not *evil-kbd-macro-recording*)))

;; --- f / F / t / T char find (+ ; and , repeat) ---------------------------------
;; Built on the core `read-char` primitive: pressing `f` arms a one-keystroke
;; capture; the next key arrives in the dispatch callback as a string. The
;; motion is line-local, vim-style. `;` repeats the last find, `,` reverses it;
;; the visual variants extend the selection instead of moving.
(define *evil-find-char* #f)     ; last sought char, for ; and ,
(define *evil-find-till* #f)     ; t/T (stop short) vs f/F (land on it)
(define *evil-find-forward* #t)

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

(define (evil-find-go tgt extend)
  (when tgt (if extend (select-to tgt) (goto-char tgt))))
(define (evil-find-dispatch ch)         ; armed from normal state
  (set! *evil-find-char* ch)
  (evil-find-go (evil-find-target-at (point) ch *evil-find-till* *evil-find-forward*) #f))
(define (evil-find-dispatch-visual ch)  ; armed from visual states — extends
  (set! *evil-find-char* ch)
  (evil-find-go (evil-find-target-at (point) ch *evil-find-till* *evil-find-forward*) #t))
(define (evil-find-arm till forward callback prompt)
  (set! *evil-find-till* till)
  (set! *evil-find-forward* forward)
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
    (evil-find-go (evil-find-target-at (point) *evil-find-char* *evil-find-till* forward)
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
      (evil-search-next))))
(define (evil-search-word-forward)  (evil-search-word #t))
(define (evil-search-word-backward) (evil-search-word #f))

;; --- % — matching bracket --------------------------------------------------------
;; Jump between a bracket and its match, nesting-aware. The bracket is the char
;; under the cursor, or the first one to the right on the current line (vim).
;; A plain char scan: strings and comments are not understood.
(define (evil-matchit)
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
        (when tgt (goto-char tgt))))))

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
              ((equal? op "select") (set-region s e)))))))

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
 (lambda (opk)
   (for-each
    (lambda (to)
      (for-each
       (lambda (kind)
         (define-key "evil-normal"
           (string-append (car opk) " " kind " " (car to))
           (string-append "evil-object-cmd \"" (cdr opk) "\" \"" kind "\" \"" (cdr to) "\"")))
       (list "i" "a")))
    *evil-text-objects*))
 (list (cons "c" "change") (cons "d" "delete") (cons "y" "yank")))
(for-each
 (lambda (to)
   (for-each
    (lambda (kind)
      (define-key "evil-visual"
        (string-append kind " " (car to))
        (string-append "evil-object-cmd \"select\" \"" kind "\" \"" (cdr to) "\"")))
    (list "i" "a")))
 *evil-text-objects*)

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
(define (evil-visual-line-change) (kill-region) (evil-set-linewise) (evil-open-above))

;; --- normal state -----------------------------------------------------------
;; motion
(define-key "evil-normal" "h" 'backward-char)
(define-key "evil-normal" "l" 'forward-char)
(define-key "evil-normal" "j" 'next-line)
(define-key "evil-normal" "k" 'previous-line)
(define-key "evil-normal" "w" 'forward-word-start)
(define-key "evil-normal" "b" 'backward-word)
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
(define-key "evil-normal" "g q q" 'evil-fill-line)       ; gqq  — format current line
(define-key "evil-normal" "g q g q" 'evil-fill-line)     ; gqgq — same, Vim's doubled form
(define-key "evil-normal" "g q i p" 'evil-fill-paragraph); gqip — format paragraph
(define-key "evil-normal" "g q a p" 'evil-fill-paragraph); gqap
(define-key "evil-normal" "g w w" 'evil-fill-line-keep)  ; gww  — format line, keep cursor
(define-key "evil-normal" "g u u" 'evil-downcase-line)   ; guu — lowercase line
(define-key "evil-normal" "g shift-u shift-u" 'evil-upcase-line) ; gUU — uppercase line
(define-key "evil-normal" "g ~ ~" 'evil-toggle-case-line); g~~ — toggle case of line
(define-key "evil-normal" "g d" 'lsp-find-definition)    ; gd — go to definition (LSP)
(define-key "evil-normal" "shift-k" 'lsp-hover)          ; K  — doc at point (LSP)
(define-key "evil-normal" "q" 'evil-kbd-macro-toggle)    ; q — record macro (toggle)
(define-key "evil-normal" "@ @" 'call-last-kbd-macro)    ; @@ — replay macro
(define-key "evil-normal" "@ q" 'call-last-kbd-macro)    ; @q — same (one register)
(define-key "evil-normal" "d d" 'evil-delete-line)       ; dd
(define-key "evil-normal" "d w" 'evil-delete-word)       ; dw
(define-key "evil-normal" "d b" 'evil-delete-word-back)  ; db
(define-key "evil-normal" "d $" 'evil-delete-to-eol)     ; d$
(define-key "evil-normal" "shift-d" 'evil-delete-to-eol) ; D  (= d$)
(define-key "evil-normal" "c c" 'evil-change-line)       ; cc
(define-key "evil-normal" "c w" 'evil-change-word)       ; cw  (Vim treats as ce)
(define-key "evil-normal" "c b" 'evil-change-word-back)  ; cb
(define-key "evil-normal" "c $" 'evil-change-to-eol)     ; c$
(define-key "evil-normal" "shift-c" 'evil-change-to-eol) ; C  (= c$)
(define-key "evil-normal" "y y" 'evil-yank-line)         ; yy  (→ system clipboard)
(define-key "evil-normal" "shift-y" 'evil-yank-line)     ; Y (= yy)
(define-key "evil-normal" "y w" 'evil-yank-word)         ; yw
(define-key "evil-normal" "y $" 'evil-yank-to-eol)       ; y$
(define-key "evil-normal" "p" 'evil-paste-after)        ; p  (line-wise → below)
(define-key "evil-normal" "shift-p" 'evil-paste-before) ; P  (line-wise → above)
(define-key "evil-normal" "u" 'undo)
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

;; --- insert state -----------------------------------------------------------
(define-key "evil-insert" "escape" 'evil-normal-state)

;; --- visual state (motions extend; the mark is active) ----------------------
(define-key "evil-visual" "h" 'backward-char)
(define-key "evil-visual" "l" 'forward-char)
(define-key "evil-visual" "j" 'next-line)
(define-key "evil-visual" "k" 'previous-line)
(define-key "evil-visual" "w" 'forward-word-start)
(define-key "evil-visual" "b" 'backward-word)
(define-key "evil-visual" "0" 'move-beginning-of-line)
(define-key "evil-visual" "$" 'move-end-of-line)
(define-key "evil-visual" "g g" 'beginning-of-buffer)
(define-key "evil-visual" "shift-g" 'end-of-buffer)
(define-key "evil-visual" "e" 'evil-forward-word-end)          ; e — end of word (extends)
(define-key "evil-visual" "{" 'evil-backward-paragraph-visual) ; { / } — extend by paragraph
(define-key "evil-visual" "}" 'evil-forward-paragraph-visual)
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
(define-key "evil-visual" "escape" 'evil-normal-state)

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
(define-key "evil-visual-line" "b" 'backward-word)
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
(define-key "evil-visual-block" "b" 'backward-word)
(define-key "evil-visual-block" "0" 'move-beginning-of-line)
(define-key "evil-visual-block" "$" 'move-end-of-line)
(define-key "evil-visual-block" "g g" 'beginning-of-buffer)
(define-key "evil-visual-block" "shift-g" 'end-of-buffer)
(define-key "evil-visual-block" "d" 'evil-block-delete)
(define-key "evil-visual-block" "x" 'evil-block-delete)
(define-key "evil-visual-block" "y" 'evil-block-yank)
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
;;   (evil-leader-set "n n" 'next-buffer)         ; <leader> n n   (global, normal state)
;;   (evil-leader-set-for-modes '("rust" "go")    ; <leader> g g / g r in those modes only
;;     (list (cons "g g" 'lsp-find-definition)
;;           (cons "g r" 'lsp-rename)))
;;
;; Commands may be a 'name symbol (canonical), a "name" string, or a bare
;; procedure identifier (the latter lets editor tooling resolve the reference),
;; same as `evil-define-key`.
(define *evil-leader* "space")

;; Set the leader to a chord string ("space", "comma", "space space", …).
(define (evil-set-leader key) (set! *evil-leader* key))

;; Prefix a chord with the current leader; "" yields the bare leader key.
(define (evil-leader-chord chord)
  (if (equal? chord "") *evil-leader* (string-append *evil-leader* " " chord)))

;; Bind one leader chord in the global normal-state map (`evil-leader/set-key`).
(define (evil-leader-set chord command)
  (evil-bind-key "evil-normal" (evil-leader-chord chord) command))

;; Bind many (chord . command) leader pairs across major modes for normal state
;; (`evil-leader/set-key-for-mode` over a list of modes).
(define (evil-leader-set-for-modes modes bindings)
  (evil-define-keys-for-modes "normal" modes
    (map (lambda (b) (cons (evil-leader-chord (car b)) (cdr b))) bindings)))

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
    (define-key km "b" 'backward-word)
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
