//! Subprocesses and embedded terminals for [`Editor`], extracted from the editor module for size.
//! These methods extend the same `Editor` type via an additional
//! `impl` block; cross-module entry points are `pub(super)`.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::process::{ProcessGroup, Signal};
use crate::util::HideConsole;

use futures::StreamExt;
use gpui::{Context, Pixels, Point};

use crate::script::DapStartArgs;
use crate::terminal::{TermEvent, Terminal};

use super::*;

/// Encode a [`Keystroke`] as the bytes a PTY expects, for the embedded terminal.
/// Returns `None` for keystrokes that produce nothing (e.g. an unhandled control
/// chord), which are then swallowed. Meta (`alt`) is sent as an ESC prefix.
pub(super) fn keystroke_to_bytes(ks: &Keystroke) -> Option<Vec<u8>> {
    let m = &ks.modifiers;
    let key = ks.key.as_str();

    // Named / navigation keys → control bytes or CSI sequences.
    let named: Option<Vec<u8>> = match key {
        "enter" | "return" => Some(vec![b'\r']),
        "escape" => Some(vec![0x1b]),
        "backspace" => Some(vec![0x7f]),
        "delete" => Some(b"\x1b[3~".to_vec()),
        "tab" => Some(if m.shift {
            b"\x1b[Z".to_vec()
        } else {
            vec![b'\t']
        }),
        "up" => Some(b"\x1b[A".to_vec()),
        "down" => Some(b"\x1b[B".to_vec()),
        "right" => Some(b"\x1b[C".to_vec()),
        "left" => Some(b"\x1b[D".to_vec()),
        "home" => Some(b"\x1b[H".to_vec()),
        "end" => Some(b"\x1b[F".to_vec()),
        "pageup" => Some(b"\x1b[5~".to_vec()),
        "pagedown" => Some(b"\x1b[6~".to_vec()),
        _ => None,
    };
    if let Some(named) = named {
        return Some(named);
    }

    // Control chords → C0 control bytes (C-a = 0x01, … C-/ = 0x1f).
    if m.control {
        let byte = match key {
            "space" | "@" => Some(0x00u8),
            k if k.len() == 1 => match k.as_bytes()[0] {
                c @ b'a'..=b'z' => Some(c - b'a' + 1),
                b'[' => Some(0x1b),
                b'\\' => Some(0x1c),
                b']' => Some(0x1d),
                b'^' => Some(0x1e),
                b'_' | b'/' => Some(0x1f),
                _ => None,
            },
            _ => None,
        };
        return byte.map(|b| if m.alt { vec![0x1b, b] } else { vec![b] });
    }

    // Printable text (shifted char from the IME, else a single-char key name).
    let text = ks
        .key_char
        .clone()
        .filter(|t| !t.is_empty())
        .or_else(|| (key.chars().count() == 1).then(|| key.to_string()))?;
    let mut bytes = text.into_bytes();
    if m.alt {
        let mut v = vec![0x1b];
        v.append(&mut bytes);
        return Some(v);
    }
    Some(bytes)
}

/// A message from a [`make_process`](Editor::make_process) worker: one stdout
/// line, or the exit code once the worker has reaped the child.
enum ChildMsg {
    Line(String),
    Exited(i32),
}

/// How long a terminated process group gets to exit on SIGTERM before SIGKILL.
const TERM_GRACE: Duration = Duration::from_millis(500);

/// A live `make-process` child kept so Scheme can write to its stdin or kill
/// it. The child's [`ProcessGroup`] is shared with the worker thread that reaps it (so the UI
/// never blocks in `wait`); stdin writes go through a dedicated writer thread
/// (so a full pipe never blocks the UI). Dropping the handle — the owning
/// buffer is killed, the editor goes away — terminates the process group.
pub(super) struct ProcessHandle {
    child: Arc<Mutex<ProcessGroup>>,
    stdin: Option<std::sync::mpsc::Sender<Vec<u8>>>,
    /// Buffer whose kill also kills this process (`set-process-buffer`).
    buffer: Option<String>,
}

/// Lock a shared child, ignoring poison (a panicked holder can't corrupt it).
fn lock_child(child: &Mutex<ProcessGroup>) -> std::sync::MutexGuard<'_, ProcessGroup> {
    child.lock().unwrap_or_else(|e| e.into_inner())
}

/// Signal `child`'s whole process group (it leads its own group, see
/// [`Editor::make_process`]) if it hasn't been reaped; true if it was still
/// running. Holding the lock across the check and the kill means the reaper
/// can't free the pid/pgid in between, so we never signal a recycled group.
fn signal_group(child: &Mutex<ProcessGroup>, sig: Signal) -> bool {
    lock_child(child).signal(sig)
}

impl ProcessHandle {
    /// SIGKILL the group now (Emacs `kill-process`).
    fn kill_now(&self) {
        signal_group(&self.child, Signal::Kill);
    }

    /// Has the child exited (polling, never blocking)?
    fn exited(&self) -> bool {
        !matches!(lock_child(&self.child).try_wait(), Ok(None))
    }
}

impl Drop for ProcessHandle {
    /// SIGTERM the group, then SIGKILL it after [`TERM_GRACE`] if it lingers —
    /// on a background thread, so dropping never blocks the UI. No-op for a
    /// process that already exited (the normal exit path drops it too).
    fn drop(&mut self) {
        if signal_group(&self.child, Signal::Term) {
            let child = self.child.clone();
            std::thread::spawn(move || {
                std::thread::sleep(TERM_GRACE);
                signal_group(&child, Signal::Kill);
            });
        }
    }
}

impl Editor {
    /// Bind process `id` to buffer `name`: killing that buffer terminates the
    /// process (Emacs `set-process-buffer`). Unbound processes live until they
    /// exit, are killed, or the app quits.
    pub(super) fn set_process_buffer(&mut self, id: u64, name: String) {
        if let Some(h) = self.processes.get_mut(&id) {
            h.buffer = Some(name);
        }
    }

