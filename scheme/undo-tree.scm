;; undo-tree / vundo — branching undo, the Macros port of undo-tree.el + vundo.
;; The buffer model now keeps undo history as a TREE, not a linear stack: undoing
;; and then editing branches rather than discarding the redone edits, so no work
;; is ever lost. `redo` re-applies the most recently created branch.
;;
;;   C-/         undo            (already bound)
;;   C-?         redo            re-apply the last branch
;;   M-x vundo   visualize       show the branch structure (read-only)
;;   evil: u undo, C-r redo
;;
;; The tree itself lives in Rust (src/editor/mod.rs); `redo` and the
;; `undo-tree-dump` bridge expose it. vundo is an informational view of the
;; active buffer's tree — actual movement is via undo / redo in the buffer.

(define-key "fundamental" "ctrl-?" "redo")
(define-key "evil-normal" "ctrl-r" "redo")

;; --- vundo: draw the branch structure --------------------------------------
(define (vundo) (undo-tree-dump "vundo--render"))

(define (vundo--indent depth)
  (if (<= depth 0) "" (string-append "  " (vundo--indent (- depth 1)))))

;; Each dump line is "idx\tparent\tcurrent?\tdepth\tsize".
(define (vundo--line rec)
  (let* ((p (split-many rec "\t"))
         (idx (nth p 0))
         (current (equal? (nth p 2) "1"))
         (depth (string->number (nth p 3)))
         (size (nth p 4)))
    (string-append (vundo--indent depth)
                   (if current "● " "○ ")
                   "state " idx
                   (if (equal? size "0") "" (string-append "  (" size " edit"
                                                            (if (equal? size "1") "" "s") ")"))
                   (if current "   ← current" ""))))

(define (vundo--render dump)
  (let ((lines (map vundo--line (non-empty-lines dump))))
    (render-buffer "*vundo*" "vundo-mode"
                   (if (null? lines)
                       "(empty history)"
                       (join-lines (cons "Undo tree (C-/ undo, C-? redo in the buffer):"
                                         (cons "" lines))))
                   '())
    (set-option "line-numbers:vundo-mode" "false")
    (define-key "vundo-mode" "q" "kill-buffer")
    (define-key "vundo-mode" "g" "vundo")))

(register-command "redo")
(register-command "vundo")
