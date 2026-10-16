//! HTTP, raw-socket and WebSocket primitives for [`Editor`]: each spawns a
//! worker running one of the blocking clients in [`crate::net`] and pumps its
//! output to Steel callbacks (extracted from the editor module for size).
//! These methods extend the same `Editor` type; cross-module entry points are
//! `pub(super)`.

use std::io::{BufRead, BufReader, Read, Write};

use futures::StreamExt;
use gpui::Context;

use super::*;

/// How long `http-serve` waits for a client's request line before dropping it.
const HTTP_SERVE_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
/// The longest request line `http-serve` reads (a browser's is far shorter).
const HTTP_SERVE_MAX_LINE: u64 = 16 * 1024;

impl Editor {
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
            crate::net::http_stream_blocking(&url, &headers, &body, |line| {
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
            let payload = crate::net::http_request_blocking(&method, &url, &headers, body, full);
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

        let Some(listener) = listener.lock().ok().and_then(|mut l| l.take()) else {
            return;
        };
        // Non-blocking accept + short sleep so the stop flag is honored promptly
        // without a second wakeup mechanism.
        if let Err(e) = listener.set_nonblocking(true) {
            self.echo(format!("http-serve {port}: {e}"), cx);
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
                        // Bounded in time and size: this one thread serves
                        // every connection, so a client that connects and
                        // sends nothing (or never sends a newline) must not
                        // wedge the server or `http-server-stop`.
                        if conn.set_nonblocking(false).is_err()
                            || conn
                                .set_read_timeout(Some(HTTP_SERVE_READ_TIMEOUT))
                                .is_err()
                        {
                            continue;
                        }
                        let target = {
                            let mut reader = BufReader::new(&conn).take(HTTP_SERVE_MAX_LINE);
                            let mut line = String::new();
                            if reader.read_line(&mut line).is_err() || !line.ends_with('\n') {
                                continue;
                            }
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
                    // A client that hung up mid-handshake isn't the server's
                    // failure; keep accepting.
                    Err(ref e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::ConnectionAborted
                                | std::io::ErrorKind::ConnectionReset
                                | std::io::ErrorKind::Interrupted
                        ) => {}
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
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<crate::net::SockCmd>();
        self.async_sockets.insert(id, cmd_tx);

        let (tx, rx) = futures::channel::mpsc::unbounded::<ProcMsg>();
        std::thread::spawn(move || {
            let emit = |chunk: String| tx.unbounded_send(ProcMsg::Line(chunk)).is_ok();
            if let Err(e) = crate::net::socket_run(&host, port, tls, &cmd_rx, emit) {
                crate::script::set_last_error(e);
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
            let _ = tx.send(crate::net::SockCmd::Send(bytes));
        }
    }

    /// Close async socket `id` (`socket-async-close`): tell its worker to stop.
    pub(super) fn socket_async_close(&mut self, id: u64) {
        if let Some(tx) = self.async_sockets.get(&id) {
            let _ = tx.send(crate::net::SockCmd::Close);
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
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<crate::net::SockCmd>();
        self.async_sockets.insert(id, cmd_tx);

        let (tx, rx) = futures::channel::mpsc::unbounded::<ProcMsg>();
        std::thread::spawn(move || {
            let emit = |text: String| tx.unbounded_send(ProcMsg::Line(text)).is_ok();
            if let Err(e) = crate::net::ws_run(&url, &cmd_rx, emit) {
                crate::script::set_last_error(e);
            }
            let _ = tx.unbounded_send(ProcMsg::Exit);
        });

        self.drain_socket_worker(id, on_message, on_close, rx, cx);
    }

    /// Queue a text message (`ws-send`) for WebSocket `id`.
    pub(super) fn ws_send(&mut self, id: u64, text: &str) {
        if let Some(tx) = self.async_sockets.get(&id) {
            let _ = tx.send(crate::net::SockCmd::Send(text.as_bytes().to_vec()));
        }
    }

    /// Close WebSocket `id` (`ws-close`).
    pub(super) fn ws_close(&mut self, id: u64) {
        if let Some(tx) = self.async_sockets.get(&id) {
            let _ = tx.send(crate::net::SockCmd::Close);
        }
    }
}
