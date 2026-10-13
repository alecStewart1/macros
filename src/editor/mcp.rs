//! Editor side of the MCP server (src/mcp.rs): starting and stopping it from
//! the `mcp-server*` options, and answering its requests on the foreground.
//!
//! Server threads evaluate Scheme directly on the engine thread (so a long
//! `eval` never blocks the UI) and send everything that needs editor state
//! here as [`McpRequest`]s, each with a reply channel the thread blocks on.

use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::Duration;

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
pub(super) const MCP_LOG_CAP: usize = 200;

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
        self.mcp_log.push(stamped.clone());
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
                let before = self.messages.take();
                self.apply_script_outcome(
                    ScriptOutcome::Commands {
                        commands,
                        message: None,
                    },
                    cx,
                );
                let new = std::mem::replace(&mut self.messages, before);
                for m in new.iter() {
                    self.messages.push(m.clone());
                }
                let _ = reply.send(new.to_vec());
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
                let text = self.messages.join_newest(count, "\n");
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
    /// line range of it), then optionally the overlay face spans. A grid
    /// shows its cells instead ([`grid_text`]).
    pub(super) fn mcp_buffer_text(
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
        if let Some(g) = doc.grid.as_deref() {
            // A grid holds its data in cells, not text.
            out.push_str(&grid_text(g, lines, faces));
            return Ok(out);
        }
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
                     Tools: eval, buffer_text, list_buffers, messages, describe, api, apropos.\n\
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

/// Rows `buffer_text` shows of a grid when no line range is given.
const GRID_TEXT_ROWS: usize = 1000;

/// A grid as `buffer_text` shows it: what it is and where the cursor is,
/// then its cells as tab-separated rows (`lines` picks rows, 0-based), each
/// led by its row number and under a row of column letters. Cells show what
/// the grid shows (a formula's result); the formulas follow, and with
/// `faces`, the cells formatted by hand.
fn grid_text(g: &super::grid::GridDoc, lines: Option<(usize, usize)>, faces: bool) -> String {
    use crate::grid::cellfmt::color_name;
    use crate::grid::view::{cell_name, col_name, range_name};
    let (rows, cols) = g.used();
    let mut out = format!(
        "grid: {rows} rows x {cols} columns, header row: {}\n",
        if g.wb.header(super::grid::SHEET) {
            "yes"
        } else {
            "no"
        }
    );
    if let Some(name) = g.xlsx_sheets.get(g.xlsx_sheet) {
        out.push_str(&format!(
            "sheet: {name} ({} of {}; all: {})\n",
            g.xlsx_sheet + 1,
            g.xlsx_sheets.len(),
            g.xlsx_sheets.join(", ")
        ));
    }
    let (cr, cc) = g.view.cursor;
    out.push_str(&format!(
        "cursor: {} (row {cr}, column {cc}, 0-based)",
        cell_name(cr, cc)
    ));
    if g.view.anchor.is_some() {
        out.push_str(&format!("  selection: {}", range_name(&g.selection())));
    }
    out.push('\n');
    if let Some(only) = g.view.rows.only() {
        out.push_str(&format!(
            "filtered: {} of {rows} rows shown\n",
            only.shown.len()
        ));
    }
    let (from, to) = match lines {
        Some((s, e)) => (s.min(rows), e.min(rows).max(s.min(rows))),
        None => (0, rows.min(GRID_TEXT_ROWS)),
    };
    if lines.is_some() || to < rows {
        out.push_str(&format!(
            "showing rows {from}..{to} of {rows} (start_line/end_line pick rows)\n"
        ));
    }
    // Tabs and newlines inside a cell would break the layout.
    let clean = |s: &str| {
        s.replace('\\', "\\\\")
            .replace('\t', "\\t")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
    };
    out.push_str("---\n");
    out.push_str(
        &(0..cols)
            .map(|c| format!("\t{}", col_name(c)))
            .collect::<String>(),
    );
    out.push('\n');
    let mut formulas = Vec::new();
    for r in from..to {
        out.push_str(&(r + 1).to_string());
        // A row a filter hides is listed, marked.
        if !g.row_shown(r) {
            out.push_str(" (hidden)");
        }
        for c in 0..cols {
            out.push('\t');
            out.push_str(&clean(g.cell(r, c)));
            if g.is_formula_at(r, c) {
                formulas.push((r, c));
            }
        }
        out.push('\n');
    }
    if !formulas.is_empty() {
        out.push_str("--- formulas (cell: formula)\n");
        for (r, c) in formulas {
            out.push_str(&format!(
                "{}: {}\n",
                cell_name(r, c),
                clean(&g.source(r, c))
            ));
        }
    }
    if faces && !g.formats.is_empty() {
        let mut cells: Vec<(usize, usize)> = g
            .formats
            .positions()
            .filter(|&(r, _)| (from..to).contains(&r))
            .collect();
        cells.sort_unstable();
        if !cells.is_empty() {
            out.push_str("--- formatting (cell: bold, text color, fill, number format)\n");
            for (r, c) in cells {
                let f = g.formats.get(r, c);
                let color = |i: Option<u8>| i.map_or("none", color_name);
                out.push_str(&format!(
                    "{}: {}, {}, {}, {}\n",
                    cell_name(r, c),
                    if f.bold { "bold" } else { "normal" },
                    color(f.text),
                    color(f.fill),
                    f.num
                        .map_or_else(|| "General".into(), |n| n.code().to_string())
                ));
            }
        }
    }
    out
}
