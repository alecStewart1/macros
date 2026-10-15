//! An MCP (Model Context Protocol) server, for debugging the editor and
//! developing Scheme packages with an AI assistant: it can evaluate Steel in
//! the live editor and read buffers, messages, and documentation.
//!
//! OFF by default. `(set-option "mcp-server" #t)` starts it (see
//! `Editor::reconcile_mcp_server`); `M-x mcp-server-info` shows how to connect.
//!
//! Transport: MCP "streamable HTTP" with plain JSON responses (no SSE), on
//! 127.0.0.1 only. Because `eval` runs arbitrary code, every request must carry
//! `Authorization: Bearer <token>`, and requests whose `Host`/`Origin` aren't
//! local are refused — the spec's guard against DNS-rebinding attacks from a
//! web page in the user's browser.
//!
//! This module is the protocol and HTTP layer only. Editor access goes through
//! [`Backend`] (implemented in `editor/mcp.rs`), which keeps it testable.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::jsonrpc::{Message, error_response as rpc_error, lenient, response as rpc_result};

/// Protocol revisions we speak, newest first. A client asking for one of
/// these gets it back; anything else gets the newest.
const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// Largest request body accepted (an `eval` of a whole package file fits).
const MAX_BODY: usize = 8 << 20;
/// Request line + headers. Read before authentication, so it is kept small.
const MAX_HEAD: u64 = 64 << 10;
/// Connections served at once; more are closed immediately. Clients send one
/// request per connection, so this only bites a flood.
const MAX_CONNECTIONS: usize = 16;
/// Wall-clock budget for a whole request to arrive (a slow-drip client can't
/// hold a connection slot by sending a byte every few seconds).
const REQUEST_DEADLINE: Duration = Duration::from_secs(15);
/// How long a refused request's remaining bytes are read off before closing
/// (see [`close_unread`]).
const LINGER: Duration = Duration::from_millis(500);

/// What the server needs from the editor. Every method may block; each runs
/// on the connection's own thread, never on the UI thread.
pub trait Backend: Send + Sync + 'static {
    /// Evaluate Steel source in the live editor, applying any editor commands
    /// it queues.
    fn eval(&self, code: &str) -> EvalReport;
    /// A buffer's text and state (the current buffer when `name` is None),
    /// optionally just lines `start..end` (0-based, end-exclusive) and with
    /// overlay face spans.
    fn buffer_text(
        &self,
        name: Option<&str>,
        lines: Option<(usize, usize)>,
        faces: bool,
    ) -> Result<String, String>;
    /// Every buffer, one per line, the current one marked.
    fn list_buffers(&self) -> Result<String, String>;
    /// The last `count` echo-area messages.
    fn messages(&self, count: usize) -> Result<String, String>;
    /// Press `keys` (see the `press_keys` tool) in the editor's window, as if
    /// typed, and report the editor's state afterwards.
    fn press_keys(&self, keys: &str) -> Result<String, String>;
    /// Record one line in the editor's `*mcp-server*` buffer.
    fn log(&self, line: String);
}

/// The outcome of an `eval`.
#[derive(Debug, Default, PartialEq)]
pub struct EvalReport {
    /// The printed result, when evaluation produced one.
    pub value: Option<String>,
    pub error: Option<String>,
    /// Echo-area messages the evaluation produced.
    pub messages: Vec<String>,
    /// How many editor commands it queued (and the editor applied).
    pub commands: usize,
}

/// A running server. Dropping it (or [`McpServer::stop`]) shuts it down.
pub struct McpServer {
    pub port: u16,
    stop: Arc<AtomicBool>,
}

