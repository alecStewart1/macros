;; Core Lisp-style helpers built on the new read primitives (point, goto-char,
;; buffer-string, …). Loaded early so later libs (magit, projectile) can use them.

;; save-excursion: run body, then restore point. Mirrors Emacs's special form.
;; `goto-char` is queued as a command, so point is restored after body's edits.
;; CAVEAT (snapshot model): reads see a pre-command snapshot and edits are
;; queued, so the saved point is the pre-body offset — if the body inserts or
;; deletes text BEFORE that offset, the restored point lands off by the edited
;; delta. Fine for read-only bodies; be wary around mutating ones.
(define-syntax save-excursion
  (syntax-rules ()
    ((_ body ...)
     (let ((saved-point (point)))
       (let ((result (begin body ...)))
         (goto-char saved-point)
         result)))))

;; Function form for callers that prefer passing a thunk.
(define (with-saved-excursion thunk)
  (let ((saved (point)))
    (let ((result (thunk)))
      (goto-char saved)
      result)))

;; --- small list/path utilities ---------------------------------------------

(define (last lst)
  (if (null? (cdr lst)) (car lst) (last (cdr lst))))

;; 0-based list indexing (Steel may lack list-ref; this is always available).
(define (nth lst i)
  (if (= i 0) (car lst) (nth (cdr lst) (- i 1))))

;; --- UTF-8 byte arithmetic ---------------------------------------------------
;; The editor's overlay/byte APIs (and string-match / regexp-captures offsets)
;; are BYTE-based, while substring / string-length are CHAR-indexed. These
;; helpers convert safely; never mix a byte offset into substring directly.

;; UTF-8 byte width of one character.
(define (char-bytes ch)
  (let ((cp (char->integer ch)))
    (cond ((< cp 128) 1) ((< cp 2048) 2) ((< cp 65536) 3) (else 4))))

;; UTF-8 byte length of a string — the byte-based twin of `string-length`.
(define (utf8-length s)
  (let loop ((chars (string->list s)) (n 0))
    (if (null? chars) n (loop (cdr chars) (+ n (char-bytes (car chars)))))))

;; Drop the first `n` BYTES of `s`, returning the remainder. Lets a scanner
;; advance past a match it located by byte offset (string-match).
(define (byte-drop s n)
  (if (<= n 0)
      s
      (let loop ((chars (string->list s)) (acc 0))
        (cond ((null? chars) "")
              ((>= acc n) (list->string chars))
              (else (loop (cdr chars) (+ acc (char-bytes (car chars)))))))))

