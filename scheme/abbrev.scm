;; requires: core

;; abbrev — Emacs-style abbreviations, the Macros port of `abbrev`.
;; An abbrev is a short word that expands to something longer via an explicit
;; command (`expand-abbrev`, bound to C-x ' by default — rebind freely; if the
;; binding clashes with yasnippet, that binding choice is yours).
;;
;;   (define-abbrev 'global-abbrev-table "teh" "the")          ; global
;;   (define-abbrev 'rust-mode-abbrev-table "fnr" "fn $0")     ; rust-mode only
;;   (define-abbrev 'global-abbrev-table "ie" "i.e." #f
;;                  '(:case-fixed #t))                         ; with props
;;
;; Props (plist): :case-fixed, :enable-function, :ts-context.
;;
;; `abbrev-mode` is the M-x toggle: it adds/removes the abbrev completion-at-
;; point source. Tables auto-save silently on exit (`save-abbrevs` 'silent,
;; set-save-abbrevs to change) to `abbrev-file-name` (set-abbrev-file-name).
;;
;; There is NO typing-time auto-expansion: expansion happens only when you run
;; `expand-abbrev` (or, optionally, on insert-exit — see
;; set-abbrev-expand-on-insert-exit). Case rules: TEH → THE, Teh → The, and
;; `:case-fixed` expansions insert exactly as defined. Anonymous procedures as
;; hooks/predicates don't persist to the abbrev file — use 'symbol names.

;; table-name -> hash of abbrev -> property alist
(define *abbrev-tables* (hash))

;; --- tables ------------------------------------------------------------------
;; An abbrev table is a Steel hash: abbrev string -> property alist:
;;   ((expansion . "…") (hook . #f|proc-or-symbol) (case-fixed . bool)
;;    (enable-function . #f|proc-or-symbol) (ts-context . #f|"@comment"))
;; Tables live in *abbrev-tables* keyed by name; "global-abbrev-table" always
;; exists; a mode's table is "<mode>-abbrev-table", created on demand.

(define (abbrev--get-table name)
  (let ((t (hash-try-get *abbrev-tables* name)))
    (if t t (hash))))

(define (abbrev--put-table name t)
  (set! *abbrev-tables* (hash-insert *abbrev-tables* name t)))

(define (abbrev--plist-get plist key)
  (cond ((null? plist) #f)
        ((eq? (car plist) key) (cadr plist))
        (else (abbrev--plist-get (cddr plist) key))))

;; (define-abbrev-table SYMBOL DEFINITIONS &optional DOCSTRING &rest PROPS)
;; Emacs signature. SYMBOL is the quoted table name; DEFINITIONS is a list of
;; (NAME EXPANSION [HOOK [PROPS]]) lists. PROPS: (:parents '(other-table …)).
;; The table need not pre-exist an abbreviation — define-abbrev creates any
;; table on demand — but this is how users declare a table for a mode (or a
;; mode GROUP, via :parents) explicitly, Emacs-style.
;;   (define-abbrev-table 'prog-mode-abbrev-table '()
;;     "Shared across all prog modes" '(:parents '()))
;;   (define-abbrev-table 'rust-mode-abbrev-table '()
;;     nil '(:parents '(prog-mode-abbrev-table)))
(define (define-abbrev-table symbol definitions . rest)
  (let* ((props (if (null? rest) '() (car (last rest))))
         (parents (abbrev--plist-get props ':parents))
         (tname (symbol->string symbol))
         (t (abbrev--get-table tname)))
    (when parents (abbrev--put-parents tname parents))
    ;; definitions: ((name expansion [hook [props]]) …)
    (for-each
     (lambda (d)
       ;; no cadddr in this Steel build — use list-ref (verified: list-ref ✓)
       (apply define-abbrev
              symbol (car d) (cadr d)
              (append (if (> (length d) 2) (list (list-ref d 2)) '(#f))
                      (if (> (length d) 3) (list (list-ref d 3)) '()))))
     definitions)))

;; :parents — list of table names tried after the local table, before global.
(define *abbrev-table-parents* (hash))   ; table-name -> list of parent names
(define (abbrev--put-parents tname parents)
  (set! *abbrev-table-parents* (hash-insert *abbrev-table-parents* tname parents)))

;; (abbrev-table-p SYMBOL) — #t when SYMBOL names an existing abbrev table.
(define (abbrev-table-p symbol)
  (let ((t (hash-try-get *abbrev-tables* (symbol->string symbol))))
    (if t #t #f)))

;; (define-abbrev TABLE NAME EXPANSION &optional HOOK PROPS) — Emacs signature.
;; TABLE is a quoted symbol ('global-abbrev-table, 'rust-mode-abbrev-table, …).
;; HOOK is a procedure or 'symbol run after expansion (pass #f for none).
;; PROPS is a plist: (:case-fixed #t :enable-function 'pred :ts-context "@comment").
;; Redefining an abbrev REPLACES it (hash-insert semantics).
(define (define-abbrev table name expansion . rest)
  (let* ((hook (if (null? rest) #f (car rest)))
         (props (if (or (null? rest) (null? (cdr rest))) '() (cadr rest)))
         (tname (symbol->string table))
         (t (abbrev--get-table tname))
         (entry (list (cons 'expansion expansion)
                      (cons 'hook hook)
                      (cons 'case-fixed (abbrev--plist-get props ':case-fixed))
                      (cons 'enable-function (abbrev--plist-get props ':enable-function))
                      (cons 'ts-context (abbrev--plist-get props ':ts-context)))))
    (abbrev--put-table tname (hash-insert t name entry))))

;; (clear-abbrev [table]) — remove every abbrev from TABLE (default global).
(define (clear-abbrev . table)
  (abbrev--put-table
   (symbol->string (if (null? table) 'global-abbrev-table (car table))) (hash)))

;; (abbrev-symbol abbrev [local-table]) — the property alist for ABBREV.
;; Resolution order: local (mode) table → :parents (in list order, via
;; *abbrev-table-parents*) → global. #f when none has it.
(define (abbrev-symbol abbrev . local)
  (let* ((local-name (if (null? local)
                         (string-append (major-mode) "-abbrev-table")
                         (symbol->string (car local))))
         (hit (or (hash-try-get (abbrev--get-table local-name) abbrev)
                  (abbrev--lookup-parents local-name abbrev))))
    (if hit hit (hash-try-get (abbrev--get-table "global-abbrev-table") abbrev))))

(define (abbrev--lookup-parents tname abbrev)
  (let loop ((parents (hash-try-get *abbrev-table-parents* tname)))
    (cond ((null? parents) #f)
          ((hash-try-get (abbrev--get-table (car parents)) abbrev))
          (else (loop (cdr parents))))))

