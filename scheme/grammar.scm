;; requires: core

;; grammar.scm — install user tree-sitter grammars at runtime.
;;
;; `install-language` downloads a grammar repo, compiles its committed parser
;; with the host C/C++ toolchain, and loads it — so macros can highlight a
;; language it wasn't built with. The heavy work (git clone + compile) runs on a
;; background thread in Rust (`grammar-install` → Editor::install_grammar); this
;; file is just the ergonomic Scheme front door.
;;
;; Put a call in your init.scm, pinning a revision for reproducibility:
;;
;;   (install-language "nim"
;;                     "https://github.com/alaviss/tree-sitter-nim"
;;                     "a4b3e3a"          ; commit / tag / branch
;;                     '("nim" "nims"))   ; file extensions (no dot)
;;
;; Two optional trailing args cover real-world repos:
;;
;;   symbol — the grammar's parser name when it differs from your id. The repo
;;            exports `tree_sitter_<symbol>`; defaults to the id. (Needed when you
;;            pick an id different from the grammar's own name.)
;;   subdir — the directory holding the grammar, for repos that nest it. E.g.
;;            tree-sitter-typescript keeps the grammar in `typescript/`:
;;
;;   (install-language "typescript"
;;                     "https://github.com/tree-sitter/tree-sitter-typescript"
;;                     "v0.23.2" '("ts")
;;                     "typescript"        ; symbol  → tree_sitter_typescript
;;                     "typescript")       ; subdir
;;
;; The first install from a given repo asks for confirmation in the minibuffer
;; ("Install grammar X from <repo> (downloads and runs native code)?") — loading
;; a grammar runs arbitrary native code, so a new source is trusted explicitly
;; once and remembered (config dir, `trusted-grammars`); later launches don't
;; re-prompt.
;;
;; Once installed, opening any matching file highlights automatically (the
;; extension lookup falls back to the runtime registry). The grammar is cached
;; under the OS cache dir keyed by id+rev, so a later launch reuses the compiled
;; library instead of recompiling.
;;
;; Self-contained on purpose: this file doesn't require language-modes.scm, so
;; it can't lean on that file's helpers.

;; Define `<id>-mode` as a nullary interactive command, so once the grammar has
;; loaded you can `M-x <id>-mode` on any buffer to colorize it (set-major-mode is
;; registry-aware). Defined eagerly at install time because the name is data; the
;; command is harmless until the grammar finishes loading. Mirrors
;; language-modes.scm's bundled-language commands.
(define (grammar--define-mode id)
  (let ((cmd (string-append id "-mode")))
    (eval-string
     (string-append "(define (" cmd ") "
                    "(set-major-mode \"" id "\") "
                    "(message \"" id " mode\"))"))
    (register-command cmd)))

;; Download, compile, and load the grammar at `repo`@`rev`, registering it for
;; the given file `extensions` (a list of bare extensions). Optional trailing
;; args: `symbol` (parser name, defaults to id) and `subdir` (grammar location in
;; the repo, defaults to the root). Returns immediately; progress and the final
;; result echo to the minibuffer.
(define (install-language id repo rev extensions . opts)
  (let ((symbol (if (>= (length opts) 1) (list-ref opts 0) ""))
        (subdir (if (>= (length opts) 2) (list-ref opts 1) "")))
    (grammar--define-mode id)
    (grammar-install id repo rev (string-join extensions " ") symbol subdir)))
