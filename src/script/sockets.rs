//! Backends for the blocking socket, SQLite and DNS primitives: the
//! process-wide handle tables behind the ids handed to Scheme, the shared
//! last-error slot, TLS setup, and the hand-rolled DNS resolver. No Steel
//! coupling: the registrations call into this.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, OnceLock};

// --- handle tables -----------------------------------------------------------
// Process-wide tables of open connections/listeners/sockets/databases, keyed by
// the ids handed to Scheme. Connections are buffered so `*-recv-line` and
// exact-length `*-recv-bytes` reads compose (an IMAP literal follows its
// control line in the same buffer). Only the engine thread calls the blocking
// primitives, but the Mutexes also let the async socket worker threads (editor
// side) share the same error slot and id source. Contention is nil in practice.

static NEXT_ID: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);

/// Next connection/listener/socket/database id (shared across every handle
/// kind so an id is globally unambiguous).
pub(crate) fn sock_next_id() -> usize {
    NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// A process-wide table of open handles of one kind, keyed by the ids
/// [`sock_next_id`] hands out.
pub(crate) struct Handles<T>(LazyLock<Mutex<HashMap<usize, T>>>);

impl<T> Handles<T> {
    pub(crate) const fn new() -> Self {
        Handles(LazyLock::new(|| Mutex::new(HashMap::new())))
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, HashMap<usize, T>> {
        self.0.lock().unwrap()
    }

    /// Store `handle` under a fresh id and return the id.
    pub(crate) fn insert(&self, handle: T) -> usize {
        let id = sock_next_id();
        self.lock().insert(id, handle);
        id
    }

    /// Drop handle `id`; whether it existed.
    pub(crate) fn remove(&self, id: usize) -> bool {
        self.lock().remove(&id).is_some()
    }
}

/// An open client connection: the buffered stream, plus any bytes a
/// `recv-line`/`recv-bytes` already pulled off it before a read timeout (or
/// other error) cut it short. Those are handed back first on the next receive,
/// so a timeout mid-line never loses data.
pub(crate) struct Conn {
    pub(crate) reader: std::io::BufReader<Sock>,
    pub(crate) pending: Vec<u8>,
}

impl Conn {
    pub(crate) fn new(sock: Sock) -> Self {
        Conn {
            reader: std::io::BufReader::new(sock),
            pending: Vec::new(),
        }
    }
}

pub(crate) static CONNS: Handles<Conn> = Handles::new();
pub(crate) static LISTENERS: Handles<std::net::TcpListener> = Handles::new();
pub(crate) static UDP: Handles<std::net::UdpSocket> = Handles::new();
/// Open SQLite connections, keyed by the id `sqlite-open` hands to Scheme.
/// `Connection` is `Send` (engine-thread only), so a Mutex keeps the static
/// sound. Shares the socket id source so every handle id is globally unique.
pub(crate) static SQLITE: Handles<rusqlite::Connection> = Handles::new();

// --- the shared last-error slot ----------------------------------------------

/// The most recent failure of any socket, UDP, SQLite or DNS primitive (and of
/// the async socket workers), for `(socket-last-error)` / `(sqlite-last-error)`.
fn last_error_slot() -> &'static Mutex<String> {
    static ERR: OnceLock<Mutex<String>> = OnceLock::new();
    ERR.get_or_init(|| Mutex::new(String::new()))
}

/// The last recorded failure (see [`set_last_error`]).
pub(crate) fn last_error() -> String {
    last_error_slot().lock().unwrap().clone()
}

/// Record `msg` as the last failure.
pub(crate) fn set_last_error(msg: String) {
    *last_error_slot().lock().unwrap() = msg;
}

/// `r`'s value, or `fallback` after recording its error as the last failure —
/// the "0 / #f / \"\" on failure, details in `*-last-error`" convention.
pub(crate) fn or_report<T>(r: Result<T, String>, fallback: T) -> T {
    r.unwrap_or_else(|e| {
        set_last_error(e);
        fallback
    })
}

// --- streams and TLS -----------------------------------------------------------

/// A byte stream that is either plain TCP or TLS-over-TCP. `Read`/`Write`
/// delegate to the inner stream; `tls-upgrade` swaps a `Plain` for a `Tls` in
/// place (STARTTLS). Boxed TLS variant keeps the enum small.
pub(crate) enum Sock {
    Plain(std::net::TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, std::net::TcpStream>>),
}

impl Sock {
    /// The underlying TCP stream, for timeout/shutdown control.
    pub(crate) fn tcp(&self) -> &std::net::TcpStream {
        match self {
            Sock::Plain(t) => t,
            Sock::Tls(s) => s.get_ref(),
        }
    }
}

impl std::io::Read for Sock {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Sock::Plain(t) => t.read(buf),
            Sock::Tls(s) => s.read(buf),
        }
    }
}

