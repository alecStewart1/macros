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

;; The TODO keyword vocabulary. The cycle nothing→TODO→DONE→nothing is fixed
;; (see `org--next-todo`); these only control fontification recognition.
(define *org-todo-keywords*
  (list "TODO" "DONE" "NEXT" "WAITING" "DOING" "CANCELLED" "HOLD"))
(define *org-done-keywords* (list "DONE" "CANCELLED"))

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
    (if (and (not (null? caps)) (member (nth caps 1) *org-todo-keywords*))
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
    ;; TODO / DONE keyword (stars + one space are ASCII, so byte = char here).
    (let* ((rest (substring line (min (+ level 1) (string-length line)) (string-length line)))
           (kw (org--leading-keyword rest)))
      (when kw
        (let ((ks (+ lstart level 1)))
          (overlay-set-face ks (+ ks (string-length kw))
                            (if (org--done-keyword? kw) "org-done" "org-todo")))))
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
  ;; links [[target]] / [[target][desc]]
  (org--paint lstart line "\\[\\[[^]\n]*\\](\\[[^]\n]*\\])?\\]" "org-link")
  ;; active <…> and inactive [...] timestamps
  (org--paint lstart line "[<\\[][0-9]{4}-[0-9]{2}-[0-9]{2}[^]>\n]*[>\\]]" "org-date"))

;; Repaint the whole buffer's faces. One pass; carries a flag for src-block
;; interiors (which are painted as a literal block, not parsed for emphasis).
(define (org--fontify)
  (when (org--active?)
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
  (when (org--active?)
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

(define (org--next-todo kw)
  (cond ((not kw) "TODO")
        ((equal? kw "TODO") "DONE")
        (else "")))

;; C-c C-t — cycle the TODO state of the headline at point.
(define (org-todo)
  (let ((H (org--heading-line-at-point)))
    (if H
        (let* ((line (org--line-text H))
               (sr (org--heading-split line))
               (stars (nth sr 0)) (rest (nth sr 1))
               (kw (org--leading-keyword rest))
               (title (org--heading-title rest kw))
               (tags (let ((c (regexp-captures "(:[A-Za-z0-9_@#%:]+:)[ \t]*$" rest)))
                       (if (null? c) "" (nth c 1))))
               (prio (let ((c (regexp-captures "\\[#[A-Z]\\]" rest))) (if (null? c) "" (car c))))
               (next (org--next-todo kw)))
          (org--replace-line H
            (string-append stars " "
              (if (equal? next "") "" (string-append next " "))
              (if (equal? prio "") "" (string-append prio " "))
              title
              (if (equal? tags "") "" (string-append " " tags)))))
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
         (s (replace-regexp-in-string "~([^~\n]+)~" "<code>$1</code>" s)))
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

;; Scan one file → entries (path line date kind todo title) for headlines that
;; carry a TODO keyword and/or a planning date.
(define (org--scan-file path)
  (let loop ((ls (split-many (read-file path) "\n")) (i 0) (acc '()))
    (if (null? ls)
        (reverse acc)
        (let ((line (car ls)))
          (if (org--heading-level line)
              (let* ((sr (org--heading-split line))
                     (rest (nth sr 1))
                     (kw (org--leading-keyword rest))
                     (title (org--heading-title rest kw))
                     (plan (if (null? (cdr ls)) #f (org--planning-info (nth ls 1)))))
                (if (or kw plan)
                    (loop (cdr ls) (+ i 1)
                          (cons (list path i
                                      (if plan (car plan) "")
                                      (if plan (cdr plan) "")
                                      (or kw "") title)
                                acc))
                    (loop (cdr ls) (+ i 1) acc)))
              (loop (cdr ls) (+ i 1) acc))))))

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
               (text (string-append
                       (if (equal? date "") "             " (string-append date "  "))
                       (if (equal? kind "") "" (string-append (substring kind 0 (min 4 (string-length kind))) ": "))
                       (if (equal? todo "") "" (string-append todo " "))
                       title "   " (basename (nth e 0)))))
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
(define (org-capture) (read-string "Capture (TODO headline): " "org--capture-finish"))
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
  (cond ((org--in-table?) (org-table-move-column-right)) ((org--on-heading?) (org-demote)) (else #f)))
(define (org-meta-left)
  (cond ((org--in-table?) (org-table-move-column-left)) ((org--on-heading?) (org-promote)) (else #f)))
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
  (let ((b (org--src-block-bounds (buffer-line))))
    (if (not b)
        (message "Not in a src block")
        (let* ((el (nth b 1)) (lang (nth b 2))
               (lines (org--lines)) (n (length lines))
               (body (join-lines (org--slice lines (+ (nth b 0) 1) (- el 1))))
               (out (org--run-src lang body))
               (res-lines (cons "#+RESULTS:" (map (lambda (l) (string-append ": " l)) (split-many out "\n"))))
               (ex (org--existing-results lines (+ el 1) n))
               (rest-start (if ex (+ (nth ex 1) 1) (+ el 1)))
               (before (org--slice lines 0 el))
               (rest (org--slice lines rest-start (- n 1))))
          (org--replace-all (join-lines (append before res-lines (list "") rest)))
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
          ((org--table-line? line) (org-table-align))
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
    (org--refresh-fold-markers)))
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
(define-key "org-mode" "alt-enter" "org-insert-heading")
(define-key "org-mode" "alt-left" "org-meta-left")
(define-key "org-mode" "alt-right" "org-meta-right")
(define-key "org-mode" "alt-up" "org-meta-up")
(define-key "org-mode" "alt-down" "org-meta-down")
;; Table structure: insert/delete column (M-S-←/→) and row (M-S-↓/↑).
(define-key "org-mode" "alt-shift-right" "org-table-insert-column")
(define-key "org-mode" "alt-shift-left" "org-table-delete-column")
(define-key "org-mode" "alt-shift-down" "org-table-insert-row")
(define-key "org-mode" "alt-shift-up" "org-table-delete-row")
(define-key "org-mode" "ctrl-c ctrl-e" "org-export-html")
(define-key "org-mode" "ctrl-c a" "org-agenda")
(define-key "org-mode" "ctrl-c t" "org-todo-list")
(define-key "org-mode" "ctrl-c c" "org-capture")
(define-key "org-mode" "ctrl-c ctrl-x ctrl-v" "org-toggle-inline-images")
(define-key "org-mode" "ctrl-c ctrl-x ctrl-l" "org-toggle-latex-preview")

;; Agenda / capture are useful from any buffer too.
(define-key "fundamental" "ctrl-c a" "org-agenda")
(define-key "fundamental" "ctrl-c c" "org-capture")

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
