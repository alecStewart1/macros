;; multiple-cursors — edit at many places at once, the Macros port of mc.el /
;; VS Code multi-cursor. Add secondary cursors, then typed text, Enter, and
;; Backspace are replicated at every cursor. C-g (or M-x mc-clear) collapses
;; back to one.
;;
;;   C-c C-n / M-x mc-add-cursor-below     add a cursor on the next line
;;   C-c C-p                               add a cursor on the previous line
;;   C-c C-d                               add a cursor at the next occurrence
;;                                         of the word at point (mc/mark-next)
;;   C-g                                   remove all secondary cursors
;;
;; The cursors, replicated edits, and rendering are all in the core
;; (src/editor/mod.rs); secondary cursors are marker-backed so they track edits.
;; Note: plain motions move only the primary cursor for now — add cursors, then
;; type/delete.

(define-key "fundamental" "ctrl-c ctrl-n" "mc-add-cursor-below")
(define-key "fundamental" "ctrl-c ctrl-p" "mc-add-cursor-above")
(define-key "fundamental" "ctrl-c ctrl-d" "mc-mark-next-like-this")

(register-command "mc-add-cursor-below")
(register-command "mc-add-cursor-above")
(register-command "mc-mark-next-like-this")
(register-command "mc-clear")
