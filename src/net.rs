//! Blocking network clients, each run on a worker thread so the UI never
//! waits: the HTTP calls behind `http-request` / `make-http-stream`, the
//! async-socket and WebSocket poll loops, and the agent for update downloads.
//! The editor side (`editor/net.rs`) only spawns these and pumps their output
//! to Steel callbacks.

use std::io::{BufReader, Read, Write};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

/// Read timeout of a socket / WebSocket poll loop, so it services its command
/// channel between inbound data.
const POLL_TIMEOUT: Duration = Duration::from_millis(150);

/// A command sent from the engine thread to an async socket / WebSocket worker.
pub enum SockCmd {
    /// Bytes (raw for a socket; a text message's UTF-8 for a WebSocket) to send.
    Send(Vec<u8>),
    /// Stop the worker and drop the connection.
    Close,
}

/// Drive an async socket worker (`socket-connect-async`): connect (TLS when
/// `tls`), then poll — drain outbound commands, then read with a short
/// timeout, handing each inbound chunk, hex-encoded, to `emit` (which returns
/// `false` once nobody's listening). Returns when the peer closes, the
/// connection errors, or a `Close` arrives; `Err` only for a failed connect or
/// TLS handshake.
pub fn socket_run(
    host: &str,
    port: u16,
    tls: bool,
    cmd_rx: &Receiver<SockCmd>,
    mut emit: impl FnMut(String) -> bool,
) -> Result<(), String> {
    // Connect (poll timeout short so the command channel stays responsive).
    let tcp = crate::script::tcp_connect_for_async(host, port)?;
    let mut sock = if tls {
        let mut s = crate::script::tls_wrap(tcp, host)?;
        // Drive the TLS handshake to completion under the connect helper's
        // default (30s) read timeout. Without this the handshake would run
        // lazily on the first send/recv under the 150ms poll timeout below and
        // a server slower than that would fail spuriously.
        s.conn
            .complete_io(&mut s.sock)
            .map_err(|e| format!("tls handshake: {e}"))?;
        crate::script::Sock::Tls(Box::new(s))
    } else {
        crate::script::Sock::Plain(tcp)
    };
    // Now drop to a short read timeout so the poll loop can service the
    // command channel between inbound chunks.
    let _ = sock.tcp().set_read_timeout(Some(POLL_TIMEOUT));
    let mut buf = [0u8; 16384];
    'outer: loop {
        // Drain EVERY queued command before the (up-to-150ms) read, so a burst
        // of sends or a close-after-sends isn't paced one per poll tick.
        loop {
            match cmd_rx.try_recv() {
                Ok(SockCmd::Send(bytes)) => {
                    if sock.write_all(&bytes).is_err() {
                        break 'outer;
                    }
                    let _ = sock.flush();
                }
                Ok(SockCmd::Close) | Err(TryRecvError::Disconnected) => break 'outer,
                Err(TryRecvError::Empty) => break, // drained
            }
        }
        match sock.read(&mut buf) {
            Ok(0) => break, // peer closed
            Ok(n) => {
                if !emit(hex::encode(&buf[..n])) {
                    break;
                }
            }
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => break,
        }
    }
    Ok(())
}

/// Drive a WebSocket worker: connect the TCP/TLS stream, hand it to tungstenite
/// for the handshake (over our own `Sock`, so no second TLS stack), then poll —
/// draining outbound text/close commands, then reading one frame with a short
/// timeout and handing text (and binary-as-text) frames to `emit` (which
/// returns `false` once nobody's listening). Returns when the socket closes or
/// errors; `Err` only for a failed connect or handshake.
pub fn ws_run(
    url: &str,
    cmd_rx: &Receiver<SockCmd>,
    mut emit: impl FnMut(String) -> bool,
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
    let _ = ws.get_ref().tcp().set_read_timeout(Some(POLL_TIMEOUT));

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
                Ok(SockCmd::Close) | Err(TryRecvError::Disconnected) => {
                    let _ = ws.close(None);
                    break 'outer;
                }
                Err(TryRecvError::Empty) => break, // drained
            }
        }
        // One inbound frame (read timeout surfaces as a WouldBlock/IO error).
        match ws.read() {
            Ok(Message::Text(t)) => {
                if !emit(t.to_string()) {
                    break;
                }
            }
            Ok(Message::Binary(b)) => {
                if !emit(String::from_utf8_lossy(&b).into_owned()) {
                    break;
                }
            }
            // tungstenite queues the Pong itself on reading a Ping (sending one
            // here too answered every ping twice); just flush it out.
            Ok(Message::Ping(_)) => {
                let _ = ws.flush();
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

/// The blocking half of `make-http-stream` (`Editor::http_stream`): POST `body` and hand each
/// response line to `emit` until it returns `false` (cancelled, or the receiver
/// is gone) or the body ends. A 4xx/5xx status becomes one
/// `error: HTTP <code>: <body>` line — relaying the API's own error JSON (e.g. a
/// 401 from a bad key) into the transcript — and a transport failure one
/// `error: <e>` line.
pub fn http_stream_blocking(
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
            for l in crate::process::lossy_lines(BufReader::new(resp.into_body().into_reader())) {
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

/// The blocking half of `http-request` (`Editor::http_request`): send one request and render
/// the response as the `on_response` payload (see [`HttpRequestArgs`]).
///
/// [`HttpRequestArgs`]: crate::script::HttpRequestArgs
pub fn http_request_blocking(
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
pub fn http_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .http_status_as_error(false)
        .allow_non_standard_methods(true)
        .build()
        .new_agent()
}

/// The agent for update checks and downloads, sending `user_agent`.
///
/// ureq 3 has no per-read timeout: `recv_response` bounds the wait for the
/// response head, and the body is left unbounded (`recv_body` caps the *whole*
/// body, which would cut off a large artifact on a slow link).
pub fn download_agent(user_agent: &str) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_recv_response(Some(Duration::from_secs(60)))
        .user_agent(user_agent)
        .build()
        .new_agent()
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
