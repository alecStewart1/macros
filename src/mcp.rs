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
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{Value, json};

/// Protocol revisions we speak, newest first. A client asking for one of
/// these gets it back; anything else gets the newest.
const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// Largest request body accepted (an `eval` of a whole package file fits).
const MAX_BODY: usize = 8 << 20;

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
    /// Record one line in the editor's `*mcp-log*` buffer.
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
    /// Bind 127.0.0.1:`port` and serve on a background thread.
    pub fn start(
        port: u16,
        token: String,
        backend: Arc<dyn Backend>,
    ) -> std::io::Result<McpServer> {
        // A restart on the same port races the old listener's thread
        // closing it; give that a moment rather than failing outright.
        let mut attempt = 0;
        let listener = loop {
            match TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
                Err(e) if e.kind() == std::io::ErrorKind::AddrInUse && attempt < 20 => {
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
        std::thread::Builder::new()
            .name("mcp-server".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    if flag.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(stream) = stream else { continue };
                    let (backend, token) = (backend.clone(), token.clone());
                    // One thread per connection: a long `eval` mustn't block
                    // a concurrent `messages` call.
                    let _ = std::thread::Builder::new()
                        .name("mcp-conn".into())
                        .spawn(move || serve_connection(stream, &token, backend.as_ref()));
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

/// A parsed HTTP request: method, path, lowercase header names, body.
struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

fn read_request(stream: &TcpStream) -> Option<Request> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let mut headers = Vec::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
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
    let len = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(0);
    if len > MAX_BODY {
        return None;
    }
    let mut body = vec![0; len];
    reader.read_exact(&mut body).ok()?;
    Some(Request {
        method,
        path,
        headers,
        body,
    })
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
    let bearer = req
        .header("authorization")
        .and_then(|v| v.strip_prefix("Bearer "));
    if !bearer.is_some_and(|t| token_matches(t.trim(), token)) {
        return Some(("401 Unauthorized", "WWW-Authenticate: Bearer"));
    }
    None
}

fn serve_connection(stream: TcpStream, token: &str, backend: &dyn Backend) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let Some(req) = read_request(&stream) else {
        respond(&stream, "400 Bad Request", &[], "");
        return;
    };
    if let Some((status, header)) = reject(&req, token) {
        if status.starts_with("401") || status.starts_with("403") {
            backend.log(format!("refused request: {status}"));
        }
        let headers: Vec<&str> = if header.is_empty() {
            vec![]
        } else {
            vec![header]
        };
        respond(&stream, status, &headers, "");
        return;
    }
    let message: Value = match serde_json::from_slice(&req.body) {
        Ok(v) => v,
        Err(e) => {
            let body = rpc_error(Value::Null, -32700, &format!("parse error: {e}"));
            respond(&stream, "400 Bad Request", &[], &body.to_string());
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
        Some(body) => respond(&stream, "200 OK", &[], &body.to_string()),
        // Notifications and responses get no body.
        None => respond(&stream, "202 Accepted", &[], ""),
    }
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

/// Answer one JSON-RPC message; `None` for a notification (no `id`).
pub fn handle_message(message: &Value, backend: &dyn Backend) -> Option<Value> {
    let id = message.get("id")?.clone();
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return Some(rpc_error(id, -32600, "invalid request: no method"));
    };
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    Some(match method {
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
`list_buffers`, and `messages` to see the effect. `describe` and `apropos` look up the \
editor's Scheme API. Buffer and overlay positions are BYTE offsets.";

fn tool_definitions() -> Value {
    json!([
        {
            "name": "eval",
            "description": "Evaluate Steel Scheme code in the live editor and return the printed \
                value of the last form, any error, and echo-area messages it produced. Editor \
                commands it queues (insert, render-buffer, set-option, …) are applied. Can \
                redefine functions in a running package — the main package-development loop.",
            "inputSchema": {
                "type": "object",
                "properties": {"code": {"type": "string", "description": "Steel source; may hold several forms."}},
                "required": ["code"]
            }
        },
        {
            "name": "buffer_text",
            "description": "A buffer's text plus its name, file, major mode, point and modified \
                flag. Defaults to the current buffer. Optionally a line range and the overlay \
                face spans (byte offsets) that color it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "buffer": {"type": "string", "description": "Buffer name; omit for the current buffer."},
                    "start_line": {"type": "integer", "minimum": 0, "description": "First line, 0-based."},
                    "end_line": {"type": "integer", "minimum": 0, "description": "End line, exclusive."},
                    "faces": {"type": "boolean", "description": "Include overlay face spans."}
                }
            }
        },
        {
            "name": "list_buffers",
            "description": "Every open buffer with its major mode and file; the current buffer is marked with *.",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "messages",
            "description": "Recent echo-area messages (the *Messages* log), oldest first. Errors from \
                async callbacks and hooks land here.",
            "inputSchema": {
                "type": "object",
                "properties": {"count": {"type": "integer", "minimum": 1, "description": "How many (default 50)."}}
            }
        },
        {
            "name": "describe",
            "description": "Documentation for a Scheme function, command, or editor option by name.",
            "inputSchema": {
                "type": "object",
                "properties": {"name": {"type": "string"}},
                "required": ["name"]
            }
        },
        {
            "name": "apropos",
            "description": "Search function, command, and option names and their documentation. \
                Every space-separated word must match.",
            "inputSchema": {
                "type": "object",
                "properties": {"pattern": {"type": "string"}},
                "required": ["pattern"]
            }
        }
    ])
}

/// Run a tool: `(text, is_error)`, or `None` for an unknown tool name.
fn call_tool(name: &str, args: &Value, backend: &dyn Backend) -> Option<(String, bool)> {
    let str_arg = |key: &str| args.get(key).and_then(Value::as_str);
    let usize_arg = |key: &str| args.get(key).and_then(Value::as_u64).map(|n| n as usize);
    let result = match name {
        "eval" => {
            let Some(code) = str_arg("code") else {
                return Some(("missing argument: code".into(), true));
            };
            backend.log(format!("eval: {}", one_line(code, 120)));
            let report = backend.eval(code);
            return Some(format_eval(&report));
        }
        "buffer_text" => {
            let lines = match (usize_arg("start_line"), usize_arg("end_line")) {
                (None, None) => None,
                (s, e) => Some((s.unwrap_or(0), e.unwrap_or(usize::MAX))),
            };
            let faces = args.get("faces").and_then(Value::as_bool).unwrap_or(false);
            backend.buffer_text(str_arg("buffer"), lines, faces)
        }
        "list_buffers" => backend.list_buffers(),
        "messages" => backend.messages(usize_arg("count").unwrap_or(50).clamp(1, 10_000)),
        "describe" => {
            let Some(n) = str_arg("name") else {
                return Some(("missing argument: name".into(), true));
            };
            let s = scheme_string(n);
            scheme_text(
                backend,
                &format!(
                    "(if (member {s} (option-list))
                         (string-append {s} \"\\n\\nEditor option. Current value: \"
                                        (option-display (get-option {s})))
                         (let ((doc (function-doc {s})))
                           (string-append {s} \"\\n\\n\"
                                          (if (equal? doc \"\") \"(no documentation)\" doc))))"
                ),
            )
        }
        "apropos" => {
            let Some(p) = str_arg("pattern") else {
                return Some(("missing argument: pattern".into(), true));
            };
            scheme_text(
                backend,
                &format!(
                    "(let ((m (apropos-matches {}))) (if (null? m) \"no matches\" (join-lines (map apropos-row m))))",
                    scheme_string(p)
                ),
            )
        }
        _ => return None,
    };
    Some(match result {
        Ok(text) => (text, false),
        Err(e) => (e, true),
    })
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
                "describe",
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
            json!({"name": "describe", "arguments": {"name": "insert"}}),
        );
        assert_eq!(text(&r), "insert\n\nInsert text.");

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
        let server = McpServer::start(0, "secret".into(), fake).unwrap();
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
        server.stop();
    }
}
