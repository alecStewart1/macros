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

;; Visual-mode change (charwise selection's exact range) and case folding. Case
;; ops work the region text directly (builtins only) rather than via editing.scm.
(define (evil-visual-change) (kill-region) (evil-set-charwise) (evil-insert-state))
(define (evil-visual-fold-case folder)
  (let ((s (region-beginning)) (e (region-end)))
    (when (and (>= s 0) (< s e))
      (replace-region s e (folder (buffer-substring s e)))))
  (evil-normal-state))
(define (evil-visual-downcase) (evil-visual-fold-case string-downcase)) ; u
(define (evil-visual-upcase)   (evil-visual-fold-case string-upcase))   ; U
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
(define-key "evil-normal" "y w" 'evil-yank-word)         ; yw
(define-key "evil-normal" "y $" 'evil-yank-to-eol)       ; y$
(define-key "evil-normal" "p" 'evil-paste-after)        ; p  (line-wise → below)
(define-key "evil-normal" "shift-p" 'evil-paste-before) ; P  (line-wise → above)
(define-key "evil-normal" "u" 'undo)
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
(define-key "evil-visual" "d" 'evil-visual-delete)
(define-key "evil-visual" "x" 'evil-visual-delete)
(define-key "evil-visual" "y" 'evil-visual-yank)
(define-key "evil-visual" "c" 'evil-visual-change)            ; c/s — change selection
(define-key "evil-visual" "s" 'evil-visual-change)
(define-key "evil-visual" "o" 'exchange-point-and-mark)       ; o — swap selection ends
(define-key "evil-visual" "u" 'evil-visual-downcase)          ; u — lowercase selection
(define-key "evil-visual" "shift-u" 'evil-visual-upcase)      ; U — uppercase selection
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
(define (evil-collection-bind-mode mode)
  (let ((km (string-append "evil-normal:" mode)))
    (define-key km "j" 'next-line)
    (define-key km "k" 'previous-line)))

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
(define (evil-collection-bind-readonly-motion mode)
  (let ((km (string-append "evil-normal:" mode)))
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

(for-each evil-collection-bind-readonly-motion
          (list "compile-mode" "grep-mode" "occur-mode"))