    /// Terminate every process bound to buffer `name` (it's being killed). The
    /// exit callbacks still fire as each one is reaped.
    pub(super) fn kill_processes_for_buffer(&mut self, name: &str) {
        for h in self.processes.values() {
            if h.buffer.as_deref() == Some(name) && signal_group(&h.child, Signal::Term) {
                let child = h.child.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(TERM_GRACE);
                    signal_group(&child, Signal::Kill);
                });
            }
        }
    }

    /// App quit: SIGTERM every `make-process` group, give them a brief grace
    /// to exit, SIGKILL the stragglers; and drop embedded terminals (their PTY
    /// teardown SIGHUPs the shell). Synchronous — the app is exiting, so a
    /// background "kill later" thread would never run.
    pub(super) fn shutdown_processes(&mut self) {
        let handles: Vec<ProcessHandle> = self.processes.drain().map(|(_, h)| h).collect();
        let live: Vec<&ProcessHandle> = handles
            .iter()
            .filter(|h| signal_group(&h.child, Signal::Term))
            .collect();
        let deadline = Instant::now() + Duration::from_millis(300);
        while Instant::now() < deadline && live.iter().any(|h| !h.exited()) {
            std::thread::sleep(Duration::from_millis(10));
        }
        for h in &live {
            h.kill_now();
        }
        drop(handles);
        self.terminals.clear();
        self.terminal_geom.clear();
    }

    /// Spawn `program` asynchronously (Emacs `make-process`): a worker thread
    /// reads its stdout line by line and forwards each line to the foreground,
    /// which calls the Steel `on_output` fn with it; `on_exit` runs when the
    /// process finishes. The UI never blocks, and the callbacks' commands flow
    /// through the normal outcome drain — so `(append-to-buffer …)` etc. work.
    pub(super) fn make_process(
        &mut self,
        spec: crate::script::MakeProcessArgs,
        cx: &mut Context<Self>,
    ) {
        let crate::script::MakeProcessArgs {
            id,
            program,
            args,
            env,
            on_output,
            on_exit,
        } = spec;
        // Spawn here (not on the worker) so we can keep the child for stdin/kill.
        // Expand a leading `~/` — `Command::new` does no shell expansion.
        let program = crate::util::expand_tilde(&program);
        let mut cmd = Command::new(&program);
        cmd.args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .hide_console();
        env.apply(&mut cmd);
        // Its own process group, so kill_process can take down the whole tree
        // — a shell-wrapped command (`sh -c "make && …"`) dies with its shell
        // instead of being orphaned by it.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => {
                self.echo(format!("make-process: {e}"), cx);
                return;
            }
        };
        let stdout = child.stdout.take();
        // stdin writes go through their own thread: a child that stops reading
        // would otherwise block `write_all` — and the UI — once the pipe fills.
        let stdin = child.stdin.take().map(|mut pipe| {
            let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
            std::thread::spawn(move || {
                for chunk in rx {
                    if pipe.write_all(&chunk).and_then(|_| pipe.flush()).is_err() {
                        break;
                    }
                }
                // Sender dropped (handle gone) or broken pipe → stdin closes.
            });
            tx
        });
        let child = Arc::new(Mutex::new(ProcessGroup::new(child)));
        let reaper = child.clone();
        self.processes.insert(
            id,
            ProcessHandle {
                child,
                stdin,
                buffer: None,
            },
        );

        let (tx, mut rx) = futures::channel::mpsc::unbounded::<ChildMsg>();
        std::thread::spawn(move || {
            if let Some(out) = stdout {
                for l in lossy_lines(BufReader::new(out)) {
                    if tx.unbounded_send(ChildMsg::Line(l)).is_err() {
                        break;
                    }
                }
            }
            // stdout closed; reap here, never on the UI thread. Poll with
            // try_wait (not a blocking wait) so the lock is free between polls
            // for kill-process / buffer-kill signalling.
            let code = ProcessGroup::wait_shared(&reaper)
                .and_then(|status| status.code())
                .unwrap_or(-1);
            let _ = tx.unbounded_send(ChildMsg::Exited(code));
        });
        cx.spawn(async move |this, cx| {
            while let Some(msg) = rx.next().await {
                let alive = this
                    .update(cx, |editor, _| match &msg {
                        ChildMsg::Line(line) => {
                            // Publish the active buffer first, so a callback
                            // reading `(current-buffer)` sees the buffer the user
                            // is actually in — not a stale snapshot. Without this,
                            // async output routed by current-buffer (e.g.
                            // agent-shell streaming) can steal focus to the wrong
                            // buffer. Mirrors `run_script`/`run_hook`.
                            editor.publish_point();
                            editor.script.dispatch_action(&on_output, Some(line))
                        }
                        ChildMsg::Exited(code) => {
                            // Already reaped by the worker: dropping the handle
                            // is a no-op signal-wise.
                            editor.processes.remove(&id);
                            if let Some(cb) = &on_exit {
                                editor.script.dispatch_action(cb, Some(&code.to_string()));
                            }
                        }
                    })
                    .is_ok();
                if !alive || matches!(msg, ChildMsg::Exited(_)) {
                    break;
                }
            }
        })
        .detach();
    }

    /// Write `text` to a live process's stdin (Emacs `process-send-string`).
    pub(super) fn send_to_process(&mut self, id: u64, text: &str, cx: &mut Context<Self>) {
        // Queued to the writer thread — never blocks on a full pipe.
        if let Some(handle) = self.processes.get(&id)
            && let Some(stdin) = handle.stdin.as_ref()
            && stdin.send(text.as_bytes().to_vec()).is_err()
        {
            self.echo(
                "process-send-string: process stdin is closed".to_string(),
                cx,
            );
        }
    }

    /// Kill a live process; its reader thread then reports exit (Emacs `kill-process`).
    pub(super) fn kill_process(&mut self, id: u64) {
        if let Some(handle) = self.processes.get(&id) {
            // The child got its own process group at spawn — signal the group,
            // so a wrapper shell's children die too (not just the shell).
            handle.kill_now();
        }
    }

    // --- DAP sessions (`dap-start` / `dap-connect` / `dap-send` / `dap-kill`) ---
    // The [`crate::dap::DapClient`] transport is protocol-free; these methods
    // only pump its framed messages to the Steel callbacks, exactly like
    // [`make_process`](Self::make_process) pumps stdout lines.

    /// Start a stdio DAP session: spawn the adapter and forward every framed
    /// message to Steel fn `on_message` as raw JSON text.
    pub(super) fn dap_start(&mut self, args: DapStartArgs, cx: &mut Context<Self>) {
        let dir = if args.cwd.is_empty() {
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/"))
        } else {
            std::path::PathBuf::from(&args.cwd)
        };
        match crate::dap::DapClient::start(&args.program, &args.args, &args.env, &dir) {
            Ok((client, rx)) => {
                self.dap_sessions.insert(args.id, client);
                self.dap_pump(args.id, rx, args.on_message, args.on_exit, cx);
            }
            Err(e) => self.dap_failed(&format!("dap-start: {e}"), &args.on_exit, cx),
        }
    }

    /// Start a TCP DAP session (adapters that serve on a socket, e.g. delve).
    /// The connect itself happens off the UI thread (see
    /// [`DapClient::connect`](crate::dap::DapClient::connect)); messages sent
    /// before it completes are queued, and a failed connect surfaces through
    /// the pump's exit path like an adapter crash.
    pub(super) fn dap_connect(
        &mut self,
        id: u64,
        host: String,
        port: u16,
        on_message: String,
        on_exit: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let (client, rx) = crate::dap::DapClient::connect(&host, port);
        self.dap_sessions.insert(id, client);
        self.dap_pump(id, rx, on_message, on_exit, cx);
    }

    /// A session that never came up: echo why and still fire `on_exit` so the
    /// Scheme state machine resets instead of waiting forever.
    fn dap_failed(&mut self, error: &str, on_exit: &Option<String>, cx: &mut Context<Self>) {
        self.echo(error.to_string(), cx);
        if let Some(cb) = on_exit {
            self.script.dispatch_action(cb, Some("-1"));
        }
    }

    /// Drain a session's incoming messages on the foreground, calling the
    /// Steel `on_message` per message; when the channel closes (adapter died,
    /// was killed, or never connected) reap it OFF the UI thread and call
    /// `on_exit` with the exit code. Also kills the adapter if the app quits
    /// mid-session, so no adapter/debuggee outlives the editor.
    fn dap_pump(
        &mut self,
        id: u64,
        mut rx: futures::channel::mpsc::UnboundedReceiver<serde_json::Value>,
        on_message: String,
        on_exit: Option<String>,
        cx: &mut Context<Self>,
    ) {
        cx.on_app_quit(move |editor, _| {
            // Dropping the client kills its process group / socket.
            editor.dap_sessions.remove(&id);
            async {}
        })
        .detach();
        cx.spawn(async move |this, cx| {
            while let Some(message) = rx.next().await {
                let alive = this
                    .update(cx, |editor, _| {
                        // Publish the active buffer first, so a callback reading
                        // `(current-buffer)` sees where the user actually is
                        // (mirrors `make_process`).
                        editor.publish_point();
                        let text = message.to_string();
                        editor.script.dispatch_action(&on_message, Some(&text));
                    })
                    .is_ok();
                if !alive {
                    return;
                }
            }
            let Ok(Some(client)) = this.update(cx, |editor, _| editor.dap_sessions.remove(&id))
            else {
                return;
            };
            // Reaping can wait on a lingering adapter — never on the UI thread.
            let (code, error) = cx
                .background_executor()
                .spawn(async move { client.finish() })
                .await;
            let _ = this.update(cx, |editor, cx| {
                if let Some(why) = error {
                    editor.echo(format!("dap: {why}"), cx);
                }
                if let Some(cb) = &on_exit {
                    editor.script.dispatch_action(cb, Some(&code.to_string()));
                }
            });
        })
        .detach();
    }

    /// Frame one complete DAP JSON message to a live session's adapter. A
    /// send to a session that already died (or failed to start) is dropped
    /// silently: its `on_exit` already reports why, and an echo here would
    /// overwrite that more useful message.
    pub(super) fn dap_send(&mut self, id: u64, message: &str) {
        if let Some(client) = self.dap_sessions.get(&id) {
            client.send(message);
        }
    }

    /// Kill a live session's adapter; the pump then reports its exit.
    pub(super) fn dap_kill(&mut self, id: u64) {
        if let Some(client) = self.dap_sessions.get_mut(&id) {
            client.kill();
        }
    }

    /// Open a streaming HTTP POST (`make-http-stream`): a worker thread sends the
    /// request with `ureq` (blocking, rustls TLS), then reads the response body
    /// line by line and forwards each line to the foreground, which calls the
    /// Steel `on_line` fn with it. `on_done` runs with the stream id when the body
    /// closes. The HTTP analogue of [`make_process`](Self::make_process) — same
    /// channel + `cx.spawn` drain, so the callbacks' editor commands flow through
    /// the normal outcome drain (`append-to-buffer`, etc.). The UI never blocks.
    ///
    /// A non-2xx status surfaces its body as one `error: HTTP <code>: <body>`
    /// line; a transport failure as `error: <e>`. Cancellation flips the shared
    /// flag (see [`http_stream_cancel`](Self::http_stream_cancel)); the reader
    /// checks it between lines and drops the connection.
    pub(super) fn http_stream(
        &mut self,
        args: crate::script::HttpStreamArgs,
        cx: &mut Context<Self>,
    ) {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let crate::script::HttpStreamArgs {
            id,
            url,
            headers,
            body,
            on_line,
            on_done,
        } = args;

        let cancel = Arc::new(AtomicBool::new(false));
        self.http_streams.insert(id, cancel.clone());

        let (tx, mut rx) = futures::channel::mpsc::unbounded::<ProcMsg>();
        std::thread::spawn(move || {
            http_stream_blocking(&url, &headers, &body, |line| {
                !cancel.load(Ordering::Relaxed) && tx.unbounded_send(ProcMsg::Line(line)).is_ok()
            });
            let _ = tx.unbounded_send(ProcMsg::Exit);
        });

        cx.spawn(async move |this, cx| {
            while let Some(msg) = rx.next().await {
                let alive = this
                    .update(cx, |editor, _| match &msg {
                        ProcMsg::Line(line) => {
                            // Publish the active buffer so a callback reading
                            // `(current-buffer)` sees the real focus (see
                            // `make_process`); the built-in agent streams here.
                            editor.publish_point();
                            editor.script.dispatch_action(&on_line, Some(line))
                        }
                        ProcMsg::Exit => {
                            editor.http_streams.remove(&id);
                            if !on_done.is_empty() {
                                editor
                                    .script
                                    .dispatch_action(&on_done, Some(&id.to_string()));
                            }
                        }
                    })
                    .is_ok();
                if !alive || matches!(msg, ProcMsg::Exit) {
                    break;
                }
            }
        })
        .detach();
    }

    /// Cancel an in-flight HTTP stream (`http-stream-cancel`): flip its flag so the
    /// reader thread stops forwarding lines and drops the connection on its next
    /// loop. The `on_done` callback still fires when the body closes.
    pub(super) fn http_stream_cancel(&mut self, id: u64) {
        if let Some(flag) = self.http_streams.get(&id) {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// One-shot HTTP request (`http-request`). Runs the blocking `ureq` call on a
    /// worker thread (same model as [`http_stream`](Self::http_stream)), then
    /// delivers the whole response to `on_response` as one `"<status>\n<body>"`
    /// string — status `0` for a transport failure. Any verb; empty `body` sends
    /// no body.
    pub(super) fn http_request(
        &mut self,
        args: crate::script::HttpRequestArgs,
        cx: &mut Context<Self>,
    ) {
        let crate::script::HttpRequestArgs {
            method,
            url,
            headers,
            body,
            on_response,
            full,
        } = args;

        let (tx, mut rx) = futures::channel::mpsc::unbounded::<String>();
        std::thread::spawn(move || {
            let payload = http_request_blocking(&method, &url, &headers, body, full);
            let _ = tx.unbounded_send(payload);
        });

        cx.spawn(async move |this, cx| {
            if let Some(payload) = rx.next().await {
                let _ = this.update(cx, |editor, _| {
                    editor.script.dispatch_action(&on_response, Some(&payload));
                });
            }
        })
        .detach();
    }

    /// Start a local HTTP server (`http-serve`). The listener is already bound (in
    /// the registry, so the port is known synchronously); here we move it to a
    /// worker thread that accepts connections, writes one fixed response, and
    /// forwards each request target to `on_request`. A stop flag (checked each
    /// poll) lets `http-server-stop` unwind the blocking accept.
    pub(super) fn http_serve(
        &mut self,
        args: crate::script::HttpServeArgs,
        cx: &mut Context<Self>,
    ) {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let crate::script::HttpServeArgs {
            listener,
            port,
            content_type,
            body,
            on_request,
        } = args;

        let Some(listener) = listener.lock().unwrap().take() else {
            return;
        };
        // Non-blocking accept + short sleep so the stop flag is honored promptly
        // without a second wakeup mechanism.
        if listener.set_nonblocking(true).is_err() {
            return;
        }

        let stop = Arc::new(AtomicBool::new(false));
        self.http_servers.insert(port, stop.clone());

        let (tx, mut rx) = futures::channel::mpsc::unbounded::<ProcMsg>();
        std::thread::spawn(move || {
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                content_type,
                body.len(),
                body,
            );
            loop {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                match listener.accept() {
                    Ok((mut conn, _)) => {
                        // Read the request line ("GET /path?query HTTP/1.1") to
                        // pull out the target; ignore the rest of the request.
                        conn.set_nonblocking(false).ok();
                        let target = {
                            let mut reader = BufReader::new(&conn);
                            let mut line = String::new();
                            let _ = reader.read_line(&mut line);
                            line.split_whitespace().nth(1).unwrap_or("").to_string()
                        };
                        let _ = conn.write_all(response.as_bytes());
                        let _ = conn.flush();
                        if tx.unbounded_send(ProcMsg::Line(target)).is_err() {
                            break;
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                    Err(_) => break,
                }
            }
            let _ = tx.unbounded_send(ProcMsg::Exit);
        });

        cx.spawn(async move |this, cx| {
            while let Some(msg) = rx.next().await {
                let alive = this
                    .update(cx, |editor, _| match &msg {
                        ProcMsg::Line(target) => {
                            editor.script.dispatch_action(&on_request, Some(target));
                        }
                        ProcMsg::Exit => {
                            editor.http_servers.remove(&port);
                        }
                    })
                    .is_ok();
                if !alive || matches!(msg, ProcMsg::Exit) {
                    break;
                }
            }
        })
        .detach();
    }

    /// Stop a local HTTP server (`http-server-stop`): flip its flag so the accept
    /// loop exits on its next poll and drops the socket.
    pub(super) fn http_server_stop(&mut self, port: u16) {
        if let Some(flag) = self.http_servers.get(&port) {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    // --- async sockets + WebSockets ---------------------------------------

    /// Open a long-lived async socket (`socket-connect-async`). A worker thread
    /// connects (TLS when `tls`), then loops: drain outbound commands from the
    /// channel, then read with a short timeout, forwarding each inbound chunk —
    /// hex-encoded — to `on_data`. On peer close or error it signals `on_close`.
    /// The socket analogue of [`make_process`](Self::make_process): same channel
    /// + `cx.spawn` drain, so the callbacks' editor commands flow normally.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn socket_connect_async(
        &mut self,
        id: u64,
        host: String,
        port: u16,
        tls: bool,
        on_data: String,
        on_close: String,
        cx: &mut Context<Self>,
    ) {
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<SockCmd>();
        self.async_sockets.insert(id, cmd_tx);

        let (tx, rx) = futures::channel::mpsc::unbounded::<ProcMsg>();
        std::thread::spawn(move || {
            // Connect (poll timeout short so the command channel stays responsive).
            let tcp = match crate::script::tcp_connect_for_async(&host, port) {
                Ok(t) => t,
                Err(e) => {
                    crate::script::tls_set_error(e);
                    let _ = tx.unbounded_send(ProcMsg::Exit);
                    return;
                }
            };
            let mut sock = if tls {
                match crate::script::tls_wrap(tcp, &host) {
                    Ok(mut s) => {
                        // Drive the TLS handshake to completion under the
                        // connect helper's default (30s) read timeout. Without
                        // this the handshake would run lazily on the first
                        // send/recv under the 150ms poll timeout below and a
                        // server slower than that would fail spuriously.
                        if let Err(e) = s.conn.complete_io(&mut s.sock) {
                            crate::script::tls_set_error(format!("tls handshake: {e}"));
                            let _ = tx.unbounded_send(ProcMsg::Exit);
                            return;
                        }
                        crate::script::Sock::Tls(Box::new(s))
                    }
                    Err(e) => {
                        crate::script::tls_set_error(e);
                        let _ = tx.unbounded_send(ProcMsg::Exit);
                        return;
                    }
                }
            } else {
                crate::script::Sock::Plain(tcp)
            };
            // Now drop to a short read timeout so the poll loop can service the
            // command channel between inbound chunks.
            let _ = sock
                .tcp()
                .set_read_timeout(Some(std::time::Duration::from_millis(150)));
            let mut buf = [0u8; 16384];
            'outer: loop {
                // Drain EVERY queued command before the (up-to-150ms) read, so
                // a burst of sends or a close-after-sends isn't paced one per
                // poll tick.
                loop {
                    match cmd_rx.try_recv() {
                        Ok(SockCmd::Send(bytes)) => {
                            use std::io::Write;
                            if sock.write_all(&bytes).is_err() {
                                break 'outer;
                            }
                            let _ = sock.flush();
                        }
                        Ok(SockCmd::Close) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            break 'outer;
                        }
                        Err(std::sync::mpsc::TryRecvError::Empty) => break, // drained
                    }
                }
                use std::io::Read;
                match sock.read(&mut buf) {
                    Ok(0) => break, // peer closed
                    Ok(n) => {
                        if tx
                            .unbounded_send(ProcMsg::Line(hex::encode(&buf[..n])))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(ref e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            || e.kind() == std::io::ErrorKind::TimedOut => {}
                    Err(_) => break,
                }
            }
            let _ = tx.unbounded_send(ProcMsg::Exit);
        });

        self.drain_socket_worker(id, on_data, on_close, rx, cx);
    }

    /// Shared worker→foreground drain for async sockets and WebSockets: `Line`
    /// payloads go to `on_data`, `Exit` calls `on_close` with the id and drops
    /// the handle. (`rx` is the worker's channel; identical shape to
    /// `make_process`'s drain.)
    fn drain_socket_worker(
        &mut self,
        id: u64,
        on_data: String,
        on_close: String,
        mut rx: futures::channel::mpsc::UnboundedReceiver<ProcMsg>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            while let Some(msg) = rx.next().await {
                let alive = this
                    .update(cx, |editor, _| match &msg {
                        ProcMsg::Line(data) => {
                            editor.publish_point();
                            editor.script.dispatch_action(&on_data, Some(data));
                        }
                        ProcMsg::Exit => {
                            editor.async_sockets.remove(&id);
                            editor
                                .script
                                .dispatch_action(&on_close, Some(&id.to_string()));
                        }
                    })
                    .is_ok();
                if !alive || matches!(msg, ProcMsg::Exit) {
                    break;
                }
            }
        })
        .detach();
    }

    /// Queue bytes (`socket-async-send`) for async socket `id`; the worker sends
    /// them on its next poll. `hex` decodes to the raw bytes.
    pub(super) fn socket_async_send(&mut self, id: u64, hex: &str) {
        if let Some(tx) = self.async_sockets.get(&id)
            && let Ok(bytes) = hex::decode(hex.trim())
        {
            let _ = tx.send(SockCmd::Send(bytes));
        }
    }

    /// Close async socket `id` (`socket-async-close`): tell its worker to stop.
    pub(super) fn socket_async_close(&mut self, id: u64) {
        if let Some(tx) = self.async_sockets.get(&id) {
            let _ = tx.send(SockCmd::Close);
        }
    }

    /// Open a WebSocket (`ws-connect`). A worker connects the TCP/TLS stream,
    /// performs the tungstenite handshake over our own `Sock` (so no second TLS
    /// stack), then polls: drain outbound text/close commands, then read one
    /// message, forwarding text (and the text of binary) frames to `on_message`.
    pub(super) fn ws_connect(
        &mut self,
        id: u64,
        url: String,
        on_message: String,
        on_close: String,
        cx: &mut Context<Self>,
    ) {
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<SockCmd>();
        self.async_sockets.insert(id, cmd_tx);

        let (tx, rx) = futures::channel::mpsc::unbounded::<ProcMsg>();
        std::thread::spawn(move || {
            match ws_run(&url, &cmd_rx, &tx) {
                Ok(()) => {}
                Err(e) => crate::script::tls_set_error(e),
            }
            let _ = tx.unbounded_send(ProcMsg::Exit);
        });

        self.drain_socket_worker(id, on_message, on_close, rx, cx);
    }

    /// Queue a text message (`ws-send`) for WebSocket `id`.
    pub(super) fn ws_send(&mut self, id: u64, text: &str) {
        if let Some(tx) = self.async_sockets.get(&id) {
            let _ = tx.send(SockCmd::Send(text.as_bytes().to_vec()));
        }
    }

    /// Close WebSocket `id` (`ws-close`).
    pub(super) fn ws_close(&mut self, id: u64) {
        if let Some(tx) = self.async_sockets.get(&id) {
            let _ = tx.send(SockCmd::Close);
        }
    }

    // --- Embedded terminal ------------------------------------------------

    /// Spawn a terminal (`make-terminal`): start the PTY/shell and show it in a
    /// read-only `term-mode` buffer named `name`. If a terminal buffer of that
    /// name already exists, just switch to it (reuse the running shell).
    pub(super) fn make_terminal(
        &mut self,
        spec: crate::script::MakeTerminalArgs,
        cx: &mut Context<Self>,
    ) {
        let crate::script::MakeTerminalArgs {
            id,
            name,
            program,
            args,
            directory,
            env,
        } = spec;
        if self.terminals.values().any(|t| t.buffer_name == name) {
            self.switch_to_buffer(&name, cx);
            return;
        }

        // Initial grid size from the current cell metrics; the element re-fits it
        // on the first frame, so rough numbers are fine.
        let (cols, rows) = self.terminal_fit_size();
        let cell_w = (f32::from(self.char_width).max(1.0)) as u16;
        let cell_h = (f32::from(self.line_height).max(1.0)) as u16;
        let program = if program.trim().is_empty() {
            None
        } else {
            Some((program, args))
        };
        // Prefer an explicit directory (term.scm passes the current project's
        // root so terminals follow the title-bar project); otherwise fall back
        // to the active buffer's directory, then the process cwd.
        let cwd = if directory.trim().is_empty() {
            self.active
                .path
                .as_deref()
                .and_then(Path::parent)
                .map(Path::to_path_buf)
        } else {
            Some(std::path::PathBuf::from(directory))
        };

        match Terminal::new(
            id,
            name.clone(),
            cols,
            rows,
            cell_w,
            cell_h,
            program,
            cwd,
            &env,
            self.term_events_tx.clone(),
        ) {
            Ok(terminal) => {
                self.terminals.insert(id, terminal);
                // A read-only special buffer drives input routing and the mode
                // line; its rope text is unused (the grid is rendered directly).
                self.render_buffer(name, "term".to_string(), String::new(), Vec::new(), cx);
            }
            Err(e) => self.echo(format!("make-terminal: {e}"), cx),
        }
    }

    /// A starting `(columns, rows)` for a new terminal from the live viewport,
    /// defaulting to 80×24 before the first frame has measured anything.
    pub(super) fn terminal_fit_size(&self) -> (usize, usize) {
        let cw = f32::from(self.char_width);
        let lh = f32::from(self.line_height);
        let cols = if cw > 0.0 {
            (f32::from(self.viewport_width) / cw) as usize
        } else {
            0
        };
        let rows = if lh > 0.0 {
            (f32::from(self.viewport_height) / lh) as usize
        } else {
            0
        };
        // Before the first frame the viewport is unmeasured; fall back to 80×24.
        (
            if cols == 0 { 80 } else { cols },
            if rows == 0 { 24 } else { rows },
        )
    }

    /// The terminal shown in pane `pane`, if its buffer is a terminal buffer.
    pub fn pane_terminal_id(&self, pane: usize) -> Option<u64> {
        let name = self.pane(pane).map(|p| p.buffer.as_str())?;
        self.terminals
            .values()
            .find(|t| t.buffer_name == name)
            .map(|t| t.id)
    }

    /// The terminal in the selected pane (the live one), if any. Includes a
    /// terminal whose shell has exited — its scrollback is still readable, so
    /// copy/scroll keep working on the leftover buffer.
    pub(super) fn active_terminal_id(&self) -> Option<u64> {
        self.terminals
            .values()
            .find(|t| t.buffer_name == self.active.name)
            .map(|t| t.id)
    }

    /// The active terminal only while its shell is still running. Input paths
    /// (key grab, action keys, paste) use this so a dead terminal releases the
    /// keyboard and normal editor dispatch (M-x, C-x k, …) resumes.
    pub(super) fn active_running_terminal_id(&self) -> Option<u64> {
        self.active_terminal_id()
            .filter(|tid| !self.terminals.get(tid).is_some_and(|t| t.exited))
    }

    /// Send already-encoded key bytes to terminal `id`, snapping the view back to
    /// the live region first (typing scrolls out of scrollback).
    pub(super) fn terminal_input(&mut self, id: u64, bytes: Vec<u8>, cx: &mut Context<Self>) {
        if let Some(t) = self.terminals.get(&id) {
            t.selection_clear(); // typing dismisses any mouse selection
            t.scroll_to_bottom();
            t.input(bytes);
            cx.notify();
        }
    }

    /// If a terminal is active, encode the named OS-special key and send it to
    /// the PTY, returning `true`. This is the *only* dispatch path for keys bound
    /// to GPUI actions (Backspace/Enter/Tab/arrows/…): gpui stops propagation
    /// after running an action, so `on_key_down` never sees them — the `act_*`
    /// handlers call this instead. Plain text and Ctrl/Alt chords still go through
    /// `on_key_down`'s own terminal branch.
    pub(super) fn terminal_action_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let Some(tid) = self.active_running_terminal_id() else {
            return false;
        };
        let bytes: Vec<u8> = match key {
            "enter" => vec![b'\r'],
            "tab" => vec![b'\t'],
            "backtab" => b"\x1b[Z".to_vec(),
            "backspace" => vec![0x7f],
            "delete" => b"\x1b[3~".to_vec(),
            "up" => b"\x1b[A".to_vec(),
            "down" => b"\x1b[B".to_vec(),
            "right" => b"\x1b[C".to_vec(),
            "left" => b"\x1b[D".to_vec(),
            "home" => b"\x1b[H".to_vec(),
            "end" => b"\x1b[F".to_vec(),
            _ => return false,
        };
        self.terminal_input(tid, bytes, cx);
        true
    }

    /// Scroll terminal `id`'s scrollback by `delta` lines (positive = older).
    pub(super) fn terminal_scroll(&mut self, id: u64, delta: i32, cx: &mut Context<Self>) {
        if let Some(t) = self.terminals.get(&id) {
            t.scroll(delta);
            cx.notify();
        }
    }

    /// Record a terminal pane's painted geometry (called by its element each
    /// frame) so mouse positions can be mapped to grid cells.
    pub fn set_terminal_geometry(
        &mut self,
        id: u64,
        origin: Point<Pixels>,
        cell_w: Pixels,
        cell_h: Pixels,
        cols: usize,
        rows: usize,
    ) {
        self.terminal_geom.insert(
            id,
            TermGeom {
                origin,
                cell_w,
                cell_h,
                cols,
                rows,
            },
        );
    }

    /// Map a window position to a `(row, col, right_half)` cell in terminal `id`,
    /// clamped to the grid. `None` if the geometry isn't known yet.
    pub(super) fn terminal_cell_at(
        &self,
        id: u64,
        pos: Point<Pixels>,
    ) -> Option<(usize, usize, bool)> {
        let g = self.terminal_geom.get(&id)?;
        let cw = f32::from(g.cell_w).max(1.0);
        let ch = f32::from(g.cell_h).max(1.0);
        let relx = (f32::from(pos.x) - f32::from(g.origin.x)) / cw;
        let rely = (f32::from(pos.y) - f32::from(g.origin.y)) / ch;
        let col = (relx.floor().max(0.0) as usize).min(g.cols.saturating_sub(1));
        let row = (rely.floor().max(0.0) as usize).min(g.rows.saturating_sub(1));
        let right = (relx - relx.floor()) >= 0.5;
        Some((row, col, right))
    }

    /// Begin a mouse text selection in the terminal under `pos` (if any). Returns
    /// whether a terminal selection was started.
    pub(super) fn terminal_selection_begin(
        &mut self,
        pos: Point<Pixels>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(pane) = self.pane_at_position(pos) else {
            return false;
        };
        let Some(tid) = self.pane_terminal_id(pane) else {
            return false;
        };
        if pane != self.selected_pane {
            self.select_pane(pane, cx);
        }
        if let Some((row, col, right)) = self.terminal_cell_at(tid, pos)
            && let Some(t) = self.terminals.get(&tid)
        {
            t.selection_start(row, col, right);
            self.terminal_drag = Some(tid);
            cx.notify();
        }
        true
    }

    /// Extend the active terminal selection to `pos` while dragging.
    pub(super) fn terminal_selection_drag(&mut self, pos: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(tid) = self.terminal_drag else {
            return;
        };
        if let Some((row, col, right)) = self.terminal_cell_at(tid, pos)
            && let Some(t) = self.terminals.get(&tid)
        {
            t.selection_update(row, col, right);
            cx.notify();
        }
    }

    /// Copy the active terminal's selection to the system clipboard. Returns
    /// whether a terminal was active (so the caller skips the buffer copy path).
    pub(super) fn terminal_copy(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(tid) = self.active_terminal_id() else {
            return false;
        };
        match self.terminals.get(&tid).and_then(|t| t.selection_text()) {
            Some(text) => {
                self.set_clipboard(&text, cx);
                if let Some(t) = self.terminals.get(&tid) {
                    t.selection_clear();
                }
                self.echo("Copied", cx);
                cx.notify();
            }
            None => self.echo("No selection", cx),
        }
        true
    }

    /// Paste the system clipboard into the active terminal. Returns whether a
    /// terminal was active.
    pub(super) fn terminal_paste(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(tid) = self.active_running_terminal_id() else {
            return false;
        };
        if let Some(text) = cx.read_from_clipboard().and_then(|i| i.text())
            && !text.is_empty()
        {
            self.terminal_input(tid, text.into_bytes(), cx);
        }
        true
    }

    /// Re-fit terminal `id` to a measured cell grid (called by the element each
    /// frame); a no-op when the size is unchanged.
    pub fn terminal_resize_fit(
        &mut self,
        id: u64,
        cols: usize,
        rows: usize,
        cell_w: u16,
        cell_h: u16,
    ) {
        if let Some(t) = self.terminals.get_mut(&id) {
            t.resize(cols, rows, cell_w, cell_h);
        }
    }

    /// A render snapshot of terminal `id`'s visible grid, with theme defaults for
    /// default-colored cells.
    pub fn terminal_snapshot(
        &self,
        id: u64,
        default_fg: u32,
        default_bg: u32,
    ) -> Option<crate::terminal::TermSnapshot> {
        self.terminals
            .get(&id)
            .map(|t| t.snapshot(default_fg, default_bg))
    }

    /// Apply a terminal event from its background event loop.
    pub(super) fn on_terminal_event(&mut self, id: u64, event: TermEvent, cx: &mut Context<Self>) {
        match event {
            TermEvent::Wakeup => {}
            TermEvent::Title(title) => {
                if let Some(t) = self.terminals.get_mut(&id) {
                    t.title = title;
                }
            }
            TermEvent::Exit => {
                // alacritty reports a finished child twice (`ChildExit`, then
                // `Exit`); act on the first only, or `terminal-exit-hook` runs
                // twice and its second window dance targets a pane the first
                // already closed.
                if let Some(t) = self.terminals.get_mut(&id)
                    && !t.exited
                {
                    t.exited = true;
                    let name = t.buffer_name.clone();
                    self.echo(format!("Terminal {name} exited"), cx);
                    // Let Scheme react (close the pane, restart, …) — called
                    // with the terminal's buffer name.
                    self.run_hook_arg("terminal-exit-hook", &name);
                }
            }
        }
        cx.notify();
    }
}