impl McpServer {
    /// Bind 127.0.0.1:`port` and serve on a background thread. `restarting`
    /// means a server of ours just let go of this port, so a brief `AddrInUse`
    /// is our own listener still closing: wait it out. Otherwise the port
    /// belongs to someone else, so fail at once rather than stall the caller
    /// (the editor calls this on its UI thread).
    pub fn start(
        port: u16,
        token: String,
        backend: Arc<dyn Backend>,
        restarting: bool,
    ) -> std::io::Result<McpServer> {
        let retries = if restarting { 20 } else { 0 };
        let mut attempt = 0;
        let listener = loop {
            match TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
                Err(e) if e.kind() == std::io::ErrorKind::AddrInUse && attempt < retries => {
                    attempt += 1;
                    std::thread::sleep(Duration::from_millis(25));
                }
                result => break result?,
            }
        };
        let port = listener.local_addr()?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let token = Arc::new(token);
        let active = Arc::new(AtomicUsize::new(0));
        std::thread::Builder::new()
            .name("mcp-server".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    if flag.load(Ordering::SeqCst) {
                        break;
                    }
                    let stream = match stream {
                        Ok(stream) => stream,
                        Err(_) => {
                            // Out of file descriptors and the like: back off
                            // instead of spinning on a failing accept.
                            std::thread::sleep(Duration::from_millis(50));
                            continue;
                        }
                    };
                    if active.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                        active.fetch_sub(1, Ordering::SeqCst);
                        continue; // dropping `stream` closes it
                    }
                    let (backend, token, active) = (backend.clone(), token.clone(), active.clone());
                    // One thread per connection: a long `eval` mustn't block
                    // a concurrent `messages` call.
                    let spawned = std::thread::Builder::new().name("mcp-conn".into()).spawn({
                        let active = active.clone();
                        move || {
                            serve_connection(stream, &token, backend.as_ref());
                            active.fetch_sub(1, Ordering::SeqCst);
                        }
                    });
                    if spawned.is_err() {
                        active.fetch_sub(1, Ordering::SeqCst);
                    }
                }
            })?;
        Ok(McpServer { port, stop })
    }

    /// Stop accepting connections. In-flight requests finish on their own.
    pub fn stop(&self) {
        if !self.stop.swap(true, Ordering::SeqCst) {
            // Wake the blocking accept so the thread sees the flag and exits.
            let _ = TcpStream::connect_timeout(
                &SocketAddr::from((Ipv4Addr::LOCALHOST, self.port)),
                Duration::from_millis(200),
            );
        }
    }
}

impl Drop for McpServer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// A parsed HTTP request head: method, path, lowercase header names. The
/// body is read separately, and only once the head has passed [`reject`].
struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Reads from the socket against one wall-clock deadline for the whole
/// request, rather than a timeout that restarts with every byte received.
struct DeadlineReader {
    stream: TcpStream,
    until: Instant,
}

impl Read for DeadlineReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = self.until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        self.stream.set_read_timeout(Some(left))?;
        self.stream.read(buf)
    }
}

type Body = std::io::Take<BufReader<DeadlineReader>>;

/// Read the request line and headers (at most [`MAX_HEAD`] bytes). Returns
/// the head and the reader positioned at the body.
fn read_head(stream: &TcpStream) -> Option<(Request, Body)> {
    let reader = DeadlineReader {
        stream: stream.try_clone().ok()?,
        until: Instant::now() + REQUEST_DEADLINE,
    };
    // `take` bounds every `read_line`: a line that never ends can't grow
    // without limit, it just runs out of budget.
    let mut reader = BufReader::new(reader).take(MAX_HEAD);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let mut headers = Vec::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).ok()? == 0 {
            return None; // EOF, or the head budget ran out
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if headers.len() > 100 {
            return None;
        }
        let (k, v) = trimmed.split_once(':')?;
        headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
    }
    Some((
        Request {
            method,
            path,
            headers,
        },
        reader,
    ))
}

/// The body, per `Content-Length`; an `Err` is the HTTP status to answer.
fn read_body(req: &Request, mut reader: Body) -> Result<Vec<u8>, &'static str> {
    if req.header("transfer-encoding").is_some() {
        return Err("501 Not Implemented"); // no chunked bodies
    }
    let Some(len) = req.header("content-length") else {
        return Err("411 Length Required");
    };
    let len = len.parse::<usize>().map_err(|_| "400 Bad Request")?;
    if len > MAX_BODY {
        return Err("413 Content Too Large");
    }
    reader.set_limit(len as u64);
    let mut body = Vec::with_capacity(len);
    reader
        .read_to_end(&mut body)
        .map_err(|_| "400 Bad Request")?;
    if body.len() != len {
        return Err("400 Bad Request"); // the client hung up early
    }
    Ok(body)
}

fn respond(mut stream: &TcpStream, status: &str, extra_headers: &[&str], body: &str) {
    let mut head = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if !body.is_empty() {
        head.push_str("Content-Type: application/json\r\n");
    }
    for h in extra_headers {
        head.push_str(h);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.flush();
}

/// Close a connection answered before its request was read in full (a
/// refused or malformed one). Closing a socket with unread bytes makes the
/// OS reset the connection, and a client still sending its body can then
/// lose the response just written. So half-close, and discard what the
/// client still sends for a moment, within a small byte and time budget so a
/// refused client can't hold the connection.
fn close_unread(mut stream: &TcpStream) {
    let _ = stream.shutdown(Shutdown::Write);
    let until = Instant::now() + LINGER;
    let mut left = MAX_HEAD as usize;
    let mut buf = [0u8; 8192];
    while left > 0 {
        let Some(wait) = until
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
        else {
            break;
        };
        if stream.set_read_timeout(Some(wait)).is_err() {
            break;
        }
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => left = left.saturating_sub(n),
        }
    }
}

