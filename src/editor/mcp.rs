//! Editor side of the MCP server (src/mcp.rs): starting and stopping it from
//! the `mcp-server*` options, and answering its requests on the foreground.
//!
//! Server threads evaluate Scheme directly on the engine thread (so a long
//! `eval` never blocks the UI) and send everything that needs editor state
//! here as [`McpRequest`]s, each with a reply channel the thread blocks on.

use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::Duration;

use futures::StreamExt as _;
use gpui::{Context, Task};

use super::*;
use crate::mcp::{Backend, EvalReport, McpServer};

/// Work a server thread needs done on the editor's foreground.
pub(crate) enum McpRequest {
    /// Publish point/buffer to the engine snapshot before an eval reads it.
    PublishPoint(Sender<()>),
    /// Apply an eval's commands; reply with the messages they echoed.
    Apply(Vec<EditorCommand>, Sender<Vec<String>>),
    BufferText {
        name: Option<String>,
        lines: Option<(usize, usize)>,
        faces: bool,
        reply: Sender<Result<String, String>>,
    },
    ListBuffers(Sender<Result<String, String>>),
    Messages(usize, Sender<Result<String, String>>),
    Log(String),
}

/// A running server plus the foreground task draining its requests.
pub(crate) struct McpState {
    server: McpServer,
    token: String,
    _drain: Task<()>,
}

/// The [`Backend`] the server threads use: the engine directly, and the
/// editor through the request channel.
struct EditorBackend {
    engine: crate::script::ActionHandle,
    tx: futures::channel::mpsc::UnboundedSender<McpRequest>,
}

impl EditorBackend {
    /// Send a request to the foreground and wait for its reply. `None` if the
    /// editor is gone or wedged (30 s).
    fn ask<T>(&self, make: impl FnOnce(Sender<T>) -> McpRequest) -> Option<T> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.tx.unbounded_send(make(tx)).ok()?;
        rx.recv_timeout(Duration::from_secs(30)).ok()
    }
}

const EDITOR_GONE: &str = "the editor did not respond";

impl Backend for EditorBackend {
    fn eval(&self, code: &str) -> EvalReport {
        self.ask(McpRequest::PublishPoint);
        let (commands, value, mut error) = self.engine.eval_sync(code);
        let mut count = commands.len();
        let mut messages = Vec::new();
        if count > 0 {
            match self.ask(|tx| McpRequest::Apply(commands, tx)) {
                Some(applied) => messages = applied,
                None => {
                    // Timed out or the server was stopped mid-request: don't
                    // claim the commands took effect.
                    count = 0;
                    let note =
                        format!("{EDITOR_GONE}; its editor commands were not confirmed applied");
                    error = Some(match error {
                        Some(e) => format!("{e}\n{note}"),
                        None => note,
                    });
                }
            }
        }
        EvalReport {
            value,
            error,
            messages,
            commands: count,
        }
    }

    fn buffer_text(
        &self,
        name: Option<&str>,
        lines: Option<(usize, usize)>,
        faces: bool,
    ) -> Result<String, String> {
        self.ask(|reply| McpRequest::BufferText {
            name: name.map(str::to_string),
            lines,
            faces,
            reply,
        })
        .unwrap_or_else(|| Err(EDITOR_GONE.into()))
    }

    fn list_buffers(&self) -> Result<String, String> {
        self.ask(McpRequest::ListBuffers)
            .unwrap_or_else(|| Err(EDITOR_GONE.into()))
    }

    fn messages(&self, count: usize) -> Result<String, String> {
        self.ask(|tx| McpRequest::Messages(count, tx))
            .unwrap_or_else(|| Err(EDITOR_GONE.into()))
    }

    fn log(&self, line: String) {
        let _ = self.tx.unbounded_send(McpRequest::Log(line));
    }
}

/// Cap on the `*mcp-server*` request log.
const MCP_LOG_CAP: usize = 200;

/// The token file in the config directory: created on first start with a
/// random 256-bit token, owner-readable only, and reused after that so a
/// client's saved configuration keeps working across restarts.
fn load_or_create_token() -> Result<String, String> {
    let dir = crate::runtime::config_directory();
    let path = dir.join("mcp-token");
    if let Ok(token) = std::fs::read_to_string(&path) {
        let token = token.trim().to_string();
        if !token.is_empty() {
            // A copied or synced file may have lost its owner-only mode.
            restrict_to_owner(&path);
            return Ok(token);
        }
    }
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|e| format!("no randomness: {e}"))?;
    let token = hex::encode(bytes);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    write_private(&path, &token).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(token)
}