/// A command sent from the engine thread to an async socket / WebSocket worker.
pub(crate) enum SockCmd {
    /// Bytes (raw for a socket; a text message's UTF-8 for a WebSocket) to send.
    Send(Vec<u8>),
    /// Stop the worker and drop the connection.
    Close,
}

/// Drive a WebSocket worker: connect the TCP/TLS stream, hand it to tungstenite
/// for the handshake (over our own `Sock`, so no second TLS stack), then poll —
/// draining outbound text/close commands, then reading one frame with a short
/// timeout and forwarding text (and binary-as-text) frames to the foreground as
/// `ProcMsg::Line`. Returns when the socket closes or errors.
fn ws_run(
    url: &str,
    cmd_rx: &std::sync::mpsc::Receiver<SockCmd>,
    tx: &futures::channel::mpsc::UnboundedSender<ProcMsg>,
) -> Result<(), String> {
    use tungstenite::Message;
    let parsed = url
        .parse::<tungstenite::http::Uri>()
        .map_err(|e| format!("bad ws url: {e}"))?;
    let host = parsed.host().ok_or("ws url has no host")?.to_string();
    let secure = matches!(parsed.scheme_str(), Some("wss") | None);
    let port = parsed.port_u16().unwrap_or(if secure { 443 } else { 80 });

    let tcp = crate::script::tcp_connect_for_async(&host, port)?;
    let sock = if secure {
        crate::script::Sock::Tls(Box::new(crate::script::tls_wrap(tcp, &host)?))
    } else {
        crate::script::Sock::Plain(tcp)
    };
    // The handshake runs under the connect helper's default (30s) read
    // timeout — a 150ms cap here would make `tungstenite::client` fail the
    // upgrade against any server slower than 150ms RTT (its blocking reads
    // would surface as WouldBlock).
    let (mut ws, _resp) =
        tungstenite::client(url, sock).map_err(|e| format!("ws handshake: {e}"))?;
    // NOW drop to a short read timeout so the poll loop can service the
    // command channel between inbound frames.
    let _ = ws
        .get_ref()
        .tcp()
        .set_read_timeout(Some(std::time::Duration::from_millis(150)));

    'outer: loop {
        // Drain every queued command before the (up-to-150ms) read.
        loop {
            match cmd_rx.try_recv() {
                Ok(SockCmd::Send(bytes)) => {
                    let text = String::from_utf8_lossy(&bytes).into_owned();
                    if ws.send(Message::Text(text.into())).is_err() {
                        break 'outer;
                    }
                }
                Ok(SockCmd::Close) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    let _ = ws.close(None);
                    break 'outer;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break, // drained
            }
        }
        // One inbound frame (read timeout surfaces as a WouldBlock/IO error).
        match ws.read() {
            Ok(Message::Text(t)) => {
                if tx.unbounded_send(ProcMsg::Line(t.to_string())).is_err() {
                    break;
                }
            }
            Ok(Message::Binary(b)) => {
                if tx
                    .unbounded_send(ProcMsg::Line(String::from_utf8_lossy(&b).into_owned()))
                    .is_err()
                {
                    break;
                }
            }
            Ok(Message::Ping(p)) => {
                let _ = ws.send(Message::Pong(p));
            }
            Ok(Message::Close(_)) => break,
            Ok(_) => {}
            Err(tungstenite::Error::Io(ref e))
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => break,
        }
    }
    Ok(())
}

