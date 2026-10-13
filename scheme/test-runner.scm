;; test-runner.scm — run tests from the buffer, on top of compile-mode (shell.scm).
;;
;;   M-x test-at-point    C-c x t   the test enclosing point
;;   M-x test-file        C-c x f   every test in the current file
;;   M-x test-project     C-c x p   the whole project's suite
;;   M-x test-rerun-last  C-c x r   repeat the last test run (also `g` in *compilation*)
;;
;; Output streams into *compilation*, so next-error / previous-error / RET jump
;; to failures (shell.scm's compile-match also knows Rust panics, jest/vitest
;; stack frames and `go test` locations). Each run happens in the directory the
;; language's tooling expects — the crate (nearest Cargo.toml), the Go package
;; (the file's own directory; the module root for the project suite), the JS
;; package (nearest package.json), the pytest rootdir — falling back to the
;; project directory (title-bar perspective / projectile root).
;;
;; Languages (detected from the file extension; for test-project from a fileless
;; buffer, from the project's marker files):
;;   rust    cargo test <file-module>::<inline-mods>::<fn>   (enclosing #[test] fn;
;;           integration tests under tests/ get `--test <name>`, src/bin/ `--bin`)
;;   python  python -m pytest <file>::<Class>::<test_fn>
;;   go      go test -run '^TestName$' .   (in the package directory)
;;   js/ts   npx jest <file> -t '<name>'  |  npx vitest run <file> -t '<name>'
;;           (jest vs vitest from package.json; otherwise `npm test -- <file>`)
;;   zig     zig test <file> --test-filter '<name>'
;; Anything else prompts for a command (default: the last one).
;;
;; Commands are templates — change one from init.scm with
;;   (test-runner-set-command "rust" 'point "cargo nextest run {filter}")
;; Kinds are 'project / 'file / 'point. Placeholders (already shell-quoted where
;; that matters): {file} file relative to the run directory; {target} rust's
;; "--test x " / "--bin x " (or ""); {filter} rust test path; {node} pytest node
;; id; {name} the test's name; {name-regex} '^name$' (go) or the escaped name
;; (jest/vitest -t); {file-regex} go's '^(TestA|TestB)$' for the file.
;;
;; All the detection below is pure (text + 0-based line in, strings out), so it
;; is unit-tested in src/script/tests.rs without an editor.

;; --- configuration ----------------------------------------------------------------

;; runner -> (project-template file-template point-template)
(define *test-commands*
  (list (list "rust"   "cargo test"       "cargo test {target}{filter}"   "cargo test {target}{filter}")
        (list "python" "python -m pytest" "python -m pytest {file}"       "python -m pytest {node}")
        (list "go"     "go test ./..."    "go test -run {file-regex} ."   "go test -run {name-regex} .")
        (list "jest"   "npx jest"         "npx jest {file}"               "npx jest {file} -t {name-regex}")
        (list "vitest" "npx vitest run"   "npx vitest run {file}"         "npx vitest run {file} -t {name-regex}")
        (list "npm"    "npm test"         "npm test -- {file}"            "npm test -- {file}")
        (list "zig"    "zig build test"   "zig test {file}"               "zig test {file} --test-filter {name}")))

(define (test-kind-index kind)
  (let ((k (if (symbol? kind) (symbol->string kind) kind)))
    (cond ((equal? k "project") 1) ((equal? k "file") 2) (else 3))))

;; Override one template: (test-runner-set-command "go" 'point "gotestsum -- -run {name-regex} .")
(define (test-runner-set-command runner kind template)
  (let ((i (test-kind-index kind))
        (row (or (assoc runner *test-commands*) (list runner "" "" ""))))
    (set! *test-commands*
          (cons (map (lambda (j) (if (= j i) template (nth row j))) (list 0 1 2 3))
                (alist-remove *test-commands* runner)))))

(define (test-template runner kind)
  (let ((row (assoc runner *test-commands*)))
    (and row (nth row (test-kind-index kind)))))

;; --- small pure helpers -------------------------------------------------------------

(define (test-shell-quote s)
  (string-append "'" (string-replace s "'" "'\\''") "'"))

;; Fill `{key}` placeholders from an alist of (key . value) strings.
(define (test-expand template vars)
  (if (null? vars)
      template
      (test-expand (string-replace template
                                   (string-append "{" (car (car vars)) "}")
                                   (cdr (car vars)))
                   (cdr vars))))

;; `lines` up to and including line `n` (0-based).
(define (test-lines-upto lines n)
  (let loop ((ls lines) (i 0) (acc '()))
    (if (or (null? ls) (> i n))
        (reverse acc)
        (loop (cdr ls) (+ i 1) (cons (car ls) acc)))))

;; The first non-#f result of `(f line)` scanning from line `n` upward.
(define (test-scan-up lines n f)
  (let loop ((ls (reverse (test-lines-upto lines n))))
    (if (null? ls)
        #f
        (or (f (car ls)) (loop (cdr ls))))))

(define (test-capture pat s i)
  (let ((m (regexp-captures pat s)))
    (if (and (not (null? m)) (< i (length m)) (not (equal? (nth m i) "")))
        (nth m i)
        #f)))

(define (test-blank? line) (equal? (string-trim line) ""))

;; `file` relative to directory `root` (unchanged when it isn't under it).
(define (test-relative root file)
  (let ((prefix (if (ends-with? root "/") root (string-append root "/"))))
    (if (starts-with? file prefix)
        (substring file (string-length prefix) (string-length file))
        file)))

;; --- rust ---------------------------------------------------------------------------

;; Blank out string/char literals and drop `//` comments so braces inside them
;; don't skew the depth count.
(define (test-rust-strip line)
  (let* ((no-str (replace-regexp-in-string "\"([^\"\\\\]|\\\\.)*\"" "\"\"" line))
         (no-chr (replace-regexp-in-string "'([^'\\\\]|\\\\.)'" "''" no-str)))
    (replace-regexp-in-string "//.*$" "" no-chr)))

(define (test-brace-delta line)
  (let loop ((cs (string->list line)) (d 0))
    (cond ((null? cs) d)
          ((char=? (car cs) #\{) (loop (cdr cs) (+ d 1)))
          ((char=? (car cs) #\}) (loop (cdr cs) (- d 1)))
          (else (loop (cdr cs) d)))))

(define *test-rust-fn-re*
  "^\\s*(?:pub(?:\\([^)]*\\))?\\s+)?(?:default\\s+)?(?:const\\s+)?(?:async\\s+)?(?:unsafe\\s+)?(?:extern\\s+\"[^\"]*\"\\s+)?fn\\s+([A-Za-z_][A-Za-z0-9_]*)")
(define *test-rust-mod-re*
  "^\\s*(?:pub(?:\\([^)]*\\))?\\s+)?mod\\s+([A-Za-z_][A-Za-z0-9_]*)\\s*\\{")
(define *test-rust-attr-re*
  "^\\s*#\\[(?:[A-Za-z_]+::)*(?:test|rstest|test_case|wasm_bindgen_test)\\b")

;; The Rust test enclosing line `n` of `text`: (inline-mods test-fn), or #f when
;; point isn't inside a #[test] fn. One pass over lines 0..n keeping a stack of
;; open `mod`/`fn` blocks: (kind name open-depth opened? test?).
(define (test-rust-point text n)
  (let loop ((ls (test-lines-upto (split-many text "\n") n))
             (i 0) (depth 0) (stack '()) (pending #f))
    (if (null? ls)
        (let* ((frames (reverse stack))   ; outermost first
               (tests (filter (lambda (f) (and (equal? (car f) 'fn) (nth f 4))) frames)))
          (if (null? tests)
              #f
              (let ((test (last tests)))  ; innermost #[test] fn
                ;; the `mod`s enclosing that fn, outermost first
                (list (map cadr (filter (lambda (f) (and (equal? (car f) 'mod)
                                                         (< (nth f 2) (nth test 2))))
                                        frames))
                      (cadr test)))))
        (let* ((line (test-rust-strip (car ls)))
               (trimmed (string-trim line))
               (fn-name (test-capture *test-rust-fn-re* line 1))
               (mod-name (test-capture *test-rust-mod-re* line 1))
               (attr? (and (not (null? (regexp-captures *test-rust-attr-re* line))) #t))
               ;; a body-less `fn f();` / `mod m;` opens nothing
               (decl-only? (and (string-contains? line ";") (not (string-contains? line "{"))))
               (stack1 (cond ((and fn-name (not decl-only?))
                              (cons (list 'fn fn-name depth #f pending) stack))
                             ((and mod-name (not decl-only?))
                              (cons (list 'mod mod-name depth #f #f) stack))
                             (else stack)))
               (pending1 (cond (attr? #t)
                               (fn-name #f)
                               ((or (equal? trimmed "") (starts-with? trimmed "#[")) pending)
                               (else #f)))
               (depth1 (+ depth (test-brace-delta line)))
               ;; mark frames whose `{` has now opened
               (stack2 (map (lambda (f) (if (> depth1 (nth f 2))
                                            (list (car f) (cadr f) (nth f 2) #t (nth f 4))
                                            f))
                            stack1))
               (last-line? (null? (cdr ls)))
               ;; close opened frames whose block ended (not on point's own line,
               ;; so point on a closing brace still counts as inside)
               (stack3 (if last-line?
                           stack2
                           (filter (lambda (f) (not (and (nth f 3) (<= depth1 (nth f 2)))))
                                   stack2))))
          (loop (cdr ls) (+ i 1) depth1 stack3 pending1)))))

;; A Rust file's place in its crate, from its path relative to the crate root:
;; (target-args . module-path). src/a/b.rs → ("" . "a::b"); src/a/mod.rs →
;; ("" . "a"); src/main.rs / src/lib.rs → ("" . ""); tests/it.rs →
;; ("--test it " . ""); src/bin/tool.rs → ("--bin tool " . "").
(define (test-rust-file-target rel)
  (let ((parts (split-many (if (ends-with? rel ".rs")
                               (substring rel 0 (- (string-length rel) 3))
                               rel)
                           "/")))
    (cond
      ((and (>= (length parts) 2) (equal? (car parts) "tests"))
       (cons (string-append "--test " (cadr parts) " ") ""))
      ((and (>= (length parts) 3) (equal? (car parts) "src") (equal? (cadr parts) "bin"))
       (cons (string-append "--bin " (nth parts 2) " ") ""))
      ((and (not (null? parts)) (equal? (car parts) "src"))
       (let* ((mods (cdr parts))
              (mods (if (and (not (null? mods)) (member (last mods) (list "mod" "main" "lib")))
                        (reverse (cdr (reverse mods)))
                        mods)))
         (cons "" (string-join mods "::"))))
      (else (cons "" "")))))

(define (test-join-path segs)
  (string-join (filter (lambda (s) (not (equal? s ""))) segs) "::"))

;; --- python -------------------------------------------------------------------------

(define (test-indent line)
  (let loop ((cs (string->list line)) (n 0))
    (if (and (not (null? cs)) (or (char=? (car cs) #\space) (char=? (car cs) #\tab)))
        (loop (cdr cs) (+ n 1))
        n)))

;; pytest node suffix for line `n`: "TestClass::test_fn", "test_fn", "TestClass"
;; (point in a test class but not in a test), or #f.
(define (test-python-point text n)
  (let loop ((ls (test-lines-upto (split-many text "\n") n)) (stack '()))
    (if (null? ls)
        (let* ((frames (reverse stack))
               (classes (map cadr (filter (lambda (f) (equal? (car f) 'class)) frames)))
               (defs (filter (lambda (f) (equal? (car f) 'def)) frames))
               (test-def (and (not (null? defs))
                              (starts-with? (cadr (car defs)) "test")
                              (cadr (car defs))))
               ;; only classes enclosing the test def count
               (outer (if test-def
                          (map cadr (filter (lambda (f) (and (equal? (car f) 'class)
                                                             (< (nth f 2) (nth (car defs) 2))))
                                            frames))
                          classes)))
          (cond (test-def (string-join (append outer (list test-def)) "::"))
                ((and (not (null? classes)) (starts-with? (last classes) "Test"))
                 (string-join classes "::"))
                (else #f)))
        (let ((line (car ls)))
          (if (or (test-blank? line) (starts-with? (string-trim line) "#"))
              (loop (cdr ls) stack)
              (let* ((ind (test-indent line))
                     (kept (filter (lambda (f) (< (nth f 2) ind)) stack))
                     (def (test-capture "^\\s*(?:async\\s+)?def\\s+([A-Za-z_][A-Za-z0-9_]*)" line 1))
                     (cls (test-capture "^\\s*class\\s+([A-Za-z_][A-Za-z0-9_]*)" line 1)))
                (loop (cdr ls)
                      (cond (def (cons (list 'def def ind) kept))
                            (cls (cons (list 'class cls ind) kept))
                            (else kept)))))))))

;; --- go -----------------------------------------------------------------------------

(define *test-go-re* "^func\\s+((?:Test|Benchmark|Example|Fuzz)[A-Za-z0-9_]*)\\s*\\(")

;; The Go test func enclosing line `n`: the nearest top-level `func` at or above
;; it, if that func is a Test/Benchmark/Example/Fuzz; else #f.
(define (test-go-point text n)
  (let ((r (test-scan-up (split-many text "\n") n
                         (lambda (line)
                           (and (starts-with? line "func")
                                (or (test-capture *test-go-re* line 1) 'other))))))
    (if (string? r) r #f)))

;; Every Test* func in the file.
(define (test-go-file-names text)
  (filter (lambda (x) x)
          (map (lambda (line)
                 (let ((name (test-capture *test-go-re* line 1)))
                   (and name (starts-with? name "Test") name)))
               (split-many text "\n"))))

;; --- js / ts ------------------------------------------------------------------------

(define *test-js-re*
  "(?:^|[^A-Za-z0-9_.$])(?:it|test|describe)(?:\\.[A-Za-z]+)*\\s*\\(\\s*(?:'([^']*)'|\"([^\"]*)\"|`([^`]*)`)")

;; The nearest it/test/describe title at or above line `n`, or #f.
(define (test-js-point text n)
  (test-scan-up (split-many text "\n") n
                (lambda (line)
                  (or (test-capture *test-js-re* line 1)
                      (test-capture *test-js-re* line 2)
                      (test-capture *test-js-re* line 3)))))

;; jest or vitest (or plain npm) from package.json's text.
(define (test-js-runner package-json)
  (cond ((string-contains? package-json "vitest") "vitest")
        ((string-contains? package-json "jest") "jest")
        (else "npm")))

;; --- zig ----------------------------------------------------------------------------

(define (test-zig-point text n)
  (test-scan-up (split-many text "\n") n
                (lambda (line) (test-capture "^\\s*test\\s+\"([^\"]*)\"" line 1))))

;; --- planning (pure) ----------------------------------------------------------------

(define (test-language-of path)
  (cond ((ends-with? path ".rs") "rust")
        ((ends-with? path ".py") "python")
        ((ends-with? path ".go") "go")
        ((or (ends-with? path ".ts") (ends-with? path ".tsx") (ends-with? path ".js")
             (ends-with? path ".jsx") (ends-with? path ".mjs") (ends-with? path ".cjs")
             (ends-with? path ".mts") (ends-with? path ".cts"))
         "js")
        ((ends-with? path ".zig") "zig")
        (else #f)))

;; The command for `kind` ('project / 'file / 'point) given the runner, the file
;; relative to the run directory, and the buffer text + point line. Returns
;; (cmd . #f), or (#f . reason) when there's nothing to run.
(define (test-command kind runner rel text line)
  (let ((tmpl (test-template runner kind))
        (k (if (symbol? kind) (symbol->string kind) kind)))
    (cond
      ((not tmpl) (cons #f (string-append "No test command for " runner)))
      ((equal? k "project") (cons tmpl #f))
      ((equal? runner "rust")
       (let ((target (test-rust-file-target rel)))
         (if (equal? k "file")
             (cons (test-expand tmpl (list (cons "target" (car target))
                                           (cons "filter" (if (equal? (cdr target) "")
                                                              ""
                                                              (string-append (cdr target) "::")))))
                   #f)
             (let ((hit (test-rust-point text line)))
               (if hit
                   (cons (test-expand tmpl (list (cons "target" (car target))
                                                 (cons "filter" (test-join-path
                                                                  (append (list (cdr target))
                                                                          (car hit)
                                                                          (list (cadr hit)))))))
                         #f)
                   (cons #f "Point is not inside a #[test] function"))))))
      ((equal? runner "python")
       (if (equal? k "file")
           (cons (test-expand tmpl (list (cons "file" (test-shell-quote rel)))) #f)
           (let ((node (test-python-point text line)))
             (if node
                 (cons (test-expand tmpl (list (cons "file" (test-shell-quote rel))
                                               (cons "node" (test-shell-quote
                                                              (string-append rel "::" node)))))
                       #f)
                 (cons #f "Point is not inside a test")))))
      ((equal? runner "go")
       (if (equal? k "file")
           (let ((names (test-go-file-names text)))
             (if (null? names)
                 (cons #f "No Test functions in this file")
                 (cons (test-expand tmpl (list (cons "file" (test-shell-quote rel))
                                               (cons "file-regex"
                                                     (test-shell-quote
                                                       (string-append "^(" (string-join names "|") ")$")))))
                       #f)))
           (let ((name (test-go-point text line)))
             (if name
                 (cons (test-expand tmpl (list (cons "name" (test-shell-quote name))
                                               (cons "name-regex"
                                                     (test-shell-quote (string-append "^" name "$")))))
                       #f)
                 (cons #f "Point is not inside a Test function")))))
      ((member runner (list "jest" "vitest" "npm"))
       (if (equal? k "file")
           (cons (test-expand tmpl (list (cons "file" (test-shell-quote rel)))) #f)
           (let ((name (test-js-point text line)))
             (if name
                 (cons (test-expand tmpl (list (cons "file" (test-shell-quote rel))
                                               (cons "name" (test-shell-quote name))
                                               (cons "name-regex" (test-shell-quote (regexp-quote name)))))
                       #f)
                 (cons #f "Point is not inside a test")))))
      ((equal? runner "zig")
       (if (equal? k "file")
           (cons (test-expand tmpl (list (cons "file" (test-shell-quote rel)))) #f)
           (let ((name (test-zig-point text line)))
             (if name
                 (cons (test-expand tmpl (list (cons "file" (test-shell-quote rel))
                                               (cons "name" (test-shell-quote name))))
                       #f)
                 (cons #f "Point is not inside a test block")))))
      (else (cons (test-expand tmpl '()) #f)))))

;; --- impure glue --------------------------------------------------------------------

;; The nearest directory at or above `dir` containing any of `markers`, or #f.
(define (test-find-up dir markers)
  (cond ((or (not dir) (equal? dir "")) #f)
        ((project-has-marker? dir markers) dir)
        (else (let ((parent (parent-directory dir)))
                (if (or (equal? parent dir) (equal? parent "")) #f (test-find-up parent markers))))))

(define *test-python-markers* (list "pyproject.toml" "pytest.ini" "setup.cfg" "setup.py" "tox.ini"))

;; (runner . run-directory) for `file`, or for a fileless buffer the project
;; directory's markers. #f when nothing is recognised.
(define (test-runner-for file kind)
  (let* ((lang (and (not (equal? file "")) (test-language-of file)))
         (fdir (if lang (parent-directory file) (project-directory)))
         (k (if (symbol? kind) (symbol->string kind) kind)))
    (cond
      ((equal? lang "rust") (cons "rust" (or (test-find-up fdir (list "Cargo.toml")) (project-directory))))
      ((equal? lang "python") (cons "python" (or (test-find-up fdir *test-python-markers*) (project-directory))))
      ((equal? lang "go")
       (cons "go" (if (equal? k "project")
                      (or (test-find-up fdir (list "go.mod")) (project-directory))
                      fdir)))
      ((equal? lang "js")
       (let ((root (or (test-find-up fdir (list "package.json")) (project-directory))))
         (cons (test-js-runner (if (file-exists? (path-join root "package.json"))
                                   (read-file (path-join root "package.json"))
                                   ""))
               root)))
      ((equal? lang "zig") (cons "zig" (or (test-find-up fdir (list "build.zig")) fdir)))
      ;; Unknown file type: only the project suite can be guessed, from markers.
      ((equal? k "project")
       (let ((dir (project-directory)))
         (cond ((project-has-marker? dir (list "Cargo.toml")) (cons "rust" dir))
               ((project-has-marker? dir (list "go.mod")) (cons "go" dir))
               ((project-has-marker? dir (list "package.json"))
                (cons (test-js-runner (read-file (path-join dir "package.json"))) dir))
               ((project-has-marker? dir *test-python-markers*) (cons "python" dir))
               ((project-has-marker? dir (list "build.zig")) (cons "zig" dir))
               (else #f))))
      (else #f))))

(define *test-last* #f)  ; (dir . cmd) of the last run

(define (test-launch dir cmd)
  (set! *test-last* (cons dir cmd))
  (compile-run-in dir cmd))

;; No runner recognised: ask for a command (run in the project directory).
(define (test-run-prompted cmd)
  (test-launch (project-directory) cmd))

(define (test-run kind)
  (let* ((file (buffer-file-name))
         (plan (test-runner-for file kind)))
    (if (not plan)
        (read-string "Test command: " 'test-run-prompted
                     (if *test-last* (cdr *test-last*) "make test"))
        (let* ((runner (car plan))
               (dir (cdr plan))
               (rel (if (equal? file "") "" (test-relative dir file)))
               (res (test-command kind runner rel (buffer-string) (buffer-line))))
          (if (car res)
              (test-launch dir (car res))
              (message (cdr res)))))))

(define-command (test-at-point) (test-run 'point))
(define-command (test-file) (test-run 'file))
(define-command (test-project) (test-run 'project))
(define-command (test-rerun-last)
  (if *test-last*
      (test-launch (car *test-last*) (cdr *test-last*))
      (message "No test run yet")))

(define-key "fundamental" "ctrl-c x t" 'test-at-point)
(define-key "fundamental" "ctrl-c x f" 'test-file)
(define-key "fundamental" "ctrl-c x p" 'test-project)
(define-key "fundamental" "ctrl-c x r" 'test-rerun-last)
