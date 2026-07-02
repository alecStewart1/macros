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

;; "parent/base" for a root, to disambiguate two projects with the same name.
(define (perspective-qualified root)
  (let ((parts (filter (lambda (s) (not (equal? s ""))) (split-many root "/"))))
    (if (>= (length parts) 2)
        (string-append (nth parts (- (length parts) 2)) "/" (last parts))
        (basename root))))

;; A root's dropdown label: its basename, qualified with the parent directory
;; when another perspective shares the same basename.
(define (perspective-display root)
  (let ((base (basename root)))
    (if (> (length (filter (lambda (kv) (equal? (basename (car kv)) base))
                           *perspectives*))
           1)
        (perspective-qualified root)
        base)))

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

;; Reopen `root`'s perspective at its last visited file. If that file vanished,
;; fall back to a find-file picker in the root (a copy of projectile-find-file-in,
;; which this file can't reference — see the load-order note up top).
;;
;; Mark the perspective current HERE, not via find-file-hook: `open-file` only
;; fires that hook on a fresh visit — switching back to a project whose resume
;; file is already buffered just activates the buffer, so relying on the hook
;; left the title bar showing the previous project.
(define (perspective-open root)
  (set! *perspective-current* root)
  (perspective-sync-titlebar)
  (let ((file (alist-ref *perspectives* root)))
    (if (and file (file-exists? file))
        (open-file file)
        (helm (list (list (string-append "Find file in " (basename root))
                          (walk-files root)
                          'open-file))))))

;; The dropdown's click callback (and perspective-switch's Helm action): the
;; picked display label, mapped back to its root.
(define (perspective-select name)
  (let ((root (perspective-root-for-name name)))
    (if root
        (perspective-open root)
        (message (string-append "No perspective named " name)))))

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
(perspective-load)

(define-key "fundamental" "ctrl-c p s" 'perspective-switch)