/// The blocking half of [`Editor::http_stream`]: POST `body` and hand each
/// response line to `emit` until it returns `false` (cancelled, or the receiver
/// is gone) or the body ends. A 4xx/5xx status becomes one
/// `error: HTTP <code>: <body>` line — relaying the API's own error JSON (e.g. a
/// 401 from a bad key) into the transcript — and a transport failure one
/// `error: <e>` line.
fn http_stream_blocking(
    url: &str,
    headers: &[(String, String)],
    body: &str,
    mut emit: impl FnMut(String) -> bool,
) {
    let mut req = http_agent().post(url);
    for (k, v) in headers {
        req = req.header(k, v);
    }
    match req.send(body) {
        // Same threshold as ureq 2's `Error::Status`: only 4xx/5xx are errors.
        Ok(resp) if resp.status().as_u16() >= 400 => {
            let code = resp.status().as_u16();
            let detail = resp.into_body().read_to_string().unwrap_or_default();
            emit(format!("error: HTTP {code}: {detail}"));
        }
        Ok(resp) => {
            for l in lossy_lines(BufReader::new(resp.into_body().into_reader())) {
                if !emit(l) {
                    break;
                }
            }
        }
        Err(e) => {
            emit(format!("error: {e}"));
        }
    }
}

/// The blocking half of [`Editor::http_request`]: send one request and render
/// the response as the `on_response` payload (see [`HttpRequestArgs`]).
///
/// [`HttpRequestArgs`]: crate::script::HttpRequestArgs
fn http_request_blocking(
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: String,
    full: bool,
) -> String {
    let agent = http_agent();
    let mut req = ureq::http::Request::builder().method(method).uri(url);
    for (k, v) in headers {
        req = req.header(k, v);
    }
    let result = if body.is_empty() {
        req.body(())
            .map_err(ureq::Error::from)
            .and_then(|r| agent.run(r))
    } else {
        req.body(body)
            .map_err(ureq::Error::from)
            .and_then(|r| agent.run(r))
    };
    // Render a response (any status — non-2xx relays the server's own
    // status + error body to Scheme) either as the plain
    // "<status>\n<text>" payload or the rich "<status>\n<headers>\n\n
    // <hex-body>" one, per `full`.
    let render = |resp: ureq::http::Response<ureq::Body>| -> String {
        let status = resp.status().as_u16();
        if full {
            let mut head = String::new();
            for (name, v) in resp.headers() {
                head.push_str(&format!(
                    "{name}: {}\n",
                    String::from_utf8_lossy(v.as_bytes())
                ));
            }
            let mut bytes = Vec::new();
            let _ = std::io::Read::read_to_end(&mut resp.into_body().into_reader(), &mut bytes);
            format!("{status}\n{head}\n{}", hex::encode(&bytes))
        } else {
            format!(
                "{status}\n{}",
                resp.into_body().read_to_string().unwrap_or_default()
            )
        }
    };
    match result {
        Ok(resp) => render(resp),
        // Transport failure (DNS, TLS, connect): status 0, error as body.
        // In full mode the empty header block still precedes the body.
        Err(e) => {
            if full {
                format!("0\n\n{}", hex::encode(e.to_string().as_bytes()))
            } else {
                format!("0\n{e}")
            }
        }
    }
}

