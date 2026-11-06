;; requires: core help abbrev spell defoption

;; help-extras.scm — C-h v describe-variable and C-h m describe-mode.

;; --- describe-variable (C-h v) ------------------------------------------------
;; Variables are the Scheme globals named `*like-this*` (the convention every
;; bundled file follows) plus the editor options. Any other global can still
;; be typed by name. `help--variable-name?` is shared with help.scm's function
;; picker, which excludes these same names.
(define (variable-list)
  (sort-strings (dedup (append (filter help--variable-name? (function-list))
                               (option-list)
                               (defoption-list)))))

;; A value as readable text: strings quoted, everything else as Steel prints it.
(define (help--value-text v) (defoption--value-text v))

;; describe-variable (C-h v): a Helm picker over the variables (annotated like
;; the function picker — see apropos-pair in help.scm; options show their
;; current value). RET shows the doc.
(define (describe-variable)
  (helm (list (list "Variables"
                    (map apropos-pair (variable-list))
                    (list (list "Describe" 'describe-variable-show))))))
(define (describe-variable-show name)
  (cond
    ((equal? name "") #f)
    ((member name (option-list)) (describe-option name))
    ((defoption-registered? name) (describe-defoption name))
    ((not (member name (function-list)))
     (message (string-append "No variable named " name)))
    (else
     (let ((doc (function-doc name)))
       (render-buffer "*help*" "help-mode"
                      (string-append
                       name " is a Scheme global.\n\n"
                       "Value: " (help--value-text (eval-string name)) "\n\n"
                       (if (equal? doc "") "(no documentation)" doc) "\n\n"
                       "Set it from your config or M-: with\n"
                       "  (set! " name " <value>)")
                      '())))))

;; The declared user-option branch of describe-variable: type, default, the
;; current value, and a drift note when the two differ — the one piece of
;; "enforcement" defoption buys over a bare `*star*` global.
(define (describe-defoption name)
  (let* ((doc (defoption-doc name))
         (default (defoption-default name))
         (current (eval-string name))
         (drifted (not (equal? current default))))
    (render-buffer "*help*" "help-mode"
                   (string-append
                    name " is a user option (declared with defoption).\n\n"
                    "Type: " (symbol->string (defoption-type name)) "\n"
                    "Default value: " (help--value-text default) "\n"
                    "Current value: " (help--value-text current)
                    (if drifted
                        "  (changed from the default)"
                        "")
                    "\n\n"
                    (if (equal? doc "") "(no documentation)" doc)
                    "\n\n"
                    "Set it from your config or M-: with\n"
                    "  (set! " name " <value>)\n"
                    "or M-x customize-option.")
                   '())))

;; M-x customize-option: pick a DECLARED user option and set its value. Only
;; defoption-declared variables are listed — "what can I tune" is the
;; registry, not every global. The new value is read as a Scheme expression
;; (a string needs its quotes, like M-:); a failed read leaves the option
;; untouched.
(define (customize-option)
  (helm (list (list "User options"
                    (map apropos-pair (defoption-list))
                    (list (list "Set value" 'customize-option--set))))))
(define (customize-option--set name)
  (when (not (equal? name ""))
    (read-string
     (string-append "Set " name " to (Scheme expression): ")
     (lambda (input)
       (when (not (equal? input ""))
         ;; INPUT is evaluated as Scheme (a string needs its quotes, like
         ;; M-:); a malformed expression raises and the option is left
         ;; untouched — there is no catch primitive to pre-validate with, so
         ;; the engine's error echo is the feedback.
         (eval-string (string-append "(set! " name " " input ")"))
         (describe-defoption name))))))

;; --- describe-mode (C-h m) ----------------------------------------------------
;; The buffer's major mode, the minor modes on in it, and the bindings each
;; keymap adds. Minor modes are registered here with a predicate so packages
;; (and your config) can list theirs: (register-minor-mode "name" (lambda () …)).
;; NOT `*minor-modes*`: that is minor-mode.scm's define-minor-mode registry,
;; and a second top-level define of it here clobbered every mode registered
;; before this file loaded (abbrev-mode's toggle body among them).
(define *describe-minor-modes* '())   ; ((name . on?-thunk) …)
(define (register-minor-mode name on?)
  (set! *describe-minor-modes* (alist-set *describe-minor-modes* name on?)))
;; abbrev-mode and spell-mode are mode-stack entries, so they come through
;; enabled-minor-modes automatically.
(register-minor-mode "evil-mode" (lambda () (and (evil-enabled?) (not (helix-enabled?)))))
(register-minor-mode "helix-mode" (lambda () (helix-enabled?)))

;; Stack minor modes are the truth for define-minor-mode modes (they are
;; entries on the buffer's mode stack); the hand-registered predicates below
;; still cover modes that live outside the framework (evil/helix — editor
;; toggles, not stack entries).
(define (enabled-minor-modes)
  (sort-strings
   (dedup (append (let ((s (active-minor-modes)))
                    (if (equal? s "") '() (split-many s " ")))
                  (map car (filter (lambda (m) ((cdr m))) *describe-minor-modes*))))))

;; A keymap's bindings as indented "key    command" rows, or a note when empty.
(define (help--keymap-rows km)
  (let ((rows (sort-strings (keys-with-prefix km ""))))
    (if (null? rows)
        "  (no bindings of its own)"
        (join-lines (map (lambda (l) (string-append "  " (string-replace l "\t" "    "))) rows)))))

;; One "Key bindings (MODE):" section per active mode keymap, in resolution
;; order (minors, then the major chain — inherited parents included). The
;; major always gets a section; other modes only when they bind something.
;; The "<state>:<mode>" maps hold the same chords as the plain mode map
;; (minor-mode--install-scoped-keys binds both), so the plain map stands for
;; both.
(define (help--mode-key-sections major)
  (let* ((modal (current-modal-keymap))
         (modes (filter (lambda (km)
                          (not (or (string-contains? km ":")
                                   (equal? km "fundamental")
                                   (equal? km modal))))
                        (active-keymaps))))
    (foldr (lambda (km acc)
             (if (or (equal? km major)
                     (not (null? (keys-with-prefix km ""))))
                 (append (list (string-append "Key bindings (" km "):")
                               (help--keymap-rows km)
                               "")
                         acc)
                 acc))
           '()
           (if (member major modes) modes (cons major modes)))))

(define (describe-mode-text)
  (let* ((major (major-mode))
         (major (if (equal? major "") "fundamental" major))
         (lang (buffer-language))
         (doc (function-doc (string-append major "-mode")))
         (minors (enabled-minor-modes))
         (modal (current-modal-keymap)))
    (join-lines
     (append
      (list (string-append "Major mode: " major (if (equal? lang "") "" (string-append "  (language: " lang ")")))
            "")
      (if (equal? doc "") '() (list doc ""))
      (help--mode-key-sections major)
      (list "Enabled minor modes:"
            (if (null? minors) "  (none)" (join-lines (map (lambda (m) (string-append "  " m)) minors)))
            "")
      (if modal
          (list (string-append "Modal key bindings (" modal "):") (help--keymap-rows modal) "")
          '())
      (list "Global bindings: C-h B (describe-bindings).")))))

(define (describe-mode)
  (render-buffer "*help*" "help-mode" (describe-mode-text) '()))

(define-key "fundamental" "ctrl-h v" 'describe-variable)
(define-key "fundamental" "ctrl-h m" 'describe-mode)
(define-key "fundamental" "ctrl-h shift-o" 'customize-option)
(register-command 'customize-option)
(register-command 'describe-variable)
(register-command 'describe-mode)
