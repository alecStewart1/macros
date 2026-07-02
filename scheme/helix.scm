;; helix.scm — Helix-style selection-first modal editing, built on Macros'
;; keymaps and the multiple-selection core. Like evil.scm, the Rust side only
;; decides *when* plain keys are commands (Normal) vs. text (Insert); every
;; binding below is Scheme and fully overridable. Opt in with (helix-mode) in
;; your config (and (helix-disable) to turn it back off). Loaded after core.scm,
;; so (string-join) and friends exist.
;;
;; The Helix model, the part that makes it Helix: editing is SELECTION-FIRST. A
;; motion *selects* (noun), then a verb acts on the selection — `w` selects the
;; word, `d` deletes it. And there can be MANY selections at once: `C` adds one
;; on the next line, and every motion/verb applies to all of them. The renderer
;; paints each one (the primary in `region`, the rest in `secondary-region`).
;;
;; How it works under the hood: `(selections)` reads every selection from the
;; core as a flat list of anchor/head byte offsets; a Helix command maps each to
;; a new (anchor . head) and hands the whole set back with `(set-selections …)`.
;; The pure position bridges (grapheme-forward, word-forward-start, line-of-pos,
;; …) compute a fresh head per selection without disturbing the others. Reads
;; come from a pre-command snapshot, so a command computes every new position up
;; front, then queues one `set-selections`.