/// `localhost[:port]`, `127.0.0.1[:port]`, or `[::1][:port]`.
fn is_local_host(host: &str) -> bool {
    let name = if let Some(rest) = host.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        host.split(':').next().unwrap_or("")
    };
    matches!(name, "localhost" | "127.0.0.1" | "::1")
}

/// Constant-time token comparison (no early exit on the first differing byte).
fn token_matches(given: &str, expected: &str) -> bool {
    given.len() == expected.len()
        && given
            .bytes()
            .zip(expected.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

/// Why an HTTP request is refused before it reaches the protocol, if it is.
fn reject(req: &Request, token: &str) -> Option<(&'static str, &'static str)> {
    if req.path != "/mcp" && req.path != "/" {
        return Some(("404 Not Found", ""));
    }
    if req.method != "POST" {
        // No server-initiated SSE stream; POST is the whole protocol here.
        return Some(("405 Method Not Allowed", "Allow: POST"));
    }
    if !req.header("host").is_some_and(is_local_host) {
        return Some(("403 Forbidden", ""));
    }
    if let Some(origin) = req.header("origin") {
        let host = origin
            .strip_prefix("http://")
            .or_else(|| origin.strip_prefix("https://"))
            .unwrap_or("");
        if !is_local_host(host) {
            return Some(("403 Forbidden", ""));
        }
    }
    // The auth scheme name is case-insensitive (RFC 9110 §11.1).
    let bearer = req
        .header("authorization")
        .and_then(|v| v.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, t)| t);
    if !bearer.is_some_and(|t| token_matches(t.trim(), token)) {
        return Some(("401 Unauthorized", "WWW-Authenticate: Bearer"));
    }
    None
}

fn serve_connection(stream: TcpStream, token: &str, backend: &dyn Backend) {
    let stream = &stream;
    let Some((req, body_reader)) = read_head(stream) else {
        respond(stream, "400 Bad Request", &[], "");
        close_unread(stream);
        return;
    };
    // Authenticate on the head alone: nothing unauthenticated gets to make
    // us allocate or wait for a body.
    if let Some((status, header)) = reject(&req, token) {
        if status.starts_with("401") || status.starts_with("403") {
            backend.log(format!("refused request: {status}"));
        }
        let headers: Vec<&str> = if header.is_empty() {
            vec![]
        } else {
            vec![header]
        };
        respond(stream, status, &headers, "");
        close_unread(stream);
        return;
    }
    let body = match read_body(&req, body_reader) {
        Ok(body) => body,
        Err(status) => {
            respond(stream, status, &[], "");
            close_unread(stream);
            return;
        }
    };
    let message: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            let body = rpc_error(Value::Null, -32700, &format!("parse error: {e}"));
            respond(stream, "400 Bad Request", &[], &body.to_string());
            return;
        }
    };
    // A batch (2025-03-26) is answered with an array of the responses.
    let reply = match &message {
        Value::Array(items) => {
            let out: Vec<Value> = items
                .iter()
                .filter_map(|m| handle_message(m, backend))
                .collect();
            (!out.is_empty()).then(|| Value::Array(out))
        }
        _ => handle_message(&message, backend),
    };
    match reply {
        Some(body) => respond(stream, "200 OK", &[], &body.to_string()),
        // Notifications and responses get no body.
        None => respond(stream, "202 Accepted", &[], ""),
    }
}

/// Answer one JSON-RPC message; `None` for a notification (no `id`).
pub fn handle_message(message: &Value, backend: &dyn Backend) -> Option<Value> {
    let (id, method, params) = match Message::from(message.clone()) {
        Message::Request { id, method, params } => (id, method, params),
        // We never send requests, so a "response" is a request missing its method.
        Message::Response { id, .. } => {
            return Some(rpc_error(id, -32600, "invalid request: no method"));
        }
        Message::Notification { .. } | Message::Invalid => return None,
    };
    Some(match method.as_str() {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::as_str);
            let version = asked
                .filter(|v| PROTOCOL_VERSIONS.contains(v))
                .unwrap_or(PROTOCOL_VERSIONS[0]);
            backend.log(format!(
                "client connected: {}",
                params["clientInfo"]["name"].as_str().unwrap_or("unknown")
            ));
            rpc_result(
                id,
                json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": "macros", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": INSTRUCTIONS,
                }),
            )
        }
        "ping" => rpc_result(id, json!({})),
        "tools/list" => rpc_result(id, json!({"tools": tool_definitions()})),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match call_tool(name, &args, backend) {
                Some((text, is_error)) => rpc_result(
                    id,
                    json!({"content": [{"type": "text", "text": text}], "isError": is_error}),
                ),
                None => rpc_error(id, -32602, &format!("unknown tool: {name}")),
            }
        }
        _ => rpc_error(id, -32601, &format!("method not found: {method}")),
    })
}

