;; org.scm — Org mode, implemented entirely in Scheme on the editor primitives.
;;
;; Five layers, each independently useful:
;;   1. fontification + heading visibility cycling (TAB / S-TAB)
;;   2. structure editing — TODO/tags/priority cycling, promote/demote, move
;;      subtree, insert heading, toggle checkbox
;;   3. an Org AST parser (`org-parse`) that both export and agenda consume
;;   4. agenda + capture across a set of .org files
;;   5. tables (align) + babel (execute source blocks)
;;
;; A `.org` file activates Org mode via `find-file-hook` (it calls
;; `set-major-mode`, so the `org-mode` keymap below takes effect). Fontification
;; paints the shared overlay-face layer (like rainbow-delimiters); rainbow skips
;; .org buffers so the two don't fight over that layer.

;; ===========================================================================
;; Faces
;; ===========================================================================
;; Heading levels cycle through this palette (1-based, wraps at 8).
(define *org-level-colors*
  (list "#8ec07c" "#83a598" "#d3869b" "#fabd2f" "#fe8019" "#b8bb26" "#fb4934" "#458588"))

(define (org--register-level-faces)
  (let loop ((cs *org-level-colors*) (i 1))
    (when (not (null? cs))
      (set-face (string-append "org-level-" (number->string i)) (car cs))
      (loop (cdr cs) (+ i 1)))))
(org--register-level-faces)

(set-face "org-todo" "#fb4934")
(set-face "org-done" "#8ec07c")
(set-face "org-tag" "#83a598")
(set-face "org-priority" "#d3869b")
(set-face "org-meta" "#928374")
(set-face "org-drawer" "#665c54")
(set-face "org-block" "#bdae93")
(set-face "org-table" "#83a598")
(set-face "org-link" "#458588")
(set-face "org-date" "#b16286")
(set-face "org-bold" "#ebdbb2")
(set-face "org-italic" "#d5c4a1")
(set-face "org-verbatim" "#b8bb26")
(set-face "org-code" "#b8bb26")
(set-face "org-checkbox" "#fabd2f")
(set-face "org-checkbox-done" "#8ec07c")
(set-face "org-strike" "#928374")
(set-face "org-underline" "#83a598")
(set-face "org-footnote" "#d3869b")

;; The TODO keyword vocabulary, and the order C-c C-t rotates through it. Like
;; Emacs `org-todo-keywords`, every keyword here is part of the cycle, so active
;; states come first and "done" states last (DONE/CANCELLED — see below). The
;; cycle is nothing → each of these → each custom keyword → nothing. Trim or
;; reorder this list from init.scm to shape the cycle.
(define *org-todo-keywords*
  (list "TODO" "NEXT" "WAITING" "DOING" "HOLD" "DONE" "CANCELLED"))
;; Keywords that count as closed (green face, treated as done in the agenda).
(define *org-done-keywords* (list "DONE" "CANCELLED"))