/// The agent behind `http-request` / `make-http-stream`. No read timeout: a
/// model stream can pause between tokens; a connect timeout still bounds the
/// initial handshake. Non-2xx statuses come back as ordinary responses (not
/// `Err`) so callers can relay the server's own error body, and any verb
/// (`PROPFIND`, …) is accepted.
fn http_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(std::time::Duration::from_secs(15)))
        .http_status_as_error(false)
        .allow_non_standard_methods(true)
        .build()
        .new_agent()
}

/// The lines of `reader`, like [`BufRead::lines`] (a trailing `\n` or `\r\n`
/// stripped) but decoding each line lossily: a process or HTTP stream that
/// emits one non-UTF-8 line (a Latin-1 filename, a binary blob) keeps flowing
/// instead of `lines()` erroring and the reader dropping the pipe. Ends at EOF
/// or on a real I/O error.
fn lossy_lines<R: BufRead>(mut reader: R) -> impl Iterator<Item = String> {
    let mut buf = Vec::new();
    std::iter::from_fn(move || {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => None,
            Ok(_) => {
                if buf.last() == Some(&b'\n') {
                    buf.pop();
                    if buf.last() == Some(&b'\r') {
                        buf.pop();
                    }
                }
                Some(String::from_utf8_lossy(&buf).into_owned())
            }
        }
    })
}