const INSTRUCTIONS: &str = "This server is a live Macros editor (an Emacs-style editor \
scripted in Steel Scheme). Use `eval` to run Steel in the running editor — define or \
redefine functions, call commands like (plot-demo), inspect state — then `buffer_text`, \
`list_buffers`, and `messages` to see the effect. `press_keys` types keys into the window \
the way a user would, to check what a binding really does. Before writing package code, call `api` \
for the categorized reference of every primitive (signatures, types, docs) and the bundled \
Scheme library; `describe` and `apropos` look up single names. Buffer and overlay positions \
are BYTE offsets.";

/// One MCP tool: what `tools/list` advertises and the handler `tools/call`
/// runs. [`TOOLS`] is the single list of them.
struct Tool {
    name: &'static str,
    description: &'static str,
    input_schema: fn() -> Value,
    /// Run with the call's `arguments`: `(text, is_error)`.
    run: fn(&Value, &dyn Backend) -> (String, bool),
}

/// Every tool, in the order `tools/list` reports them.
const TOOLS: &[Tool] = &[
    Tool {
        name: "eval",
        description: "Evaluate Steel Scheme code in the live editor and return the printed \
        value of the last form, any error, and echo-area messages it produced. Editor \
        commands it queues (insert, render-buffer, set-option, …) are applied. Can \
        redefine functions in a running package — the main package-development loop.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {"code": {"type": "string", "description": "Steel source; may hold several forms."}},
                "required": ["code"]
            })
        },
        run: eval_tool,
    },
    Tool {
        name: "buffer_text",
        description: "A buffer's text plus its name, file, major mode, point and modified \
        flag. Defaults to the current buffer. Optionally a line range and the overlay \
        face spans (byte offsets) that color it. A grid (CSV/TSV/.xlsx) buffer shows its \
        cells instead: tab-separated rows under column letters (the line range picks \
        rows; 1000 by default; a row a filter hides is marked `(hidden)`), the cursor and \
        sheet, then its formulas, and with `faces` its hand-set formatting.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {
                    "buffer": {"type": "string", "description": "Buffer name; omit for the current buffer."},
                    "start_line": {"type": "integer", "minimum": 0, "description": "First line, 0-based."},
                    "end_line": {"type": "integer", "minimum": 0, "description": "End line, exclusive."},
                    "faces": {"type": "boolean", "description": "Include overlay face spans."}
                }
            })
        },
        run: buffer_text_tool,
    },
    Tool {
        name: "list_buffers",
        description: "Every open buffer with its major mode and file; the current buffer is marked with *.",
        input_schema: || json!({"type": "object", "properties": {}}),
        run: list_buffers_tool,
    },
    Tool {
        name: "messages",
        description: "Recent echo-area messages (the *Messages* log), oldest first. Errors from \
        async callbacks and hooks land here.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {"count": {"type": "integer", "minimum": 1, "description": "How many (default 50)."}}
            })
        },
        run: messages_tool,
    },
    Tool {
        name: "press_keys",
        description: "Press keys in the editor's window exactly as if typed: each keystroke \
        goes through the window's key dispatch (OS-key actions like RET/DEL/arrows, then \
        the editor's modal layers and keymaps, then text input), and the editor settles \
        (Scheme commands finish) before the next one. Returns the modal state, point, the \
        current line, any pending prefix, an open minibuffer or Helm prompt, and the \
        messages the keys echoed. Notation: space-separated keys, Emacs-style (`C-x C-f`, \
        `M-x`, `C-M-a`, `s-v` for Cmd, `RET`, `SPC`, `TAB`, `ESC`, `DEL`, `<left>`, `<f1>`) \
        or gpui-style (`ctrl-x`, `alt-f`, `cmd-v`, `shift-tab`, `enter`, `escape`). A \
        single character is that key (`A` is shift-a). A double-quoted string types each \
        of its characters (`\"hello world\"`). Keys go wherever the window's focus is, \
        as a user's would.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {"keys": {"type": "string", "description": "Keys to press, e.g. \"C-x b\" or \"i \\\"hi\\\" ESC d d\"."}},
                "required": ["keys"]
            })
        },
        run: press_keys_tool,
    },
    Tool {
        name: "describe",
        description: "Documentation for a Scheme function, command, or editor option by name.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {"name": {"type": "string"}},
                "required": ["name"]
            })
        },
        run: describe_tool,
    },
    Tool {
        name: "api",
        description: "The package-API reference. No arguments: an index of every Rust \
        primitive category (buffers, overlays, processes, http, sockets, sqlite, git, …) \
        and bundled Scheme library module. `category` (e.g. \"sockets\" or \"lib/core\"): \
        each function's signature with argument/return types and its doc. `search`: words \
        matched against names, signatures and docs (within `category` if given). Use it to \
        find what to build a package on before reading source.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {
                    "category": {"type": "string", "description": "A category from the index; omit for the index or to search everything."},
                    "search": {"type": "string", "description": "Space-separated words; all must match."}
                }
            })
        },
        run: api_tool,
    },
    Tool {
        name: "apropos",
        description: "Search function, command, and option names and their documentation. \
        Every space-separated word must match.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {"pattern": {"type": "string"}},
                "required": ["pattern"]
            })
        },
        run: apropos_tool,
    },
];

