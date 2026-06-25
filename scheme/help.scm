;; help.scm — discoverability: an M-x that lists Scheme commands too, plus
;; describe-key / apropos / describe-bindings. Loads after helm.scm so its
;; M-x rebinding wins.

;; All invocable command names: built-ins + registered Scheme commands.
(define (all-commands)
  (sort-strings (append (command-list) *interactive-commands*)))

;; Run a command (built-in OR Scheme fn) by name — `(name)` via eval-string.
(define (execute-command name)
  (when (not (equal? name ""))
    (eval-string (string-append "(" name ")"))))

;; M-x: fuzzy-pick any command and run it.
(define (helm-execute-command)
  (helm (list (list "M-x" (all-commands) "execute-command"))))
(define-key "fundamental" "alt-x" "helm-execute-command")

;; describe-key (C-h k): what is a chord bound to (in fundamental)?
(define (describe-key) (read-string "Describe key (e.g. ctrl-x ctrl-f): " "describe-key-finish"))
(define (describe-key-finish chord)
  (let ((cmd (key-binding "fundamental" chord)))
    (message (if (equal? cmd "")
                 (string-append chord " is unbound")
                 (string-append chord " runs " cmd)))))

;; Drop duplicates from a list (keeps the last of each — fine, we sort after).
(define (dedupe lst)
  (cond ((null? lst) '())
        ((member (car lst) (cdr lst)) (dedupe (cdr lst)))
        (else (cons (car lst) (dedupe (cdr lst))))))

;; First line of a function's doc (its source comment), or "".
(define (doc-summary name)
  (let ((doc (function-doc name)))
    (if (equal? doc "") "" (car (split-many doc "\n")))))

;; Every name apropos / helm-apropos searches: commands, Scheme functions and
;; variables, and editor options (so "font" surfaces the `font-size` option).
(define (all-names)
  (sort-strings (dedupe (append (all-commands) (function-list) (option-list)))))

;; "name  —  one-line summary" for the apropos listing (bare name if undocumented).
(define (apropos-row n)
  (let ((s (doc-summary n)))
    (if (equal? s "") n (string-append n "  —  " s))))

;; Does `tok` occur in `hay`? Substring match, plus a cheap plural/singular
;; tolerance so a query word like "sizes" still finds the singular "size" in
;; "font-size" (and vice versa). Both args are expected already lower-cased.
(define (token-matches? tok hay)
  (or (string-contains? hay tok)
      (and (> (string-length tok) 3)
           (ends-with? tok "s")
           (string-contains? hay (substring tok 0 (- (string-length tok) 1))))))

;; #t when EVERY whitespace-separated word of `pat` occurs in `hay` — word order
;; and extra words don't matter (the Emacs `apropos` multi-keyword behaviour).
;; Case-insensitive. Blank tokens (from runs of spaces) are ignored.
(define (all-words-match? pat hay)
  (let ((h (string-downcase hay)))
    (define (go toks)
      (cond ((null? toks) #t)
            ((equal? (car toks) "") (go (cdr toks)))
            ((token-matches? (car toks) h) (go (cdr toks)))
            (else #f)))
    (go (split-many (string-downcase pat) " "))))

;; Names matching `pat` across their own text AND their doc-comment, treated as
;; one haystack so any word may match either. The shared engine behind both the
;; static `apropos` buffer and the live `helm-apropos` picker.
(define (apropos-matches pat)
  (filter (lambda (n) (all-words-match? pat (string-append n " " (function-doc n))))
          (all-names)))

;; apropos (C-h a): names OR docs containing a pattern → a help buffer.
(define (apropos) (read-string "Apropos: " "apropos-finish"))
(define (apropos-finish pat)
  (let ((matches (apropos-matches pat)))
    (render-buffer "*apropos*" "help-mode"
                   (if (null? matches)
                       (string-append "Nothing matching \"" pat "\"")
                       (string-append "Matches for \"" pat "\":\n\n"
                                      (join-lines (map apropos-row matches))))
                   '())))

;; describe-function (C-h f): show a function's documentation — its bundled source
;; comment. Completion ranges over every Scheme-defined function.
(define (describe-function)
  (completing-read "Describe function: " (function-list) "describe-function-show"))
(define (describe-function-show name)
  (when (not (equal? name ""))
    (if (member name (option-list))
        (describe-option name)
        (let ((doc (function-doc name)))
          (render-buffer "*help*" "help-mode"
                         (string-append name "\n\n"
                                        (if (equal? doc "") "(no documentation)" doc))
                         '())))))

;; A `get-option` result (string / number / boolean) as display text. Steel has
;; no generic `->string`, so dispatch on type.
(define (option-display v)
  (cond ((string? v) v)
        ((number? v) (number->string v))
        ((boolean? v) (if v "true" "false"))
        (else "")))

;; Options have no source to jump to; describe one instead — its current value
;; and how to set it. (`get-option` returns #false for an option left at its
;; built-in default.)
(define (describe-option name)
  (render-buffer "*help*" "help-mode"
                 (string-append
                  name "\n\n"
                  "Editor option.\n\n"
                  "Current value: " (option-display (get-option name)) "\n\n"
                  "Set it in your config:\n"
                  "  (set-option \"" name "\" <value>)\n\n"
                  "See the Configuration docs for every option and its type.")
                 '()))

;; A candidate as a `(name summary)` pair so Helm dims the one-line doc as a
;; right-aligned annotation; matching/the action still see the bare `name`.
;; Options are tagged "option" (with their value when set); a name with no doc
;; shows as itself.
(define (apropos-pair n)
  (cond
    ((member n (option-list))
     (list n (string-append "option · " (option-display (get-option n)))))
    (else (let ((s (doc-summary n)))
            (if (equal? s "") n (list n s))))))

;; jump-to-definition: open the source of `name` at its define and put point
;; there. `function-source` → `(file line)` (1-based) for anything in the doc
;; index — built-in or dynamically loaded package alike. When the file is absent
;; (an installed binary has no on-disk built-in sources) we fall back to showing
;; the documentation, the way Emacs does for a C-defined primitive.
(define (jump-to-definition name)
  (cond
    ((equal? name "") #f)
    ;; Options have no source line — describe (current value + how to set).
    ((member name (option-list)) (describe-option name))
    (else
     (let ((loc (function-source name)))
       (if (and (not (null? loc)) (file-exists? (car loc)))
           (begin
             (open-file (car loc))
             ;; function-source reports 1-based lines; goto-line is 0-based.
             (goto-line (max 0 (- (cadr loc) 1))))
           (describe-function-show name))))))

;; A LIVE (self-filtered) Helm source: type words from a name or doc-comment
;; ("font size") and the matching functions, variables, and options narrow as you
;; go, each annotated with its summary. >=2 chars so the first keystroke doesn't
;; list the whole world. RET jumps to the implementation (or describes an option);
;; M-a → Describe shows the doc.
(define (helm-apropos-candidates pattern)
  (if (< (string-length pattern) 2)
      '()
      (map apropos-pair (apropos-matches pattern))))

;; helm-apropos (C-h C-a): discoverability search over every loaded definition's
;; doc-comment — built-ins and dynamically loaded packages together.
(define (helm-apropos)
  (helm (list (list "Apropos"
                    (list 'raw "helm-apropos-candidates")
                    (list (list "Jump to definition" "jump-to-definition")
                          (list "Describe"           "describe-function-show"))))))
(define-key "fundamental" "ctrl-h ctrl-a" "helm-apropos")

;; version (M-x version): show the running Macros version in the echo area.
;; `macros-version` is a Rust builtin returning the compiled-in package version.
(define (version)
  (message (string-append "Macros " (macros-version))))

;; describe-bindings (C-h b): every fundamental-mode binding.
(define (describe-bindings)
  (let ((rows (map (lambda (l) (string-replace l "\t" "    "))
                   (sort-strings (keys-with-prefix "fundamental" "")))))
    (render-buffer "*bindings*" "help-mode"
                   (string-append "Key bindings (fundamental):\n\n" (join-lines rows)) '())))

(define-key "help-mode" "q" "kill-buffer")
(define-key "help-mode" "n" "next-line")
(define-key "help-mode" "p" "previous-line")

;; C-h help prefix.
(define-key "fundamental" "ctrl-h k" "describe-key")
(define-key "fundamental" "ctrl-h f" "describe-function")
(define-key "fundamental" "ctrl-h a" "apropos")
(define-key "fundamental" "ctrl-h b" "describe-bindings")
(define-key "fundamental" "ctrl-h e" "list-messages")   ; echo-area scrollback
(define-key "fundamental" "ctrl-h l" "list-lsp-log")    ; language-server stderr
;; C-h t (help-with-tutorial) is defined in tutorial.scm, loaded after this.

(register-command "describe-key")
(register-command "describe-function")
(register-command "apropos")
(register-command "helm-apropos")
(register-command "describe-bindings")
(register-command "version")

;; *Messages* / *lsp-log* are normal, editable buffers (copy/paste, edit; new
;; entries append). No line-number gutter, and no syntax highlighting/LSP runs
;; on them (their major mode declares no language).
(set-option "line-numbers:messages-mode" "false")
