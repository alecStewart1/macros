//! Socket primitives: blocking TCP / TLS / UDP and DNS over the handle
//! tables in `script::sockets`, plus the async socket and WebSocket
//! families.

use super::*;

pub(in crate::script) fn register(reg: &mut Registrar, ctx: &EngineShared) {
    let EngineShared { queue, .. } = ctx;
    let async_ids = &ctx.ids;
    reg.section(Section::Sockets);
    // --- client sockets (blocking, engine thread) ------------------------
    // Synchronous TCP/TLS/UDP for Scheme protocol code (IMAP, SMTP, NNTP,
    // Redis, DNS …). Blocking is deliberate: protocol scripts read like prose,
    // and the engine thread already hosts blocking primitives
    // (call-process-full, grep-files) — the UI thread never waits on these. A
    // long-lived or push-driven connection (IMAP IDLE, a chat gateway) should
    // instead use the async `socket-connect-async` / `ws-connect` families,
    // which deliver inbound data to a callback off-thread.
    //
    // Received bytes come back as a byte-preserving latin-1 string
    // (char->integer = the raw byte), so binary payloads survive the boundary
    // and Scheme re-decodes text with `decode-charset` once the charset is
    // known. Outbound strings are sent as their UTF-8 bytes.
    //
    //   (tcp-connect host port connect-ms read-ms) → conn id, 0 on failure
    //   (tls-connect host port connect-ms read-ms) → conn id, 0 on failure
    //     (0 for either timeout picks a sane default; see socket-last-error)
    //   (tls-upgrade id host)     → #t once a plain conn is wrapped in TLS
    //                               (STARTTLS; `host` is the cert/SNI name)
    //   (socket-set-timeout id ms)→ #t; ms=0 blocks forever (IMAP IDLE)
    //   (socket-send id text)     → #t on full write
    //   (socket-recv-line id)     → one line incl. terminator, "" on EOF/error
    //   (socket-recv-bytes id n)  → exactly n bytes, "" on error
    //   (socket-close id)         → #t if the id existed
    //   (socket-last-error)       → the most recent failure, for messages
    //   `tls-send`/`tls-recv-*`/`tls-close` remain as aliases (compatibility).
    //
    // TCP servers:
    //   (tcp-listen host port)    → listener id, 0 on failure (port 0 = OS pick)
    //   (tcp-listener-port id)    → the bound port
    //   (tcp-accept id)           → an inbound conn id (blocks), 0 on error
    //   (tcp-listener-close id)   → #t if the id existed
    //
    // UDP datagrams:
    //   (udp-open)                → socket id bound to an ephemeral port, 0 fail
    //   (udp-send id host port hex) → #t; payload is HEX (binary-clean)
    //   (udp-recv id max)         → "host:port\n<hex>" of one datagram, "" fail
    //   (udp-close id)            → #t if the id existed
    reg.register_fn(
        "tcp-connect",
        |host: String,
         Port(port): Port,
         NonNeg(connect_ms): NonNeg,
         NonNeg(read_ms): NonNeg|
         -> usize {
            let tcp = tcp_connect_impl(&host, port, connect_ms as u64, read_ms as u64);
            or_report(tcp.map(|tcp| CONNS.insert(Conn::new(Sock::Plain(tcp)))), 0)
        },
    );
    reg.register_fn(
        "tls-connect",
        |host: String,
         Port(port): Port,
         NonNeg(connect_ms): NonNeg,
         NonNeg(read_ms): NonNeg|
         -> usize {
            let tls = tcp_connect_impl(&host, port, connect_ms as u64, read_ms as u64)
                .and_then(|tcp| tls_wrap(tcp, &host));
            or_report(
                tls.map(|tls| CONNS.insert(Conn::new(Sock::Tls(Box::new(tls))))),
                0,
            )
        },
    );
    // STARTTLS: pull the plain socket out of the table and wrap it. Any bytes
    // already buffered by prior recv calls are dropped — after a STARTTLS "OK"
    // the client must not have read past it, so a non-empty buffer here would
    // be a plaintext-injection attempt; refusing it is correct.
    reg.register_fn("tls-upgrade", |id: usize, host: String| -> bool {
        let mut conns = CONNS.lock();
        let Some(conn) = conns.remove(&id) else {
            set_last_error(format!("tls-upgrade: no connection {id}"));
            return false;
        };
        if !conn.reader.buffer().is_empty() {
            conns.insert(id, conn);
            set_last_error("tls-upgrade: buffered data before TLS (injection?)".into());
            return false;
        }
        // Bytes held back by an interrupted recv stay pending across the
        // upgrade, as they did when they lived in a separate table.
        let Conn { reader, pending } = conn;
        let wrapped = |sock| Conn {
            reader: std::io::BufReader::new(sock),
            pending,
        };
        match reader.into_inner() {
            Sock::Tls(tls) => {
                conns.insert(id, wrapped(Sock::Tls(tls)));
                set_last_error(format!("tls-upgrade: {id} is already TLS"));
                false
            }
            Sock::Plain(tcp) => match tls_wrap(tcp, &host) {
                Ok(tls) => {
                    conns.insert(id, wrapped(Sock::Tls(Box::new(tls))));
                    true
                }
                Err(e) => {
                    set_last_error(e);
                    false
                }
            },
        }
    });
    reg.register_fn(
        "socket-set-timeout",
        |id: usize, NonNeg(ms): NonNeg| -> bool {
            let conns = CONNS.lock();
            let Some(conn) = conns.get(&id) else {
                set_last_error(format!("socket-set-timeout: no connection {id}"));
                return false;
            };
            let dur = if ms == 0 {
                None
            } else {
                Some(std::time::Duration::from_millis(ms as u64))
            };
            conn.reader.get_ref().tcp().set_read_timeout(dur).is_ok()
        },
    );
    reg.register_fn("socket-send", |id: usize, text: String| -> bool {
        socket_send_impl(id, text.as_bytes())
    });
    reg.register_fn("socket-recv-line", |id: usize| -> String {
        socket_recv_line_impl(id)
    });
    reg.register_fn(
        "socket-recv-bytes",
        |id: usize, NonNeg(n): NonNeg| -> String { socket_recv_bytes_impl(id, n) },
    );
    reg.register_fn("socket-close", |id: usize| -> bool { CONNS.remove(id) });
    reg.register_fn("socket-last-error", last_error);
    // Back-compat aliases for the original TLS-only names.
    reg.register_fn("tls-send", |id: usize, text: String| -> bool {
        socket_send_impl(id, text.as_bytes())
    });
    reg.register_fn("tls-recv-line", |id: usize| -> String {
        socket_recv_line_impl(id)
    });
    reg.register_fn("tls-recv-bytes", |id: usize, NonNeg(n): NonNeg| -> String {
        socket_recv_bytes_impl(id, n)
    });
    reg.register_fn("tls-close", |id: usize| -> bool { CONNS.remove(id) });
    reg.register_fn("tls-last-error", last_error);
    // TCP server.
    reg.register_fn("tcp-listen", |host: String, Port(port): Port| -> usize {
        let listener = std::net::TcpListener::bind((host.as_str(), port))
            .map_err(|e| format!("tcp-listen {host}:{port}: {e}"));
        or_report(listener.map(|l| LISTENERS.insert(l)), 0)
    });
    reg.register_fn("tcp-listener-port", |id: usize| -> usize {
        LISTENERS
            .lock()
            .get(&id)
            .and_then(|l| l.local_addr().ok())
            .map(|a| a.port() as usize)
            .unwrap_or(0)
    });
    reg.register_fn("tcp-accept", |id: usize| -> usize {
        // Clone the listener handle out so the accept doesn't hold the table
        // lock (which would deadlock a concurrent close or another accept).
        let listener = match LISTENERS.lock().get(&id) {
            Some(l) => l.try_clone().map_err(|e| format!("tcp-accept: {e}")),
            None => Err(format!("tcp-accept: no listener {id}")),
        };
        let accepted = listener.and_then(|l| l.accept().map_err(|e| format!("tcp-accept: {e}")));
        or_report(
            accepted.map(|(tcp, _)| CONNS.insert(Conn::new(Sock::Plain(tcp)))),
            0,
        )
    });
    reg.register_fn("tcp-listener-close", |id: usize| -> bool {
        LISTENERS.remove(id)
    });
    // UDP.
    reg.register_fn("udp-open", || -> usize {
        let sock = std::net::UdpSocket::bind("0.0.0.0:0").map_err(|e| format!("udp-open: {e}"));
        or_report(
            sock.map(|s| {
                let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(10)));
                UDP.insert(s)
            }),
            0,
        )
    });
    reg.register_fn(
        "udp-send",
        |id: usize, host: String, Port(port): Port, hex_data: String| -> bool {
            let Ok(bytes) = hex::decode(hex_data.trim()) else {
                set_last_error("udp-send: bad hex".into());
                return false;
            };
            let socks = UDP.lock();
            let Some(sock) = socks.get(&id) else {
                set_last_error(format!("udp-send: no socket {id}"));
                return false;
            };
            let sent = sock
                .send_to(&bytes, (host.as_str(), port))
                .map_err(|e| format!("udp-send: {e}"));
            or_report(sent.map(|_| true), false)
        },
    );
    reg.register_fn("udp-recv", |id: usize, NonNeg(max): NonNeg| -> String {
        let socks = UDP.lock();
        let Some(sock) = socks.get(&id) else {
            set_last_error(format!("udp-recv: no socket {id}"));
            return String::new();
        };
        // A UDP datagram can't exceed ~64 KiB; clamp so a huge `max` can't
        // pre-allocate unboundedly (and 1 is the floor so recv has room).
        let mut buf = vec![0u8; max.clamp(1, 65535)];
        let received = sock
            .recv_from(&mut buf)
            .map(|(n, from)| format!("{from}\n{}", hex::encode(&buf[..n])))
            .map_err(|e| format!("udp-recv: {e}"));
        or_report(received, String::new())
    });
    reg.register_fn("udp-close", |id: usize| -> bool { UDP.remove(id) });
    // DNS: resolve `host` for a record `type` ("A" "AAAA" "MX" "TXT" "SRV"
    // "CNAME" "NS"). Returns newline-separated records ("" = none/failure);
    // MX/SRV keep their priority/weight fields space-separated. Hand-rolled
    // over UDP to the system resolver, so no async DNS dependency.
    reg.register_fn("dns-resolve", |host: String, rtype: String| -> String {
        let records = dns_resolve_impl(&host, &rtype)
            .map(|records| records.join("\n"))
            .map_err(|e| format!("dns-resolve: {e}"));
        or_report(records, String::new())
    });
    // --- async sockets + WebSockets (off-thread, callback-driven) ---------
    // For long-lived or push-driven connections where blocking recv is wrong:
    // IMAP IDLE, chat gateways, CDP, market feeds. A worker thread owns the
    // connection and delivers inbound data to a named callback (the socket
    // analogue of `make-process`); outbound sends and close are queued to it.
    //
    //   (socket-connect-async host port tls? on-data on-close) → id, 0 on queue
    //     on-data  called with each inbound chunk, HEX-encoded (binary-clean)
    //     on-close called with the id (as a string) when the peer/we close
    //   (socket-async-send id hex)  → #t once queued; payload is HEX
    //   (socket-async-close id)     → #t once queued
    //
    //   (ws-connect url on-message on-close) → id  (url is ws:// or wss://)
    //     on-message called with each inbound TEXT message (binary decoded lossy)
    //     on-close   called with the id when the socket closes
    //   (ws-send id text)  → #t once queued
    //   (ws-close id)      → #t once queued
    {
        let q = queue.clone();
        let ids = async_ids.clone();
        reg.register_fn(
            "socket-connect-async",
            move |host: String,
                  Port(port): Port,
                  tls: bool,
                  on_data: Callback,
                  on_close: Callback|
                  -> usize {
                let id = ids.next();
                let Callback(on_data) = on_data;
                let Callback(on_close) = on_close;
                q.push(EditorCommand::SocketConnectAsync {
                    id,
                    host,
                    port,
                    tls,
                    on_data,
                    on_close,
                });
                id as usize
            },
        );
    }
    {
        let q = queue.clone();
        reg.register_fn(
            "socket-async-send",
            move |id: usize, hex_data: String| -> bool {
                q.push(EditorCommand::SocketAsyncSend {
                    id: id as u64,
                    hex: hex_data,
                });
                true
            },
        );
    }
    {
        let q = queue.clone();
        reg.register_fn("socket-async-close", move |id: usize| -> bool {
            q.push(EditorCommand::SocketAsyncClose { id: id as u64 });
            true
        });
    }
    {
        let q = queue.clone();
        let ids = async_ids.clone();
        reg.register_fn(
            "ws-connect",
            move |url: String, on_message: Callback, on_close: Callback| -> usize {
                let id = ids.next();
                let Callback(on_message) = on_message;
                let Callback(on_close) = on_close;
                q.push(EditorCommand::WsConnect {
                    id,
                    url,
                    on_message,
                    on_close,
                });
                id as usize
            },
        );
    }
    {
        let q = queue.clone();
        reg.register_fn("ws-send", move |id: usize, text: String| -> bool {
            q.push(EditorCommand::WsSend {
                id: id as u64,
                text,
            });
            true
        });
    }
    {
        let q = queue.clone();
        reg.register_fn("ws-close", move |id: usize| -> bool {
            q.push(EditorCommand::WsClose { id: id as u64 });
            true
        });
    }
}