/// The tool names, in [`TOOLS`] order (for the `mcp-server-info` summary).
pub fn tool_names() -> impl Iterator<Item = &'static str> {
    TOOLS.iter().map(|t| t.name)
}

fn tool_definitions() -> Value {
    TOOLS
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "inputSchema": (t.input_schema)(),
            })
        })
        .collect()
}

/// Run a tool: `(text, is_error)`, or `None` for an unknown tool name.
fn call_tool(name: &str, args: &Value, backend: &dyn Backend) -> Option<(String, bool)> {
    TOOLS
        .iter()
        .find(|t| t.name == name)
        .map(|t| (t.run)(args, backend))
}

/// A tool's `arguments` as `T`. Lenient like the protocol's clients expect:
/// a missing or ill-typed argument is just absent (see [`lenient`]), and
/// anything but an object means no arguments at all.
fn parse_args<T: DeserializeOwned + Default>(args: &Value) -> T {
    if args.is_object() {
        T::deserialize(args).unwrap_or_default()
    } else {
        T::default()
    }
}

/// A backend result as a tool reply.
fn reply(result: Result<String, String>) -> (String, bool) {
    match result {
        Ok(text) => (text, false),
        Err(e) => (e, true),
    }
}

fn missing(argument: &str) -> (String, bool) {
    (format!("missing argument: {argument}"), true)
}

#[derive(Deserialize, Default)]
struct EvalArgs {
    #[serde(default, deserialize_with = "lenient")]
    code: Option<String>,
}

fn eval_tool(args: &Value, backend: &dyn Backend) -> (String, bool) {
    let Some(code) = parse_args::<EvalArgs>(args).code else {
        return missing("code");
    };
    backend.log(format!("eval: {}", one_line(&code, 120)));
    format_eval(&backend.eval(&code))
}

#[derive(Deserialize, Default)]
struct BufferTextArgs {
    #[serde(default, deserialize_with = "lenient")]
    buffer: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    start_line: Option<u64>,
    #[serde(default, deserialize_with = "lenient")]
    end_line: Option<u64>,
    #[serde(default, deserialize_with = "lenient")]
    faces: Option<bool>,
}

fn buffer_text_tool(args: &Value, backend: &dyn Backend) -> (String, bool) {
    let args: BufferTextArgs = parse_args(args);
    let lines = match (args.start_line, args.end_line) {
        (None, None) => None,
        (s, e) => Some((
            s.map_or(0, |n| n as usize),
            e.map_or(usize::MAX, |n| n as usize),
        )),
    };
    reply(backend.buffer_text(args.buffer.as_deref(), lines, args.faces.unwrap_or(false)))
}

fn list_buffers_tool(_args: &Value, backend: &dyn Backend) -> (String, bool) {
    reply(backend.list_buffers())
}

#[derive(Deserialize, Default)]
struct MessagesArgs {
    #[serde(default, deserialize_with = "lenient")]
    count: Option<u64>,
}

fn messages_tool(args: &Value, backend: &dyn Backend) -> (String, bool) {
    let count = parse_args::<MessagesArgs>(args)
        .count
        .map_or(50, |n| n as usize);
    reply(backend.messages(count.clamp(1, 10_000)))
}

#[derive(Deserialize, Default)]
struct PressKeysArgs {
    #[serde(default, deserialize_with = "lenient")]
    keys: Option<String>,
}

fn press_keys_tool(args: &Value, backend: &dyn Backend) -> (String, bool) {
    let Some(keys) = parse_args::<PressKeysArgs>(args).keys else {
        return missing("keys");
    };
    backend.log(format!("press_keys: {}", one_line(&keys, 120)));
    reply(backend.press_keys(&keys))
}

#[derive(Deserialize, Default)]
struct DescribeArgs {
    #[serde(default, deserialize_with = "lenient")]
    name: Option<String>,
}

