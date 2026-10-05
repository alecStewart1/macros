;; session.scm — survive a restart (Emacs `savehist-mode` + `desktop-save-mode`).
;;
;;   * savehist : minibuffer history, search ring, M-x command history and
;;                (opt-out) the kill ring → ~/.config/macros/savehist
;;   * desktop  : open file buffers + point, the window split tree (which buffer
;;                in which pane, split ratios, selected window) and the current
;;                perspective → ~/.config/macros/desktop
;;
;; Both are saved on quit (`kill-emacs-hook`, run synchronously by the editor
;; before it exits) and every `*session-autosave-ms*` (a timer, so a crash loses
;; at most a few minutes), and restored at startup from `after-init-hook` — which
;; the editor runs once the bundled runtime AND your init.scm have loaded, before
;; the window is revealed. So opting out from init.scm works:
;;
;;   (savehist-mode #f)               ; don't save/restore histories at all
;;   (savehist-save-kill-ring #f)     ; keep histories, but never write kills to disk
;;   (savehist-set-max 500)           ; entries kept per history ring
;;   (desktop-save-mode #f)           ; don't save/restore buffers + windows
;;
;; (Set these via the functions, not `set!` — a cross-file `set!` never reaches
;; this file's readers; see steel-language gotchas.)
;;
;; The desktop is skipped when the editor was launched on explicit files, and
;; only the first window of a process restores/saves it (a `cmd-n` window starts
;; clean). Files that no longer exist are skipped silently; panes that showed a
;; special buffer (*compilation*, magit, a terminal) are dropped, their split
;; collapsing onto the sibling.
;;
;; Rust side (src/script/session.rs + src/editor/session.rs): the rings and the
;; layout are published on demand — `(session-refresh 'callback)` publishes, then
;; calls `callback`, which reads `(session-ring …)`, `(session-file-buffers)` and
;; `(window-layout)`; `set-session-ring!`, `restore-file-buffers` and
;; `set-window-layout` restore. Loads after persistence.scm (`persistence-dir`)
;; and perspectives.scm (`perspective-current`) — sorted order.

;; --- line escaping --------------------------------------------------------------
;; Ring entries (kills especially) contain newlines/tabs; the files are one record
;; per line, tab-separated. Escape \ → \\, newline → \n, tab → \t. Unescaping
;; parks literal backslashes on a \x01 sentinel first so "\\n" round-trips; the
;; sentinel itself is stripped from entries on the way out.
(define *session-sentinel* (string (integer->char 1)))

(define (session-escape s)
  (string-replace
    (string-replace
      (string-replace
        (string-replace
          (string-replace s *session-sentinel* "")
          "\\" "\\\\")
        "\n" "\\n")
      "\r" "\\r")
    "\t" "\\t"))

(define (session-unescape s)
  (string-replace
    (string-replace
      (string-replace
        (string-replace
          (string-replace s "\\\\" *session-sentinel*)
          "\\n" "\n")
        "\\r" "\r")
      "\\t" "\t")
    *session-sentinel* "\\"))

;; The last `n` elements of `lst` (all of it when shorter).
(define (session-take-last lst n)
  (let ((len (length lst)))
    (if (<= len n) lst (list-tail lst (- len n)))))

;; =============================================================================
;; savehist
;; =============================================================================

(define *savehist-enabled* #t)
(define *savehist-max* 300)              ; entries kept per ring
(define *savehist-kill-ring* #t)         ; persist the kill ring? (may hold secrets)
(define *savehist-kill-ring-max* 60)     ; kills kept on disk
(define *savehist-kill-entry-max* 8192)  ; longer kills are not written
(define *savehist-rings* (list "minibuffer" "search" "command" "kill"))

(define (savehist-file) (string-append (persistence-dir) "/savehist"))

;; Configuration (call from init.scm).
(define (savehist-mode on) (set! *savehist-enabled* on))
(define (savehist-set-max n) (set! *savehist-max* n))
(define (savehist-save-kill-ring on . rest)
  (set! *savehist-kill-ring* on)
  (when (not (null? rest)) (set! *savehist-kill-ring-max* (car rest))))

;; The entries of `ring` worth writing, oldest first, already bounded.
(define (savehist-entries ring items)
  (if (equal? ring "kill")
      (if *savehist-kill-ring*
          (session-take-last
            (filter (lambda (s) (<= (string-length s) *savehist-kill-entry-max*)) items)
            *savehist-kill-ring-max*)
          '())
      (session-take-last items *savehist-max*)))

;; ring name -> entries (oldest first) ⇒ the file text: one "ring<TAB>entry"
;; line per entry. Pure (the caller passes the rings), so it's testable.
(define (savehist-serialize rings)
  (join-lines
    (cons "# macros savehist v1"
          (apply append
                 (map (lambda (kv)
                        (map (lambda (e) (string-append (car kv) "\t" (session-escape e)))
                             (savehist-entries (car kv) (cdr kv))))
                      rings)))))

;; The inverse: file text ⇒ alist ring -> entries (oldest first), every known
;; ring present. Unknown rings and malformed lines are ignored.
(define (savehist-parse text)
  (let ((acc (map (lambda (r) (cons r '())) *savehist-rings*)))
    (for-each
      (lambda (line)
        ;; Entries are escaped, so a record has exactly one raw tab.
        (let ((parts (split-many line "\t")))
          (when (and (= (length parts) 2) (not (starts-with? line "#")))
            (let ((ring (car parts))
                  (entry (session-unescape (cadr parts))))
              (when (and (alist-ref acc ring) (not (equal? entry "")))
                (set! acc (map (lambda (kv)
                                 (if (equal? (car kv) ring) (cons ring (cons entry (cdr kv))) kv))
                               acc)))))))
      (split-many text "\n"))
    (map (lambda (kv) (cons (car kv) (reverse (cdr kv)))) acc)))

;; Write the (freshly published) rings to disk.
(define (savehist-save)
  (when *savehist-enabled*
    (write-file (savehist-file)
                (savehist-serialize
                  (map (lambda (r) (cons r (session-ring r))) *savehist-rings*)))))

;; Restore every ring from disk (startup).
(define (savehist-load)
  (when (and *savehist-enabled* (file-exists? (savehist-file)))
    (for-each
      (lambda (kv)
        (when (and (not (null? (cdr kv)))
                   (or *savehist-kill-ring* (not (equal? (car kv) "kill"))))
          (set-session-ring! (car kv) (cdr kv))))
      (savehist-parse (read-file (savehist-file))))))

;; =============================================================================
;; desktop
;; =============================================================================

(define *desktop-save-mode* #t)
(define *desktop-primary* #f)  ; this window owns the desktop (first in process)
(define *desktop-cleared* #f)  ; desktop-clear: stop writing until desktop-save

(define (desktop-file) (string-append (persistence-dir) "/desktop"))
(define (desktop-save-mode on) (set! *desktop-save-mode* on))

;; --- layout <-> lines (pure) ------------------------------------------------------
;; The split tree `(split vertical? ratio a b)` / `(leaf name path point selected?)`
;; (what `window-layout` returns) is written in preorder, one node per line:
;;   split<TAB>v|h<TAB>ratio
;;   leaf<TAB>point<TAB>1|0<TAB>name<TAB>path
(define (desktop-layout->lines tree)
  (if (equal? (car tree) 'split)
      (append (list (string-append "split\t" (if (nth tree 1) "v" "h")
                                   "\t" (number->string (nth tree 2))))
              (desktop-layout->lines (nth tree 3))
              (desktop-layout->lines (nth tree 4)))
      (list (string-append "leaf\t" (number->string (nth tree 3))
                           "\t" (if (nth tree 4) "1" "0")
                           "\t" (nth tree 1)
                           "\t" (nth tree 2)))))

;; Parse one node from the front of `lines` ⇒ (tree . remaining-lines), or #f.
(define (desktop-parse-node lines)
  (if (null? lines)
      #f
      (let ((parts (split-many (car lines) "\t")))
        (cond
          ((and (equal? (car parts) "leaf") (>= (length parts) 4))
           (cons (list 'leaf
                       (nth parts 3)
                       (if (>= (length parts) 5) (nth parts 4) "")
                       (or (string->number (nth parts 1)) 0)
                       (equal? (nth parts 2) "1"))
                 (cdr lines)))
          ((and (equal? (car parts) "split") (>= (length parts) 3))
           (let ((a (desktop-parse-node (cdr lines))))
             (and a
                  (let ((b (desktop-parse-node (cdr a))))
                    (and b
                         (cons (list 'split
                                     (equal? (nth parts 1) "v")
                                     (or (string->number (nth parts 2)) 0.5)
                                     (car a) (car b))
                               (cdr b)))))))
          (else #f)))))

(define (desktop-lines->layout lines)
  (let ((r (desktop-parse-node lines)))
    (if r (car r) #f)))

;; Drop leaves for which `keep?` is false (a buffer that can't be restored),
;; collapsing each split onto its surviving child. #f if nothing survives.
(define (desktop-prune-layout tree keep?)
  (cond
    ((not tree) #f)
    ((equal? (car tree) 'leaf) (if (keep? tree) tree #f))
    (else
     (let ((a (desktop-prune-layout (nth tree 3) keep?))
           (b (desktop-prune-layout (nth tree 4) keep?)))
       (cond ((and a b) (list 'split (nth tree 1) (nth tree 2) a b))
             (a a)
             (else b))))))

;; --- desktop file (pure) ----------------------------------------------------------
;;   # macros desktop v1
;;   perspective<TAB>root            (optional)
;;   buffer<TAB>point<TAB>path        (one per file buffer, ring order, active last)
;;   split… / leaf…                   (the layout, preorder; optional)
(define (desktop-serialize buffers layout perspective)
  (join-lines
    (append
      (list "# macros desktop v1")
      (if perspective (list (string-append "perspective\t" perspective)) '())
      (map (lambda (b) (string-append "buffer\t" (number->string (cadr b)) "\t" (car b)))
           buffers)
      (if layout (desktop-layout->lines layout) '()))))

;; ⇒ (buffers layout perspective): buffers as ((path point) …).
(define (desktop-parse text)
  (let loop ((lines (split-many text "\n")) (buffers '()) (persp #f))
    (if (null? lines)
        (list (reverse buffers) #f persp)
        (let* ((line (car lines))
               (parts (split-many line "\t"))
               (head (car parts)))
          (cond
            ((or (equal? head "split") (equal? head "leaf"))
             (list (reverse buffers) (desktop-lines->layout lines) persp))
            ((and (equal? head "buffer") (>= (length parts) 3))
             (loop (cdr lines)
                   (cons (list (nth parts 2) (or (string->number (nth parts 1)) 0)) buffers)
                   persp))
            ((and (equal? head "perspective") (>= (length parts) 2))
             (loop (cdr lines) buffers (nth parts 1)))
            (else (loop (cdr lines) buffers persp)))))))

;; --- save / read ----------------------------------------------------------------

;; Write the (freshly published) desktop.
(define (desktop-write)
  (write-file (desktop-file)
              (desktop-serialize (session-file-buffers)
                                 (window-layout)
                                 (perspective-current))))

;; Should the automatic paths (timer, quit) write the desktop?
(define (desktop-auto-save?)
  (and *desktop-save-mode* *desktop-primary* (not *desktop-cleared*)))

;; Restore a parsed desktop: files (missing ones skipped), then the pane tree
;; pruned to leaves showing a restorable file, then the current perspective.
(define (desktop-restore parsed)
  (let* ((buffers (filter (lambda (b) (file-exists? (car b))) (car parsed)))
         (pruned (desktop-prune-layout
                   (cadr parsed)
                   (lambda (leaf)
                     (let ((path (nth leaf 2)))
                       (and (not (equal? path "")) (file-exists? path))))))
         ;; No restorable windows: still land on the last active file.
         (layout (or pruned
                     (and (not (null? buffers))
                          (let ((b (last buffers)))
                            (list 'leaf "" (car b) (cadr b) #t))))))
    (restore-file-buffers buffers)
    (when layout (set-window-layout layout))
    (perspective-restore-current! (nth parsed 2))
    (length buffers)))

(define (desktop-read-file)
  (if (file-exists? (desktop-file))
      (desktop-restore (desktop-parse (read-file (desktop-file))))
      #f))

;; M-x desktop-save: write the desktop now (also re-arms saving after a clear).
(define (desktop--save-now)
  (desktop-write)
  (message (string-append "Desktop saved to " (desktop-file))))
(define-command (desktop-save)
  (set! *desktop-cleared* #f)
  (set! *desktop-primary* #t)
  (session-refresh 'desktop--save-now))

;; M-x desktop-read: restore the saved desktop into this session.
(define-command (desktop-read)
  (let ((n (desktop-read-file)))
    (set! *desktop-cleared* #f)
    (message (if n
                 (string-append "Desktop restored (" (number->string n) " file(s))")
                 "No saved desktop"))))

;; M-x desktop-clear: forget the saved desktop — delete the file and stop the
;; automatic saves (quit / timer) until the next desktop-save / desktop-read, so
;; the next launch starts clean. Open buffers are left alone.
(define-command (desktop-clear)
  (when (file-exists? (desktop-file)) (delete-file (desktop-file)))
  (set! *desktop-cleared* #t)
  (message "Desktop cleared — the next launch starts fresh"))

;; =============================================================================
;; lifecycle
;; =============================================================================

(define *session-autosave-ms* 300000)  ; 5 minutes
(define (session-set-autosave-interval ms) (set! *session-autosave-ms* ms))

;; Everything that writes on a fresh `session-refresh`.
(define (session-save-all)
  (savehist-save)
  (when (desktop-auto-save?) (desktop-write)))

(define (session-autosave-tick)
  (session-refresh 'session-save-all)
  (run-with-timer *session-autosave-ms* "session-autosave-tick"))

;; after-init-hook: restore, then start the autosave timer. Only the first window
;; of the process owns the desktop. A launch on explicit files doesn't take it
;; at all: it neither restores the saved desktop nor overwrites it on quit or
;; autosave (an explicit desktop-read / desktop-save still claims it).
;; The launch's explicit files. Tests override them through the setter, since
;; the real list comes from the process arguments. (The setter must live in
;; this file: Steel treats a global its own file never set!s as a constant, so
;; a set! from outside wouldn't reach session-launch-files.)
(define *session-launch-files-override* #f)
(define (session-set-launch-files! files) (set! *session-launch-files-override* files))
(define (session-launch-files) (or *session-launch-files-override* (command-line-files)))

(define (session-startup)
  (savehist-load)
  (set! *desktop-primary* (and (null? (session-launch-files)) (session-claim-primary!)))
  (when (and *desktop-save-mode* *desktop-primary*)
    (desktop-read-file))
  (when (> *session-autosave-ms* 0)
    (run-with-timer *session-autosave-ms* "session-autosave-tick")))

;; kill-emacs-hook: the editor has just published fresh session state.
(define (session-kill-emacs) (session-save-all))

(add-hook "after-init-hook" 'session-startup)
(add-hook "kill-emacs-hook" 'session-kill-emacs)
