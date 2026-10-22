;; requires: core completion hooks

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

;; :ts-context — syntax-context predicate --------------------------------------
;; (define-abbrev 'prog-mode-abbrev-table "x" "y" #f '(:ts-context "@comment"))
;; expands only when the abbrev sits inside a comment (a space-separated list
;; also works: '(:ts-context "@comment @string")). Names may carry the '@' of
;; tree-sitter highlight captures or be bare words; either way the enclosing
;; node KIND just has to contain the name (so "comment" matches the
;; line_comment/block_comment/comment node kinds across grammars).
;;
;; tree-sitter-node-at is callback-async, so we cache the enclosing node chain
;; (kind start end) at point and refresh it debounced after edits and cursor
;; moves — but only while some in-scope abbrev actually uses :ts-context
;; (zero cost otherwise). Reading the cache at expand time is synchronous, so
;; the predicate behaves like :enable-function; worst case the cache is one
;; edit cycle stale (the same trade rainbow.scm and spell.scm make). The cache
;; is buffer-global: with multiple cursors, :ts-context follows the PRIMARY
;; cursor only. In a grammar-less buffer no callback ever fires, so an abbrev
;; with :ts-context simply never expands — a visible, documented degradation.

(define *abbrev-ts-captures* '())    ; (kind start end) records, last refresh
(define *abbrev-ts-hooked* #f)

;; #t when any table in scope has an abbrev with a :ts-context prop.
(define (abbrev--ts-any-wanted?)
  (let outer ((ts (hash->list *abbrev-tables*)))
    (if (null? ts)
        #f
        (let inner ((es (hash->list (cdr (car ts)))))
          (cond ((null? es) (outer (cdr ts)))
                ((cdr (assoc 'ts-context (cdr (car es)))) #t)
                (else (inner (cdr es))))))))

;; Refresh the cache: node chain at point, primary cursor. Async — the result
;; lands in abbrev--ts-apply. No-grammar buffers just never call back.
(define (abbrev--ts-refresh)
  (when (abbrev--ts-any-wanted?)
    (tree-sitter-node-at (point) 'abbrev--ts-apply)))

;; Debounced schedule (core.scm's make-debounce): a burst of edits refreshes
;; once. The hooks call this by name.
(define abbrev--ts-schedule (make-debounce "abbrev-ts" 250 abbrev--ts-refresh))

;; Callback for tree-sitter-node-at: records are "kind\tstart\tend" lines
;; (byte offsets, innermost first). Return value is ignored.
(define (abbrev--ts-apply records)
  (set! *abbrev-ts-captures*
        (filter (lambda (x) x)
                (map (lambda (rec)
                       (let ((p (split-many rec "\t")))
                         (if (>= (length p) 3)
                             (list (nth p 0)
                                   (string->number (nth p 1))
                                   (string->number (nth p 2)))
                             #f)))
                     (non-empty-lines records))))
  #f)

;; Hook the refresh onto edit + motion only while some abbrev wants
;; :ts-context; called from define-abbrev / clear-abbrev.
(define (abbrev--ts-update-hooks)
  (let ((want (abbrev--ts-any-wanted?)))
    (cond ((and want (not *abbrev-ts-hooked*))
           (set! *abbrev-ts-hooked* #t)
           (add-hook "after-change-functions" "abbrev--ts-schedule")
           (add-hook "post-command-hook" "abbrev--ts-schedule"))
          ((and (not want) *abbrev-ts-hooked*)
           (set! *abbrev-ts-hooked* #f)
           (remove-hook "after-change-functions" "abbrev--ts-schedule")
           (remove-hook "post-command-hook" "abbrev--ts-schedule")))))

;; #t when NODE-KIND is one of NAMES (a '@'-stripped context name), either
;; exactly or by substring — so "comment" matches line_comment/block_comment.
(define (abbrev--kind-match? kind names)
  (cond ((null? names) #f)
        ((or (string=? kind (car names))
             (string-contains? kind (car names)))
         #t)
        (else (abbrev--kind-match? kind (cdr names)))))

;; Drop the leading '@' of a capture-style name, if any.
(define (abbrev--strip-at n)
  (if (and (> (string-length n) 0) (string=? "@" (substring n 0 1)))
      (substring n 1 (string-length n))
      n))

;; True when the entry has no :ts-context, or one of its context names matches
;; a cached node kind whose span overlaps START..END (byte offsets).
(define (abbrev--ts-context-ok entry start end)
  (let ((want (cdr (assoc 'ts-context entry))))
    (or (not want)
        (let ((names (map abbrev--strip-at (non-empty-lines (replace-regexp-in-string " " "\n" want)))))
          (let loop ((cs *abbrev-ts-captures*))
            (cond ((null? cs) #f)
                  ((and (abbrev--kind-match? (car (car cs)) names)
                        (<= (nth (car cs) 1) end)     ; overlap test: the
                        (>= (nth (car cs) 2) start))  ; spans share bytes
                   #t)
                  (else (loop (cdr cs)))))))))

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
        ((eq? (car plist) key) (abbrev--unquote (cadr plist)))
        (else (abbrev--plist-get (cddr plist) key))))

;; Inside a quoted plist, '(:enable-function 'pred) stores (quote pred), not
;; pred — peel one quote level so prop values come out usable.
(define (abbrev--unquote x)
  (if (and (pair? x) (eq? (car x) 'quote))
      (cadr x)
      x))

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
  (let* ((props (let scan ((xs rest))
                  ;; The PROPS plist is the last trailing list argument that
                  ;; starts with a keyword; skip a docstring/#f before it.
                  ;; (steel-core's `last` misbehaves on this shape, so walk
                  ;; manually.)
                  (cond ((null? xs) '())
                        ((null? (cdr xs))
                         (if (and (pair? (car xs))
                                  (not (string? (car (car xs)))))
                             (car xs)
                             '()))
                        (else (scan (cdr xs))))))
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
  (set! *abbrev-table-parents*
        (hash-insert *abbrev-table-parents* tname
                     ;; Accept both '(:parents (a b)) and '(:parents '(a b)):
                     ;; the inner quote leaves (quote a) elements behind.
                     ;; Names normalize to strings (table keys are strings).
                     (map (lambda (p)
                            (let ((u (abbrev--unquote p)))
                              (if (symbol? u) (symbol->string u) u)))
                          parents))))

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
    (abbrev--put-table tname (hash-insert t name entry))
    (abbrev--ts-update-hooks)))

;; (clear-abbrev [table]) — remove every abbrev from TABLE (default global).
(define (clear-abbrev . table)
  (abbrev--put-table
   (symbol->string (if (null? table) 'global-abbrev-table (car table))) (hash))
  (abbrev--ts-update-hooks))

;; (abbrev-symbol abbrev [local-table]) — the property alist for ABBREV.
;; Resolution order: local (mode) table → :parents (in list order, via
;; *abbrev-table-parents*) → global. #f when none has it. LOCAL-TABLE may be
;; given as the bare mode symbol ('rust-mode) or the full table name
;; ('rust-mode-abbrev-table); when omitted, the current buffer's major mode
;; table is used.
(define (abbrev-symbol abbrev . local)
  (let* ((given (if (null? local)
                    (major-mode)
                    (symbol->string (car local))))
         (local-name (if (and (>= (string-length given) 13)
                              (string=? (substring given
                                                    (- (string-length given) 13)
                                                    (string-length given))
                                        "-abbrev-table"))
                         given
                         (string-append given "-abbrev-table")))
         (hit (or (hash-try-get (abbrev--get-table local-name) abbrev)
                  (abbrev--lookup-parents local-name abbrev))))
    (if hit hit (hash-try-get (abbrev--get-table "global-abbrev-table") abbrev))))

(define (abbrev--lookup-parents tname abbrev)
  (let loop ((parents (hash-try-get *abbrev-table-parents* tname)))
    ;; hash-try-get yields #f when the table declares no parents — stop there
    ;; (Steel's null? does not accept #f).
    (cond ((not parents) #f)
          ((null? parents) #f)
          ((hash-try-get (abbrev--get-table (car parents)) abbrev))
          (else (loop (cdr parents))))))

;; --- expansion ---------------------------------------------------------------
(define (abbrev--word-char? s) (>= (string-match "[A-Za-z0-9_]" s) 0))

;; (start . word) for the identifier ending at point, or #f.
(define (abbrev--word-before)
  (let loop ((i (point)))
    (if (and (> i 0) (abbrev--word-char? (buffer-substring (- i 1) i)))
        (loop (- i 1))
        (if (< i (point)) (cons i (buffer-substring i (point))) #f))))

;; Case rule (Emacs default; skipped when :case-fixed is set):
;;   typed all-caps -> expansion all-caps     TEH -> THE
;;   Capitalized    -> Capitalized expansion  Teh -> The
;;   otherwise      -> expansion as defined
;; ASCII letters only: non-ASCII abbrevs (`café`) get the literal expansion.
(define (abbrev--apply-case word expansion entry)
  (if (cdr (assoc 'case-fixed entry))
      expansion
      (let ((all-upper? (abbrev--all-upper? word))
            (first-upper? (let ((cs (filter abbrev--alpha? (string->list word))))
                            (and (> (length cs) 0) (abbrev--upper? (car cs))))))
        (cond (all-upper? (string-upcase expansion))
              (first-upper?
               (string-append (string-upcase (substring expansion 0 1))
                              (substring expansion 1 (string-length expansion))))
              (else expansion)))))

;; NOTE: steel-core 0.8.x has NO every? / char-upper-case? — hand-rolled here
;; (verified against the steel-core primitive surface).
(define (abbrev--alpha? c) (or (abbrev--upper? c) (char<=? #\a c #\z)))
(define (abbrev--upper? c) (char<=? #\A c #\Z))
(define (abbrev--all-upper? word)
  (let ((cs (filter abbrev--alpha? (string->list word))))
    (and (> (length cs) 0)
         (let loop ((cs cs))
           (cond ((null? cs) #t)
                 ((abbrev--upper? (car cs)) (loop (cdr cs)))
                 (else #f))))))

;; HOOK and :enable-function accept a procedure or a 'symbol name (the same
;; convention as hooks.scm). Called with one argument.
(define (abbrev--call-pred f arg)
  (cond ((procedure? f) (f arg))
        ((symbol? f) (eval-string (string-append "(" (symbol->string f)
                                                 " \"" arg "\")")))
        (else #f)))

;; v0 stub replaced: the real abbrev--ts-context-ok lives in the :ts-context
;; section above.

;; Shared expansion core: replace the word (START . END) with EXP, then run
;; HOOK. Queued edits apply in order (yasnippet.scm): delete the abbrev, then
;; insert the expansion at its start. Used by expand-abbrev and the
;; insert-exit wrappers.
(define (abbrev--expand-at start end exp hook)
  (delete-region start end)
  (goto-char start)
  (insert exp)
  (when hook (abbrev--call-pred hook exp)))

;; expand-abbrev — replace the word before point with its expansion (local
;; table first, then global). :enable-function runs first, then :ts-context;
;; the HOOK runs after a successful expansion. Primary cursor only.
(define (expand-abbrev)
  (let ((wb (abbrev--word-before)))
    (if (not wb)
        (message "No word before point")
        (let ((entry (abbrev-symbol (cdr wb))))
          (cond
            ((not entry)
             (message (string-append "No abbrev: " (cdr wb))))
            ((let ((pred (cdr (assoc 'enable-function entry))))
               (and pred (not (abbrev--call-pred pred (cdr wb)))))
             #f)  ; predicate declined — stay silent, like Emacs
            ((not (abbrev--ts-context-ok entry (car wb) (point)))
             #f)  ; wrong syntax context — silent
            (else
             (let ((exp (abbrev--apply-case (cdr wb)
                                            (cdr (assoc 'expansion entry))
                                            entry)))
               (abbrev--expand-at (car wb) (point) exp
                                  (cdr (assoc 'hook entry)))
               (message (string-append "Expanded: " (cdr wb))))))))))

(register-command 'expand-abbrev)
(define-key "fundamental" "ctrl-x '" 'expand-abbrev)

;; --- abbrev-mode: the M-x toggle ---------------------------------------------
;; Enabling registers the abbrev CAPF; disabling removes it.

(define *abbrev-mode-on* #f)
(define *abbrev-capf-registered* #f)
(define *abbrev-capf-min-prefix* 2)   ; decline prefixes shorter than this
(define (set-abbrev-capf-min-prefix n) (set! *abbrev-capf-min-prefix* n))

;; Completes a prefix against the local + global tables; local shadows global.
;; Candidates are (text annotation) pairs — annotation = the expansion.
;; Flag-gated (there is NO remove-completion-at-point-function primitive —
;; verified): the CAPF registers once, on first enable, and declines with '()
;; while the mode is off. Registration order = priority order, so a user who
;; wants abbrevs before dabbrev enables abbrev-mode early in init.scm.
(define (abbrev-capf prefix)
  (if (or (not *abbrev-mode-on*)
          (< (string-length prefix) *abbrev-capf-min-prefix*))
      '()
      (let loop ((entries (append (hash->list (abbrev--get-table
                                               (string-append (major-mode)
                                                              "-abbrev-table")))
                                  (hash->list (abbrev--get-table
                                               "global-abbrev-table"))))
                 (out '()))
        (if (null? entries)
            (reverse out)
            (let ((a (car (car entries))) (entry (cdr (car entries))))
              (if (and (abbrev--prefix? prefix a)
                       (not (string=? prefix a))
                       (not (assoc a out)))
                  (loop (cdr entries)
                        (cons (list a (cdr (assoc 'expansion entry))) out))
                  (loop (cdr entries) out)))))))

(define (abbrev--prefix? prefix s)
  ;; steel-core has no string-prefix? — hand-rolled substring compare.
  (and (>= (string-length s) (string-length prefix))
       (string=? prefix (substring s 0 (string-length prefix)))))

(define (abbrev-mode)
  (cond (*abbrev-mode-on*
         (set! *abbrev-mode-on* #f)
         (message "abbrev-mode disabled"))
        ((not *abbrev-capf-registered*)
         (set! *abbrev-capf-registered* #t)
         (set! *abbrev-mode-on* #t)
         (add-completion-at-point-function "abbrev-capf")
         (message "abbrev-mode enabled"))
        (else
         (set! *abbrev-mode-on* #t)
         (message "abbrev-mode enabled"))))

(register-command 'abbrev-mode)

;; --- expand-on-insert-exit ---------------------------------------------------
;; The `evil-want-abbrev-expand-on-insert-exit` analog: while enabled, leaving
;; insert mode (escape) expands the abbrev before point — silently. Mechanism:
;; `escape` in the evil/helix INSERT keymaps is rebound to a wrapper here
;; (never editing evil.scm/helix.scm); the state-exit command is invoked by
;; 'symbol name through eval-string, so this stays load-order safe no matter
;; where evil.scm/helix.scm land in the startup graph.

(define *abbrev-expand-on-insert-exit* #f)

(define (set-abbrev-expand-on-insert-exit v)
  (set! *abbrev-expand-on-insert-exit* v)
  (if v
      (begin
        (bind-key "evil-insert" "escape" 'abbrev--evil-insert-exit)
        (bind-key "helix-insert" "escape" 'abbrev--helix-insert-exit))
      (begin
        (bind-key "evil-insert" "escape" 'evil-normal-state)
        (bind-key "helix-insert" "escape" 'helix-normal-state))))

;; Expansion core shared with expand-abbrev: full predicate chain (case rules,
;; :enable-function, :ts-context) + hook, but SILENT — escape should not nag.
;; #t when an expansion happened.
(define (abbrev--maybe-expand-word)
  (let ((wb (abbrev--word-before)))
    (if (not wb)
        #f
        (let ((entry (abbrev-symbol (cdr wb))))
          (cond ((not entry) #f)
                ((let ((pred (cdr (assoc 'enable-function entry))))
                   (and pred (not (abbrev--call-pred pred (cdr wb)))))
                 #f)
                ((not (abbrev--ts-context-ok entry (car wb) (point)))
                 #f)
                (else
                 (abbrev--expand-at
                  (car wb) (point)
                  (abbrev--apply-case (cdr wb)
                                      (cdr (assoc 'expansion entry))
                                      entry)
                  (cdr (assoc 'hook entry)))
                 #t))))))

;; Shared insert-exit body: expand, then fall through to the state's own exit
;; command (by name — see the load-order note above).
(define (abbrev--insert-exit-common exit-name)
  (when *abbrev-expand-on-insert-exit*
    (abbrev--maybe-expand-word))
  (eval-string (string-append "(" exit-name ")")))

(define (abbrev--evil-insert-exit) (abbrev--insert-exit-common "evil-normal-state"))
(define (abbrev--helix-insert-exit) (abbrev--insert-exit-common "helix-normal-state"))

;; --- persistence -------------------------------------------------------------
;; The abbrev file is Scheme data: exactly the (define-abbrev …) forms that
;; rebuild the tables — so `read-abbrev-file` is just `load`, and any string
;; (tabs, newlines, quotes) round-trips through the escaped literal. The file
;; is a BACKUP; the in-memory tables are the source of truth, and re-reading
;; replaces entries (hash-insert semantics), never duplicates.
;;
;; Round-trips: expansions, 'symbol hooks, prop plists, :parents (emitted as a
;; define-abbrev-table form). Does NOT round-trip: anonymous procedures (a
;; lambda hook/predicate) — the entry is skipped with a ;; comment naming the
;; abbrev; keep those definitions in init.scm.

(define *abbrev-file-name* "")   ; default set at the bottom of this section
(define *save-abbrevs* 'silent)  ; 'silent — auto-save on exit, no messages

;; Scheme string literal escaping for the serialized forms: backslash first,
;; then quote, control chars (mirrors session.scm's session-escape idiom).
(define (abbrev--escape s)
  (string-replace
   (string-replace
    (string-replace
     (string-replace s "\\" "\\\\")
     "\"" "\\\"")
    "\n" "\\n")
   "\t" "\\t"))

;; Prop fragments: " :key value" pieces; only truthy props are emitted.
;; Anonymous procedures are not serializable — the whole entry is skipped.
(define (abbrev--prop-frag key val)
  (cond ((eq? val #t) (string-append " :" (symbol->string key) " #t"))
        ((symbol? val) (string-append " :" (symbol->string key) " '"
                                      (symbol->string val)))
        ((string? val) (string-append " :" (symbol->string key) " \""
                                      (abbrev--escape val) "\""))
        (else "")))

;; One (define-abbrev …) form for an entry, or #f when it can't round-trip.
(define (abbrev--entry-form tname name entry)
  (let ((exp (cdr (assoc 'expansion entry)))
        (hook (cdr (assoc 'hook entry)))
        (enable (cdr (assoc 'enable-function entry))))
    (if (or (and hook (procedure? hook))
            (and enable (procedure? enable)))
        #f  ; anonymous procedure — caller leaves a ;; comment
        (let* ((props (string-append
                       (if enable (abbrev--prop-frag 'enable-function enable) "")
                       (if (cdr (assoc 'case-fixed entry))
                           (abbrev--prop-frag 'case-fixed #t)
                           "")
                       (let ((ts (cdr (assoc 'ts-context entry))))
                         (if ts (abbrev--prop-frag 'ts-context ts) ""))))
               (props-arg (if (> (string-length props) 0)
                              (string-append " '(" props ")")
                              ""))
               (hook-arg (cond ((not hook) "#f")
                               ((symbol? hook)
                                (string-append "'" (symbol->string hook)))
                               (else "#f"))))
          (string-append "(define-abbrev '" tname
                         " \"" (abbrev--escape name) "\""
                         " \"" (abbrev--escape exp) "\""
                         " " hook-arg props-arg ")")))))

;; (define-abbrev-table 'name '() #f '(:parents '(…))) for :parents declarations.
(define (abbrev--parents-forms)
  (let loop ((ts (hash->list *abbrev-table-parents*)) (out '()))
    (if (null? ts)
        (apply string-append (reverse out))
        (let* ((tname (car (car ts)))
               (parents (cdr (car ts)))
               (names (apply string-append
                             (map (lambda (p)
                                    (string-append " '" p))
                                  parents))))
          (loop (cdr ts)
                (cons (string-append "(define-abbrev-table '"
                                     tname " '() #f '(:parents '(" names
                                     ")))\n")
                      out))))))

;; The whole abbrev file: parents declarations, then one form per entry.
;; Entries with anonymous hooks/predicates become ;; comments (see above).
(define (abbrev--serialize)
  (let loop ((ts (hash->list *abbrev-tables*)) (out '()))
    (if (null? ts)
        (string-append (abbrev--parents-forms)
                       (apply string-append (reverse out)))
        (let* ((tname (car (car ts)))
               (entries (hash->list (cdr (car ts))))
               (forms (let inner ((es entries) (acc '()))
                        (if (null? es)
                            (reverse acc)
                            (let ((name (car (car es)))
                                  (entry (cdr (car es))))
                              (let ((form (abbrev--entry-form tname name entry)))
                                (inner (cdr es)
                                       (cons (if form
                                                 (string-append form "\n")
                                                 (string-append ";; not saved (anonymous procedure): "
                                                                tname " " name "\n"))
                                             acc))))))))
          (loop (cdr ts) (append forms out))))))

;; Silent internal saver — never messages (the 'silent contract). No file is
;; written when *save-abbrevs* is #f or no abbrevs exist.
(define (abbrev--save)
  (when *save-abbrevs*
    (let ((s (abbrev--serialize)))
      (when (> (string-length s) 0)
        (write-file *abbrev-file-name* s)))))

;; User-facing explicit commands.
(define (write-abbrev-file . file)
  (write-file (if (null? file) *abbrev-file-name* (car file))
              (abbrev--serialize)))

;; (read-abbrev-file [path]) — the file is Scheme data, so reading is loading.
(define (read-abbrev-file . file)
  (load (if (null? file) *abbrev-file-name* (car file))))

(define (set-abbrev-file-name path) (set! *abbrev-file-name* path))
(define (set-save-abbrevs val) (set! *save-abbrevs* val))

(add-hook "kill-emacs-hook" "abbrev--save")

;; Default location; init.scm can override.
(set! *abbrev-file-name*
      (string-append (getenv "HOME") "/.config/macros/abbrevs.scm"))

(register-command 'write-abbrev-file)
(register-command 'read-abbrev-file)
