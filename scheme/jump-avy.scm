;; avy — jump to any visible character in a couple of keystrokes (the Macros port
;; of avy / ace-jump). Type the target character; every on-screen occurrence is
;; tagged with a hint letter painted over the buffer; press the letter to jump.
;;
;;   C-c j / M-x avy-goto-char    jump to a visible character
;;   evil: g s                    (selection-friendly jump)
;;
;; Rendering is the Rust inline-hint primitive (`avy-show-hints`, painted at byte
;; positions by src/element.rs); the next key is captured by the editor and sent
;; to `avy-pick`, so picking a hint is a single keypress. C-g cancels.

(define *avy-keys* "asdfghjklqwertyuiopzxcvbnm")
(define *avy-map* '())   ; label -> byte position, for the active hint set

;; Byte offsets of `ch` (a one-char string) on lines [first,last), tracking the
;; byte offset and line as we walk the buffer text.
(define (avy--targets ch first last)
  (let loop ((chars (string->list (buffer-string))) (byte 0) (line 0) (acc '()))
    (if (null? chars)
        (reverse acc)
        (let* ((cs (string (car chars)))
               (clen (utf8-length cs))
               (hit (and (>= line first) (< line last) (equal? cs ch)))
               (acc2 (if hit (cons byte acc) acc))
               (line2 (if (equal? cs "\n") (+ line 1) line)))
          (loop (cdr chars) (+ byte clen) line2 acc2)))))

;; Pair each target byte with a hint letter (capped at the available letters).
(define (avy--assign positions)
  (let loop ((ps positions) (i 0) (acc '()))
    (if (or (null? ps) (>= i (string-length *avy-keys*)))
        (reverse acc)
        (loop (cdr ps) (+ i 1)
              (cons (cons (substring *avy-keys* i (+ i 1)) (car ps)) acc)))))

(define (avy--show pairs)
  (set! *avy-map* pairs)
  (avy-show-hints
    (join-lines (map (lambda (kv) (string-append (int->string (cdr kv)) "\t" (car kv)))
                     pairs))))

;; Entry: prompt for the target character.
(define (avy-goto-char)
  (read-string "avy char: " 'avy--got-char ""))

(define (avy--got-char ch)
  (if (equal? ch "")
      (message "avy: cancelled")
      (let* ((vr (visible-lines))
             (targets (avy--targets ch (nth vr 0) (nth vr 1)))
             (pairs (avy--assign targets)))
        (if (null? pairs)
            (message (string-append "avy: no \"" ch "\" on screen"))
            (begin
              (avy--show pairs)
              (message "avy: type a hint letter to jump (C-g cancels)"))))))

;; Called by the editor with the hint letter the user pressed.
(define (avy--pick key)
  (let ((pos (alist-ref *avy-map* key)))
    (set! *avy-map* '())
    (if pos
        (begin (goto-char pos) (message "avy: jumped"))
        (message "avy: cancelled"))))

;; The editor dispatches the captured key to `avy-pick`.
(define (avy-pick key) (avy--pick key))

(define-key "fundamental" "ctrl-c j" 'avy-goto-char)
(define-key "evil-normal" "g s" 'avy-goto-char)

(register-command 'avy-goto-char)
