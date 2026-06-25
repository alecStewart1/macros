;; A tiny git-based package manager. `package-install` clones a repo into
;; `(packages-directory)` (<config-dir>/packages) on first launch, optionally pins
;; it to a commit/tag, then `load`s its entry file — so a user's init.scm can pull
;; in third-party Steel extensions. Subsequent launches skip the clone and just
;; load (fast, offline). Loaded as a bundled startup script, before the user's
;; init.scm, so `package-install` is defined by the time the config runs.
;;
;; For a package you're developing locally, `package-install-local` loads it in
;; place from a working-tree directory — no clone, no copy, edits live on restart.
;;
;; Builds entirely on existing primitives: `call-process-full` (git), the file
;; predicates, and Steel's native `load`. No Rust command of its own.

;; Fetch the value following `key` in a quoted-symbol plist (the `lsp-language`
;; convention), or #f if absent. e.g. (pkg--opt '('ref "v1" 'entry "x.scm") 'ref).
(define (pkg--opt opts key)
  (cond ((or (null? opts) (null? (cdr opts))) #f)
        ((equal? (car opts) key) (car (cdr opts)))
        (else (pkg--opt (cdr (cdr opts)) key))))

;; Derive a package name from its git URL: the final path segment with any
;; trailing slash and ".git" suffix removed.
;;   "https://github.com/u/macros-foo.git" -> "macros-foo"
(define (pkg--name-from-url url)
  (let* ((trimmed (if (ends-with? url "/")
                      (substring url 0 (- (string-length url) 1))
                      url))
         (base    (basename trimmed)))
    (if (ends-with? base ".git")
        (substring base 0 (- (string-length base) 4))
        base)))

;; Names of packages successfully loaded this session — the registry `'depends`
;; checks against (declaration-order dependency: a dep must have been installed
;; earlier in init.scm).
(define *packages-loaded* '())
(define (package-loaded? name) (if (member name *packages-loaded*) #t #f))

;; The directory of the package currently being loaded — bound just before its
;; entry file runs, so a multi-file package can find its own siblings (Steel's
;; `load` resolves relative paths against the process cwd, not the entry file).
;; Capture it into a package-local binding AT LOAD TIME:
;;   (define my-dir *package-directory*)
;;   (load (path-join my-dir "helper.scm"))
;; It is only meaningful while a package loads; don't read it later (e.g. inside a
;; command), where it will hold whichever package loaded last.
(define *package-directory* "")

;; A readable one-liner for an error raised during package load.
(define (pkg--error->string err)
  (if (error-object? err) (error-object-message err) "unknown error"))

;; Run `git args`, returning #t on success. On failure, echo git's first stderr
;; line (tagged with `what`, e.g. "clone") and return #f — the caller decides
;; whether to continue.
(define (pkg--git args what)
  (let* ((result (call-process-full "git" args ""))
         (code   (car result))
         (stderr (car (cdr (cdr result)))))
    (if (equal? code "0")
        #t
        (begin
          (message (string-append "package: git " what " failed — "
                                  (let ((lines (non-empty-lines stderr)))
                                    (if (null? lines) "(no output)" (car lines)))))
          #f))))

;; The installed package directories (immediate subdirs of the packages dir).
(define (pkg--installed-dirs)
  (let ((root (packages-directory)))
    (if (file-directory? root)
        (filter file-directory? (list-directory root))
        '())))

;; Load a package's entry file and run its `config` thunk, isolating any error so
;; one broken package can't abort the rest of init.scm. The package is marked
;; loaded only on success, so dependents using `'depends` see an accurate state.
(define (pkg--load name dir entry config)
  (let ((entry-path (path-join dir entry)))
    (cond
      ((not (file-directory? dir))
       (message (string-append "package: " name " — clone failed, not loaded")))
      ((not (file-exists? entry-path))
       (message (string-append "package: " name " — entry not found: " entry)))
      (else
       (with-handler
         (lambda (err)
           (message (string-append "package: " name " — load error: "
                                   (pkg--error->string err))))
         (set! *package-directory* dir)   ; entry may read this to find siblings
         (load entry-path)
         (when config (config))
         (pkg--index-docs dir)            ; make its commented defs discoverable
         (set! *packages-loaded* (cons name *packages-loaded*))
         (message (string-append "package: " name " ready")))))))

;; Feed a freshly loaded package's `;;`-documented defines into the doc index, so
;; `helm-apropos` / `describe-function` find and jump to them just like built-ins.
;; Indexes the entry plus any top-level `.scm` siblings in `dir` (covers flat
;; packages; sources the entry pulls in from subdirectories are not walked).
;; `index-doc-file` upserts by name, so re-loading a package never grows the index.
(define (pkg--index-docs dir)
  (for-each (lambda (path) (when (ends-with? path ".scm") (index-doc-file path)))
            (list-directory dir)))

;; (package-install URL 'option value …) — install and load a git package.
;;
;; Options (all optional, quoted symbols like `lsp-language`):
;;   'name    NAME    install/registry name + clone dir (default: derived from URL)
;;   'ref     REF     commit, tag, or branch to check out after clone (pin it!)
;;   'entry   FILE    file to load, relative to the repo (default: "<name>.scm")
;;   'depends (NAMES) packages that must already be installed earlier in init.scm;
;;                    if any is missing this package is skipped (ordering only —
;;                    nothing is auto-fetched)
;;   'config  THUNK   a (lambda () …) run AFTER the entry loads, for your settings
;;
;; The repo is cloned only when its directory is absent (so only the first launch
;; touches the network); a pinned 'ref is re-checked-out every launch, so editing
;; it in init.scm re-pins on the next start. Example:
;;
;;   (package-install "https://github.com/u/macros-http"
;;     'ref     "v2.1.0"
;;     'depends '("async")
;;     'config  (lambda () (set-option "http-timeout" "30")))
(define (package-install url . opts)
  (let* ((name    (or (pkg--opt opts 'name) (pkg--name-from-url url)))
         (ref     (pkg--opt opts 'ref))
         (entry   (or (pkg--opt opts 'entry) (string-append name ".scm")))
         (deps    (or (pkg--opt opts 'depends) '()))
         (config  (pkg--opt opts 'config))
         (dir     (path-join (packages-directory) name))
         (missing (filter (lambda (d) (not (package-loaded? d))) deps)))
    (cond
      ;; A declared dependency hasn't loaded yet — skip (deps must come first).
      ((not (null? missing))
       (message (string-append "package: " name " skipped — install these first: "
                               (string-join missing ", "))))
      (else
       (when (not (file-directory? dir))
         (message (string-append "package: cloning " name " …"))
         (pkg--git (list "clone" "--quiet" url dir) "clone"))
       (when (and ref (file-directory? dir))
         (pkg--git (list "-C" dir "checkout" "--quiet" ref) "checkout"))
       (pkg--load name dir entry config)))))

;; (package-install-local DIR 'option value …) — load a package from a local
;; directory IN PLACE, without cloning or copying. This is the workflow for
;; developing a package: point at its working tree, edit the source, and restart
;; the editor (or re-`load` the entry) to pick up changes — no `package-update`
;; round-trip, no stale clone in the packages dir.
;;
;; DIR is an absolute path to the package's working tree (no `~` expansion; use a
;; full path, or build one with `(path-join (current-directory) "…")`). Accepts
;; the same 'name / 'entry / 'depends / 'config options as `package-install`;
;; 'ref is intentionally unsupported — your working tree loads exactly as-is.
;;
;;   (package-install-local "/Users/me/dev/macros-foo"
;;     'config (lambda () (set-option "foo-mode" "on")))
(define (package-install-local dir . opts)
  (let* ((name    (or (pkg--opt opts 'name) (basename dir)))
         (entry   (or (pkg--opt opts 'entry) (string-append name ".scm")))
         (deps    (or (pkg--opt opts 'depends) '()))
         (config  (pkg--opt opts 'config))
         (missing (filter (lambda (d) (not (package-loaded? d))) deps)))
    (cond
      ;; A declared dependency hasn't loaded yet — skip (deps must come first).
      ((not (null? missing))
       (message (string-append "package: " name " skipped — install these first: "
                               (string-join missing ", "))))
      ;; No clone happens here, so give a clearer message than pkg--load's
      ;; clone-oriented one when the path is wrong.
      ((not (file-directory? dir))
       (message (string-append "package: " name " — local dir not found: " dir)))
      (else
       (pkg--load name dir entry config)))))

;; M-x package-list — echo the installed packages (dirs under *packages-directory*).
(define (package-list)
  (let ((names (map basename (pkg--installed-dirs))))
    (message (if (null? names)
                 "package: none installed"
                 (string-append "packages: " (string-join (sort-strings names) ", "))))))
(register-command "package-list")

;; M-x package-update — fast-forward every installed package to its upstream and
;; report. Packages pinned to a commit/tag ('ref) are re-pinned from init.scm on
;; the next launch, so this mainly advances branch-tracking installs; a detached
;; pin that can't fast-forward is simply reported as failed and left untouched.
(define (package-update)
  (for-each
   (lambda (dir)
     (let ((name (basename dir)))
       (when (pkg--git (list "-C" dir "pull" "--ff-only" "--quiet") "pull")
         (message (string-append "package: " name " updated")))))
   (pkg--installed-dirs))
  (message "package-update: done"))
(register-command "package-update")
