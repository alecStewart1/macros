;; Ghost text (copilot-style inline suggestions) — the thin Scheme layer over
;; the Rust primitives:
;;
;;   (ghost-text-show text)   render `text` as dim italic virtual text at point
;;                            (display-only; a multi-line suggestion pushes the
;;                            following display lines down)
;;   (ghost-text-clear)       dismiss it
;;   (ghost-text-accept)      insert the visible suggestion as one edit
;;
;; A ghost is deliberately ephemeral: any edit, any cursor motion, and C-g all
;; dismiss it, so a completion package just re-shows after each pause. The
;; canonical trigger loop is:
;;
;;   (add-hook 'after-change-functions 'my-ghost-request)  ; debounce inside,
;;   ...fetch a suggestion (e.g. via http-request)...      ; e.g. run-with-timer
;;   (ghost-text-show suggestion)
;;
;; The `ghost` keymap below is consulted only while a suggestion is visible
;; (like the `completion` keymap while the popup is open); unbound keys fall
;; through, so the ghost never swallows ordinary typing.
(define-key "ghost" "tab" 'ghost-text-accept)
