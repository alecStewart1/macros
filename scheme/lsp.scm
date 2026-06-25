;; lsp.scm — LSP navigation, documentation, refactoring, and formatting on top
;; of the Rust protocol layer. The heavy lifting (sending requests, routing
;; responses, jumping to locations, applying edits) is in Rust; this file wires
;; the user-facing commands, the list-buffer keymaps, and the default keys.
;;
;; Available commands (all M-x-discoverable):
;;   lsp-find-definition       go to the definition of the symbol at point
;;   lsp-find-declaration      go to its declaration
;;   lsp-find-type-definition  go to the type's definition
;;   lsp-find-implementation   go to implementation(s)
;;   lsp-find-references       list all references (RET to visit)
;;   lsp-hover                 documentation for the symbol at point
;;   lsp-signature-help        signature of the call at point
;;   lsp-document-symbols      symbols in this buffer (RET to visit)
;;   lsp-workspace-symbol      query symbols across the project
;;   lsp-rename                rename the symbol everywhere
;;   lsp-format-buffer         format the whole buffer
;;   lsp-code-action           offer code actions / quick fixes

;; --- minibuffer-driven commands (need an argument) --------------------------

;; Rename is a Rust builtin (`lsp-rename`): it validates the position with
;; prepareRename, then prompts (prefilled) and applies the workspace edit.

;; Workspace symbol: read a query, then Rust runs `workspace/symbol`.
(define (lsp-workspace-symbol)
  (read-string "Symbol: " "lsp-workspace-symbol-go"))

;; --- result list buffers (*lsp-xref*, *lsp-symbols*) ------------------------
;; Read-only; RET visits the target for the current row, q closes, n/p move.
;; A reference/symbol list, not code — no line-number gutter.
(set-option "line-numbers:lsp-xref-mode" "false")
(define-key "lsp-xref-mode" "enter" "lsp-xref-visit")
(define-key "lsp-xref-mode" "q" "kill-buffer")
(define-key "lsp-xref-mode" "n" "next-line")
(define-key "lsp-xref-mode" "p" "previous-line")

;; Code actions buffer: RET applies the action on the current row.
(define-key "lsp-actions-mode" "enter" "lsp-apply-code-action")
(define-key "lsp-actions-mode" "q" "kill-buffer")
(define-key "lsp-actions-mode" "n" "next-line")
(define-key "lsp-actions-mode" "p" "previous-line")

;; Hover documentation buffer (multi-line hovers render here).
(define-key "lsp-doc-mode" "q" "kill-buffer")

;;; ===========================================================================
;;; GENERIC LSP BRIDGE — features built in Scheme on top of `(lsp-request …)`.
;;;
;;; The Rust layer exposes ONE request primitive and a few editing helpers:
;;;   (lsp-request method kind params callback)  — send any request; `kind` is
;;;       "position" | "document" | "range" | "region" | "positions" | "raw"
;;;       (the textDocument/position/range is filled in for you), `params` is a
;;;       JSON object string merged over that base (or "" for none), and the
;;;       result JSON is delivered to Steel fn `callback`.
;;;   (lsp-goto uri line col)                    — open + jump (pushes a marker)
;;;   (lsp-apply-workspace-edit json)            — apply a WorkspaceEdit
;;;   (lsp-apply-text-edits json)                — apply TextEdit[] to this buffer
;;;   (position->byte line col)                  — LSP position -> byte offset
;;;   (set-region start end)                     — select a byte range
;;; Plus the existing render primitives (render-buffer, overlay-set-face,
;;; set-line-suffix, fold-lines, …). Steel's `string->jsexpr` parses results
;;; (objects -> hashes with symbol keys, arrays -> lists, numbers -> floats).
;;; Everything below is ordinary Scheme you can copy, rebind, or replace.
;;; ===========================================================================

