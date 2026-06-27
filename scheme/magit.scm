;; Magit — a git porcelain, built entirely in Scheme on the special-buffer and
;; transient primitives (the Rust side knows nothing about git):
;;   * `call-process`  — run git, get stdout
;;   * `render-buffer` — draw a read-only buffer with per-line faces
;;   * `buffer-line` / `goto-line` — find / restore the section at point
;;   * `transient`     — the commit popup
;;
;; This mirrors how `magit-section` + `transient` sit on top of plain Emacs
;; primitives. Folding-by-regeneration: a refresh just rebuilds the buffer text.

;; Faces (fall back to the default fg if a theme doesn't define them). Every
;; "magit-*" face below is user-overridable: redefine any of them in init.scm
;; with (set-face "<name>" "#rrggbb") — init.scm loads after this file, so your
;; color wins (themes can set them too, via load-theme).
(set-face "magit-section-heading" "#a6e22e")
(set-face "magit-file" "#e6db74")
(set-face "magit-key" "#66d9ef")
;; Diff faces: whole-line colors (via render-buffer line faces).
(set-face "magit-added" "#7a9a3a")    ; dim green body for +lines
(set-face "magit-removed" "#b34a5f")  ; dim red body for -lines
(set-face "magit-hunk" "#66d9ef")     ; @@ hunk headers
(set-face "magit-context" "#888888")  ; unchanged context lines
;; Structural-view sub-heading: the enclosing function/scope a run of hunks
;; lives in (git's `@@ … @@ <context>` funcname). Dim teal so it reads as a
;; child of the yellow file name without competing with the green section title.
(set-face "magit-group" "#7fb0a8")
;; Sub-line overlay accents: the leading +/- marker, brighter than its line.
(set-face "magit-add-marker" "#a6e22e")
(set-face "magit-remove-marker" "#f92672")
;; Within-line word diff (delta / diff-highlight style). A MODIFIED line reads in
;; three tiers: a dim base for the unchanged text, the bright marker, and the
;; bright changed span. (A fully added/removed line — no pairing — keeps the
;; medium body face above instead, so brand-new lines still read clearly.)
(set-face "magit-added-dim" "#4e5e34")    ; unchanged text on a modified +line
(set-face "magit-removed-dim" "#6e4651")  ; unchanged text on a modified -line
(set-face "magit-word-added" "#cdee69")   ; the changed span on a +line
(set-face "magit-word-removed" "#fc6a8a") ; the changed span on a -line
;; File-status labels in front of changed files (Emacs magit's "modified",
;; "new file", "deleted", …). Each label is overlay-colored by its status.
(set-face "magit-label-modified" "#e2c08d")
(set-face "magit-label-new"      "#a6e22e")
(set-face "magit-label-deleted"  "#f92672")
(set-face "magit-label-renamed"  "#66d9ef")
(set-face "magit-label-other"    "#e6db74")
;; Commit short hashes in log / unpushed / recent-commits sections.
(set-face "magit-hash" "#b58900")
;; Header lines: the "Head:" / "Merge:" label column vs the branch name vs the
;; trailing subject/tracking info — three tiers so they read apart.
(set-face "magit-header" "#9a9a9a")  ; the label column
(set-face "magit-branch" "#66d9ef")  ; branch / upstream names
;; Current-line band (hl-line, painted by the Rust element as a background).
;; Translucent white reads well on any dark theme.
(set-face "hl-line" "#ffffff14")

;; --- git plumbing -----------------------------------------------------------

;; The repo root magit is operating on. Resolved from the buffer you launch from
;; (NOT the process cwd) — like Emacs's `magit-toplevel` over `default-directory`.
(define *magit-dir* ".")

;; Run git in the status buffer's repo, returning stdout.
(define (git args)
  (call-process "git" (append (list "-C" *magit-dir*) args)))

;; Walk up from `dir` to the enclosing git work tree, like `magit-toplevel`.
;; Returns `dir` unchanged if it isn't inside a repo (gix returns "").
(define (magit-toplevel dir)
  (let ((root (gix-toplevel dir)))
    (if (equal? root "") dir root)))

;; Non-empty lines of a git command's output.
(define (git-lines args)
  (filter (lambda (l) (not (equal? l "")))
          (split-many (git args) "\n")))

;; Porcelain entry helpers: a line is "XY path" (X = index, Y = worktree).
;; A rename/copy entry's path field is "OLD -> NEW"; the helpers below split it
;; so git operations get real pathspecs rather than the literal arrow string.
(define (entry-status e) (substring e 0 2))
(define (entry-path e) (substring e 3 (string-length e)))
(define (char-at s i) (substring s i (+ i 1)))

;; The pathspecs a "OLD -> NEW" (or plain) section path covers — used for `add` /
;; `reset` / `checkout`, which must name every side of a rename.
(define (magit-pathspecs path) (split-many path " -> "))
;; The current on-disk path of a section: NEW for a rename, else the path itself.
;; Used wherever a single real file is needed (visit, blame, log, gitignore, diff).
(define (magit-newpath path)
  (let ((p (magit-pathspecs path))) (list-ref p (- (length p) 1))))

;; Map a porcelain status char (X for staged, Y for unstaged) to the label magit
;; shows in front of the file ("" when the status has no useful label).
(define (magit-label ch)
  (cond ((equal? ch "M") "modified")
        ((equal? ch "A") "new file")
        ((equal? ch "D") "deleted")
        ((equal? ch "R") "renamed")
        ((equal? ch "C") "copied")
        ((equal? ch "T") "typechange")
        ((equal? ch "U") "unmerged")
        (else "")))

;; The overlay face that colors a given label.
(define (magit-label-face label)
  (cond ((equal? label "modified") "magit-label-modified")
        ((equal? label "new file") "magit-label-new")
        ((equal? label "deleted")  "magit-label-deleted")
        ((equal? label "renamed")  "magit-label-renamed")
        (else "magit-label-other")))

;; Pad a label to a fixed column so the file names after it line up.
(define magit-label-width 11)
(define (magit-spaces n)
  (if (<= n 0) "" (string-append " " (magit-spaces (- n 1)))))
(define (magit-pad s width)
  (let ((len (utf8-length s)))
    (if (>= len width) (string-append s " ")
        (string-append s (magit-spaces (- width len))))))

;; ONE `gix-status` call (no subprocess) gives the working tree AND the branch
;; line: porcelain-v1 entries plus a leading "## <branch>...<upstream> [ahead N,
;; behind M]" header, so branch name, upstream, and ahead/behind all come from
;; it. Untracked files are listed individually (like `--untracked-files=all`).
;; Renames read as delete+add (gix doesn't run rename detection) — magit still
;; stages/unstages each side correctly. Being subprocess-free, this is what makes
;; the live repo-watch refresh cheap.
(define (magit-porcelain-full)
  (let* ((lines  (gix-status *magit-dir*))
         (header (if (and (not (null? lines)) (starts-with? (car lines) "##")) (car lines) ""))
         (entries (filter (lambda (l) (not (starts-with? l "##"))) lines)))
    (cons (magit-parse-branch-line header) entries)))

;; Parse the "## …" header into (branch upstream ahead behind), all strings.
;; Handles: "## main", "## main...origin/main", "## main...origin/main [ahead 1,
;; behind 2]", "## No commits yet on main", "## HEAD (no branch)".
(define (magit-parse-branch-line header)
  (if (equal? header "")
      (list "" "" "0" "0")
      (let ((s (substring header 3 (string-length header))))  ; drop "## "
        (cond
          ((starts-with? s "No commits yet on ")
           (list (substring s 18 (string-length s)) "" "0" "0"))
          ((starts-with? s "HEAD (no branch)")
           (list "HEAD (detached)" "" "0" "0"))
          (else
           (let ((dots (split-many s "...")))               ; branch...upstream[ ab]
             (if (= (length dots) 1)
                 (list (car dots) "" "0" "0")               ; no upstream
                 (let ((br (split-many (string-join (cdr dots) "...") " [")))
                   (if (= (length br) 1)
                       (list (car dots) (car br) "0" "0")   ; upstream, in sync
                       (let ((ab (magit-parse-ab (car (split-many (cadr br) "]")))))
                         (list (car dots) (car br) (car ab) (cdr ab))))))))))))

;; "ahead 1, behind 2" / "ahead 1" / "behind 2" → (ahead . behind) strings.
(define (magit-trim-comma s)
  (if (ends-with? s ",") (substring s 0 (- (string-length s) 1)) s))
(define (magit-parse-ab s)
  (let loop ((toks (split-many s " ")) (ahead "0") (behind "0"))
    (cond ((null? toks) (cons ahead behind))
          ((and (equal? (car toks) "ahead") (not (null? (cdr toks))))
           (loop (cdr (cdr toks)) (magit-trim-comma (cadr toks)) behind))
          ((and (equal? (car toks) "behind") (not (null? (cdr toks))))
           (loop (cdr (cdr toks)) ahead (magit-trim-comma (cadr toks))))
          (else (loop (cdr toks) ahead behind)))))

;; Staged / unstaged / untracked entries are derived from ONE porcelain scan
;; (passed in as `entries`) rather than each re-running `git status`.
;; Staged / unstaged carry their status label: (label path).
(define (magit-staged entries)
  (map (lambda (e) (list (magit-label (char-at e 0)) (entry-path e)))
       (filter (lambda (e)
                 (let ((x (char-at e 0)))
                   (and (not (equal? x " ")) (not (equal? x "?")))))
               entries)))

(define (magit-unstaged entries)
  (map (lambda (e) (list (magit-label (char-at e 1)) (entry-path e)))
       (filter (lambda (e)
                 (let ((y (char-at e 1)))
                   (and (not (equal? y " ")) (not (equal? y "?")))))
               entries)))

(define (magit-untracked entries)
  (map entry-path
       (filter (lambda (e) (equal? (entry-status e) "??"))
               entries)))

;; --- branch / upstream introspection (for the header) -----------------------
;; Branch, upstream, and ahead/behind all come from the `--branch` status header
;; (see magit-parse-branch-line) — no dedicated rev-parse / rev-list spawns.

;; The "  (ahead N, behind M)" suffix for a Merge header — "" when in sync, or
;; one-sided when only ahead or only behind (matches magit's tracking display).
(define (magit-ab-string ab)
  (if (not ab)
      ""
      (let ((ahead (car ab)) (behind (cdr ab)))
        (cond ((and (equal? ahead "0") (equal? behind "0")) "")
              ((equal? behind "0") (string-append "  (ahead " ahead ")"))
              ((equal? ahead "0")  (string-append "  (behind " behind ")"))
              (else (string-append "  (ahead " ahead ", behind " behind ")"))))))

;; In-progress merge / rebase, read from the repo's git-dir state by gix.
(define (magit-merging?)  (equal? (gix-state *magit-dir*) "merge"))
(define (magit-rebasing?) (equal? (gix-state *magit-dir*) "rebase"))

;; --- confirmation for destructive operations --------------------------------
;; A one-shot thunk gated behind `y-or-n-p`; the answer arrives asynchronously
;; (minibuffer), so the action is captured as a closure and run only on "yes".
(define *magit-confirm-thunk* #f)
(define (magit-confirm prompt thunk)
  (set! *magit-confirm-thunk* thunk)
  (y-or-n-p prompt "magit-confirm-yes"))
(define (magit-confirm-yes ans)
  (let ((a *magit-confirm-thunk*))
    (set! *magit-confirm-thunk* #f)
    (when (and a (equal? ans "yes")) (a))))

;; --- async git (push / pull / fetch) ----------------------------------------
;; Network ops would freeze the UI synchronously, so stream them through a
;; subprocess. `/bin/sh -c "… 2>&1"` mirrors shell.scm's compile lane (git's
;; progress goes to stderr). Output is accumulated, not shown live — the output
;; primitives steal focus — and only surfaced in *magit-process* on failure.
(define *magit-async-out* "")
(define *magit-async-done* "")
(define (magit-run-async args done-msg)
  (set! *magit-async-out* "")
  (set! *magit-async-done* done-msg)
  (message (string-append "git " (string-join args " ") "…"))
  (make-process "/bin/sh"
                (list "-c" (string-append "cd " *magit-dir* " && git "
                                          (string-join args " ") " 2>&1"))
                "magit-async-output" "magit-async-exit"))
(define (magit-async-output line)
  (set! *magit-async-out* (string-append *magit-async-out* line "\n")))
(define (magit-async-exit code)
  (magit-refresh)
  (if (equal? code "0")
      (message *magit-async-done*)
      (begin
        (render-buffer "*magit-process*" "magit-log-mode"
                       (if (equal? (trim-end *magit-async-out*) "") "git failed"
                           (trim-end *magit-async-out*)) '())
        (message "git failed — see *magit-process*"))))

;; --- commit-message buffer helpers ------------------------------------------
(define magit-commit-comment-intro
  "# Please enter the commit message. Lines starting with '#' are ignored.\n#\n# Changes to be committed:\n")
;; Prefix each diff line with "# " so it shows as an ignored comment.
(define (magit-comment-diff raw)
  (string-join (map (lambda (l) (string-append "# " l))
                    (split-many (trim-end raw) "\n")) "\n"))
;; Drop comment lines (and trim) — the inverse, applied before committing.
(define (magit-strip-comments s)
  (trim-end (string-join
              (filter (lambda (l) (not (starts-with? l "#"))) (split-many s "\n"))
              "\n")))

;; --- transient switches (magit-style toggleable flags) ----------------------
;; The Rust transient is modal — any row key closes it. A "switch" row exploits
;; that: it toggles a flag, then re-opens the SAME popup (re-rendered with the
;; new [x]/[ ] state), so the popup appears to stay open while flags flip. Action
;; rows read the flags to assemble their git arguments.
(define *magit-switches* '())  ; (flag . #t/#f), flags namespaced "popup:arg"
(define (magit-switch-on? flag)
  (let loop ((s *magit-switches*))
    (cond ((null? s) #f)
          ((equal? (car (car s)) flag) (cdr (car s)))
          (else (loop (cdr s))))))
(define (magit-switch-toggle! flag)
  (let ((on (magit-switch-on? flag)))
    (set! *magit-switches*
          (cons (cons flag (not on))
                (filter (lambda (p) (not (equal? (car p) flag))) *magit-switches*)))))
;; Clear every flag under a popup prefix (e.g. after the action runs).
(define (magit-switch-clear! prefix)
  (set! *magit-switches*
        (filter (lambda (p) (not (starts-with? (car p) prefix))) *magit-switches*)))
;; A switch row's label: "[x] Text" when on, "[ ] Text" when off.
(define (magit-sw-label text flag)
  (string-append (if (magit-switch-on? flag) "[x] " "[ ] ") text))
;; The git args contributed by the enabled flags among `specs` ((flag . arg) …).
(define (magit-switch-args specs)
  (let loop ((s specs) (acc '()))
    (cond ((null? s) (reverse acc))
          ((magit-switch-on? (car (car s))) (loop (cdr s) (cons (cdr (car s)) acc)))
          (else (loop (cdr s) acc)))))

;; --- buffer construction ----------------------------------------------------
;; Emit accumulates lines + whole-line faces while tracking BOTH the line number
;; (for point→section mapping) and the byte offset (for sub-line overlay accents,
;; which the Rust overlay API addresses by byte). Sections record each file's
;; heading line and its diff body line range, so TAB can fold a section.

(define *magit-lines* '())     ; rendered text lines (reversed while building)
(define *magit-faces* '())     ; (line face) — whole-line colors
(define *magit-n* 0)           ; next line number
(define *magit-byte* 0)        ; byte offset of the next line
(define *magit-accents* '())   ; (start end face) — sub-line overlay accents
(define *magit-sections* '())  ; (heading-line path type body-start body-end)
(define *magit-expanded* '())  ; (type . path) keys the user has expanded; every
                               ; other file section is collapsed by default
                               ; (persists). Keyed by type AND path so expanding a
                               ; file in Unstaged doesn't also expand its Staged
                               ; counterpart when it moves groups.
(define *magit-hunks* '())     ; (start-line end-line file-header hunk-text type path)
;; Structural view (see "structural grouping" below): runs of hunks grouped by
;; their enclosing function. (heading-line body-start body-end type path context).
(define *magit-groups* '())
;; Groups the user has collapsed (default is expanded so a freshly-expanded file
;; reads like the line view). Keyed (type path context), persists across refresh.
(define *magit-collapsed-groups* '())
(define *magit-commit-lines* '()) ; (line . ref) — commit/stash lines RET can show
(define *magit-show-history* #t)  ; stashes/unpushed/unpulled — cheap via gix (no subprocess), so shown by default
;; When set, `magit-render` updates the buffer in place without stealing focus
;; (a background refresh, e.g. from `after-save-hook`). It also skips the sub-line
;; accent overlays, which target the *active* buffer — see magit-maybe-refresh.
(define *magit-quiet* #f)

(define (magit-emit text face type path)
  (set! *magit-lines* (cons text *magit-lines*))
  (when face (set! *magit-faces* (cons (list *magit-n* face) *magit-faces*)))
  (set! *magit-n* (+ *magit-n* 1))
  (set! *magit-byte* (+ *magit-byte* (utf8-length text) 1))) ; +1 = the newline

;; Section accessors over (heading-line path type body-start body-end).
(define (sec-heading s) (nth s 0))
(define (sec-path s) (nth s 1))
(define (sec-type s) (nth s 2))
(define (sec-body-start s) (nth s 3))
(define (sec-body-end s) (nth s 4))
;; Identity of a file section in *magit-expanded* — type AND path, so the same
;; file in Unstaged vs Staged folds independently.
(define (sec-key s) (cons (sec-type s) (sec-path s)))

;; Emit a file heading line "  <label>  <path>", coloring the label by its
;; status via a sub-line overlay accent (the path keeps the magit-file face).
;; An empty label (untracked files) just emits "  <path>".
(define (magit-emit-file-heading label path type)
  (let ((byte *magit-byte*))
    (if (equal? label "")
        (magit-emit (string-append "  " path) "magit-file" type path)
        (begin
          (magit-emit (string-append "  " (magit-pad label magit-label-width) path)
                      "magit-file" type path)
          ;; Accent just the label bytes (after the 2-space indent).
          (set! *magit-accents*
                (cons (list (+ byte 2) (+ byte 2 (utf8-length label))
                            (magit-label-face label))
                      *magit-accents*))))))

;; --- diff rendering ---------------------------------------------------------

;; Keep only hunk/body lines from `git diff` (drop the file-header noise).
(define (magit-diff-keep? l)
  (not (or (starts-with? l "diff --git") (starts-with? l "index ")
           (starts-with? l "--- ") (starts-with? l "+++ "))))

(define (magit-diff-face l)
  (cond ((starts-with? l "@@") "magit-hunk")
        ((starts-with? l "+") "magit-added")
        ((starts-with? l "-") "magit-removed")
        (else "magit-context")))

(define (magit-diff-marker? l)
  (or (starts-with? l "+") (starts-with? l "-")))  ; (+++/--- already filtered)

(define (magit-diff-marker-face l)
  (if (starts-with? l "+") "magit-add-marker" "magit-remove-marker"))

;; Emit one diff line, recording a 1-byte overlay accent on its +/- marker.
(define (magit-emit-diff-line l)
  (let ((byte *magit-byte*))
    (magit-emit l (magit-diff-face l) #f #f)
    (when (magit-diff-marker? l)
      (set! *magit-accents*
            (cons (list byte (+ byte 1) (magit-diff-marker-face l)) *magit-accents*)))))

;; --- within-line word diff --------------------------------------------------
;; A `-`/`+` line pair usually differs in only part of the line. Highlight just
;; that part: trim the shared head and tail, paint the middle on a brighter face.
;; This is git's diff-highlight heuristic — it never changes the hunk TEXT (so
;; staging is unaffected), only adds overlay accents on the changed byte span.
;; Pairing happens only when a run of N removed lines is immediately followed by
;; N added lines (same as diff-highlight); otherwise lines stay whole-line lit.

;; Count of leading chars `a` and `b` share (char-indexed, UTF-8 safe).
(define (magit-common-prefix a b)
  (let ((la (string-length a)) (lb (string-length b)))
    (let loop ((i 0))
      (if (and (< i la) (< i lb)
               (equal? (substring a i (+ i 1)) (substring b i (+ i 1))))
          (loop (+ i 1))
          i))))
;; Count of trailing chars shared, capped at `limit` so head and tail can't
;; overlap (e.g. a one-sided insertion).
(define (magit-common-suffix a b limit)
  (let ((la (string-length a)) (lb (string-length b)))
    (let loop ((i 0))
      (if (and (< i limit)
               (equal? (substring a (- la i 1) (- la i))
                       (substring b (- lb i 1) (- lb i))))
          (loop (+ i 1))
          i))))
;; The changed middle of `a` relative to `b`, as a (start . end) CHAR range on
;; `a`. Empty (start = end) when `a`'s content is wholly shared (pure insertion
;; on the other side) — the caller then paints nothing.
(define (magit-change-span a b)
  (let* ((la (string-length a)) (lb (string-length b))
         (p (magit-common-prefix a b))
         (s (magit-common-suffix a b (min (- la p) (- lb p)))))
    (cons p (- la s))))

;; Emit diff line `l`, accenting its +/- marker AND its changed span vs the
;; paired line `other`. `removed?` selects the removed vs added word face. The
;; marker is one ASCII byte, so content starts at byte+1; the changed span's
;; byte offsets come from utf8-length of the content prefix (UTF-8 safe).
(define (magit-emit-intra-line l other removed?)
  (let ((byte *magit-byte*))
    ;; Dim the whole modified line; the marker and changed span are re-brightened
    ;; by the overlays below. (Unpaired add/remove lines never reach here, so
    ;; they keep the medium body face from magit-emit-diff-line.)
    (magit-emit l (if removed? "magit-removed-dim" "magit-added-dim") #f #f)
    (set! *magit-accents*
          (cons (list byte (+ byte 1) (magit-diff-marker-face l)) *magit-accents*))
    (let* ((c1 (substring l 1 (string-length l)))
           (c2 (substring other 1 (string-length other)))
           (span (magit-change-span c1 c2)))
      (when (> (cdr span) (car span))
        (let ((bs (+ byte 1 (utf8-length (substring c1 0 (car span)))))
              (be (+ byte 1 (utf8-length (substring c1 0 (cdr span))))))
          (set! *magit-accents*
                (cons (list bs be (if removed? "magit-word-removed" "magit-word-added"))
                      *magit-accents*)))))))

;; The maximal leading run of `lines` whose members start with `prefix`, as
;; (run . rest).
(define (magit-split-run lines prefix)
  (let loop ((ls lines) (run '()))
    (if (and (not (null? ls)) (starts-with? (car ls) prefix))
        (loop (cdr ls) (cons (car ls) run))
        (cons (reverse run) ls))))

;; Emit a paired removed-run + added-run: the removed lines first (in file
;; order), each accented against its positional partner, then the added lines.
(define (magit-emit-paired-lines rem add)
  (let loop ((rs rem) (as add))
    (when (not (null? rs))
      (magit-emit-intra-line (car rs) (car as) #t)
      (loop (cdr rs) (cdr as))))
  (let loop ((rs rem) (as add))
    (when (not (null? as))
      (magit-emit-intra-line (car as) (car rs) #f)
      (loop (cdr rs) (cdr as)))))

;; Emit a hunk body (the lines after the @@ header) with within-line word diff:
;; an N-removed / N-added block is paired and intra-highlighted; everything else
;; (context, the @@ line, unbalanced blocks) is emitted whole-line as before.
(define (magit-emit-hunk-body lines)
  (cond
    ((null? lines) #t)
    ((starts-with? (car lines) "-")
     (let* ((rsplit (magit-split-run lines "-"))
            (rem (car rsplit)) (after-rem (cdr rsplit))
            (asplit (magit-split-run after-rem "+"))
            (add (car asplit)) (after-add (cdr asplit)))
       (if (and (not (null? add)) (= (length rem) (length add)))
           (begin (magit-emit-paired-lines rem add)
                  (magit-emit-hunk-body after-add))
           (begin (for-each magit-emit-diff-line rem)
                  (magit-emit-hunk-body after-rem)))))
    (else
     (magit-emit-diff-line (car lines))
     (magit-emit-hunk-body (cdr lines)))))

;; Split a `git diff` into (header . hunks): the file header (lines before the
;; first @@) and each @@-hunk as its own string. The header + one hunk form a
;; valid patch for `git apply` (hunk-level staging).
(define (parse-diff raw)
  (let loop ((lines (split-many raw "\n")) (header '()) (hunks '()) (cur '()) (in #f))
    (cond
      ((null? lines)
       (cons (join-lines (reverse header))
             (reverse (if in (cons (join-lines (reverse cur)) hunks) hunks))))
      ((starts-with? (car lines) "@@")
       (loop (cdr lines) header
             (if in (cons (join-lines (reverse cur)) hunks) hunks)
             (list (car lines)) #t))
      (in (loop (cdr lines) header hunks (cons (car lines) cur) #t))
      (else (loop (cdr lines) (cons (car lines) header) hunks cur #f)))))

;; --- bulk diff (one git call per section, not per file) ---------------------
;; A single `git diff` lists every changed file; splitting it per file lets one
;; invocation replace N. Each file's slice is byte-identical to what
;; `git diff -- <path>` prints, so parse-diff and hunk-apply are unaffected.

;; Split bulk diff lines into per-file line-lists (each starts at "diff --git").
(define (split-diff-files lines)
  (let loop ((lines lines) (cur '()) (acc '()))
    (cond
      ((null? lines)
       (reverse (if (null? cur) acc (cons (reverse cur) acc))))
      ((and (starts-with? (car lines) "diff --git") (not (null? cur)))
       (loop (cdr lines) (list (car lines)) (cons (reverse cur) acc)))
      (else (loop (cdr lines) (cons (car lines) cur) acc)))))

;; The first line in `lines` with `prefix`, sans that prefix (or #f). File-header
;; lines precede any hunk body, so the header's path is always found first.
(define (chunk-prefixed lines prefix)
  (cond ((null? lines) #f)
        ((starts-with? (car lines) prefix)
         (substring (car lines) (string-length prefix) (string-length (car lines))))
        (else (chunk-prefixed (cdr lines) prefix))))

;; The work-tree path a chunk describes: from "+++ b/PATH", or "--- a/PATH" when
;; the new side is /dev/null (a deletion).
(define (chunk-path chunk)
  (or (chunk-prefixed chunk "+++ b/")
      (chunk-prefixed chunk "--- a/")))

;; (path diff-text) per file in a bulk `git diff`. trim-end first so the last
;; file's slice carries no trailing blank line (keeps its hunks apply-clean).
(define (build-diff-map raw)
  (if (equal? (trim-end raw) "")
      '()
      (map (lambda (chunk) (list (chunk-path chunk) (join-lines chunk)))
           (split-diff-files (split-many (trim-end raw) "\n")))))

(define (diff-map-get dmap path)
  (cond ((null? dmap) "")
        ((equal? (car (car dmap)) path) (cadr (car dmap)))
        (else (diff-map-get (cdr dmap) path))))

;; --- structural grouping ----------------------------------------------------
;; `git diff` already names the enclosing function on every hunk header
;; ("@@ -a,b +c,d @@ fn parse_expr"). The structural view groups a file's hunks
;; by that context under a collapsible sub-heading, so review steps function by
;; function (fold a reviewed fn, stage a whole fn) instead of hunk by hunk.
;; It's a pure reshaping of the SAME line hunks — no AST, no extra git. Toggle
;; with `v`. Live state is a plain global (like *magit-show-history*) so a toggle
;; takes effect on the very next render; set it in init.scm to change the default:
;;   (set! *magit-diff-view* "structural")
(define *magit-diff-view* "line")  ; "line" | "structural"
(define (magit-structural?) (equal? *magit-diff-view* "structural"))

;; The funcname context of a hunk: everything after the second "@@" on its
;; header line, trimmed. "" when git emitted no context (top-of-file changes, or
;; a language with no funcname pattern) — those collapse into one "(top level)".
(define (magit-ltrim s)
  (if (and (> (string-length s) 0) (equal? (substring s 0 1) " "))
      (magit-ltrim (substring s 1 (string-length s)))
      s))
(define (magit-hunk-context hunk-text)
  (let ((parts (split-many (car (split-many hunk-text "\n")) "@@")))
    (if (>= (length parts) 3)
        (magit-ltrim (trim-end (string-join (cdr (cdr parts)) "@@")))
        "")))
(define (magit-group-display ctx) (if (equal? ctx "") "(top level)" ctx))

;; Partition hunks into (context hunk…) groups, merging only CONSECUTIVE hunks
;; that share a context (a function occupies a contiguous span in a diff, so its
;; hunks are always adjacent). Preserves file order.
(define (magit-group-hunks hunks)
  (let loop ((hs hunks) (ctx #f) (cur '()) (acc '()))
    (cond
      ((null? hs)
       (reverse (if (null? cur) acc (cons (cons ctx (reverse cur)) acc))))
      (else
       (let ((c (magit-hunk-context (car hs))))
         (if (and ctx (equal? c ctx))
             (loop (cdr hs) ctx (cons (car hs) cur) acc)
             (loop (cdr hs) c (list (car hs))
                   (if (null? cur) acc (cons (cons ctx (reverse cur)) acc)))))))))

;; Emit one hunk's lines and record it in *magit-hunks* (shared by both views,
;; so per-hunk `s`/`u`/`k` and partial-region staging work identically). The @@
;; header is emitted plainly; the body goes through the word-diff emitter. Line
;; count is unchanged (one emit per line), so hunk/group spans stay correct.
(define (magit-emit-one-hunk header hunk type path)
  (let ((hstart *magit-n*)
        (lines (split-many hunk "\n")))
    (magit-emit-diff-line (car lines))
    (magit-emit-hunk-body (cdr lines))
    (set! *magit-hunks*
          (cons (list hstart *magit-n* header hunk type path) *magit-hunks*))))

;; Line view: hunks back to back, exactly as before the structural view existed.
(define (magit-emit-hunks-flat header hunks type path)
  (for-each (lambda (hunk) (magit-emit-one-hunk header hunk type path)) hunks))

;; A group sub-heading "    <context>  (N hunks)", indented under the file name.
(define (magit-emit-group-heading ctx n)
  (magit-emit (string-append "    " (magit-group-display ctx)
                             "  (" (int->string n) (if (= n 1) " hunk)" " hunks)"))
              "magit-group" #f #f))

;; Structural view: a sub-heading per funcname group, its hunks foldable as a
;; unit. Records each group's line span in *magit-groups* for fold/stage/nav.
(define (magit-emit-hunks-grouped header hunks type path)
  (for-each
    (lambda (grp)
      (let ((ctx (car grp)) (ghunks (cdr grp)) (ghead *magit-n*))
        (magit-emit-group-heading ctx (length ghunks))
        (let ((gbody *magit-n*))
          (for-each (lambda (hunk) (magit-emit-one-hunk header hunk type path)) ghunks)
          (set! *magit-groups*
                (cons (list ghead gbody *magit-n* type path ctx) *magit-groups*)))))
    (magit-group-hunks hunks)))

;; Group accessors over (heading body-start body-end type path context).
(define (grp-heading g) (nth g 0))
(define (grp-body-start g) (nth g 1))
(define (grp-body-end g) (nth g 2))
(define (grp-type g) (nth g 3))
(define (grp-path g) (nth g 4))
(define (grp-context g) (nth g 5))
(define (grp-key g) (list (grp-type g) (grp-path g) (grp-context g)))

;; Emit a file heading + its diff, one hunk at a time; record each hunk's line
;; range + the patch material so `s`/`u`/`k` can act on a single hunk. In the
;; structural view the hunks are first grouped by enclosing function.
;; `entry` is (label path); `diff-text` is this file's slice of the section's
;; bulk diff (see build-diff-map).
(define (magit-emit-diff entry type diff-text)
  (let* ((label (car entry))
         (path (cadr entry))
         (heading *magit-n*)
         (parsed (parse-diff diff-text))
         (header (car parsed))
         (hunks (cdr parsed)))
    (magit-emit-file-heading label path type)
    (let ((body-start *magit-n*))
      (if (magit-structural?)
          (magit-emit-hunks-grouped header hunks type path)
          (magit-emit-hunks-flat header hunks type path))
      (set! *magit-sections*
            (cons (list heading path type body-start *magit-n*) *magit-sections*)))))

;; `dmap` is the section's bulk-diff map; each file pulls its slice by path.
(define (magit-emit-diff-section title files type dmap)
  (when (not (null? files))
    (magit-emit (string-append title " (" (int->string (length files)) ")")
                "magit-section-heading" #f #f)
    (for-each (lambda (f) (magit-emit-diff f type (diff-map-get dmap (magit-newpath (cadr f))))) files)
    (magit-emit "" #f #f #f)))

;; Untracked files have no diff — list them as (empty-body) sections.
(define (magit-emit-file-section title files type)
  (when (not (null? files))
    (magit-emit (string-append title " (" (int->string (length files)) ")")
                "magit-section-heading" #f #f)
    (for-each (lambda (f)
                (let ((heading *magit-n*))
                  (magit-emit-file-heading "" f type)
                  (set! *magit-sections*
                        (cons (list heading f type heading heading) *magit-sections*))))
              files)
    (magit-emit "" #f #f #f)))

;; Emit one commit/stash line "  <ref> <subject>", accenting the leading ref
;; (short hash or stash ref) and recording (line . ref) so RET can show it.
;; A trailing ":" on a stash ref ("stash@{0}:") is stripped for `git show`.
(define (magit-emit-commit line)
  (let* ((parts (split-many line " "))
         (tok (if (null? parts) "" (car parts)))
         (ref (if (ends-with? tok ":") (substring tok 0 (- (string-length tok) 1)) tok))
         (ln *magit-n*)
         (byte *magit-byte*))
    (magit-emit (string-append "  " line) #f #f #f)
    (when (not (equal? tok ""))
      (set! *magit-accents*
            (cons (list (+ byte 2) (+ byte 2 (utf8-length tok)) "magit-hash") *magit-accents*))
      (set! *magit-commit-lines* (cons (cons ln ref) *magit-commit-lines*)))))

;; A titled section of commit/stash lines (no diff bodies, nothing to fold).
(define (magit-emit-commit-section title lines)
  (when (not (null? lines))
    (magit-emit (string-append title " (" (int->string (length lines)) ")")
                "magit-section-heading" #f #f)
    (for-each magit-emit-commit lines)
    (magit-emit "" #f #f #f)))

;; The commit/stash ref on `line`, or #f.
(define (magit-commit-at-line line)
  (let loop ((cs *magit-commit-lines*))
    (cond ((null? cs) #f)
          ((= (car (car cs)) line) (cdr (car cs)))
          (else (loop (cdr cs))))))

;; Emit a header line "<label>   <branch> <rest>", accenting the branch name and
;; dimming the rest, so the label column / branch / detail read as three tiers.
;; `rest` is "" or starts with its own leading space.
(define magit-header-width 10)
(define (magit-emit-header label branch rest)
  (let* ((labelcol (magit-pad label magit-header-width))
         (byte *magit-byte*)
         (bstart (+ byte (utf8-length labelcol)))
         (bend (+ bstart (utf8-length branch))))
    (magit-emit (string-append labelcol branch rest) "magit-header" #f #f)
    (set! *magit-accents* (cons (list bstart bend "magit-branch") *magit-accents*))
    (when (> (utf8-length rest) 0)
      (set! *magit-accents*
            (cons (list bend (+ bend (utf8-length rest)) "magit-context") *magit-accents*)))))

;; The section matching both `path` and `type`, or #f. Type-qualified because a
;; partially-staged file appears in BOTH Unstaged and Staged — point restore must
;; find the one in the group the cursor was actually in.
(define (magit-section-by-path-type path type)
  (let loop ((secs *magit-sections*))
    (cond ((null? secs) #f)
          ((and (equal? (sec-path (car secs)) path) (equal? (sec-type (car secs)) type))
           (car secs))
          (else (loop (cdr secs))))))

;; Apply overlay accents + folds AFTER render (they target the now-active buffer).
;; clear-overlays first so a refresh doesn't stack stale accents/folds.
(define (magit-apply-overlays)
  (clear-overlays)
  (for-each (lambda (a) (overlay-set-face (nth a 0) (nth a 1) (nth a 2)))
            (reverse *magit-accents*))
  ;; Fold every file section the user hasn't explicitly expanded (collapsed by
  ;; default). Sections with no diff body (untracked files) have nothing to fold.
  (for-each (lambda (sec)
              (when (and (not (member (sec-key sec) *magit-expanded*))
                         (> (sec-body-end sec) (sec-body-start sec)))
                (fold-lines (sec-body-start sec) (sec-body-end sec))))
            *magit-sections*)
  ;; Then fold each collapsed group — but only inside an expanded file section,
  ;; since a collapsed file already hides its groups (and double-folding an
  ;; already-hidden range is needless).
  (for-each (lambda (g)
              (when (and (member (grp-key g) *magit-collapsed-groups*)
                         (member (cons (grp-type g) (grp-path g)) *magit-expanded*)
                         (> (grp-body-end g) (grp-body-start g)))
                (fold-lines (grp-body-start g) (grp-body-end g))))
            *magit-groups*))

;; --- point restore across refresh -------------------------------------------
;; A refresh rebuilds the buffer. Two cases:
;;   * The file at point is still in the SAME section group (hunk-level stage,
;;     fold toggle, partial unstage) → follow it, keeping the in-section offset.
;;     This keeps you on the hunk you were editing.
;;   * The file LEFT its group (e.g. you fully staged it, so it jumped from
;;     Unstaged to Staged) → don't chase it. Stay at the same screen line, where
;;     the next sibling has now shifted up — so you can keep staging down the list,
;;     which is what magit does.
(define (magit-capture-point)
  (if (equal? (current-buffer) "*magit*")
      (let ((s (magit-section-at-line (buffer-line))))
        (if s
            (list "section" (sec-path s) (sec-type s)
                  (- (buffer-line) (sec-heading s)) (buffer-line))
            (list "line" (buffer-line))))
      #f))

(define (magit-restore-point cap)
  (when cap
    (cond
      ((equal? (car cap) "section")
       (let* ((path (nth cap 1)) (type (nth cap 2)) (delta (nth cap 3)) (absline (nth cap 4))
              (s (magit-section-by-path-type path type)))
         (if s
             ;; Same file, same group → follow. Keep the offset only when the body
             ;; is visible (a collapsed section folds it), else land on the head.
             (let* ((d (if (member (cons type path) *magit-expanded*) delta 0))
                    (target (+ (sec-heading s) d)))
               (goto-line (min target (max (sec-heading s) (- (sec-body-end s) 1)))))
             ;; Moved out of its group → hold the line; the next item sits there now.
             (goto-line absline))))
      ((equal? (car cap) "line") (goto-line (nth cap 1))))))

;; Render the status buffer for the current *magit-dir* (no re-resolution).
(define (magit-render)
  (let ((cap (magit-capture-point))
        ;; The cursor's on-screen row before the redraw, so we can pin it back
        ;; afterward and the buffer doesn't visually jump (only meaningful when
        ;; *magit* is the focused buffer — the interactive refresh path).
        (row (window-cursor-row)))
    (set! *magit-lines* '())
    (set! *magit-faces* '())
    (set! *magit-n* 0)
    (set! *magit-byte* 0)
    (set! *magit-accents* '())
    (set! *magit-sections* '())
    (set! *magit-hunks* '())
    (set! *magit-groups* '())
    (set! *magit-commit-lines* '())
    ;; ONE status scan (`--branch`) feeds the working-tree sections AND the
    ;; branch/upstream/ahead-behind header — no separate rev-parse/rev-list calls.
    (let* ((porc      (magit-porcelain-full))
           (binfo     (car porc))
           (entries   (cdr porc))
           (untracked (magit-untracked entries))
           (unstaged  (magit-unstaged entries))
           (staged    (magit-staged entries))
           (branch    (nth binfo 0))
           (up        (nth binfo 1))
           (ab        (if (equal? up "") #f (cons (nth binfo 2) (nth binfo 3))))
           (subject   (gix-head-subject *magit-dir*)))
      ;; --- header ---
      (magit-emit-header "Head:" (if (equal? branch "") "(no commits)" branch)
                         (if (equal? subject "") "" (string-append " " subject)))
      (when (not (equal? up ""))
        (magit-emit-header "Merge:" up (magit-ab-string ab)))
      (when (magit-merging?)
        (magit-emit "Merging — resolve conflicts, stage, then commit."
                    "magit-label-deleted" #f #f))
      (when (magit-rebasing?)
        (magit-emit "Rebase in progress." "magit-label-deleted" #f #f))
      (magit-emit "" #f #f #f)
      ;; --- working tree ---
      (magit-emit-file-section "Untracked files" untracked "untracked")
      (magit-emit-diff-section "Unstaged changes" unstaged "unstaged"
                               (if (null? unstaged) '() (build-diff-map (gix-diff *magit-dir*))))
      (magit-emit-diff-section "Staged changes" staged "staged"
                               (if (null? staged) '() (build-diff-map (gix-diff-cached *magit-dir*))))
      (when (and (null? untracked) (null? unstaged) (null? staged))
        (magit-emit "Nothing to commit, working tree clean." "magit-file" #f #f)
        (magit-emit "" #f #f #f))
      ;; --- stashes + ahead/behind (lazy: 3 git calls only when shown, toggle
      ;;     with H — recent commits below always render) ---
      (if *magit-show-history*
          (begin
            (magit-emit-commit-section "Stashes" (gix-stash-list *magit-dir*))
            (when (not (equal? up ""))
              (magit-emit-commit-section (string-append "Unpushed to " up)
                                         (gix-log-range *magit-dir* "HEAD" up 256))
              (magit-emit-commit-section (string-append "Unpulled from " up)
                                         (gix-log-range *magit-dir* up "HEAD" 256))))
          (magit-emit "Press H to show stashes / unpushed / unpulled."
                      "magit-context" #f #f))
      ;; --- recent commits (shown by default, one gix rev-walk) ---
      (magit-emit-commit-section "Recent commits"
                                 (gix-log-oneline *magit-dir* 10)))
    (if *magit-quiet*
        ;; Background refresh: update in place (works on an inactive pane) and
        ;; skip overlays — they'd paint onto whatever buffer is in front.
        (render-buffer-quiet "*magit*" "magit-status"
                             (join-lines (reverse *magit-lines*))
                             (reverse *magit-faces*))
        (begin
          (render-buffer "*magit*" "magit-status"
                         (join-lines (reverse *magit-lines*))
                         (reverse *magit-faces*))
          (magit-apply-overlays)
          (magit-restore-point cap)
          ;; Pin the cursor's line back to the same screen row it was on, so
          ;; restoring point doesn't scroll the buffer under the user.
          (scroll-cursor-to-row row)))
    ;; Watch the whole repo so changes from anywhere — CLI git, other editors,
    ;; saves in other windows — refresh the status live (see magit-on-fs-change).
    ;; Replace-per-owner, so switching repos re-points the watch.
    (when (and (not (equal? *magit-dir* "")) (not (equal? *magit-dir* ".")))
      (watch-tree "magit" (list *magit-dir*)))))

;; Launch: resolve the repo from the buffer you're in, then render. A refresh
;; from inside *magit* keeps the repo we're already viewing.
;; Resolve via projectile's shared project-root (walks up for `.git` & friends),
;; then git-confirm the exact work-tree. Falls back to the buffer directory.
(define (magit-status)
  (when (not (equal? (current-buffer) "*magit*"))
    (let ((start (or (project-root (buffer-directory)) (buffer-directory))))
      (set! *magit-dir* (magit-toplevel start))))
  (magit-render))

;; Open magit on an explicitly chosen directory (the override path — used by the
;; Helm picker below, or callable directly for any repo).
(define (magit-status-in dir)
  (set! *magit-dir* (magit-toplevel dir))
  (magit-render))

;; Point *magit-dir* at the current buffer's repo when invoked from a NON-magit
;; buffer. magit-status does this on entry; M-x entry points reachable from a
;; file buffer (log, dispatch) must too, or they'd inherit a stale/default
;; *magit-dir* and operate on the wrong repo. Inside a *magit…* buffer, the
;; directory is already resolved — leave it untouched.
(define (magit-ensure-dir)
  (when (not (starts-with? (current-buffer) "*magit"))
    (let ((start (or (project-root (buffer-directory)) (buffer-directory))))
      (set! *magit-dir* (magit-toplevel start)))))

;; Refresh re-runs git for the SAME repo (never re-resolves).
(define (magit-refresh) (magit-render))

;; H: toggle the stashes / unpushed / unpulled block. Hidden by default so the
;; common refresh skips its 3 extra `git log`/`stash` calls; recent commits
;; always render regardless.
(define (magit-toggle-history)
  (set! *magit-show-history* (not *magit-show-history*))
  (magit-refresh))

;; --- section at point -------------------------------------------------------

;; The section (file) whose heading or diff body contains `line`, or #f.
(define (magit-section-at-line line)
  (let loop ((secs *magit-sections*))
    (cond ((null? secs) #f)
          ((let ((s (car secs)))
             (or (= (sec-heading s) line)
                 (and (>= line (sec-body-start s)) (< line (sec-body-end s)))))
           (car secs))
          (else (loop (cdr secs))))))

;; The (type path) of the file at point, or #f.
(define (magit-current)
  (let ((s (magit-section-at-line (buffer-line))))
    (if s (list (sec-type s) (sec-path s)) #f)))

;; The hunk whose line range contains `line`, or #f.
;; hunk = (start end header hunk-text type path).
(define (magit-hunk-at-line line)
  (let loop ((hs *magit-hunks*))
    (cond ((null? hs) #f)
          ((let ((h (car hs))) (and (>= line (nth h 0)) (< line (nth h 1)))) (car hs))
          (else (loop (cdr hs))))))

;; The group (heading or body lines) containing `line`, or #f. Structural view
;; only — *magit-groups* is empty in the line view, so this is a no-op there.
(define (magit-group-at-line line)
  (let loop ((gs *magit-groups*))
    (cond ((null? gs) #f)
          ((let ((g (car gs)))
             (or (= (grp-heading g) line)
                 (and (>= line (grp-body-start g)) (< line (grp-body-end g)))))
           (car gs))
          (else (loop (cdr gs))))))
;; The group whose heading is exactly `line`, or #f — used by `s`/`u`/`k` to act
;; on a whole function when point sits on its sub-heading.
(define (magit-group-on-heading line)
  (let loop ((gs *magit-groups*))
    (cond ((null? gs) #f)
          ((= (grp-heading (car gs)) line) (car gs))
          (else (loop (cdr gs))))))
;; The hunks belonging to group `g`, in file order, as a single applyable patch
;; (file header + each hunk body). #f when the group is somehow empty.
(define (magit-group-hunks-of g)
  (reverse
    (filter (lambda (h) (and (>= (nth h 0) (grp-body-start g))
                             (< (nth h 0) (grp-body-end g))))
            *magit-hunks*)))
(define (magit-group-patch g)
  (let ((hs (magit-group-hunks-of g)))
    (if (null? hs)
        #f
        (string-append (nth (car hs) 2) "\n"
                       (string-join (map (lambda (h) (nth h 3)) hs) "\n") "\n"))))

;; --- error surfacing --------------------------------------------------------
;; Run git, refresh, echo `msg` on success — or pop the stderr into a buffer.
(define (magit-do args msg)
  (let ((r (call-process-full "git" (append (list "-C" *magit-dir*) args) "")))
    (magit-refresh)
    (if (equal? (car r) "0") (message msg) (magit-error (nth r 2)))))
(define (magit-error stderr)
  (render-buffer "*magit-process*" "magit-log-mode"
                 (if (equal? (trim-end stderr) "") "git failed (no output)" (trim-end stderr)) '())
  (message "git failed — see *magit-process*"))

;; Lists from git, one item per non-empty line.
(define (magit-branches) (gix-branches *magit-dir*))
(define (magit-remotes)  (gix-remotes  *magit-dir*))
(define (magit-tags)     (gix-tags     *magit-dir*))

;; --- staging (hunk- and region-level) ---------------------------------------
;; Apply a patch to the index (or, for discard, the worktree). `--recount` lets
;; git fix the @@ counts so a hand-built (isolated or partial) hunk applies.
(define (magit-apply-patch-text patch to-index reverse?)
  (let* ((args (append (list "-C" *magit-dir* "apply" "--recount")
                       (if to-index (list "--cached") '())
                       (if reverse? (list "--reverse") '())
                       (list "-")))
         (r (call-process-full "git" args patch)))
    (magit-refresh)
    (if (equal? (car r) "0") #t (begin (magit-error (nth r 2)) #f))))
(define (magit-apply-hunk h to-index reverse?)
  (magit-apply-patch-text (string-append (nth h 2) "\n" (nth h 3) "\n") to-index reverse?))

;; --- partial (selected-region) hunk application -----------------------------
;; 0-based line index of byte `b` (newlines before it). O(b) but the status
;; buffer is small; avoids a dedicated primitive.
(define (magit-byte->line b)
  (- (length (split-many (buffer-substring 0 b) "\n")) 1))

;; Rebuild a hunk to contain ONLY the changed lines whose buffer line is within
;; [lo, hi], transforming the rest so the patch still applies. The transform
;; differs by direction; both keep context, keep selected +/- lines, and recast
;; the *unselected* changed lines into the form that makes the patch's matched
;; side equal the current index/worktree (git apply checks content; --recount
;; relaxes only the counts). Returns #f when no changed line is selected.
;; Forward (stage): unselected '+' dropped, unselected '-' becomes context.
(define (magit-xf-stage l sel?)
  (cond ((starts-with? l "+") (if sel? l #f))
        ((starts-with? l "-") (if sel? l (string-append " " (substring l 1 (string-length l)))))
        (else l)))
;; Reverse (unstage / discard): unselected '+' becomes context, '-' dropped.
(define (magit-xf-reverse l sel?)
  (cond ((starts-with? l "+") (if sel? l (string-append " " (substring l 1 (string-length l)))))
        ((starts-with? l "-") (if sel? l #f))
        (else l)))
;; Transform one hunk to its selected-region form, returning
;; (cons changed? "@@…\n<body>") — `changed?` is #t iff a +/- line was selected.
;; Several of these concatenate under one file header into a multi-hunk patch.
(define (magit-partial-hunk-text h lo hi xf)
  (let* ((hstart (nth h 0))
         (hlines (split-many (nth h 3) "\n"))
         (atat (car hlines))
         (body (cdr hlines)))
    (let loop ((bs body) (i 0) (out '()) (changed #f))
      (if (null? bs)
          (cons changed (string-append atat "\n" (string-join (reverse out) "\n")))
          (let* ((l (car bs))
                 (bl (+ hstart 1 i))
                 (sel? (and (>= bl lo) (<= bl hi)))
                 (xl (xf l sel?))
                 (change? (or (starts-with? l "+") (starts-with? l "-"))))
            (loop (cdr bs) (+ i 1)
                  (if xl (cons xl out) out)
                  (or changed (and sel? change?))))))))
;; A patch over `hunks` (all one file) keeping only those with a selected change.
;; #f when nothing in the region changed.
(define (magit-partial-multi hunks lo hi xf)
  (if (null? hunks)
      #f
      (let* ((header (nth (car hunks) 2))
             (kept (filter car (map (lambda (h) (magit-partial-hunk-text h lo hi xf)) hunks))))
        (if (null? kept)
            #f
            (string-append header "\n" (string-join (map cdr kept) "\n") "\n")))))

;; The selected buffer-line span [lo hi]; hi from re-1 so a selection ending at a
;; line boundary doesn't pull in the next line.
(define (magit-region-lines)
  (let ((rb (region-beginning)) (re (region-end)))
    (cons (magit-byte->line rb) (magit-byte->line (max rb (- re 1))))))
;; File-section paths of `type` whose heading line falls in [lo hi].
(define (magit-files-in lo hi type)
  (map sec-path (filter (lambda (s) (and (equal? (sec-type s) type)
                                         (>= (sec-heading s) lo) (<= (sec-heading s) hi)))
                        *magit-sections*)))
;; Hunks of `type` whose line range overlaps [lo hi].
(define (magit-hunks-in lo hi type)
  (filter (lambda (h) (and (equal? (nth h 4) type)
                           (<= (nth h 0) hi) (> (nth h 1) lo)))
          *magit-hunks*))
;; Distinct elements, preserving order.
(define (magit-dedup lst)
  (let loop ((l lst) (seen '()) (out '()))
    (cond ((null? l) (reverse out))
          ((member (car l) seen) (loop (cdr l) seen out))
          (else (loop (cdr l) (cons (car l) seen) (cons (car l) out))))))

;; A region selects a SET of things — whole files (when their headings are in the
;; region) and/or individual diff lines. Magit applies the op to all of them;
;; these handlers mirror that. A contiguous region can't span two files' diffs
;; without crossing the second file's heading, so the hunk path is single-file.

;; s: stage the region (multiple files and/or selected lines), else hunk/file at point.
(define (magit-stage)
  (if (region-active?)
      (let* ((r (magit-region-lines)) (lo (car r)) (hi (cdr r))
             (files (magit-dedup (append (magit-files-in lo hi "untracked")
                                         (magit-files-in lo hi "unstaged")))))
        (if (not (null? files))
            (magit-do (append (list "add" "--") (map magit-newpath files))
                      (string-append "Staged " (int->string (length files)) " file(s)"))
            (let ((patch (magit-partial-multi (magit-hunks-in lo hi "unstaged") lo hi magit-xf-stage)))
              (if patch
                  (when (magit-apply-patch-text patch #t #f) (message "Staged region"))
                  (message "Nothing to stage in region")))))
      (let ((h (magit-hunk-at-line (buffer-line)))
            (g (and (magit-structural?) (magit-group-on-heading (buffer-line)))))
        (cond
          (h (when (magit-apply-hunk h #t #f) (message "Staged hunk")))
          (g (let ((patch (magit-group-patch g)))
               (when (and patch (magit-apply-patch-text patch #t #f))
                 (message (string-append "Staged " (magit-group-display (grp-context g)))))))
          (else
           (let ((cur (magit-current)))
             (if cur
                 (magit-do (list "add" "--" (magit-newpath (cadr cur))) "Staged file")
                 (message "Nothing to stage here"))))))))

;; u: unstage the region (multiple files and/or selected lines), else hunk/file at point.
(define (magit-unstage)
  (if (region-active?)
      (let* ((r (magit-region-lines)) (lo (car r)) (hi (cdr r))
             (files (magit-dedup (magit-files-in lo hi "staged"))))
        (if (not (null? files))
            (magit-do (append (list "reset" "-q" "HEAD" "--")
                              (apply append (map magit-pathspecs files)))
                      (string-append "Unstaged " (int->string (length files)) " file(s)"))
            (let ((patch (magit-partial-multi (magit-hunks-in lo hi "staged") lo hi magit-xf-reverse)))
              (if patch
                  (when (magit-apply-patch-text patch #t #t) (message "Unstaged region"))
                  (message "Nothing to unstage in region")))))
      (let ((h (magit-hunk-at-line (buffer-line)))
            (g (and (magit-structural?) (magit-group-on-heading (buffer-line)))))
        (cond
          (h (when (magit-apply-hunk h #t #t) (message "Unstaged hunk")))
          (g (let ((patch (magit-group-patch g)))
               (when (and patch (magit-apply-patch-text patch #t #t))
                 (message (string-append "Unstaged " (magit-group-display (grp-context g)))))))
          (else
           (let ((cur (magit-current)))
             (if cur
                 (magit-do (append (list "reset" "-q" "HEAD" "--") (magit-pathspecs (cadr cur)))
                           "Unstaged file")
                 (message "Nothing to unstage here"))))))))

(define (magit-stage-all)   (magit-do (list "add" "-A") "Staged all changes"))
(define (magit-unstage-all)
  (magit-confirm "Unstage all changes?"
    (lambda () (magit-do (list "reset" "-q" "HEAD") "Unstaged everything"))))

;; Discard one side wholesale: untracked files via `clean`, tracked via `checkout`.
(define (magit-discard-files untracked unstaged)
  (when (not (null? untracked))
    (magit-do (append (list "clean" "-f" "--") (map magit-newpath untracked))
              "Removed untracked file(s)"))
  (when (not (null? unstaged))
    (magit-do (append (list "checkout" "HEAD" "--") (apply append (map magit-pathspecs unstaged)))
              "Discarded file(s)")))

;; k: discard the region (multiple files and/or selected lines), else hunk/file
;; at point. Discarding is irreversible (working-tree loss), so it always confirms.
(define (magit-discard)
  (if (region-active?)
      (let* ((r (magit-region-lines)) (lo (car r)) (hi (cdr r))
             (untracked (magit-dedup (magit-files-in lo hi "untracked")))
             (unstaged  (magit-dedup (magit-files-in lo hi "unstaged"))))
        (if (or (not (null? untracked)) (not (null? unstaged)))
            (magit-confirm (string-append "Discard "
                             (int->string (+ (length untracked) (length unstaged))) " file(s)?")
              (lambda () (magit-discard-files untracked unstaged)))
            (let ((patch (magit-partial-multi (magit-hunks-in lo hi "unstaged") lo hi magit-xf-reverse)))
              (if (not patch)
                  (message "Nothing to discard in region")
                  (magit-confirm "Discard region?"
                    (lambda () (when (magit-apply-patch-text patch #f #t) (message "Discarded region"))))))))
      (let ((h (magit-hunk-at-line (buffer-line)))
            (g (and (magit-structural?) (magit-group-on-heading (buffer-line)))))
        (cond
          (h (magit-confirm "Discard hunk?"
               (lambda () (when (magit-apply-hunk h #f #t) (message "Discarded hunk")))))
          (g (let ((patch (magit-group-patch g)))
               (when patch
                 (magit-confirm (string-append "Discard "
                                               (magit-group-display (grp-context g)) "?")
                   (lambda () (when (magit-apply-patch-text patch #f #t)
                                (message "Discarded group")))))))
          (else
           (let ((cur (magit-current)))
              (if (not cur)
                  (message "Nothing to discard here")
                  (let ((type (car cur)) (path (cadr cur)))
                    (magit-confirm (string-append "Discard changes to " path "?")
                      (lambda ()
                        (if (equal? type "untracked")
                            (magit-do (list "clean" "-f" "--" (magit-newpath path))
                                      (string-append "Removed " path))
                            (magit-do (append (list "checkout" "HEAD" "--") (magit-pathspecs path))
                                      (string-append "Discarded " path)))))))))))))

;; --- commit (c): an editable *git-commit* buffer ----------------------------
;; Switch rows toggle commit flags; action rows read them. "Amend HEAD" reuses
;; the last message; --signoff / --no-verify reach the final `git commit`.
(define (magit-commit-toggle-amend)    (magit-switch-toggle! "commit:amend")    (magit-commit-popup))
(define (magit-commit-toggle-signoff)  (magit-switch-toggle! "commit:signoff")  (magit-commit-popup))
(define (magit-commit-toggle-noverify) (magit-switch-toggle! "commit:noverify") (magit-commit-popup))
(define (magit-commit-popup)
  (transient "Commit"
    (list
      (list "Switches"
            (list "a" (magit-sw-label "Amend HEAD"        "commit:amend")    "magit-commit-toggle-amend")
            (list "s" (magit-sw-label "Sign off (-s)"     "commit:signoff")  "magit-commit-toggle-signoff")
            (list "n" (magit-sw-label "No verify (-n)"    "commit:noverify") "magit-commit-toggle-noverify"))
      (list "Actions"
            (list "c" "Commit"             "magit-do-commit")
            (list "e" "Amend (no edit)"    "magit-do-amend")
            (list "w" "Reword last commit" "magit-do-reword")))))
;; The extra `git commit` flags currently toggled on.
(define (magit-commit-flags)
  (magit-switch-args (list (cons "commit:signoff" "--signoff")
                           (cons "commit:noverify" "--no-verify"))))

;; Open the message buffer (empty for commit, last message for reword). `amend?`
;; is stashed in a buffer-local so C-c C-c knows which git command to run.
(define (magit-commit-buffer initial amend)
  (append-to-buffer "*git-commit*" initial)
  ;; Show the staged diff (commented out) below the message, like `git commit`.
  (append-to-buffer "*git-commit*"
    (string-append "\n\n" magit-commit-comment-intro
                   (magit-comment-diff (gix-diff-cached *magit-dir*))))
  (set-major-mode "git-commit-mode")
  (buffer-local-set "magit-amend" amend)
  (goto-char 0) ; cursor at the top, ready to type the summary
  (message "Edit message — C-x k or C-c C-c to commit, C-c C-k to cancel"))
;; Save unsaved file buffers first, so the commit captures on-disk content the
;; user actually sees — magit's save-some-buffers-before-commit guard.
(define (magit-do-commit)
  (save-some-buffers)
  (if (magit-switch-on? "commit:amend")
      (magit-commit-buffer (gix-head-message *magit-dir*) "yes")
      (magit-commit-buffer "" "no")))
(define (magit-do-reword) (magit-commit-buffer (gix-head-message *magit-dir*) "yes"))
(define (magit-do-amend)
  (save-some-buffers)
  (magit-do (append (list "commit" "--amend" "--no-edit") (magit-commit-flags)) "Amended HEAD")
  (magit-switch-clear! "commit:"))

(define (magit-commit-confirm)
  (let ((msg (magit-strip-comments (buffer-string)))
        (amend (equal? (buffer-local-get "magit-amend") "yes")))
    (if (equal? msg "")
        (message "Aborting: empty commit message")
        (let ((file (path-join *magit-dir* ".git/MACROS_EDITMSG")))
          (write-file file msg)
          (let ((r (call-process-full "git"
                     (append (list "-C" *magit-dir* "commit")
                             (if amend (list "--amend") '())
                             (magit-commit-flags)
                             (list "-F" file)) "")))
            (kill-buffer-named "*git-commit*")
            (magit-switch-clear! "commit:")
            (magit-render)
            (if (equal? (car r) "0")
                (message (if amend "Amended HEAD" "Committed"))
                (magit-error (nth r 2))))))))
(define (magit-commit-cancel)
  (kill-buffer-named "*git-commit*")
  (magit-switch-clear! "commit:")
  (magit-render)
  (message "Commit cancelled"))

;; Exiting the buffer (C-x k) finishes the commit, mirroring how a terminal
;; `git commit` commits when you save-and-close the editor: a non-empty message
;; commits, an empty one aborts (and closes the buffer either way).
(define (magit-commit-finish)
  (if (equal? (magit-strip-comments (buffer-string)) "")
      (magit-commit-cancel)
      (magit-commit-confirm)))
(define-key "git-commit-mode" "ctrl-c ctrl-c" "magit-commit-confirm")
(define-key "git-commit-mode" "ctrl-c ctrl-k" "magit-commit-cancel")
(define-key "git-commit-mode" "ctrl-x k"      "magit-commit-finish")

;; --- branch (b) -------------------------------------------------------------
(define (magit-branch-popup)
  (transient "Branch"
    (list (list "b" "Checkout"          "magit-checkout")
          (list "r" "Checkout remote"   "magit-checkout-remote")
          (list "c" "Create & checkout" "magit-branch-create")
          (list "u" "Set upstream"      "magit-branch-set-upstream")
          (list "m" "Rename current"    "magit-branch-rename")
          (list "k" "Delete"            "magit-branch-delete"))))
(define (magit-remote-branches) (gix-remote-branches *magit-dir*))
;; Check out a remote branch as a new local tracking branch ("origin/x" → "x").
(define (magit-checkout-remote)
  (helm (list (list "Checkout remote" (magit-remote-branches) "magit-do-checkout-remote"))))
(define (magit-do-checkout-remote rb)
  (let ((local (car (reverse (split-many rb "/")))))
    (magit-do (list "checkout" "-b" local "--track" rb) (string-append "Checked out " local))))
(define (magit-branch-set-upstream)
  (helm (list (list "Set upstream to" (magit-remote-branches) "magit-do-set-upstream"))))
(define (magit-do-set-upstream u) (magit-do (list "branch" "--set-upstream-to" u) (string-append "Upstream → " u)))
(define (magit-checkout)
  (helm (list (list "Checkout" (magit-branches) "magit-do-checkout"))))
(define (magit-do-checkout b) (magit-do (list "checkout" b) (string-append "Checked out " b)))
(define (magit-branch-create) (read-string "Create & checkout branch: " "magit-do-branch-create"))
(define (magit-do-branch-create name)
  (when (not (equal? (trim-end name) ""))
    (magit-do (list "checkout" "-b" name) (string-append "Created " name))))
(define (magit-branch-rename) (read-string "Rename current branch to: " "magit-do-branch-rename"))
(define (magit-do-branch-rename name)
  (when (not (equal? (trim-end name) ""))
    (magit-do (list "branch" "-m" name) (string-append "Renamed to " name))))
(define (magit-branch-delete)
  (helm (list (list "Delete branch" (magit-branches) "magit-do-branch-delete"))))
(define (magit-do-branch-delete b) (magit-do (list "branch" "-d" b) (string-append "Deleted " b)))

;; --- push / pull / fetch (P / F / f) ----------------------------------------
(define (magit-push-toggle-force)    (magit-switch-toggle! "push:force")    (magit-push-popup))
(define (magit-push-toggle-noverify) (magit-switch-toggle! "push:noverify") (magit-push-popup))
(define (magit-push-flags)
  (magit-switch-args (list (cons "push:force" "--force-with-lease")
                           (cons "push:noverify" "--no-verify"))))
(define (magit-push-popup)
  (transient "Push"
    (list
      (list "Switches"
            (list "f" (magit-sw-label "Force with lease" "push:force")    "magit-push-toggle-force")
            (list "n" (magit-sw-label "No verify"        "push:noverify") "magit-push-toggle-noverify"))
      (list "Actions"
            (list "p" "Push"                "magit-push")
            (list "u" "Push & set upstream" "magit-push-upstream")))))
(define (magit-push)
  (magit-run-async (append (list "push") (magit-push-flags)) "Pushed")
  (magit-switch-clear! "push:"))
(define (magit-push-upstream)
  (magit-run-async (append (list "push" "-u" "origin" "HEAD") (magit-push-flags)) "Pushed, upstream set")
  (magit-switch-clear! "push:"))

(define (magit-pull-toggle-rebase) (magit-switch-toggle! "pull:rebase") (magit-pull-popup))
(define (magit-pull-popup)
  (transient "Pull"
    (list
      (list "Switches"
            (list "r" (magit-sw-label "Rebase (--rebase)" "pull:rebase") "magit-pull-toggle-rebase"))
      (list "Actions"
            (list "p" "Pull" "magit-pull")))))
(define (magit-pull)
  (magit-run-async (append (list "pull")
                           (magit-switch-args (list (cons "pull:rebase" "--rebase")))) "Pulled")
  (magit-switch-clear! "pull:"))

(define (magit-fetch-toggle-prune) (magit-switch-toggle! "fetch:prune") (magit-fetch-popup))
(define (magit-fetch-flags) (magit-switch-args (list (cons "fetch:prune" "--prune"))))
(define (magit-fetch-popup)
  (transient "Fetch"
    (list
      (list "Switches"
            (list "p" (magit-sw-label "Prune (--prune)" "fetch:prune") "magit-fetch-toggle-prune"))
      (list "Actions"
            (list "a" "Fetch all"    "magit-fetch-all")
            (list "o" "Fetch origin" "magit-fetch-origin")))))
(define (magit-fetch-all)
  (magit-run-async (append (list "fetch" "--all") (magit-fetch-flags)) "Fetched all remotes")
  (magit-switch-clear! "fetch:"))
(define (magit-fetch-origin)
  (magit-run-async (append (list "fetch" "origin") (magit-fetch-flags)) "Fetched origin")
  (magit-switch-clear! "fetch:"))

;; --- stash (z) --------------------------------------------------------------
(define (magit-stash-popup)
  (transient "Stash"
    (list (list "z" "Stash"            "magit-stash")
          (list "m" "Stash w/ message" "magit-stash-push-msg")
          (list "p" "Pop latest"       "magit-stash-pop")
          (list "shift-p" "Pop a stash…" "magit-stash-pick")
          (list "a" "Apply latest"     "magit-stash-apply")
          (list "k" "Drop latest"      "magit-stash-drop")
          (list "l" "List"             "magit-stash-list"))))
(define (magit-stash-push-msg) (read-string "Stash message: " "magit-do-stash-msg"))
(define (magit-do-stash-msg m)
  (when (not (equal? (trim-end m) "")) (magit-do (list "stash" "push" "-m" m) "Stashed")))
(define (magit-stash-refs) (git-lines (list "stash" "list" "--format=%gd")))
(define (magit-stash-pick)
  (helm (list (list "Pop stash" (magit-stash-refs) "magit-do-stash-pop-ref"))))
(define (magit-do-stash-pop-ref r) (magit-do (list "stash" "pop" r) (string-append "Popped " r)))
(define (magit-stash)       (magit-do (list "stash" "push") "Stashed"))
(define (magit-stash-pop)   (magit-do (list "stash" "pop") "Popped stash"))
(define (magit-stash-apply) (magit-do (list "stash" "apply") "Applied stash"))
(define (magit-stash-drop)
  (magit-confirm "Drop latest stash?"
    (lambda () (magit-do (list "stash" "drop") "Dropped stash"))))
(define (magit-stash-list)
  (magit-show-log "*magit-stash*" (string-join (gix-stash-list *magit-dir*) "\n")))

;; --- log (l) → a read-only *magit-log* buffer -------------------------------
(define (magit-log-toggle-graph) (magit-switch-toggle! "log:graph") (magit-log-popup))
(define (magit-log-popup)
  (magit-ensure-dir)            ; reachable via M-x from a file buffer
  (transient "Log"
    (list
      (list "Switches"
            (list "g" (magit-sw-label "Graph (--graph)" "log:graph") "magit-log-toggle-graph"))
      (list "Actions"
            (list "l" "Current branch" "magit-log-current")
            (list "a" "All branches"   "magit-log-all")
            (list "f" "File at point"  "magit-log-file")))))
(define (magit-log-flags) (magit-switch-args (list (cons "log:graph" "--graph"))))
;; gix renders the oneline+decorate log (no subprocess); the `--graph` toggle
;; falls back to git, whose ASCII commit-graph art gix can't reproduce.
(define (magit-log-current)
  (magit-show-log "*magit-log*"
                  (if (magit-switch-on? "log:graph")
                      (git (append (list "log" "--oneline" "--decorate" "--graph")
                                   (list "-n" "200")))
                      (string-join (gix-log-decorated *magit-dir* #f 200) "\n"))))
(define (magit-log-all)
  (magit-show-log "*magit-log*"
                  (if (magit-switch-on? "log:graph")
                      (git (append (list "log" "--oneline" "--decorate" "--all" "--graph")
                                   (list "-n" "200")))
                      (string-join (gix-log-decorated *magit-dir* #t 200) "\n"))))
(define (magit-show-log name text)
  (render-buffer name "magit-log-mode"
                 (if (equal? (trim-end text) "") "No output." (trim-end text)) '()))
;; The short hash on the current log line, or "". The first whitespace-separated
;; token of length >= 7 — which skips any leading `--graph` glyphs (* | / \, all
;; single chars) and lands on the abbreviated hash.
(define (magit-log-hash)
  (let* ((b (line-bounds))
         (line (buffer-substring (car b) (cadr b)))
         (parts (filter (lambda (s) (>= (string-length s) 7)) (split-many line " "))))
    (if (null? parts) "" (car parts))))

;; RET in a log buffer: show the commit whose short hash starts the line, with
;; the same colorized diff as the status buffer (see magit-show-commit).
(define (magit-log-show)
  (let ((hash (magit-log-hash)))
    (when (not (equal? hash ""))
      (magit-show-commit hash))))

;; A (cherry-pick / revert) onto the commit at point in a log buffer.
(define (magit-log-cherry-pick)
  (let ((h (magit-log-hash)))
    (when (not (equal? h ""))
      (magit-do (list "cherry-pick" h) (string-append "Cherry-picked " h)))))
(define (magit-log-revert)
  (let ((h (magit-log-hash)))
    (when (not (equal? h ""))
      (magit-confirm (string-append "Revert " h "?")
        (lambda () (magit-do (list "revert" "--no-edit" h) (string-append "Reverted " h)))))))

;; Blame / log the file at point in the status buffer (read-only views).
(define (magit-blame)
  (let ((cur (magit-current)))
    (if cur
        (render-buffer "*magit-blame*" "magit-log-mode"
                       (git (list "blame" "--date=short" (magit-newpath (cadr cur)))) '())
        (message "No file at point"))))
(define (magit-log-file)
  (let ((cur (magit-current)))
    (if cur
        (magit-show-log "*magit-log*"
                        (string-join
                          (gix-log-file *magit-dir* (magit-newpath (cadr cur)) 200) "\n"))
        (message "No file at point"))))

;; --- reset (X) --------------------------------------------------------------
(define (magit-reset-popup)
  (transient "Reset"
    (list (list "m" "Mixed — unstage, keep changes"   "magit-reset-mixed")
          (list "s" "Soft — undo last commit, keep"   "magit-reset-soft")
          (list "h" "Hard — DISCARD working changes"  "magit-reset-hard"))))
(define (magit-reset-mixed) (magit-do (list "reset" "--mixed" "HEAD") "Reset (mixed)"))
(define (magit-reset-soft)  (magit-do (list "reset" "--soft" "HEAD~1") "Undid last commit (soft)"))
(define (magit-reset-hard)
  (magit-confirm "Hard reset to HEAD — discard ALL working changes?"
    (lambda () (magit-do (list "reset" "--hard" "HEAD") "Reset --hard HEAD"))))

;; --- remote (M) -------------------------------------------------------------
(define *magit-remote-name* "")
(define (magit-remote-popup)
  (transient "Remote"
    (list (list "a" "Add"    "magit-remote-add")
          (list "k" "Remove" "magit-remote-remove"))))
(define (magit-remote-add) (read-string "Remote name: " "magit-remote-add-name"))
(define (magit-remote-add-name name)
  (set! *magit-remote-name* name)
  (read-string "Remote URL: " "magit-remote-add-url"))
(define (magit-remote-add-url url)
  (magit-do (list "remote" "add" *magit-remote-name* url) "Remote added"))
(define (magit-remote-remove)
  (helm (list (list "Remove remote" (magit-remotes) "magit-do-remote-remove"))))
(define (magit-do-remote-remove r) (magit-do (list "remote" "remove" r) "Remote removed"))

;; --- tag (t) ----------------------------------------------------------------
(define (magit-tag-popup)
  (transient "Tag"
    (list (list "t" "Create" "magit-tag-create")
          (list "k" "Delete" "magit-tag-delete"))))
(define (magit-tag-create) (read-string "Tag name: " "magit-do-tag-create"))
(define (magit-do-tag-create name)
  (when (not (equal? (trim-end name) ""))
    (magit-do (list "tag" name) (string-append "Tagged " name))))
(define (magit-tag-delete)
  (helm (list (list "Delete tag" (magit-tags) "magit-do-tag-delete"))))
(define (magit-do-tag-delete t) (magit-do (list "tag" "-d" t) (string-append "Deleted tag " t)))

;; --- merge (m) --------------------------------------------------------------
(define (magit-merge-popup)
  (transient "Merge"
    (list (list "m" "Merge branch"               "magit-merge")
          (list "o" "Take ours (file at point)"  "magit-take-ours")
          (list "t" "Take theirs (file at point)" "magit-take-theirs")
          (list "a" "Abort"                      "magit-merge-abort"))))
(define (magit-merge)
  (helm (list (list "Merge branch" (magit-branches) "magit-do-merge"))))
(define (magit-do-merge b) (magit-do (list "merge" b) (string-append "Merged " b)))
(define (magit-merge-abort) (magit-do (list "merge" "--abort") "Merge aborted"))

;; --- dispatch (?) : the top-level menu --------------------------------------
(define (magit-dispatch)
  (magit-ensure-dir)            ; reachable via M-x from a file buffer
  ;; Grouped (multi-column) transient: each inner list is a titled column.
  (transient "Magit"
    (list
      (list "Changes"
            (list "c"       "Commit" "magit-commit-popup")
            (list "z"       "Stash"  "magit-stash-popup")
            (list "shift-x" "Reset"  "magit-reset-popup")
            (list "shift-h" "History" "magit-toggle-history")
            (list "g"       "Refresh" "magit-refresh"))
      (list "Branch"
            (list "b"       "Branch" "magit-branch-popup")
            (list "m"       "Merge"  "magit-merge-popup")
            (list "r"       "Rebase" "magit-rebase-popup")
            (list "t"       "Tag"    "magit-tag-popup")
            (list "l"       "Log"    "magit-log-popup"))
      (list "Remote"
            (list "shift-p" "Push"   "magit-push-popup")
            (list "shift-f" "Pull"   "magit-pull-popup")
            (list "f"       "Fetch"  "magit-fetch-popup")
            (list "shift-m" "Remote" "magit-remote-popup")))))

;; --- TAB / RET (sections) ---------------------------------------------------
;; TAB: expand/collapse the section at point. Sections are collapsed by default,
;; so this toggles the file's membership in the explicitly-expanded set, then
;; just re-applies folds/accents to the EXISTING buffer — no git, no re-render
;; (the text and section data are unchanged, only which sections are folded).
(define (magit-toggle-fold)
  ;; `magit-apply-overlays` unfolds everything then re-folds, which churns the
  ;; scroll; pin the cursor's screen row so the buffer doesn't jump. Point stays
  ;; on the heading (byte unchanged), so its final row equals the captured one.
  (let ((row (window-cursor-row)))
    (let ((g (and (magit-structural?) (magit-group-at-line (buffer-line)))))
      (if g
          ;; Structural view, point in a function group → fold just that function.
          (let ((key (grp-key g)))
            (set! *magit-collapsed-groups*
                  (if (member key *magit-collapsed-groups*)
                      (filter (lambda (k) (not (equal? k key))) *magit-collapsed-groups*)
                      (cons key *magit-collapsed-groups*)))
            (magit-apply-overlays))
          ;; Otherwise (file heading, or line view) → fold the whole file section.
          (let ((s (magit-section-at-line (buffer-line))))
            (when s
              (let ((key (sec-key s)))
                (set! *magit-expanded*
                      (if (member key *magit-expanded*)
                          (filter (lambda (k) (not (equal? k key))) *magit-expanded*)
                          (cons key *magit-expanded*)))
                (magit-apply-overlays))))))
    (scroll-cursor-to-row row)))

;; The new-file starting line ("c" in "@@ -a,b +c,d @@") of a hunk header.
(define (magit-hunk-new-start header)
  (let loop ((parts (split-many header " ")))
    (cond ((null? parts) 1)
          ((starts-with? (car parts) "+")
           (or (string->number (car (split-many
                                      (substring (car parts) 1 (string-length (car parts))) ",")))
               1))
          (else (loop (cdr parts))))))

;; Work-tree line number for the diff line `line` inside hunk `h`: the hunk's
;; new-file start plus the count of context/added lines above `line` (removed
;; lines have no new-file counterpart, so they don't advance the count).
(define (magit-hunk-target-line h line)
  (let* ((idx    (- line (nth h 0)))            ; offset into the hunk (0 = @@)
         (hlines (split-many (nth h 3) "\n"))
         (c      (magit-hunk-new-start (car hlines))))
    (let loop ((i 1) (k 0))
      (if (or (>= i idx) (>= i (length hlines)))
          (+ c k)
          (let ((l (list-ref hlines i)))
            (loop (+ i 1)
                  (if (or (starts-with? l "+") (starts-with? l " ")) (+ k 1) k)))))))

;; Whole-line face for a `git show` line. `in-diff` is #t once we've passed the
;; first "diff --git"; before that we're in the commit metadata/message, where
;; only Author/Date/Merge get a dim header tone (the subject/body stay default,
;; the hash is accented separately). In the diff body we reuse the status
;; buffer's added/removed/hunk/context faces.
(define (magit-show-commit-face l in-diff)
  (if (not in-diff)
      (cond ((starts-with? l "Author:") "magit-header")
            ((starts-with? l "Date:")   "magit-header")
            ((starts-with? l "Merge:")  "magit-header")
            ((starts-with? l "Commit:") "magit-header")
            (else #f))                                  ; "commit <hash>", subject, body
      (cond ((starts-with? l "diff --git") "magit-file")
            ((starts-with? l "index ")     "magit-context")
            ((starts-with? l "--- ")       "magit-context")
            ((starts-with? l "+++ ")       "magit-context")
            ((starts-with? l "old mode")   "magit-context")
            ((starts-with? l "new mode")   "magit-context")
            ((starts-with? l "new file")   "magit-context")
            ((starts-with? l "deleted file") "magit-context")
            ((starts-with? l "rename ")    "magit-context")
            ((starts-with? l "similarity") "magit-context")
            ((starts-with? l "@@")         "magit-hunk")
            ((starts-with? l "+")          "magit-added")
            ((starts-with? l "-")          "magit-removed")
            (else                          "magit-context"))))  ; context (leading space)

;; Render `git show <ref>` into *magit-commit* (magit-log-mode) with the same
;; colorization as the status diffs: green/red bodies, accented +/- markers, an
;; accented commit hash. Builds local accumulators (NOT the status buffer's
;; render globals) so returning to *magit* leaves its navigation state intact.
(define (magit-show-commit ref)
  (let* ((raw   (trim-end (gix-show *magit-dir* ref)))
         (lines (split-many raw "\n")))
    (let loop ((ls lines) (n 0) (byte 0) (in-diff #f) (faces '()) (accents '()))
      (if (null? ls)
          (begin
            (render-buffer "*magit-commit*" "magit-log-mode" raw (reverse faces))
            (clear-overlays)
            (for-each (lambda (a) (overlay-set-face (nth a 0) (nth a 1) (nth a 2)))
                      (reverse accents)))
          (let* ((l    (car ls))
                 (ind  (or in-diff (starts-with? l "diff --git")))
                 (face (magit-show-commit-face l ind))
                 (faces2 (if face (cons (list n face) faces) faces))
                 ;; Accent the hash on the "commit <hash>" metadata line.
                 (accents2
                  (if (starts-with? l "commit ")
                      (let ((hl (utf8-length
                                 (car (split-many (substring l 7 (string-length l)) " ")))))
                        (cons (list (+ byte 7) (+ byte 7 hl) "magit-hash") accents))
                      accents))
                 ;; Brighten the +/- marker on diff body lines (skip +++/--- headers).
                 (accents3
                  (cond ((not ind) accents2)
                        ((or (starts-with? l "+++ ") (starts-with? l "--- ")) accents2)
                        ((starts-with? l "+")
                         (cons (list byte (+ byte 1) "magit-add-marker") accents2))
                        ((starts-with? l "-")
                         (cons (list byte (+ byte 1) "magit-remove-marker") accents2))
                        (else accents2))))
            (loop (cdr ls) (+ n 1) (+ byte (utf8-length l) 1) ind faces2 accents3))))))

;; RET: on a commit/stash line, show it; otherwise visit the file at point in a
;; split — landing on the exact line when point is inside a diff hunk.
(define (magit-visit-file)
  (let ((ref (magit-commit-at-line (buffer-line))))
    (if ref
        (magit-show-commit ref)
        (let ((s (magit-section-at-line (buffer-line))))
          (when s
            (let* ((h (magit-hunk-at-line (buffer-line)))
                   (target (if h (magit-hunk-target-line h (buffer-line)) 0)))
              (split-window-right)
              (other-window)
              (open-file (path-join *magit-dir* (magit-newpath (sec-path s))))
              (when (> target 0) (goto-line (- target 1)))))))))

;; --- inline blame (git-blame as virtual line suffixes) ----------------------
;; GitLens-style annotations on the END of each line, built on `set-line-suffix`.
;; Two modes share one parsed cache: full-file (`magit-blame-toggle`, every line)
;; and current-line (`magit-blame-line-toggle`, only point's line, follows the
;; cursor via post-command-hook). Each annotation shows short-hash, author, a
;; humanized date, and the commit summary, tinted by age.

;; Age-tinted faces for full-file mode: recent edits pop, ancient code recedes.
(set-face "magit-blame-recent" "#a6e22e")  ; < ~1 month
(set-face "magit-blame-mid"    "#b58900")  ; < ~1 year
(set-face "magit-blame-old"    "#6a6a6a")  ; older / dim
;; Current-line mode stays a quiet, uniform grey — it's a single annotation that
;; should sit behind the code, not a heat-map to scan.
(set-face "magit-blame-line"   "#5c5c5c")

;; `git blame --line-porcelain` repeats a full header block per line, so each
;; record is unambiguous: a 40-hex SHA header line, key/value lines, then the
;; source line (prefixed with a tab). Parse into `(final-line sha author epoch
;; summary)` records, in file order.
(define (magit-blame-hex40? line)
  (and (>= (string-length line) 41)
       (equal? (substring line 40 41) " ")
       (let loop ((i 0))
         (cond ((>= i 40) #t)
               ((string-contains? "0123456789abcdef" (substring line i (+ i 1)))
                (loop (+ i 1)))
               (else #f)))))
(define (magit-blame-parse lines)
  (let loop ((ls lines) (sha "") (author "") (epoch 0) (summary "") (fline 0) (acc '()))
    (cond
      ((null? ls) (reverse acc))
      (else
        (let ((line (car ls)))
          (cond
            ;; The tab-prefixed content line ends the current record.
            ((and (> (string-length line) 0) (equal? (substring line 0 1) "\t"))
             (loop (cdr ls) "" "" 0 "" 0
                   (cons (list fline sha author epoch summary) acc)))
            ((starts-with? line "author ")
             (loop (cdr ls) sha (substring line 7 (string-length line)) epoch summary fline acc))
            ((starts-with? line "author-time ")
             (loop (cdr ls) sha author
                   (or (string->number (substring line 12 (string-length line))) 0)
                   summary fline acc))
            ((starts-with? line "summary ")
             (loop (cdr ls) sha author epoch (substring line 8 (string-length line)) fline acc))
            ((magit-blame-hex40? line)             ; "<sha> <orig> <final> [<n>]"
             (let ((toks (split-many line " ")))
               (loop (cdr ls) (list-ref toks 0) author epoch summary
                     (or (string->number (list-ref toks 2)) 0) acc)))
            (else (loop (cdr ls) sha author epoch summary fline acc))))))))

;; Age from the commit's author-time epoch — `current-time "%s"` is now in epoch
;; seconds, so no calendar arithmetic is needed.
(define (magit-blame-age-seconds epoch)
  (- (or (string->number (current-time "%s")) 0) epoch))
(define *magit-blame-day* 86400)
(define (magit-blame-humanize epoch)
  (let ((age (magit-blame-age-seconds epoch)) (day *magit-blame-day*))
    (cond ((< age 0) "in the future")
          ((< age day) "today")
          ((< age (* 2 day)) "yesterday")
          ((< age (* 7 day))   (string-append (number->string (quotient age day)) " days ago"))
          ((< age (* 30 day))  (string-append (number->string (quotient age (* 7 day))) " weeks ago"))
          ((< age (* 365 day)) (string-append (number->string (quotient age (* 30 day))) " months ago"))
          (else (string-append (number->string (quotient age (* 365 day))) " years ago")))))
(define (magit-blame-age-face epoch)
  (let ((age (magit-blame-age-seconds epoch)) (day *magit-blame-day*))
    (cond ((< age (* 30 day)) "magit-blame-recent")
          ((< age (* 365 day)) "magit-blame-mid")
          (else "magit-blame-old"))))

;; Record accessors + suffix rendering. rec = (final-line sha author epoch summary).
(define (magit-blame-rec-line rec) (list-ref rec 0))
(define (magit-blame-rec-sha rec)  (list-ref rec 1))
(define (magit-blame-uncommitted? rec)
  (let ((sha (magit-blame-rec-sha rec)))
    (equal? (substring sha 0 (min 8 (string-length sha))) "00000000")))
(define (magit-blame-trim s)          ; keep summaries from overflowing the line
  (if (> (string-length s) 50)
      (string-append (substring s 0 50) "…")
      s))
(define (magit-blame-rec-suffix rec)
  (if (magit-blame-uncommitted? rec)
      "  Not committed yet"
      (string-append "  " (substring (magit-blame-rec-sha rec) 0 8)
                     "  " (list-ref rec 2)
                     ", " (magit-blame-humanize (list-ref rec 3))
                     " · " (magit-blame-trim (list-ref rec 4)))))
(define (magit-blame-rec-face rec)
  (if (magit-blame-uncommitted? rec) "magit-context" (magit-blame-age-face (list-ref rec 3))))

;; Parsed records for the current buffer, cached by line so current-line mode and
;; the at-point actions are O(1) per cursor move (no re-blame). Invalidated when
;; the active buffer changes.
(define *magit-blame-cache* (hash))
(define *magit-blame-buf* "")
;; gix-blame rows ("LINE\tSHA\tAUTHOR\tEPOCH\tSUMMARY") → the same
;; `(final-line sha author epoch summary)` records magit-blame-parse produces, so
;; all downstream rendering is shared.
(define (magit-blame-gix-records dir file)
  (map (lambda (row)
         (let ((f (split-many row "\t")))
           (list (or (string->number (list-ref f 0)) 0)   ; final line
                 (list-ref f 1)                            ; sha
                 (list-ref f 2)                            ; author
                 (or (string->number (list-ref f 3)) 0)   ; author-time epoch
                 (list-ref f 4))))                         ; summary
       (gix-blame dir file)))
;; Prefer gix (no subprocess). It returns rows only when the file matches HEAD
;; byte-for-byte, so a non-empty result is guaranteed to line up. For a modified
;; file gix yields nothing and we fall back to `git blame --line-porcelain`,
;; which blames the working tree and flags uncommitted lines.
(define (magit-blame-fetch dir file)
  (let ((recs (magit-blame-gix-records dir file)))
    (if (null? recs)
        (magit-blame-parse
          (split-many (call-process "git" (list "-C" dir "blame" "--line-porcelain" "--" file)) "\n"))
        recs)))
(define (magit-blame-store file recs)
  (set! *magit-blame-buf* file)
  (set! *magit-blame-cache* (hash))
  (for-each (lambda (rec)
              (set! *magit-blame-cache*
                    (hash-insert *magit-blame-cache* (magit-blame-rec-line rec) rec)))
            recs))
(define (magit-blame-ensure)
  (when (not (equal? (current-buffer) *magit-blame-buf*))
    (magit-blame-store (current-buffer) (magit-blame-fetch (buffer-directory) (current-buffer))))
  *magit-blame-cache*)
(define (magit-blame-rec-at line)     ; line is 1-based (git's final line)
  (hash-try-get (magit-blame-ensure) line))

;; --- full-file mode ---------------------------------------------------------
;; Annotate only the FIRST line of each run of consecutive lines from the same
;; commit, so a block authored in one commit reads cleanly instead of repeating
;; the same hash/date on every line.
(define *magit-blame-on* #f)
(define (magit-blame-annotate)
  (let* ((dir (buffer-directory)) (file (current-buffer))
         (recs (magit-blame-fetch dir file)))
    (if (null? recs)
        (message "git blame: nothing to annotate")
        (begin
          (magit-blame-store file recs)
          (let loop ((rs recs) (prev ""))
            (when (not (null? rs))
              (let* ((rec (car rs)) (sha (magit-blame-rec-sha rec)))
                (when (not (equal? sha prev))
                  (set-line-suffix (- (magit-blame-rec-line rec) 1)
                                   (magit-blame-rec-suffix rec) (magit-blame-rec-face rec)))
                (loop (cdr rs) sha))))
          (set! *magit-blame-on* #t)
          (message "Blame on — C-x v b toggles off")))))

;; --- current-line mode (off by default) -------------------------------------
;; Show blame only on point's line, re-placing it as the cursor moves. Driven by
;; `post-command-hook`, which fires (once per cursor-line change) only while the
;; hook is registered — so there's zero cost unless this mode is on.
(define *magit-blame-line-on* #f)
(define *magit-blame-line-last* -1)
(define (magit-blame-line-refresh)
  ;; Skip special buffers (`*magit*`, `*Help*`, …): they visit no file, so
  ;; blaming them just spawns a failing git on every cursor move.
  (when (and *magit-blame-line-on* (not (starts-with? (current-buffer) "*")))
    (let ((ln (+ (buffer-line) 1)))          ; buffer-line is 0-based; cache is 1-based
      (when (not (equal? ln *magit-blame-line-last*))
        (set! *magit-blame-line-last* ln)
        (clear-line-suffixes)
        (let ((rec (magit-blame-rec-at ln)))
          (when rec
            (set-line-suffix (- ln 1) (magit-blame-rec-suffix rec) "magit-blame-line")))))))

;; --- shared teardown + toggles ----------------------------------------------
(define (magit-blame-off)
  (clear-line-suffixes)
  (set! *magit-blame-on* #f)
  (when *magit-blame-line-on*
    (set! *magit-blame-line-on* #f)
    (set! *magit-blame-line-last* -1)
    (remove-hook "post-command-hook" "magit-blame-line-refresh")))
(define (magit-blame-toggle)
  (if *magit-blame-on*
      (begin (magit-blame-off) (message "Blame off"))
      (begin (magit-blame-off) (magit-blame-annotate))))
(define (magit-blame-line-toggle)
  (if *magit-blame-line-on*
      (begin (magit-blame-off) (message "Blame (current line) off"))
      (begin
        (magit-blame-off)                    ; full-file and line modes are exclusive
        (set! *magit-blame-line-on* #t)
        (add-hook "post-command-hook" "magit-blame-line-refresh")
        (magit-blame-line-refresh)
        (message "Blame (current line) on — follows point"))))

;; --- at-point actions (work in either mode) ---------------------------------
(define (magit-blame-show)              ; show the commit that last touched this line
  (let ((rec (magit-blame-rec-at (+ (buffer-line) 1))))
    (cond ((not rec) (message "No blame for this line"))
          ((magit-blame-uncommitted? rec) (message "Line not committed yet"))
          (else
           ;; `magit-show-commit` runs `git show` via `*magit-dir*`, which is only
           ;; set once magit-status has opened. Inline blame can run with no status
           ;; buffer, so point git at THIS buffer's repo for the call, then restore
           ;; so a status buffer for another repo keeps its directory.
           (let ((saved *magit-dir*))
             (set! *magit-dir* (buffer-directory))
             (magit-show-commit (magit-blame-rec-sha rec))
             (set! *magit-dir* saved))))))
(define (magit-blame-copy)              ; copy the commit hash to the clipboard
  (let ((rec (magit-blame-rec-at (+ (buffer-line) 1))))
    (cond ((not rec) (message "No blame for this line"))
          ((magit-blame-uncommitted? rec) (message "Line not committed yet"))
          (else (let ((sha (magit-blame-rec-sha rec)))
                  (clipboard-set sha)
                  (message (string-append "Copied " (substring sha 0 8))))))))

;; --- section navigation + visibility cycling --------------------------------
;; Navigation targets: every file-section heading + every commit/stash line.
(define (magit-nav-lines)
  (append (map sec-heading *magit-sections*)
          ;; In the structural view, function sub-headings are nav stops too, so
          ;; M-n / M-p step function by function within a file.
          (if (magit-structural?) (map grp-heading *magit-groups*) '())
          (map car *magit-commit-lines*)))
(define (magit-next-section)
  (let loop ((hs (magit-nav-lines)) (cur (buffer-line)) (best #f))
    (cond ((null? hs) (when best (goto-line best)))
          ((and (> (car hs) cur) (or (not best) (< (car hs) best)))
           (loop (cdr hs) cur (car hs)))
          (else (loop (cdr hs) cur best)))))
(define (magit-prev-section)
  (let loop ((hs (magit-nav-lines)) (cur (buffer-line)) (best #f))
    (cond ((null? hs) (when best (goto-line best)))
          ((and (< (car hs) cur) (or (not best) (> (car hs) best)))
           (loop (cdr hs) cur (car hs)))
          (else (loop (cdr hs) cur best)))))
;; S-TAB: collapse-all when anything is expanded, else expand-all (magit cycle).
(define (magit-cycle-all)
  (let ((row (window-cursor-row)))
    (set! *magit-expanded*
          (if (null? *magit-expanded*) (map sec-key *magit-sections*) '()))
    (magit-apply-overlays)
    (scroll-cursor-to-row row)))

;; v: switch between the line view and the structural (group-by-function) view.
;; A full re-render is needed — the two lay the diff out differently.
(define (magit-toggle-view)
  (set! *magit-diff-view* (if (magit-structural?) "line" "structural"))
  (magit-refresh)
  (message (if (magit-structural?) "Structural diff view" "Line diff view")))

;; --- process buffer ($) + opportunistic refresh -----------------------------
(define (magit-process-buffer)
  (if (member "*magit-process*" (buffer-names))
      (switch-to-buffer "*magit-process*")
      (render-buffer "*magit-process*" "magit-log-mode" "(no git process output yet)" '())))
;; Refresh the status whenever *magit* is open — including while you're editing
;; and saving a file in another window. The render is quiet (no focus steal, in
;; place) since the save happens from a different buffer. Cheap now that the
;; whole render is gix-backed (no subprocess per refresh).
(define (magit-maybe-refresh)
  (when (member "*magit*" (buffer-names))
    (set! *magit-quiet* #t)
    (magit-render)
    (set! *magit-quiet* #f)))
(add-hook "after-save-hook" "magit-maybe-refresh")

;; Live refresh from the repo watch (watch-tree in magit-render). Fires once per
;; debounced change batch with the watched repo root. Render fully (overlays) when
;; *magit* is in front, quietly (in place, no overlays) when it's in a background
;; window — overlays target the active buffer, so a background refresh must skip
;; them. Only reacts to our own repo (the hook also fires for dired/treemacs).
(define (magit-fs-strip-slash p)
  (if (ends-with? p "/") (substring p 0 (- (string-length p) 1)) p))
(define (magit-on-fs-change dir)
  (when (and (member "*magit*" (buffer-names))
             (equal? (magit-fs-strip-slash dir) (magit-fs-strip-slash *magit-dir*)))
    (if (equal? (current-buffer) "*magit*")
        (magit-render)
        (begin (set! *magit-quiet* #t) (magit-render) (set! *magit-quiet* #f)))))
(add-hook "directory-changed-hook" "magit-on-fs-change")

;; Stop watching the repo when the status buffer is closed.
(define (magit-quit)
  (watch-tree "magit" '())
  (kill-buffer))

;; --- copy (y) + gitignore (i) -----------------------------------------------
(define (magit-copy)
  (let ((ref (magit-commit-at-line (buffer-line))))
    (if ref
        (begin (clipboard-set ref) (message (string-append "Copied " ref)))
        (let ((cur (magit-current)))
          (if cur
              (let ((p (magit-newpath (cadr cur))))
                (clipboard-set p) (message (string-append "Copied " p)))
              (message "Nothing to copy here"))))))
(define (magit-gitignore)
  (let ((cur (magit-current)))
    (if (not cur)
        (message "No file at point")
        (let* ((path (magit-newpath (cadr cur)))
               (gi (path-join *magit-dir* ".gitignore"))
               (existing (read-file gi))
               (sep (if (or (equal? existing "") (ends-with? existing "\n")) "" "\n")))
          (write-file gi (string-append existing sep path "\n"))
          (magit-refresh)
          (message (string-append "Added " path " to .gitignore"))))))

;; --- rebase (r) -------------------------------------------------------------
;; Non-interactive rebase + the in-progress controls. (Interactive todo editing
;; — squash/fixup/reorder — needs a sequence-editor callback the process layer
;; doesn't expose yet, so it's intentionally omitted.)
(define (magit-rebase-popup)
  (transient "Rebase"
    (list (list "e" "Rebase onto…" "magit-rebase-onto")
          (list "c" "Continue"     "magit-rebase-continue")
          (list "s" "Skip"         "magit-rebase-skip")
          (list "a" "Abort"        "magit-rebase-abort"))))
(define (magit-rebase-onto)
  (helm (list (list "Rebase onto" (magit-branches) "magit-do-rebase-onto"))))
(define (magit-do-rebase-onto b) (magit-do (list "rebase" b) (string-append "Rebased onto " b)))
(define (magit-rebase-continue) (magit-do (list "rebase" "--continue") "Rebase continued"))
(define (magit-rebase-skip)     (magit-do (list "rebase" "--skip") "Skipped commit"))
(define (magit-rebase-abort)
  (magit-confirm "Abort rebase?" (lambda () (magit-do (list "rebase" "--abort") "Rebase aborted"))))

;; --- merge-conflict resolution ----------------------------------------------
;; Conflicted files show with an "unmerged" label in the working tree. Take one
;; side wholesale, then stage (`s`) to mark resolved.
(define (magit-take-ours)
  (let ((cur (magit-current)))
    (if cur
        (magit-do (list "checkout" "--ours" "--" (magit-newpath (cadr cur))) "Took ours — stage to resolve")
        (message "No file at point"))))
(define (magit-take-theirs)
  (let ((cur (magit-current)))
    (if cur
        (magit-do (list "checkout" "--theirs" "--" (magit-newpath (cadr cur))) "Took theirs — stage to resolve")
        (message "No file at point"))))

;; The status / log porcelain are read-only views, not code — no line numbers.
;; (The commit-message buffer, git-commit-mode, keeps them: it's editable text.)
(set-option "line-numbers:magit-status" "false")
(set-option "line-numbers:magit-log-mode" "false")
;; Highlight the line at point (hl-line) — these are navigational list buffers
;; where "what's under the cursor" drives every single-key action.
(set-option "hl-line:magit-status" "true")
(set-option "hl-line:magit-log-mode" "true")

;; --- keymaps ----------------------------------------------------------------
;; magit-status (matches magit's keys; uppercase = shift-, shown prettified in
;; transient menus via Rust pretty_key).
(define-key "magit-status" "s" "magit-stage")
(define-key "magit-status" "shift-s" "magit-stage-all")
(define-key "magit-status" "u" "magit-unstage")
(define-key "magit-status" "shift-u" "magit-unstage-all")
(define-key "magit-status" "k" "magit-discard")
(define-key "magit-status" "c" "magit-commit-popup")
(define-key "magit-status" "b" "magit-branch-popup")
(define-key "magit-status" "shift-p" "magit-push-popup")
(define-key "magit-status" "shift-f" "magit-pull-popup")
(define-key "magit-status" "f" "magit-fetch-popup")
(define-key "magit-status" "z" "magit-stash-popup")
(define-key "magit-status" "l" "magit-log-popup")
(define-key "magit-status" "shift-x" "magit-reset-popup")
(define-key "magit-status" "shift-m" "magit-remote-popup")
(define-key "magit-status" "t" "magit-tag-popup")
(define-key "magit-status" "m" "magit-merge-popup")
(define-key "magit-status" "shift-b" "magit-blame")
(define-key "magit-status" "r" "magit-rebase-popup")
(define-key "magit-status" "y" "magit-copy")
(define-key "magit-status" "i" "magit-gitignore")
(define-key "magit-status" "$" "magit-process-buffer")
(define-key "magit-status" "shift-h" "magit-toggle-history")
(define-key "magit-status" "?" "magit-dispatch")
(define-key "magit-status" "g" "magit-refresh")
(define-key "magit-status" "v" "magit-toggle-view")
(define-key "magit-status" "n" "next-line")
(define-key "magit-status" "p" "previous-line")
(define-key "magit-status" "alt-n" "magit-next-section")
(define-key "magit-status" "alt-p" "magit-prev-section")
(define-key "magit-status" "q" "magit-quit")
(define-key "magit-status" "tab" "magit-toggle-fold")
(define-key "magit-status" "backtab" "magit-cycle-all")
(define-key "magit-status" "shift-tab" "magit-cycle-all")
(define-key "magit-status" "enter" "magit-visit-file")

;; Evil-mode overlay (consulted only under evil, before the keymap above, so the
;; Emacs defaults are untouched). `evil-collection-init` gives every special
;; buffer `j`/`k` motion, which takes `k` over from discard — so re-expose
;; discard on `x` here, the magit-specific half of the overlay.
(define-key "evil-normal:magit-status" "x" "magit-discard")
;; Vim top/bottom motion. `g g` makes `g` a prefix, which shadows the single-`g`
;; refresh from the keymap above — so re-expose refresh on `g r`, matching
;; evil-collection's magit bindings.
(define-key "evil-normal:magit-status" "g g" "evil-goto-first-line")
(define-key "evil-normal:magit-status" "shift-g" "evil-goto-last-line")
(define-key "evil-normal:magit-status" "g r" "magit-refresh")

;; Log / commit / stash-list buffers.
(define-key "magit-log-mode" "enter" "magit-log-show")
(define-key "magit-log-mode" "shift-a" "magit-log-cherry-pick")
(define-key "magit-log-mode" "v" "magit-log-revert")
(define-key "magit-log-mode" "q" "kill-buffer")
(define-key "magit-log-mode" "n" "next-line")
(define-key "magit-log-mode" "p" "previous-line")

;; Launch with C-x g (resolves the repo from the current buffer's directory).
(define-key "fundamental" "ctrl-x g" "magit-status")

;; C-x M: pick a different repo via Helm, then open magit there (e.g. from
;; *scratch*, or to jump to another project).
(define (helm-magit)
  (helm (list (list "Open git repo in" "find-file-candidates" "magit-status-in"))))
(define-key "fundamental" "ctrl-x M" "helm-magit")

;; C-x v …: inline git-blame controls on the current file buffer (Emacs vc-prefix).
(define-key "fundamental" "ctrl-x v b" "magit-blame-toggle")        ; full-file blame
(define-key "fundamental" "ctrl-x v shift-b" "magit-blame-line-toggle") ; current-line blame
(define-key "fundamental" "ctrl-x v enter" "magit-blame-show")     ; show commit at point
(define-key "fundamental" "ctrl-x v y" "magit-blame-copy")         ; copy hash at point

;; `magit` is the familiar alias for `magit-status`.
(define (magit) (magit-status))

;; Expose magit's entry points to M-x. helm-execute-command only lists
;; registered commands; the single-key actions stay in-buffer.
(register-command "magit-status")
(register-command "magit")
(register-command "magit-dispatch")
(register-command "magit-log-popup")
(register-command "helm-magit")
(register-command "magit-blame-toggle")
(register-command "magit-blame-line-toggle")
(register-command "magit-blame-show")
(register-command "magit-blame-copy")
(register-command "magit-toggle-view")

;; Standalone git operations: these act on the repo (prompting via helm /
;; read-string where they need a target), so they're meaningful from M-x in any
;; buffer — no need to enter the status buffer first. Excluded: the section /
;; file-at-point operations (stage, unstage, discard, blame-file, log-file,
;; take-ours/theirs) which only make sense on point inside a magit buffer, and
;; the *-popup / accessor helpers.
(register-command "magit-do-commit")
(register-command "magit-do-amend")
(register-command "magit-do-reword")
(register-command "magit-pull")
(register-command "magit-push")
(register-command "magit-push-upstream")
(register-command "magit-fetch-all")
(register-command "magit-fetch-origin")
(register-command "magit-stash")
(register-command "magit-stash-pop")
(register-command "magit-stash-apply")
(register-command "magit-stash-drop")
(register-command "magit-stash-list")
(register-command "magit-stash-pick")
(register-command "magit-stash-push-msg")
(register-command "magit-checkout")
(register-command "magit-checkout-remote")
(register-command "magit-branch-create")
(register-command "magit-branch-delete")
(register-command "magit-branch-rename")
(register-command "magit-branch-set-upstream")
(register-command "magit-merge")
(register-command "magit-rebase-onto")
(register-command "magit-rebase-continue")
(register-command "magit-rebase-skip")
(register-command "magit-rebase-abort")
(register-command "magit-reset-soft")
(register-command "magit-reset-mixed")
(register-command "magit-reset-hard")
(register-command "magit-remote-add")
(register-command "magit-remote-remove")
(register-command "magit-tag-create")
(register-command "magit-tag-delete")
(register-command "magit-log-all")
(register-command "magit-log-current")
