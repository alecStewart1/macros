;; yasnippet — template expansion with tab-stops, the Macros port of YASnippet.
;; Type a snippet key and press the expand key: the key is replaced with the
;; template, point lands on the first field (its placeholder selected so typing
;; overwrites it), and you tab between fields.
;;
;;   C-c y       yas-expand            expand the snippet key before point
;;   C-c C-f     snippet-next-field    jump to the next field
;;   C-c C-b     snippet-prev-field    jump to the previous field
;;
;; Template syntax: $1 $2 … ordered tab-stops, $0 the final stop, ${1:default}
;; a stop pre-filled with selectable placeholder text. The heavy lifting (insert
;; + field markers + navigation) is the Rust `snippet-expand` primitive and the
;; `snippet-next-field`/`-prev-field` commands; markers track edits so fields
;; stay put as you type. (Live mirrors — one $1 echoing another — aren't done.)

;; key -> template body
(define *yas-snippets* '())

;; Define or replace the snippet bound to `key`.
(define (yas-define-snippet key template)
  (set! *yas-snippets*
        (cons (cons key template)
              (filter (lambda (kv) (not (equal? (car kv) key))) *yas-snippets*))))

(define (yas--word-char? s)
  (>= (string-match "[A-Za-z0-9_]" s) 0))

;; (start . word) for the identifier ending at point, or #f if none.
(define (yas--word-before)
  (let loop ((i (point)))
    (if (and (> i 0) (yas--word-char? (buffer-substring (- i 1) i)))
        (loop (- i 1))
        (if (< i (point)) (cons i (buffer-substring i (point))) #f))))

;; Expand the snippet keyed by the word before point, if any.
(define (yas-expand)
  (let ((wb (yas--word-before)))
    (if (not wb)
        (message "No snippet key at point")
        (let ((tmpl (alist-ref *yas-snippets* (cdr wb))))
          (if (not tmpl)
              (message (string-append "No snippet: " (cdr wb)))
              (begin
                ;; Delete the key, then expand at its start. Both are queued and
                ;; applied in order, so snippet-expand inserts at the right spot.
                (delete-region (car wb) (point))
                (goto-char (car wb))
                (snippet-expand tmpl)))))))

;; --- a few generic starter snippets ----------------------------------------
(yas-define-snippet "fn"    "fn ${1:name}(${2:args}) {\n    $0\n}")
(yas-define-snippet "for"   "for ${1:item} in ${2:iter} {\n    $0\n}")
(yas-define-snippet "if"    "if ${1:cond} {\n    $0\n}")
(yas-define-snippet "while" "while ${1:cond} {\n    $0\n}")
(yas-define-snippet "let"   "let ${1:name} = ${2:value};$0")

(define-key "fundamental" "ctrl-c y" 'yas-expand)
(define-key "fundamental" "ctrl-c ctrl-f" 'snippet-next-field)
(define-key "fundamental" "ctrl-c ctrl-b" 'snippet-prev-field)

(register-command 'yas-expand)
(register-command 'yas-define-snippet)