fn describe_tool(args: &Value, backend: &dyn Backend) -> (String, bool) {
    let Some(name) = parse_args::<DescribeArgs>(args).name else {
        return missing("name");
    };
    let s = scheme_string(&name);
    reply(scheme_text(
        backend,
        &format!(
            "(if (member {s} (option-list))
                 (string-append {s} \"\\n\\nEditor option. Current value: \"
                                (option-display (get-option {s})))
                 (let ((doc (function-doc {s})))
                   (string-append {s} \"\\n\\n\"
                                  (if (equal? doc \"\") \"(no documentation)\" doc))))"
        ),
    ))
}

#[derive(Deserialize, Default)]
struct ApiArgs {
    #[serde(default, deserialize_with = "lenient")]
    category: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    search: Option<String>,
}

fn api_tool(args: &Value, backend: &dyn Backend) -> (String, bool) {
    let args: ApiArgs = parse_args(args);
    reply(scheme_text(
        backend,
        &format!(
            "(api-reference {} {})",
            scheme_string(args.category.as_deref().unwrap_or("")),
            scheme_string(args.search.as_deref().unwrap_or(""))
        ),
    ))
}

#[derive(Deserialize, Default)]
struct AproposArgs {
    #[serde(default, deserialize_with = "lenient")]
    pattern: Option<String>,
}

fn apropos_tool(args: &Value, backend: &dyn Backend) -> (String, bool) {
    let Some(pattern) = parse_args::<AproposArgs>(args).pattern else {
        return missing("pattern");
    };
    reply(scheme_text(
        backend,
        &format!(
            "(let ((m (apropos-matches {}))) (if (null? m) \"no matches\" (join-lines (map apropos-row m))))",
            scheme_string(&pattern)
        ),
    ))
}

/// Evaluate a Scheme expression that returns a string, and unquote it.
fn scheme_text(backend: &dyn Backend, expr: &str) -> Result<String, String> {
    let report = backend.eval(expr);
    if let Some(e) = report.error {
        return Err(e);
    }
    Ok(report.value.as_deref().map(unquote).unwrap_or_default())
}

fn format_eval(report: &EvalReport) -> (String, bool) {
    let mut out = String::new();
    match (&report.error, &report.value) {
        (Some(e), _) => out.push_str(&format!("error: {e}")),
        (None, Some(v)) => out.push_str(&format!("=> {v}")),
        (None, None) => out.push_str("=> (no value)"),
    }
    if report.commands > 0 {
        out.push_str(&format!(
            "\n({} editor command(s) applied)",
            report.commands
        ));
    }
    if !report.messages.is_empty() {
        out.push_str("\nmessages:");
        for m in &report.messages {
            out.push_str("\n  ");
            out.push_str(m);
        }
    }
    (out, report.error.is_some())
}

/// A Rust string as a Steel string literal.
fn scheme_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The inverse of Steel's printed string form (`"a\nb"` → `a` newline `b`);
/// anything that isn't a quoted string comes back unchanged.
fn unquote(printed: &str) -> String {
    let Some(inner) = printed.strip_prefix('"').and_then(|s| s.strip_suffix('"')) else {
        return printed.to_string();
    };
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// First line of `s`, cut to `max` chars, for log lines.
fn one_line(s: &str, max: usize) -> String {
    let first = s.trim().lines().next().unwrap_or("");
    let cut: String = first.chars().take(max).collect();
    if cut.len() < s.trim().len() {
        format!("{cut} …")
    } else {
        cut
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// A fake editor: eval echoes the code back as a value; logs are kept.
    #[derive(Default)]
    struct Fake {
        logs: Mutex<Vec<String>>,
    }

    impl Backend for Fake {
        fn eval(&self, code: &str) -> EvalReport {
            if code.contains("(boom)") {
                return EvalReport {
                    error: Some("boom".into()),
                    ..Default::default()
                };
            }
            if code.contains("api-reference") {
                return EvalReport {
                    value: Some(format!("{code:?}")),
                    ..Default::default()
                };
            }
            if code.contains("function-doc") {
                return EvalReport {
                    value: Some("\"insert\\n\\nInsert text.\"".into()),
                    ..Default::default()
                };
            }
            EvalReport {
                value: Some(code.to_string()),
                messages: vec!["hello".into()],
                commands: 2,
                ..Default::default()
            }
        }
        fn buffer_text(
            &self,
            name: Option<&str>,
            lines: Option<(usize, usize)>,
            faces: bool,
        ) -> Result<String, String> {
            Ok(format!("{name:?} {lines:?} {faces}"))
        }
        fn list_buffers(&self) -> Result<String, String> {
            Ok("* *scratch*".into())
        }
        fn messages(&self, count: usize) -> Result<String, String> {
            Ok(format!("{count} messages"))
        }
        fn press_keys(&self, keys: &str) -> Result<String, String> {
            if keys == "bogus" {
                return Err("unknown key: bogus".into());
            }
            Ok(format!("pressed {keys}"))
        }
        fn log(&self, line: String) {
            self.logs.lock().unwrap().push(line);
        }
    }

    fn call(fake: &Fake, method: &str, params: Value) -> Value {
        handle_message(
            &json!({"jsonrpc": "2.0", "id": 7, "method": method, "params": params}),
            fake,
        )
        .expect("a request gets a response")
    }

    #[test]
    fn initialize_negotiates_version_and_lists_tools() {
        let fake = Fake::default();
        let r = call(
            &fake,
            "initialize",
            json!({"protocolVersion": "2025-03-26", "clientInfo": {"name": "claude-code"}}),
        );
        assert_eq!(r["id"], 7);
        assert_eq!(r["result"]["protocolVersion"], "2025-03-26");
        assert!(r["result"]["capabilities"]["tools"].is_object());
        let r = call(
            &fake,
            "initialize",
            json!({"protocolVersion": "1999-01-01"}),
        );
        assert_eq!(r["result"]["protocolVersion"], PROTOCOL_VERSIONS[0]);
        assert_eq!(
            fake.logs.lock().unwrap()[0],
            "client connected: claude-code"
        );

        let tools = call(&fake, "tools/list", json!({}));
        let names: Vec<&str> = tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "eval",
                "buffer_text",
                "list_buffers",
                "messages",
                "press_keys",
                "describe",
                "api",
                "apropos"
            ]
        );
    }

    #[test]
    fn notifications_get_no_response_and_unknowns_error() {
        let fake = Fake::default();
        assert!(
            handle_message(
                &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
                &fake
            )
            .is_none()
        );
        assert_eq!(call(&fake, "bogus", json!({}))["error"]["code"], -32601);
        let r = call(&fake, "tools/call", json!({"name": "nope"}));
        assert_eq!(r["error"]["code"], -32602);
    }

    #[test]
    fn tools_route_to_the_backend() {
        let fake = Fake::default();
        let text = |r: &Value| {
            r["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .to_string()
        };

        let r = call(
            &fake,
            "tools/call",
            json!({"name": "eval", "arguments": {"code": "(+ 1 2)"}}),
        );
        assert_eq!(
            text(&r),
            "=> (+ 1 2)\n(2 editor command(s) applied)\nmessages:\n  hello"
        );
        assert_eq!(r["result"]["isError"], false);

        let r = call(
            &fake,
            "tools/call",
            json!({"name": "eval", "arguments": {"code": "(boom)"}}),
        );
        assert_eq!(
            (text(&r).as_str(), &r["result"]["isError"]),
            ("error: boom", &json!(true))
        );

        let r = call(
            &fake,
            "tools/call",
            json!({"name": "buffer_text", "arguments": {"buffer": "a", "end_line": 5, "faces": true}}),
        );
        assert_eq!(text(&r), "Some(\"a\") Some((0, 5)) true");

        let r = call(
            &fake,
            "tools/call",
            json!({"name": "press_keys", "arguments": {"keys": "C-x b"}}),
        );
        assert_eq!(text(&r), "pressed C-x b");
        assert!(
            fake.logs
                .lock()
                .unwrap()
                .contains(&"press_keys: C-x b".to_string())
        );
        let r = call(
            &fake,
            "tools/call",
            json!({"name": "press_keys", "arguments": {"keys": "bogus"}}),
        );
        assert_eq!(
            (text(&r).as_str(), &r["result"]["isError"]),
            ("unknown key: bogus", &json!(true))
        );
        let r = call(&fake, "tools/call", json!({"name": "press_keys"}));
        assert_eq!(r["result"]["isError"], true);

        let r = call(
            &fake,
            "tools/call",
            json!({"name": "describe", "arguments": {"name": "insert"}}),
        );
        assert_eq!(text(&r), "insert\n\nInsert text.");

        let r = call(
            &fake,
            "tools/call",
            json!({"name": "api", "arguments": {"category": "sockets"}}),
        );
        assert_eq!(text(&r), "(api-reference \"sockets\" \"\")");
        let r = call(&fake, "tools/call", json!({"name": "api"}));
        assert_eq!(text(&r), "(api-reference \"\" \"\")");

        let r = call(
            &fake,
            "tools/call",
            json!({"name": "eval", "arguments": {}}),
        );
        assert_eq!(r["result"]["isError"], true);
    }

    #[test]
    fn scheme_strings_round_trip() {
        let s = "say \"hi\"\\\n\tok";
        assert_eq!(unquote(&scheme_string(s)), s);
        assert_eq!(unquote("42"), "42");
    }

    #[test]
    fn local_host_and_token_checks() {
        assert!(
            is_local_host("127.0.0.1:7811")
                && is_local_host("localhost")
                && is_local_host("[::1]:80")
        );
        assert!(!is_local_host("evil.example:7811") && !is_local_host("127.0.0.1.evil.example"));
        assert!(
            token_matches("abc", "abc")
                && !token_matches("abd", "abc")
                && !token_matches("ab", "abc")
        );
    }

    /// Over real HTTP: auth, Host/Origin guards, and a round trip.
    #[test]
    fn http_transport_enforces_auth_and_origin() {
        let fake: Arc<dyn Backend> = Arc::new(Fake::default());
        let server = McpServer::start(0, "secret".into(), fake, false).unwrap();
        let port = server.port;
        let post = |headers: &str, body: &str| -> String {
            let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
            write!(
                s,
                "POST /mcp HTTP/1.1\r\nContent-Length: {}\r\n{headers}\r\n{body}",
                body.len()
            )
            .unwrap();
            let mut out = String::new();
            s.read_to_string(&mut out).unwrap();
            out
        };
        let ping = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
        let host = format!("Host: 127.0.0.1:{port}\r\n");

        assert!(post(&host, ping).starts_with("HTTP/1.1 401"));
        assert!(
            post(&format!("{host}Authorization: Bearer wrong\r\n"), ping)
                .starts_with("HTTP/1.1 401")
        );
        let ok = post(&format!("{host}Authorization: Bearer secret\r\n"), ping);
        assert!(
            ok.starts_with("HTTP/1.1 200")
                && ok.ends_with(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#),
            "{ok}"
        );
        // A web page (non-local Origin) or a rebound hostname is refused even with the token.
        let auth = "Authorization: Bearer secret\r\n";
        assert!(
            post(
                &format!("{host}{auth}Origin: https://evil.example\r\n"),
                ping
            )
            .starts_with("HTTP/1.1 403")
        );
        assert!(
            post(&format!("Host: evil.example:{port}\r\n{auth}"), ping).starts_with("HTTP/1.1 403")
        );
        // Notifications are accepted with no body.
        let note = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        assert!(post(&format!("{host}{auth}"), note).starts_with("HTTP/1.1 202"));
        // The auth scheme name is case-insensitive.
        assert!(
            post(&format!("{host}authorization: bearer secret\r\n"), ping)
                .starts_with("HTTP/1.1 200")
        );
        server.stop();
    }

    /// Send raw bytes, return the status line (empty if the server just hung up).
    fn raw(port: u16, request: &[u8]) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let _ = s.write_all(request);
        let mut out = Vec::new();
        let _ = s.read_to_end(&mut out);
        String::from_utf8_lossy(&out)
            .lines()
            .next()
            .unwrap_or("")
            .to_string()
    }

    /// Nothing unauthenticated can make the server buffer much: an endless
    /// header line hits the head cap, and a big body is refused on its
    /// headers alone (401 without waiting for the 8 MiB that never comes).
    #[test]
    fn http_limits_apply_before_auth() {
        let fake: Arc<dyn Backend> = Arc::new(Fake::default());
        let server = McpServer::start(0, "secret".into(), fake, false).unwrap();
        let port = server.port;
        let host = format!("Host: 127.0.0.1:{port}\r\n");

        let mut endless = b"POST /mcp HTTP/1.1\r\nX-Pad: ".to_vec();
        endless.extend(std::iter::repeat_n(b'a', (MAX_HEAD as usize) + 1024));
        assert_eq!(raw(port, &endless), "HTTP/1.1 400 Bad Request");

        let started = Instant::now();
        let big = format!("POST /mcp HTTP/1.1\r\n{host}Content-Length: 8000000\r\n\r\n");
        assert!(raw(port, big.as_bytes()).starts_with("HTTP/1.1 401"));
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "waited for the body"
        );

        let auth = "Authorization: Bearer secret\r\n";
        let no_len = format!("POST /mcp HTTP/1.1\r\n{host}{auth}\r\n");
        assert!(raw(port, no_len.as_bytes()).starts_with("HTTP/1.1 411"));
        let bad_len = format!("POST /mcp HTTP/1.1\r\n{host}{auth}Content-Length: -4\r\n\r\n");
        assert!(raw(port, bad_len.as_bytes()).starts_with("HTTP/1.1 400"));
        let too_big = format!("POST /mcp HTTP/1.1\r\n{host}{auth}Content-Length: 99999999\r\n\r\n");
        assert!(raw(port, too_big.as_bytes()).starts_with("HTTP/1.1 413"));
        let chunked =
            format!("POST /mcp HTTP/1.1\r\n{host}{auth}Transfer-Encoding: chunked\r\n\r\n");
        assert!(raw(port, chunked.as_bytes()).starts_with("HTTP/1.1 501"));
        server.stop();
    }
}
