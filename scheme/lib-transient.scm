;; lib-transient — a popup menu of (key label command) rows, the magit /
;; transient.el pattern (the "lib-" name keeps this file sorting BEFORE
;; magit.scm, which builds its switch popups on the argument mechanism here). The popup UI and key dispatch live in Rust (`transient` opens it,
;; `transient-quit` closes it; pressing a row's key closes the popup and runs its
;; command). Everything else — argument toggles, nested menus — is plain Scheme.
;;
;; Infix ARGUMENTS (toggleable flags like `--force`) are modeled as Scheme state:
;; a flag key toggles membership in a set and re-opens the menu so its label
;; refreshes. A command suffix then reads the accumulated args.

(define *transient-args* '())

;; Is `flag` (e.g. "--force") currently active?
(define (transient-arg-active? flag)
  (if (member flag *transient-args*) #t #f))

;; Toggle a flag on/off, then re-show `menu` (a thunk that calls `transient`).
(define (transient-toggle-arg flag menu)
  (set! *transient-args*
        (if (member flag *transient-args*)
            (filter (lambda (a) (not (equal? a flag))) *transient-args*)
            (cons flag *transient-args*)))
  (menu))

;; Drop all accumulated arguments (call when a transient finishes).
(define (transient-reset-args)
  (set! *transient-args* '()))

;; Drop every argument under a namespace prefix (e.g. "commit:"), so popups
;; that namespace their flags ("popup:arg", magit-style) can clear just their
;; own after the action runs.
(define (transient-clear-args prefix)
  (set! *transient-args*
        (filter (lambda (a) (not (starts-with? a prefix))) *transient-args*)))

;; A checkbox label for a flag row: "[*] --force" when active, "[ ] --force".
(define (transient-flag-label flag)
  (string-append (if (transient-arg-active? flag) "[*] " "[ ] ") flag))