#[cfg(unix)]
fn write_private(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    // `mode` only applies when the file is created; tighten an existing one.
    restrict_to_owner(path);
    f.write_all(contents.as_bytes())
}

/// Make `path` readable and writable by its owner only (Unix; a no-op elsewhere,
/// where the config directory's ACLs apply).
fn restrict_to_owner(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(not(unix))]
fn write_private(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    std::fs::write(path, contents)
}

impl Editor {
    /// Start, stop, or restart the MCP server to match the `mcp-server`,
    /// `mcp-server-port`, and `mcp-server-token` options. Idempotent.
    pub(super) fn reconcile_mcp_server(&mut self, cx: &mut Context<Self>) {
        let settings = cx.global::<crate::settings::Settings>();
        let (enabled, port) = (settings.mcp_server(), settings.mcp_server_port());
        let configured_token = settings.mcp_server_token().map(str::to_string);
        if !enabled {
            if self.mcp.take().is_some() {
                self.mcp_log_line("server stopped".into());
                self.echo("MCP server stopped", cx);
            }
            return;
        }
        let token = match configured_token.map_or_else(load_or_create_token, Ok) {
            Ok(token) => token,
            Err(e) => {
                self.echo(format!("MCP server not started: {e}"), cx);
                return;
            }
        };
        if self
            .mcp
            .as_ref()
            .is_some_and(|m| m.server.port == port && m.token == token)
        {
            return;
        }
        // Restarting: stop the old server first so its port is free. Only
        // then is a brief AddrInUse expected (our listener closing).
        let restarting = self.mcp.take().is_some_and(|m| m.server.port == port);
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        let backend = Arc::new(EditorBackend {
            engine: self.script.action_handle(),
            tx,
        });
        match McpServer::start(port, token.clone(), backend, restarting) {
            Ok(server) => {
                let drain = cx.spawn(async move |this, cx| {
                    while let Some(request) = rx.next().await {
                        if this
                            .update(cx, |editor, cx| editor.on_mcp_request(request, cx))
                            .is_err()
                        {
                            break;
                        }
                    }
                });
                let url = format!("http://127.0.0.1:{}/mcp", server.port);
                self.mcp_log_line(format!("listening on {url}"));
                self.mcp = Some(McpState {
                    server,
                    token,
                    _drain: drain,
                });
                self.echo(format!("MCP server on {url} — M-x mcp-server-info"), cx);
            }
            Err(e) => self.echo(
                format!("MCP server could not listen on port {port}: {e}"),
                cx,
            ),
        }
    }

    fn mcp_log_line(&mut self, line: String) {
        let stamped = format!("{} {line}", chrono::Local::now().format("%H:%M:%S"));
        push_capped(&mut self.mcp_log, stamped.clone(), MCP_LOG_CAP);
        self.append_to_log_buffer("*mcp-server*", &stamped);
    }

    fn on_mcp_request(&mut self, request: McpRequest, cx: &mut Context<Self>) {
        match request {
            McpRequest::PublishPoint(reply) => {
                self.publish_point();
                let _ = reply.send(());
            }
            McpRequest::Apply(commands, reply) => {
                // Capture what the batch echoes: swap the message log out,
                // apply, and splice the new tail back in.
                let before = std::mem::take(&mut self.messages);
                self.apply_script_outcome(
                    ScriptOutcome::Commands {
                        commands,
                        message: None,
                    },
                    cx,
                );
                let new = std::mem::replace(&mut self.messages, before);
                for m in &new {
                    push_capped(&mut self.messages, m.clone(), MESSAGES_CAP);
                }
                let _ = reply.send(new);
            }
            McpRequest::BufferText {
                name,
                lines,
                faces,
                reply,
            } => {
                let _ = reply.send(self.mcp_buffer_text(name.as_deref(), lines, faces));
            }
            McpRequest::ListBuffers(reply) => {
                let mut out = Vec::new();
                for (doc, current) in std::iter::once((&self.active, true))
                    .chain(self.inactive.iter().map(|d| (d, false)))
                {
                    out.push(format!(
                        "{} {}  [{}]{}",
                        if current { "*" } else { " " },
                        doc.name,
                        doc.modes.names().join(" "),
                        doc.path
                            .as_deref()
                            .map(|p| format!("  {}", p.display()))
                            .unwrap_or_default(),
                    ));
                }
                let _ = reply.send(Ok(out.join("\n")));
            }
            McpRequest::Messages(count, reply) => {
                let start = self.messages.len().saturating_sub(count);
                let text = self.messages[start..].join("\n");
                let _ = reply.send(Ok(if text.is_empty() {
                    "(no messages)".into()
                } else {
                    text
                }));
            }
            McpRequest::Log(line) => self.mcp_log_line(line),
        }
    }