#[cfg(test)]
mod lossy_lines_tests {
    use super::lossy_lines;

    #[test]
    fn non_utf8_line_does_not_end_the_stream() {
        let input: &[u8] = b"one\r\nbad \xff\xfe byte\nthree";
        let lines: Vec<String> = lossy_lines(input).collect();
        assert_eq!(lines, vec!["one", "bad \u{fffd}\u{fffd} byte", "three"]);
    }
}

#[cfg(all(test, unix))]
mod process_tests {
    use super::*;

    /// Is any process left in group `pgid`?
    #[cfg(not(target_os = "linux"))]
    fn group_alive(pgid: i32) -> bool {
        unsafe { libc::kill(-pgid, 0) == 0 }
    }

    /// Is any *live* process left in group `pgid`? Zombies don't count: a
    /// killed, orphaned child is reparented to PID 1, and in a CI container
    /// PID 1 often never reaps, so the zombie keeps `kill(-pgid, 0)` succeeding.
    #[cfg(target_os = "linux")]
    fn group_alive(pgid: i32) -> bool {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return unsafe { libc::kill(-pgid, 0) == 0 };
        };
        entries.flatten().any(|e| {
            let Ok(stat) = std::fs::read_to_string(e.path().join("stat")) else {
                return false;
            };
            // `pid (comm) state ppid pgrp …` — comm may contain spaces/parens.
            let Some(rest) = stat.rfind(')').map(|i| &stat[i + 1..]) else {
                return false;
            };
            let mut fields = rest.split_whitespace();
            let state = fields.next();
            let pgrp = fields.nth(1).and_then(|f| f.parse::<i32>().ok());
            pgrp == Some(pgid) && state != Some("Z")
        })
    }

    fn wait_until(mut done: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if done() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    /// Dropping a handle (buffer killed / editor gone) takes the whole process
    /// group down — the wrapper shell *and* its background child.
    #[test]
    fn dropping_handle_kills_process_group() {
        use std::os::unix::process::CommandExt;
        let child = Command::new("/bin/sh")
            .args(["-c", "sleep 30 & sleep 30"])
            .process_group(0)
            .spawn()
            .unwrap();
        let pgid = child.id() as i32;
        let child = Arc::new(Mutex::new(ProcessGroup::new(child)));
        let handle = ProcessHandle {
            child: child.clone(),
            stdin: None,
            buffer: Some("*shell*".into()),
        };
        assert!(!handle.exited());
        drop(handle);
        // The leader exits (we reap it, as the worker thread would)…
        assert!(wait_until(|| matches!(
            lock_child(&child).try_wait(),
            Ok(Some(_))
        )));
        // …and so does the backgrounded `sleep` in its group.
        assert!(wait_until(|| !group_alive(pgid)));
        // Signalling an already-reaped child is a no-op, not a stray kill.
        assert!(!signal_group(&child, Signal::Kill));
    }
}

