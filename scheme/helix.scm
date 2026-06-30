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

;; --- small list / string helpers (self-contained) --------------------------
;; Insertion sort (Steel has no guaranteed `sort`); stable enough for the small
;; selection lists we sort by position.
(define (helix-insert-sorted x lst less?)
  (cond ((null? lst) (list x))
        ((less? x (car lst)) (cons x lst))
        (else (cons (car lst) (helix-insert-sorted x (cdr lst) less?)))))
(define (helix-sort lst less?)
  (if (null? lst) '()
      (helix-insert-sorted (car lst) (helix-sort (cdr lst) less?) less?)))

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

;; --- goto (g …) ------------------------------------------------------------
(define (helix-goto-top)    (helix-set! (list (cons 0 0))))
(define (helix-goto-bottom)
  (let ((mx (point-max))) (helix-set! (list (cons mx mx)))))

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
(define-key "helix-normal" "g g" 'helix-goto-top)     ; gg — top of file
(define-key "helix-normal" "g e" 'helix-goto-bottom)  ; ge — bottom of file
(define-key "helix-normal" "g h" 'helix-goto-line-start)
(define-key "helix-normal" "g l" 'helix-goto-line-end)
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
;; enter insert
(define-key "helix-normal" "i" 'helix-insert-before)
(define-key "helix-normal" "a" 'helix-append-after)
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
;; mode's own overlay). n/p keep working from the base keymap.
(define (helix-collection-bind-mode mode)
  (let ((km (string-append "helix-normal:" mode)))
    (define-key km "j" 'next-line)
    (define-key km "k" 'previous-line)))
(for-each helix-collection-bind-mode *evil-collection-modes*)

;; Plain-text result buffers (compile / grep / occur) are read-only text you
;; scroll like a file, so they get the full read-only motion set (matches
;; evil-collection-bind-readonly-motion). `g g` is left off so single `g` stays
;; free for each mode's own key; jump to the top with C-u or k.
(define (helix-collection-bind-readonly-motion mode)
  (let ((km (string-append "helix-normal:" mode)))
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
(for-each helix-collection-bind-readonly-motion
          (list "compile-mode" "grep-mode" "occur-mode"))

;; helix-mode / helix-disable flip the modal state machine in the core; expose
;; them to M-x so they're discoverable like evil-mode. Opt in from your config
;; with (helix-mode) in ~/.config/macros/init.scm.
(register-command 'helix-mode)
(register-command 'helix-disable)