impl std::io::Write for Sock {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Sock::Plain(t) => t.write(buf),
            Sock::Tls(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Sock::Plain(t) => t.flush(),
            Sock::Tls(s) => s.flush(),
        }
    }
}

/// Bytes as the byte-preserving latin-1 string the socket primitives return.
fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

/// Build a rustls client config trusting the platform certificate store. Shared
/// by `tls-connect`, `tls-upgrade`, and the async/WebSocket socket workers.
pub(crate) fn tls_client_config() -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls_native_certs::load_native_certs().certs {
        let _ = roots.add(cert);
    }
    Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

/// Open a plain TCP connection with the given timeouts (0 = a sane default).
pub(crate) fn tcp_connect_impl(
    host: &str,
    port: u16,
    connect_ms: u64,
    read_ms: u64,
) -> Result<std::net::TcpStream, String> {
    use std::net::ToSocketAddrs;
    let addr = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("resolve {host}: {e}"))?
        .next()
        .ok_or_else(|| format!("resolve {host}: no addresses"))?;
    let connect_ms = if connect_ms == 0 { 10_000 } else { connect_ms };
    let tcp =
        std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(connect_ms))
            .map_err(|e| format!("connect {host}:{port}: {e}"))?;
    let read_ms = if read_ms == 0 { 30_000 } else { read_ms };
    tcp.set_read_timeout(Some(std::time::Duration::from_millis(read_ms)))
        .map_err(|e| format!("read-timeout: {e}"))?;
    Ok(tcp)
}

/// Connect a plain TCP stream with default timeouts, for the async socket /
/// WebSocket workers (editor side). Thin wrapper over [`tcp_connect_impl`].
pub(crate) fn tcp_connect_for_async(host: &str, port: u16) -> Result<std::net::TcpStream, String> {
    tcp_connect_impl(host, port, 0, 0)
}

/// Wrap an already-connected TCP stream in a rustls client session against
/// `host` (the SNI / cert name). The handshake completes lazily on the first
/// read/write, so a bad certificate surfaces as a first-recv error. Shared by
/// `tls-connect` (fresh socket) and `tls-upgrade` (STARTTLS on a live socket).
pub(crate) fn tls_wrap(
    tcp: std::net::TcpStream,
    host: &str,
) -> Result<rustls::StreamOwned<rustls::ClientConnection, std::net::TcpStream>, String> {
    let name = rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|e| format!("server name {host}: {e}"))?;
    let conn = rustls::ClientConnection::new(tls_client_config(), name)
        .map_err(|e| format!("tls session: {e}"))?;
    Ok(rustls::StreamOwned::new(conn, tcp))
}

// --- blocking send / receive -------------------------------------------------

/// Write all of `bytes` to connection `id`; the backing primitive for
/// `socket-send` / `tls-send`.
pub(crate) fn socket_send_impl(id: usize, bytes: &[u8]) -> bool {
    use std::io::Write;
    let mut conns = CONNS.lock();
    match conns.get_mut(&id) {
        Some(conn) => {
            let s = conn.reader.get_mut();
            match s.write_all(bytes) {
                Ok(()) => s.flush().is_ok(),
                Err(e) => {
                    set_last_error(format!("send: {e}"));
                    false
                }
            }
        }
        None => {
            set_last_error(format!("send: no connection {id}"));
            false
        }
    }
}

/// Read one line (through `\n`, inclusive) from connection `id` as a
/// byte-preserving latin-1 string. Backs `socket-recv-line` / `tls-recv-line`.
pub(crate) fn socket_recv_line_impl(id: usize) -> String {
    use std::io::BufRead;
    let mut conns = CONNS.lock();
    let Some(conn) = conns.get_mut(&id) else {
        set_last_error(format!("recv-line: no connection {id}"));
        return String::new();
    };
    let mut buf = std::mem::take(&mut conn.pending);
    if let Some(i) = buf.iter().position(|&b| b == b'\n') {
        conn.pending = buf.split_off(i + 1);
        return latin1(&buf);
    }
    // `read_until` appends whatever it got before an error to `buf`; those
    // bytes are already out of the reader, so keep them for the next call.
    match conn.reader.read_until(b'\n', &mut buf) {
        Ok(0) if buf.is_empty() => {
            set_last_error("recv-line: connection closed".into());
            String::new()
        }
        Ok(_) => latin1(&buf),
        Err(e) => {
            conn.pending = buf;
            set_last_error(format!("recv-line: {e}"));
            String::new()
        }
    }
}

