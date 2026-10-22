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
