;; Core Lisp-style helpers built on the new read primitives (point, goto-char,
;; buffer-string, …). Loaded early so later libs (magit, projectile) can use them.

;; save-excursion: run body, then restore point. Mirrors Emacs's special form.
;; `goto-char` is queued as a command, so point is restored after body's edits.
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

;; UTF-8 byte length of a string — the editor's overlay/byte APIs are byte-based,
;; so Scheme that computes byte offsets (e.g. magit diff accents) needs this
;; rather than the char-count `string-length`.
(define (utf8-length s)
  (let loop ((chars (string->list s)) (n 0))
    (if (null? chars)
        n
        (let ((cp (char->integer (car chars))))
          (loop (cdr chars)
                (+ n (cond ((< cp 128) 1) ((< cp 2048) 2) ((< cp 65536) 3) (else 4))))))))

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

;; Insertion sort of a string list (Steel has no builtin `sort`).
(define (insert-sorted x sorted)
  (cond ((null? sorted) (list x))
        ((string<? x (car sorted)) (cons x sorted))
        (else (cons (car sorted) (insert-sorted x (cdr sorted))))))
(define (sort-strings lst)
  (if (null? lst) '() (insert-sorted (car lst) (sort-strings (cdr lst)))))

;; Minibuffer reads with an OPTIONAL initial value (Emacs `read-string` /
;; `completing-read` style). The Rust `*-impl` primitives take a required
;; initial; these wrappers default it to "" so existing 2-/3-arg calls keep
;; working while new callers may pre-fill the input:
;;   (read-string "New name: " "cb" old-name)
;;   (completing-read "Pick: " '("a" "b") "cb" "a")
(define (read-string prompt callback . rest)
  (read-string-impl prompt callback (if (null? rest) "" (car rest))))
(define (completing-read prompt candidates callback . rest)
  (completing-read-impl prompt candidates callback (if (null? rest) "" (car rest))))

;; (set-line-image line path [max-width]) — display an image on rows below
;; `line`; the optional max-width (px) caps the displayed width (0 = default).
(define (set-line-image line path . rest)
  (set-line-image-impl line path (if (null? rest) 0 (car rest))))

;; Interactive-command registry: Scheme commands register here so M-x lists them
;; alongside the built-in commands (see help.scm).
(define *interactive-commands* '())
(define (register-command name)
  (when (not (member name *interactive-commands*))
    (set! *interactive-commands* (cons name *interactive-commands*))))

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
