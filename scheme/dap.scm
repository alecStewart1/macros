;; requires: core persistence

;; dap.scm — a Debug Adapter Protocol client (Emacs dap-mode / dape): the
;; protocol core. The transport is Rust and deliberately dumb — `dap-start`
;; (stdio) / `dap-connect` (socket) spawn or reach an adapter and deliver every
;; framed message here as raw JSON text; `dap-send` frames one message back.
;; Everything protocol-shaped lives in this file: `seq` stamping,
;; request/response correlation, the event table, the session lifecycle
;; (initialize → launch → initialized → setBreakpoints → configurationDone),
;; breakpoints with gutter dots, stepping, the stopped-frame jump, scopes and
;; variables, watches, and the *dap-console* REPL.
;;
;; This is the UI-free protocol core. The panel and transient UI live in
;; ui-dap.scm, which requires this file; it registers itself via `dap-on-change`.
;;
;; Adapters: debugpy (python) and delve (go) ship registered; add more with
;;   (dap-register-adapter id exts start-thunk launch-args-fn)
;; where `start-thunk` brings the session up (eventually calling
;; `dap--session-up` with a transport id) and `launch-args-fn file` returns the
;; adapter-specific `launch` request arguments as a hash.

;; --- faces (defaults; themes override with their own palette) ----------------
(set-face "dap-breakpoint"         "#e24b4a")  ; verified breakpoint gutter dot
(set-face "dap-breakpoint-pending" "#8a6a6a")  ; not (yet) verified by the adapter
(set-face "dap-stopped-line"       "#efb080")  ; the line execution stopped on
(set-face "dap-inline"             "#8a8a72")  ; inline locals on the stopped line
(set-face "dap-heading"            "#87c3ff")  ; panel section headings
(set-face "dap-dim"                "#707070")  ; panel de-emphasis (paths, ids)

