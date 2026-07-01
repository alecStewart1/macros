;; which-key — discover the key bindings available under a prefix (Emacs
;; which-key). `M-x which-key` (or the keys below) pops a read-only list of every
;; binding whose chord starts with the prefix you type.
;;
;; The classic "appears automatically after you pause on a prefix" behaviour
;; needs a Rust hook that fires while `pending_keys` is non-empty with no full
;; match yet — see the WHICH-KEY-AUTO note at the end. Until that exists this is
;; manual, but it covers the discovery use case (C-x ?, C-c ?, C-h b).

(define *which-key-mode* "fundamental")

(define (which-key-pad s) (pad-right s 18))

;; "chord   command" body lines for every binding under `prefix`, sorted.
(define (which-key-lines mode prefix)
  (sort-strings
    (map (lambda (rec)
           (let* ((parts (split-many rec "\t"))
                  (chord (nth parts 0))
                  (cmd   (if (>= (length parts) 2) (nth parts 1) "")))
             (string-append (which-key-pad chord) "  " cmd)))
         (keys-with-prefix mode prefix))))

;; Render the binding list for `prefix` in `mode` into a *which-key* buffer.
(define (which-key-show mode prefix)
  (let ((lines (which-key-lines mode prefix)))
    (render-buffer
      "*which-key*" "which-key-mode"
      (if (null? lines)
          (string-append "No bindings under \"" prefix "\" in " mode)
          (join-lines
            (cons (string-append "Bindings under \"" prefix "\" (" mode "):")
                  (cons "" lines))))
      (list (list 0 "tabulated-header")))
    (set-option "line-numbers:which-key-mode" "false")
    (define-key "which-key-mode" "q" 'kill-buffer)))

;; M-x which-key : prompt for a prefix, then list its bindings.
(define (which-key)
  (read-string "which-key prefix: " 'which-key-from-input ""))
(define (which-key-from-input prefix)
  (which-key-show *which-key-mode* prefix))

;; Convenience entry points for the common prefixes.
(define (which-key-cx)  (which-key-show "fundamental" "ctrl-x"))
(define (which-key-cc)  (which-key-show "fundamental" "ctrl-c"))
(define (which-key-top) (which-key-show "fundamental" ""))

(define-key "fundamental" "ctrl-x ?" 'which-key-cx)
(define-key "fundamental" "ctrl-c ?" 'which-key-cc)
(define-key "fundamental" "ctrl-h b" 'which-key-top)   ; ~describe-bindings

(register-command 'which-key)
(register-command 'which-key-cx)
(register-command 'which-key-cc)
(register-command 'which-key-top)

;; WHICH-KEY-AUTO (future, needs Rust): when a prefix is pending and no full
;; binding resolves, start a short idle timer; on expiry call a Scheme hook with
;; the pending prefix so we can pop this buffer automatically (and dismiss it on
;; the next key). The state already exists — `pending_keys` in src/editor/mod.rs
;; — it just needs to dispatch into Scheme. Add e.g. an `on-prefix-pending` hook
;; in on_key_down and bind it to `which-key-show`.