    /// `buffer_text`: a header describing the buffer, then its text (or a
    /// line range of it), then optionally the overlay face spans.
    fn mcp_buffer_text(
        &self,
        name: Option<&str>,
        lines: Option<(usize, usize)>,
        faces: bool,
    ) -> Result<String, String> {
        let (doc, current) = match name {
            None => (&self.active, true),
            Some(n) if n == self.active.name => (&self.active, true),
            Some(n) => (
                self.inactive
                    .iter()
                    .find(|d| d.name == n)
                    .ok_or_else(|| format!("no buffer named {n:?}"))?,
                false,
            ),
        };
        let buffer = &doc.buffer;
        let total = buffer.line_count();
        let mut out = format!(
            "buffer: {}\nmode: {}\nfile: {}\nlines: {total}  bytes: {}  modified: {}\n",
            doc.name,
            doc.modes.names().join(" "),
            doc.path
                .as_deref()
                .map_or("(none)".into(), |p| p.display().to_string()),
            buffer.len(),
            doc.modified,
        );
        if current {
            let point = self.point();
            let (line, col) = buffer.byte_to_position(point);
            out.push_str(&format!(
                "point: byte {point} (line {line}, column {col}, 0-based)\n"
            ));
        }
        let text = buffer.full_text();
        let (from, to) = match lines {
            None => (0, total),
            Some((s, e)) => (s.min(total), e.min(total).max(s.min(total))),
        };
        let byte_range = if from >= total {
            text.len()..text.len()
        } else {
            let start = buffer.line_range(from).start;
            let end = if to >= total {
                text.len()
            } else {
                buffer.line_range(to).start
            };
            start..end
        };
        if lines.is_some() {
            out.push_str(&format!(
                "showing lines {from}..{to} (bytes {}..{})\n",
                byte_range.start, byte_range.end
            ));
        }
        out.push_str("---\n");
        out.push_str(&text[byte_range.clone()]);
        if faces {
            out.push_str("\n--- faces (start end face, absolute byte offsets)\n");
            for (r, f) in &doc.overlays.faces {
                if r.end > byte_range.start && r.start < byte_range.end {
                    out.push_str(&format!("{} {} {f}\n", r.start, r.end));
                }
            }
        }
        Ok(out)
    }

    /// `M-x mcp-server-info`: how to connect, plus the request log.
    pub(super) fn show_mcp_server_info(&mut self, cx: &mut Context<Self>) {
        let mut text = String::from("MCP server — for debugging and package development\n\n");
        match &self.mcp {
            Some(m) => {
                let url = format!("http://127.0.0.1:{}/mcp", m.server.port);
                text.push_str(&format!(
                    "Status: listening on {url}\n\n\
                     Connect Claude Code:\n\n  \
                     claude mcp add --transport http macros {url} \\\n    \
                     --header \"Authorization: Bearer {}\"\n\n\
                     Tools: eval, buffer_text, list_buffers, messages, describe, apropos.\n\
                     `eval` runs code in this editor, so the token is required on every\n\
                     request and only local connections are accepted.\n\n\
                     Stop with M-x mcp-server-stop or (set-option \"mcp-server\" #f).\n",
                    m.token
                ));
            }
            None => text.push_str(
                "Status: off (the default)\n\n\
                 Start it with M-x mcp-server-start, or in init.scm:\n\n  \
                 (set-option \"mcp-server\" #t)\n  \
                 (set-option \"mcp-server-port\" 7811)     ; optional, the default\n  \
                 (set-option \"mcp-server-token\" \"...\")  ; optional; else generated\n\n\
                 The generated token is kept in mcp-token in the config directory.\n",
            ),
        }
        text.push_str("\nLog:\n");
        text.push_str(&self.mcp_log.join("\n"));
        text.push('\n');
        self.apply_command(
            EditorCommand::RenderBuffer {
                name: "*mcp-server*".into(),
                mode: "help-mode".into(),
                text,
                line_faces: Vec::new(),
            },
            cx,
        );
    }
}
