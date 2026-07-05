;; Perspectives — one workspace context per project, switched from the title
;; bar. Visiting a file inside a project (find-file-hook) creates that
;; project's perspective on first touch and records the file as its resume
;; point; the title-bar dropdown (`set-titlebar-dropdown`, workspace.rs) lists
;; every perspective and reopens the picked project at its last visited file.
;; The list survives restarts via ~/.config/macros/perspectives.
;;
;; All the logic lives here — Rust only draws the dropdown and hands the
;; clicked label back to `perspective-select`. This file loads BEFORE
;; projectile.scm (sorted order) and Steel resolves free identifiers at
;; definition time, so it must not call projectile functions; the root
;; resolver (`project-root`) lives in core.scm, which is fine.

(define *perspectives* '())        ;; alist: project root -> last visited file, creation order
(define *perspective-current* #f)  ;; root of the perspective shown in the title bar

(define (perspectives-file) (string-append (persistence-dir) "/perspectives"))

;; --- display names -----------------------------------------------------------

(define (perspective-parts root)
  (filter (lambda (s) (not (equal? s ""))) (split-many root "/")))

(define (take-last lst n)
  (if (<= (length lst) n) lst (take-last (cdr lst) n)))

;; The trailing `n` path segments of `root`, joined ("b/proj" for n = 2); the
;; whole root when it has fewer than `n` segments.
(define (perspective-suffix root n)
  (string-join (take-last (perspective-parts root) n) "/"))

;; Whether no OTHER perspective root shares `root`'s n-segment suffix.
(define (perspective-suffix-unique? root n)
  (null? (filter (lambda (kv)
                   (and (not (equal? (car kv) root))
                        (equal? (perspective-suffix (car kv) n)
                                (perspective-suffix root n))))
                 *perspectives*)))

;; A root's dropdown label: the shortest trailing-path suffix ("proj", then
;; "b/proj", then "a/b/proj", …) that no other perspective shares. One
;; qualification level isn't always enough (/x/a/proj and /y/a/proj would both
;; label "a/proj"), and duplicate labels break the label→root reverse lookup —
;; a click on either row would open whichever project registered first.
(define (perspective-display root)
  (define (shortest n)
    (if (or (perspective-suffix-unique? root n)
            (>= n (length (perspective-parts root))))
        (perspective-suffix root n)
        (shortest (+ n 1))))
  (shortest 1))

(define (perspective-root-for-name name)
  (let ((hits (filter (lambda (kv) (equal? (perspective-display (car kv)) name))
                      *perspectives*)))
    (if (null? hits) #f (car (car hits)))))

;; --- title bar ----------------------------------------------------------------

(define (perspective-sync-titlebar)
  (set-titlebar-dropdown
    (map (lambda (kv) (perspective-display (car kv))) *perspectives*)
    (if *perspective-current* (perspective-display *perspective-current*) "")
    'perspective-select))

;; --- tracking -----------------------------------------------------------------

;; Record `file` as `root`'s resume point, creating the perspective on first
;; touch. Updates happen in place so the dropdown keeps creation order.
(define (perspective-update root file)
  (if (alist-ref *perspectives* root)
      (set! *perspectives*
            (map (lambda (kv) (if (equal? (car kv) root) (cons root file) kv))
                 *perspectives*))
      (set! *perspectives* (append *perspectives* (list (cons root file))))))

;; find-file-hook: visiting a file inside a project creates/refreshes that
;; project's perspective, makes it current, and writes the list through to disk
;; (so a perspective survives a restart as soon as it exists — no clean quit
;; required, matching projectile's recent-ring behaviour).
(define (perspective-track)
  (let ((root (project-root (buffer-directory)))
        (file (buffer-file-name)))
    (when (and root (not (equal? file "")))
      (perspective-update root file)
      (set! *perspective-current* root)
      (perspective-sync-titlebar)
      (perspective-save))))

;; --- switching ----------------------------------------------------------------

;; Drop `root`'s perspective (its project vanished from disk): remove it from
;; the list, clear it from the title bar if it was current, and write through.
(define (perspective-remove root)
  (set! *perspectives*
        (filter (lambda (kv) (not (equal? (car kv) root))) *perspectives*))
  (when (equal? *perspective-current* root)
    (set! *perspective-current* #f))
  (perspective-sync-titlebar)
  (perspective-save))

;; Reopen `root`'s perspective at its last visited file. If that file vanished,
;; fall back to a find-file picker in the root (a copy of projectile-find-file-in,
;; which this file can't reference — see the load-order note up top). If the
;; ROOT itself vanished (project deleted/unmounted mid-session — startup only
;; prunes at load), drop the perspective instead of switching the title bar to
;; a dead project over an empty picker.
;;
;; Mark the perspective current HERE, not via find-file-hook: `open-file` only
;; fires that hook on a fresh visit — switching back to a project whose resume
;; file is already buffered just activates the buffer, so relying on the hook
;; left the title bar showing the previous project.
(define (perspective-open root)
  (if (not (file-exists? root))
      (begin
        (perspective-remove root)
        (message (string-append "Project " root " no longer exists — perspective removed")))
      (begin
        (set! *perspective-current* root)
        (perspective-sync-titlebar)
        (let ((file (alist-ref *perspectives* root)))
          (if (and file (file-exists? file))
              (open-file file)
              (helm (list (list (string-append "Find file in " (basename root))
                                (walk-files root)
                                'open-file))))))))

;; The dropdown's click callback (and perspective-switch's Helm action): the
;; picked display label, mapped back to its root.
(define (perspective-select name)
  (let ((root (perspective-root-for-name name)))
    (if root
        (perspective-open root)
        (begin
          (set! *perspectives*
                (filter (lambda (pair) (file-exists? (car pair)))
                        *perspectives*))
          (perspective-save)
          (perspective-sync-titlebar)
          (message (string-append "Removed stale project: " name))))))

;; Keyboard access to the same list.
(define-command (perspective-switch)
  (if (null? *perspectives*)
      (message "No perspectives yet — open a file in a project first")
      (helm (list (list "Switch perspective"
                        (map (lambda (kv) (perspective-display (car kv))) *perspectives*)
                        'perspective-select)))))

;; --- persistence --------------------------------------------------------------
;; One "root<TAB>last-file" line per perspective, in dropdown order.

(define (perspective-save)
  (write-file (perspectives-file)
              (join-lines (map (lambda (kv) (string-append (car kv) "\t" (cdr kv)))
                               *perspectives*))))

;; Restore at startup, dropping projects whose roots no longer exist. The bar
;; starts with no active perspective (the trigger shows "projects") until the
;; first file visit picks one.
(define (perspective-load)
  (when (file-exists? (perspectives-file))
    (for-each
      (lambda (line)
        (let ((parts (split-many line "\t")))
          (when (and (>= (length parts) 2) (file-exists? (nth parts 0)))
            (perspective-update (nth parts 0) (nth parts 1)))))
      (non-empty-lines (read-file (perspectives-file))))
    (perspective-sync-titlebar)))

(add-hook "find-file-hook" "perspective-track")
;; Re-activating an already-open buffer never fires find-file-hook (C-x b,
;; buffer cycling, find-file to a buffered file, the survivor after a kill),
;; so the title bar would keep naming the previous project when moving between
;; projects through existing buffers. `buffer-switch-hook` (buffers.rs) fires
;; on exactly those switches; perspective-track's guards skip fileless buffers.
(add-hook "buffer-switch-hook" "perspective-track")
(perspective-load)

(define-key "fundamental" "ctrl-c p s" 'perspective-switch)