;; The first `n` BYTES of `s` — the head-side twin of `byte-drop`, for splitting
;; a string at a byte offset returned by `string-match`.
(define (byte-take s n)
  (if (<= n 0)
      ""
      (let loop ((chars (string->list s)) (acc 0) (out '()))
        (cond ((null? chars) s)
              ((>= acc n) (list->string (reverse out)))
              (else (loop (cdr chars)
                          (+ acc (char-bytes (car chars)))
                          (cons (car chars) out)))))))

;; Byte offset of the start of line `i` in a `lines` list (each line + newline).
(define (lines-byte-offset lines i)
  (let loop ((k 0) (acc 0))
    (if (>= k i) acc (loop (+ k 1) (+ acc (utf8-length (nth lines k)) 1)))))

(define (alist-ref alist key)
  (cond ((null? alist) #f)
        ((equal? (car (car alist)) key) (cdr (car alist)))
        (else (alist-ref (cdr alist) key))))

(define (non-empty-lines s)
  (filter (lambda (l) (not (equal? l ""))) (split-many s "\n")))

;; Join a list of strings with `sep`.
(define (string-join lst sep)
  (cond ((null? lst) "")
        ((null? (cdr lst)) (car lst))
        (else (string-append (car lst) sep (string-join (cdr lst) sep)))))

;; Join a list of strings with newlines (inverse of split-many on "\n").
(define (join-lines lines) (string-join lines "\n"))

;; Concatenate a list of strings.
(define (string-concat lst)
  (if (null? lst) "" (string-append (car lst) (string-concat (cdr lst)))))

;; `s` repeated `n` times ("ab" 3 → "ababab"; n <= 0 → "").
(define (string-repeat s n)
  (if (<= n 0) "" (string-append s (string-repeat s (- n 1)))))

;; Pad `s` with trailing spaces to at least `width` CHARS (left-aligned; an
;; already-wide string is returned unchanged, never truncated).
(define (pad-right s width)
  (let ((len (string-length s)))
    (if (>= len width) s (string-append s (string-repeat " " (- width len))))))

;; Trim leading and trailing spaces/tabs.
(define (string-trim s)
  (replace-regexp-in-string "^[ \t]+" ""
    (replace-regexp-in-string "[ \t]+$" "" s)))

;; Toggle the case of every cased character in `s` (vim `~`); uncased chars
;; pass through. Char-indexed — safe on multibyte text.
(define (string-toggle-case s)
  (string-concat
   (map (lambda (ch)
          (let* ((c (list->string (list ch)))
                 (up (string-upcase c))
                 (dn (string-downcase c)))
            (if (equal? c up) dn up)))
        (string->list s))))

;; --- fill / word wrap ---------------------------------------------------------
;; Column the fill commands wrap at (Emacs `fill-column`, Vim `textwidth`).
;; Shared by fill-paragraph (M-q, editing.scm), evil's gq family (evil.scm) and
;; helix's gq reflow (helix.scm). Change it in init.scm with
;; (set! *fill-column* 100) or interactively with M-x set-fill-column.
(define *fill-column* 80)

;; Comment leaders that wrapping continues onto every new line (Vim keeps the
;; comment leader when formatting; Emacs calls this the fill prefix). Longest
;; first so ";;" is matched before ";".
(define *fill-leaders* (list ";;;" ";;" ";" "//" "#" "--" ">"))

;; The comment leader `rest` (a line minus its indentation) starts with, or #f.
;; The leader must be followed by whitespace or end the line — "# heading" has
;; a leader, "#hashtag" is just a word.
(define (fill-leader-of rest)
  (let loop ((ls *fill-leaders*))
    (cond ((null? ls) #f)
          ((and (starts-with? rest (car ls))
                (let ((r (substring rest (string-length (car ls)) (string-length rest))))
                  (or (equal? r "") (starts-with? r " ") (starts-with? r "\t"))))
           (car ls))
          (else (loop (cdr ls))))))

;; The fill prefix of `line`: its leading whitespace, plus "<leader> " when the
;; line opens with a comment leader ("  ;; aaa" → "  ;; ", "    aaa" → "    ").
(define (fill-prefix-of line)
  (let* ((rest   (trim-start line))
         (indent (substring line 0 (- (string-length line) (string-length rest))))
         (leader (fill-leader-of rest)))
    (if leader (string-append indent leader " ") indent)))

;; `line` with its indentation and comment leader (if any) stripped — the text
;; that becomes words when a paragraph is re-wrapped.
(define (fill-strip-leader line)
  (let* ((rest   (trim-start line))
         (leader (fill-leader-of rest)))
    (if leader
        (trim-start (substring rest (string-length leader) (string-length rest)))
        rest)))

;; The words of a line: split on runs of spaces/tabs, empties dropped.
(define (fill-words line)
  (filter (lambda (w) (not (equal? w "")))
          (split-many (replace-regexp-in-string "[ \t]+" " " line) " ")))

;; Re-wrap ONE paragraph (no blank lines in `text`) at `width` columns: the
;; words of every line are pooled and re-flowed greedily, each output line
;; starting with the FIRST line's fill prefix. A word that alone overflows
;; `width` gets its own line — never hard-broken. Pure and CHAR-indexed
;; throughout (columns are chars); callers translate buffer bytes.
(define (fill-string text width)
  (let* ((lines  (split-many text "\n"))
         (prefix (fill-prefix-of (car lines)))
         (words  (let loop ((ls lines))
                   (if (null? ls)
                       '()
                       (append (fill-words (fill-strip-leader (car ls)))
                               (loop (cdr ls)))))))
    (if (null? words)
        text
        (let loop ((ws (cdr words))
                   (cur (string-append prefix (car words)))
                   (out '()))
          (cond ((null? ws) (join-lines (reverse (cons cur out))))
                ((<= (+ (string-length cur) 1 (string-length (car ws))) width)
                 (loop (cdr ws) (string-append cur " " (car ws)) out))
                (else (loop (cdr ws) (string-append prefix (car ws)) (cons cur out))))))))

;; Re-wrap every blank-line-separated paragraph in `text`, preserving the blank
;; lines between them — the workhorse behind fill-region and visual-mode gq.
(define (fill-text text width)
  (let ((flush (lambda (para out)
                 (if (null? para)
                     out
                     (cons (fill-string (join-lines (reverse para)) width) out)))))
    (let loop ((lines (split-many text "\n")) (para '()) (out '()))
      (cond ((null? lines) (join-lines (reverse (flush para out))))
            ((equal? (string-trim (car lines)) "")
             (loop (cdr lines) '() (cons (car lines) (flush para out))))
            (else (loop (cdr lines) (cons (car lines) para) out))))))

;; --- paragraph bounds (buffer reads) ------------------------------------------
;; Is the buffer line holding byte `pos` blank (empty or only spaces/tabs)?
(define (blank-line-at? pos)
  (equal? (string-trim (buffer-substring (line-start-pos pos) (line-end-pos pos))) ""))

;; (start end) byte offsets of the blank-line-delimited paragraph containing
;; `pos` — start of its first line to end of its last (before that newline) —
;; or #f when pos sits on a blank line. Walks by lines via the line-start-pos /
;; line-end-pos bridges: a line's end is its "\n" (which belongs to that line),
;; so end±1 crosses into the neighbor.
(define (paragraph-bounds pos)
  (if (blank-line-at? pos)
      #f
      (list
       (let loop ((ls (line-start-pos pos)))
         (if (or (= ls 0) (blank-line-at? (- ls 1)))
             ls
             (loop (line-start-pos (- ls 1)))))
       (let ((mx (point-max)))
         (let loop ((le (line-end-pos pos)))
           (if (or (>= (+ le 1) mx) (blank-line-at? (+ le 1)))
               le
               (loop (line-end-pos (+ le 1)))))))))

;; The next paragraph boundary after `pos`: the start of the first blank line
;; past some non-blank text (Emacs M-}, vim `}`), or point-max at the end.
(define (paragraph-forward-pos pos)
  (let ((mx (point-max)))
    (let loop ((p pos) (seen (not (blank-line-at? pos))))
      (let ((next (+ (line-end-pos p) 1)))
        (cond ((>= next mx) mx)
              ((and seen (blank-line-at? next)) next)
              (else (loop next (or seen (not (blank-line-at? next))))))))))

;; The previous paragraph boundary before `pos`: the start of the first blank
;; line above some non-blank text (Emacs M-{, vim `{`), or 0 at the top.
(define (paragraph-backward-pos pos)
  (let loop ((ls (line-start-pos pos)) (seen (not (blank-line-at? pos))))
    (if (= ls 0)
        0
        (let ((prev (line-start-pos (- ls 1))))
          (if (and seen (blank-line-at? (- ls 1)))
              prev
              (loop prev (or seen (not (blank-line-at? (- ls 1))))))))))

;; Keyed insertion sort (Steel has no builtin `sort`): order `lst` by `(key x)`
;; under `less?`. Stable enough for the small lists Scheme sorts here.
(define (insert-sorted-by x sorted key less?)
  (cond ((null? sorted) (list x))
        ((less? (key x) (key (car sorted))) (cons x sorted))
        (else (cons (car sorted) (insert-sorted-by x (cdr sorted) key less?)))))
(define (sort-by key less? lst)
  (if (null? lst) '()
      (insert-sorted-by (car lst) (sort-by key less? (cdr lst)) key less?)))
(define (sort-strings lst) (sort-by (lambda (x) x) string<? lst))

;; --- Gregorian calendar arithmetic (pure) ------------------------------------
;; Proleptic-Gregorian day count (days since 1970-01-01), via Howard Hinnant's
;; algorithm. All our dates are positive years, so plain `quotient` suffices.
;; Shared substrate for org scheduling/repeaters and org-roam dailies.
(define (ymd->days y m d)
  (let* ((y2 (if (<= m 2) (- y 1) y))
         (era (quotient y2 400))
         (yoe (- y2 (* era 400)))
         (doy (+ (quotient (+ (* 153 (+ m (if (> m 2) -3 9))) 2) 5) (- d 1)))
         (doe (+ (* yoe 365) (quotient yoe 4) (- (quotient yoe 100)) doy)))
    (- (+ (* era 146097) doe) 719468)))

(define (days->ymd z0)
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

;; Two-digit zero-padded number ("07"), for date/time formatting.
(define (pad2 n) (if (< n 10) (string-append "0" (number->string n)) (number->string n)))

;; Minibuffer reads with an OPTIONAL initial value (Emacs `read-string` /
;; `completing-read` style). The Rust `*-impl` primitives take a required
;; initial; these wrappers default it to "" so existing 2-/3-arg calls keep
;; working while new callers may pre-fill the input:
;;   (read-string "New name: " "cb" old-name)
;;   (completing-read "Pick: " '("a" "b") "cb" "a")
;; `callback` may be a 'symbol / "string" naming a one-arg function, or a
;; PROCEDURE — a closure is stashed via `callback->name` (below), so a command
;; can capture its operands instead of smuggling them through globals:
;;   (read-string "Tags: " (lambda (input) (apply-tags H input)))
(define (read-string prompt callback . rest)
  (read-string-impl prompt (callback->name callback) (if (null? rest) "" (car rest))))
(define (completing-read prompt candidates callback . rest)
  (completing-read-impl prompt candidates (callback->name callback)
                        (if (null? rest) "" (car rest))))

;; (set-line-image line path [max-width]) — display an image on rows below
;; `line`; the optional max-width (px) caps the displayed width (0 = default).
(define (set-line-image line path . rest)
  (set-line-image-impl line path (if (null? rest) 0 (car rest))))

;; Interactive-command registry: Scheme commands register here so M-x lists them
;; alongside the built-in commands (see help.scm). `name` is the canonical
;; 'symbol form, but a "string" still works (M-x runs it by name either way).
(define *interactive-commands* '())
(define (register-command name)
  (let ((name (if (symbol? name) (symbol->string name) name)))
    (when (not (member name *interactive-commands*))
      (set! *interactive-commands* (cons name *interactive-commands*)))))

;; (define-command (name args …) body …) — define a command AND register it for
;; M-x in one form, so the name isn't repeated as a separate string. Mirrors a
;; plain `define`; M-x invokes the command with no arguments. The two-form
;; variant `(define-command name value)` registers a value binding likewise.
;;
;;   (define-command (layout-demo) (render-layout …))   ; was: (define (layout-demo) …)
;;                                                       ;       (register-command "layout-demo")
(define-syntax define-command
  (syntax-rules ()
    ((_ (name . args) body ...)
     (begin
       (define (name . args) body ...)
       (register-command 'name)))
    ((_ name value)
     (begin
       (define name value)
       (register-command 'name)))))

;; `load-theme` — load a built-in theme. With an argument (string or symbol) it
;; loads directly: (load-theme 'macros-dark). With no argument — e.g. M-x
;; load-theme — it prompts with completion over (theme-names), reusing itself as
;; the callback (the chosen name comes back as a string). Mirrors the
;; read-string / completing-read *-impl wrapper pattern above.
(define (load-theme . rest)
  (if (null? rest)
      (completing-read "Load theme: " (theme-names) 'load-theme)
      (load-theme-impl (car rest))))
(register-command 'load-theme)
;; In-buffer completion of `(load-theme 'mac…)` is wired in completion.scm, which
;; owns the argument-aware CAPF machinery.

(define (path-join a b)
  (if (ends-with? a "/") (string-append a b) (string-append a "/" b)))

;; Last path component, e.g. "/home/me/proj/" -> "proj".
(define (basename path)
  (let ((parts (filter (lambda (s) (not (equal? s "")))
                       (split-many path "/"))))
    (if (null? parts) path (last parts))))

;; --- project root resolution ------------------------------------------------
;; The Emacs `project.el` concept: walk up to the nearest marker. Lives in core
;; (loaded early) so BOTH magit and projectile can build on it. Steel resolves
;; free identifiers at definition time, so the shared resolver must precede them.

;; Files/dirs whose presence marks a project root (git first, like projectile).
;; VCS markers win over build-tool markers: a workspace member crate (its own
;; Cargo.toml) resolves to the enclosing repo, not itself, so sub-folders don't
;; show up as separate projects in switch-project.
(define *vcs-markers* (list ".git" ".hg"))
(define *project-markers* (list ".git" ".hg" ".projectile" "Cargo.toml" "package.json" "go.mod"))
(define *project-cache* '())     ; dir -> resolved-root
(define *project-known* '())     ; roots we've seen (for switch-project)

(define (project-has-marker? dir markers)
  (cond ((null? markers) #f)
        ((file-exists? (path-join dir (car markers))) #t)
        (else (project-has-marker? dir (cdr markers)))))

;; Walk up from `dir`, tracking two candidates: the OUTERMOST dir with a VCS
;; marker, and the NEAREST dir with any marker. Prefer the VCS root when one
;; exists (so nested build-tool roots collapse into the repo); otherwise fall
;; back to the nearest build marker. #f outside any project.
(define (project-root-walk dir)
  (let loop ((d dir) (vcs-root #f) (nearest #f))
    (if (equal? d "")
        (or vcs-root nearest)
        (let* ((vcs-root* (if (project-has-marker? d *vcs-markers*) d vcs-root))
               (nearest*  (if (and (not nearest) (project-has-marker? d *project-markers*))
                              d nearest))
               (parent (parent-directory d)))
          (if (equal? parent d)
              (or vcs-root* nearest*)
              (loop parent vcs-root* nearest*))))))

;; Cached resolution; remembers each root for switch-project.
(define (project-root dir)
  (or (alist-ref *project-cache* dir)
      (let ((root (project-root-walk dir)))
        (when root
          (set! *project-cache* (cons (cons dir root) *project-cache*))
          (when (not (member root *project-known*))
            (set! *project-known* (cons root *project-known*))))
        root)))

;; The directory project-aware commands (compile, term, …) should default to.
;; Prefers the title-bar project: perspectives.scm installs *project-dir-hook*
;; as a thunk returning the current perspective's root (or #f). With no active
;; perspective the hook is #f and we fall back to the current buffer's project
;; root, then its own directory (a fileless / non-project buffer). This keeps
;; project commands pinned to the SELECTED project rather than following whatever
;; buffer happens to be current — the same intent as magit's *magit-scope-hook*.
(define *project-dir-hook* #f)   ; #f or a thunk -> project-root-or-#f
;; Installer for the hook. Installers MUST go through this setter: a raw
;; top-level `set!` from a later-loaded file mutates a binding `project-directory`
;; never reads (Steel resolves free identifiers at definition time); a set!
;; executed inside this core-defined function mutates the one it does.
(define (set-project-dir-hook! f) (set! *project-dir-hook* f))
(define (project-directory)
  (or (and *project-dir-hook* (*project-dir-hook*))
      (project-root (buffer-directory))
      (buffer-directory)))

;; --- updates ----------------------------------------------------------------

;; Enable or disable the launch-time auto-update check against macros.dev. On by
;; default. (set-auto-update #f) in init.scm turns it off: no network check at
;; launch, no background download, and no "restart to update" notice. Re-enable
;; with (set-auto-update #t). Backed by the "auto-update" option; the manual
;; M-x check-for-updates command still works regardless of this setting.
(define (set-auto-update enable)
  (set-option "auto-update" enable))

;; --- procedure dispatch -----------------------------------------------------
;; Lets the keymap API bind a *procedure* (an identity) as well as a
;; "command-name" string: a bound procedure is stashed here under a generated id,
;; and the keymap binds a `(run-thunk "id")` stub that dispatch evaluates.

(define *thunk-table* (hash))
(define *thunk-counter* 0)

;; Stash `proc` and return an id whose `(run-thunk "id")` stub calls it.
(define (register-thunk proc)
  (set! *thunk-counter* (+ *thunk-counter* 1))
  (let ((id (string-append "thunk-" (number->string *thunk-counter*))))
    (set! *thunk-table* (hash-insert *thunk-table* id proc))
    id))

;; Call the procedure registered under `id` (a no-op if there is none).
(define (run-thunk id)
  (let ((p (hash-try-get *thunk-table* id)))
    (when p (p))))

;; One-argument variant, for stashed callbacks that receive a value (e.g. the
;; minibuffer result).
(define (run-thunk-1 id arg)
  (let ((p (hash-try-get *thunk-table* id)))
    (when p (p arg))))

;; Normalize a one-arg callback (for read-string / completing-read) to the name
;; the minibuffer will call with the result. A procedure is stashed and exposed
;; through a uniquely-named one-arg stub — the same trick as `hook->name` below,
;; which handles the no-arg hook shape.
(define (callback->name proc)
  (cond
    ((string? proc) proc)
    ((symbol? proc) (symbol->string proc))
    (else
     (let* ((id (register-thunk proc))
            (name (string-append "callback-fn-" id)))
       (eval-string (string-append "(define (" name " x) (run-thunk-1 \"" id "\" x))"))
       name))))

;; --- debounce ----------------------------------------------------------------
;; (make-debounce name delay-ms thunk) → a schedule procedure. Each call bumps a
;; pending counter and starts a `delay-ms` timer; only the LAST timer to fire
;; (counter back to 0) runs `thunk` — so a burst of edits repaints once. The
;; timer calls by name, so the fire step is exposed as a named stub (the
;; register-thunk pattern). The schedule procedure accepts an optional delay
;; override: `(schedule)` or `(schedule ms)`.
(define *debounce-pending* (hash))
(define (debounce--pending key)
  (let ((v (hash-try-get *debounce-pending* key))) (if v v 0)))
(define (debounce--set-pending key v)
  (set! *debounce-pending* (hash-insert *debounce-pending* key v)))
(define (make-debounce name delay-ms thunk)
  (let* ((fire (lambda ()
                 (when (> (debounce--pending name) 0)
                   (debounce--set-pending name (- (debounce--pending name) 1)))
                 (when (= (debounce--pending name) 0) (thunk))))
         (id (register-thunk fire))
         (fire-name (string-append "debounce-fire-" id)))
    (eval-string (string-append "(define (" fire-name ") (run-thunk \"" id "\"))"))
    (lambda rest
      (debounce--set-pending name (+ (debounce--pending name) 1))
      (run-with-timer (if (null? rest) delay-ms (car rest)) fire-name))))

;; --- per-buffer state over a plain alist ---------------------------------------
;; The `buffer-local-*` primitives store STRINGS only, so per-buffer Scheme
;; values (fold states, flags, lists) live in module alists keyed by buffer
;; name. These two helpers give every such alist the same read/replace shape:
;;   (define (mode--state-get)   (buffer-state-get *mode-state* '()))
;;   (define (mode--state-set v) (set! *mode-state* (buffer-state-set *mode-state* v)))
;; (Entries for killed buffers linger until overwritten — same as before; a
;; buffer-local migration needs non-string value support in the primitive.)
(define (buffer-state-get alist default)
  (let ((e (alist-ref alist (current-buffer)))) (if e e default)))
(define (buffer-state-set alist v)
  (cons (cons (current-buffer) v)
        (filter (lambda (p) (not (equal? (car p) (current-buffer)))) alist)))

;; --- list-buffer keymap boilerplate --------------------------------------------
;; The conventional read-only list-buffer keys: RET visits, n/p move, q kills,
;; and no line-number gutter (these are list views, not code). Mode-specific
;; keys (sort, refresh, marks, a different q) layer on with `define-key` after.
(define (define-list-mode-keys mode visit-cmd)
  (when visit-cmd (define-key mode "enter" visit-cmd))
  (define-key mode "n" 'next-line)
  (define-key mode "p" 'previous-line)
  (define-key mode "q" 'kill-buffer)
  (set-option (string-append "line-numbers:" mode) "false"))

;; Bind `cmd` under `chord` in keymap `map`. Like `define-key`, but `cmd` may be
;; a 'name symbol (the canonical config form) or a procedure (an identity), as
;; well as a "command-name" string. A symbol defers resolution to keypress time,
;; so it works regardless of file load order. Passing the function directly means
;; a typo is an unbound-variable error at load time, and editor tooling
;; (go-to-definition, rename) follows the binding.
;;
;;   (bind-key "fundamental" "ctrl-c g" 'magit-status)  ; canonical: a 'name symbol
;;   (bind-key "fundamental" "ctrl-c g" magit-status)   ; a Scheme command identity
;;   (bind-key "fundamental" "ctrl-c d" (lambda () (insert-string "hi")))
;;   (bind-key "fundamental" "ctrl-x g" "magit-status") ; string still works
(define (bind-key map chord cmd)
  (cond
    ((symbol? cmd) (define-key map chord (symbol->string cmd)))
    ((string? cmd) (define-key map chord cmd))
    (else (define-key map chord (string-append "run-thunk \"" (register-thunk cmd) "\"")))))

;; --- hook argument plumbing -------------------------------------------------
;; The editor stores hooks BY NAME and calls them by name on the engine thread —
;; a live closure can't cross that boundary — so the raw builtins take a string.
;; Capture them here so the friendlier `add-hook` / `remove-hook` wrappers (in
;; hooks.scm, which must live in a *separate* file: redefining `add-hook` in this
;; file would make the line above a forward-reference to the not-yet-initialized
;; local binding) can accept a 'symbol or a procedure too, like `bind-key`.
(define raw-add-hook add-hook)
(define raw-remove-hook remove-hook)

;; Normalize a hook argument to the string name the editor will call. (`proc`,
;; not `fn` — `fn` is Steel's lambda shorthand and can't be a parameter name.)
(define (hook->name proc)
  (cond
    ((string? proc) proc)
    ((symbol? proc) (symbol->string proc))
    ;; Procedure: stash it, then emit a uniquely-named no-arg global stub that
    ;; runs it. The stub's source only references the thunk by id (a string), so
    ;; eval-string is enough — no live value need cross into the definition.
    (else
     (let* ((id (register-thunk proc))
            (name (string-append "hook-fn-" id)))
       (eval-string (string-append "(define (" name ") (run-thunk \"" id "\"))"))
       name))))

;; --- active keybinding style -------------------------------------------------
;; The Normal-state keymap actually in effect: "helix-normal" under helix-mode,
;; "evil-normal" under evil-mode, and #f under plain Emacs keys. Helix must be
;; tested FIRST — `evil-enabled?` is also true under Helix, because the two
;; styles share one Normal/Insert state machine and differ only in which keymaps
;; the router consults.
;;
;; Lives here (loaded before everything but completion.scm) so the help and
;; discovery commands can branch on the style without depending on evil.scm or
;; helix.scm having loaded.
(define (current-modal-keymap)
  (cond ((helix-enabled?) "helix-normal")
        ((evil-enabled?)  "evil-normal")
        (else #f)))

;; Every keymap a plain key could resolve against right now, most specific
;; first. The modal Normal map shadows `fundamental`, and the Emacs chords stay
;; reachable underneath it — so this is the list the help commands must search
;; rather than assuming "fundamental".
(define (active-keymaps)
  (let ((m (current-modal-keymap)))
    (if m (list m "fundamental") (list "fundamental"))))
