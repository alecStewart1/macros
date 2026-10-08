;; secrets.scm — one encrypted password store for every package (Emacs
;; `auth-source` + ~/.authinfo.gpg).
;;
;; Passwords live in ~/.config/macros/authinfo.gpg, authinfo lines encrypted
;; with one master passphrase:
;;
;;   machine db.example.com login alice port 5432 password hunter2
;;   machine imap.gmail.com login me@example.com password "has spaces"
;;
;; A package never prompts for a password itself; it asks the store:
;;
;;   (secret-get "db.example.com" "alice"
;;     (lambda (pw) (connect pw))
;;     'port "5432"                                ; optional extra match key
;;     'on-cancel (lambda (_) (message "cancelled")))
;;
;; The first lookup of a session asks for the master passphrase (once). A hit
;; goes straight to the callback; a miss prompts "Password for alice@db…:",
;; offers to save it, and then calls back. The first save creates the file
;; (you pick the master passphrase then). When a service REJECTS a password
;; (changed, expired), the package calls
;;
;;   (secret-forget "db.example.com" "alice" "5432")
;;
;; which deletes it, and the next `secret-get` prompts again.
;; A password you choose NOT to save is still remembered for the session.
;;
;; Config can name a secret instead of writing it out: called WITHOUT a
;; callback, `secret-get` returns a reference that `set-option` stores as-is:
;;
;;   (set-option "psql-url" (secret-get "psql-url-1" "alice"))
;;
;; `get-option` then reads the real value whenever the store can answer (#f
;; until then), so the secret is never in init.scm or the option table. A
;; package gates its commands with `(option-ensure "psql-url" k)`, which
;; prompts — master passphrase, then the value itself if there's no entry
;; yet, offering to save it — only when the value isn't available.
;;
;; Code that must stay synchronous (a catalog query, a completion source) uses
;;
;;   (secret-lookup "db.example.com" "alice" "5432")   ; → password or #f
;;
;; which never prompts: it answers from the unlocked store or the session, so
;; gate the user-facing command with `secret-get` first and look up inside.
;;
;; Requests are served one at a time, so two packages asking at once get two
;; prompts in turn rather than one displacing the other.
;;
;; Commands: secrets-unlock, secrets-lock, secrets-add, secrets-change-passphrase.
;; Options (init.scm):
;;   (set-option "authinfo-file" "~/.config/macros/authinfo.gpg")   ; the default
;;   (set-option "gpg-command" "/opt/homebrew/bin/gpg")   ; default: auto-detect
;;
;; The decrypted store (and the session's unsaved passwords) lives in Rust
;; memory (src/authinfo.rs) until quit or `secrets-lock`; Scheme only ever sees
;; the one password it asked for.

(define (secrets-file)
  (let ((v (get-option "authinfo-file")))
    (if (and (string? v) (not (equal? v "")))
        v
        (string-append (persistence-directory) "/authinfo.gpg"))))

(define (secrets--gpg)
  (let ((v (get-option "gpg-command")))
    (if (string? v) v "")))

;; Value after `key` in a '(key value …) option list, else `default`.
(define (secrets--opt opts key default)
  (cond ((or (null? opts) (null? (cdr opts))) default)
        ((equal? (car opts) key) (cadr opts))
        (else (secrets--opt (cddr opts) key default))))

(define (secrets--label host user port)
  (string-append (if (equal? user "") "" (string-append user "@"))
                 host
                 (if (equal? port "") "" (string-append ":" port))))

;; A one-shot named callback for a procedure: freed once it fires, so a closure
;; holding a password doesn't stay reachable for the session.
(define (secrets--once proc) (car (callback-group->names (list proc))))

;; Run a caller's callback; an error is reported without its message (Steel
;; quotes the offending value — possibly the password), and never stalls the queue.
(define (secrets--safely thunk)
  (with-handler
    (lambda (_) (message "secret-get: the callback failed"))
    (thunk)))

;; ── request queue ─────────────────────────────────────────────────────────────
;; Each job is (lambda (done) …) and calls `done` exactly once when it's over
;; (answered or cancelled); the next job starts then.
(define *secrets--queue* '())
(define *secrets--busy* #f)

(define (secrets--enqueue job)
  (set! *secrets--queue* (append *secrets--queue* (list job)))
  (unless *secrets--busy* (secrets--next)))

(define (secrets--next)
  (if (null? *secrets--queue*)
      (set! *secrets--busy* #f)
      (let ((job (car *secrets--queue*)))
        (set! *secrets--queue* (cdr *secrets--queue*))
        (set! *secrets--busy* #t)
        (job secrets--next))))

;; ── unlock / create ───────────────────────────────────────────────────────────
;; Call `k` once the store is usable: already unlocked, or no file yet (nothing
;; to unlock — it's created on the first save). Else prompt for the passphrase,
;; with up to `tries` attempts; `cancel` runs if the user gives up.
(define (secrets--ensure-unlocked k cancel)
  (cond ((authinfo-unlocked?) (k))
        ((not (file-exists? (secrets-file))) (k))
        (else (secrets--prompt-unlock "" k cancel 3))))

(define (secrets--prompt-unlock note k cancel tries)
  (read-passwd (string-append note "authinfo.gpg passphrase: ")
    (lambda (pass)
      (let ((err (authinfo-unlock (secrets-file) (secrets--gpg) pass)))
        (cond ((equal? err "") (k))
              ((> tries 1) (secrets--prompt-unlock (string-append "(" err ") ") k cancel (- tries 1)))
              (else (message (string-append "authinfo: " err)) (cancel)))))
    (lambda (_) (cancel))))

;; Make sure there's a store to save into: unlock an existing file, or create a
;; new one by choosing (and confirming) the master passphrase.
(define (secrets--ensure-store k cancel)
  (if (or (authinfo-unlocked?) (file-exists? (secrets-file)))
      (secrets--ensure-unlocked k cancel)
      (secrets--new-passphrase
        (lambda (pass)
          (let ((err (authinfo-unlock (secrets-file) (secrets--gpg) pass)))
            (if (equal? err "")
                (k)
                (begin (message (string-append "authinfo: " err)) (cancel)))))
        cancel
        "New authinfo.gpg passphrase: ")))

;; Ask for a new passphrase twice; `k` gets it once both entries agree.
(define (secrets--new-passphrase k cancel prompt)
  (read-passwd prompt
    (lambda (p1)
      (read-passwd "Confirm passphrase: "
        (lambda (p2)
          (if (equal? p1 p2)
              (k p1)
              (secrets--new-passphrase k cancel "(didn't match) New passphrase: ")))
        (lambda (_) (cancel))))
    (lambda (_) (cancel))))

;; ── save ──────────────────────────────────────────────────────────────────────
(define (secrets--save host user port pw)
  (let ((err (authinfo-save host user port pw)))
    (message (if (equal? err "")
                 (string-append "authinfo: saved " (secrets--label host user port))
                 (string-append "authinfo: save failed — " err)))))

;; Ask whether to save a freshly typed password, then `k`. Sticky, so the
;; answer always arrives (an aborted prompt would never call back).
(define (secrets--offer-save host user port pw k)
  (y-or-n-p-sticky
    (string-append "Save " (secrets--label host user port) " to authinfo.gpg?")
    (secrets--once
      (lambda (answer)
        (if (equal? answer "yes")
            (secrets--ensure-store
              (lambda () (secrets--save host user port pw) (k))
              k)
            (begin (authinfo-remember host user port pw) (k)))))))

;; ── public API ────────────────────────────────────────────────────────────────
;; (secret-get host user callback ['port P] ['on-cancel proc] ['prompt S])
;; `user` / port "" match any entry for the host. `callback` gets the password;
;; `on-cancel` (optional) gets #f if the user aborts a prompt; `prompt`
;; replaces the default "Password for user@host: " when one is needed.
;;
;; (secret-get host user ['port P]) — no callback: → a reference to the entry,
;; for `set-option` (see the header). Never prompts.
(define (secret-get host user . rest)
  (if (or (null? rest) (not (procedure? (car rest))))
      (authinfo-secret-ref host user (secrets--opt rest 'port ""))
      (apply secrets--get host user rest)))

(define (secrets--get host user callback . opts)
  (let* ((port (secrets--opt opts 'port ""))
         (on-cancel (secrets--opt opts 'on-cancel #f))
         (label (secrets--label host user port))
         (prompt (secrets--opt opts 'prompt (string-append "Password for " label ": "))))
    (secrets--enqueue
      (lambda (done)
        (let ((cancel (lambda ()
                        (when on-cancel (secrets--safely (lambda () (on-cancel #f))))
                        (done)))
              (deliver (lambda (pw)
                         (secrets--safely (lambda () (callback pw)))
                         (done))))
          (let ((known (authinfo-lookup host user port)))   ; session memory works locked
            (if known
                (deliver known)
                (secrets--ensure-unlocked
                  (lambda ()
                    (let ((stored (authinfo-lookup host user port)))
                      (if stored
                          (deliver stored)
                          (read-passwd prompt
                            (lambda (pw)
                              (secrets--offer-save host user port pw (lambda () (deliver pw))))
                            (lambda (_) (cancel))))))
                  cancel))))))))

;; (secret-lookup host user [port]) → the password, or #f. Never prompts: only
;; the unlocked store and this session's unsaved passwords are consulted.
(define (secret-lookup host user . rest)
  (authinfo-lookup host user (if (null? rest) "" (car rest))))

;; (secret-forget host user [port]) — the service rejected the stored password:
;; delete it (session memory, the unlocked store, and authinfo.gpg) so the next
;; `secret-get` prompts. If the store is locked, only session memory is
;; cleared, and the file's copy goes when it next fails while unlocked.
(define (secret-forget host user . rest)
  (let ((err (authinfo-forget host user (if (null? rest) "" (car rest)))))
    (unless (equal? err "") (message (string-append "authinfo: " err)))))

;; (option-ensure name k ['on-cancel proc]) — run `k` once option `name` can
;; be read. A plain option (or an unset one) runs `k` right away; one set to a
;; secret reference runs it once the store or this session has the value,
;; prompting for the master passphrase and/or the value itself if needed.
;; Gate every command that reads such an option with this; a synchronous
;; read (`get-option`) of an unavailable secret is #f, never a prompt.
(define (option-ensure name k . opts)
  (let ((ref (option-secret-ref name)))
    (if (or (not ref) (apply authinfo-lookup ref))
        (k)
        (secrets--get (car ref) (cadr ref)
                      (lambda (_) (k))
                      'port (car (cddr ref))
                      'prompt (string-append name " (" (secrets--label (car ref) (cadr ref) (car (cddr ref))) "): ")
                      'on-cancel (secrets--opt opts 'on-cancel #f)))))

;; (option-secret-label name) → "user@host" naming the entry option `name`
;; refers to, or #f for a plain option — for display ("locked: alice@db").
(define (option-secret-label name)
  (let ((ref (option-secret-ref name)))
    (and ref (secrets--label (car ref) (cadr ref) (car (cddr ref))))))

;; (option-secret-forget name) — the service rejected the value behind option
;; `name`: delete that entry (when it's a reference), so the next
;; `option-ensure` prompts again. → #t if it was a reference.
(define (option-secret-forget name)
  (let ((ref (option-secret-ref name)))
    (and ref (begin (apply secret-forget ref) #t))))

;; ── commands ──────────────────────────────────────────────────────────────────
(define-command (secrets-unlock)
  (cond ((authinfo-unlocked?) (message "authinfo: already unlocked"))
        ((not (file-exists? (secrets-file)))
         (message "authinfo: no authinfo.gpg yet — it's created when you first save a password"))
        (else (secrets--enqueue
                (lambda (done)
                  (secrets--ensure-unlocked
                    (lambda () (message "authinfo: unlocked") (done))
                    done))))))

(define-command (secrets-lock)
  (authinfo-lock)
  (message "authinfo: locked (unsaved session passwords forgotten too)"))

;; Store a password ahead of time (host, user, optional port, then the password).
(define-command (secrets-add)
  (read-string "Host: "
    (lambda (host)
      (read-string "User (empty = any): "
        (lambda (user)
          (read-string "Port (empty = any): "
            (lambda (port)
              (read-passwd (string-append "Password for " (secrets--label host user port) ": ")
                (lambda (pw)
                  (secrets--enqueue
                    (lambda (done)
                      (secrets--ensure-store
                        (lambda () (secrets--save host user port pw) (done))
                        done))))))))))))

(define-command (secrets-change-passphrase)
  (if (not (file-exists? (secrets-file)))
      (message "authinfo: no authinfo.gpg yet")
      (secrets--enqueue
        (lambda (done)
          (secrets--ensure-unlocked
            (lambda ()
              (secrets--new-passphrase
                (lambda (pass)
                  (let ((err (authinfo-change-passphrase pass)))
                    (message (if (equal? err "")
                                 "authinfo: passphrase changed"
                                 (string-append "authinfo: " err))))
                  (done))
                done
                "New authinfo.gpg passphrase: "))
            done)))))