;; --- JSON helpers -----------------------------------------------------------
(define (lsp-int x) (if (number? x) (exact (round x)) 0))   ; JSON num -> int
(define (jget o k) (if (hash? o) (hash-try-get o k) #f))    ; safe field access
(define (as-list r) (cond ((list? r) r) ((hash? r) (list r)) (else '())))

;; A range's start position, as byte offsets in the current buffer.
(define (range-start-byte r)
  (let ((s (jget r 'start)))
    (position->byte (lsp-int (jget s 'line)) (lsp-int (jget s 'character)))))
(define (range-end-byte r)
  (let ((e (jget r 'end)))
    (position->byte (lsp-int (jget e 'line)) (lsp-int (jget e 'character)))))
(define (range-start-line r) (lsp-int (jget (jget r 'start) 'line)))

;; --- jump-back (Emacs pop-tag-mark / vim C-o) -------------------------------
;; `xref-push-marker` / `xref-pop-marker` are Rust builtins; built-in jumps push
;; automatically. Nothing to define — just bound below.

;; --- document highlight -----------------------------------------------------
;; Highlight every occurrence of the symbol at point (overlay face "lsp-highlight").
(define (lsp-document-highlight)
  (lsp-request "textDocument/documentHighlight" "position" "" "lsp--highlight-apply"))
(define (lsp--highlight-apply json)
  (clear-overlay-faces)             ; only faces — keep folds/suffixes/semantic
  (for-each
   (lambda (h)
     (let ((r (jget h 'range)))
       (when (hash? r)
         (overlay-set-face (range-start-byte r) (range-end-byte r) "lsp-highlight"))))
   (as-list (string->jsexpr json))))
(define (lsp-highlight-clear) (clear-overlay-faces))

;; --- inlay hints ------------------------------------------------------------
;; Show parameter/type hints as end-of-line virtual text. Re-run to refresh.
(define (lsp-inlay-hints)
  (lsp-request "textDocument/inlayHint" "range" "" "lsp--inlay-apply"))
(define (lsp--inlay-label lbl)        ; label is a string OR a list of {value}
  (cond ((string? lbl) lbl)
        ((list? lbl) (apply string-append (map (lambda (p) (or (jget p 'value) "")) lbl)))
        (else "")))
(define (lsp--inlay-apply json)
  (clear-line-suffixes)             ; drop stale hints before repainting
  (for-each
   (lambda (h)
     (let ((line (lsp-int (jget (jget h 'position) 'line)))
           (text (lsp--inlay-label (jget h 'label))))
       (set-line-suffix line (string-append "  " text) "lsp-inlay-hint")))
   (as-list (string->jsexpr json))))

;; --- code lens --------------------------------------------------------------
;; Show resolved lens titles as virtual text above their line's end.
;; `lsp-code-lens` is a Rust builtin: it requests lenses, resolves those whose
;; command isn't inlined (codeLens/resolve), and renders titles as virtual text.

;; --- folding ----------------------------------------------------------------
;; Fold every server-reported region (use `unfold-all` to reveal).
(define (lsp-fold-all)
  (lsp-request "textDocument/foldingRange" "document" "" "lsp--fold-apply"))
(define (lsp--fold-apply json)
  (for-each
   (lambda (f)
     (fold-lines (lsp-int (jget f 'startLine)) (+ 1 (lsp-int (jget f 'endLine)))))
   (as-list (string->jsexpr json))))

;; --- selection range (expand-region / contract) -----------------------------
;; Grow the selection to the smallest server range strictly larger than it;
;; `lsp-contract-selection` shrinks back through the history.
(define *lsp-sel-history* '())   ; stack of (start end) byte ranges
(define (lsp-expand-selection)
  (lsp-request "textDocument/selectionRange" "positions" "" "lsp--expand-apply"))
(define (lsp--cur-start) (if (region-active?) (region-beginning) (point)))
(define (lsp--cur-end)   (if (region-active?) (region-end) (point)))
(define (lsp--expand-apply json)
  (let ((ranges (as-list (string->jsexpr json))))
    (when (not (null? ranges))
      (set! *lsp-sel-history*
            (cons (list (lsp--cur-start) (lsp--cur-end)) *lsp-sel-history*))
      (lsp--expand-walk (car ranges)))))
(define (lsp-contract-selection)
  (if (null? *lsp-sel-history*)
      (message "No selection to contract")
      (let ((prev (car *lsp-sel-history*)))
        (set! *lsp-sel-history* (cdr *lsp-sel-history*))
        (set-region (car prev) (car (cdr prev))))))
(define (lsp--expand-walk node)        ; node = {range, parent}
  (let* ((r (jget node 'range))
         (s (range-start-byte r))
         (e (range-end-byte r))
         (cs (lsp--cur-start))
         (ce (lsp--cur-end)))
    (if (and (<= s cs) (>= e ce) (or (< s cs) (> e ce)))
        (set-region s e)               ; strictly larger -> select it
        (let ((parent (jget node 'parent)))
          (if (hash? parent) (lsp--expand-walk parent) (set-region s e))))))

;; --- document links ---------------------------------------------------------
;; Open the link on the current line (file targets jump; others open as paths).
(define (lsp-open-link)
  (lsp-request "textDocument/documentLink" "document" "" "lsp--open-link-apply"))
(define (lsp--open-link-apply json)
  (let ((cur (buffer-line)))
    (let loop ((ls (as-list (string->jsexpr json))))
      (cond ((null? ls) (message "No link on this line"))
            ((= (range-start-line (jget (car ls) 'range)) cur)
             (let ((target (jget (car ls) 'target)))
               (if target (lsp-goto target 0 0) (message "Link has no target"))))
            (else (loop (cdr ls)))))))

;; --- worked example: definition via the bridge (for your own commands) ------
;; This reimplements go-to-definition purely in Scheme. The built-in
;; `lsp-find-definition` (Rust) is bound by default, but you can rebind M-. to
;; this — or copy it as a template for custom navigation.
(define (lsp-definition-scheme)
  (lsp-request "textDocument/definition" "position" "" "lsp--definition-go"))
(define (lsp--definition-go json)
  (let* ((r (string->jsexpr json))
         (loc (cond ((list? r) (if (null? r) #f (car r))) ((hash? r) r) (else #f))))
    (if (not loc)
        (message "No definition found")
        (let ((uri (or (jget loc 'uri) (jget loc 'targetUri)))
              (rng (or (jget loc 'range) (jget loc 'targetSelectionRange) (jget loc 'targetRange))))
          (lsp-goto uri (range-start-line rng)
                    (lsp-int (jget (jget rng 'start) 'character)))))))

;; --- semantic tokens --------------------------------------------------------
;; Color the buffer from the server's semantic tokens (overlays, face
;; "lsp-sem-<type>", e.g. lsp-sem-function). Layers over tree-sitter; opt-in.
;; Uses the delta path: the first call is a full request (cached), later calls
;; send only the changes — the efficient live-refresh path.
(define (lsp-semantic-tokens-apply)
  (lsp-semantic-tokens-delta "lsp--semantic-apply"))
(define (lsp--semantic-apply records)
  (clear-overlay-faces)             ; repaint from scratch each refresh
  (for-each
   (lambda (rec)
     (let ((parts (split-many rec "\t")))
       (when (>= (length parts) 4)
         (let* ((line (string->number (list-ref parts 0)))
                (col  (string->number (list-ref parts 1)))
                (len  (string->number (list-ref parts 2)))
                (type (list-ref parts 3)))
           (overlay-set-face (position->byte line col)
                             (position->byte line (+ col len))
                             (string-append "lsp-sem-" type))))))
   (split-many records "\n")))

;;; ===========================================================================
;;; ERGONOMIC PER-LANGUAGE CONFIG — the one form to set up a language.
;;;
;;;   (lsp-language "rust"
;;;     'extensions     '("rs")
;;;     'command        "rust-analyzer"
;;;     'format-on-save 'lsp)              ; 'lsp | (list "rustfmt") | "scheme-fn" | #f
;;;
;;;   (lsp-language "go"
;;;     'extensions     '("go")
;;;     'command        "gopls"
;;;     'format-on-save (list "gofmt")     ; shell out: buffer text on stdin -> stdout
;;;     'inlay-hints    #t                 ; on by default; #f to disable
;;;     'code-lens      #t
;;;     'semantic-tokens #t)
;;;
;;; 'language-ids is an optional alist of (ext . id) overriding the wire
;;; `languageId` per extension — needed when one server handles several file
;;; kinds, e.g. '(("tsx" . "typescriptreact")) so tsserver parses JSX.
;;;
;;; format-on-save values: 'lsp uses textDocument/formatting; a LIST is a shell
;;; command fed the buffer on stdin (rustfmt, gofmt, prettier, black, …); a
;;; STRING is the name of a Scheme function to call. Call it from your
;;; ~/.config/macros/init.scm (loaded after this file).
;;; ===========================================================================

(define *lsp-lang-exts*    '())  ; lang -> (ext …)
(define *lsp-formatters*   '())  ; ext  -> formatter spec (list | string)
(define *lsp-auto-features* '()) ; ext  -> fn name to run on open/save

(define (lsp--alist-get alist key)
  (cond ((null? alist) #f)
        ((equal? (car (car alist)) key) (cdr (car alist)))
        (else (lsp--alist-get (cdr alist) key))))
(define (lsp--plist-get plist key)
  (cond ((or (null? plist) (null? (cdr plist))) #f)
        ((equal? (car plist) key) (car (cdr plist)))
        (else (lsp--plist-get (cdr (cdr plist)) key))))
;; Whether `key` was supplied at all — lets a flag default to on while still
;; honoring an explicit `'key #f` (which `lsp--plist-get` can't distinguish from
;; "absent").
(define (lsp--plist-has? plist key)
  (cond ((or (null? plist) (null? (cdr plist))) #f)
        ((equal? (car plist) key) #t)
        (else (lsp--plist-has? (cdr (cdr plist)) key))))

(define (lsp-language lang . opts)
  (let ((exts     (lsp--plist-get opts 'extensions))
        (cmd      (lsp--plist-get opts 'command))
        (fos      (lsp--plist-get opts 'format-on-save))
        ;; Inlay hints are on by default; pass 'inlay-hints #f to opt out.
        (inlay    (if (lsp--plist-has? opts 'inlay-hints)
                      (lsp--plist-get opts 'inlay-hints)
                      #t))
        (lenses   (lsp--plist-get opts 'code-lens))
        (semantic (lsp--plist-get opts 'semantic-tokens))
        ;; Optional per-extension wire languageId overrides, as an alist of
        ;; (ext . id) — e.g. '(("tsx" . "typescriptreact")). When an extension
        ;; isn't listed, the server uses `lang` as the id (the common case).
        (lang-ids (lsp--plist-get opts 'language-ids)))
    (when exts
      (for-each (lambda (e) (set-extension e lang)) exts)
      (set! *lsp-lang-exts* (cons (cons lang exts) *lsp-lang-exts*)))
    (when lang-ids
      (for-each (lambda (pair) (set-language-id (car pair) (cdr pair))) lang-ids))
    (when cmd (set-lsp lang cmd))
    (when fos (lsp--configure-formatter lang fos))
    (when inlay    (lsp--auto-feature lang "lsp-inlay-hints"))
    (when lenses   (lsp--auto-feature lang "lsp-code-lens"))
    (when semantic (lsp--auto-feature lang "lsp-semantic-tokens-apply"))))

(define (lsp--configure-formatter lang spec)
  (if (equal? spec 'lsp)
      (set-lsp-format-on-save lang #t)         ; async LSP format, handled in Rust
      (for-each (lambda (e) (set! *lsp-formatters* (cons (cons e spec) *lsp-formatters*)))
                (or (lsp--alist-get *lsp-lang-exts* lang) '()))))

(define (lsp--auto-feature lang fnname)
  (for-each (lambda (e) (set! *lsp-auto-features* (cons (cons e fnname) *lsp-auto-features*)))
            (or (lsp--alist-get *lsp-lang-exts* lang) '())))

;; The extension of a file name ("src/main.rs" -> "rs"; "" if none).
(define (lsp--file-ext name)
  (let loop ((i (- (string-length name) 1)))
    (cond ((< i 0) "")
          ((equal? (substring name i (+ i 1)) ".") (substring name (+ i 1) (string-length name)))
          ((equal? (substring name i (+ i 1)) "/") "")
          (else (loop (- i 1))))))

;; before-save-hook: run the formatter configured for this file's extension. A
;; shell formatter gets the buffer on stdin and replaces it with stdout; a
;; Scheme-function name is just called. (LSP 'lsp formatting is async and runs
;; in Rust as part of the save.) Runs SYNCHRONOUSLY before the write.
(define (lsp--shell-format cmd)
  (let* ((result (call-process-full (car cmd) (cdr cmd) (buffer-string)))
         (code (car result))
         (out  (car (cdr result))))
    (when (and (equal? code "0") (> (string-length out) 0))
      (replace-region 0 (point-max) out))))
(define (lsp-run-before-save-formatter)
  (let ((spec (lsp--alist-get *lsp-formatters* (lsp--file-ext (current-buffer)))))
    (cond ((not spec) #f)
          ((list? spec) (lsp--shell-format spec))
          ((string? spec) (eval-string (string-append "(" spec ")")))
          (else #f))))
(add-hook "before-save-hook" "lsp-run-before-save-formatter")

;; Auto-features (inlay hints / code lens / semantic tokens): re-run on open and
;; after save for files whose language requested them.
;; Only runs when a server is ready, so opening a file before its server has
;; started is a silent no-op (no "No language server" spam).
(define (lsp-run-auto-features)
  (when (lsp-server-ready?)
    (let ((ext (lsp--file-ext (current-buffer))))
      (let loop ((fs *lsp-auto-features*))
        (when (not (null? fs))
          (when (equal? (car (car fs)) ext)
            (eval-string (string-append "(" (cdr (car fs)) ")")))
          (loop (cdr fs)))))))
(add-hook "find-file-hook" "lsp-run-auto-features")
(add-hook "after-save-hook" "lsp-run-auto-features")

;; Does the current buffer's language have any live feature configured?
(define (lsp--has-live-features?)
  (let ((ext (lsp--file-ext (current-buffer))))
    (let loop ((fs *lsp-auto-features*))
      (cond ((null? fs) #f)
            ((equal? (car (car fs)) ext) #t)
            (else (loop (cdr fs)))))))

;; Live refresh: coalesce bursts of edits with a counter — every edit schedules
;; a 300ms timer, but only the last to fire (counter back to 0) runs the
;; features. Zero cost for buffers/languages with nothing configured. This is
;; what makes inlay hints / semantic tokens update as you type (Zed-style).
(define *lsp-live-pending* 0)
(define (lsp-schedule-live)
  (when (lsp--has-live-features?)
    (set! *lsp-live-pending* (+ *lsp-live-pending* 1))
    (run-with-timer 300 "lsp-live-fire")))
(define (lsp-live-fire)
  (when (> *lsp-live-pending* 0)
    (set! *lsp-live-pending* (- *lsp-live-pending* 1)))
  (when (and (= *lsp-live-pending* 0) (lsp-server-ready?))
    (lsp-run-auto-features)))
(add-hook "after-change-functions" "lsp-schedule-live")

;; --- default keybindings (Emacs / eglot-style) ------------------------------
;; xref family
(define-key "fundamental" "alt-." "lsp-find-definition")     ; M-.  xref-find-definitions
(define-key "fundamental" "alt-?" "lsp-find-references")     ; M-?  xref-find-references
(define-key "fundamental" "ctrl-c ctrl-d" "lsp-hover")       ; doc at point
;; eglot's C-c C-l prefix for the rest
(define-key "fundamental" "ctrl-c ctrl-l d" "lsp-find-definition")
(define-key "fundamental" "ctrl-c ctrl-l D" "lsp-find-declaration")
(define-key "fundamental" "ctrl-c ctrl-l t" "lsp-find-type-definition")
(define-key "fundamental" "ctrl-c ctrl-l i" "lsp-find-implementation")
(define-key "fundamental" "ctrl-c ctrl-l r" "lsp-find-references")
(define-key "fundamental" "ctrl-c ctrl-l n" "lsp-rename")
(define-key "fundamental" "ctrl-c ctrl-l h" "lsp-hover")
(define-key "fundamental" "ctrl-c ctrl-l s" "lsp-document-symbols")
(define-key "fundamental" "ctrl-c ctrl-l S" "lsp-workspace-symbol")
(define-key "fundamental" "ctrl-c ctrl-l f" "lsp-format-buffer")
(define-key "fundamental" "ctrl-c ctrl-l a" "lsp-code-action")
(define-key "fundamental" "ctrl-c ctrl-l k" "lsp-signature-help")
;; the rest (built in Scheme above)
(define-key "fundamental" "alt-*" "xref-pop-marker")            ; M-*  pop-tag-mark
(define-key "fundamental" "ctrl-c ctrl-l p" "xref-pop-marker")
(define-key "fundamental" "ctrl-c ctrl-l H" "lsp-document-highlight")
(define-key "fundamental" "ctrl-c ctrl-l I" "lsp-inlay-hints")
(define-key "fundamental" "ctrl-c ctrl-l L" "lsp-code-lens")
(define-key "fundamental" "ctrl-c ctrl-l z" "lsp-fold-all")
(define-key "fundamental" "ctrl-c ctrl-l e" "lsp-expand-selection")
(define-key "fundamental" "ctrl-c ctrl-l E" "lsp-contract-selection")
(define-key "fundamental" "ctrl-c ctrl-l o" "lsp-open-link")
(define-key "fundamental" "ctrl-c ctrl-l T" "lsp-semantic-tokens-apply")
(define-key "fundamental" "ctrl-c ctrl-l c" "lsp-call-hierarchy-incoming")
(define-key "fundamental" "ctrl-c ctrl-l C" "lsp-call-hierarchy-outgoing")

;; --- M-x discoverability ----------------------------------------------------
;; The lsp-find-*/lsp-hover/lsp-format-buffer/call-hierarchy/xref-*/etc. commands
;; are Rust builtins, already in `command-list` — only the Scheme-defined entry
;; points need registering.
(register-command "lsp-workspace-symbol")
(register-command "lsp-document-highlight")
(register-command "lsp-highlight-clear")
(register-command "lsp-inlay-hints")
(register-command "lsp-code-lens")
(register-command "lsp-fold-all")
(register-command "lsp-expand-selection")
(register-command "lsp-contract-selection")
(register-command "lsp-open-link")
(register-command "lsp-semantic-tokens-apply")
;; lsp-rename and lsp-code-lens are Rust builtins (already in command-list).

;;; ===========================================================================
;;; DEFAULT LANGUAGE CONFIGURATION
;;;
;;; Reasonable out-of-the-box setup for the common languages, via the ergonomic
;;; `lsp-language` form (defined above — this is why config lives here in
;;; lsp.scm rather than init.scm, which loads first). Each language gets:
;;;   - extension -> language id mapping
;;;   - the language server command (started lazily on first matching file;
;;;     the binary must be installed and on PATH)
;;;   - format-on-save via the server's own formatter ('lsp): rust-analyzer ->
;;;     rustfmt, gopls -> gofmt, clangd -> clang-format, tsserver, pylsp. No
;;;     extra binary required, and a harmless no-op if the server can't format.
;;;   - inlay hints (on by default in `lsp-language`).
;;;
;;; To override any of this, redefine the language in ~/.config/macros/init.scm
;;; (loaded last, so it wins): swap the formatter (e.g. (list "gofmt") or
;;; (list "black" "-")), turn on 'code-lens / 'semantic-tokens, or pass
;;; 'format-on-save #f to disable formatting on save for that language.

(lsp-language "rust"
  'extensions     '("rs")
  'command        "rust-analyzer"
  'format-on-save 'lsp)

(lsp-language "go"
  'extensions     '("go")
  'command        "gopls"
  'format-on-save 'lsp)

(lsp-language "c"
  'extensions     '("c" "h")
  'command        "clangd"
  'format-on-save 'lsp)

(lsp-language "cpp"
  'extensions     '("cpp" "cc" "hpp")
  'command        "clangd"
  'format-on-save 'lsp)

;; .ts and .tsx share one server but must announce different languageIds:
;; tsserver only enables JSX parsing for the *react variants. Without this, a
;; .tsx file opens as plain "typescript" and every JSX element is a syntax error.
(lsp-language "typescript"
  'extensions     '("ts" "tsx")
  'language-ids   '(("ts" . "typescript") ("tsx" . "typescriptreact"))
  'command        "typescript-language-server --stdio"
  'format-on-save 'lsp)

(lsp-language "javascript"
  'extensions     '("js" "jsx")
  'language-ids   '(("js" . "javascript") ("jsx" . "javascriptreact"))
  'command        "typescript-language-server --stdio"
  'format-on-save 'lsp)

(lsp-language "python"
  'extensions     '("py")
  'command        "pylsp"
  'format-on-save 'lsp)

;; Scheme/Steel uses the built-in (in-process) server — no external binary, and
;; no reliable on-save formatter, so format-on-save and inlay hints are off.
(lsp-language "scheme"
  'extensions  '("scm" "ss" "rkt" "steel")
  'command     "builtin"
  'inlay-hints #f)

;; `macros-scheme` — the editor-aware Scheme server. Rust routes config /
;; extension files (under the config dir, or named *.macros.scm) to this
;; language, and seeds its built-in server with the whole editor API so editor
;; functions/commands complete and aren't flagged "unknown". No `extensions`
;; here: it's reached only via that path check, never by file suffix.
(set-lsp "macros-scheme" "builtin")