/// Ceiling on a single `recv-bytes` (256 MiB). `n` is usually a server-quoted
/// protocol length (an IMAP `{NNN}` literal); an oversized value would
/// otherwise `vec![0u8; n]`-abort the whole process. Above the cap we error
/// rather than pre-allocate.
const SOCKET_RECV_MAX: usize = 256 * 1024 * 1024;

/// Read exactly `n` bytes from connection `id`. Backs `socket-recv-bytes` /
/// `tls-recv-bytes`.
pub(crate) fn socket_recv_bytes_impl(id: usize, n: usize) -> String {
    use std::io::Read;
    if n > SOCKET_RECV_MAX {
        set_last_error(format!("recv-bytes: length {n} exceeds cap"));
        return String::new();
    }
    let mut conns = CONNS.lock();
    let Some(conn) = conns.get_mut(&id) else {
        set_last_error(format!("recv-bytes: no connection {id}"));
        return String::new();
    };
    let mut buf = std::mem::take(&mut conn.pending);
    if buf.len() >= n {
        conn.pending = buf.split_off(n);
        return latin1(&buf);
    }
    // Read the remainder piecewise (not `read_exact`, whose partial bytes are
    // unspecified on error) so a timeout keeps what already arrived.
    let mut have = buf.len();
    buf.resize(n, 0);
    while have < n {
        match conn.reader.read(&mut buf[have..]) {
            Ok(0) => {
                buf.truncate(have);
                conn.pending = buf;
                set_last_error("recv-bytes: connection closed".into());
                return String::new();
            }
            Ok(k) => have += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => {
                buf.truncate(have);
                conn.pending = buf;
                set_last_error(format!("recv-bytes: {e}"));
                return String::new();
            }
        }
    }
    latin1(&buf)
}

// --- DNS ---------------------------------------------------------------------

/// The `nameserver`s in /etc/resolv.conf, in order, else Google public DNS.
/// macOS keeps a usable resolv.conf for the primary resolver even though
/// scutil is the system-of-record; the 8.8.8.8 fallback covers the rare empty
/// case. Unparseable entries (e.g. a link-local `fe80::1%en0`) are skipped.
fn dns_nameservers() -> Vec<std::net::IpAddr> {
    let conf = std::fs::read_to_string("/etc/resolv.conf").unwrap_or_default();
    let mut servers = parse_resolv_conf_nameservers(&conf);
    if servers.is_empty() {
        servers.push(std::net::IpAddr::from([8, 8, 8, 8]));
    }
    servers
}

/// The parseable `nameserver` addresses in resolv.conf text, in order.
pub(crate) fn parse_resolv_conf_nameservers(conf: &str) -> Vec<std::net::IpAddr> {
    conf.lines()
        .filter_map(|line| line.trim().strip_prefix("nameserver"))
        .filter_map(|rest| rest.split_whitespace().next()?.parse().ok())
        .collect()
}

/// One query/response round trip to nameserver `ns`, from a socket of the
/// matching address family (an IPv4 socket can't reach an IPv6 server).
fn dns_query(ns: std::net::IpAddr, msg: &[u8]) -> Result<Vec<u8>, String> {
    let bind = if ns.is_ipv6() { "[::]:0" } else { "0.0.0.0:0" };
    let socket = std::net::UdpSocket::bind(bind).map_err(|e| e.to_string())?;
    socket
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .ok();
    // `connect` filters out datagrams from anyone but the nameserver.
    socket
        .connect((ns, 53))
        .map_err(|e| format!("connect to {ns}: {e}"))?;
    socket.send(msg).map_err(|e| format!("send to {ns}: {e}"))?;
    let mut resp = vec![0u8; 4096];
    let n = socket
        .recv(&mut resp)
        .map_err(|e| format!("recv from {ns}: {e}"))?;
    resp.truncate(n);
    Ok(resp)
}

