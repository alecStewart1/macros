;; persistence — remember things across sessions:
;;   * saveplace  : restore point to where you left it in each file (Emacs
;;                  `save-place-mode`).
;;   * recentf    : persist the recent-files ring (defined in helm.scm) so it
;;                  survives a restart — the slice of `savehist` we can do today.
;;
;; State is written to ~/.config/macros/{saveplace,recentf} on save and reloaded
;; at startup. Full savehist (search ring, kill ring, minibuffer history) needs
;; Rust primitives to expose those rings, which don't exist yet.

;; ~/.config/macros, resolved in Rust (a per-engine temp dir under `cargo test`).
(define (persistence-dir) (persistence-directory))
(define (saveplace-file)  (string-append (persistence-dir) "/saveplace"))
(define (recentf-file)    (string-append (persistence-dir) "/recentf"))

;; --- saveplace --------------------------------------------------------------
;; path -> byte position of point
(define *saveplace* '())

(define (saveplace-set! path pos)
  (set! *saveplace*
        (cons (cons path pos)
              (filter (lambda (kv) (not (equal? (car kv) path))) *saveplace*))))

;; Parse "pos\tpath" lines back into the alist.
(define (saveplace-load)
  (when (file-exists? (saveplace-file))
    (for-each
      (lambda (line)
        (let ((parts (split-many line "\t")))
          (when (>= (length parts) 2)
            (saveplace-set! (nth parts 1) (string->number (nth parts 0))))))
      (non-empty-lines (read-file (saveplace-file))))))

(define (saveplace-serialize)
  (join-lines
    (map (lambda (kv) (string-append (int->string (cdr kv)) "\t" (car kv)))
         *saveplace*)))

;; find-file-hook: jump to the remembered position for this file (clamped to the
;; current buffer length, in case the file shrank since last visit).
;; Keyed on the visited file's real path — the buffer name is only a basename
;; (possibly uniquified `name<dir>`), which collides across directories and
;; makes file-exists? resolve against the process cwd.
(define (saveplace-restore)
  (let* ((f (buffer-file-name))
         (pos (alist-ref *saveplace* f)))
    (when (and pos (file-exists? f))
      (goto-char (min pos (point-max))))))

;; Record point for the current file (run before each save).
(define (saveplace-remember)
  (let ((f (buffer-file-name)))
    (when (and (not (equal? f "")) (file-exists? f))
      (saveplace-set! f (point)))))

;; --- write-through on save --------------------------------------------------
;; Persist both rings to disk. Cheap (a few dozen short lines).
(define (persistence-save)
  (saveplace-remember)
  (write-file (saveplace-file) (saveplace-serialize))
  (write-file (recentf-file) (join-lines *recentf*)))

(define (recentf-load)
  (when (file-exists? (recentf-file))
    (set! *recentf*
          (helm-take (filter (lambda (p) (file-exists? p))
                             (non-empty-lines (read-file (recentf-file))))
                     *recentf-max*))))

(add-hook "find-file-hook" "saveplace-restore")
(add-hook "after-save-hook" "persistence-save")

;; Load persisted state at startup (these files load after helm.scm, so
;; *recentf* / *recentf-max* / helm-take already exist).
(saveplace-load)
(recentf-load)