;; --- session state ------------------------------------------------------------
(define *dap-session* -1)       ; transport id (dap-start/dap-connect), -1 = none
(define *dap-server-proc* -1)   ; a socket adapter's server process (delve), -1 = none
(define *dap-seq* 0)            ; outgoing message sequence number
(define *dap-pending* '())      ; (seq . callback) awaiting responses
(define *dap-capabilities* (hash))
(define *dap-state* 'dead)      ; dead | starting | running | stopped
(define *dap-adapter* #f)       ; the active adapter record (see registry below)
(define *dap-launch-file* "")   ; the file dap-debug launched
(define *dap-threads* '())      ; last `threads` response (raw hashes)
(define *dap-thread* 1)         ; the stopped/selected thread id
(define *dap-frames* '())       ; last `stackTrace` frames (raw hashes)
(define *dap-frame-idx* 0)      ; selected frame (0 = innermost)
(define *dap-scopes* '())       ; (name varref expensive vars) per scope of the frame
(define *dap-watches* '())      ; watch expressions (strings)
(define *dap-watch-vals* '())   ; (expr . value) results for the current stop
(define *dap-stopped-reason* "")
(define *dap-stopped-path* "")  ; truename of the file execution stopped in
(define *dap-stopped-line* 0)   ; its 1-based DAP line
(define *dap-stopped-buffer* "") ; the buffer NAME carrying the stopped-line highlight
(define *dap--pending-jump* #f) ; (truename line highlight?) awaiting its buffer's load
(define *dap--kill-target* -1)  ; the session a scheduled force-kill is aimed at

;; Procedures to run after any session/data change — ui-dap.scm registers its
;; panel refresh here, which keeps this file free of forward references.
(define *dap-on-change* '())
(define (dap-on-change proc) (set! *dap-on-change* (cons proc *dap-on-change*)))
(define (dap--changed) (for-each (lambda (p) (p)) *dap-on-change*))

;; --- small JSON helpers (string->jsexpr hashes key by symbol) -----------------
(define dap--get json-get)
(define (dap--str o k) (let ((v (dap--get o k))) (if (string? v) v "")))
(define dap--int json-int)
(define (dap--list x) (if (list? x) x '()))
(define (dap--truncate s n)
  (if (> (string-length s) n) (string-append (substring s 0 n) "…") s))

(define (dap--alive?) (>= *dap-session* 0))

;; Breakpoints and stopped frames are keyed by TRUENAME: the adapter reports
;; the paths it sees (debugpy/delve resolve symlinks, so macOS `/tmp/x.py`
;; comes back as `/private/tmp/x.py`), while a buffer keeps the path it was
;; opened with. Comparing canonical paths makes both sides agree.
(define (dap--truename path) (if (equal? path "") "" (file-truename path)))
(define (dap--buffer-truename) (dap--truename (buffer-file-name)))

;; The name of an open buffer visiting `path` (compared by truename), or #f.
(define (dap--buffer-visiting path) (find-buffer-visiting path))

;; Show `path` at 1-based `line`, optionally painting the stopped-line face.
;; An already-open buffer is switched to (its own window if it has one) and
;; positioned right away. A file not open yet loads ASYNCHRONOUSLY
;; (`open-file` reads off the UI thread), so a goto-line queued now would land
;; in the previous buffer — instead the jump is parked in
;; `*dap--pending-jump*` and completed by the find-file-hook (`dap--on-visit`).
(define (dap--visit path line highlight?)
  (let ((name (dap--buffer-visiting path)))
    (if name
        (let ((w (get-buffer-window name)))
          (set! *dap--pending-jump* #f)
          (if (>= w 0) (select-window w) (open-file (buffer-file-name-of name)))
          (goto-line (- line 1))
          (when highlight? (dap--highlight-stopped name line)))
        (begin
          (set! *dap--pending-jump* (list (dap--truename path) line highlight?))
          (open-file path)))))

(define (dap--highlight-stopped name line)
  (set! *dap-stopped-buffer* name)
  (buffer-highlight-lines name (- line 1) line "dap-stopped-line"))

;; find-file-hook: paint the visited file's breakpoint dots, and finish a jump
;; that was waiting for this file to load.
(define (dap--on-visit)
  (dap--paint-breakpoints)
  (let ((p *dap--pending-jump*))
    (when (and p (equal? (dap--buffer-truename) (car p)))
      (set! *dap--pending-jump* #f)
      (goto-line (- (nth p 1) 1))
      (when (nth p 2) (dap--highlight-stopped (current-buffer) (nth p 1)))
      (dap--inline-values))))
(define (dap--send-message m) (dap-send *dap-session* (value->jsexpr-string m)))

;; Send `command`; `arguments` is a hash or #f (omitted). `callback` (or #f)
;; receives the response `body` on success — a Scheme closure, held here, so no
;; global stub names are needed for correlation.
(define (dap-request command arguments callback)
  (when (dap--alive?)
    (set! *dap-seq* (+ *dap-seq* 1))
    (when callback
      (set! *dap-pending* (cons (cons *dap-seq* callback) *dap-pending*)))
    (dap--send-message
      (if arguments
          (hash "seq" *dap-seq* "type" "request" "command" command
                "arguments" arguments)
          (hash "seq" *dap-seq* "type" "request" "command" command)))))

;; --- incoming message dispatch ------------------------------------------------
;; Every message the adapter sends arrives here (the dap-start callback).
(define (dap--on-message json)
  (let* ((m (string->jsexpr json))
         (type (dap--str m 'type)))
    (cond ((equal? type "response") (dap--on-response m))
          ((equal? type "event")    (dap--on-event m))
          ((equal? type "request")  (dap--on-reverse-request m)))))

(define (dap--on-response m)
  (let* ((rseq (dap--int (dap--get m 'request_seq)))
         (entry (assoc rseq *dap-pending*)))
    (set! *dap-pending*
          (filter (lambda (p) (not (= (car p) rseq))) *dap-pending*))
    (cond
      ((not (dap--get m 'success))
       (let ((why (let ((msg (dap--str m 'message)))
                    (if (equal? msg "") "request failed" msg))))
         (dap--console-log (string-append (dap--str m 'command) ": " why))))
      (entry ((cdr entry) (dap--get m 'body))))))

(define (dap--on-event m)
  (let ((ev (dap--str m 'event))
        (body (dap--get m 'body)))
    (cond
      ((equal? ev "initialized") (dap--configure))
      ((equal? ev "stopped")     (dap--on-stopped body))
      ((equal? ev "continued")   (dap--continue-ui))
      ((equal? ev "output")      (dap--console-append (dap--str body 'output)))
      ((equal? ev "terminated")  (dap--on-terminated))
      ((equal? ev "exited")
       (dap--console-log (string-append "program exited with code "
                                        (int->string (dap--int (dap--get body 'exitCode))))))
      ;; thread start/exit — the list refreshes wholesale on the next stop.
      (else #f))))

;; Reverse requests (runInTerminal, startDebugging): we advertise no support in
;; `initialize`, so a well-behaved adapter never sends one — but answer rather
;; than hang one that does.
(define (dap--on-reverse-request m)
  (set! *dap-seq* (+ *dap-seq* 1))
  (dap--send-message
    (hash "seq" *dap-seq* "type" "response"
          "request_seq" (dap--int (dap--get m 'seq))
          "command" (dap--str m 'command)
          "success" #f "message" "unsupported")))

;; --- adapter registry ----------------------------------------------------------
;; An adapter record: (id exts start-thunk launch-args-fn).
(define *dap-adapters* '())
(define (dap-register-adapter id exts start launch)
  (set! *dap-adapters*
        (alist-set *dap-adapters* id (list exts start launch))))

(define (dap--file-ext path) (file-extension path))

(define (dap--adapter-for path)
  (let ((ext (dap--file-ext path)))
    (let loop ((as *dap-adapters*))
      (cond ((null? as) #f)
            ((member ext (nth (car as) 1)) (car as))
            (else (loop (cdr as)))))))

;; --- session lifecycle ----------------------------------------------------------
;; Start debugging the current buffer's file with whichever adapter claims its
;; extension. The adapter's start thunk brings the transport up (immediately
;; for stdio adapters; after a socket server announces itself for delve) and
;; calls `dap--session-up`, which runs the initialize → launch handshake. The
;; `initialized` event then triggers breakpoint sync + configurationDone.
(define-command (dap-debug)
  (cond
    ((or (dap--alive?) (not (eq? *dap-state* 'dead)))
     (message "dap: session already running (dap-quit first)"))
    (else
     (let* ((path (buffer-file-name))
            (adapter (dap--adapter-for path)))
       (cond
         ((equal? path "") (message "dap: buffer is not visiting a file"))
         ((not adapter)
          (message (string-append "dap: no adapter registered for ." (dap--file-ext path))))
         (else
          (set! *dap-adapter* adapter)
          (set! *dap-launch-file* (dap--truename path))
          (set! *dap-state* 'starting)
          (dap--set-modeline)
          (dap--console-show)
          (dap--console-log (string-append "session: " (car adapter) " — " path))
          ((nth adapter 2))
          (dap--changed)))))))

(define (dap--session-up id)
  (set! *dap-session* id)
  (dap-request "initialize"
    (hash "clientID" "macros" "clientName" "macros"
          "adapterID" (car *dap-adapter*)
          "pathFormat" "path" "linesStartAt1" #t "columnsStartAt1" #t
          "supportsRunInTerminalRequest" #f "locale" "en-US")
    (lambda (body)
      (set! *dap-capabilities* (if (hash? body) body (hash)))
      (dap-request "launch" ((nth *dap-adapter* 3) *dap-launch-file*)
                   (lambda (b) #f)))))

;; The adapter is ready for configuration: send every file's breakpoints, then
;; configurationDone releases the debuggee.
(define (dap--configure)
  (for-each dap--sync-file-breakpoints (dap--breakpoint-files))
  (dap-request "configurationDone" #f (lambda (b) #f))
  (set! *dap-state* 'running)
  (dap--set-modeline)
  (dap--changed))

(define (dap--on-terminated)
  (dap--console-log "program terminated")
  (when (dap--alive?)
    (dap-request "disconnect" #f (lambda (b) #f))
    (dap--schedule-kill)))

(define-command (dap-quit)
  (cond
    ((dap--alive?)
     (dap-request "disconnect" (hash "terminateDebuggee" #t) (lambda (b) #f))
     ;; A stuck adapter is reaped; a prompt one already exited by then.
     (dap--schedule-kill))
    ;; Starting but no transport yet (delve still building, never announced,
    ;; or `dlv` not even found): reset now — this also kills a server process.
    ((not (eq? *dap-state* 'dead)) (dap--on-exit "quit"))
    (else (message "dap: no session"))))

;; Force-kill THIS session after a grace period — the target is pinned so a
;; timer left over from a quick quit → debug can't kill the next session.
(define (dap--schedule-kill)
  (set! *dap--kill-target* *dap-session*)
  (run-with-timer 1500 "dap--force-kill"))
(define (dap--force-kill)
  (when (and (dap--alive?) (= *dap-session* *dap--kill-target*))
    (dap-kill *dap-session*)))

;; The transport's on-exit: the adapter died or was killed — reset everything.
(define (dap--on-exit code)
  (dap--console-log (string-append "adapter exited (" code ")"))
  (set! *dap-session* -1)
  (set! *dap-state* 'dead)
  (set! *dap-pending* '())
  (set! *dap-capabilities* (hash))
  (set! *dap-threads* '())
  (set! *dap-frames* '())
  (set! *dap-scopes* '())
  (set! *dap-watch-vals* '())
  (set! *dap--pending-jump* #f)
  ;; Per-session state must not leak into the next session: a stale thread id
  ;; made `dap-pause` target the old session's thread, and stale "pending"
  ;; marks showed already-verified breakpoints as unbound.
  (set! *dap-thread* 1)
  (set! *dap-frame-idx* 0)
  (set! *dap-bp-unverified* '())
  (set! *dap--cond-target* '())
  (dap--clear-stopped-highlight)
  (clear-line-suffixes)
  (when (>= *dap-server-proc* 0)
    (kill-process *dap-server-proc*)
    (set! *dap-server-proc* -1))
  (dap--set-modeline)
  (dap--changed))

(define-command (dap-restart)
  (cond
    ((not (dap--alive?)) (message "dap: no session"))
    ((dap--get *dap-capabilities* 'supportsRestartRequest)
     (dap-request "restart" #f (lambda (b) #f))
     (dap--continue-ui))
    (else (message "dap: adapter can't restart — dap-quit, then dap-debug"))))

;; --- breakpoints -----------------------------------------------------------------
;; (path line condition) with 1-based DAP lines; `condition` "" = plain.
;; Persisted to ~/.config/macros/dap-breakpoints across sessions.
(define *dap-breakpoints* '())
(define *dap-bp-unverified* '())  ; (path . line) the adapter rejected/hasn't bound

(define (dap--bps-for path)
  (filter (lambda (bp) (equal? (car bp) path)) *dap-breakpoints*))
(define (dap--bp-at path line)
  (let ((hits (filter (lambda (bp) (and (equal? (car bp) path) (= (nth bp 1) line)))
                      *dap-breakpoints*)))
    (if (null? hits) #f (car hits))))
(define (dap--breakpoint-files)
  (let loop ((bps *dap-breakpoints*) (files '()))
    (cond ((null? bps) files)
          ((member (car (car bps)) files) (loop (cdr bps) files))
          (else (loop (cdr bps) (cons (car (car bps)) files))))))

;; Paint the ACTIVE buffer's breakpoint dots. Dots are stored per document, so
;; one paint on visit persists across window/buffer switches.
(define (dap--paint-breakpoints)
  (let ((path (dap--buffer-truename)))
    ;; Only file buffers carry dots (painting from the *dap* panel cleared the
    ;; panel and left the source file's stale dots on screen).
    (when (not (equal? path ""))
      (clear-gutter-dots)
      (for-each
        (lambda (bp)
          (set-gutter-dot (- (nth bp 1) 1)
                          (if (member (cons path (nth bp 1)) *dap-bp-unverified*)
                              "dap-breakpoint-pending"
                              "dap-breakpoint")))
        (dap--bps-for path)))))

;; Replace-per-file sync to a live adapter (the DAP model: setBreakpoints
;; carries the file's whole list). The response records which ones the adapter
;; actually bound, for the pending face + panel.
(define (dap--sync-file-breakpoints path)
  (when (dap--alive?)
    (dap-request "setBreakpoints"
      (hash "source" (hash "path" path)
            "breakpoints"
            (map (lambda (bp)
                   (if (equal? (nth bp 2) "")
                       (hash "line" (nth bp 1))
                       (hash "line" (nth bp 1) "condition" (nth bp 2))))
                 (dap--bps-for path)))
      (lambda (body) (dap--note-verified path (dap--list (dap--get body 'breakpoints)))))))

(define (dap--note-verified path results)
  (set! *dap-bp-unverified*
        (alist-remove *dap-bp-unverified* path))
  (let loop ((bps (dap--bps-for path)) (rs results))
    (when (and (not (null? bps)) (not (null? rs)))
      (when (not (dap--get (car rs) 'verified))
        (set! *dap-bp-unverified*
              (cons (cons path (nth (car bps) 1)) *dap-bp-unverified*)))
      (loop (cdr bps) (cdr rs))))
  (dap--paint-breakpoints)
  (dap--changed))

(define (dap--bp-mutate path)
  (dap--paint-breakpoints)
  (dap--sync-file-breakpoints path)
  (dap--save-breakpoints)
  (dap--changed))

(define-command (dap-toggle-breakpoint)
  (let ((path (dap--buffer-truename))
        (line (+ (buffer-line) 1)))
    (if (equal? path "")
        (message "dap: buffer is not visiting a file")
        (begin
          (if (dap--bp-at path line)
              (set! *dap-breakpoints*
                    (filter (lambda (bp) (not (and (equal? (car bp) path)
                                                   (= (nth bp 1) line))))
                            *dap-breakpoints*))
              (set! *dap-breakpoints*
                    (cons (list path line "") *dap-breakpoints*)))
          (dap--bp-mutate path)))))

;; Set/edit the breakpoint condition on the current line (adds one if absent).
(define *dap--cond-target* '())
(define-command (dap-breakpoint-condition)
  (let ((path (dap--buffer-truename))
        (line (+ (buffer-line) 1)))
    (if (equal? path "")
        (message "dap: buffer is not visiting a file")
        (begin
          (set! *dap--cond-target* (list path line))
          (let ((existing (dap--bp-at path line)))
            (read-string "Break when: " 'dap--set-condition
                         (if existing (nth existing 2) "")))))))
(define (dap--set-condition cond-expr)
  (when (not (null? *dap--cond-target*))
    (let ((path (car *dap--cond-target*))
          (line (nth *dap--cond-target* 1)))
      (set! *dap-breakpoints*
            (cons (list path line cond-expr)
                  (filter (lambda (bp) (not (and (equal? (car bp) path)
                                                 (= (nth bp 1) line))))
                          *dap-breakpoints*)))
      (dap--bp-mutate path))))

(define-command (dap-breakpoints-clear)
  (let ((files (dap--breakpoint-files)))
    (set! *dap-breakpoints* '())
    (set! *dap-bp-unverified* '())
    (dap--paint-breakpoints)
    (for-each dap--sync-file-breakpoints files)
    (dap--save-breakpoints)
    (dap--changed)
    (message "dap: all breakpoints cleared")))

;; After a save, re-sync the file's breakpoints (lines may have moved under
;; edits; the adapter rebinds against the saved text).
(define (dap--after-save)
  (let ((path (dap--buffer-truename)))
    (when (and (dap--alive?) (not (equal? path "")) (not (null? (dap--bps-for path))))
      (dap--sync-file-breakpoints path))))

;; --- persistence (breakpoints survive restarts; helpers from persistence.scm) --
(define (dap--bp-file) (persistence-file "dap-breakpoints"))
(define (dap--save-breakpoints)
  (persistence-write-lines "dap-breakpoints"
    (map (lambda (bp) (string-append (car bp) "\t"
                                     (int->string (nth bp 1)) "\t"
                                     (nth bp 2)))
         *dap-breakpoints*)))
(define (dap--load-breakpoints)
  (for-each
    (lambda (line)
      (let ((parts (split-many line "\t")))
        (when (>= (length parts) 2)
          (let ((n (string->number (nth parts 1))))
            (when (and n (file-exists? (nth parts 0)))
              (set! *dap-breakpoints*
                    (cons (list (dap--truename (nth parts 0)) (exact (round n))
                                (if (>= (length parts) 3) (nth parts 2) ""))
                          *dap-breakpoints*)))))))
    (persistence-read-lines "dap-breakpoints")))

;; --- execution control ------------------------------------------------------------
(define (dap--exec command)
  (if (not (dap--alive?))
      (message "dap: no session")
      (begin
        (dap-request command (hash "threadId" *dap-thread*) (lambda (b) #f))
        (dap--continue-ui))))
(define-command (dap-continue) (dap--exec "continue"))
(define-command (dap-next)     (dap--exec "next"))
(define-command (dap-step-in)  (dap--exec "stepIn"))
(define-command (dap-step-out) (dap--exec "stepOut"))
(define-command (dap-pause)
  (if (not (dap--alive?))
      (message "dap: no session")
      (dap-request "pause" (hash "threadId" *dap-thread*) (lambda (b) #f))))

;; The running-again UI reset (stepping, continue, the `continued` event).
(define (dap--continue-ui)
  (when (not (eq? *dap-state* 'running))
    (set! *dap-state* 'running)
    (set! *dap-frames* '())
    (set! *dap-scopes* '())
    (set! *dap-watch-vals* '())
    (set! *dap--pending-jump* #f)
    (dap--clear-stopped-highlight)
    (clear-line-suffixes)
    (dap--set-modeline)
    (dap--changed)))

;; --- the stop: jump, stack, scopes, inline values ---------------------------------
(define (dap--on-stopped body)
  (set! *dap-state* 'stopped)
  (set! *dap-stopped-reason* (dap--str body 'reason))
  (let ((tid (dap--get body 'threadId)))
    (when (number? tid) (set! *dap-thread* (dap--int tid))))
  (dap--set-modeline)
  (dap-request "threads" #f
    (lambda (b)
      (set! *dap-threads* (dap--list (dap--get b 'threads)))
      (dap--changed)))
  (dap-request "stackTrace"
    (hash "threadId" *dap-thread* "startFrame" 0 "levels" 20)
    (lambda (b)
      (set! *dap-frames* (dap--list (dap--get b 'stackFrames)))
      (dap--select-frame 0 #t))))

;; Switch the selected thread (panel RET on a thread line) and refetch its stack.
(define (dap-select-thread tid)
  (set! *dap-thread* tid)
  (dap-request "stackTrace"
    (hash "threadId" tid "startFrame" 0 "levels" 20)
    (lambda (b)
      (set! *dap-frames* (dap--list (dap--get b 'stackFrames)))
      (dap--select-frame 0 #t))))

;; Select stack frame `idx`: optionally jump to its source, then refetch
;; scopes/variables and watches against it.
(define (dap--select-frame idx jump?)
  (set! *dap-frame-idx* idx)
  (when (and (>= idx 0) (< idx (length *dap-frames*)))
    (let ((f (nth *dap-frames* idx)))
      (when jump? (dap--jump-to-frame f))
      (dap--fetch-scopes f)
      (dap--refresh-watches)))
  (dap--changed))

(define (dap--current-frame)
  (if (and (>= *dap-frame-idx* 0) (< *dap-frame-idx* (length *dap-frames*)))
      (nth *dap-frames* *dap-frame-idx*)
      #f))
(define (dap--current-frame-id)
  (let ((f (dap--current-frame)))
    (if f (dap--int (dap--get f 'id)) #f)))

(define-command (dap-up-frame)   (dap--reselect (+ *dap-frame-idx* 1)))
(define-command (dap-down-frame) (dap--reselect (- *dap-frame-idx* 1)))
(define (dap--reselect idx)
  (when (and (>= idx 0) (< idx (length *dap-frames*)))
    (dap--select-frame idx #t)))

(define (dap--jump-to-frame f)
  (let* ((src (dap--get f 'source))
         (path (dap--str src 'path))
         (line (dap--int (dap--get f 'line))))
    (dap--clear-stopped-highlight)
    (when (and (not (equal? path "")) (file-exists? path) (> line 0))
      (set! *dap-stopped-path* (dap--truename path))
      (set! *dap-stopped-line* line)
      ;; Breakpoint dots are per-document and painted on visit, so the jump
      ;; needn't repaint them.
      (dap--visit path line #t))))

(define (dap--clear-stopped-highlight)
  (when (not (equal? *dap-stopped-buffer* ""))
    (buffer-clear-highlights *dap-stopped-buffer*)
    (set! *dap-stopped-buffer* ""))
  (set! *dap-stopped-path* "")
  (set! *dap-stopped-line* 0))

;; Scopes for the selected frame; each non-expensive scope's variables fetch
;; eagerly (Locals first — scope order is the adapter's). An entry is
;; (name varref expensive vars).
(define (dap--fetch-scopes f)
  (set! *dap-scopes* '())
  (dap-request "scopes" (hash "frameId" (dap--int (dap--get f 'id)))
    (lambda (b)
      (set! *dap-scopes*
            (map (lambda (s)
                   (list (let ((n (dap--str s 'name))) (if (equal? n "") "Scope" n))
                         (dap--int (dap--get s 'variablesReference))
                         (if (dap--get s 'expensive) #t #f)
                         '()))
                 (dap--list (dap--get b 'scopes))))
      (for-each
        (lambda (entry)
          (when (and (> (nth entry 1) 0) (not (nth entry 2)))
            (dap--fetch-variables (nth entry 1) dap--scope-vars-arrived)))
        *dap-scopes*)
      (dap--changed))))

;; Fetch the children of a variablesReference; `k` gets (ref vars).
(define (dap--fetch-variables ref k)
  (dap-request "variables" (hash "variablesReference" ref)
    (lambda (b) (k ref (dap--list (dap--get b 'variables))))))

(define (dap--scope-vars-arrived ref vars)
  (set! *dap-scopes*
        (map (lambda (entry)
               (if (= (nth entry 1) ref)
                   (list (nth entry 0) (nth entry 1) (nth entry 2) vars)
                   entry))
             *dap-scopes*))
  (dap--inline-values)
  (dap--changed))

;; A few first-scope locals rendered as virtual text after the stopped line —
;; the modest cousin of dap-mode's inline values.
(define (dap--inline-values)
  (when (and (equal? (dap--buffer-truename) *dap-stopped-path*)
             (> *dap-stopped-line* 0)
             (not (null? *dap-scopes*)))
    (let* ((vars (nth (car *dap-scopes*) 3))
           (shown (take vars 4))
           (txt (string-join
                  (map (lambda (v)
                         (string-append (dap--str v 'name) " = "
                                        (dap--truncate (dap--str v 'value) 40)))
                       shown)
                  ", ")))
      (when (not (equal? txt ""))
        (set-line-suffix (- *dap-stopped-line* 1)
                         (string-append "   ⏸ " txt)
                         "dap-inline")))))

;; --- evaluate / watches -------------------------------------------------------------
(define (dap--evaluate expr context k)
  (let ((fid (dap--current-frame-id)))
    (dap-request "evaluate"
      (if fid
          (hash "expression" expr "context" context "frameId" fid)
          (hash "expression" expr "context" context))
      (lambda (b) (k (dap--str b 'result))))))

;; Evaluate the region (if active) or a prompted expression; echo the result.
(define-command (dap-eval)
  (cond
    ((not (dap--alive?)) (message "dap: no session"))
    ((region-active?) (dap--eval-echo (region-text)))
    (else (read-string "Eval: " 'dap--eval-echo))))
(define (dap--eval-echo expr)
  (when (not (equal? (string-trim expr) ""))
    (dap--evaluate (string-trim expr) "hover"
                   (lambda (r) (message (string-append expr " ⇒ " r))))))

(define-command (dap-watch-add) (read-string "Watch expression: " 'dap--watch-add))
(define (dap--watch-add expr)
  (when (not (equal? (string-trim expr) ""))
    (set! *dap-watches* (append *dap-watches* (list (string-trim expr))))
    (if (eq? *dap-state* 'stopped) (dap--refresh-watches) (dap--changed))))
(define-command (dap-watch-clear)
  (set! *dap-watches* '())
  (set! *dap-watch-vals* '())
  (dap--changed))

(define (dap--refresh-watches)
  (set! *dap-watch-vals* '())
  (for-each
    (lambda (expr)
      (dap--evaluate expr "watch"
        (lambda (r)
          (set! *dap-watch-vals* (cons (cons expr r) *dap-watch-vals*))
          (dap--changed))))
    *dap-watches*))

;; --- *dap-console*: program output + a REPL against the selected frame -------------
(define *dap-console-name* "*dap-console*")
;; `append-to-buffer` on a buffer that isn't in any window SWITCHES the
;; selected window to it — fine for a console the user is watching, but
;; debuggee output must never yank focus once the console has been closed.
;; So output for a hidden console accumulates in a capped backlog that
;; `dap--console-show` (M-x dap-console) flushes.
(define *dap--console-backlog* "")
(define *dap--console-backlog-max* 65536)
;; #t between `dap--console-show` and the first time the window snapshot
;; reports the console visible (the split is queued, not yet applied, so
;; get-buffer-window still says -1 within that window).
(define *dap--console-opening* #f)
(define (dap--console-append text)
  (when (not (equal? text ""))
    (cond
      ((>= (get-buffer-window *dap-console-name*) 0)
       (set! *dap--console-opening* #f)
       (append-to-buffer *dap-console-name* text))
      (*dap--console-opening* (append-to-buffer *dap-console-name* text))
      (else
       (let* ((s (string-append *dap--console-backlog* text))
              (n (string-length s)))
         (set! *dap--console-backlog*
               (if (> n *dap--console-backlog-max*)
                   (substring s (- n *dap--console-backlog-max*) n)
                   s)))))))
(define (dap--console-log line) (dap--console-append (string-append "· " line "\n")))

;; Show the console in a bottom split (the *compilation* pattern), keeping
;; focus where it was, and flush any output that arrived while it was hidden.
;; No-op when it's already visible.
(define (dap--console-show)
  (when (< (get-buffer-window *dap-console-name*) 0)
    (let ((orig (selected-window)))
      (split-window-below)
      (other-window)
      (append-to-buffer *dap-console-name* *dap--console-backlog*)
      (set! *dap--console-backlog* "")
      (set! *dap--console-opening* #t)
      (set-major-mode "dap-console-mode")
      (select-window orig))))
(define-command (dap-console) (dap--console-show))

;; RET in the console: send the current line to `evaluate` (context "repl").
(define (dap-console-send-input)
  (let* ((b (line-bounds))
         (line (string-trim (buffer-substring (car b) (cadr b)))))
    (insert "\n")
    (cond
      ((equal? line "") #f)
      ((not (dap--alive?)) (dap--console-log "no session"))
      (else
       (dap--evaluate line "repl"
         (lambda (r) (dap--console-append (string-append r "\n"))))))))

(define-key "dap-console-mode" "enter" 'dap-console-send-input)

;; --- modeline ------------------------------------------------------------------------
;; The "dap" slot; ui-dap.scm adds the segment to the mode-line format.
(define (dap--set-modeline)
  (set-mode-line-slot "dap"
    (cond ((eq? *dap-state* 'stopped)
           (string-append "⏸ "
                          (if (equal? *dap-stopped-reason* "") "stopped" *dap-stopped-reason*)))
          ((eq? *dap-state* 'running)  "▶ dbg")
          ((eq? *dap-state* 'starting) "dbg…")
          (else ""))))

;; --- bundled adapters ------------------------------------------------------------------
;; python — debugpy speaks DAP on stdio (`python -m debugpy.adapter`).
;; `pip install debugpy`; point *dap-python* at a venv's python if needed.
(define *dap-python* "python3")
(dap-register-adapter "debugpy" (list "py")
  (lambda ()
    (dap--session-up
      (dap-start *dap-python* (list "-m" "debugpy.adapter") (buffer-directory)
                 'dap--on-message 'dap--on-exit)))
  (lambda (file)
    (hash "request" "launch" "type" "python" "program" file
          "console" "internalConsole" "cwd" (buffer-directory)
          "justMyCode" #t)))

;; go — delve serves DAP on a socket, not stdio: spawn `dlv dap --listen`,
;; connect when it announces itself. Debugs the package in the file's
;; directory (delve builds it), the `dlv debug` model. It listens on an
;; OS-assigned port (`:0`, so concurrent editors / a stale dlv can't collide)
;; and we connect to whatever port its "listening at: host:port" line names.
(define (dap--delve-port line)
  (let ((m (regexp-captures "listening at:? *[^ ]*:([0-9]+)" line)))
    (if (null? m) #f (string->number (nth m 1)))))
(define (dap--delve-output line)
  (dap--console-append (string-append line "\n"))
  (let ((port (dap--delve-port line)))
    (when (and port (not (dap--alive?)) (eq? *dap-state* 'starting))
      (dap--session-up
        (dap-connect "127.0.0.1" port 'dap--on-message 'dap--on-exit)))))
;; dlv exited. If it never got a session up (not installed, build failure,
;; killed by dap-quit while starting) nothing else will reset the state.
(define (dap--delve-exit code)
  (set! *dap-server-proc* -1)
  (when (and (not (dap--alive?)) (not (eq? *dap-state* 'dead)))
    (dap--on-exit code)))
(dap-register-adapter "delve" (list "go")
  (lambda ()
    (set! *dap-server-proc*
          (make-process "dlv" (list "dap" "--listen=127.0.0.1:0")
                        'dap--delve-output 'dap--delve-exit)))
  (lambda (file)
    (hash "request" "launch" "mode" "debug"
          "program" (parent-directory file))))

;; --- wiring ---------------------------------------------------------------------------
(add-hook "find-file-hook" "dap--on-visit")
;; Breakpoints changed from another buffer (the *dap* panel) repaint when the
;; file's buffer is shown again, not only when it's first visited.
(add-hook "buffer-switch-hook" "dap--paint-breakpoints")
(add-hook "after-save-hook" "dap--after-save")
(dap--load-breakpoints)
