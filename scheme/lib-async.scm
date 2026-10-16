;; requires: core

;; lib-async — named, cancellable subprocess "lanes" (the "lib-" prefix keeps
;; this file sorting before its consumers: magit.scm, shell.scm, and any
;; installed package).
;;
;; A lane is a slot for ONE long-running background subprocess — a `git push`,
;; a compile — that the user should be able to cancel with C-g. The pattern a
;; consumer follows:
;;
;;   * run:    (async-lane-run "mylane" "/bin/sh" (list "-c" cmd) 'out 'exit)
;;   * cancel: bind C-g in the owning mode's keymap to a command that calls
;;             (async-lane-quit "mylane") — kills the process if one is live,
;;             otherwise falls through to the ordinary keyboard-quit. Keep the
;;             binding scoped to the mode that owns the lane: a GLOBAL C-g
;;             that kills background work would turn "deactivate my region"
;;             into "kill my build".
;;   * exit:   the exit callback ALWAYS fires (a killed process reports its
;;             exit too); call (async-lane-finish "mylane") first — it frees
;;             the lane and returns #t when the run was cancelled, so the
;;             callback can say "cancelled" instead of reporting a failure.
;;
;; One process per lane: starting a second run while one is live is refused
;; with a message (cancel the first or let it finish). Lanes are global, not
;; buffer-local — name them after the owning package/mode ("magit",
;; "compile", …) so refusals and cancels read sensibly.
;;
;; Cancelling kills the process GROUP (each make-process child gets its own),
;; so shell-wrapped commands (`sh -c "make && …"`) die whole — the wrapper sh
;; can't orphan the real work.

(define *async-lanes* '())  ; name → (proc-id . cancelled?), live lanes only

(define (async-lane-live? name)
  (if (alist-ref *async-lanes* name) #t #f))

;; The lane's live process id (e.g. for process-send-string), or #f when idle.
(define (async-lane-proc name)
  (let ((e (alist-ref *async-lanes* name)))
    (if e (car e) #f)))

;; Was the lane's live run cancelled (killed via async-lane-cancel)?
(define (async-lane-cancelled? name)
  (let ((e (alist-ref *async-lanes* name)))
    (if e (cdr e) #f)))

(define (async-lane--drop name)
  (set! *async-lanes*
        (alist-remove *async-lanes* name)))

;; Spawn `program`/`args` on the lane → the process id, or #f (after a
;; message) when the lane already has a live process. `on-output` / `on-exit`
;; are callback names ('symbols), passed straight to make-process. An optional
;; trailing `env` list ("KEY=VALUE" / bare "KEY" = unset) applies to the child
;; only, via make-process-env — e.g. a password as PGPASSWORD instead of argv.
(define (async-lane-run name program args on-output on-exit . rest)
  (if (async-lane-live? name)
      (begin
        (message (string-append name ": a process is still running — C-g cancels it"))
        #f)
      (let ((id (if (null? rest)
                    (make-process program args on-output on-exit)
                    (make-process-env program args on-output on-exit (car rest)))))
        (set! *async-lanes* (cons (cons name (cons id #f)) *async-lanes*))
        id)))

;; Kill the lane's live process → #t; #f when the lane is idle. The process's
;; exit callback still fires afterwards — with the cancelled flag set, so
;; async-lane-finish there reports the kill as a cancel, not a failure.
(define (async-lane-cancel name)
  (let ((e (alist-ref *async-lanes* name)))
    (if e
        (begin
          (async-lane--drop name)
          (set! *async-lanes* (cons (cons name (cons (car e) #t)) *async-lanes*))
          (kill-process (car e))
          #t)
        #f)))

;; Free the lane — call FIRST in the lane's exit callback. Returns #t if the
;; finished run was cancelled (vs exiting on its own).
(define (async-lane-finish name)
  (let ((cancelled (async-lane-cancelled? name)))
    (async-lane--drop name)
    cancelled))

;; The C-g command body for a mode that owns a lane: cancel a live run, else
;; behave as the ordinary quit.
(define (async-lane-quit name)
  (when (not (async-lane-cancel name))
    (keyboard-quit)))