;; Custom heading keywords: a user-defined array of labels, appended to the
;; C-c C-t cycle after the TODO vocabulary above. Empty by default — the
;; built-in ships no labels; define your own from init.scm. Each is recognized
;; on a headline like a TODO word but carries NO color of its own (it renders in
;; the heading title color) unless one is set in *org-custom-keyword-colors*.
;; A typical init.scm:
;;   (set! *org-custom-keywords* (list "BUG" "FEATURE" "IDEA"))
;;   (set! *org-custom-keyword-colors*               ; optional per-label colors
;;     (list (cons "BUG" "#fb4934") (cons "FEATURE" "#83a598")))
;;   (org--register-custom-keyword-faces)            ; apply the colors
;; They join the C-c C-t rotation automatically; bind a key only if you also
;; want to jump straight to one, e.g.
;;   (define (org-toggle-bug) (org-set-keyword "BUG"))
;;   (define-key "org-mode" "ctrl-c ctrl-b" "org-toggle-bug")
;;   (register-command "org-toggle-bug")
(define *org-custom-keywords* '())

;; Per-keyword colors: an alist (LABEL . "#rrggbb"). Empty by default, so every
;; custom keyword inherits the title color. Set from init.scm, then call
;; `org--register-custom-keyword-faces` to apply.
(define *org-custom-keyword-colors* '())

;; The face name carrying a custom keyword's color (e.g. "org-kw-BUG").
(define (org--keyword-face-name kw) (string-append "org-kw-" kw))

(define (org--custom-keyword? kw) (and (member kw *org-custom-keywords*) #t))

;; The user-defined color for `kw`, or #f if none is set (⇒ inherit the title).
(define (org--keyword-color kw)
  (let loop ((es *org-custom-keyword-colors*))
    (cond ((null? es) #f)
          ((equal? (car (car es)) kw) (cdr (car es)))
          (else (loop (cdr es))))))

;; Recognized on a headline: a TODO word or any custom keyword.
(define (org--known-keyword? kw)
  (or (member kw *org-todo-keywords*) (org--custom-keyword? kw)))

;; Register (or refresh) one face per *colored* custom keyword. Call again after
;; re-setting *org-custom-keyword-colors* to apply new colors.
(define (org--register-custom-keyword-faces)
  (for-each (lambda (e) (set-face (org--keyword-face-name (car e)) (cdr e)))
            *org-custom-keyword-colors*))
(org--register-custom-keyword-faces)

;; Collapse the leading stars on every headline from display (the text stays in
;; the buffer), so a heading shows only its colored title — all stars hidden, not
;; just the all-but-one of Emacs's `org-hide-leading-stars`. Disable from init.scm
;; with
;;   (set! *org-hide-leading-stars* #f)  ; then M-x org-mode, or reopen the file
(define *org-hide-leading-stars* #t)

;; Virtual indentation (Emacs `org-indent-mode`): indent each heading by its depth
;; and the body text beneath it one level further, all display-only — no spaces
;; are written to the buffer, so there is nothing for the cursor to step through.
;; A heading of level L is indented (L-1)*per-level columns and its body L*per.
(define *org-indent* #t)
(define *org-indent-per-level* 4)

;; ===========================================================================
;; Tiny utilities
;; ===========================================================================
;; UTF-8 byte width of one character (mirrors core's `utf8-length` per char).
(define (org--char-bytes ch)
  (let ((cp (char->integer ch)))
    (cond ((< cp 128) 1) ((< cp 2048) 2) ((< cp 65536) 3) (else 4))))

;; Drop the first `n` BYTES of string `s`, returning the remainder. The overlay
;; APIs are byte-based; this lets the inline scanner advance past a match it
;; located by byte offset.
(define (org--byte-drop s n)
  (if (<= n 0)
      s
      (let loop ((chars (string->list s)) (acc 0))
        (cond ((null? chars) "")
              ((>= acc n) (list->string chars))
              (else (loop (cdr chars) (+ acc (org--char-bytes (car chars)))))))))

(define (org--spaces k) (if (<= k 0) "" (string-append " " (org--spaces (- k 1)))))
(define (org--dashes k) (if (<= k 0) "" (string-append "-" (org--dashes (- k 1)))))
(define (org--stars n) (if (<= n 0) "" (string-append "*" (org--stars (- n 1)))))
(define (org--pad s w) (string-append s (org--spaces (- w (utf8-length s)))))
(define (org--concat lst) (if (null? lst) "" (string-append (car lst) (org--concat (cdr lst)))))

;; Trim leading and trailing spaces/tabs.
(define (org--trim s)
  (replace-regexp-in-string "^[ \t]+" ""
    (replace-regexp-in-string "[ \t]+$" "" s)))

;; Inclusive sublist `lines[a..b]`, clamped; '() when the range is empty.
(define (org--slice lines a b)
  (let ((n (length lines)))
    (if (or (> a b) (>= a n) (< b 0))
        '()
        (let loop ((ls lines) (i 0) (acc '()))
          (cond ((null? ls) (reverse acc))
                ((> i b) (reverse acc))
                ((>= i a) (loop (cdr ls) (+ i 1) (cons (car ls) acc)))
                (else (loop (cdr ls) (+ i 1) acc)))))))

;; ===========================================================================
;; Line predicates (pure; operate on a single line of text)
;; ===========================================================================
;; Heading depth (number of leading stars) if `line` is a headline, else #f.
(define (org--heading-level line)
  (let ((caps (regexp-captures "^(\\*+)([ \t]|$)" line)))
    (if (null? caps) #f (string-length (nth caps 1)))))

(define (org--block-begin? line) (>= (string-match "^[ \t]*#\\+[bB][eE][gG][iI][nN]_" line) 0))
(define (org--block-end? line)   (>= (string-match "^[ \t]*#\\+[eE][nN][dD]_" line) 0))
(define (org--keyword-line? line) (>= (string-match "^[ \t]*#\\+[A-Za-z_]+:" line) 0))
(define (org--comment-line? line) (>= (string-match "^[ \t]*#[ \t]" line) 0))
(define (org--drawer-line? line)  (>= (string-match "^[ \t]*:[A-Za-z]+:[ \t]*$" line) 0))
(define (org--table-line? line)   (>= (string-match "^[ \t]*\\|" line) 0))
(define (org--ulist-line? line)   (>= (string-match "^[ \t]*[-+][ \t]+" line) 0))
(define (org--olist-line? line)   (>= (string-match "^[ \t]*[0-9]+[.)][ \t]+" line) 0))
(define (org--hr-line? line)      (>= (string-match "^[ \t]*-----+[ \t]*$" line) 0))

;; A separator row in a table (|---+---|): a table line of only |-+:space.
(define (org--table-sep? line)
  (and (org--table-line? line)
       (>= (string-match "^[ \t]*\\|[-+:| \t]*$" line) 0)
       (>= (string-match "-" line) 0)))

;; The language token of a `#+begin_src LANG …` line, or "".
(define (org--block-lang line)
  (let ((c (regexp-captures "[bB][eE][gG][iI][nN]_[sS][rR][cC][ \t]+([A-Za-z0-9_+-]+)" line)))
    (if (null? c) "" (nth c 1))))

;; ===========================================================================
;; Buffer line / offset helpers (operate on the active buffer)
;; ===========================================================================
(define (org--lines) (split-many (buffer-string) "\n"))

(define (org--line-text i)
  (let ((ls (org--lines))) (if (< i (length ls)) (nth ls i) "")))

;; Byte offset of the start of line `i`.
(define (org--line-start i)
  (let ((ls (org--lines)))
    (let loop ((k 0) (acc 0))
      (if (>= k i) acc (loop (+ k 1) (+ acc (utf8-length (nth ls k)) 1))))))

(define (org--replace-all text) (replace-region 0 (point-max) text))

(define (org--replace-line i newtext)
  (let* ((line (org--line-text i))
         (start (org--line-start i)))
    (replace-region start (+ start (utf8-length line)) newtext)))

;; Replace the byte range covering lines [start..end] (inclusive) with the
;; joined `new-lines`.
(define (org--replace-line-range start end new-lines)
  (let* ((ls (org--lines))
         (sb (org--line-start start))
         (eb (+ (org--line-start end) (utf8-length (nth ls end)))))
    (replace-region sb eb (join-lines new-lines))))

;; ===========================================================================
;; Heading-component parsing
;; ===========================================================================
;; The first whitespace-delimited token of `s` if it is a known TODO keyword.
(define (org--leading-keyword s)
  (let ((caps (regexp-captures "^([A-Z]+)([ \t]|$)" s)))
    (if (and (not (null? caps)) (org--known-keyword? (nth caps 1)))
        (nth caps 1)
        #f)))

(define (org--done-keyword? kw) (and kw (member kw *org-done-keywords*) #t))

;; The headline title with keyword, priority cookie and trailing tags removed.
(define (org--heading-title rest kw)
  (let* ((s (if kw (replace-regexp-in-string "^[A-Z]+[ \t]+" "" rest) rest))
         (s (replace-regexp-in-string "^\\[#[A-Z]\\][ \t]*" "" s))
         (s (replace-regexp-in-string "[ \t]*:[A-Za-z0-9_@#%:]+:[ \t]*$" "" s)))
    (org--trim s)))

;; (stars-string rest) where rest is everything after `* `.
(define (org--heading-split line)
  (let ((caps (regexp-captures "^(\\*+)[ \t]*(.*)$" line)))
    (if (null? caps) (list "*" "") (list (nth caps 1) (nth caps 2)))))

;; ===========================================================================
;; Layer 1a — fontification
;; ===========================================================================
;; Is the active buffer an Org buffer? (.org by name, or explicitly activated.)
(define (org--active?)
  (or (ends-with? (current-buffer) ".org")
      (equal? (buffer-local-get "org-active") "t")))

(define (org--level-face n)
  (string-append "org-level-" (number->string (+ 1 (modulo (- n 1) 8)))))

;; Overlay `face` on every match of `pattern` within one line. `lstart` is the
;; line's start byte; `string-match` gives the byte offset of a match inside the
;; line and `regexp-captures` its text (→ byte length), so offsets stay exact
;; across multi-byte content.
(define (org--paint lstart line pattern face)
  (let loop ((base 0) (s line))
    (let ((m (string-match pattern s)))
      (when (>= m 0)
        (let* ((caps (regexp-captures pattern s))
               (whole (if (null? caps) "" (car caps)))
               (wlen (utf8-length whole)))
          (when (> wlen 0)
            (overlay-set-face (+ lstart base m) (+ lstart base m wlen) face)
            (let ((consumed (+ m wlen)))
              (loop (+ base consumed) (org--byte-drop s consumed)))))))))

(define (org--fontify-heading lstart line level)
  (let ((lend (+ lstart (utf8-length line))))
    (overlay-set-face lstart lend (org--level-face level))
    ;; Collapse all the leading stars AND the single separator space after them, so
    ;; the title sits exactly at the heading's virtual indent — a level-N heading
    ;; then aligns with the body under a level-(N-1) heading, like org-indent.
    ;; Stars + the space are ASCII (1 byte each); overlay-hide removes them from
    ;; display without touching the buffer. A bare heading (`**` with no title, so
    ;; no separator) hides only the stars, never the newline.
    (when *org-hide-leading-stars*
      (let ((hide (if (> (utf8-length line) level) (+ level 1) level)))
        (overlay-hide lstart (+ lstart hide))))
    ;; Keyword (stars + one space are ASCII, so byte = char here). A custom
    ;; keyword paints in its own color only if the user defined one — otherwise
    ;; it inherits the heading title color (no overlay). DONE-words are green and
    ;; the remaining TODO words share the org-todo red.
    (let* ((rest (substring line (min (+ level 1) (string-length line)) (string-length line)))
           (kw (org--leading-keyword rest)))
      (when kw
        (let ((ks (+ lstart level 1)))
          (cond
            ((org--custom-keyword? kw)
             (when (org--keyword-color kw)
               (overlay-set-face ks (+ ks (string-length kw)) (org--keyword-face-name kw))))
            ((org--done-keyword? kw)
             (overlay-set-face ks (+ ks (string-length kw)) "org-done"))
            (else
             (overlay-set-face ks (+ ks (string-length kw)) "org-todo"))))))
    ;; priority cookie [#A]
    (let ((b (string-match "\\[#[A-Z]\\]" line)))
      (when (>= b 0) (overlay-set-face (+ lstart b) (+ lstart b 4) "org-priority")))
    ;; trailing tags
    (let ((c (regexp-captures "(:[A-Za-z0-9_@#%:]+:)[ \t]*$" line)))
      (when (not (null? c))
        (let ((tags (nth c 1)))
          (overlay-set-face (- lend (utf8-length tags)) lend "org-tag"))))))

(define (org--fontify-plain lstart line)
  ;; checkbox state
  (let ((b (string-match "\\[[ xX-]\\]" line)))
    (when (and (>= b 0) (>= (string-match "^[ \t]*[-+*][ \t]+\\[[ xX-]\\]" line) 0))
      (let ((mark (substring line (+ b 1) (+ b 2))))
        (overlay-set-face (+ lstart b) (+ lstart b 3)
                          (if (equal? (string-upcase mark) "X") "org-checkbox-done" "org-checkbox")))))
  ;; inline emphasis (delimiters cannot span their own delimiter or a newline)
  (org--paint lstart line "\\*[^ \t\n*][^*\n]*\\*" "org-bold")
  (org--paint lstart line "/[^ \t\n/][^/\n]*/" "org-italic")
  (org--paint lstart line "=[^ \t\n=][^=\n]*=" "org-verbatim")
  (org--paint lstart line "~[^ \t\n~][^~\n]*~" "org-code")
  (org--paint lstart line "\\+[^ \t\n+][^+\n]*\\+" "org-strike")
  (org--paint lstart line "_[^ \t\n_][^_\n]*_" "org-underline")
  ;; footnote references [fn:label]
  (org--paint lstart line "\\[fn:[^]\n]*\\]" "org-footnote")
  ;; links [[target]] / [[target][desc]]
  (org--paint lstart line "\\[\\[[^]\n]*\\](\\[[^]\n]*\\])?\\]" "org-link")
  ;; active <…> and inactive [...] timestamps
  (org--paint lstart line "[<\\[][0-9]{4}-[0-9]{2}-[0-9]{2}[^]>\n]*[>\\]]" "org-date"))

;; Fontification, indent, and fold-marker passes each walk every line of the
;; buffer (re-splitting `org--lines` as they go) in interpreted Scheme on the
;; engine thread. On a very large .org file that blocks the engine long enough
;; that M-x and every other Scheme command stop responding, even though cursor
;; movement (handled in Rust) still works. Past a line threshold we skip the
;; auto passes — the Emacs `so-long` move. The mode, fold-style, and keymap are
;; still set, so editing works; only the org face/indent/chevron decoration is
;; dropped. Mirrors folding.scm's *fold-auto-limit* and markdown's guard.
(define *org-fontify-line-limit* 20000)
(define (org--fontify-ok?)
  (and (org--active?) (<= (line-count) *org-fontify-line-limit*)))

;; Repaint the whole buffer's faces. One pass; carries a flag for src-block
;; interiors (which are painted as a literal block, not parsed for emphasis).
(define (org--fontify)
  (when (org--fontify-ok?)
    (clear-overlay-faces)
    (clear-overlay-hides)   ; clear-overlay-faces no longer clears hides
    (let loop ((lines (org--lines)) (off 0) (insrc #f))
      (when (not (null? lines))
        (let* ((line (car lines))
               (lend (+ off (utf8-length line)))
               (lvl (org--heading-level line)))
          (cond
            ((and insrc (org--block-end? line))
             (overlay-set-face off lend "org-meta") (loop (cdr lines) (+ lend 1) #f))
            (insrc
             (overlay-set-face off lend "org-block") (loop (cdr lines) (+ lend 1) #t))
            ((org--block-begin? line)
             (overlay-set-face off lend "org-meta")
             (loop (cdr lines) (+ lend 1)
                   (>= (string-match "_[sS][rR][cC]\\|_[eE][xX]" line) 0)))
            ((org--block-end? line)
             (overlay-set-face off lend "org-meta") (loop (cdr lines) (+ lend 1) #f))
            (lvl
             (org--fontify-heading off line lvl) (loop (cdr lines) (+ lend 1) #f))
            ((or (org--keyword-line? line) (org--comment-line? line))
             (overlay-set-face off lend "org-meta") (loop (cdr lines) (+ lend 1) #f))
            ((org--drawer-line? line)
             (overlay-set-face off lend "org-drawer") (loop (cdr lines) (+ lend 1) #f))
            ((org--table-line? line)
             (overlay-set-face off lend "org-table") (loop (cdr lines) (+ lend 1) #f))
            (else
             (org--fontify-plain off line) (loop (cdr lines) (+ lend 1) #f))))))
    (org--apply-indent)))

;; Virtual `org-indent-mode` indentation. Walk the lines tracking the enclosing
;; heading level: a heading of level L is indented (L-1)*per-level columns, and
;; every body line beneath it L*per-level (one level deeper, so it sits under the
;; title). Display-only via `set-line-indent`; cleared first so toggling off and
;; edits both refresh cleanly.
(define (org--apply-indent)
  (clear-line-indents)
  (when *org-indent*
    (let loop ((lines (org--lines)) (i 0) (curlvl 0))
      (when (not (null? lines))
        (let ((lvl (org--heading-level (car lines))))
          (if lvl
              (begin
                (when (> lvl 1) (set-line-indent i (* (- lvl 1) *org-indent-per-level*)))
                (loop (cdr lines) (+ i 1) lvl))
              (begin
                (when (> curlvl 0) (set-line-indent i (* curlvl *org-indent-per-level*)))
                (loop (cdr lines) (+ i 1) curlvl))))))))

;; Debounced live refresh (the rainbow/LSP pattern): each edit schedules a
;; timer; only the last to fire repaints.
(define *org-fontify-pending* 0)
(define (org--fontify-schedule)
  (when (org--fontify-ok?)
    (set! *org-fontify-pending* (+ *org-fontify-pending* 1))
    (run-with-timer 200 "org--fontify-fire")))
(define (org--fontify-fire)
  (when (> *org-fontify-pending* 0) (set! *org-fontify-pending* (- *org-fontify-pending* 1)))
  (when (and (= *org-fontify-pending* 0) (org--active?)) (org--fontify)))

;; ===========================================================================
;; Layer 1b — visibility cycling (folding)
;; ===========================================================================
;; All headlines in the active buffer as (line-index level), in document order.
(define (org--headings)
  (let loop ((lines (org--lines)) (i 0) (acc '()))
    (if (null? lines)
        (reverse acc)
        (let ((lvl (org--heading-level (car lines))))
          (loop (cdr lines) (+ i 1) (if lvl (cons (list i lvl) acc) acc))))))

(define (org--total-lines) (length (org--lines)))

(define (org--find-heading headings line)
  (cond ((null? headings) #f)
        ((= (car (car headings)) line) (car headings))
        (else (org--find-heading (cdr headings) line))))

(define (org--next-heading-from headings line)
  (cond ((null? headings) #f)
        ((>= (car (car headings)) line) (car headings))
        (else (org--next-heading-from (cdr headings) line))))

;; Last body line of the subtree rooted at headline `H` (level `L`): one line
;; before the next headline of level ≤ L, else the last line.
(define (org--subtree-end H L headings total)
  (let loop ((hs headings))
    (cond ((null? hs) (- total 1))
          ((and (> (car (car hs)) H) (<= (nth (car hs) 1) L)) (- (car (car hs)) 1))
          (else (loop (cdr hs))))))

;; Per-buffer local fold state: an alist buffer → (list (heading-line . state)),
;; state ∈ 'folded | 'children. A heading absent from the list is 'subtree (fully
;; shown). This drives the three-state TAB cycle (subtree→folded→children→…).
(define *org-fold-state* '())
(define (org--fold-state-get)
  (let ((e (alist-ref *org-fold-state* (current-buffer)))) (if e e '())))
(define (org--fold-state-set v)
  (set! *org-fold-state*
        (cons (cons (current-buffer) v)
              (filter (lambda (p) (not (equal? (car p) (current-buffer)))) *org-fold-state*))))
(define (org--heading-state H)
  (let loop ((ps (org--fold-state-get)))
    (cond ((null? ps) 'subtree)
          ((= (car (car ps)) H) (cdr (car ps)))
          (else (loop (cdr ps))))))
(define (org--set-heading-state H st)
  (let ((rest (filter (lambda (p) (not (= (car p) H))) (org--fold-state-get))))
    (org--fold-state-set (if (eq? st 'subtree) rest (cons (cons H st) rest)))))

;; Smallest heading level among the descendants in subtree (H, E], or #f.
(define (org--min-child-level headings H E)
  (let loop ((hs headings) (m #f))
    (cond ((null? hs) m)
          ((and (> (car (car hs)) H) (<= (car (car hs)) E))
           (loop (cdr hs) (if m (min m (nth (car hs) 1)) (nth (car hs) 1))))
          (else (loop (cdr hs) m)))))

;; The direct child heading lines of H (the shallowest descendant level).
(define (org--direct-children headings H E)
  (let ((ml (org--min-child-level headings H E)))
    (if (not ml) '()
        (let loop ((hs headings) (acc '()))
          (cond ((null? hs) (reverse acc))
                ((and (> (car (car hs)) H) (<= (car (car hs)) E) (= (nth (car hs) 1) ml))
                 (loop (cdr hs) (cons (car (car hs)) acc)))
                (else (loop (cdr hs) acc)))))))

;; Inclusive (start . end) line ranges to hide for heading H in `state`. Pure, so
;; the visibility logic is unit-testable independently of the fold primitives.
(define (org--fold-ranges-for headings total H state)
  (let* ((entry (org--find-heading headings H))
         (L (nth entry 1))
         (E (org--subtree-end H L headings total)))
    (cond
      ((<= E H) '())                                   ; nothing under the heading
      ((eq? state 'folded) (list (cons (+ H 1) E)))    ; hide the whole subtree
      ((eq? state 'children)
       (let ((kids (org--direct-children headings H E)))
         (if (null? kids)
             (list (cons (+ H 1) E))                   ; no children ⇒ same as folded
             (let* ((first (car kids))
                    (body (if (> first (+ H 1)) (list (cons (+ H 1) (- first 1))) '())))
               (append body
                 (map (lambda (C)
                        (cons (+ C 1)
                              (org--subtree-end C (nth (org--find-heading headings C) 1)
                                                headings total)))
                      kids))))))
      (else '()))))

;; Re-derive folds from the per-heading state (the only fold primitives are
;; fold-lines / unfold-all, so we clear and re-apply).
(define (org--reapply-folds)
  (unfold-all)
  (let ((headings (org--headings)) (total (org--total-lines)))
    (for-each
     (lambda (pair)
       (when (org--find-heading headings (car pair))
         (for-each (lambda (r)
                     (when (>= (cdr r) (car r)) (fold-lines (car r) (+ (cdr r) 1))))
                   (org--fold-ranges-for headings total (car pair) (cdr pair)))))
     (org--fold-state-get))))

;; Paint a gutter chevron on every headline (down = expanded, right = folded),
;; so the fold can be toggled by clicking the chevron as well as with TAB.
(define (org--refresh-fold-markers)
  (clear-fold-markers)
  (for-each (lambda (h)
              (set-fold-marker (car h)
                               (not (eq? (org--heading-state (car h)) 'folded))))
            (org--headings)))

;; Toggle the fold of the heading on `line` — the chevron click entry point (a
;; click can't move point first, so this takes an explicit line rather than
;; reading point like `org-cycle`). Unlike TAB's three-state cycle, a chevron is
;; a binary affordance: collapse a shown subtree, or fully expand a folded one —
;; so the arrow always matches what's actually visible (no "open arrow but body
;; still hidden" `children` state).
(define (org-fold-toggle-at-line line)
  (when (org--heading-level (org--line-text line))
    (let ((next (if (eq? (org--heading-state line) 'folded) 'subtree 'folded)))
      (org--set-heading-state line next)
      (org--reapply-folds)
      (org--refresh-fold-markers))))

;; Route chevron clicks in Org buffers (fold-style "org") to the headline toggle.
(fold-register-style "org" org-fold-toggle-at-line)

;; TAB — on a headline, cycle local visibility subtree→folded→children→subtree
;; (like Emacs); inside a src block, indent the code; in plain body do nothing
;; (Emacs never inserts a tab at point there — it would just shove the line right).
(define (org-cycle)
  (let* ((L (buffer-line)) (line (org--line-text L)))
    (cond
      ((org--heading-level line)
       (let ((next (cond ((eq? (org--heading-state L) 'subtree) 'folded)
                         ((eq? (org--heading-state L) 'folded) 'children)
                         (else 'subtree))))
         (org--set-heading-state L next)
         (org--reapply-folds)
         (org--refresh-fold-markers)
         (message (cond ((eq? next 'folded) "Org: folded")
                        ((eq? next 'children) "Org: children")
                        (else "Org: subtree")))))
      ((org--src-block-bounds L) (indent-for-tab-command))
      (else #f))))

(define *org-global-state* '())
(define (org--global-state)
  (let ((e (alist-ref *org-global-state* (current-buffer)))) (if e e "showall")))
(define (org--set-global-state s)
  (set! *org-global-state*
        (cons (cons (current-buffer) s)
              (filter (lambda (p) (not (equal? (car p) (current-buffer)))) *org-global-state*))))

;; Show only top-level headlines (fold every level-1 subtree). Routed through the
;; per-heading state so a following local TAB expands one heading and leaves the
;; rest folded, like Emacs.
(define (org--show-overview)
  (org--fold-state-set
   (map (lambda (h) (cons (car h) 'folded))
        (filter (lambda (h) (= (nth h 1) 1)) (org--headings))))
  (org--reapply-folds))

;; Show all headlines, hide all body text (fold the gap between each headline
;; and the next).
(define (org--show-contents)
  (org--fold-state-set '()) (unfold-all)
  (let ((headings (org--headings)) (total (org--total-lines)))
    (let loop ((hs headings))
      (when (not (null? hs))
        (let* ((h (car hs))
               (next-line (if (null? (cdr hs)) total (car (car (cdr hs)))))
               (body-start (+ (car h) 1)))
          (when (> next-line body-start) (fold-lines body-start next-line))
          (loop (cdr hs)))))))

;; S-TAB — rotate the whole buffer: overview → contents → show-all.
(define (org-global-cycle)
  (let ((next (cond ((equal? (org--global-state) "showall") "overview")
                    ((equal? (org--global-state) "overview") "contents")
                    (else "showall"))))
    (org--set-global-state next)
    (cond ((equal? next "overview") (org--show-overview))
          ((equal? next "contents") (org--show-contents))
          (else (org--fold-state-set '()) (unfold-all)))
    (org--refresh-fold-markers)
    (message (string-append "Org global: " next))))

;; ===========================================================================
;; Layer 2 — structure editing
;; ===========================================================================
;; The headline line at point, or the nearest one above, or #f.
(define (org--heading-line-at-point)
  (let ((L (buffer-line)))
    (if (org--heading-level (org--line-text L))
        L
        (let loop ((i (- L 1)))
          (cond ((< i 0) #f)
                ((org--heading-level (org--line-text i)) i)
                (else (loop (- i 1))))))))

;; The keyword on the heading at point, or #f (a public accessor, so configs
;; need not reach into the org--* internals to read it).
(define (org-keyword-at-point)
  (let ((H (org--heading-line-at-point)))
    (if H (org--leading-keyword (nth (org--heading-split (org--line-text H)) 1)) #f)))

;; The full ordered cycle C-c C-t rotates through: the TODO vocabulary followed
;; by the user's custom keywords. Like Emacs `org-todo-keywords`, every keyword
;; that is recognized is part of the rotation.
(define (org--cycle-keywords) (append *org-todo-keywords* *org-custom-keywords*))

;; The keyword following `cur` in `seq`, Emacs-style: #f (none) → first entry;
;; the last entry → "" (clear); otherwise the next entry. A `cur` not in `seq`
;; restarts the cycle at the first entry.
(define (org--next-keyword cur seq)
  (cond ((null? seq) "")
        ((not cur) (car seq))
        (else
         (let loop ((ls seq))
           (cond ((null? ls) (car seq))
                 ((equal? (car ls) cur) (if (null? (cdr ls)) "" (car (cdr ls))))
                 (else (loop (cdr ls))))))))

;; Rewrite heading H to carry keyword `kw` ("" clears it), preserving the
;; priority cookie and trailing tags. Shared by org-todo and org-set-keyword.
(define (org--apply-keyword H kw)
  (let* ((sr (org--heading-split (org--line-text H)))
         (stars (nth sr 0)) (rest (nth sr 1))
         (title (org--heading-title rest (org--leading-keyword rest)))
         (tags (let ((c (regexp-captures "(:[A-Za-z0-9_@#%:]+:)[ \t]*$" rest)))
                 (if (null? c) "" (nth c 1))))
         (prio (let ((c (regexp-captures "\\[#[A-Z]\\]" rest))) (if (null? c) "" (car c)))))
    (org--replace-line H
      (string-append stars " "
        (if (equal? kw "") "" (string-append kw " "))
        (if (equal? prio "") "" (string-append prio " "))
        title
        (if (equal? tags "") "" (string-append " " tags))))))

;; C-c C-t — rotate the headline at point through the full keyword cycle
;; (nothing → each TODO/custom keyword → nothing).
(define (org-todo)
  (let ((H (org--heading-line-at-point)))
    (if (not H)
        (message "Not on a heading")
        (let ((next (org--next-keyword (org-keyword-at-point) (org--cycle-keywords))))
          ;; Marking a repeating task DONE shifts its planning stamps to the next
          ;; occurrence and resets the keyword instead (Emacs `org-todo-repeat`).
          (if (and (org--done-keyword? next) (org--has-repeater? H))
              (org--repeat-task H)
              (org--apply-keyword H next))))))

;; Set a specific keyword on the headline at point, or clear it if that same
;; keyword is already there (so a binding toggles it). Unlike org-todo's rotate,
;; this jumps straight to one keyword — handy for direct per-label bindings.
(define (org-set-keyword kw)
  (let ((H (org--heading-line-at-point)))
    (if H
        (org--apply-keyword H (if (equal? (org-keyword-at-point) kw) "" kw))
        (message "Not on a heading"))))

;; M-right — demote (one deeper): add a star.
(define (org-demote)
  (let ((H (org--heading-line-at-point)))
    (when H (org--replace-line H (string-append "*" (org--line-text H))))))

;; M-left — promote (one shallower): remove a star.
(define (org-promote)
  (let ((H (org--heading-line-at-point)))
    (when H
      (let ((line (org--line-text H)))
        (if (>= (string-match "^\\*\\*" line) 0)
            (org--replace-line H (substring line 1 (string-length line)))
            (message "Already at top level"))))))

;; M-RET — insert a sibling headline just after the current subtree.
(define (org-insert-heading)
  (let* ((H (org--heading-line-at-point))
         (headings (org--headings)) (total (org--total-lines))
         (lvl (if H (nth (org--find-heading headings H) 1) 1))
         (end (if H (org--subtree-end H lvl headings total) (buffer-line)))
         (pos (+ (org--line-start end) (utf8-length (org--line-text end))))
         (stars (org--stars lvl)))
    (insert-at pos (string-append "\n" stars " "))
    (goto-char (+ pos 1 (string-length stars) 1))))

;; M-down — move the subtree at point below its next sibling.
(define (org-move-subtree-down)
  (let ((H (org--heading-line-at-point)))
    (when H
      (let* ((lines (org--lines)) (headings (org--headings)) (total (length lines))
             (lvl (nth (org--find-heading headings H) 1))
             (end (org--subtree-end H lvl headings total))
             (next (org--next-heading-from headings (+ end 1))))
        (if (and next (<= (nth next 1) lvl))
            (let* ((H2 (car next))
                   (end2 (org--subtree-end H2 (nth next 1) headings total))
                   (blockA (org--slice lines H end))
                   (blockB (org--slice lines (+ end 1) end2))
                   (before (org--slice lines 0 (- H 1)))
                   (after  (org--slice lines (+ end2 1) (- total 1))))
              (org--replace-all (join-lines (append before blockB blockA after)))
              (goto-line (+ H (length blockB))))
            (message "No sibling below"))))))

;; M-up — move the subtree at point above its previous sibling.
(define (org--prev-sibling headings H lvl)
  (let loop ((hs headings) (cand #f))
    (cond ((null? hs) (if (and cand (= (nth cand 1) lvl)) cand #f))
          ((>= (car (car hs)) H) (if (and cand (= (nth cand 1) lvl)) cand #f))
          ((<= (nth (car hs) 1) lvl) (loop (cdr hs) (car hs)))
          (else (loop (cdr hs) cand)))))
;; M-up — move the subtree at point above its previous sibling. Done as a direct
;; block swap (not goto + move-down): under the snapshot model a queued goto isn't
;; visible to reads in the same command, so re-reading point would move the wrong
;; subtree the wrong way.
(define (org-move-subtree-up)
  (let ((H (org--heading-line-at-point)))
    (when H
      (let* ((lines (org--lines)) (headings (org--headings)) (total (length lines))
             (lvl (nth (org--find-heading headings H) 1))
             (prev (org--prev-sibling headings H lvl)))
        (if prev
            (let* ((P (car prev))
                   (endH (org--subtree-end H lvl headings total))
                   (blockP (org--slice lines P (- H 1)))   ; prev sibling's subtree
                   (blockH (org--slice lines H endH))       ; this subtree
                   (before (org--slice lines 0 (- P 1)))
                   (after  (org--slice lines (+ endH 1) (- total 1))))
              (org--replace-all (join-lines (append before blockH blockP after)))
              (goto-line P))
            (message "No sibling above"))))))

;; C-c C-q — set the headline's tags (colon- or space-separated input).
(define *org--tag-target* -1)
(define (org--current-tags H)
  (let ((c (regexp-captures ":([A-Za-z0-9_@#%:]+):[ \t]*$" (org--line-text H))))
    (if (null? c) "" (nth c 1))))
(define (org-set-tags)
  (let ((H (org--heading-line-at-point)))
    (if H
        (begin (set! *org--tag-target* H)
               (read-string "Tags: " "org--set-tags-finish" (org--current-tags H)))
        (message "Not on a heading"))))
(define (org--normalize-tags s)
  (string-join (filter (lambda (x) (not (equal? x "")))
                       (split-many (replace-regexp-in-string "[ \t]+" ":" (org--trim s)) ":"))
               ":"))
(define (org--set-tags-finish input)
  (let* ((H *org--tag-target*)
         (line (org--line-text H))
         (base (replace-regexp-in-string "[ \t]*:[A-Za-z0-9_@#%:]+:[ \t]*$" "" line))
         (clean (org--trim input)))
    (org--replace-line H
      (if (equal? clean "")
          base
          (string-append base " :" (org--normalize-tags clean) ":")))))

;; C-c , — set or clear the headline's priority cookie.
(define *org--prio-target* -1)
(define (org-set-priority)
  (let ((H (org--heading-line-at-point)))
    (if H
        (begin (set! *org--prio-target* H)
               (read-string "Priority (A-C, blank clears): " "org--set-priority-finish"))
        (message "Not on a heading"))))
(define (org--set-priority-finish input)
  (let* ((H *org--prio-target*)
         (line (org--line-text H))
         (sr (org--heading-split line))
         (stars (nth sr 0)) (rest (nth sr 1))
         (kw (org--leading-keyword rest))
         (title (org--heading-title rest kw))
         (tags (let ((c (regexp-captures "(:[A-Za-z0-9_@#%:]+:)[ \t]*$" rest)))
                 (if (null? c) "" (nth c 1))))
         (p (string-upcase (org--trim input))))
    (org--replace-line H
      (string-append stars " "
        (if kw (string-append kw " ") "")
        (if (>= (string-match "^[A-C]$" p) 0) (string-append "[#" p "] ") "")
        title
        (if (equal? tags "") "" (string-append " " tags))))))

;; Toggle the checkbox on a `- [ ]` list item at point; #t if it did.
(define (org-toggle-checkbox)
  (let* ((L (buffer-line)) (line (org--line-text L))
         (caps (regexp-captures "^([ \t]*[-+*][ \t]+)\\[([ xX-])\\]" line)))
    (if (null? caps)
        #f
        (let* ((prefix (nth caps 1)) (state (nth caps 2))
               (new (if (equal? (string-upcase state) "X") " " "X"))
               (rest (substring line (+ (string-length prefix) 3) (string-length line))))
          (org--replace-line L (string-append prefix "[" new "]" rest))
          #t))))

;; ===========================================================================
;; Layer 3 — the Org AST parser
;; ===========================================================================
;; `(org-parse TEXT)` → a flat list of block elements, each a list whose head is
;; a type string:
;;   ("heading" level todo priority title tags)
;;   ("para" text) ("src" lang code) ("example" text) ("quote" text)
;;   ("ulist" items) ("olist" items)   items = list of (checkbox . text)
;;   ("table" rows)  rows = ("sep") | ("row" cell …)
;;   ("keyword" key value) ("hr")
;; Both the HTML exporter and the agenda scanner consume this.
(define (org--parse-heading line)
  (let* ((sr (org--heading-split line))
         (stars (nth sr 0)) (rest (nth sr 1))
         (kw (org--leading-keyword rest))
         (prio (let ((c (regexp-captures "\\[#([A-Z])\\]" rest))) (if (null? c) "" (nth c 1))))
         (tags (let ((c (regexp-captures ":([A-Za-z0-9_@#%:]+):[ \t]*$" rest))) (if (null? c) "" (nth c 1)))))
    (list "heading" (string-length stars) (or kw "") prio (org--heading-title rest kw) tags)))

(define (org--parse-keyword line)
  (let ((c (regexp-captures "^[ \t]*#\\+([A-Za-z_]+):[ \t]*(.*)$" line)))
    (list "keyword" (if (null? c) "" (nth c 1)) (if (null? c) "" (nth c 2)))))

;; Gather lines up to the matching `#+end_…`; → (body-lines . rest-after-end).
(define (org--collect-block ls)
  (let loop ((ls ls) (acc '()))
    (cond ((null? ls) (cons (reverse acc) '()))
          ((org--block-end? (car ls)) (cons (reverse acc) (cdr ls)))
          (else (loop (cdr ls) (cons (car ls) acc))))))

(define (org--parse-item line kind)
  (let* ((stripped (if (equal? kind "u")
                       (replace-regexp-in-string "^[ \t]*[-+][ \t]+" "" line)
                       (replace-regexp-in-string "^[ \t]*[0-9]+[.)][ \t]+" "" line)))
         (cb (regexp-captures "^\\[([ xX-])\\][ \t]*(.*)$" stripped)))
    (if (null? cb)
        (cons "" stripped)
        (cons (if (equal? (string-upcase (nth cb 1)) "X") "X" (nth cb 1)) (nth cb 2)))))

(define (org--collect-list ls kind)
  (let loop ((ls ls) (acc '()))
    (cond ((null? ls) (cons (reverse acc) '()))
          ((and (equal? kind "u") (org--ulist-line? (car ls)))
           (loop (cdr ls) (cons (org--parse-item (car ls) "u") acc)))
          ((and (equal? kind "o") (org--olist-line? (car ls)))
           (loop (cdr ls) (cons (org--parse-item (car ls) "o") acc)))
          (else (cons (reverse acc) ls)))))

;; Split a table line into trimmed cells.
(define (org--table-cells line)
  (let* ((t (org--trim line))
         (t2 (replace-regexp-in-string "\\|[ \t]*$" ""
               (replace-regexp-in-string "^[ \t]*\\|" "" t))))
    (map org--trim (split-many t2 "|"))))

(define (org--collect-table ls)
  (let loop ((ls ls) (acc '()))
    (cond ((or (null? ls) (not (org--table-line? (car ls)))) (cons (reverse acc) ls))
          ((org--table-sep? (car ls)) (loop (cdr ls) (cons (list "sep") acc)))
          (else (loop (cdr ls) (cons (cons "row" (org--table-cells (car ls))) acc))))))

(define (org--collect-para ls)
  (let loop ((ls ls) (acc '()))
    (cond ((null? ls) (cons (reverse acc) '()))
          ((let ((l (car ls)))
             (or (equal? (org--trim l) "")
                 (org--heading-level l) (org--keyword-line? l) (org--block-begin? l)
                 (org--table-line? l) (org--ulist-line? l) (org--olist-line? l) (org--hr-line? l)))
           (cons (reverse acc) ls))
          (else (loop (cdr ls) (cons (car ls) acc))))))

(define (org-parse text)
  (let loop ((ls (split-many text "\n")) (acc '()))
    (if (null? ls)
        (reverse acc)
        (let ((line (car ls)))
          (cond
            ((equal? (org--trim line) "") (loop (cdr ls) acc))
            ((org--heading-level line) (loop (cdr ls) (cons (org--parse-heading line) acc)))
            ((org--keyword-line? line) (loop (cdr ls) (cons (org--parse-keyword line) acc)))
            ((>= (string-match "[bB][eE][gG][iI][nN]_[sS][rR][cC]" line) 0)
             (let ((cd (org--collect-block (cdr ls))))
               (loop (cdr cd) (cons (list "src" (org--block-lang line) (join-lines (car cd))) acc))))
            ((>= (string-match "[bB][eE][gG][iI][nN]_[eE][xX]" line) 0)
             (let ((cd (org--collect-block (cdr ls))))
               (loop (cdr cd) (cons (list "example" (join-lines (car cd))) acc))))
            ((org--block-begin? line)   ; quote / other block
             (let ((cd (org--collect-block (cdr ls))))
               (loop (cdr cd) (cons (list "quote" (join-lines (car cd))) acc))))
            ((org--hr-line? line) (loop (cdr ls) (cons (list "hr") acc)))
            ((org--ulist-line? line)
             (let ((cd (org--collect-list ls "u")))
               (loop (cdr cd) (cons (list "ulist" (car cd)) acc))))
            ((org--olist-line? line)
             (let ((cd (org--collect-list ls "o")))
               (loop (cdr cd) (cons (list "olist" (car cd)) acc))))
            ((org--table-line? line)
             (let ((cd (org--collect-table ls)))
               (loop (cdr cd) (cons (list "table" (car cd)) acc))))
            (else
             (let ((cd (org--collect-para ls)))
               (loop (cdr cd) (cons (list "para" (join-lines (car cd))) acc)))))))))

;; ===========================================================================
;; Layer 3b — HTML export (walks the AST)
;; ===========================================================================
(define (org--escape-html s)
  (replace-regexp-in-string ">" "&gt;"
    (replace-regexp-in-string "<" "&lt;"
      (replace-regexp-in-string "&" "&amp;" s))))

;; Inline markup → HTML. Escape first, then links, then emphasis.
(define (org--inline->html s)
  (let* ((s (org--escape-html s))
         (s (replace-regexp-in-string "\\[\\[([^]]+)\\]\\[([^]]+)\\]\\]" "<a href=\"$1\">$2</a>" s))
         (s (replace-regexp-in-string "\\[\\[([^]]+)\\]\\]" "<a href=\"$1\">$1</a>" s))
         (s (replace-regexp-in-string "\\*([^*\n]+)\\*" "<strong>$1</strong>" s))
         (s (replace-regexp-in-string "/([^/\n]+)/" "<em>$1</em>" s))
         (s (replace-regexp-in-string "=([^=\n]+)=" "<code>$1</code>" s))
         (s (replace-regexp-in-string "~([^~\n]+)~" "<code>$1</code>" s))
         (s (replace-regexp-in-string "\\+([^+\n]+)\\+" "<del>$1</del>" s))
         (s (replace-regexp-in-string "_([^_\n]+)_" "<u>$1</u>" s))
         (s (replace-regexp-in-string "\\[fn:([^]\n]*)\\]" "<sup>$1</sup>" s)))
    s))

(define (org--list->html e tag)
  (string-append "<" tag ">\n"
    (org--concat (map (lambda (it)
                        (string-append "<li>"
                          (if (equal? (car it) "") "" (string-append "[" (car it) "] "))
                          (org--inline->html (cdr it)) "</li>\n"))
                      (nth e 1)))
    "</" tag ">\n"))

(define (org--table->html e)
  (string-append "<table>\n"
    (org--concat (map (lambda (row)
                        (if (equal? (car row) "sep")
                            ""
                            (string-append "<tr>"
                              (org--concat (map (lambda (c)
                                                  (string-append "<td>" (org--inline->html c) "</td>"))
                                                (cdr row)))
                              "</tr>\n")))
                      (nth e 1)))
    "</table>\n"))

(define (org--block->html e)
  (let ((type (car e)))
    (cond
      ((equal? type "heading")
       (let ((lvl (number->string (min 6 (nth e 1)))) (todo (nth e 2))
             (title (nth e 4)) (tags (nth e 5)))
         (string-append "<h" lvl ">"
           (if (equal? todo "") "" (string-append "<span class=\"todo\">" (org--escape-html todo) "</span> "))
           (org--inline->html title)
           (if (equal? tags "") "" (string-append " <span class=\"tags\">" (org--escape-html tags) "</span>"))
           "</h" lvl ">\n")))
      ((equal? type "para") (string-append "<p>" (org--inline->html (nth e 1)) "</p>\n"))
      ((equal? type "src") (string-append "<pre><code>" (org--escape-html (nth e 2)) "</code></pre>\n"))
      ((equal? type "example") (string-append "<pre>" (org--escape-html (nth e 1)) "</pre>\n"))
      ((equal? type "quote") (string-append "<blockquote>" (org--inline->html (nth e 1)) "</blockquote>\n"))
      ((equal? type "hr") "<hr>\n")
      ((equal? type "ulist") (org--list->html e "ul"))
      ((equal? type "olist") (org--list->html e "ol"))
      ((equal? type "table") (org--table->html e))
      (else ""))))

(define (org--doc-title ast)
  (let loop ((es ast))
    (cond ((null? es) "Untitled")
          ((and (equal? (car (car es)) "keyword")
                (equal? (string-upcase (nth (car es) 1)) "TITLE"))
           (nth (car es) 2))
          (else (loop (cdr es))))))

(define (org--ast->html ast)
  (string-append
    "<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n<title>"
    (org--escape-html (org--doc-title ast))
    "</title>\n<style>body{font-family:sans-serif;max-width:48rem;margin:2rem auto;padding:0 1rem;line-height:1.5}pre{background:#f4f4f4;padding:.6rem;overflow:auto}code{background:#f4f4f4}table{border-collapse:collapse}td,th{border:1px solid #ccc;padding:.2rem .5rem}.todo{color:#c00}.tags{color:#888;font-size:.8em}</style>\n</head>\n<body>\n"
    (org--concat (map org--block->html ast))
    "</body>\n</html>\n"))

(define (org--html-output-path)
  (let* ((b (current-buffer))
         (stem (if (ends-with? b ".org") (substring b 0 (- (string-length b) 4)) b)))
    (path-join (buffer-directory) (string-append stem ".html"))))

;; C-c C-e h — export the current buffer to a sibling .html file.
(define (org-export-html)
  (let ((path (org--html-output-path)))
    (write-file path (org--ast->html (org-parse (buffer-string))))
    (message (string-append "Exported to " path))))

;; ===========================================================================
;; Layer 4 — agenda + capture
;; ===========================================================================
;; The set of files the agenda scans. If unset, every .org file under the
;; current project (or buffer directory).
(define *org-agenda-files* '())
(define (org-set-agenda-files lst) (set! *org-agenda-files* lst))
(define (org--agenda-file-list)
  (if (not (null? *org-agenda-files*))
      *org-agenda-files*
      (let ((root (or (project-root (buffer-directory)) (buffer-directory))))
        (filter (lambda (p) (ends-with? p ".org")) (walk-files root)))))

;; (date . kind) from a SCHEDULED:/DEADLINE: planning line, or #f.
(define (org--planning-info line)
  (let ((c (regexp-captures "(SCHEDULED|DEADLINE):[ \t]*[<\\[]([0-9]{4}-[0-9]{2}-[0-9]{2})" line)))
    (if (null? c) #f (cons (nth c 2) (nth c 1)))))

;; ":a:b:" / "a:b" → ("a" "b").
(define (org--split-tag-string s)
  (filter (lambda (x) (not (equal? x ""))) (split-many s ":")))
;; a ∪ b, preserving a's order then b's new tags.
(define (org--union a b) (append a (filter (lambda (x) (not (member x a))) b)))

;; Scan one file → entries (path line date kind todo title tags) for headlines
;; that carry a TODO keyword and/or a planning date. `tags` includes tags
;; inherited from ancestor headings (Emacs `org-use-tag-inheritance`). A `stack`
;; of (level . inherited-tags) tracks the heading ancestry as we descend.
(define (org--scan-file path)
  (let loop ((ls (split-many (read-file path) "\n")) (i 0) (stack '()) (acc '()))
    (if (null? ls)
        (reverse acc)
        (let ((line (car ls)))
          (if (org--heading-level line)
              (let* ((lvl (org--heading-level line))
                     (sr (org--heading-split line))
                     (rest (nth sr 1))
                     (kw (org--leading-keyword rest))
                     (title (org--heading-title rest kw))
                     (own (org--split-tag-string
                           (let ((c (regexp-captures ":([A-Za-z0-9_@#%:]+):[ \t]*$" rest)))
                             (if (null? c) "" (nth c 1)))))
                     (stack2 (filter (lambda (e) (< (car e) lvl)) stack))
                     (tags (org--union (if (null? stack2) '() (cdr (car stack2))) own))
                     (plan (if (null? (cdr ls)) #f (org--planning-info (nth ls 1)))))
                (if (or kw plan)
                    (loop (cdr ls) (+ i 1) (cons (cons lvl tags) stack2)
                          (cons (list path i
                                      (if plan (car plan) "")
                                      (if plan (cdr plan) "")
                                      (or kw "") title tags)
                                acc))
                    (loop (cdr ls) (+ i 1) (cons (cons lvl tags) stack2) acc)))
              (loop (cdr ls) (+ i 1) stack acc))))))

(define (org--all-entries)
  (let loop ((files (org--agenda-file-list)) (acc '()))
    (if (null? files) acc (loop (cdr files) (append acc (org--scan-file (car files)))))))

;; Sort by date (undated entries last), lexically — ISO dates order correctly.
(define (org--entry-key e) (let ((d (nth e 2))) (if (equal? d "") "9999-99-99" d)))
(define (org--insert-entry x sorted)
  (cond ((null? sorted) (list x))
        ((string<? (org--entry-key x) (org--entry-key (car sorted))) (cons x sorted))
        (else (cons (car sorted) (org--insert-entry x (cdr sorted))))))
(define (org--sort-entries es)
  (if (null? es) '() (org--insert-entry (car es) (org--sort-entries (cdr es)))))

;; line-in-agenda-buffer → (path . src-line), for RET.
(define *org-agenda-targets* '())
(define (org--render-agenda entries)
  (set! *org-agenda-targets* '())
  (let loop ((es entries) (i 0) (lines '()) (faces '()))
    (if (null? es)
        (render-buffer "*Org Agenda*" "org-agenda-mode" (join-lines (reverse lines)) (reverse faces))
        (let* ((e (car es))
               (date (nth e 2)) (kind (nth e 3)) (todo (nth e 4)) (title (nth e 5))
               (tags (if (> (length e) 6) (nth e 6) '()))
               (text (string-append
                       (if (equal? date "") "             " (string-append date "  "))
                       (if (equal? kind "") "" (string-append (substring kind 0 (min 4 (string-length kind))) ": "))
                       (if (equal? todo "") "" (string-append todo " "))
                       title
                       (if (null? tags) "" (string-append "  :" (string-join tags ":") ":"))
                       "   " (basename (nth e 0)))))
          (set! *org-agenda-targets* (cons (cons i (cons (nth e 0) (nth e 1))) *org-agenda-targets*))
          (loop (cdr es) (+ i 1) (cons text lines)
                (cons (list i (if (org--done-keyword? todo) "org-done" "org-todo")) faces))))))

;; C-c a — agenda: every dated/TODO headline across the agenda files.
(define (org-agenda) (org--render-agenda (org--sort-entries (org--all-entries))))
;; C-c t — just the open TODO items.
(define (org-todo-list)
  (org--render-agenda
   (org--sort-entries (filter (lambda (e) (not (equal? (nth e 4) ""))) (org--all-entries)))))

(define (org-agenda-visit)
  (let ((t (alist-ref *org-agenda-targets* (buffer-line))))
    (when t (open-file (car t)) (goto-line (cdr t)))))

;; C-c c — capture a TODO headline, appended to the capture file on disk.
(define *org-capture-target* #f)
(define (org-set-capture-file path) (set! *org-capture-target* path))
(define (org--capture-file)
  (or *org-capture-target*
      (let ((files (org--agenda-file-list)))
        (if (null? files) (path-join (buffer-directory) "inbox.org") (car files)))))
;; Capture templates (see Layer 7f for the engine). Defined here so org-capture,
;; below, can test it; populate from init.scm.
(define *org-capture-templates* '())
;; C-c c — capture. With no templates configured this is the simple "* TODO"
;; prompt; configure *org-capture-templates* (see Layer 7f) for the full flow.
(define (org-capture)
  (if (null? *org-capture-templates*)
      (read-string "Capture (TODO headline): " "org--capture-finish")
      (org--capture-pick)))
(define (org--capture-finish text)
  (when (not (equal? (org--trim text) ""))
    (let* ((path (org--capture-file))
           (existing (if (file-exists? path) (read-file path) ""))
           (sep (if (or (equal? existing "") (ends-with? existing "\n")) "" "\n")))
      (write-file path (string-append existing sep "* TODO " (org--trim text) "\n"))
      (message (string-append "Captured to " (basename path))))))

;; ===========================================================================
;; Layer 5a — table alignment
;; ===========================================================================
(define (org--table-bounds L)
  (let ((lines (org--lines)))
    (let ((n (length lines)))
      (if (or (>= L n) (not (org--table-line? (nth lines L))))
          #f
          (let ((s (let up ((i L)) (if (and (>= i 0) (org--table-line? (nth lines i))) (up (- i 1)) (+ i 1))))
                (e (let dn ((i L)) (if (and (< i n) (org--table-line? (nth lines i))) (dn (+ i 1)) (- i 1)))))
            (list s e))))))

(define (org--cell cells c) (if (< c (length cells)) (nth cells c) ""))
(define (org--max-cols parsed)
  (let loop ((ps parsed) (m 0))
    (cond ((null? ps) m)
          ((car (car ps)) (loop (cdr ps) m))
          (else (loop (cdr ps) (max m (length (cdr (car ps)))))))))
(define (org--col-width parsed c)
  (let loop ((ps parsed) (m 1))
    (cond ((null? ps) m)
          ((car (car ps)) (loop (cdr ps) m))
          (else (loop (cdr ps) (max m (utf8-length (org--cell (cdr (car ps)) c))))))))
(define (org--col-widths parsed ncols)
  (let loop ((c 0) (acc '()))
    (if (>= c ncols) (reverse acc) (loop (+ c 1) (cons (org--col-width parsed c) acc)))))
(define (org--pad-cells cells widths)
  (let loop ((ws widths) (c 0) (acc '()))
    (if (null? ws)
        (reverse acc)
        (loop (cdr ws) (+ c 1) (cons (org--pad (org--cell cells c) (car ws)) acc)))))
(define (org--data-row cells widths)
  (string-append "| " (string-join (org--pad-cells cells widths) " | ") " |"))
(define (org--sep-row widths)
  (string-append "|" (string-join (map (lambda (w) (org--dashes (+ w 2))) widths) "+") "|"))
(define (org--format-table rows)
  (let* ((parsed (map (lambda (l) (cons (org--table-sep? l) (org--table-cells l))) rows))
         (ncols (org--max-cols parsed))
         (widths (org--col-widths parsed ncols)))
    (map (lambda (p) (if (car p) (org--sep-row widths) (org--data-row (cdr p) widths))) parsed)))

;; Re-align the table at point (also bound on C-c C-c inside a table).
(define (org-table-align)
  (let ((b (org--table-bounds (buffer-line))))
    (if (not b)
        (message "Not in a table")
        (org--replace-line-range (nth b 0) (nth b 1)
          (org--format-table (org--slice (org--lines) (nth b 0) (nth b 1)))))))

;; --- interactive table editing (cell nav, row/column ops) -------------------
;; Byte positions of every '|' (ASCII 124) in a row.
(define (org--pipe-positions line)
  (let loop ((chars (string->list line)) (byte 0) (acc '()))
    (cond ((null? chars) (reverse acc))
          ((= (char->integer (car chars)) 124) (loop (cdr chars) (+ byte 1) (cons byte acc)))
          (else (loop (cdr chars) (+ byte (org--char-bytes (car chars))) acc)))))

;; Which cell (0-based) the byte-column `col` falls in, on a table row.
(define (org--cell-index-at line col)
  (let ((m (length (filter (lambda (p) (< p col)) (org--pipe-positions line)))))
    (max 0 (- m 1))))

;; A string of n '|' — i.e. an empty row with n-1 cells (org--format-table pads it).
(define (org--pipes n) (if (<= n 0) "" (string-append "|" (org--pipes (- n 1)))))

;; Column count of a table slice (its widest data row).
(define (org--table-ncols slice)
  (org--max-cols (map (lambda (l) (cons (org--table-sep? l) (org--table-cells l))) slice)))

;; Total bytes (incl. newlines) of rows[0..idx).
(define (org--rows-byte-prefix rows idx)
  (let loop ((rs rows) (i 0) (acc 0))
    (if (or (null? rs) (>= i idx)) acc
        (loop (cdr rs) (+ i 1) (+ acc (utf8-length (car rs)) 1)))))

;; Point column (byte offset within its line).
(define (org--point-col)
  (- (point) (org--line-start (buffer-line))))

;; Replace the table [start..end] with `formatted`, then put point at the start of
;; cell `col` on formatted row index `row` (cell content begins 2 bytes past the
;; row's `col`-th pipe: the "| " prefix). Lines before `start` are untouched, so
;; the start byte is stable across the replace.
(define (org--retab-goto start end formatted row col)
  (let* ((base (org--line-start start))
         (rowline (nth formatted row))
         (pipes (org--pipe-positions rowline))
         (cellpos (if (< col (length pipes)) (+ (nth pipes col) 2) (utf8-length rowline)))
         (target (+ base (org--rows-byte-prefix formatted row) cellpos)))
    (org--replace-line-range start end formatted)
    (goto-char target)))

;; Index of the next / previous non-separator row in `slice` from index `from`
;; (exclusive), or #f.
(define (org--next-data-row slice from)
  (let ((n (length slice)))
    (let loop ((i (+ from 1)))
      (cond ((>= i n) #f)
            ((org--table-sep? (nth slice i)) (loop (+ i 1)))
            (else i)))))
(define (org--prev-data-row slice from)
  (let loop ((i (- from 1)))
    (cond ((< i 0) #f)
          ((org--table-sep? (nth slice i)) (loop (- i 1)))
          (else i))))

;; Cells of `line` padded to exactly `ncols` (missing trailing cells become "").
(define (org--cells-pad line ncols)
  (let ((cells (org--table-cells line)))
    (let loop ((i 0) (acc '()))
      (if (>= i ncols) (reverse acc) (loop (+ i 1) (cons (org--cell cells i) acc))))))
(define (org--list-set lst i v)
  (let loop ((l lst) (k 0) (acc '()))
    (cond ((null? l) (reverse acc))
          ((= k i) (loop (cdr l) (+ k 1) (cons v acc)))
          (else (loop (cdr l) (+ k 1) (cons (car l) acc))))))
(define (org--list-swap lst i j)
  (org--list-set (org--list-set lst i (nth lst j)) j (nth lst i)))
(define (org--row-from-cells cells)
  (string-append "| " (string-join cells " | ") " |"))

;; TAB — re-align and move to the next cell (creating a new row past the last).
(define (org-table-next-cell)
  (let* ((L (buffer-line)) (b (org--table-bounds L)))
    (when b
      (let* ((start (nth b 0)) (end (nth b 1))
             (slice (org--slice (org--lines) start end))
             (ridx (- L start))
             (ncols (org--table-ncols slice))
             (line (org--line-text L)) (sep? (org--table-sep? line))
             (col (if sep? -1 (org--cell-index-at line (org--point-col)))))
        (if (and (not sep?) (< col (- ncols 1)))
            (org--retab-goto start end (org--format-table slice) ridx (+ col 1))
            (let ((nxt (org--next-data-row slice ridx)))
              (if nxt
                  (org--retab-goto start end (org--format-table slice) nxt 0)
                  (let* ((slice2 (append slice (list (org--pipes (+ ncols 1)))))
                         (formatted2 (org--format-table slice2)))
                    (org--retab-goto start end formatted2 (- (length formatted2) 1) 0)))))))))

;; S-TAB — re-align and move to the previous cell.
(define (org-table-prev-cell)
  (let* ((L (buffer-line)) (b (org--table-bounds L)))
    (when b
      (let* ((start (nth b 0)) (end (nth b 1))
             (slice (org--slice (org--lines) start end))
             (ridx (- L start))
             (ncols (org--table-ncols slice))
             (line (org--line-text L)) (sep? (org--table-sep? line))
             (col (if sep? ncols (org--cell-index-at line (org--point-col)))))
        (if (and (not sep?) (> col 0))
            (org--retab-goto start end (org--format-table slice) ridx (- col 1))
            (let ((prv (org--prev-data-row slice ridx)))
              (when prv
                (org--retab-goto start end (org--format-table slice) prv (- ncols 1)))))))))

;; RET — re-align and move to the first cell of the next row (creating one).
(define (org-table-next-row)
  (let* ((L (buffer-line)) (b (org--table-bounds L)))
    (when b
      (let* ((start (nth b 0)) (end (nth b 1))
             (slice (org--slice (org--lines) start end))
             (ridx (- L start))
             (ncols (org--table-ncols slice))
             (nxt (org--next-data-row slice ridx)))
        (if nxt
            (org--retab-goto start end (org--format-table slice) nxt 0)
            (let* ((slice2 (append slice (list (org--pipes (+ ncols 1)))))
                   (formatted2 (org--format-table slice2)))
              (org--retab-goto start end formatted2 (- (length formatted2) 1) 0)))))))

;; Swap the current column with its neighbour (data rows only); keep point in it.
(define (org--table-move-column delta)
  (let* ((L (buffer-line)) (b (org--table-bounds L)))
    (when b
      (let* ((start (nth b 0)) (end (nth b 1))
             (slice (org--slice (org--lines) start end))
             (ridx (- L start))
             (ncols (org--table-ncols slice))
             (col (org--cell-index-at (org--line-text L) (org--point-col)))
             (other (+ col delta)))
        (when (and (>= other 0) (< other ncols))
          (let ((swapped (map (lambda (l)
                                (if (org--table-sep? l) l
                                    (org--row-from-cells
                                     (org--list-swap (org--cells-pad l ncols) col other))))
                              slice)))
            (org--retab-goto start end (org--format-table swapped) ridx other)))))))
(define (org-table-move-column-right) (org--table-move-column 1))
(define (org-table-move-column-left)  (org--table-move-column -1))

;; Swap the current data row with the next / previous data row; keep point in it.
(define (org--table-move-row finder)
  (let* ((L (buffer-line)) (b (org--table-bounds L)))
    (when b
      (let* ((start (nth b 0)) (end (nth b 1))
             (slice (org--slice (org--lines) start end))
             (ridx (- L start)))
        (when (not (org--table-sep? (org--line-text L)))
          (let ((other (finder slice ridx)))
            (when other
              (let ((col (org--cell-index-at (org--line-text L) (org--point-col))))
                (org--retab-goto start end
                  (org--format-table (org--list-swap slice ridx other)) other col)))))))))
(define (org-table-move-row-down) (org--table-move-row org--next-data-row))
(define (org-table-move-row-up)   (org--table-move-row org--prev-data-row))

;; Insert / delete a column at point.
(define (org-table-insert-column)
  (let* ((L (buffer-line)) (b (org--table-bounds L)))
    (when b
      (let* ((start (nth b 0)) (end (nth b 1))
             (slice (org--slice (org--lines) start end))
             (ridx (- L start))
             (ncols (org--table-ncols slice))
             (col (org--cell-index-at (org--line-text L) (org--point-col))))
        (let ((grown (map (lambda (l)
                            (if (org--table-sep? l) l
                                (let* ((cells (org--cells-pad l ncols))
                                       (head (org--slice cells 0 (- col 1)))
                                       (tail (org--slice cells col (- ncols 1))))
                                  (org--row-from-cells (append head (list "") tail)))))
                          slice)))
          (org--retab-goto start end (org--format-table grown) ridx col))))))
(define (org-table-delete-column)
  (let* ((L (buffer-line)) (b (org--table-bounds L)))
    (when b
      (let* ((start (nth b 0)) (end (nth b 1))
             (slice (org--slice (org--lines) start end))
             (ridx (- L start))
             (ncols (org--table-ncols slice))
             (col (org--cell-index-at (org--line-text L) (org--point-col))))
        (when (> ncols 1)
          (let ((shrunk (map (lambda (l)
                               (if (org--table-sep? l) l
                                   (org--row-from-cells
                                    (filter (lambda (x) (not (eq? x 'drop)))
                                      (let loop ((cs (org--cells-pad l ncols)) (i 0) (acc '()))
                                        (if (null? cs) (reverse acc)
                                            (loop (cdr cs) (+ i 1)
                                                  (cons (if (= i col) 'drop (car cs)) acc))))))))
                             slice)))
            (org--retab-goto start end (org--format-table shrunk) ridx
                             (min col (- ncols 2)))))))))

;; Insert a new empty row below / delete the current row.
(define (org-table-insert-row)
  (let* ((L (buffer-line)) (b (org--table-bounds L)))
    (when b
      (let* ((start (nth b 0)) (end (nth b 1))
             (slice (org--slice (org--lines) start end))
             (ridx (- L start))
             (ncols (org--table-ncols slice))
             (before (org--slice slice 0 ridx))
             (after  (org--slice slice (+ ridx 1) (- (length slice) 1)))
             (rows (append before (list (org--pipes (+ ncols 1))) after)))
        (org--retab-goto start end (org--format-table rows) (+ ridx 1) 0)))))
(define (org-table-delete-row)
  (let* ((L (buffer-line)) (b (org--table-bounds L)))
    (when b
      (let* ((start (nth b 0)) (end (nth b 1))
             (slice (org--slice (org--lines) start end))
             (ridx (- L start))
             (before (org--slice slice 0 (- ridx 1)))
             (after  (org--slice slice (+ ridx 1) (- (length slice) 1)))
             (rows (append before after)))
        (if (null? rows)
            (org--replace-line-range start end (list ""))
            (org--retab-goto start end (org--format-table rows)
                             (min ridx (- (length rows) 1)) 0))))))

;; Context-sensitive dispatchers: table action when in a table, else the
;; heading / default action (mirrors Org's overloaded TAB / RET / M-arrows).
(define (org--in-table?) (org--table-line? (org--line-text (buffer-line))))
(define (org--on-heading?) (and (org--heading-level (org--line-text (buffer-line))) #t))
(define (org-tab)        (if (org--in-table?) (org-table-next-cell) (org-cycle)))
(define (org-shift-tab)  (if (org--in-table?) (org-table-prev-cell) (org-global-cycle)))
(define (org-return)     (if (org--in-table?) (org-table-next-row) (newline)))
;; M-arrows act on the table column/row, else on the heading *at point* — never on
;; an enclosing heading from body text (Emacs leaves plain body alone).
(define (org-meta-right)
  (cond ((org--in-table?) (org-table-move-column-right))
        ((org--list-line-prefix (org--line-text (buffer-line))) (org-indent-item))
        ((org--on-heading?) (org-demote)) (else #f)))
(define (org-meta-left)
  (cond ((org--in-table?) (org-table-move-column-left))
        ((org--list-line-prefix (org--line-text (buffer-line))) (org-outdent-item))
        ((org--on-heading?) (org-promote)) (else #f)))
(define (org-meta-up)
  (cond ((org--in-table?) (org-table-move-row-up)) ((org--on-heading?) (org-move-subtree-up)) (else #f)))
(define (org-meta-down)
  (cond ((org--in-table?) (org-table-move-row-down)) ((org--on-heading?) (org-move-subtree-down)) (else #f)))

;; C-k — like Emacs `org-kill-line`. On a collapsed heading the visible "line"
;; spans the whole folded subtree, so kill from point through the subtree's end
;; (Emacs leaves the empty heading line, like plain kill-line). Everywhere else
;; this is the ordinary kill-line (kill to end of line).
(define (org-kill-line)
  (let* ((L (buffer-line)) (line (org--line-text L)) (lvl (org--heading-level line)))
    (if (and lvl (not (eq? (org--heading-state L) 'subtree)))
        (let* ((headings (org--headings)) (total (org--total-lines))
               (E (org--subtree-end L lvl headings total))
               (s (point))
               (e (+ (org--line-start E) (utf8-length (org--line-text E)))))
          (if (> e s) (kill-region-between s e) (kill-line)))
        (kill-line))))

;; ===========================================================================
;; Layer 5b — babel (execute source blocks)
;; ===========================================================================
;; (begin-line end-line lang) for the src block containing line L, or #f.
(define (org--src-block-bounds L)
  (let* ((lines (org--lines)) (n (length lines)))
    (if (>= L n)
        #f
        (let up ((i L))
          (cond ((< i 0) #f)
                ((and (< i L) (org--block-end? (nth lines i))) #f)
                ((org--block-begin? (nth lines i))
                 (let dn ((j (+ i 1)))
                   (cond ((>= j n) #f)
                         ((org--block-end? (nth lines j))
                          (if (>= j L) (list i j (org--block-lang (nth lines i))) #f))
                         (else (dn (+ j 1))))))
                (else (up (- i 1))))))))

(define (org--run-src lang body)
  (let ((cmd (cond ((or (equal? lang "sh") (equal? lang "bash") (equal? lang "shell")) (list "bash"))
                   ((or (equal? lang "python") (equal? lang "python3")) (list "python3"))
                   ((equal? lang "ruby") (list "ruby"))
                   ((or (equal? lang "js") (equal? lang "javascript") (equal? lang "node")) (list "node"))
                   (else #f))))
    (if (not cmd)
        (string-append "Unsupported src language: " lang)
        (let* ((res (call-process-full (car cmd) (cdr cmd) (string-append body "\n")))
               (out (trim-end (nth res 1))) (err (trim-end (nth res 2))))
          (if (equal? out "") err out)))))

;; Existing #+RESULTS block right after `start` (skipping blanks): (s e) or #f.
(define (org--existing-results lines start n)
  (let skip ((i start))
    (cond ((>= i n) #f)
          ((equal? (org--trim (nth lines i)) "") (skip (+ i 1)))
          ((>= (string-match "^[ \t]*#\\+[rR][eE][sS][uU][lL][tT][sS]:" (nth lines i)) 0)
           (let body ((j (+ i 1)))
             (cond ((>= j n) (list i (- j 1)))
                   ((or (>= (string-match "^[ \t]*:" (nth lines j)) 0)
                        (>= (string-match "^[ \t]*\\|" (nth lines j)) 0))
                    (body (+ j 1)))
                   (else (list i (- j 1))))))
          (else #f))))

;; C-c C-c inside a src block — run it, write a #+RESULTS block after it.
(define (org-babel-execute)
  (let ((L0 (buffer-line))
        (b (org--src-block-bounds (buffer-line))))
    (if (not b)
        (message "Not in a src block")
        (let* ((el (nth b 1)) (lang (nth b 2))
               (lines (org--lines)) (n (length lines))
               (body (org--expand-noweb (join-lines (org--slice lines (+ (nth b 0) 1) (- el 1)))))
               (out (org--run-src lang body))
               (res-lines (cons "#+RESULTS:" (map (lambda (l) (string-append ": " l)) (split-many out "\n"))))
               (ex (org--existing-results lines (+ el 1) n))
               (rest-start (if ex (+ (nth ex 1) 1) (+ el 1)))
               (before (org--slice lines 0 el))
               (rest (org--slice lines rest-start (- n 1))))
          (org--replace-all (join-lines (append before res-lines (list "") rest)))
          (goto-line L0)
          (message "Block executed")))))

;; ===========================================================================
;; Layer 6 — inline images + LaTeX preview
;; ===========================================================================
;; These display decoded bitmaps on rows reserved below their line, via the
;; `set-line-image` / `clear-line-images` primitives. The image and LaTeX
;; toggles share that one display layer, so turning one on clears the other.

;; --- inline images ----------------------------------------------------------
;; Absolute path of the local image a `[[file:…]]` / `[[…]]` link points at, or
;; #f. Like Org, only undescribed links to a recognised image type count.
;; Raster types the `image` crate decodes. (SVG is vector — not handled here, so
;; an .svg link stays a normal clickable link rather than silently blank.)
(define (org--image-path? p)
  (>= (string-match "(?i)\\.(png|jpe?g|gif|bmp|webp)$" p) 0))
(define (org--resolve-path p)
  (cond ((>= (string-match "^[/~]" p) 0) p)            ; absolute / home
        (else (path-join (buffer-directory) p))))
(define (org--image-link-path line)
  (let ((c (regexp-captures "\\[\\[(file:)?([^]]+)\\]\\]" line)))
    (if (and (not (null? c)) (org--image-path? (nth c 2)))
        (org--resolve-path (nth c 2))
        #f)))

;; Per-buffer "shown?" flags for the two displays (defined up front so either
;; show-fn can clear the other — they share the one line-image layer).
(define *org-images-shown* '())
(define *org-latex-shown* '())
(define (org--images-shown?)
  (if (alist-ref *org-images-shown* (current-buffer)) #t #f))
(define (org--set-images-shown v)
  (set! *org-images-shown*
        (cons (cons (current-buffer) v)
              (filter (lambda (p) (not (equal? (car p) (current-buffer)))) *org-images-shown*))))
(define (org--latex-shown?)
  (if (alist-ref *org-latex-shown* (current-buffer)) #t #f))
(define (org--set-latex-shown v)
  (set! *org-latex-shown*
        (cons (cons (current-buffer) v)
              (filter (lambda (p) (not (equal? (car p) (current-buffer)))) *org-latex-shown*))))

(define (org-display-inline-images)
  (clear-line-images)
  (org--set-latex-shown #f)
  (let loop ((lines (org--lines)) (i 0) (n 0))
    (if (null? lines)
        (begin (org--set-images-shown #t)
               (message (string-append "Displaying " (int->string n) " inline image(s)")))
        (let ((p (org--image-link-path (car lines))))
          (if p
              (begin (set-line-image i p) (loop (cdr lines) (+ i 1) (+ n 1)))
              (loop (cdr lines) (+ i 1) n))))))

(define (org-remove-inline-images)
  (clear-line-images) (org--set-images-shown #f) (message "Inline images hidden"))

;; C-c C-x C-v — toggle inline image display.
(define (org-toggle-inline-images)
  (if (org--images-shown?) (org-remove-inline-images) (org-display-inline-images)))

;; --- LaTeX preview ----------------------------------------------------------
;; Render `$…$` fragments to PNG with the system TeX toolchain (latex + dvipng)
;; and display them inline — the org-preview-latex model. Tunable from init.scm.
(define *org-latex-density* "150")               ; dvipng -D (dots per inch)
(define *org-latex-fg* "rgb 0.85 0.85 0.85")     ; light, for dark themes
(define (org--latex-tmpdir)
  (let ((t (getenv "TMPDIR"))) (path-join (if (equal? t "") "/tmp" t) "macros-org-latex")))

;; Inner math of the first `$…$` on a line, or #f.
(define (org--latex-fragment line)
  (let ((c (regexp-captures "\\$([^$\n]+)\\$" line))) (if (null? c) #f (nth c 1))))

;; Compile one fragment → png path (or #f). Writes a standalone doc, runs latex
;; then dvipng in a temp dir; both shell out via call-process(-full).
(define (org--latex->png inner idx)
  (let* ((dir (org--latex-tmpdir))
         (base (string-append "frag-" (int->string idx)))
         (tex (path-join dir (string-append base ".tex")))
         (dvi (path-join dir (string-append base ".dvi")))
         (png (path-join dir (string-append base ".png")))
         (doc (string-append
                "\\documentclass[preview,border=2pt]{standalone}\n"
                "\\usepackage{amsmath,amssymb,amsfonts}\n"
                "\\begin{document}\n\\( " inner " \\)\n\\end{document}\n")))
    (write-file tex doc)
    (call-process-full "latex"
      (list "-interaction=nonstopmode" "-halt-on-error" "-output-directory" dir tex) "")
    (if (not (file-exists? dvi))
        #f
        (begin
          (call-process-full "dvipng"
            (list "-D" *org-latex-density* "-T" "tight" "-bg" "Transparent"
                  "-fg" *org-latex-fg* "-o" png dvi) "")
          (if (file-exists? png) png #f)))))

(define (org-show-latex-previews)
  (clear-line-images)
  (org--set-images-shown #f)
  (call-process "mkdir" (list "-p" (org--latex-tmpdir)))
  (let loop ((lines (org--lines)) (i 0) (idx 0) (n 0))
    (if (null? lines)
        (begin (org--set-latex-shown #t)
               (message (if (= n 0)
                            "No $…$ fragments (or latex/dvipng not found)"
                            (string-append "Rendered " (int->string n) " LaTeX fragment(s)"))))
        (let ((frag (org--latex-fragment (car lines))))
          (if frag
              (let ((png (org--latex->png frag idx)))
                (when png (set-line-image i png))
                (loop (cdr lines) (+ i 1) (+ idx 1) (if png (+ n 1) n)))
              (loop (cdr lines) (+ i 1) idx n))))))

(define (org-remove-latex-previews)
  (clear-line-images) (org--set-latex-shown #f) (message "LaTeX previews hidden"))

;; C-c C-x C-l — toggle LaTeX fragment previews.
(define (org-toggle-latex-preview)
  (if (org--latex-shown?) (org-remove-latex-previews) (org-show-latex-previews)))

;; ===========================================================================
;; C-c C-c dispatcher (context-sensitive, like Org's)
;; ===========================================================================
(define (org-ctrl-c-ctrl-c)
  (let* ((L (buffer-line)) (line (org--line-text L)))
    (cond ((org--src-block-bounds L) (org-babel-execute))
          ((org--table-line? line) (org-table-recalculate))
          ((>= (string-match "src_[A-Za-z0-9]+(\\[[^]]*\\])?\\{" line) 0) (org-babel-execute-inline))
          ((org--heading-level line) (org-set-tags))
          ((org-toggle-checkbox) #t)
          (else (message "C-c C-c: nothing to do here")))))

;; Flip leading-star hiding for the session and repaint.
(define (org-toggle-leading-stars)
  (set! *org-hide-leading-stars* (not *org-hide-leading-stars*))
  (org--fontify)
  (message (if *org-hide-leading-stars*
               "Leading stars hidden"
               "Leading stars shown")))

;; Flip virtual indentation for the session and repaint.
(define (org-toggle-indent)
  (set! *org-indent* (not *org-indent*))
  (org--fontify)
  (message (if *org-indent* "Org indent on" "Org indent off")))

;; ===========================================================================
;; ===========================================================================
;; Layer 7 — calendar / date arithmetic (pure)
;; ===========================================================================
;; Proleptic-Gregorian day count (days since 1970-01-01), via Howard Hinnant's
;; algorithm. All our dates are positive years, so plain `quotient` suffices.
;; This is the substrate for scheduling, repeaters, and relative date entry.
(define (org--ymd->days y m d)
  (let* ((y2 (if (<= m 2) (- y 1) y))
         (era (quotient y2 400))
         (yoe (- y2 (* era 400)))
         (doy (+ (quotient (+ (* 153 (+ m (if (> m 2) -3 9))) 2) 5) (- d 1)))
         (doe (+ (* yoe 365) (quotient yoe 4) (- (quotient yoe 100)) doy)))
    (- (+ (* era 146097) doe) 719468)))

(define (org--days->ymd z0)
  (let* ((z (+ z0 719468))
         (era (quotient z 146097))
         (doe (- z (* era 146097)))
         (yoe (quotient (+ (- doe (quotient doe 1460))
                           (quotient doe 36524) (- (quotient doe 146096)))
                        365))
         (y (+ yoe (* era 400)))
         (doy (- doe (+ (* 365 yoe) (quotient yoe 4) (- (quotient yoe 100)))))
         (mp (quotient (+ (* 5 doy) 2) 153))
         (d (+ (- doy (quotient (+ (* 153 mp) 2) 5)) 1))
         (m (+ mp (if (< mp 10) 3 -9))))
    (list (if (<= m 2) (+ y 1) y) m d)))

(define (org--pad2 n) (if (< n 10) (string-append "0" (number->string n)) (number->string n)))
(define *org-dow-names* (list "Sun" "Mon" "Tue" "Wed" "Thu" "Fri" "Sat"))
;; 1970-01-01 (day 0) is a Thursday (index 4), so (days+4) mod 7 is the weekday.
(define (org--dow-name days) (nth *org-dow-names* (modulo (+ days 4) 7)))

;; "YYYY-MM-DD" (anywhere in `s`) → (y m d), or #f.
(define (org--parse-date s)
  (let ((c (regexp-captures "([0-9]{4})-([0-9]{2})-([0-9]{2})" s)))
    (if (null? c) #f
        (list (string->number (nth c 1)) (string->number (nth c 2)) (string->number (nth c 3))))))
(define (org--date-str->days s)
  (let ((ymd (org--parse-date s)))
    (if ymd (org--ymd->days (nth ymd 0) (nth ymd 1) (nth ymd 2)) #f)))
(define (org--days->date-str days)
  (let ((ymd (org--days->ymd days)))
    (string-append (number->string (nth ymd 0)) "-" (org--pad2 (nth ymd 1)) "-"
                   (org--pad2 (nth ymd 2)) " " (org--dow-name days))))
(define (org--today-days) (org--date-str->days (current-time "%Y-%m-%d")))

(define (org--leap? y) (and (= 0 (modulo y 4)) (or (not (= 0 (modulo y 100))) (= 0 (modulo y 400)))))
(define (org--month-days y m)
  (cond ((or (= m 1) (= m 3) (= m 5) (= m 7) (= m 8) (= m 10) (= m 12)) 31)
        ((= m 2) (if (org--leap? y) 29 28))
        (else 30)))
;; Add `n` calendar months to (y m d), clamping the day to the target month.
(define (org--add-months y m d n)
  (let* ((total (+ (* y 12) (- m 1) n))
         (ny (quotient total 12))
         (nm (+ 1 (modulo total 12))))
    (list ny nm (min d (org--month-days ny nm)))))

;; Apply a "+N", "+Nd/w/m/y" (or negative) offset to a base day count.
(define (org--apply-offset s base)
  (let* ((c (regexp-captures "^([+-])([0-9]+)([dwmy]?)$" s)))
    (if (null? c) base
        (let* ((sign (if (equal? (nth c 1) "-") -1 1))
               (num (* sign (string->number (nth c 2))))
               (unit (nth c 3)))
          (cond ((or (equal? unit "") (equal? unit "d")) (+ base num))
                ((equal? unit "w") (+ base (* 7 num)))
                ((equal? unit "m")
                 (let* ((ymd (org--days->ymd base))
                        (r (org--add-months (nth ymd 0) (nth ymd 1) (nth ymd 2) num)))
                   (org--ymd->days (nth r 0) (nth r 1) (nth r 2))))
                ((equal? unit "y")
                 (let* ((ymd (org--days->ymd base))
                        (r (org--add-months (nth ymd 0) (nth ymd 1) (nth ymd 2) (* 12 num))))
                   (org--ymd->days (nth r 0) (nth r 1) (nth r 2))))
                (else base))))))

;; Interpret a user date string against `base` (day count): "", ".", "today",
;; "tomorrow"/"yesterday", an absolute YYYY-MM-DD, or a relative ±N[dwmy].
(define (org--read-date input base)
  (let ((s (org--trim input)))
    (cond ((or (equal? s "") (equal? s ".") (equal? (string-downcase s) "today")) base)
          ((equal? (string-downcase s) "tomorrow") (+ base 1))
          ((equal? (string-downcase s) "yesterday") (- base 1))
          ((org--parse-date s) (org--date-str->days s))
          ((>= (string-match "^[+-][0-9]+[dwmy]?$" s) 0) (org--apply-offset s base))
          (else base))))

;; ===========================================================================
;; Layer 7b — properties drawers + the entry property API
;; ===========================================================================
(define (org--planning-line? line)
  (>= (string-match "^[ \t]*(SCHEDULED|DEADLINE|CLOSED):" line) 0))

;; (start . end) line indices of the :PROPERTIES: … :END: drawer belonging to
;; heading H (it may follow a planning line), or #f.
(define (org--property-drawer-bounds H)
  (let* ((lines (org--lines)) (n (length lines)))
    (let loop ((i (+ H 1)))
      (cond ((>= i n) #f)
            ((org--heading-level (nth lines i)) #f)
            ((>= (string-match "^[ \t]*:PROPERTIES:[ \t]*$" (nth lines i)) 0)
             (let end ((j (+ i 1)))
               (cond ((>= j n) #f)
                     ((>= (string-match "^[ \t]*:END:[ \t]*$" (nth lines j)) 0) (cons i j))
                     ((org--heading-level (nth lines j)) #f)
                     (else (end (+ j 1))))))
            ((org--planning-line? (nth lines i)) (loop (+ i 1)))
            (else #f)))))

;; Parse the property lines between drawer bounds [s,e] → alist (KEY . VALUE).
(define (org--drawer-props lines s e)
  (let loop ((i (+ s 1)) (acc '()))
    (if (>= i e) (reverse acc)
        (let ((c (regexp-captures "^[ \t]*:([A-Za-z0-9_@%-]+):[ \t]*(.*)$" (nth lines i))))
          (if (null? c) (loop (+ i 1) acc)
              (loop (+ i 1) (cons (cons (nth c 1) (org--trim (nth c 2))) acc)))))))

;; Case-insensitive alist lookup → the (key . value) pair, or #f.
(define (org--assoc-ci alist key)
  (let ((k (string-upcase key)))
    (let loop ((a alist))
      (cond ((null? a) #f)
            ((equal? (string-upcase (car (car a))) k) (car a))
            (else (loop (cdr a)))))))

;; The nearest ancestor heading line of H (level strictly shallower), or #f.
(define (org--parent-heading H)
  (let ((entry (org--find-heading (org--headings) H)))
    (if (not entry) #f
        (let ((lvl (nth entry 1)))
          (let loop ((hs (org--headings)) (parent #f))
            (cond ((null? hs) parent)
                  ((>= (car (car hs)) H) parent)
                  ((< (nth (car hs) 1) lvl) (loop (cdr hs) (car (car hs))))
                  (else (loop (cdr hs) parent))))))))

;; org-entry-get: the value of property KEY on heading H, or #f.
(define (org--entry-get-local H key)
  (let ((b (org--property-drawer-bounds H)))
    (if (not b) #f
        (let ((p (org--assoc-ci (org--drawer-props (org--lines) (car b) (cdr b)) key)))
          (if p (cdr p) #f)))))
(define (org-entry-get H key) (org--entry-get-local H key))

;; Like org-entry-get with inheritance: search H and its ancestors.
(define (org-entry-get-inherited H key)
  (let loop ((h H))
    (if (not h) #f
        (let ((v (org--entry-get-local h key)))
          (if v v (loop (org--parent-heading h)))))))

(define (org--render-drawer props)
  (append (list ":PROPERTIES:")
          (map (lambda (p) (string-append ":" (car p) ": " (cdr p))) props)
          (list ":END:")))

;; Line after which a new drawer is inserted: the heading, or its planning line.
(define (org--planning-end H)
  (let ((lines (org--lines)))
    (if (and (< (+ H 1) (length lines)) (org--planning-line? (nth lines (+ H 1)))) (+ H 1) H)))

(define (org--insert-lines-after idx new-lines)
  (let ((pos (+ (org--line-start idx) (utf8-length (org--line-text idx)))))
    (insert-at pos (string-append "\n" (join-lines new-lines)))))

;; org-entry-put: set property KEY=VALUE on heading H (creating the drawer).
(define (org-entry-put H key value)
  (let* ((lines (org--lines)) (b (org--property-drawer-bounds H)))
    (if b
        (let* ((s (car b)) (e (cdr b))
               (props (org--drawer-props lines s e))
               (existing (org--assoc-ci props key))
               (newprops (if existing
                             (map (lambda (p)
                                    (if (equal? (string-upcase (car p)) (string-upcase key))
                                        (cons (car p) value) p))
                                  props)
                             (append props (list (cons key value))))))
          (org--replace-line-range s e (org--render-drawer newprops)))
        (org--insert-lines-after (org--planning-end H)
                                 (org--render-drawer (list (cons key value)))))))

;; C-c C-x p — set a property interactively.
(define *org--prop-target* -1)
(define *org--prop-key* "")
(define (org-set-property)
  (let ((H (org--heading-line-at-point)))
    (if (not H) (message "Not on a heading")
        (begin (set! *org--prop-target* H)
               (read-string "Property: " "org--set-property-key")))))
(define (org--set-property-key key)
  (when (not (equal? (org--trim key) ""))
    (set! *org--prop-key* (org--trim key))
    (read-string (string-append (org--trim key) ": ") "org--set-property-value"
                 (or (org--entry-get-local *org--prop-target* (org--trim key)) ""))))
(define (org--set-property-value val)
  (org-entry-put *org--prop-target* *org--prop-key* (org--trim val))
  (message (string-append "Set " *org--prop-key*)))

;; org-id-get-create — return the heading's :ID:, minting and storing one if absent.
(define (org-id-get-create)
  (let ((H (org--heading-line-at-point)))
    (if (not H) (begin (message "Not on a heading") #f)
        (let ((existing (org--entry-get-local H "ID")))
          (if existing existing
              (let ((id (org-id-new))) (org-entry-put H "ID" id) id))))))

;; ===========================================================================
;; Layer 7c — scheduling / deadlines / timestamps (+ repeaters)
;; ===========================================================================
;; The planning line right below heading H (SCHEDULED/DEADLINE/CLOSED), or #f.
(define (org--planning-line-index H)
  (let ((lines (org--lines)))
    (if (and (< (+ H 1) (length lines)) (org--planning-line? (nth lines (+ H 1)))) (+ H 1) #f)))

;; The "<…>"/"[…]" stamp for KIND on a planning line, or #f.
(define (org--planning-get line kind)
  (let ((c (regexp-captures (string-append kind ":[ \t]*([<\\[][^]>\n]*[>\\]])") line)))
    (if (null? c) #f (nth c 1))))

(define (org--render-planning sched dead closed)
  (org--trim (string-append
              (if closed (string-append "CLOSED: " closed " ") "")
              (if dead (string-append "DEADLINE: " dead " ") "")
              (if sched (string-append "SCHEDULED: " sched " ") ""))))

(define *org--plan-target* -1)
(define *org--plan-kind* "SCHEDULED")
(define (org-schedule) (org--plan-read "SCHEDULED"))
(define (org-deadline) (org--plan-read "DEADLINE"))
(define (org--plan-read kind)
  (let ((H (org--heading-line-at-point)))
    (if (not H) (message "Not on a heading")
        (begin (set! *org--plan-target* H) (set! *org--plan-kind* kind)
               (read-string (string-append kind " (date / +Nd / today): ")
                            "org--plan-finish" "")))))
(define (org--plan-finish input)
  (let* ((H *org--plan-target*) (kind *org--plan-kind*)
         (stamp (string-append "<" (org--days->date-str (org--read-date input (org--today-days))) ">"))
         (pidx (org--planning-line-index H))
         (old (if pidx (org--line-text pidx) ""))
         (sched (if (equal? kind "SCHEDULED") stamp (org--planning-get old "SCHEDULED")))
         (dead (if (equal? kind "DEADLINE") stamp (org--planning-get old "DEADLINE")))
         (closed (org--planning-get old "CLOSED"))
         (newline (string-append "  " (org--render-planning sched dead closed))))
    (if pidx (org--replace-line pidx newline) (org--insert-lines-after H (list newline)))
    (message (string-append kind " set"))))

;; Insert an active/inactive timestamp at point.
(define (org-time-stamp)
  (insert (string-append "<" (org--days->date-str (org--today-days)) ">")))
(define (org-time-stamp-inactive)
  (insert (string-append "[" (org--days->date-str (org--today-days)) "]")))

;; --- repeaters --------------------------------------------------------------
;; A planning line carries a repeater (+N / ++N / .+N with a unit)?
(define (org--has-repeater? H)
  (let ((p (org--planning-line-index H)))
    (and p (>= (string-match "(\\.\\+|\\+\\+|\\+)[0-9]+[dwmy]" (org--line-text p)) 0))))

;; Shift one timestamp string forward per its repeater, preserving brackets and
;; the repeater cookie. `+N`/`++N` advance from the stamp's date (`++` until past
;; today); `.+N` advances from today.
(define (org--shift-stamp stamp)
  (let ((rc (regexp-captures "(\\.\\+|\\+\\+|\\+)([0-9]+)([dwmy])" stamp))
        (days (org--date-str->days stamp)))
    (if (or (null? rc) (not days)) stamp
        (let* ((mark (nth rc 1)) (num (string->number (nth rc 2))) (unit (nth rc 3))
               (offset (string-append "+" (number->string num) unit))
               (today (org--today-days))
               (newdays (cond ((equal? mark ".+") (org--apply-offset offset today))
                              ((equal? mark "++")
                               (let lp ((d (org--apply-offset offset days)))
                                 (if (> d today) d (lp (org--apply-offset offset d)))))
                              (else (org--apply-offset offset days)))))
          (string-append (substring stamp 0 1)
                         (org--days->date-str newdays) " " mark (number->string num) unit
                         (substring stamp (- (string-length stamp) 1) (string-length stamp)))))))

;; Shift every repeating active timestamp on a planning line.
(define (org--shift-planning line)
  (let loop ((s line) (acc ""))
    (let ((m (string-match "<[^>\n]*(\\.\\+|\\+\\+|\\+)[0-9]+[dwmy][^>\n]*>" s)))
      (if (< m 0) (string-append acc s)
          (let* ((c (regexp-captures "<[^>\n]*(\\.\\+|\\+\\+|\\+)[0-9]+[dwmy][^>\n]*>" s))
                 (whole (car c))
                 (before (substring s 0 m))
                 (after (org--byte-drop s (+ m (utf8-length whole)))))
            (loop after (string-append acc before (org--shift-stamp whole))))))))

(define (org--repeat-task H)
  (let* ((p (org--planning-line-index H)) (line (org--line-text p)))
    (org--replace-line p (org--shift-planning line))
    (org--apply-keyword H (if (null? *org-todo-keywords*) "" (car *org-todo-keywords*)))
    (message "Repeating task shifted to next occurrence")))

;; ===========================================================================
;; Layer 7d — clocking (CLOCK lines in a LOGBOOK drawer) + clock table
;; ===========================================================================
;; Minutes since the epoch for an Org stamp "[YYYY-MM-DD … HH:MM]" (no time → 0).
(define (org--stamp-minutes stamp)
  (let ((days (org--date-str->days stamp))
        (tc (regexp-captures "([0-9]{2}):([0-9]{2})" stamp)))
    (if (not days) #f
        (+ (* days 1440)
           (if (null? tc) 0 (+ (* 60 (string->number (nth tc 1))) (string->number (nth tc 2))))))))
(define (org--fmt-duration mins)
  (string-append (number->string (quotient mins 60)) ":" (org--pad2 (modulo mins 60))))
(define (org--now-stamp) (current-time "%Y-%m-%d %a %H:%M"))

;; Last content line of H's own section (before its first child / next heading).
(define (org--entry-content-end H)
  (let ((lines (org--lines)) (n (org--total-lines)))
    (let loop ((i (+ H 1)))
      (cond ((>= i n) (- n 1))
            ((org--heading-level (nth lines i)) (- i 1))
            (else (loop (+ i 1)))))))

;; (start . end) line indices of the :NAME: … :END: drawer in H's section, or #f.
(define (org--named-drawer-bounds H name)
  (let ((lines (org--lines)) (e (org--entry-content-end H))
        (pat (string-append "^[ \t]*:" name ":[ \t]*$")))
    (let loop ((i (+ H 1)))
      (cond ((> i e) #f)
            ((>= (string-match pat (nth lines i)) 0)
             (let end ((j (+ i 1)))
               (cond ((> j e) #f)
                     ((>= (string-match "^[ \t]*:END:[ \t]*$" (nth lines j)) 0) (cons i j))
                     (else (end (+ j 1))))))
            (else (loop (+ i 1)))))))

;; Prepend a LOGBOOK entry to H (creating the drawer below any PROPERTIES/planning).
(define (org--logbook-add H entry)
  (let ((b (org--named-drawer-bounds H "LOGBOOK")))
    (if b
        (org--insert-lines-after (car b) (list (string-append "  " entry)))
        (let* ((pb (org--property-drawer-bounds H))
               (after (cond (pb (cdr pb))
                            ((org--planning-line-index H) (org--planning-line-index H))
                            (else H))))
          (org--insert-lines-after after (list "  :LOGBOOK:" (string-append "  " entry) "  :END:"))))))

;; The line index of an open clock (CLOCK: [start] with no end) anywhere, or #f.
(define (org--open-clock-line)
  (let ((lines (org--lines)) (n (org--total-lines)))
    (let loop ((i 0))
      (cond ((>= i n) #f)
            ((>= (string-match "^[ \t]*CLOCK:[ \t]*\\[[^]]*\\][ \t]*$" (nth lines i)) 0) i)
            (else (loop (+ i 1)))))))

;; C-c C-x C-i — start a clock on the heading at point.
(define (org-clock-in)
  (let ((H (org--heading-line-at-point)))
    (if (not H) (message "Not on a heading")
        (if (org--open-clock-line) (message "A clock is already running")
            (begin (org--logbook-add H (string-append "CLOCK: [" (org--now-stamp) "]"))
                   (message "Clock started"))))))

;; C-c C-x C-o — stop the running clock, recording the elapsed time.
(define (org-clock-out)
  (let ((i (org--open-clock-line)))
    (if (not i) (message "No running clock")
        (let* ((line (org--line-text i))
               (start (nth (regexp-captures "\\[([^]]*)\\]" line) 1))
               (now (org--now-stamp))
               (mins (max 0 (- (org--stamp-minutes (string-append "[" now "]"))
                               (org--stamp-minutes (string-append "[" start "]"))))))
          (org--replace-line i (string-append "  CLOCK: [" start "]--[" now "] =>  "
                                              (org--fmt-duration mins)))
          (message (string-append "Clocked " (org--fmt-duration mins)))))))

(define (org--delete-lines a b)
  (let* ((n (org--total-lines))
         (sb (org--line-start a))
         (be (+ (org--line-start b) (utf8-length (org--line-text b)))))
    (cond ((< (+ b 1) n) (replace-region sb (+ be 1) ""))
          ((> a 0) (replace-region (- sb 1) be ""))
          (else (replace-region sb be "")))))

;; C-c C-x C-x — discard the running clock.
(define (org-clock-cancel)
  (let ((i (org--open-clock-line)))
    (if (not i) (message "No running clock")
        (begin (org--delete-lines i i) (message "Clock canceled")))))

;; --- clock table ------------------------------------------------------------
(define (org--parse-clock-mins line)
  (let ((c (regexp-captures "=>[ \t]*([0-9]+):([0-9]{2})" line)))
    (if (null? c) #f (+ (* 60 (string->number (nth c 1))) (string->number (nth c 2))))))
(define (org--acc-add acc key m)
  (if (org--assoc-ci acc key)
      (map (lambda (e) (if (equal? (car e) key) (cons (car e) (+ (cdr e) m)) e)) acc)
      (append acc (list (cons key m)))))

;; (total . ((title . minutes) …)) of all clocked time, attributed to the nearest
;; enclosing heading.
(define (org--clock-table-rows)
  (let loop ((ls (org--lines)) (cur "(before first heading)") (acc '()) (total 0))
    (if (null? ls)
        (cons total acc)
        (let* ((line (car ls)) (lvl (org--heading-level line)))
          (if lvl
              (let* ((rest (nth (org--heading-split line) 1))
                     (kw (org--leading-keyword rest)))
                (loop (cdr ls) (org--heading-title rest kw) acc total))
              (let ((m (org--parse-clock-mins line)))
                (if m (loop (cdr ls) cur (org--acc-add acc cur m) (+ total m))
                    (loop (cdr ls) cur acc total))))))))

(define (org--clocktable-lines)
  (let* ((data (org--clock-table-rows)) (total (car data)) (rows (cdr data)))
    (org--format-table
     (append
      (list "| Headline | Time |" "|-+-|"
            (string-append "| *Total time* | *" (org--fmt-duration total) "* |"))
      (map (lambda (e) (string-append "| " (car e) " | " (org--fmt-duration (cdr e)) " |")) rows)))))

;; Bounds of a #+BEGIN: clocktable … #+END: block enclosing line L, or #f.
(define (org--clocktable-bounds L)
  (let ((lines (org--lines)) (n (org--total-lines)))
    (let up ((i (min L (- n 1))))
      (cond ((< i 0) #f)
            ((>= (string-match "^[ \t]*#\\+BEGIN:[ \t]+clocktable" (nth lines i)) 0)
             (let dn ((j (+ i 1)))
               (cond ((>= j n) #f)
                     ((>= (string-match "^[ \t]*#\\+END:" (nth lines j)) 0) (cons i j))
                     (else (dn (+ j 1))))))
            ((>= (string-match "^[ \t]*#\\+END:" (nth lines i)) 0) #f)
            (else (up (- i 1)))))))

;; C-c C-x C-r — insert or refresh a clock report (dynamic clocktable block).
(define (org-clock-report)
  (let* ((L (buffer-line)) (b (org--clocktable-bounds L))
         (block (append (list "#+BEGIN: clocktable :maxlevel 3")
                        (org--clocktable-lines) (list "#+END:"))))
    (if b (org--replace-line-range (car b) (cdr b) block)
        (org--insert-lines-after L block))
    (message "Clock report updated")))

;; ===========================================================================
;; Layer 7e — refile + archive
;; ===========================================================================
(define (org--current-file-path) (path-join (buffer-directory) (current-buffer)))

;; (root-level . subtree-lines) for the subtree rooted at heading H.
(define (org--subtree-text H)
  (let* ((headings (org--headings)) (total (org--total-lines))
         (lvl (nth (org--find-heading headings H) 1))
         (E (org--subtree-end H lvl headings total)))
    (cons lvl (org--slice (org--lines) H E))))

;; Shift the depth of every heading line in `lines` by `delta` (clamped ≥ 1 star).
(define (org--shift-levels lines delta)
  (map (lambda (l)
         (let ((lvl (org--heading-level l)))
           (cond ((not lvl) l)
                 ((> delta 0) (string-append (org--stars delta) l))
                 ((< delta 0) (substring l (min (- 0 delta) (- lvl 1)) (string-length l)))
                 (else l))))
       lines))

(define (org--delete-subtree H)
  (let* ((headings (org--headings)) (total (org--total-lines))
         (lvl (nth (org--find-heading headings H) 1))
         (E (org--subtree-end H lvl headings total)))
    (org--delete-lines H E)))

;; Insert `block` as the last child of the heading at `tline` (level `tlvl`) in
;; the on-disk file `path`.
(define (org--insert-into-file path tline tlvl block)
  (let* ((lines (split-many (read-file path) "\n")) (n (length lines))
         (E (let loop ((i (+ tline 1)))
              (cond ((>= i n) (- n 1))
                    ((let ((l (org--heading-level (nth lines i)))) (and l (<= l tlvl))) (- i 1))
                    (else (loop (+ i 1))))))
         (before (org--slice lines 0 E))
         (after (org--slice lines (+ E 1) (- n 1))))
    (write-file path (join-lines (append before block after)))))

;; Reorder within the current buffer: move the subtree at H under heading tline.
(define (org--inbuffer-refile H tline tlvl)
  (let* ((lines (org--lines)) (total (length lines)) (headings (org--headings))
         (slvl (nth (org--find-heading headings H) 1))
         (E (org--subtree-end H slvl headings total))
         (block (org--shift-levels (org--slice lines H E) (- (+ tlvl 1) slvl)))
         (Et (org--subtree-end tline tlvl headings total))
         (without (append (org--slice lines 0 (- H 1)) (org--slice lines (+ E 1) (- total 1))))
         (blocklen (+ (- E H) 1))
         (insert-idx (if (> tline H) (- (+ Et 1) blocklen) (+ Et 1)))
         (newlines (append (org--slice without 0 (- insert-idx 1)) block
                           (org--slice without insert-idx (- (length without) 1)))))
    (org--replace-all (join-lines newlines))))

;; Candidate "Title  (file)" → (path line level), rebuilt on each refile.
(define *org-refile-map* '())
(define *org--refile-source* -1)
(define (org--refile-file-targets path)
  (let ((lines (split-many (read-file path) "\n")) (bn (basename path)))
    (let loop ((ls lines) (i 0) (acc '()))
      (if (null? ls) (reverse acc)
          (let ((lvl (org--heading-level (car ls))))
            (if lvl
                (let* ((rest (nth (org--heading-split (car ls)) 1))
                       (kw (org--leading-keyword rest))
                       (cand (string-append (org--heading-title rest kw) "  (" bn ")")))
                  (set! *org-refile-map* (cons (cons cand (list path i lvl)) *org-refile-map*))
                  (loop (cdr ls) (+ i 1) (cons cand acc)))
                (loop (cdr ls) (+ i 1) acc)))))))
(define (org--collect-refile-candidates)
  (set! *org-refile-map* '())
  (let loop ((files (org--agenda-file-list)) (acc '()))
    (if (null? files) acc (loop (cdr files) (append acc (org--refile-file-targets (car files)))))))

;; C-c C-w — refile the subtree at point under a chosen heading.
(define (org-refile)
  (let ((H (org--heading-line-at-point)))
    (if (not H) (message "Not on a heading")
        (begin (set! *org--refile-source* H)
               (completing-read "Refile to: " (org--collect-refile-candidates)
                                "org--refile-finish")))))
(define (org--refile-finish cand)
  (let ((tgt (alist-ref *org-refile-map* cand)))
    (if (not tgt) (message "No such refile target")
        (let* ((H *org--refile-source*)
               (tpath (nth tgt 0)) (tline (nth tgt 1)) (tlvl (nth tgt 2)))
          (if (equal? tpath (org--current-file-path))
              (org--inbuffer-refile H tline tlvl)
              (let* ((st (org--subtree-text H))
                     (block (org--shift-levels (cdr st) (- (+ tlvl 1) (car st)))))
                (org--insert-into-file tpath tline tlvl block)
                (org--delete-subtree H)))
          (message (string-append "Refiled to " (basename tpath)))))))

;; C-c C-x C-a — archive the subtree to <file>_archive.
(define (org--archive-file-path) (string-append (org--current-file-path) "_archive"))
(define (org-archive-subtree)
  (let ((H (org--heading-line-at-point)))
    (if (not H) (message "Not on a heading")
        (let* ((block (cdr (org--subtree-text H)))
               (stamp (string-append "  :ARCHIVE_TIME: [" (org--now-stamp) "]"))
               (with-time (append (list (car block)
                                        "  :PROPERTIES:" stamp "  :END:")
                                  (cdr block)))
               (apath (org--archive-file-path))
               (existing (if (file-exists? apath) (read-file apath) ""))
               (sep (if (or (equal? existing "") (ends-with? existing "\n")) "" "\n")))
          (write-file apath (string-append existing sep (join-lines with-time) "\n"))
          (org--delete-subtree H)
          (message (string-append "Archived to " (basename apath)))))))

;; ===========================================================================
;; Layer 7f — capture templates
;; ===========================================================================
;; Each template is (KEY DESCRIPTION TARGET TEMPLATE-STRING) where TARGET is
;;   (list "file" PATH)               → append to the file, or
;;   (list "file+headline" PATH HEAD) → append under heading HEAD in PATH.
;; The template string may contain %? (point / prompt), %t %T (active date),
;; %u %U (inactive date / datetime). Example, from init.scm:
;;   (set! *org-capture-templates*
;;     (list (list "t" "Todo" (list "file" "~/org/inbox.org") "* TODO %?\n  %U")
;;           (list "n" "Note" (list "file+headline" "~/org/notes.org" "Notes")
;;                 "* %?\n  %U")))
;; (*org-capture-templates* itself is defined back in Layer 4 so org-capture can
;; see it.)
(define *org--capture-tmpl* #f)
(define *org--capture-expanded* "")

(define (org--capture-template-by-key key)
  (let loop ((es *org-capture-templates*))
    (cond ((null? es) #f) ((equal? (nth (car es) 0) key) (car es)) (else (loop (cdr es))))))

;; Non-interactive placeholder expansion (everything but %?).
(define (org--expand-template s)
  (let* ((today (org--days->date-str (org--today-days)))
         (s (replace-regexp-in-string "%T" (string-append "<" today ">") s))
         (s (replace-regexp-in-string "%t" (string-append "<" today ">") s))
         (s (replace-regexp-in-string "%U" (string-append "[" (org--now-stamp) "]") s))
         (s (replace-regexp-in-string "%u" (string-append "[" today "]") s)))
    s))
(define (org--expand-template-final s input) (replace-regexp-in-string "%\\?" input s))

(define (org--capture-pick)
  (completing-read "Template: "
    (map (lambda (e) (string-append (nth e 0) " — " (nth e 1))) *org-capture-templates*)
    "org--capture-chosen"))
(define (org--capture-chosen cand)
  (let* ((key (car (split-many cand " ")))
         (tmpl (org--capture-template-by-key key)))
    (if (not tmpl) (message "No such template")
        (begin
          (set! *org--capture-tmpl* tmpl)
          (set! *org--capture-expanded* (org--expand-template (nth tmpl 3)))
          (if (>= (string-match "%\\?" (nth tmpl 3)) 0)
              (read-string "Capture: " "org--capture-fill")
              (org--capture-commit (org--expand-template-final *org--capture-expanded* "")))))))
(define (org--capture-fill input)
  (org--capture-commit (org--expand-template-final *org--capture-expanded* input)))

(define (org--capture-append path text)
  (let* ((existing (if (file-exists? path) (read-file path) ""))
         (sep (if (or (equal? existing "") (ends-with? existing "\n")) "" "\n")))
    (write-file path (string-append existing sep text "\n"))))
(define (org--capture-under-headline path headline text)
  (let* ((lines (if (file-exists? path) (split-many (read-file path) "\n") '()))
         (n (length lines))
         (hidx (let loop ((i 0))
                 (cond ((>= i n) #f)
                       ((let ((l (nth lines i)))
                          (and (org--heading-level l)
                               (equal? (org--heading-title (nth (org--heading-split l) 1)
                                                           (org--leading-keyword (nth (org--heading-split l) 1)))
                                       headline))) i)
                       (else (loop (+ i 1)))))))
    (if (not hidx)
        (org--capture-append path text)
        (let* ((hlvl (org--heading-level (nth lines hidx)))
               (E (let loop ((i (+ hidx 1)))
                    (cond ((>= i n) (- n 1))
                          ((let ((l (org--heading-level (nth lines i)))) (and l (<= l hlvl))) (- i 1))
                          (else (loop (+ i 1))))))
               (before (org--slice lines 0 E))
               (after (org--slice lines (+ E 1) (- n 1))))
          (write-file path (join-lines (append before (list text) after)))))))
(define (org--capture-commit text)
  (let* ((target (nth *org--capture-tmpl* 2))
         (path (org--resolve-path (nth target 1))))
    (if (equal? (car target) "file+headline")
        (org--capture-under-headline path (nth target 2) text)
        (org--capture-append path text))
    (message "Captured")))

;; ===========================================================================
;; Layer 7g — tag search + custom agenda views
;; ===========================================================================
;; Match a tag expression against a tag list. "a+b" = AND, "a|b" = OR, a leading
;; "-" on a term negates it. Combine with care: "|" splits first.
(define (org--tag-match expr tags)
  (cond
    ((>= (string-match "\\|" expr) 0)
     (let any ((as (split-many expr "|")))
       (cond ((null? as) #f) ((org--tag-match (org--trim (car as)) tags) #t) (else (any (cdr as))))))
    (else
     (let all ((ts (filter (lambda (x) (not (equal? x ""))) (split-many expr "+"))))
       (cond ((null? ts) #t)
             (else (let* ((t (car ts)) (neg (>= (string-match "^-" t) 0))
                          (name (if neg (substring t 1 (string-length t)) t))
                          (present (and (member name tags) #t)))
                     (if (if neg (not present) present) (all (cdr ts)) #f))))))))

;; C-c \ — show every entry whose (inherited) tags match an expression.
(define (org-tags-view) (read-string "Match tags (work+urgent, a|b, -done): " "org--tags-view-finish"))
(define (org--tags-view-finish expr)
  (org--render-agenda
   (org--sort-entries
    (filter (lambda (e) (and (> (length e) 6) (org--tag-match (org--trim expr) (nth e 6))))
            (org--all-entries)))))

;; Custom agenda commands: each is (KEY DESCRIPTION TYPE ARG), TYPE one of
;; "agenda", "todo" (ARG = keyword), "tags" (ARG = tag expression). e.g.
;;   (set! *org-agenda-custom-commands*
;;     (list (list "w" "Waiting" "todo" "WAITING")
;;           (list "u" "Urgent"  "tags" "urgent")))
(define *org-agenda-custom-commands* '())
(define (org-agenda-dispatch)
  (if (null? *org-agenda-custom-commands*)
      (org-agenda)
      (completing-read "Agenda command: "
        (map (lambda (c) (string-append (nth c 0) " — " (nth c 1))) *org-agenda-custom-commands*)
        "org--agenda-dispatch-run")))
(define (org--agenda-dispatch-run cand)
  (let* ((key (car (split-many cand " ")))
         (c (let loop ((cs *org-agenda-custom-commands*))
              (cond ((null? cs) #f) ((equal? (nth (car cs) 0) key) (car cs)) (else (loop (cdr cs)))))))
    (if (not c) (message "No such command")
        (let ((type (nth c 2)) (arg (if (> (length c) 3) (nth c 3) "")))
          (cond ((equal? type "tags") (org--tags-view-finish arg))
                ((equal? type "todo")
                 (org--render-agenda
                  (org--sort-entries (filter (lambda (e) (equal? (nth e 4) arg)) (org--all-entries)))))
                (else (org-agenda)))))))

;; ===========================================================================
;; Layer 7h — link store/insert + footnotes
;; ===========================================================================
;; A ring of (TARGET . DESCRIPTION) links, pushed by org-store-link.
(define *org-stored-links* '())
;; C-c l — store a link to the current location (an id: link if the heading has
;; an :ID:, else a file: link).
(define (org-store-link)
  (let* ((H (org--heading-line-at-point))
         (link (if H
                   (let* ((rest (nth (org--heading-split (org--line-text H)) 1))
                          (title (org--heading-title rest (org--leading-keyword rest)))
                          (id (org--entry-get-local H "ID")))
                     (if id (cons (string-append "id:" id) title)
                         (cons (string-append "file:" (org--current-file-path)) title)))
                   (cons (string-append "file:" (org--current-file-path)) (current-buffer)))))
    (set! *org-stored-links* (cons link *org-stored-links*))
    (message (string-append "Stored link: " (car link)))))

;; C-c C-l — insert a link, choosing from the stored ring or typing a target.
(define (org-insert-link)
  (if (null? *org-stored-links*)
      (read-string "Link target: " "org--insert-link-target")
      (completing-read "Link: " (map car *org-stored-links*) "org--insert-link-chosen")))
(define (org--insert-link-target target)
  (when (not (equal? (org--trim target) ""))
    (insert (string-append "[[" (org--trim target) "]]"))))
(define (org--insert-link-chosen target)
  (let ((p (let loop ((ls *org-stored-links*))
             (cond ((null? ls) #f) ((equal? (car (car ls)) target) (car ls)) (else (loop (cdr ls)))))))
    (if p (insert (string-append "[[" (car p) "][" (cdr p) "]]"))
        (insert (string-append "[[" target "]]")))))

;; The largest numeric footnote label in the buffer (0 if none).
(define (org--max-fn-number)
  (let loop ((s (buffer-string)) (mx 0))
    (let ((m (string-match "\\[fn:([0-9]+)\\]" s)))
      (if (< m 0) mx
          (let* ((c (regexp-captures "\\[fn:([0-9]+)\\]" s))
                 (consumed (+ m (utf8-length (car c)))))
            (loop (org--byte-drop s consumed) (max mx (string->number (nth c 1)))))))))
;; C-c C-x f — insert a numbered footnote reference and a definition stub at EOF.
(define (org-footnote-new)
  (let ((label (string-append "fn:" (number->string (+ 1 (org--max-fn-number))))))
    (insert-at (point-max) (string-append "\n[" label "] "))
    (insert (string-append "[" label "]"))
    (message (string-append "Footnote [" label "] — definition added at end of file"))))

;; ===========================================================================
;; Layer 7i — table formulas (#+TBLFM:)
;; ===========================================================================
;; A small arithmetic evaluator (+ - * / parens, unary minus) over a tokenized
;; expression, driven by shunting-yard → RPN. Field references ($C, @R$C) are
;; substituted with their numeric values (wrapped in parens) before evaluation.
(define (org--char->str ch) (list->string (list ch)))
(define (org--digit-char? ch) (let ((cp (char->integer ch))) (or (and (>= cp 48) (<= cp 57)) (= cp 46))))
(define (org--op-char? ch)
  (let ((cp (char->integer ch))) (or (= cp 43) (= cp 45) (= cp 42) (= cp 47) (= cp 40) (= cp 41))))
(define (org--ws-char? ch) (let ((cp (char->integer ch))) (or (= cp 32) (= cp 9))))

(define (org--tokenize s)
  (let loop ((chars (string->list s)) (acc '()) (cur ""))
    (cond
      ((null? chars) (reverse (if (equal? cur "") acc (cons cur acc))))
      (else
       (let ((ch (car chars)) (rest (cdr chars)) (flushed (if (equal? cur "") acc (cons cur acc))))
         (cond
           ((org--digit-char? ch) (loop rest acc (string-append cur (org--char->str ch))))
           ((org--op-char? ch) (loop rest (cons (org--char->str ch) flushed) ""))
           ((org--ws-char? ch) (loop rest flushed ""))
           (else (loop rest acc (string-append cur (org--char->str ch))))))))))

(define (org--num-tok? t) (and (string->number t) #t))
(define (org--op-tok? t)
  (or (equal? t "+") (equal? t "-") (equal? t "*") (equal? t "/") (equal? t "u-")))
(define (org--prec op)
  (cond ((or (equal? op "+") (equal? op "-")) 1)
        ((or (equal? op "*") (equal? op "/")) 2)
        ((equal? op "u-") 3) (else 0)))

(define (org--to-rpn tokens)
  (let loop ((ts tokens) (prev #f) (out '()) (ops '()))
    (cond
      ((null? ts) (append (reverse out) ops))
      (else
       (let ((t (car ts)))
         (cond
           ((org--num-tok? t) (loop (cdr ts) t (cons t out) ops))
           ((equal? t "(") (loop (cdr ts) t out (cons t ops)))
           ((equal? t ")")
            (let pop ((o ops) (acc out))
              (cond ((null? o) (loop (cdr ts) t acc '()))
                    ((equal? (car o) "(") (loop (cdr ts) t acc (cdr o)))
                    (else (pop (cdr o) (cons (car o) acc))))))
           ((or (equal? t "+") (equal? t "-") (equal? t "*") (equal? t "/"))
            (let* ((unary (and (or (equal? t "-") (equal? t "+"))
                               (or (not prev) (equal? prev "(") (org--op-tok? prev))))
                   (tok (cond ((and unary (equal? t "-")) "u-")
                              ((and unary (equal? t "+")) "u+")
                              (else t))))
              (if (equal? tok "u+")
                  (loop (cdr ts) t out ops)
                  (let pop ((o ops) (acc out))
                    (cond ((and (not (null? o)) (not (equal? (car o) "("))
                                (not (equal? tok "u-"))
                                (>= (org--prec (car o)) (org--prec tok)))
                           (pop (cdr o) (cons (car o) acc)))
                          (else (loop (cdr ts) t acc (cons tok o))))))))
           (else (loop (cdr ts) t out ops))))))))

(define (org--apply-op op a b)
  (cond ((equal? op "+") (+ a b)) ((equal? op "-") (- a b)) ((equal? op "*") (* a b))
        ((equal? op "/") (if (= b 0) 0 (/ (* 1.0 a) (* 1.0 b)))) (else 0)))
(define (org--eval-rpn rpn)
  (let loop ((ts rpn) (stack '()))
    (cond
      ((null? ts) (if (null? stack) 0 (car stack)))
      (else
       (let ((t (car ts)))
         (cond
           ((equal? t "u-") (loop (cdr ts) (cons (- 0 (car stack)) (cdr stack))))
           ((org--op-tok? t)
            (let ((b (car stack)) (a (car (cdr stack))) (rest (cdr (cdr stack))))
              (loop (cdr ts) (cons (org--apply-op t a b) rest))))
           (else (loop (cdr ts) (cons (string->number t) stack)))))))))
(define (org--eval-arith expr) (org--eval-rpn (org--to-rpn (org--tokenize expr))))

;; Format a number for a cell: integers bare, others rounded to 6 dp, trailing
;; ".0" stripped.
(define (org--fmt-num n)
  (let* ((n2 (if (integer? n) n (/ (round (* n 1000000.0)) 1000000.0)))
         (s (number->string n2)))
    (if (ends-with? s ".0") (substring s 0 (- (string-length s) 2)) s)))

;; --- grid evaluation --------------------------------------------------------
(define (org--cell-num grid r c)
  (let* ((row (if (and (>= r 1) (<= r (length grid))) (nth grid (- r 1)) '()))
         (v (if (and (>= c 1) (<= c (length row))) (nth row (- c 1)) ""))
         (n (string->number (org--trim v))))
    (if n n 0)))
(define (org--subst-at grid expr)
  (let loop ((s expr) (acc ""))
    (let ((m (string-match "@([0-9]+)\\$([0-9]+)" s)))
      (if (< m 0) (string-append acc s)
          (let* ((c (regexp-captures "@([0-9]+)\\$([0-9]+)" s))
                 (whole (car c)) (r (string->number (nth c 1))) (col (string->number (nth c 2)))
                 (before (substring s 0 m)) (after (org--byte-drop s (+ m (utf8-length whole)))))
            (loop after (string-append acc before "(" (org--fmt-num (org--cell-num grid r col)) ")")))))))
(define (org--subst-dollar grid curr expr)
  (let loop ((s expr) (acc ""))
    (let ((m (string-match "\\$([0-9]+)" s)))
      (if (< m 0) (string-append acc s)
          (let* ((c (regexp-captures "\\$([0-9]+)" s))
                 (whole (car c)) (col (string->number (nth c 1)))
                 (before (substring s 0 m)) (after (org--byte-drop s (+ m (utf8-length whole)))))
            (loop after (string-append acc before "(" (org--fmt-num (org--cell-num grid curr col)) ")")))))))
(define (org--subst-refs expr grid curr) (org--subst-dollar grid curr (org--subst-at grid expr)))

(define (org--row-set cells col val)
  (let ((n (max col (length cells))))
    (let loop ((i 1) (acc '()))
      (if (> i n) (reverse acc) (loop (+ i 1) (cons (if (= i col) val (org--cell cells (- i 1))) acc))))))
(define (org--grid-set grid r col val)
  (let loop ((g grid) (i 1) (acc '()))
    (cond ((null? g) (reverse acc))
          ((= i r) (loop (cdr g) (+ i 1) (cons (org--row-set (car g) col val) acc)))
          (else (loop (cdr g) (+ i 1) (cons (car g) acc))))))

(define (org--apply-formula grid fstr)
  (let ((c (regexp-captures "^[ \t]*(@?[0-9]*\\$[0-9]+)[ \t]*=[ \t]*(.*)$" fstr)))
    (if (null? c) grid
        (let* ((lhs (nth c 1)) (rhs (nth c 2)) (atc (regexp-captures "@([0-9]+)\\$([0-9]+)" lhs)))
          (if (not (null? atc))
              (org--grid-set grid (string->number (nth atc 1)) (string->number (nth atc 2))
                             (org--fmt-num (org--eval-arith (org--subst-refs rhs grid (string->number (nth atc 1))))))
              (let ((col (string->number (nth (regexp-captures "\\$([0-9]+)" lhs) 1))))
                (let loop ((r 1) (g grid))
                  (if (> r (length grid)) g
                      (loop (+ r 1)
                            (org--grid-set g r col
                                           (org--fmt-num (org--eval-arith (org--subst-refs rhs g r)))))))))))))

;; (slice-index . cells) for each non-separator row of a table slice.
(define (org--table-grid slice)
  (let loop ((ls slice) (i 0) (acc '()))
    (cond ((null? ls) (reverse acc))
          ((org--table-sep? (car ls)) (loop (cdr ls) (+ i 1) acc))
          (else (loop (cdr ls) (+ i 1) (cons (cons i (org--table-cells (car ls))) acc))))))
(define (org--grid->slice slice rows grid2)
  (let loop ((ls slice) (i 0) (rs rows) (gs grid2) (acc '()))
    (cond ((null? ls) (reverse acc))
          ((and (not (null? rs)) (= (car (car rs)) i))
           (loop (cdr ls) (+ i 1) (cdr rs) (cdr gs) (cons (org--row-from-cells (car gs)) acc)))
          (else (loop (cdr ls) (+ i 1) rs gs (cons (car ls) acc))))))

(define (org--tblfm-after end)
  (let ((lines (org--lines)) (n (org--total-lines)))
    (if (and (< (+ end 1) n) (>= (string-match "^[ \t]*#\\+TBLFM:" (nth lines (+ end 1))) 0)) (+ end 1) #f)))
(define (org--tblfm-formulas line)
  (let ((c (regexp-captures "#\\+TBLFM:[ \t]*(.*)$" line)))
    (if (null? c) '() (filter (lambda (x) (not (equal? (org--trim x) ""))) (split-many (nth c 1) "::")))))

;; Recompute the table at point from its #+TBLFM: line (or just re-align it).
(define (org-table-recalculate)
  (let* ((L (buffer-line)) (b (org--table-bounds L)))
    (if (not b) (message "Not in a table")
        (let* ((start (nth b 0)) (end (nth b 1)) (fl (org--tblfm-after end)))
          (if (not fl) (org-table-align)
              (let* ((slice (org--slice (org--lines) start end))
                     (rows (org--table-grid slice))
                     (grid (map cdr rows))
                     (formulas (org--tblfm-formulas (org--line-text fl)))
                     (grid2 (let loop ((fs formulas) (g grid))
                              (if (null? fs) g (loop (cdr fs) (org--apply-formula g (car fs)))))))
                (org--replace-line-range start end (org--format-table (org--grid->slice slice rows grid2)))
                (message "Table recalculated")))))))

;; ===========================================================================
;; Layer 7j — export backends: Markdown, LaTeX, ASCII (walk the org-parse AST)
;; ===========================================================================
(define (org--range n) (let loop ((i 0) (acc '())) (if (>= i n) (reverse acc) (loop (+ i 1) (cons i acc)))))
(define (org--indent-lines text prefix)
  (join-lines (map (lambda (l) (string-append prefix l)) (split-many text "\n"))))
(define (org--ast-table->rows e)
  (map (lambda (r) (if (equal? (car r) "sep") "|-|" (org--row-from-cells (cdr r)))) (nth e 1)))
(define (org--output-path ext)
  (let* ((b (current-buffer)) (stem (if (ends-with? b ".org") (substring b 0 (- (string-length b) 4)) b)))
    (path-join (buffer-directory) (string-append stem ext))))

;; --- Markdown (GitHub-flavored) ---------------------------------------------
(define (org--inline->md s)
  (let* ((s (replace-regexp-in-string "\\[\\[([^]]+)\\]\\[([^]]+)\\]\\]" "[$2]($1)" s))
         (s (replace-regexp-in-string "\\[\\[([^]]+)\\]\\]" "[$1]($1)" s))
         (s (replace-regexp-in-string "\\*([^*\n]+)\\*" "**$1**" s))
         (s (replace-regexp-in-string "/([^/\n]+)/" "_$1_" s))
         (s (replace-regexp-in-string "=([^=\n]+)=" "`$1`" s))
         (s (replace-regexp-in-string "~([^~\n]+)~" "`$1`" s))
         (s (replace-regexp-in-string "\\+([^+\n]+)\\+" "~~$1~~" s))
         (s (replace-regexp-in-string "\\[fn:([^]\n]*)\\]" "[^$1]" s)))
    s))
(define (org--md-hashes n) (if (<= n 0) "" (string-append "#" (org--md-hashes (- n 1)))))
(define (org--list->md e ordered)
  (let loop ((its (nth e 1)) (i 1) (acc ""))
    (if (null? its) acc
        (let ((it (car its)))
          (loop (cdr its) (+ i 1)
                (string-append acc (if ordered (string-append (number->string i) ". ") "- ")
                               (if (equal? (car it) "") "" (string-append "[" (car it) "] "))
                               (org--inline->md (cdr it)) "\n"))))))
(define (org--md-row cells ncols)
  (string-append "|"
    (org--concat (map (lambda (i) (string-append " " (org--inline->md (org--cell cells i)) " |")) (org--range ncols)))
    "\n"))
(define (org--table->md e)
  (let* ((rows (filter (lambda (r) (not (equal? (car r) "sep"))) (nth e 1)))
         (ncols (let loop ((rs rows) (m 1)) (if (null? rs) m (loop (cdr rs) (max m (length (cdr (car rs)))))))))
    (if (null? rows) ""
        (string-append (org--md-row (cdr (car rows)) ncols)
                       "|" (org--concat (map (lambda (i) " --- |") (org--range ncols))) "\n"
                       (org--concat (map (lambda (r) (org--md-row (cdr r) ncols)) (cdr rows)))))))
(define (org--block->md e)
  (let ((type (car e)))
    (cond
      ((equal? type "heading")
       (string-append (org--md-hashes (min 6 (nth e 1))) " "
                      (if (equal? (nth e 2) "") "" (string-append (nth e 2) " "))
                      (org--inline->md (nth e 4))
                      (if (equal? (nth e 5) "") "" (string-append "  :" (nth e 5) ":")) "\n\n"))
      ((equal? type "para") (string-append (org--inline->md (nth e 1)) "\n\n"))
      ((equal? type "src") (string-append "```" (nth e 1) "\n" (nth e 2) "\n```\n\n"))
      ((equal? type "example") (string-append "```\n" (nth e 1) "\n```\n\n"))
      ((equal? type "quote") (string-append "> " (replace-regexp-in-string "\n" "\n> " (org--inline->md (nth e 1))) "\n\n"))
      ((equal? type "hr") "---\n\n")
      ((equal? type "ulist") (string-append (org--list->md e #f) "\n"))
      ((equal? type "olist") (string-append (org--list->md e #t) "\n"))
      ((equal? type "table") (string-append (org--table->md e) "\n"))
      (else ""))))
(define (org--ast->md ast)
  (let ((title (org--doc-title ast)))
    (string-append (if (equal? title "Untitled") "" (string-append "# " title "\n\n"))
                   (org--concat (map org--block->md ast)))))

;; --- LaTeX ------------------------------------------------------------------
(define (org--escape-latex s)
  (let* ((s (replace-regexp-in-string "%" "\\%" s))
         (s (replace-regexp-in-string "&" "\\&" s))
         (s (replace-regexp-in-string "#" "\\#" s))
         (s (replace-regexp-in-string "_" "\\_" s)))
    s))
(define (org--inline->latex s)
  (let* ((s (org--escape-latex s))
         (s (replace-regexp-in-string "\\[\\[([^]]+)\\]\\[([^]]+)\\]\\]" "\\href{$1}{$2}" s))
         (s (replace-regexp-in-string "\\[\\[([^]]+)\\]\\]" "\\url{$1}" s))
         (s (replace-regexp-in-string "\\*([^*\n]+)\\*" "\\textbf{$1}" s))
         (s (replace-regexp-in-string "/([^/\n]+)/" "\\emph{$1}" s))
         (s (replace-regexp-in-string "=([^=\n]+)=" "\\texttt{$1}" s))
         (s (replace-regexp-in-string "~([^~\n]+)~" "\\texttt{$1}" s)))
    s))
(define (org--latex-section-cmd lvl)
  (cond ((= lvl 1) "\\section") ((= lvl 2) "\\subsection") ((= lvl 3) "\\subsubsection")
        ((= lvl 4) "\\paragraph") (else "\\subparagraph")))
(define (org--list->latex e ordered)
  (let ((env (if ordered "enumerate" "itemize")))
    (string-append "\\begin{" env "}\n"
      (org--concat (map (lambda (it) (string-append "\\item " (org--inline->latex (cdr it)) "\n")) (nth e 1)))
      "\\end{" env "}\n\n")))
(define (org--table->latex e)
  (let* ((rows (filter (lambda (r) (not (equal? (car r) "sep"))) (nth e 1)))
         (ncols (let loop ((rs rows) (m 1)) (if (null? rs) m (loop (cdr rs) (max m (length (cdr (car rs)))))))))
    (string-append "\\begin{tabular}{" (org--concat (map (lambda (i) "l") (org--range ncols))) "}\n"
      (org--concat (map (lambda (r)
        (string-append (string-join (map (lambda (i) (org--inline->latex (org--cell (cdr r) i))) (org--range ncols)) " & ")
                       " \\\\\n")) rows))
      "\\end{tabular}\n\n")))
(define (org--block->latex e)
  (let ((type (car e)))
    (cond
      ((equal? type "heading")
       (string-append (org--latex-section-cmd (nth e 1)) "{" (org--inline->latex (nth e 4)) "}\n\n"))
      ((equal? type "para") (string-append (org--inline->latex (nth e 1)) "\n\n"))
      ((equal? type "src") (string-append "\\begin{verbatim}\n" (nth e 2) "\n\\end{verbatim}\n\n"))
      ((equal? type "example") (string-append "\\begin{verbatim}\n" (nth e 1) "\n\\end{verbatim}\n\n"))
      ((equal? type "quote") (string-append "\\begin{quote}\n" (org--inline->latex (nth e 1)) "\n\\end{quote}\n\n"))
      ((equal? type "hr") "\\hrulefill\n\n")
      ((equal? type "ulist") (org--list->latex e #f))
      ((equal? type "olist") (org--list->latex e #t))
      ((equal? type "table") (org--table->latex e))
      (else ""))))
(define (org--ast->latex ast)
  (string-append
   "\\documentclass{article}\n\\usepackage[utf8]{inputenc}\n\\usepackage{hyperref}\n"
   "\\title{" (org--escape-latex (org--doc-title ast)) "}\n\\begin{document}\n\\maketitle\n\n"
   (org--concat (map org--block->latex ast))
   "\\end{document}\n"))

;; --- ASCII ------------------------------------------------------------------
(define (org--inline->ascii s)
  (let* ((s (replace-regexp-in-string "\\[\\[([^]]+)\\]\\[([^]]+)\\]\\]" "$2 ($1)" s))
         (s (replace-regexp-in-string "\\[\\[([^]]+)\\]\\]" "$1" s))
         (s (replace-regexp-in-string "\\*([^*\n]+)\\*" "$1" s))
         (s (replace-regexp-in-string "/([^/\n]+)/" "$1" s))
         (s (replace-regexp-in-string "=([^=\n]+)=" "$1" s))
         (s (replace-regexp-in-string "~([^~\n]+)~" "$1" s))
         (s (replace-regexp-in-string "\\+([^+\n]+)\\+" "$1" s)))
    s))
(define (org--ascii-rule ch n) (if (<= n 0) "" (string-append ch (org--ascii-rule ch (- n 1)))))
(define (org--list->ascii e ordered)
  (let loop ((its (nth e 1)) (i 1) (acc ""))
    (if (null? its) acc
        (let ((it (car its)))
          (loop (cdr its) (+ i 1)
                (string-append acc (if ordered (string-append (number->string i) ". ") "- ")
                               (if (equal? (car it) "") "" (string-append "[" (car it) "] "))
                               (org--inline->ascii (cdr it)) "\n"))))))
(define (org--block->ascii e)
  (let ((type (car e)))
    (cond
      ((equal? type "heading")
       (let* ((title (string-append (if (equal? (nth e 2) "") "" (string-append (nth e 2) " "))
                                    (org--inline->ascii (nth e 4))))
              (ch (cond ((= (nth e 1) 1) "=") ((= (nth e 1) 2) "-") (else "~"))))
         (string-append title "\n" (org--ascii-rule ch (utf8-length title)) "\n\n")))
      ((equal? type "para") (string-append (org--inline->ascii (nth e 1)) "\n\n"))
      ((equal? type "src") (string-append (org--indent-lines (nth e 2) "    ") "\n\n"))
      ((equal? type "example") (string-append (org--indent-lines (nth e 1) "    ") "\n\n"))
      ((equal? type "quote") (string-append (org--indent-lines (org--inline->ascii (nth e 1)) "  ") "\n\n"))
      ((equal? type "hr") "----------------------------------------\n\n")
      ((equal? type "ulist") (string-append (org--list->ascii e #f) "\n"))
      ((equal? type "olist") (string-append (org--list->ascii e #t) "\n"))
      ((equal? type "table") (string-append (join-lines (org--format-table (org--ast-table->rows e))) "\n\n"))
      (else ""))))
(define (org--ast->ascii ast) (org--concat (map org--block->ascii ast)))

;; Export commands (C-c C-e m / l / a).
(define (org-export-markdown)
  (let ((p (org--output-path ".md")))
    (write-file p (org--ast->md (org-parse (buffer-string)))) (message (string-append "Exported to " p))))
(define (org-export-latex)
  (let ((p (org--output-path ".tex")))
    (write-file p (org--ast->latex (org-parse (buffer-string)))) (message (string-append "Exported to " p))))
(define (org-export-ascii)
  (let ((p (org--output-path ".txt")))
    (write-file p (org--ast->ascii (org-parse (buffer-string)))) (message (string-append "Exported to " p))))

;; ===========================================================================
;; Layer 7k — column view (read-only, driven by #+COLUMNS:)
;; ===========================================================================
(define *org-columns-default* "%40ITEM %TODO %PRIORITY %TAGS")
(define (org--columns-spec)
  (let loop ((ls (org--lines)))
    (cond ((null? ls) *org-columns-default*)
          (else (let ((c (regexp-captures "^[ \t]*#\\+COLUMNS:[ \t]*(.*)$" (car ls))))
                  (if (null? c) (loop (cdr ls)) (nth c 1)))))))
;; "%[width]NAME" tokens → list of (width . name).
(define (org--parse-columns spec)
  (let loop ((toks (filter (lambda (x) (not (equal? x ""))) (split-many spec " "))) (acc '()))
    (if (null? toks) (reverse acc)
        (let ((c (regexp-captures "^%([0-9]*)([A-Za-z_]+)" (car toks))))
          (if (null? c) (loop (cdr toks) acc)
              (loop (cdr toks)
                    (cons (cons (if (equal? (nth c 1) "") 0 (string->number (nth c 1))) (nth c 2)) acc)))))))

;; Total clocked minutes in heading H's own section.
(define (org--heading-clocksum H)
  (let ((e (org--entry-content-end H)))
    (let loop ((i (+ H 1)) (sum 0))
      (if (> i e) sum
          (let ((m (org--parse-clock-mins (org--line-text i)))) (loop (+ i 1) (if m (+ sum m) sum)))))))

(define (org--column-value H name)
  (let* ((line (org--line-text H)) (rest (nth (org--heading-split line) 1))
         (kw (org--leading-keyword rest)))
    (cond
      ((equal? name "ITEM") (org--heading-title rest kw))
      ((equal? name "TODO") (or kw ""))
      ((equal? name "PRIORITY") (let ((c (regexp-captures "\\[#([A-Z])\\]" rest))) (if (null? c) "" (nth c 1))))
      ((equal? name "TAGS")
       (let ((c (regexp-captures ":([A-Za-z0-9_@#%:]+):[ \t]*$" rest))) (if (null? c) "" (nth c 1))))
      ((equal? name "CLOCKSUM") (org--fmt-duration (org--heading-clocksum H)))
      (else (or (org-entry-get-inherited H name) "")))))

;; M-x org-columns — tabulate every heading's column values in a side buffer.
(define (org-columns)
  (let* ((names (map cdr (org--parse-columns (org--columns-spec))))
         (header (string-append "| " (string-join names " | ") " |"))
         (rows (map (lambda (h)
                      (let ((H (car h)) (lvl (nth h 1)))
                        (string-append "| "
                          (string-join
                           (map (lambda (nm)
                                  (let ((v (org--column-value H nm)))
                                    (if (equal? nm "ITEM") (string-append (org--spaces (* 2 (- lvl 1))) v) v)))
                                names)
                           " | ")
                          " |")))
                    (org--headings))))
    (render-buffer "*Org Columns*" "org-columns-mode"
                   (join-lines (org--format-table (append (list header "|-|") rows))) '())
    (message "Org columns")))

;; ===========================================================================
;; Layer 7l — babel noweb / named blocks / inline + list-item editing
;; ===========================================================================
;; Body of the src block named NAME (via a #+name: line), or #f.
(define (org--named-block-body name)
  (let ((lines (org--lines)) (n (org--total-lines)))
    (let loop ((i 0))
      (cond ((>= i n) #f)
            ((>= (string-match (string-append "^[ \t]*#\\+name:[ \t]*" (regexp-quote name) "[ \t]*$")
                               (nth lines i)) 0)
             (let skip ((j (+ i 1)))
               (cond ((>= j n) #f)
                     ((org--block-begin? (nth lines j))
                      (let end ((k (+ j 1)) (acc '()))
                        (cond ((>= k n) (join-lines (reverse acc)))
                              ((org--block-end? (nth lines k)) (join-lines (reverse acc)))
                              (else (end (+ k 1) (cons (nth lines k) acc))))))
                     ((equal? (org--trim (nth lines j)) "") (skip (+ j 1)))
                     (else #f))))
            (else (loop (+ i 1)))))))
;; Expand <<name>> noweb references in a block body.
(define (org--expand-noweb body)
  (let loop ((s body) (acc ""))
    (let ((m (string-match "<<([A-Za-z0-9_-]+)>>" s)))
      (if (< m 0) (string-append acc s)
          (let* ((c (regexp-captures "<<([A-Za-z0-9_-]+)>>" s))
                 (whole (car c)) (name (nth c 1))
                 (before (substring s 0 m)) (after (org--byte-drop s (+ m (utf8-length whole)))))
            (loop after (string-append acc before (or (org--named-block-body name) ""))))))))

;; C-c C-c on an inline `src_lang{code}` runs it and appends {{{results(…)}}}.
(define (org-babel-execute-inline)
  (let* ((L (buffer-line)) (line (org--line-text L))
         (m (string-match "src_([A-Za-z0-9]+)(\\[[^]]*\\])?\\{([^}]*)\\}" line)))
    (if (< m 0) (message "No inline src at point")
        (let* ((c (regexp-captures "src_([A-Za-z0-9]+)(\\[[^]]*\\])?\\{([^}]*)\\}" line))
               (whole (car c)) (out (trim-end (org--run-src (nth c 1) (nth c 3))))
               (pos (+ (org--line-start L) m (utf8-length whole))))
          (insert-at pos (string-append " {{{results(" out ")}}}"))
          (message "Inline src executed")))))

;; --- list-item editing ------------------------------------------------------
;; (whole indent bullet) for a plain-list line, or #f.
(define (org--list-line-prefix line)
  (let ((c (regexp-captures "^([ \t]*)([-+*]|[0-9]+[.)])[ \t]+" line)))
    (if (null? c) #f c)))
;; M-RET on a list item — insert a sibling item below (incrementing an ordinal).
(define (org-insert-item)
  (let* ((L (buffer-line)) (line (org--line-text L)) (c (org--list-line-prefix line)))
    (if (not c) #f
        (let* ((indent (nth c 1)) (bullet (nth c 2))
               (n (regexp-captures "^([0-9]+)([.)])$" bullet))
               (newbullet (if (null? n) bullet
                              (string-append (number->string (+ 1 (string->number (nth n 1)))) (nth n 2))))
               (pos (+ (org--line-start L) (utf8-length line))))
          (insert-at pos (string-append "\n" indent newbullet " "))
          (goto-char (+ pos 1 (utf8-length indent) (utf8-length newbullet) 1))
          #t))))
(define (org-indent-item)
  (let* ((L (buffer-line)) (line (org--line-text L)))
    (when (org--list-line-prefix line) (org--replace-line L (string-append "  " line)))))
(define (org-outdent-item)
  (let* ((L (buffer-line)) (line (org--line-text L)))
    (when (and (org--list-line-prefix line) (>= (string-match "^  " line) 0))
      (org--replace-line L (substring line 2 (string-length line))))))
;; M-RET dispatcher: a list item on a list line, else a heading.
(define (org-meta-return)
  (if (org--list-line-prefix (org--line-text (buffer-line))) (org-insert-item) (org-insert-heading)))

;; Activation
;; ===========================================================================
;; Turn the active buffer into an Org buffer (M-x org-mode, or auto for .org).
(define (org-mode)
  (set-major-mode "org-mode")
  (buffer-local-set "org-active" "t")
  (org--fontify)
  (message "Org mode"))

;; .org files: activate on open. (set-major-mode installs the org-mode keymap.)
(define (org--maybe-activate)
  (when (and (ends-with? (current-buffer) ".org")
             (not (equal? (buffer-local-get "org-active") "t")))
    (set-major-mode "org-mode")
    (buffer-local-set "org-active" "t")
    ;; Use Org's outline folding (headlines) for this buffer's chevrons rather
    ;; than the generic indentation folder.
    (buffer-local-set "fold-style" "org")
    (org--fontify)
    ;; Fold chevrons walk every heading (another whole-buffer pass); skip on
    ;; oversized files, same threshold as fontify so neither hangs the engine.
    (when (org--fontify-ok?) (org--refresh-fold-markers))))
(add-hook "find-file-hook" "org--maybe-activate")
(add-hook "after-change-functions" "org--fontify-schedule")

;; ===========================================================================
;; Keymap + M-x
;; ===========================================================================
;; TAB / S-TAB / RET / M-arrows are context-sensitive: in a table they edit it,
;; otherwise they fall back to heading folding / structure editing (like Org).
(define-key "org-mode" "tab" "org-tab")
(define-key "org-mode" "shift-tab" "org-shift-tab")
(define-key "org-mode" "enter" "org-return")
(define-key "org-mode" "ctrl-k" "org-kill-line")
(define-key "org-mode" "ctrl-c ctrl-t" "org-todo")
(define-key "org-mode" "ctrl-c ctrl-c" "org-ctrl-c-ctrl-c")
(define-key "org-mode" "ctrl-c ctrl-q" "org-set-tags")
(define-key "org-mode" "ctrl-c ," "org-set-priority")
(define-key "org-mode" "alt-enter" "org-meta-return")
(define-key "org-mode" "alt-left" "org-meta-left")
(define-key "org-mode" "alt-right" "org-meta-right")
(define-key "org-mode" "alt-up" "org-meta-up")
(define-key "org-mode" "alt-down" "org-meta-down")
;; Table structure: insert/delete column (M-S-←/→) and row (M-S-↓/↑).
(define-key "org-mode" "alt-shift-right" "org-table-insert-column")
(define-key "org-mode" "alt-shift-left" "org-table-delete-column")
(define-key "org-mode" "alt-shift-down" "org-table-insert-row")
(define-key "org-mode" "alt-shift-up" "org-table-delete-row")
;; Export dispatcher prefix C-c C-e: h html, m markdown, l latex, a ascii.
(define-key "org-mode" "ctrl-c ctrl-e h" "org-export-html")
(define-key "org-mode" "ctrl-c ctrl-e m" "org-export-markdown")
(define-key "org-mode" "ctrl-c ctrl-e l" "org-export-latex")
(define-key "org-mode" "ctrl-c ctrl-e a" "org-export-ascii")
(define-key "org-mode" "ctrl-c a" "org-agenda")
(define-key "org-mode" "ctrl-c shift-a" "org-agenda-dispatch")
(define-key "org-mode" "ctrl-c t" "org-todo-list")
(define-key "org-mode" "ctrl-c c" "org-capture")
;; Scheduling / properties / links / footnotes.
(define-key "org-mode" "ctrl-c ctrl-s" "org-schedule")
(define-key "org-mode" "ctrl-c ctrl-d" "org-deadline")
(define-key "org-mode" "ctrl-c ." "org-time-stamp")
(define-key "org-mode" "ctrl-c ctrl-w" "org-refile")
(define-key "org-mode" "ctrl-c \\" "org-tags-view")
(define-key "org-mode" "ctrl-c ctrl-l" "org-insert-link")
(define-key "org-mode" "ctrl-c l" "org-store-link")
;; Clocking / properties / archive / columns / footnote / inline images / latex
;; live under the C-c C-x prefix.
(define-key "org-mode" "ctrl-c ctrl-x ctrl-i" "org-clock-in")
(define-key "org-mode" "ctrl-c ctrl-x ctrl-o" "org-clock-out")
(define-key "org-mode" "ctrl-c ctrl-x ctrl-x" "org-clock-cancel")
(define-key "org-mode" "ctrl-c ctrl-x ctrl-r" "org-clock-report")
(define-key "org-mode" "ctrl-c ctrl-x ctrl-a" "org-archive-subtree")
(define-key "org-mode" "ctrl-c ctrl-x ctrl-c" "org-columns")
(define-key "org-mode" "ctrl-c ctrl-x p" "org-set-property")
(define-key "org-mode" "ctrl-c ctrl-x f" "org-footnote-new")
(define-key "org-mode" "ctrl-c ctrl-x ctrl-v" "org-toggle-inline-images")
(define-key "org-mode" "ctrl-c ctrl-x ctrl-l" "org-toggle-latex-preview")

;; Agenda / capture / store-link are useful from any buffer too.
(define-key "fundamental" "ctrl-c a" "org-agenda")
(define-key "fundamental" "ctrl-c c" "org-capture")
(define-key "fundamental" "ctrl-c l" "org-store-link")

;; *Org Agenda* buffer.
(define-key "org-agenda-mode" "enter" "org-agenda-visit")
(define-key "org-agenda-mode" "g" "org-agenda")
(define-key "org-agenda-mode" "n" "next-line")
(define-key "org-agenda-mode" "p" "previous-line")
(define-key "org-agenda-mode" "q" "kill-buffer")
(set-option "line-numbers:org-agenda-mode" "false")

(register-command "org-mode")
(register-command "org-agenda")
(register-command "org-todo-list")
(register-command "org-capture")
(register-command "org-export-html")
(register-command "org-table-align")
(register-command "org-kill-line")
(register-command "org-table-next-cell")
(register-command "org-table-prev-cell")
(register-command "org-table-next-row")
(register-command "org-table-move-column-right")
(register-command "org-table-move-column-left")
(register-command "org-table-move-row-down")
(register-command "org-table-move-row-up")
(register-command "org-table-insert-column")
(register-command "org-table-delete-column")
(register-command "org-table-insert-row")
(register-command "org-table-delete-row")
(register-command "org-cycle")
(register-command "org-global-cycle")
(register-command "org-insert-heading")
(register-command "org-todo")
(register-command "org-set-tags")
(register-command "org-set-priority")
(register-command "org-promote")
(register-command "org-demote")
(register-command "org-move-subtree-up")
(register-command "org-move-subtree-down")
(register-command "org-babel-execute")
(register-command "org-toggle-inline-images")
(register-command "org-display-inline-images")
(register-command "org-remove-inline-images")
(register-command "org-toggle-latex-preview")
(register-command "org-toggle-leading-stars")
(register-command "org-toggle-indent")
(register-command "org-schedule")
(register-command "org-deadline")
(register-command "org-time-stamp")
(register-command "org-time-stamp-inactive")
(register-command "org-set-property")
(register-command "org-id-get-create")
(register-command "org-clock-in")
(register-command "org-clock-out")
(register-command "org-clock-cancel")
(register-command "org-clock-report")
(register-command "org-refile")
(register-command "org-archive-subtree")
(register-command "org-tags-view")
(register-command "org-agenda-dispatch")
(register-command "org-store-link")
(register-command "org-insert-link")
(register-command "org-footnote-new")
(register-command "org-table-recalculate")
(register-command "org-export-markdown")
(register-command "org-export-latex")
(register-command "org-export-ascii")
(register-command "org-columns")
(register-command "org-babel-execute-inline")
(register-command "org-insert-item")

;; *Org Columns* buffer.
(define-key "org-columns-mode" "q" "kill-buffer")
(set-option "line-numbers:org-columns-mode" "false")

;; *scratch* defaults to Org mode (Emacs `initial-major-mode`). At startup the
;; active buffer is *scratch* (this file evals before welcome.scm), so switch it
;; to org-mode here — the same steps org--maybe-activate runs for .org files,
;; minus the find-file message. The mode sticks to the buffer, so dismissing the
;; splash with `q` lands you in an Org *scratch* buffer.
(when (equal? (current-buffer) "*scratch*")
  (set-major-mode "org-mode")
  (buffer-local-set "org-active" "t")
  (buffer-local-set "fold-style" "org")
  (org--fontify))