#[cfg(test)]
mod http_tests {
    use super::{http_request_blocking, http_stream_blocking};
    use std::io::{BufRead, BufReader, Read, Write};

    /// What the one-shot server saw: method, request target, body.
    type Seen = (String, String, String);

    /// Serve exactly one request on a fresh port with the raw `response`, and
    /// hand back what the client sent. Returns the base URL and a join handle.
    fn serve_once(response: &'static str) -> (String, std::thread::JoinHandle<Seen>) {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let handle = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(conn.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let mut parts = line.split_whitespace();
            let method = parts.next().unwrap_or("").to_string();
            let target = parts.next().unwrap_or("").to_string();
            let mut len = 0usize;
            loop {
                let mut h = String::new();
                reader.read_line(&mut h).unwrap();
                if h.trim().is_empty() {
                    break;
                }
                if let Some((k, v)) = h.split_once(':')
                    && k.eq_ignore_ascii_case("content-length")
                {
                    len = v.trim().parse().unwrap();
                }
            }
            let mut body = vec![0u8; len];
            reader.read_exact(&mut body).unwrap();
            conn.write_all(response.as_bytes()).unwrap();
            conn.flush().unwrap();
            (method, target, String::from_utf8(body).unwrap())
        });
        (url, handle)
    }

    const OK: &str = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";

    #[test]
    fn non_2xx_relays_status_and_body() {
        let (url, server) = serve_once(
            "HTTP/1.1 401 Unauthorized\r\nContent-Length: 11\r\nConnection: close\r\n\r\nbad api key",
        );
        let got = http_request_blocking("GET", &url, &[], String::new(), false);
        assert_eq!(got, "401\nbad api key");
        server.join().unwrap();
    }

    #[test]
    fn body_is_sent_with_every_verb() {
        // Verbs ureq 3 treats as body-less by default must still carry a body
        // when the caller gives one (ureq 2 sent it; APIs rely on DELETE/GET
        // with a JSON body), and non-standard verbs must be accepted.
        for verb in [
            "GET", "DELETE", "OPTIONS", "POST", "PUT", "PATCH", "PROPFIND",
        ] {
            let (url, server) = serve_once(OK);
            let got = http_request_blocking(
                verb,
                &format!("{url}/x?y=1"),
                &[("Content-Type".into(), "application/json".into())],
                "{\"k\":1}".into(),
                false,
            );
            assert_eq!(got, "200\nok", "{verb}");
            let (method, target, body) = server.join().unwrap();
            assert_eq!(method, verb);
            assert_eq!(target, "/x?y=1", "{verb}");
            assert_eq!(body, "{\"k\":1}", "{verb} body not sent");
        }
    }

    #[test]
    fn empty_body_sends_none() {
        let (url, server) = serve_once(OK);
        assert_eq!(
            http_request_blocking("DELETE", &url, &[], String::new(), false),
            "200\nok"
        );
        assert_eq!(server.join().unwrap().2, "");
    }

    #[test]
    fn full_mode_lists_every_header_and_hex_body() {
        let (url, server) = serve_once(
            "HTTP/1.1 201 Created\r\nSet-Cookie: a=1\r\nSet-Cookie: b=2\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi",
        );
        let got = http_request_blocking("POST", &url, &[], "x".into(), true);
        let (head, body) = got.rsplit_once("\n\n").unwrap();
        assert!(head.starts_with("201\n"), "{got}");
        assert!(head.contains("set-cookie: a=1"), "{got}");
        assert!(head.contains("set-cookie: b=2"), "{got}");
        assert_eq!(body, hex::encode("hi"));
        server.join().unwrap();
    }

    #[test]
    fn transport_failure_is_status_zero() {
        // Bind then drop: nothing is listening on the port.
        let port = std::net::TcpListener::bind(("127.0.0.1", 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let got = http_request_blocking(
            "GET",
            &format!("http://127.0.0.1:{port}/"),
            &[],
            String::new(),
            false,
        );
        assert!(got.starts_with("0\n"), "{got}");
    }

    fn stream(url: &str, stop_after: usize) -> Vec<String> {
        let mut lines = Vec::new();
        http_stream_blocking(url, &[], "{\"q\":1}", |l| {
            lines.push(l);
            lines.len() < stop_after
        });
        lines
    }

    #[test]
    fn stream_posts_body_and_emits_each_line() {
        let (url, server) = serve_once(
            "HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\na\nb\r\nc\n",
        );
        assert_eq!(stream(&url, usize::MAX), ["a", "b", "c"]);
        let (method, _, body) = server.join().unwrap();
        assert_eq!((method.as_str(), body.as_str()), ("POST", "{\"q\":1}"));
    }

    #[test]
    fn stream_stops_when_emit_returns_false() {
        let (url, server) = serve_once(
            "HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\na\nb\nc\n",
        );
        assert_eq!(stream(&url, 1), ["a"]);
        server.join().unwrap();
    }

    #[test]
    fn stream_reports_4xx_5xx_but_not_3xx_as_errors() {
        let (url, server) = serve_once(
            "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 4\r\nConnection: close\r\n\r\nboom",
        );
        assert_eq!(stream(&url, usize::MAX), ["error: HTTP 500: boom"]);
        server.join().unwrap();

        // ureq 2 surfaced only >= 400 as `Error::Status`; a 304 is not an error.
        let (url, server) = serve_once("HTTP/1.1 304 Not Modified\r\nConnection: close\r\n\r\n");
        assert_eq!(stream(&url, usize::MAX), Vec::<String>::new());
        server.join().unwrap();
    }
}
