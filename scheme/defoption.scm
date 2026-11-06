;; requires: core

;; defoption.scm — a `defcustom`-style declaration for user-customizable
;; Scheme variables. Doc: web/content/docs/defoption.md
;;
;; Emacs's defcustom doesn't prevent `set!` any more than defvar does — what
;; it buys is DECLARED metadata: a docstring, a type, and the standard value,
;; plus the contract that the variable is public API the Custom UI may write.
;; This module is the Scheme-side equivalent for Macros:
;;
;;   (defoption *spell-program* "aspell"
;;     "External spell-checker program run by spell-mode."
;;     :type 'string)
;;
;; VALUE is the default; like Emacs defvar, init.scm (which loads AFTER the
;; bundled modules) overrides it by re-defining the name. VALUE is a CONSTANT
;; expression (a literal or quoted datum) — it is evaluated for both the
;; define and the registration, never re-run later. Steel's expander refuses
;; DOTTED PAIRS in macro arguments, so a dotted-alist default must be built
;; without the dot syntax (e.g. via `cons` at load, or a flat list). The
;; registered metadata —
;; not the value — is the point: help.scm renders the type, default, and
;; current-vs-default drift in describe-variable, and `customize-option`
;; (help-extras.scm) lists ONLY declared options, so "what can I tune" is
;; introspectable instead of tribal knowledge.
;;
;; Undeclared `*star*` globals keep their current behavior (visible in the
;; pickers, tagged undeclared) so modules migrate to defoption incrementally.
;; Internal scratch state should use the `--` prefix instead of stars so it
;; drops out of describe-variable at the next rename.

;; --- registry ----------------------------------------------------------------
;; name STRING -> (doc type default) alist, hash-backed (O(1) lookups; the
;; default is the EVALUATED value, as written in the defoption form).
(define *defoption-registry* (hash))

(define (defoption--register! name-sym default doc type)
  (set! *defoption-registry*
        (hash-insert *defoption-registry*
                     (symbol->string name-sym)
                     (list (cons 'doc doc)
                           (cons 'type type)
                           (cons 'default default)))))

;; A defoption name (string or symbol) with a registration, or #f.
(define (defoption--entry name)
  (hash-try-get *defoption-registry*
                (if (symbol? name) (symbol->string name) name)))

(define (defoption-registered? name) (if (defoption--entry name) #t #f))

;; The registration as a (doc type default) alist — #f for an unregistered
;; name, so callers can distinguish "not declared" from a prop whose value
;; is #f.
(define (defoption-props name) (defoption--entry name))

(define (defoption--prop name key)
  (let ((e (defoption--entry name)))
    (if e (cdr (assq key e)) #f)))

(define (defoption-doc name)     (defoption--prop name 'doc))
(define (defoption-type name)    (defoption--prop name 'type))
(define (defoption-default name) (defoption--prop name 'default))

;; A registered option's value as readable text: strings quoted, booleans as
;; #t/#f, everything else as Steel prints it. Lives here (not help-extras)
;; because help.scm's apropos-pair annotates with it and help.scm loads
;; FIRST — the defoption module is its earliest dependency that carries it.
(define (defoption--value-text v)
  (cond ((string? v) (string-append "\"" v "\""))
        ((boolean? v) (if v "#t" "#f"))
        (else (to-string v))))

;; Every declared option name, sorted — the customize-option / apropos scope.
(define (defoption-list)
  (sort-strings (map car (hash->list *defoption-registry*))))

;; --- the macro ---------------------------------------------------------------
;; Grammar: (defoption NAME VALUE DOC [:type KIND])
;; KIND is one of 'boolean 'integer 'float 'string 'any (metadata for the
;; help/customize layer; 'any is the default when :type is omitted).
;; Expansion shape:
;;   (begin
;;     (define NAME <value>)                       ; the default, as written
;;     (defoption--register! 'NAME VALUE DOC 'KIND))   ; VALUE evaluated here
;;
;; Like define-minor-mode, the transformer returns a whole constructed datum
;; (defines land in the caller's scope) — no dotted patterns, keywords are
;; colon-prefixed symbols, and an unknown keyword or :type errors loudly at
;; expansion rather than being silently absorbed.
(define-syntax defoption
  (lambda (stx)
    (syntax-case stx ()
      [(_ name value doc rest ...)
       (let* ((name-sym (syntax->datum #'name))
              (doc-str (syntax->datum #'doc))
              ;; syntax->datum is SHALLOW: nested lists (a quoted 'symbol, a
              ;; list default) stay Syntax-wrapped — deep-convert exactly like
              ;; define-minor-mode's :keymap handling.
              (items (let deep ((x (syntax->datum #'(rest ...))))
                       (cond ((syntax? x) (deep (syntax->datum x)))
                             ((null? x) '())
                             ((pair? x) (cons (deep (car x)) (deep (cdr x))))
                             (else x))))
              (value-datum (let deep ((x (syntax->datum #'value)))
                             (cond ((syntax? x) (deep (syntax->datum x)))
                                   ((null? x) '())
                                   ((pair? x) (cons (deep (car x)) (deep (cdr x))))
                                   (else x)))))
         ;; Expander-time code can only call PRIMITIVES — module defines
         ;; don't exist yet at macro-expansion (compile) time, so the keyword
         ;; walk is inline here.
         (define (parse-keywords items type)
           (cond ((null? items) type)
                 ((and (pair? items) (pair? (cdr items))
                       (eq? (car items) ':type))
                  (let ((k (cadr items)))
                    ;; `:type 'string` arrives as the list (quote string) —
                    ;; unwrap the quote so the type is the bare symbol.
                    (let ((k (if (and (pair? k) (eq? (car k) 'quote)
                                      (pair? (cdr k)) (symbol? (cadr k)))
                                 (cadr k)
                                 k)))
                      (unless (and (symbol? k)
                                   (memq k '(boolean integer float string any)))
                        (error "defoption: unsupported :type:" k))
                      (parse-keywords (cddr items) k))))
                 ((and (pair? items) (symbol? (car items))
                       (starts-with? (symbol->string (car items)) ":"))
                  (error "defoption: unsupported keyword:" (car items)))
                 (else
                  (error "defoption: malformed keyword arguments:" items))))
         (unless (string? doc-str)
           (error "defoption: DOC must be a string:" doc-str))
         (let ((type (parse-keywords items 'any)))
           (list 'begin
                 (list 'define name-sym value-datum)
                 ;; VALUE is evaluated HERE too (constant expressions only)
                 ;; so the stored default is the value itself, whatever shape
                 ;; the literal had — no quote-wrapping guesswork.
                 (list 'defoption--register!
                       (list 'quote name-sym)
                       value-datum
                       doc-str
                       (list 'quote type)))))])))