/// A hand-rolled DNS resolver over UDP (one query, one datagram). Builds the
/// query packet, sends it to the system nameserver, and parses the answer
/// section — including name compression — for the requested record type.
pub(crate) fn dns_resolve_impl(host: &str, rtype: &str) -> Result<Vec<String>, String> {
    let qtype: u16 = match rtype.to_ascii_uppercase().as_str() {
        "A" => 1,
        "NS" => 2,
        "CNAME" => 5,
        "MX" => 15,
        "TXT" => 16,
        "AAAA" => 28,
        "SRV" => 33,
        other => return Err(format!("unknown record type {other}")),
    };
    // --- build the query -------------------------------------------------
    let mut msg: Vec<u8> = Vec::with_capacity(64);
    let id: u16 = 0x4d41; // "MA" — a fixed id is fine for one blocking round trip
    msg.extend_from_slice(&id.to_be_bytes());
    msg.extend_from_slice(&0x0100u16.to_be_bytes()); // flags: standard query, RD
    msg.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT
    msg.extend_from_slice(&[0, 0, 0, 0, 0, 0]); // AN/NS/AR counts = 0
    for label in host.trim_end_matches('.').split('.') {
        if label.is_empty() || label.len() > 63 {
            return Err("bad hostname label".into());
        }
        msg.push(label.len() as u8);
        msg.extend_from_slice(label.as_bytes());
    }
    msg.push(0); // root label
    msg.extend_from_slice(&qtype.to_be_bytes());
    msg.extend_from_slice(&1u16.to_be_bytes()); // QCLASS = IN
    // --- send + receive: each nameserver in turn until one answers ---------
    let mut last_err = String::from("no nameservers");
    for ns in dns_nameservers() {
        match dns_query(ns, &msg).and_then(|resp| dns_parse_answers(&resp, qtype)) {
            Ok(records) => return Ok(records),
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

/// Read a (possibly compression-pointer-following) domain name starting at
/// `pos`; returns the dotted name and the offset just past the name *in the
/// original stream* (pointers don't advance the outer cursor past the pointer).
fn dns_read_name(msg: &[u8], mut pos: usize) -> Result<(String, usize), String> {
    let mut labels = Vec::new();
    let mut jumped = false;
    let mut next = pos;
    let mut guard = 0;
    loop {
        guard += 1;
        if guard > 128 || pos >= msg.len() {
            return Err("malformed name".into());
        }
        let len = msg[pos] as usize;
        if len & 0xc0 == 0xc0 {
            // Compression pointer: two bytes, offset in the low 14 bits.
            if pos + 1 >= msg.len() {
                return Err("truncated pointer".into());
            }
            let ptr = (((len & 0x3f) << 8) | msg[pos + 1] as usize) & 0x3fff;
            if !jumped {
                next = pos + 2;
            }
            jumped = true;
            pos = ptr;
        } else if len == 0 {
            pos += 1;
            if !jumped {
                next = pos;
            }
            break;
        } else {
            pos += 1;
            if pos + len > msg.len() {
                return Err("truncated label".into());
            }
            labels.push(String::from_utf8_lossy(&msg[pos..pos + len]).into_owned());
            pos += len;
        }
    }
    Ok((labels.join("."), next))
}

/// Parse the answer section, returning the records matching `qtype` formatted
/// as text (A/AAAA as addresses; CNAME/NS/PTR as names; MX/SRV keep their
/// numeric fields; TXT concatenates its character-strings).
fn dns_parse_answers(msg: &[u8], qtype: u16) -> Result<Vec<String>, String> {
    if msg.len() < 12 {
        return Err("short response".into());
    }
    let qd = u16::from_be_bytes([msg[4], msg[5]]) as usize;
    let an = u16::from_be_bytes([msg[6], msg[7]]) as usize;
    let mut pos = 12;
    // Skip the question section.
    for _ in 0..qd {
        let (_, p) = dns_read_name(msg, pos)?;
        pos = p + 4; // QTYPE + QCLASS
    }
    let mut out = Vec::new();
    for _ in 0..an {
        let (_, p) = dns_read_name(msg, pos)?;
        pos = p;
        if pos + 10 > msg.len() {
            return Err("truncated RR header".into());
        }
        let rtype = u16::from_be_bytes([msg[pos], msg[pos + 1]]);
        let rdlen = u16::from_be_bytes([msg[pos + 8], msg[pos + 9]]) as usize;
        pos += 10;
        if pos + rdlen > msg.len() {
            return Err("truncated rdata".into());
        }
        let rdata = &msg[pos..pos + rdlen];
        if rtype == qtype {
            match qtype {
                1 if rdlen == 4 => {
                    out.push(format!(
                        "{}.{}.{}.{}",
                        rdata[0], rdata[1], rdata[2], rdata[3]
                    ));
                }
                28 if rdlen == 16 => {
                    let segs: Vec<String> = (0..8)
                        .map(|i| {
                            format!("{:x}", u16::from_be_bytes([rdata[i * 2], rdata[i * 2 + 1]]))
                        })
                        .collect();
                    out.push(segs.join(":"));
                }
                5 | 2 => {
                    if let Ok((name, _)) = dns_read_name(msg, pos) {
                        out.push(name);
                    }
                }
                // rdlen guards: a truncated/hostile MX (<2) or SRV (<6) rdata
                // would otherwise index a too-short `rdata` slice and panic.
                // The trailing name is read via the bounds-checked
                // `dns_read_name`, so only the fixed fields need guarding.
                15 if rdlen >= 2 => {
                    let pref = u16::from_be_bytes([rdata[0], rdata[1]]);
                    if let Ok((name, _)) = dns_read_name(msg, pos + 2) {
                        out.push(format!("{pref} {name}"));
                    }
                }
                33 if rdlen >= 6 => {
                    let prio = u16::from_be_bytes([rdata[0], rdata[1]]);
                    let weight = u16::from_be_bytes([rdata[2], rdata[3]]);
                    let port = u16::from_be_bytes([rdata[4], rdata[5]]);
                    if let Ok((name, _)) = dns_read_name(msg, pos + 6) {
                        out.push(format!("{prio} {weight} {port} {name}"));
                    }
                }
                16 => {
                    let mut text = String::new();
                    let mut i = 0;
                    while i < rdata.len() {
                        let l = rdata[i] as usize;
                        i += 1;
                        if i + l <= rdata.len() {
                            text.push_str(&String::from_utf8_lossy(&rdata[i..i + l]));
                            i += l;
                        } else {
                            break;
                        }
                    }
                    out.push(text);
                }
                _ => {}
            }
        }
        pos += rdlen;
    }
    Ok(out)
}

#[cfg(test)]
mod dns_tests {
    use super::{dns_parse_answers, dns_read_name};

    // Build a minimal DNS response: header (qd=1, an=1), a question for name
    // "a." of `qtype`, then one answer whose owner name is a compression
    // pointer back to the question, with the given `rtype`/`rdata`.
    fn response(qtype: u16, rtype: u16, rdata: &[u8]) -> Vec<u8> {
        let mut m = vec![
            0x00, 0x00, // id
            0x81, 0x80, // flags: response, RD, RA
            0x00, 0x01, // qdcount = 1
            0x00, 0x01, // ancount = 1
            0x00, 0x00, 0x00, 0x00, // ns/ar = 0
        ];
        // Question: "a." + qtype + IN
        m.extend_from_slice(&[0x01, b'a', 0x00]);
        m.extend_from_slice(&qtype.to_be_bytes());
        m.extend_from_slice(&1u16.to_be_bytes());
        // Answer: owner = pointer to offset 12 (the question name), type, IN,
        // ttl=0, rdlen, rdata.
        m.extend_from_slice(&[0xc0, 0x0c]);
        m.extend_from_slice(&rtype.to_be_bytes());
        m.extend_from_slice(&1u16.to_be_bytes());
        m.extend_from_slice(&[0, 0, 0, 0]); // ttl
        m.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
        m.extend_from_slice(rdata);
        m
    }

    #[test]
    fn short_mx_rdata_does_not_panic() {
        // MX with a 1-byte rdata (< the 2-byte preference) must be skipped,
        // not indexed — this panicked before the rdlen guard.
        let msg = response(15, 15, &[0x00]);
        let out = dns_parse_answers(&msg, 15).expect("parse");
        assert!(out.is_empty(), "short MX should yield no records: {out:?}");
    }

    #[test]
    fn short_srv_rdata_does_not_panic() {
        // SRV needs 6 fixed bytes; a 5-byte rdata must be skipped.
        let msg = response(33, 33, &[0, 0, 0, 0, 0]);
        let out = dns_parse_answers(&msg, 33).expect("parse");
        assert!(out.is_empty(), "short SRV should yield no records: {out:?}");
    }

    #[test]
    fn well_formed_mx_and_a_parse() {
        // A valid MX: preference 10 + name "mx.a." (label "mx" then pointer).
        let mut rdata = vec![0x00, 0x0a, 0x02, b'm', b'x', 0xc0, 0x0c];
        let msg = response(15, 15, &rdata);
        let out = dns_parse_answers(&msg, 15).expect("parse");
        assert_eq!(out, vec!["10 mx.a".to_string()]);
        // A record: 1.2.3.4
        rdata = vec![1, 2, 3, 4];
        let msg = response(1, 1, &rdata);
        assert_eq!(
            dns_parse_answers(&msg, 1).unwrap(),
            vec!["1.2.3.4".to_string()]
        );
    }

    #[test]
    fn compression_pointer_loop_terminates() {
        // A name that is a pointer to itself must error, not hang.
        let mut msg = vec![0u8; 14];
        msg[12] = 0xc0;
        msg[13] = 0x0c; // points at offset 12 == itself
        assert!(dns_read_name(&msg, 12).is_err());
    }
}
