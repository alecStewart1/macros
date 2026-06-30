;; Mode line — the Emacs `mode-line-format` analogue. The bar's *content* lives
;; here in Scheme: an ordered list of segments handed to `set-mode-line-format`.
;; Rust owns only a small vocabulary of live FIELDS (resolved from editor state
;; every frame, since the engine thread can't be called per-frame) plus named
;; SLOTS that Scheme writes on hooks (`set-mode-line-slot`). This file decides
;; which items show, in what order, with what separators and faces.
;;
;; A segment crosses to Rust as one string:
;;   * a bare string  -> an inert literal (separators, parens, labels)
;;   * "kind\tpayload\tface\tprefix\tsuffix" -> a field/slot/styled literal,
;;     built by the `ml-*` helpers below. `prefix`/`suffix` wrap the value and
;;     are emitted ONLY when it's non-empty, so an absent optional field takes
;;     its separator with it. `face` (a theme face name) colors the whole span.

;; Encode one tagged segment. Empty face/prefix/suffix mean "none".
(define (ml--seg kind payload face prefix suffix)
  (string-append kind "\t" payload "\t" face "\t" prefix "\t" suffix))

;; A live field. `name` is one of: modified buffer-name modes evil-mode lsp
;; diagnostics. Plain form takes no styling; `ml-field*` adds face/prefix/suffix.
(define (ml-field name) (ml--seg "field" (symbol->string name) "" "" ""))
(define (ml-field* name face prefix suffix)
  (ml--seg "field" (symbol->string name) face prefix suffix))

;; A named register written elsewhere with `(set-mode-line-slot key value)`.
(define (ml-slot name) (ml--seg "slot" name "" "" ""))
(define (ml-slot* name face prefix suffix) (ml--seg "slot" name face prefix suffix))

;; A literal; `ml-text*` colors it (a bare string also works for the default).
(define (ml-text s) s)
(define (ml-text* s face) (ml--seg "lit" s face "" ""))

;; The default layout — identical to the Rust fallback:
;;   -- name  (modes)  [project]  git:branch  lsp …  E:n W:n
;; Reorder, drop, restyle, or add slots here to taste. Held in a global so a
;; module loaded later (e.g. a plugin) can contribute a segment via
;; `mode-line-add-segment` without restating the whole list.
(define *mode-line-format*
 (list (ml-field 'modified)
       "  "
       (ml-field 'buffer-name)
       "  ("
       (ml-field 'modes)
       ")"
       (ml-slot* "project" "" "  [" "]")
       (ml-slot* "git" "magit-branch" "  git:" "")
       (ml-field* 'lsp "" "  lsp " "")
       (ml-field* 'diagnostics "" "  " "")))

(set-mode-line-format *mode-line-format*)

;; Append a segment (any `ml-field`/`ml-slot`/`ml-text` token, or a bare string)
;; to the mode line and re-apply — the extension point for modules that add their
;; own field/slot without owning the whole format, e.g.
;;   (mode-line-add-segment (ml-slot* "agent" "" "  " ""))
;; Idempotent: re-adding the same segment (a reload) is a no-op.
(define (mode-line-add-segment seg)
  (unless (member seg *mode-line-format*)
    (set! *mode-line-format* (append *mode-line-format* (list seg)))
    (set-mode-line-format *mode-line-format*)))
