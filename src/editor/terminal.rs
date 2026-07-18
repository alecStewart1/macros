//! Subprocesses and embedded terminals for [`Editor`], extracted from the editor module for size.
//! These methods extend the same `Editor` type via an additional
//! `impl` block; cross-module entry points are `pub(super)`.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use futures::StreamExt;
use gpui::{Context, Pixels, Point};

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

impl Editor {
    /// Spawn `program` asynchronously (Emacs `make-process`): a worker thread
    /// reads its stdout line by line and forwards each line to the foreground,
    /// which calls the Steel `on_output` fn with it; `on_exit` runs when the
    /// process finishes. The UI never blocks, and the callbacks' commands flow
    /// through the normal outcome drain — so `(append-to-buffer …)` etc. work.
    pub(super) fn make_process(
        &mut self,
        id: u64,
        program: String,
        args: Vec<String>,
        on_output: String,
        on_exit: Option<String>,
        cx: &mut Context<Self>,
    ) {
        // Spawn here (not on the worker) so we can keep the child for stdin/kill.
        // Expand a leading `~/` — `Command::new` does no shell expansion.
        let program = crate::util::expand_tilde(&program);
        let mut cmd = Command::new(&program);
        cmd.args(&args).stdin(Stdio::piped()).stdout(Stdio::piped());
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
        let stdin = child.stdin.take();
        self.processes.insert(id, ProcessHandle { child, stdin });

        let (tx, mut rx) = futures::channel::mpsc::unbounded::<ProcMsg>();
        std::thread::spawn(move || {
            if let Some(out) = stdout {
                for line in BufReader::new(out).lines() {
                    let Ok(l) = line else { break };
                    if tx.unbounded_send(ProcMsg::Line(l)).is_err() {
                        break;
                    }
                }
            }
            // The editor wait()s for the exit code on Exit.
            let _ = tx.unbounded_send(ProcMsg::Exit);
        });
        cx.spawn(async move |this, cx| {
            while let Some(msg) = rx.next().await {
                let alive = this
                    .update(cx, |editor, _| match &msg {
                        ProcMsg::Line(line) => {
                            // Publish the active buffer first, so a callback
                            // reading `(current-buffer)` sees the buffer the user
                            // is actually in — not a stale snapshot. Without this,
                            // async output routed by current-buffer (e.g.
                            // agent-shell streaming) can steal focus to the wrong
                            // buffer. Mirrors `run_script`/`run_hook`.
                            editor.publish_point();
                            editor.script.dispatch_action(&on_output, Some(line))
                        }
                        ProcMsg::Exit => {
                            let code = editor
                                .processes
                                .remove(&id)
                                .and_then(|mut h| h.child.wait().ok())
                                .and_then(|status| status.code())
                                .unwrap_or(-1);
                            if let Some(cb) = &on_exit {
                                editor.script.dispatch_action(cb, Some(&code.to_string()));
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

    /// Write `text` to a live process's stdin (Emacs `process-send-string`).
    pub(super) fn send_to_process(&mut self, id: u64, text: &str, cx: &mut Context<Self>) {
        if let Some(handle) = self.processes.get_mut(&id)
            && let Some(stdin) = handle.stdin.as_mut()
            && let Err(e) = stdin.write_all(text.as_bytes())
        {
            self.echo(format!("process-send-string: {e}"), cx);
        }
    }

    /// Kill a live process; its reader thread then reports exit (Emacs `kill-process`).
    pub(super) fn kill_process(&mut self, id: u64) {
        if let Some(handle) = self.processes.get_mut(&id) {
            // The child got its own process group at spawn — signal the group,
            // so a wrapper shell's children die too (not just the shell).
            #[cfg(unix)]
            unsafe {
                libc::kill(-(handle.child.id() as i32), libc::SIGKILL);
            }
            let _ = handle.child.kill();
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
            // No read timeout: a model stream can pause between tokens. A connect
            // timeout still bounds the initial handshake.
            let agent = ureq::AgentBuilder::new()
                .timeout_connect(std::time::Duration::from_secs(15))
                .build();
            let mut req = agent.post(&url);
            for (k, v) in &headers {
                req = req.set(k, v);
            }
            match req.send_string(&body) {
                Ok(resp) => {
                    for line in BufReader::new(resp.into_reader()).lines() {
                        if cancel.load(Ordering::Relaxed) {
                            break;
                        }
                        let Ok(l) = line else { break };
                        if tx.unbounded_send(ProcMsg::Line(l)).is_err() {
                            break;
                        }
                    }
                }
                // Non-2xx: ureq returns the response so we can relay the API's own
                // error JSON (e.g. a 401 from a bad key) into the transcript.
                Err(ureq::Error::Status(code, resp)) => {
                    let detail = resp.into_string().unwrap_or_default();
                    let _ =
                        tx.unbounded_send(ProcMsg::Line(format!("error: HTTP {code}: {detail}")));
                }
                Err(e) => {
                    let _ = tx.unbounded_send(ProcMsg::Line(format!("error: {e}")));
                }
            }
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
            let agent = ureq::AgentBuilder::new()
                .timeout_connect(std::time::Duration::from_secs(15))
                .build();
            let mut req = agent.request(&method, &url);
            for (k, v) in &headers {
                req = req.set(k, v);
            }
            let result = if body.is_empty() {
                req.call()
            } else {
                req.send_string(&body)
            };
            // Render a response (2xx or the error variant) either as the plain
            // "<status>\n<text>" payload or the rich "<status>\n<headers>\n\n
            // <hex-body>" one, per `full`.
            let render = |status: u16, resp: ureq::Response| -> String {
                if full {
                    let mut head = String::new();
                    for name in resp.headers_names() {
                        if let Some(v) = resp.header(&name) {
                            head.push_str(&format!("{name}: {v}\n"));
                        }
                    }
                    let mut bytes = Vec::new();
                    let _ = std::io::Read::read_to_end(&mut resp.into_reader(), &mut bytes);
                    format!("{status}\n{head}\n{}", hex::encode(&bytes))
                } else {
                    format!("{status}\n{}", resp.into_string().unwrap_or_default())
                }
            };
            let payload = match result {
                Ok(resp) => render(resp.status(), resp),
                // Non-2xx: relay the server's own status + error body to Scheme.
                Err(ureq::Error::Status(code, resp)) => render(code, resp),
                // Transport failure (DNS, TLS, connect): status 0, error as body.
                // In full mode the empty header block still precedes the body.
                Err(e) => {
                    if full {
                        format!("0\n\n{}", hex::encode(e.to_string().as_bytes()))
                    } else {
                        format!("0\n{e}")
                    }
                }
            };
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
        id: u64,
        name: String,
        program: String,
        args: Vec<String>,
        cx: &mut Context<Self>,
    ) {
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
        let cwd = self
            .active
            .path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf);

        match Terminal::new(
            id,
            name.clone(),
            cols,
            rows,
            cell_w,
            cell_h,
            program,
            cwd,
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
                if let Some(t) = self.terminals.get_mut(&id) {
                    t.exited = true;
                    let name = t.buffer_name.clone();
                    self.echo(format!("Terminal {name} exited"), cx);
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
                    if ws.send(Message::Text(text)).is_err() {
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
                if tx.unbounded_send(ProcMsg::Line(t)).is_err() {
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