;; Sort the small selection lists by position (core's keyed `sort-by`).
(define (helix-sort lst less?) (sort-by (lambda (x) x) less? lst))

;; --- selection plumbing ----------------------------------------------------
;; `(selections)` → flat (a0 h0 a1 h1 …). Chunk it into ((a0 . h0) (a1 . h1) …).
(define (helix-chunk-pairs lst)
  (if (or (null? lst) (null? (cdr lst)))
      '()
      (cons (cons (car lst) (cadr lst)) (helix-chunk-pairs (cddr lst)))))

;; All selections as (anchor . head) pairs, primary first.
(define (helix-selections)
  (helix-chunk-pairs (selections)))

;; Flatten ((a . h) …) back to (a h …) and install as the new selection set.
(define (helix-flatten pairs)
  (if (null? pairs)
      '()
      (cons (caar pairs) (cons (cdar pairs) (helix-flatten (cdr pairs))))))
(define (helix-set! pairs)
  (when (not (null? pairs))
    (set-selections (helix-flatten pairs))))

;; --- select mode (extend) --------------------------------------------------
;; `v` toggles "extend": motions keep each selection's anchor and move only its
;; head, growing the selection instead of replacing it (Helix's select mode).
(define *helix-extend* #f)
(define (helix-toggle-extend)
  (set! *helix-extend* (not *helix-extend*))
  (message (if *helix-extend* "-- SELECT (extend) --" "")))

;; The generic motion driver. `head->new` maps a selection's current head to its
;; new head. `select?` #t means a non-extend move still leaves a visible
;; selection from the old head to the new (word/object motions); #f collapses to
;; a bare cursor at the new head (plain h/j/k/l moves).
(define (helix-move head->new select?)
  (helix-set!
    (map (lambda (s)
           (let* ((a (car s)) (h (cdr s)) (nh (head->new h)))
             (cond (*helix-extend* (cons a nh))
                   (select?        (cons h nh))
                   (else           (cons nh nh)))))
         (helix-selections))))

;; --- character / line motions ----------------------------------------------
(define (helix-char-left)  (helix-move grapheme-backward #f))
(define (helix-char-right) (helix-move grapheme-forward  #f))

;; Vertical motion preserves the byte column on each selection's head.
(define (helix-line-move dir)
  (helix-move
    (lambda (c)
      (let* ((ln  (line-of-pos c))
             (col (- c (line-start-pos c)))
             (tgt (+ ln dir)))
        (if (< tgt 0)
            c
            (let* ((ts (line-start-of-line tgt))
                   (te (line-end-pos ts)))
              (min (+ ts col) te)))))
    #f))
(define (helix-line-down) (helix-line-move 1))
(define (helix-line-up)   (helix-line-move -1))

(define (helix-goto-line-start) (helix-move line-start-pos #f))
(define (helix-goto-line-end)   (helix-move line-end-pos   #f))

;; --- word motions (these select) -------------------------------------------
(define (helix-word-next) (helix-move word-forward-start #t)) ; w
(define (helix-word-end)  (helix-move word-forward       #t)) ; e
(define (helix-word-prev) (helix-move word-backward      #t)) ; b

;; --- line selection (x) ----------------------------------------------------
;; Extend each selection out to whole lines (incl. the trailing newline). Repeat
;; to grow downward — Helix's `x`.
(define (helix-select-line)
  (helix-set!
    (map (lambda (s)
           (let* ((a (car s)) (h (cdr s))
                  (lo (min a h)) (hi (max a h))
                  (ls (line-start-pos lo))
                  (le (grapheme-forward (line-end-pos hi))))
             (cons ls le)))
         (helix-selections))))

;; --- collapsing / pruning selections ---------------------------------------
;; The effective edit range of a selection: itself, or — when empty (a bare
;; cursor) — the single grapheme under the head, so `d`/`c`/`y` act like Helix
;; even right after a plain move.
(define (helix-range s)
  (let ((a (car s)) (h (cdr s)))
    (if (= a h)
        (cons a (grapheme-forward a))
        (cons (min a h) (max a h)))))

;; `;` — collapse every selection to a bare cursor at its head.
(define (helix-collapse)
  (helix-set! (map (lambda (s) (let ((h (cdr s))) (cons h h))) (helix-selections))))

;; `,` — drop all but the primary selection.
(define (helix-keep-primary)
  (let ((sels (helix-selections)))
    (when (not (null? sels)) (helix-set! (list (car sels))))))

;; Escape — leave extend, collapse to a single bare cursor at the primary head.
(define (helix-escape)
  (set! *helix-extend* #f)
  (let ((sels (helix-selections)))
    (when (not (null? sels))
      (let ((h (cdr (car sels)))) (helix-set! (list (cons h h)))))))

;; --- yank / delete / change ------------------------------------------------
;; Selection ranges in document order, deduped trivially by sort.
(define (helix-ranges-asc)
  (helix-sort (map helix-range (helix-selections))
              (lambda (x y) (< (car x) (car y)))))

;; Copy every selection (joined by newlines) to the clipboard, silently. `p`
;; then pastes it (evil-paste-after syncs the clipboard into the kill ring).
(define (helix-copy-ranges ranges)
  (clipboard-set
    (string-join (map (lambda (r) (buffer-substring (car r) (cdr r))) ranges) "\n")))

;; `y` — copy every selection and report how many.
(define (helix-yank)
  (let ((ranges (helix-ranges-asc)))
    (helix-copy-ranges ranges)
    (message (string-append (number->string (length ranges)) " selection(s) yanked"))))

;; Cursor positions left after deleting `asc` (ascending, non-overlapping): each
;; range start, shifted left by the total length deleted before it.
(define (helix-cursors-after-delete asc)
  (let loop ((rs asc) (dropped 0) (acc '()))
    (if (null? rs)
        (reverse acc)
        (let* ((r (car rs)) (s (car r)) (e (cdr r)) (ns (- s dropped)))
          (loop (cdr rs) (+ dropped (- e s)) (cons (cons ns ns) acc))))))

;; Delete every selection (yanking first), then leave one bare cursor per
;; deletion. Deletes run high-offset-first so earlier edits don't shift later
;; ranges; the resulting cursors are computed up front from the snapshot.
(define (helix-delete-core)
  (let* ((asc  (helix-ranges-asc))
         (desc (helix-sort asc (lambda (x y) (> (car x) (car y))))))
    (when (not (null? asc))
      (helix-copy-ranges asc)
      (for-each (lambda (r) (delete-region (car r) (cdr r))) desc)
      (helix-set! (helix-cursors-after-delete asc)))))

(define (helix-delete) (helix-delete-core))       ; d
(define (helix-change) (helix-delete-core) (evil-insert-state)) ; c

;; --- entering insert -------------------------------------------------------
;; Collapse each selection to a bare cursor at its start (`i`) or end (`a`), then
;; switch to Insert. Typed text replicates to every cursor (multiple-cursors).
(define (helix-insert-before)
  (helix-set! (map (lambda (s) (let ((m (min (car s) (cdr s)))) (cons m m))) (helix-selections)))
  (evil-insert-state))
(define (helix-append-after)
  (helix-set! (map (lambda (s) (let ((m (max (car s) (cdr s)))) (cons m m))) (helix-selections)))
  (evil-insert-state))

;; `o` / `O` — open a line below / above (single cursor) and insert.
(define (helix-open-below)
  (helix-keep-primary) (move-end-of-line) (newline) (evil-insert-state))
(define (helix-open-above)
  (helix-keep-primary) (move-beginning-of-line) (open-line) (evil-insert-state))

;; `I` / `A` — collapse each selection to its line's first non-blank / end,
;; then insert; typed text lands on every line at once (multiple-cursors).
(define (helix-insert-line-start)
  (helix-set!
   (map (lambda (s)
          (let* ((h (cdr s))
                 (fnb (evil-skip-blanks-to (line-start-pos h) (line-end-pos h))))
            (cons fnb fnb)))
        (helix-selections)))
  (evil-insert-state))
(define (helix-append-line-end)
  (helix-set!
   (map (lambda (s) (let ((le (line-end-pos (cdr s)))) (cons le le)))
        (helix-selections)))
  (evil-insert-state))

;; --- whole-buffer / case / anchor operations ---------------------------------
;; `%` — one selection spanning the whole buffer (Helix select-all).
(define (helix-select-all) (helix-set! (list (cons 0 (point-max)))))

;; `~` / `` ` `` / `M-` ` — case-fold every selection in place (a bare cursor
;; folds the single grapheme under it). Edits run high→low; the selections are
;; left as they were (case folding is length-preserving for ASCII).
(define (helix-fold-ranges folder)
  (let ((desc (helix-sort (helix-ranges-asc) (lambda (x y) (> (car x) (car y))))))
    (for-each (lambda (r)
                (replace-region (car r) (cdr r)
                                (folder (buffer-substring (car r) (cdr r)))))
              desc)))
(define (helix-switch-case)  (helix-fold-ranges string-toggle-case))
(define (helix-to-lowercase) (helix-fold-ranges string-downcase))
(define (helix-to-uppercase) (helix-fold-ranges string-upcase))

;; `M-;` — flip each selection's anchor and head (Helix flip_selections).
(define (helix-flip-selections)
  (helix-set! (map (lambda (s) (cons (cdr s) (car s))) (helix-selections))))

;; --- f / t / F / T char find --------------------------------------------------
;; Selection-first flavor of the evil char find (evil.scm owns the line-local
;; scanner and the `read-char` plumbing): every head moves to the target on
;; its own line, selecting from the old head like the word motions; heads with
;; no match stay put.
(define *helix-find-till* #f)
(define *helix-find-forward* #t)
(define (helix-find-dispatch ch)
  (helix-move
   (lambda (h) (or (evil-find-target-at h ch *helix-find-till* *helix-find-forward*) h))
   #t))
(define (helix-find-arm till forward prompt)
  (set! *helix-find-till* till)
  (set! *helix-find-forward* forward)
  (read-char prompt 'helix-find-dispatch))
(define (helix-find-char-forward)  (helix-find-arm #f #t "f-"))
(define (helix-find-char-backward) (helix-find-arm #f #f "F-"))
(define (helix-till-char-forward)  (helix-find-arm #t #t "t-"))
(define (helix-till-char-backward) (helix-find-arm #t #f "T-"))

;; gs — first non-blank of each head's line (Helix goto_first_nonwhitespace).
(define (helix-goto-first-nonblank)
  (helix-move
   (lambda (h) (evil-skip-blanks-to (line-start-pos h) (line-end-pos h)))
   #f))

;; --- goto (g …) ------------------------------------------------------------
(define (helix-goto-top)    (helix-set! (list (cons 0 0))))
(define (helix-goto-bottom)
  (let ((mx (point-max))) (helix-set! (list (cons mx mx)))))

;; --- whole-line block edits (gq reflow, J join) --------------------------------
;; Shared bookkeeping: expand each selection to whole lines (a bare cursor =
;; its current line), merge overlapping ranges, apply `transform` to each
;; block's text with edits running high→low (earlier offsets stay valid), and
;; leave a bare cursor at each block's (shifted) start — helix-delete-core's
;; pattern, except the shift is each block's new-minus-old BYTE length.
;; `extend-single` grows a one-line block to include the next line (for J).
(define (helix-transform-line-blocks transform extend-single)
  (let* ((mx (point-max))
         (asc (helix-sort
               (map (lambda (s)
                      (let* ((lo (min (car s) (cdr s)))
                             (hi (max (car s) (cdr s)))
                             (ls (line-start-pos lo))
                             (le (line-end-pos (max lo (- hi 1)))))
                        (if (and extend-single
                                 (= le (line-end-pos ls))     ; a single line…
                                 (< (+ le 1) mx))             ; …with one below
                            (cons ls (line-end-pos (+ le 1)))
                            (cons ls le))))
                    (helix-selections))
               (lambda (x y) (< (car x) (car y)))))
         (merged (let loop ((rs asc) (acc '()))
                   (cond ((null? rs) (reverse acc))
                         ((and (not (null? acc)) (<= (car (car rs)) (cdr (car acc))))
                          (loop (cdr rs)
                                (cons (cons (car (car acc))
                                            (max (cdr (car acc)) (cdr (car rs))))
                                      (cdr acc))))
                         (else (loop (cdr rs) (cons (car rs) acc))))))
         ;; ((range . new-text) …) in ascending order.
         (blocks (map (lambda (r)
                        (cons r (transform (buffer-substring (car r) (cdr r)))))
                      merged)))
    (when (not (null? blocks))
      (for-each (lambda (b)
                  (let ((r (car b))) (replace-region (car r) (cdr r) (cdr b))))
                (reverse blocks))
      (helix-set!
       (let loop ((bs blocks) (delta 0) (acc '()))
         (if (null? bs)
             (reverse acc)
             (let* ((r (car (car bs))) (new (cdr (car bs)))
                    (s (+ (car r) delta)))
               (loop (cdr bs)
                     (+ delta (- (utf8-length new) (- (cdr r) (car r))))
                     (cons (cons s s) acc)))))))))

;; gq — Helix's `:reflow` on a key (`:` here is the vim ex bridge, which has no
;; reflow): re-wrap each selection's lines at *fill-column* (fill-text,
;; core.scm).
(define (helix-reflow)
  (helix-transform-line-blocks (lambda (text) (fill-text text *fill-column*)) #f))

;; J — join the selected lines (a bare cursor joins the next line up),
;; collapsing each newline + following indentation to one space.
(define (helix-join-lines)
  (helix-transform-line-blocks
   (lambda (text) (replace-regexp-in-string "\n[ \t]*" " " text))
   #t))

;; =====================================================================
;; Keymap — Normal (selection-first). Plain keys are commands here.
;; =====================================================================
;; motion
(define-key "helix-normal" "h" 'helix-char-left)
(define-key "helix-normal" "l" 'helix-char-right)
(define-key "helix-normal" "j" 'helix-line-down)
(define-key "helix-normal" "k" 'helix-line-up)
(define-key "helix-normal" "w" 'helix-word-next)
(define-key "helix-normal" "e" 'helix-word-end)
(define-key "helix-normal" "b" 'helix-word-prev)
(define-key "helix-normal" "0" 'helix-goto-line-start)
(define-key "helix-normal" "$" 'helix-goto-line-end)
(define-key "helix-normal" "x" 'helix-select-line)
(define-key "helix-normal" "shift-x" 'helix-select-line)   ; X — extend to line bounds
(define-key "helix-normal" "%" 'helix-select-all)          ; % — select whole buffer
(define-key "helix-normal" "f" 'helix-find-char-forward)   ; f/t/F/T — char find (selects)
(define-key "helix-normal" "shift-f" 'helix-find-char-backward)
(define-key "helix-normal" "t" 'helix-till-char-forward)
(define-key "helix-normal" "shift-t" 'helix-till-char-backward)
(define-key "helix-normal" "g g" 'helix-goto-top)     ; gg — top of file
(define-key "helix-normal" "g e" 'helix-goto-bottom)  ; ge — bottom of file
(define-key "helix-normal" "g h" 'helix-goto-line-start)
(define-key "helix-normal" "g l" 'helix-goto-line-end)
(define-key "helix-normal" "g s" 'helix-goto-first-nonblank) ; gs — first non-blank
(define-key "helix-normal" "g w" 'avy-goto-char)      ; gw — jump by char (avy)
(define-key "helix-normal" "g q" 'helix-reflow)       ; gq — reflow selected lines (:reflow)
;; selection management
(define-key "helix-normal" "v" 'helix-toggle-extend)  ; toggle select/extend
(define-key "helix-normal" ";" 'helix-collapse)       ; collapse to cursors
(define-key "helix-normal" "," 'helix-keep-primary)   ; keep only primary
(define-key "helix-normal" "shift-c" 'mc-add-cursor-below) ; C — cursor below
(define-key "helix-normal" "escape" 'helix-escape)
;; changes (act on the selection — noun→verb)
(define-key "helix-normal" "d" 'helix-delete)
(define-key "helix-normal" "c" 'helix-change)
(define-key "helix-normal" "y" 'helix-yank)
(define-key "helix-normal" "p" 'evil-paste-after)
(define-key "helix-normal" "shift-p" 'evil-paste-before)
(define-key "helix-normal" "r" 'evil-replace-char)    ; r<char> — replace
(define-key "helix-normal" "u" 'undo)
(define-key "helix-normal" "shift-u" 'redo)           ; U — redo
(define-key "helix-normal" "shift-j" 'helix-join-lines)   ; J — join selected lines
(define-key "helix-normal" "~" 'helix-switch-case)        ; ~ — toggle case
(define-key "helix-normal" "`" 'helix-to-lowercase)       ; ` — lowercase
(define-key "helix-normal" "alt-`" 'helix-to-uppercase)   ; M-` — uppercase
(define-key "helix-normal" "alt-;" 'helix-flip-selections); M-; — flip anchor/head
;; enter insert
(define-key "helix-normal" "i" 'helix-insert-before)
(define-key "helix-normal" "a" 'helix-append-after)
(define-key "helix-normal" "shift-i" 'helix-insert-line-start) ; I — insert at line start
(define-key "helix-normal" "shift-a" 'helix-append-line-end)   ; A — append at line end
(define-key "helix-normal" "o" 'helix-open-below)
(define-key "helix-normal" "shift-o" 'helix-open-above)
;; search & ex
(define-key "helix-normal" "/" 'evil-search-forward)
(define-key "helix-normal" "n" 'evil-search-next)
(define-key "helix-normal" "shift-n" 'evil-search-previous)
(define-key "helix-normal" ":" 'evil-ex-command)
;; scrolling (shared with evil)
(define-key "helix-normal" "ctrl-d" 'evil-scroll-down)
(define-key "helix-normal" "ctrl-u" 'evil-scroll-up)

;; =====================================================================
;; Keymap — Insert. Plain keys self-insert; Escape returns to Normal,
;; KEEPING any extra cursors (so multi-selection `c`/`i`/`a` survive).
;; =====================================================================
(define-key "helix-insert" "escape" 'helix-normal-state)

;; =====================================================================
;; helix-collection — vim/helix keys on read-only special buffers
;; =====================================================================
;; Mirrors evil-collection (evil.scm): the editor consults a `helix-normal:<major>`
;; overlay BEFORE a special buffer's own keymap, but only under helix-mode (see
;; `modal_collection_prefix` in the core). So whatever we bind here shadows a
;; default key under Helix and is inert otherwise. Reuses evil.scm's mode list
;; and motion helpers (evil.scm loads first); per-buffer specifics (e.g. magit's
;; `x`=discard) live next to each mode's keymap, like the evil overlay does.

;; Vim motion shared by every special buffer: j/k by default (single `g` is
;; refresh in several modes, so a `g g` here would shadow it — left to each
;; mode's own overlay). n/p keep working from the base keymap. The binding set
;; itself is evil.scm's, parameterized over the overlay prefix.
(define (helix-collection-bind-mode mode)
  (modal-collection-bind-mode "helix-normal:" mode))
(for-each helix-collection-bind-mode *evil-collection-modes*)

;; Plain-text result buffers (compile / grep / occur) are read-only text you
;; scroll like a file, so they get the full read-only motion set — the same
;; evil.scm binding set, on the helix overlay prefix. `g g` is left off so
;; single `g` stays free for each mode's own key; jump to the top with C-u or k.
(define (helix-collection-bind-readonly-motion mode)
  (modal-collection-bind-readonly-motion "helix-normal:" mode))
(for-each helix-collection-bind-readonly-motion
          (list "compile-mode" "grep-mode" "occur-mode"))

;; helix-mode / helix-disable flip the modal state machine in the core; expose
;; them to M-x so they're discoverable like evil-mode. Opt in from your config
;; with (helix-mode) in ~/.config/macros/init.scm.
(register-command 'helix-mode)
(register-command 'helix-disable)
