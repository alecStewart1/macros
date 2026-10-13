use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::ops::Range;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Sender, channel};

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use serde_json::{Value, json};
use steel::rvals::SteelVal;
use steel::steel_vm::engine::Engine;

use crate::util::HideConsole;

/// An LSP diagnostic severity. Declared worst-first so the derived `Ord`
/// makes "worst severity" an explicit `min()` (matching the wire encoding,
/// where 1 = error is the smallest value).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Severity {
    Error = 1,
    Warning = 2,
    Info = 3,
    Hint = 4,
}

impl Severity {
    /// From the wire integer (1–4). Anything else — including a missing field —
    /// is treated as an error, matching the old `unwrap_or(1)` default.
    pub fn from_wire(n: u64) -> Self {
        match n {
            2 => Severity::Warning,
            3 => Severity::Info,
            4 => Severity::Hint,
            _ => Severity::Error,
        }
    }

    /// The lowercase name Scheme sees (`error`, `warning`, `info`, `hint`).
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
            Severity::Hint => "hint",
        }
    }
}

/// A diagnostic from the language server, already mapped to buffer byte offsets.
#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub range: Range<usize>,
    pub severity: Severity,
    pub message: String,
}

/// A minimal LSP client: one server process, JSON-RPC over stdio. A background
/// thread reads framed messages and forwards them as JSON `Value`s over a
/// channel; the editor drives requests/notifications from the main thread.
pub struct LspClient {
    /// The server process; killed and reaped on Drop (an `Option` only so
    /// Drop can move it to a reaper thread).
    child: Option<Child>,
    /// Outgoing framed messages, drained by a writer thread so the UI never
    /// blocks on a full stdin pipe (e.g. a large `didOpen` to a busy server).
    outgoing: Sender<String>,
    next_id: i64,
    root_uri: String,
}

impl LspClient {
    /// Spawn `command` rooted at `root`, returning the client and a receiver of
    /// incoming messages. `Err(reason)` if the process can't start — the reason
    /// is human-readable and, for a missing executable, names the program and
    /// the `PATH` it was searched in (the usual GUI-launch failure mode).
    pub fn start(command: &str, root: &Path) -> Result<(Self, UnboundedReceiver<Value>), String> {
        // The command may include arguments, e.g. "typescript-language-server --stdio".
        let mut parts = command.split_whitespace();
        let program = parts
            .next()
            .ok_or_else(|| "empty LSP command".to_string())?;
        // `Command::new` does no shell expansion, so expand a leading `~/` in the
        // program path ourselves — otherwise `~/.opam/…/ocaml_lsp` is looked up
        // as a literal path and fails with NotFound.
        let program = crate::util::expand_tilde(program);
        // A missing working directory makes `spawn` fail with `NotFound` too,
        // which would otherwise be misreported as the program missing from PATH.
        check_spawn_cwd(root)?;
        let mut cmd = Command::new(&program);
        cmd.args(parts)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .hide_console();
        // Its own process group, so dropping the client takes down whatever a
        // wrapper (`npx`, `sh -c`, `opam exec`) spawned too, not just the
        // wrapper (mirrors `make-process` and DAP adapters).
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(format!(
                    "`{program}` not found in PATH. \
                     If it's installed via nvm/pyenv/cargo/brew, the GUI launch may not see it — \
                     try (setenv \"PATH\" …) in init.scm. PATH={}",
                    std::env::var("PATH").unwrap_or_default(),
                ));
            }
            Err(e) => return Err(format!("spawning `{program}`: {e}")),
        };
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| "no stdin pipe".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "no stdout pipe".to_string())?;

        // Reader thread: framed messages → channel the editor drains async.
        let (tx, rx) = unbounded();
        // Forward the server's stderr as synthetic `$/stderr` notifications on the
        // same channel, so the editor can surface it in `*lsp-log*` (otherwise a
        // crash/startup failure is invisible).
        if let Some(stderr) = child.stderr.take() {
            let tx_err = tx.clone();
            std::thread::spawn(move || {
                for_each_stderr_line(stderr, |line| {
                    let msg = json!({ "method": "$/stderr", "params": { "line": line } });
                    tx_err.unbounded_send(msg).is_ok()
                });
            });
        }
        std::thread::spawn(move || read_loop(stdout, tx));

        // Writer thread: drains queued frames to stdin off the UI thread.
        let (outgoing, out_rx) = channel::<String>();
        std::thread::spawn(move || {
            while let Ok(frame) = out_rx.recv() {
                if stdin.write_all(frame.as_bytes()).is_err() {
                    break; // pipe closed
                }
                let _ = stdin.flush();
            }
        });

        let mut client = Self {
            child: Some(child),
            outgoing,
            next_id: 1,
            root_uri: path_to_uri(root),
        };
        client.send_initialize();
        Ok((client, rx))
    }

    fn send(&mut self, message: &Value) {
        let body = message.to_string();
        // Non-blocking: hand the framed message to the writer thread. If the
        // channel is gone the server has died; the editor drops the client.
        let _ = self
            .outgoing
            .send(format!("Content-Length: {}\r\n\r\n{body}", body.len()));
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    fn request(&mut self, method: &str, params: Value) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        id
    }

    fn send_initialize(&mut self) {
        let root = self.root_uri.clone();
        self.request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": root,
                "capabilities": client_capabilities(),
            }),
        );
    }

    /// Acknowledge the server's `initialize` response.
    pub fn initialized(&mut self) {
        self.notify("initialized", json!({}));
    }

    pub fn did_open(&mut self, uri: &str, language_id: &str, version: i32, text: &str) {
        self.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": language_id,
                    "version": version,
                    "text": text,
                }
            }),
        );
    }

    /// Full-document sync (used when the server only supports `Full`).
    pub fn did_change(&mut self, uri: &str, version: i32, text: &str) {
        self.notify(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": [ { "text": text } ]
            }),
        );
    }

    /// Incremental sync: one ranged change. `start`/`end` are pre-edit LSP
    /// positions `(line, utf16_col)`; `text` is the replacement.
    pub fn did_change_incremental(
        &mut self,
        uri: &str,
        version: i32,
        start: (usize, usize),
        end: (usize, usize),
        text: &str,
    ) {
        self.notify(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": [ {
                    "range": {
                        "start": { "line": start.0, "character": start.1 },
                        "end": { "line": end.0, "character": end.1 },
                    },
                    "text": text,
                } ]
            }),
        );
    }

    /// Request completions at a position; returns the request id to match the
    /// response.
    pub fn completion(&mut self, uri: &str, line: usize, character: usize) -> i64 {
        self.request(
            "textDocument/completion",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character }
            }),
        )
    }

    /// Send an arbitrary request, returning its id (the generic path the editor
    /// uses for navigation/rename/symbols/etc.).
    pub fn send_request(&mut self, method: &str, params: Value) -> i64 {
        self.request(method, params)
    }

    /// Reply to a server→client request with a null result (keeps servers that
    /// expect a response, e.g. `client/registerCapability`, unblocked).
    /// Reply to a server→client request with `result`.
    pub fn respond(&mut self, id: Value, result: Value) {
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));
    }

    /// Reply to a server→client request with a JSON-RPC error.
    pub fn respond_error(&mut self, id: Value, code: i64, message: &str) {
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message }
        }));
    }

    pub fn did_close(&mut self, uri: &str) {
        self.notify(
            "textDocument/didClose",
            json!({ "textDocument": { "uri": uri } }),
        );
    }

    /// `textDocument/didSave`; `text` is included only when the server asked.
    pub fn did_save(&mut self, uri: &str, text: Option<&str>) {
        let mut params = json!({ "textDocument": { "uri": uri } });
        if let Some(text) = text {
            params["text"] = json!(text);
        }
        self.notify("textDocument/didSave", params);
    }

    /// `$/cancelRequest` for an in-flight request we no longer want.
    pub fn cancel(&mut self, id: i64) {
        self.notify("$/cancelRequest", json!({ "id": id }));
    }
}

/// The `ClientCapabilities` we advertise in `initialize`. Everything here must
/// be backed by real handling: `didSave` is sent after each successful save,
/// `workspace/applyEdit` / `workspace/configuration` requests are answered, and
/// code actions arrive as literals (resolved lazily via `codeAction/resolve`
/// when the server leaves out the edit).
pub fn client_capabilities() -> Value {
    json!({
        "workspace": {
            "applyEdit": true,
            "configuration": true,
            "symbol": {},
            "workspaceEdit": { "documentChanges": true },
            "executeCommand": {}
        },
        "textDocument": {
            "synchronization": { "didSave": true, "willSave": false, "willSaveWaitUntil": false },
            "publishDiagnostics": {},
            "completion": {
                "completionItem": {
                    "snippetSupport": false,
                    "documentationFormat": ["plaintext", "markdown"],
                    "resolveSupport": {
                        "properties": ["documentation", "detail", "additionalTextEdits"]
                    }
                }
            },
            "hover": { "contentFormat": ["plaintext", "markdown"] },
            "definition": { "linkSupport": true },
            "declaration": { "linkSupport": true },
            "typeDefinition": { "linkSupport": true },
            "implementation": { "linkSupport": true },
            "references": {},
            "documentHighlight": {},
            "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
            "formatting": {},
            "rangeFormatting": {},
            "rename": { "prepareSupport": false },
            "signatureHelp": {},
            "codeAction": {
                "codeActionLiteralSupport": {
                    "codeActionKind": {
                        "valueSet": [
                            "", "quickfix", "refactor", "refactor.extract",
                            "refactor.inline", "refactor.rewrite", "source",
                            "source.organizeImports", "source.fixAll"
                        ]
                    }
                },
                "isPreferredSupport": true,
                "dataSupport": true,
                "resolveSupport": { "properties": ["edit"] }
            },
            "semanticTokens": {
                "requests": { "full": true },
                "tokenTypes": [
                    "namespace", "type", "class", "enum", "interface",
                    "struct", "typeParameter", "parameter", "variable",
                    "property", "enumMember", "event", "function",
                    "method", "macro", "keyword", "modifier", "comment",
                    "string", "number", "regexp", "operator", "decorator"
                ],
                "tokenModifiers": [],
                "formats": ["relative"]
            }
        }
    })
}

/// JSON-RPC `MethodNotFound`, for server→client requests we don't implement.
pub const METHOD_NOT_FOUND: i64 = -32601;

/// The reply to a server→client request that needs no editor state:
/// `Ok(result)` to answer, or `Err((code, message))` for an error response.
/// `workspace/applyEdit` is NOT handled here — it edits buffers, so the editor
/// answers it itself.
pub fn default_server_reply(method: &str, params: &Value) -> Result<Value, (i64, String)> {
    match method {
        // One `null` per requested item = "no client-side setting", which every
        // server treats as "use your defaults".
        "workspace/configuration" => {
            let n = params["items"].as_array().map_or(0, Vec::len);
            Ok(Value::Array(vec![Value::Null; n]))
        }
        // Void-result (or "no answer") requests: `null` is the correct reply.
        "window/workDoneProgress/create"
        | "client/registerCapability"
        | "client/unregisterCapability"
        | "window/showMessageRequest"
        | "workspace/workspaceFolders"
        | "workspace/semanticTokens/refresh"
        | "workspace/codeLens/refresh"
        | "workspace/inlayHint/refresh"
        | "workspace/inlineValue/refresh"
        | "workspace/diagnostic/refresh" => Ok(Value::Null),
        _ => Err((METHOD_NOT_FOUND, format!("unhandled method: {method}"))),
    }
}

/// The mode-line text for a `$/progress` begin/report `value` of a task
/// titled `title` (reports don't repeat the title), e.g.
/// `rust: Indexing 3/10 30%`.
pub fn progress_text(language: &str, title: &str, value: &Value) -> String {
    let mut text = format!("{language}: {title}");
    if let Some(message) = value["message"].as_str().filter(|m| !m.is_empty()) {
        text.push(' ');
        text.push_str(&crate::util::single_line(message));
    }
    if let Some(pct) = value["percentage"].as_u64() {
        text.push_str(&format!(" {pct}%"));
    }
    text
}

/// Whether (and how) the server wants `textDocument/didSave`, from its
/// `initialize` result: `None` = don't send, `Some(include_text)` = send. A
/// bare-number `textDocumentSync` predates the `save` option and gets text-less
/// notifications (matching vscode-languageclient's resolution).
pub fn parse_save_option(result: &Value) -> Option<bool> {
    let sync = &result["capabilities"]["textDocumentSync"];
    if sync.is_number() {
        return Some(false);
    }
    match &sync["save"] {
        Value::Bool(true) => Some(false),
        Value::Object(o) => Some(o.get("includeText").and_then(Value::as_bool) == Some(true)),
        _ => None,
    }
}

/// A diagnostic in LSP coordinates (line, UTF-16 column), before mapping to a
/// specific buffer's byte offsets.
#[derive(Clone, Debug)]
pub struct RawDiagnostic {
    pub start: (usize, usize),
    pub end: (usize, usize),
    pub severity: Severity,
    pub message: String,
    /// The diagnostic exactly as the server published it (`code`, `source`,
    /// `data`, … included), echoed back verbatim in a `codeAction` request's
    /// context so the server can match its quick fixes to it.
    pub raw: Value,
}

/// How a server wants document changes synced, from its `initialize` result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncKind {
    /// Sync kind 0: the server wants no `didChange` notifications at all.
    None,
    /// Sync kind 1: resend the whole document on change.
    Full,
    /// Sync kind 2: send only the changed range.
    Incremental,
}

/// The text-sync kind the server wants, from its `initialize` result.
/// `textDocumentSync` may be a bare number or an object with a `change` field.
/// A missing field defaults to full (the safe choice); an out-of-range value
/// means no sync (matching the old "unrecognized number ⇒ send nothing").
pub fn parse_sync_kind(result: &Value) -> SyncKind {
    let sync = &result["capabilities"]["textDocumentSync"];
    let n = sync
        .as_u64()
        .or_else(|| sync["change"].as_u64())
        .unwrap_or(1);
    match n {
        1 => SyncKind::Full,
        2 => SyncKind::Incremental,
        _ => SyncKind::None,
    }
}

/// An LSP `Position` on the wire. Missing fields default to 0 (the same lossy
/// behavior as the manual `as_u64().unwrap_or(0)` digs this replaces).
#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct WirePos {
    #[serde(default)]
    line: usize,
    #[serde(default)]
    character: usize,
}

/// An LSP `Range` on the wire. `start`/`end` default so a missing or malformed
/// endpoint becomes `(0, 0)` — matching the old per-field `unwrap_or(0)` digs.
#[derive(serde::Deserialize, Default)]
struct WireRange {
    #[serde(default)]
    start: WirePos,
    #[serde(default)]
    end: WirePos,
}

impl From<WireRange> for ((usize, usize), (usize, usize)) {
    fn from(r: WireRange) -> ((usize, usize), (usize, usize)) {
        (r.start.into(), r.end.into())
    }
}

impl From<WirePos> for (usize, usize) {
    fn from(p: WirePos) -> (usize, usize) {
        (p.line, p.character)
    }
}

fn position(value: &Value) -> (usize, usize) {
    serde_json::from_value::<WirePos>(value.clone()).map_or((0, 0), Into::into)
}

/// A `Diagnostic` on the wire — just the fields we consume. A missing/malformed
/// field takes its default, matching the old per-field `unwrap_or` digs.
#[derive(serde::Deserialize, Default)]
struct WireDiagnostic {
    #[serde(default)]
    range: WireRange,
    severity: Option<u64>,
    #[serde(default)]
    message: String,
}

/// Parse a `textDocument/publishDiagnostics` params object into `(uri, diags)`.
pub fn parse_diagnostics(params: &Value) -> (String, Vec<RawDiagnostic>) {
    let uri = normalize_uri(params["uri"].as_str().unwrap_or_default());
    let diagnostics = params["diagnostics"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|d| {
                    let wire: WireDiagnostic =
                        serde_json::from_value(d.clone()).unwrap_or_default();
                    let (start, end) = wire.range.into();
                    RawDiagnostic {
                        start,
                        end,
                        severity: wire.severity.map_or(Severity::Error, Severity::from_wire),
                        message: wire.message,
                        raw: d.clone(),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    (uri, diagnostics)
}

/// A completion candidate with the extra metadata servers provide.
#[derive(Clone, Debug)]
pub struct CompletionItem {
    /// What the user sees / filters on.
    pub label: String,
    /// What's inserted on accept (often the same as `label`).
    pub insert_text: String,
    /// Signature / parameters / type, e.g. `(x: i32) -> bool` (from
    /// `labelDetails` or `detail`).
    pub detail: Option<String>,
    /// A short kind tag, e.g. `fn`, `var`, `kw`, `type`.
    pub kind: Option<String>,
    /// Documentation (plain text), filled in by `completionItem/resolve`.
    pub documentation: Option<String>,
    /// Edits applied alongside the insertion on accept — e.g. an auto-added
    /// `use`/import. May arrive in the initial response or via resolve.
    pub additional_edits: Vec<TextEdit>,
    /// The raw item JSON, echoed back verbatim to `completionItem/resolve`
    /// (kept as a `Value` so integer fields stay integers).
    pub data: Value,
    /// Whether this item has been through `completionItem/resolve`.
    pub resolved: bool,
}

/// Extract completion items from a `textDocument/completion` result.
pub fn parse_completion(result: &Value) -> Vec<CompletionItem> {
    // Result is either `CompletionItem[]` or `{ items: CompletionItem[] }`.
    result
        .as_array()
        .or_else(|| result["items"].as_array())
        .map(|arr| arr.iter().filter_map(parse_completion_item).collect())
        .unwrap_or_default()
}

/// A `CompletionItem` on the wire — the fields we consume, typed. Two fields
/// stay raw `Value`s because they're genuinely polymorphic: `documentation`
/// (string-or-`MarkupContent`) and `additionalTextEdits` (parsed by the shared
/// [`parse_text_edits`]). The original JSON is kept separately as
/// [`CompletionItem::data`] to echo back to `completionItem/resolve`.
#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct WireCompletionItem {
    label: Option<String>,
    insert_text: Option<String>,
    /// `TextEdit` or `InsertReplaceEdit` — both carry `newText`.
    text_edit: Option<WireNewText>,
    kind: Option<u64>,
    #[serde(default)]
    documentation: Value,
    #[serde(default)]
    additional_text_edits: Value,
    #[serde(default)]
    label_details: WireLabelDetails,
    detail: Option<String>,
}

#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct WireNewText {
    new_text: Option<String>,
}

#[derive(serde::Deserialize, Default)]
struct WireLabelDetails {
    detail: Option<String>,
    description: Option<String>,
}

impl WireCompletionItem {
    /// The signature/type annotation, preferring `labelDetails` (params +
    /// return or module) and falling back to `detail`.
    fn signature(&self) -> Option<String> {
        let detail = self.label_details.detail.as_deref().unwrap_or("");
        let description = self.label_details.description.as_deref().unwrap_or("");
        let combined = format!("{detail} {description}");
        let combined = combined.trim();
        if !combined.is_empty() {
            return Some(combined.to_string());
        }
        self.detail.clone().filter(|s| !s.is_empty())
    }
}

fn parse_completion_item(item: &Value) -> Option<CompletionItem> {
    // Completion labels and detail annotations render as a single popup line
    // (`shape_line`, which panics on an embedded newline) — and servers like
    // ocamllsp put multi-line type signatures in `detail`.
    use crate::util::single_line;
    let wire: WireCompletionItem = serde_json::from_value(item.clone()).ok()?;
    let detail = wire.signature().map(|d| single_line(&d));
    let raw_label = wire.label?;
    // `insert_text` goes into the buffer, so fall back to the *raw* label; the
    // display `label` is collapsed to a single line for the popup.
    // Per the spec, `textEdit` wins over `insertText` when both are present.
    let insert_text = wire
        .text_edit
        .and_then(|e| e.new_text)
        .or(wire.insert_text)
        .unwrap_or_else(|| raw_label.clone());
    let label = single_line(&raw_label);
    let kind = wire.kind.and_then(completion_kind).map(str::to_string);
    Some(CompletionItem {
        label,
        insert_text,
        detail,
        kind,
        documentation: parse_documentation(&wire.documentation),
        additional_edits: parse_text_edits(&wire.additional_text_edits),
        data: item.clone(),
        resolved: false,
    })
}

/// Merge a `completionItem/resolve` result into `item` — servers fill in
/// `documentation` and `additionalTextEdits` (auto-imports) lazily.
pub fn merge_resolved_completion(item: &mut CompletionItem, resolved: &Value) {
    let wire: WireCompletionItem = serde_json::from_value(resolved.clone()).unwrap_or_default();
    if let Some(doc) = parse_documentation(&wire.documentation) {
        item.documentation = Some(doc);
    }
    let edits = parse_text_edits(&wire.additional_text_edits);
    if !edits.is_empty() {
        item.additional_edits = edits;
    }
    if let Some(detail) = wire.signature() {
        // Same single-line rule as `parse_completion_item`: the popup can't
        // render an embedded newline.
        item.detail = Some(crate::util::single_line(&detail));
    }
    item.resolved = true;
}

/// Plain text of a `documentation` field (a string or a `MarkupContent`).
pub fn parse_documentation(value: &Value) -> Option<String> {
    let text = match value {
        Value::String(s) => s.clone(),
        Value::Object(_) => value["value"].as_str().unwrap_or("").to_string(),
        _ => String::new(),
    };
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// The result of `textDocument/prepareRename`: whether the position is
/// renameable, and the server's placeholder/range if given.
#[derive(Clone, Debug, PartialEq)]
pub struct PrepareRename {
    pub placeholder: Option<String>,
    pub range: Option<((usize, usize), (usize, usize))>,
}

/// Parse a `prepareRename` result: `Range` | `{range, placeholder}` |
/// `{defaultBehavior:true}` | `null`. `None` ⇒ not renameable here.
pub fn parse_prepare_rename(result: &Value) -> Option<PrepareRename> {
    if result.is_null() {
        return None;
    }
    if result.get("defaultBehavior").and_then(Value::as_bool) == Some(true) {
        return Some(PrepareRename {
            placeholder: None,
            range: None,
        });
    }
    if let Some(placeholder) = result.get("placeholder").and_then(Value::as_str) {
        let range = result
            .get("range")
            .map(|r| (position(&r["start"]), position(&r["end"])));
        return Some(PrepareRename {
            placeholder: Some(placeholder.to_string()),
            range,
        });
    }
    if result.get("start").is_some() {
        return Some(PrepareRename {
            placeholder: None,
            range: Some((position(&result["start"]), position(&result["end"]))),
        });
    }
    None
}

/// Whether the server's `renameProvider` advertises `prepareSupport`.
pub fn parse_prepare_rename_support(result: &Value) -> bool {
    result["capabilities"]["renameProvider"]["prepareProvider"]
        .as_bool()
        .unwrap_or(false)
}

/// The server's `completionProvider.triggerCharacters` — characters that should
/// auto-open completion even though they aren't identifier characters (e.g. `.`
/// for member access in OCaml/Rust). Empty if the server advertises none.
pub fn parse_completion_triggers(result: &Value) -> Vec<String> {
    result["capabilities"]["completionProvider"]["triggerCharacters"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// A code lens: where it sits, its title (if the command is already present),
/// and the raw lens (echoed to `codeLens/resolve` when the command is missing).
#[derive(Clone, Debug)]
pub struct CodeLens {
    pub line: usize,
    pub character: usize,
    pub title: Option<String>,
    pub data: Value,
}

/// A `CodeLens` on the wire — position plus the (possibly absent) command title.
#[derive(serde::Deserialize, Default)]
struct WireCodeLens {
    #[serde(default)]
    range: WireRange,
    command: Option<WireCommandTitle>,
}

#[derive(serde::Deserialize, Default)]
struct WireCommandTitle {
    title: Option<String>,
}

/// Parse a `textDocument/codeLens` result.
pub fn parse_code_lenses(result: &Value) -> Vec<CodeLens> {
    result
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|lens| {
                    let wire: WireCodeLens =
                        serde_json::from_value(lens.clone()).unwrap_or_default();
                    CodeLens {
                        line: wire.range.start.line,
                        character: wire.range.start.character,
                        title: wire.command.and_then(|c| c.title),
                        data: lens.clone(),
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The title of a resolved code lens (`codeLens/resolve` fills in `command`).
pub fn parse_code_lens_title(resolved: &Value) -> Option<String> {
    serde_json::from_value::<WireCodeLens>(resolved.clone())
        .ok()?
        .command?
        .title
}

/// Map an LSP `CompletionItemKind` to a short tag.
fn completion_kind(kind: u64) -> Option<&'static str> {
    Some(match kind {
        2 | 3 => "fn",           // Method / Function
        4 => "ctor",             // Constructor
        5 | 10 => "field",       // Field / Property
        6 => "var",              // Variable
        7 | 8 | 22 => "type",    // Class / Interface / Struct
        9 => "mod",              // Module
        14 => "kw",              // Keyword
        21 => "const",           // Constant
        12 | 13 | 20 => "value", // Value / Enum / EnumMember
        _ => return None,
    })
}

/// A source location: a document URI and a `(line, utf16-col)` range. Used for
/// go-to-definition and friends, references, and symbols.
#[derive(Clone, Debug, PartialEq)]
pub struct Location {
    pub uri: String,
    pub start: (usize, usize),
    pub end: (usize, usize),
}

/// Parse a definition/references/etc. result, which may be `null`, a single
/// `Location`, an array of `Location`, or an array of `LocationLink`.
pub fn parse_locations(result: &Value) -> Vec<Location> {
    fn one(v: &Value) -> Option<Location> {
        // `LocationLink` (link-support responses) uses `targetUri` plus a
        // `targetSelectionRange` (the identifier) / `targetRange` (the whole def).
        if let Some(uri) = v.get("targetUri").and_then(Value::as_str) {
            let range = if v.get("targetSelectionRange").is_some() {
                &v["targetSelectionRange"]
            } else {
                &v["targetRange"]
            };
            return Some(Location {
                uri: uri.to_string(),
                start: position(&range["start"]),
                end: position(&range["end"]),
            });
        }
        // Plain `Location`.
        let uri = v.get("uri").and_then(Value::as_str)?;
        Some(Location {
            uri: uri.to_string(),
            start: position(&v["range"]["start"]),
            end: position(&v["range"]["end"]),
        })
    }
    if let Some(arr) = result.as_array() {
        return arr.iter().filter_map(one).collect();
    }
    one(result).into_iter().collect()
}

/// Plain-text rendering of a `Hover` result (`contents` may be a string, a
/// `MarkupContent`, a `MarkedString`, or an array of those). `None` when empty.
pub fn parse_hover(result: &Value) -> Option<String> {
    fn render(v: &Value) -> String {
        match v {
            Value::String(s) => s.clone(),
            Value::Array(arr) => arr.iter().map(render).collect::<Vec<_>>().join("\n"),
            // `MarkupContent { kind, value }` or `MarkedString { language, value }`.
            Value::Object(_) => v
                .get("value")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            _ => String::new(),
        }
    }
    let text = render(&result["contents"]);
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// A document/workspace symbol, flattened from the (possibly nested) response.
#[derive(Clone, Debug, PartialEq)]
pub struct SymbolInfo {
    pub name: String,
    pub kind: u64,
    pub uri: String,
    pub start: (usize, usize),
    pub container: String,
    /// Nesting depth (0 = top level) — for indentation in the list.
    pub depth: usize,
}

/// A `DocumentSymbol | SymbolInformation` on the wire: a flat
/// `SymbolInformation` carries its own `location`, a hierarchical
/// `DocumentSymbol` only ranges + `children`.
#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct WireSymbol {
    #[serde(default)]
    name: String,
    #[serde(default)]
    kind: u64,
    location: Option<WireLocation>,
    container_name: Option<String>,
    #[serde(default)]
    range: WireRange,
    selection_range: Option<WireRange>,
    #[serde(default)]
    children: Vec<WireSymbol>,
}

#[derive(serde::Deserialize, Default)]
struct WireLocation {
    uri: Option<String>,
    #[serde(default)]
    range: WireRange,
}

/// Parse a `documentSymbol` (nested `DocumentSymbol[]` or flat
/// `SymbolInformation[]`) or `workspace/symbol` result. `fallback_uri` is used
/// for `DocumentSymbol`s, which carry only ranges, not URIs.
pub fn parse_symbols(result: &Value, fallback_uri: &str) -> Vec<SymbolInfo> {
    fn push(
        item: WireSymbol,
        fallback_uri: &str,
        container: &str,
        depth: usize,
        out: &mut Vec<SymbolInfo>,
    ) {
        if let Some(loc) = item.location {
            // `SymbolInformation`: a flat item carrying its own location.
            out.push(SymbolInfo {
                name: item.name,
                kind: item.kind,
                uri: loc.uri.unwrap_or_else(|| fallback_uri.to_string()),
                start: loc.range.start.into(),
                container: item.container_name.unwrap_or_else(|| container.to_string()),
                depth,
            });
        } else {
            // `DocumentSymbol`: hierarchical, ranges only. Prefer the narrower
            // `selectionRange` (the identifier) over the whole definition.
            let range = item.selection_range.unwrap_or(item.range);
            out.push(SymbolInfo {
                name: item.name.clone(),
                kind: item.kind,
                uri: fallback_uri.to_string(),
                start: range.start.into(),
                container: container.to_string(),
                depth,
            });
            for child in item.children {
                push(child, fallback_uri, &item.name, depth + 1, out);
            }
        }
    }
    let mut out = Vec::new();
    if let Some(arr) = result.as_array() {
        for item in arr {
            if let Ok(sym) = serde_json::from_value::<WireSymbol>(item.clone()) {
                push(sym, fallback_uri, "", 0, &mut out);
            }
        }
    }
    out
}

/// A short name for an LSP `SymbolKind`, for display in the symbol list.
pub fn symbol_kind_name(kind: u64) -> &'static str {
    match kind {
        1 => "file",
        2 => "module",
        3 => "namespace",
        4 => "package",
        5 => "class",
        6 => "method",
        7 => "property",
        8 => "field",
        9 => "ctor",
        10 => "enum",
        11 => "interface",
        12 => "fn",
        13 => "var",
        14 => "const",
        15 => "string",
        16 => "number",
        17 => "bool",
        18 => "array",
        23 => "struct",
        26 => "type",
        _ => "sym",
    }
}

/// A single text edit in LSP `(line, utf16-col)` coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct TextEdit {
    pub start: (usize, usize),
    pub end: (usize, usize),
    pub new_text: String,
}

/// A `TextEdit` on the wire. `range` is required (an edit without one is
/// meaningless and is skipped, as before); a missing `newText` deletes.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireTextEdit {
    range: WireRange,
    #[serde(default)]
    new_text: String,
}

fn parse_one_edit(v: &Value) -> Option<TextEdit> {
    let edit: WireTextEdit = serde_json::from_value(v.clone()).ok()?;
    let (start, end) = edit.range.into();
    Some(TextEdit {
        start,
        end,
        new_text: edit.new_text,
    })
}

/// Parse a `TextEdit[]` (e.g. a `formatting` response).
pub fn parse_text_edits(result: &Value) -> Vec<TextEdit> {
    result
        .as_array()
        .map(|arr| arr.iter().filter_map(parse_one_edit).collect())
        .unwrap_or_default()
}

/// A workspace edit: per-document text edits. File create/rename/delete
/// operations in `documentChanges` are ignored (we only apply text edits).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WorkspaceEdit {
    pub changes: Vec<(String, Vec<TextEdit>)>,
}

/// Parse a `WorkspaceEdit` from either the newer `documentChanges` array or the
/// older `changes` map.
pub fn parse_workspace_edit(result: &Value) -> WorkspaceEdit {
    let mut changes = Vec::new();
    if let Some(arr) = result["documentChanges"].as_array() {
        for dc in arr {
            // Only `TextDocumentEdit`s have an `edits` array; skip file ops.
            if let Some(uri) = dc["textDocument"]["uri"].as_str() {
                let edits = parse_text_edits(&dc["edits"]);
                if !edits.is_empty() {
                    changes.push((uri.to_string(), edits));
                }
            }
        }
    } else if let Some(map) = result["changes"].as_object() {
        for (uri, edits) in map {
            let edits = parse_text_edits(edits);
            if !edits.is_empty() {
                changes.push((uri.clone(), edits));
            }
        }
    }
    WorkspaceEdit { changes }
}

/// A code action: a title plus an optional inline `WorkspaceEdit` and/or a
/// server `command` to run via `workspace/executeCommand`.
#[derive(Clone, Debug, PartialEq)]
pub struct CodeAction {
    pub title: String,
    pub edit: Option<WorkspaceEdit>,
    /// `(command, arguments)` for `workspace/executeCommand`.
    pub command: Option<(String, Value)>,
    /// The raw `CodeAction` literal when it arrived without an `edit`: the
    /// server may fill the edit in lazily, so applying it first round-trips
    /// this through `codeAction/resolve`. `None` for bare `Command`s and for
    /// actions that already carry their edit.
    pub resolve: Option<Value>,
}

/// A `Command | CodeAction` on the wire. `edit` (a `WorkspaceEdit`, itself two
/// shapes — see [`parse_workspace_edit`]) and `command` (a bare string on a
/// `Command`, a nested object on a `CodeAction`) are genuinely polymorphic, so
/// they stay raw `Value`s.
#[derive(serde::Deserialize, Default)]
struct WireCodeAction {
    title: Option<String>,
    #[serde(default)]
    edit: Value,
    #[serde(default)]
    command: Value,
    #[serde(default)]
    arguments: Value,
}

/// Parse a `(Command | CodeAction)[]` result.
pub fn parse_code_actions(result: &Value) -> Vec<CodeAction> {
    let Some(arr) = result.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|item| {
            let wire: WireCodeAction = serde_json::from_value(item.clone()).ok()?;
            let title = wire.title?;
            // A bare `Command` has a string `command`; a `CodeAction` may have an
            // `edit` and/or a nested `command` object.
            let (edit, command, resolve) = if let Value::String(cmd) = &wire.command {
                (None, Some((cmd.clone(), wire.arguments)), None)
            } else {
                let edit = (!wire.edit.is_null()).then(|| parse_workspace_edit(&wire.edit));
                let command = wire.command["command"]
                    .as_str()
                    .map(|c| (c.to_string(), wire.command["arguments"].clone()));
                let resolve = edit.is_none().then(|| item.clone());
                (edit, command, resolve)
            };
            Some(CodeAction {
                title,
                edit,
                command,
                resolve,
            })
        })
        .collect()
}

/// The semantic-tokens legend (`tokenTypes`) from the server's `initialize`
/// result, indexed by the token-type id in the encoded stream.
pub fn parse_semantic_legend(result: &Value) -> Vec<String> {
    result["capabilities"]["semanticTokensProvider"]["legend"]["tokenTypes"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|v| v.as_str().unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// One decoded semantic token: `(line, start_char, length, type_name)` in LSP
/// coordinates (UTF-16 column).
pub type SemanticToken = (usize, usize, usize, String);

/// Decode a `semanticTokens/full` result. The `data` array is groups of five
/// integers — `[deltaLine, deltaStartChar, length, tokenType, tokenModifiers]`
/// — delta-encoded against the previous token (same line ⇒ delta column adds,
/// new line ⇒ column resets). `legend` maps `tokenType` ids to names.
/// Decode an already-extracted packed `data` array (used after a delta merge).
pub fn decode_semantic_data(data: &[i64], legend: &[String]) -> Vec<SemanticToken> {
    let mut tokens = Vec::new();
    let (mut line, mut col) = (0usize, 0usize);
    for group in data.as_chunks::<5>().0 {
        let delta_line = group[0].max(0) as usize;
        let delta_start = group[1].max(0) as usize;
        let length = group[2].max(0) as usize;
        let token_type = group[3].max(0) as usize;
        if delta_line > 0 {
            line += delta_line;
            col = delta_start;
        } else {
            col += delta_start;
        }
        let name = legend.get(token_type).cloned().unwrap_or_default();
        tokens.push((line, col, length, name));
    }
    tokens
}

/// The packed `data` integer array from a `semanticTokens` result.
pub fn semantic_data(result: &Value) -> Vec<i64> {
    result["data"]
        .as_array()
        .map(|a| a.iter().map(|v| v.as_i64().unwrap_or(0)).collect())
        .unwrap_or_default()
}

/// A `semanticTokens` result's `resultId` (for follow-up delta requests).
pub fn semantic_result_id(result: &Value) -> Option<String> {
    result["resultId"].as_str().map(str::to_string)
}

/// Apply a `semanticTokens/full/delta` result to the previous packed `data`.
/// A delta result has `edits: [{start, deleteCount, data}]` splicing the array;
/// a full result (no `edits`) just carries fresh `data`.
pub fn apply_semantic_delta(prev: &[i64], result: &Value) -> Vec<i64> {
    let Some(edits) = result["edits"].as_array() else {
        return semantic_data(result); // server sent a full set instead
    };
    let mut data = prev.to_vec();
    // Apply edits high-offset-first so earlier splices don't shift later ones.
    let mut edits: Vec<&Value> = edits.iter().collect();
    edits.sort_by_key(|e| std::cmp::Reverse(e["start"].as_u64().unwrap_or(0)));
    for edit in edits {
        let start = edit["start"].as_u64().unwrap_or(0) as usize;
        let delete = edit["deleteCount"].as_u64().unwrap_or(0) as usize;
        let new: Vec<i64> = edit["data"]
            .as_array()
            .map(|a| a.iter().map(|v| v.as_i64().unwrap_or(0)).collect())
            .unwrap_or_default();
        let start = start.min(data.len());
        let end = (start + delete).min(data.len());
        data.splice(start..end, new);
    }
    data
}

/// Extract call locations from a `callHierarchy/incomingCalls` (`key = "from"`)
/// or `outgoingCalls` (`key = "to"`) result. Each call's item contributes a
/// `Location` (its `selectionRange`, falling back to `range`) plus name via the
/// returned `(name, Location)` pairs.
pub fn parse_call_hierarchy(result: &Value, key: &str) -> Vec<(String, Location)> {
    /// The `CallHierarchyItem` inside an incoming/outgoing call.
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct WireHierarchyItem {
        name: Option<String>,
        #[serde(default)]
        uri: String,
        #[serde(default)]
        range: WireRange,
        selection_range: Option<WireRange>,
    }

    let Some(arr) = result.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|call| {
            let item: WireHierarchyItem = serde_json::from_value(call.get(key)?.clone()).ok()?;
            // Prefer the narrower `selectionRange` (the identifier).
            let range = item.selection_range.unwrap_or(item.range);
            Some((
                item.name.unwrap_or_else(|| "?".to_string()),
                Location {
                    uri: item.uri,
                    start: range.start.into(),
                    end: range.end.into(),
                },
            ))
        })
        .collect()
}

/// The active signature from a `signatureHelp` result, as a single line. `None`
/// when there are no signatures.
pub fn parse_signature_help(result: &Value) -> Option<String> {
    let sigs = result["signatures"].as_array()?;
    if sigs.is_empty() {
        return None;
    }
    let active = result["activeSignature"].as_u64().unwrap_or(0) as usize;
    let sig = sigs.get(active).or_else(|| sigs.first())?;
    sig["label"].as_str().map(str::to_string)
}

/// Apply LSP `TextEdit`s to `text`, returning the new text. Edits are applied
/// last-position-first so earlier byte offsets stay valid as we splice.
pub fn apply_edits_to_text(text: &str, edits: &[TextEdit]) -> String {
    let buf = crate::buffer::Buffer::new(text);
    let mut spans: Vec<(usize, usize, usize, &str)> = edits
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let a = buf.position_to_byte(e.start.0, e.start.1);
            let b = buf.position_to_byte(e.end.0, e.end.1);
            (a.min(b), a.max(b), i, e.new_text.as_str())
        })
        .collect();
    // Descending by start so splicing one span never shifts another. Ties
    // (e.g. several inserts at one position) go in *reverse* array order:
    // each later splice lands in front of the earlier one, so the final text
    // reads in array order, as the LSP spec requires.
    spans.sort_by(|a, b| b.0.cmp(&a.0).then(b.2.cmp(&a.2)));
    let mut out = text.to_string();
    for (start, end, _, new) in spans {
        let start = start.min(out.len());
        let end = end.min(out.len());
        if start <= end {
            out.replace_range(start..end, new);
        }
    }
    out
}

/// Convert a `file://` URI to a filesystem path string (inverse of
/// [`path_to_uri`] for local paths), percent-decoding the path — servers like
/// rust-analyzer encode spaces and non-ASCII (`%20`, `%C3%A9`, …).
pub fn uri_to_path(uri: &str) -> String {
    let Some(rest) = uri.strip_prefix("file://") else {
        return uri.to_string();
    };
    let path = percent_decode(rest);
    // Windows drive form: `file:///C:/…` → `C:/…`.
    #[cfg(windows)]
    if let Some(drive) = path.strip_prefix('/') {
        let mut chars = drive.chars();
        if let (Some(letter), Some(':')) = (chars.next(), chars.next())
            && letter.is_ascii_alphabetic()
        {
            return drive.to_string();
        }
    }
    path
}

/// Percent-decode `%XX` escapes in `s`; malformed escapes pass through
/// literally. Decoded bytes are reassembled as (lossy) UTF-8.
fn percent_decode(s: &str) -> String {
    fn hex(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let (Some(hi), Some(lo)) = (
                bytes.get(i + 1).copied().and_then(hex),
                bytes.get(i + 2).copied().and_then(hex),
            )
        {
            out.push(hi << 4 | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl Drop for LspClient {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            // Kill the whole group (see `start`). The child is not reaped yet,
            // so its pid (= the pgid) can't have been recycled.
            #[cfg(unix)]
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.kill();
            // Reap it, or it lingers as a zombie. SIGKILL makes `wait` return
            // promptly, but do it off-thread so a drop on the UI thread never
            // blocks.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }
}

/// A language-server backend: either an external process (`LspClient`) or the
/// in-process [`SteelLsp`]. The editor drives both through the same methods, so
/// nothing downstream of here cares which kind it's talking to.
pub enum LspBackend {
    /// Both variants are boxed: an `LspClient` holds a subprocess handle +
    /// channels, and a `SteelLsp` embeds a whole Steel engine — both far larger
    /// than a pointer, so boxing keeps the enum (and everything holding it) small.
    External(Box<LspClient>),
    Builtin(Box<SteelLsp>),
}

impl LspBackend {
    pub fn initialized(&mut self) {
        match self {
            LspBackend::External(c) => c.initialized(),
            LspBackend::Builtin(b) => b.initialized(),
        }
    }

    pub fn did_open(&mut self, uri: &str, language_id: &str, version: i32, text: &str) {
        match self {
            LspBackend::External(c) => c.did_open(uri, language_id, version, text),
            LspBackend::Builtin(b) => b.did_open(uri, text),
        }
    }

    pub fn did_change(&mut self, uri: &str, version: i32, text: &str) {
        match self {
            LspBackend::External(c) => c.did_change(uri, version, text),
            LspBackend::Builtin(b) => b.did_change(uri, text),
        }
    }

    pub fn did_change_incremental(
        &mut self,
        uri: &str,
        version: i32,
        start: (usize, usize),
        end: (usize, usize),
        text: &str,
    ) {
        match self {
            LspBackend::External(c) => c.did_change_incremental(uri, version, start, end, text),
            // The builtin advertises full sync, so this isn't normally reached.
            LspBackend::Builtin(_) => {}
        }
    }

    pub fn completion(&mut self, uri: &str, line: usize, character: usize) -> i64 {
        match self {
            LspBackend::External(c) => c.completion(uri, line, character),
            LspBackend::Builtin(b) => b.completion(uri, line, character),
        }
    }

    /// Send a generic request. Returns its id, or `None` when unsupported. The
    /// builtin Steel server answers definition/references (plus diagnostics and
    /// completion, handled elsewhere) and declines everything else.
    pub fn request(&mut self, method: &str, params: Value) -> Option<i64> {
        match self {
            LspBackend::External(c) => Some(c.send_request(method, params)),
            LspBackend::Builtin(b) => b.request(method, &params),
        }
    }

    pub fn respond(&mut self, id: Value, result: Value) {
        match self {
            LspBackend::External(c) => c.respond(id, result),
            LspBackend::Builtin(_) => {} // never sends server→client requests
        }
    }

    pub fn respond_error(&mut self, id: Value, code: i64, message: &str) {
        match self {
            LspBackend::External(c) => c.respond_error(id, code, message),
            LspBackend::Builtin(_) => {}
        }
    }

    pub fn did_close(&mut self, uri: &str) {
        match self {
            LspBackend::External(c) => c.did_close(uri),
            LspBackend::Builtin(b) => b.did_close(uri),
        }
    }

    pub fn did_save(&mut self, uri: &str, text: Option<&str>) {
        match self {
            LspBackend::External(c) => c.did_save(uri, text),
            LspBackend::Builtin(_) => {} // re-checks on every change already
        }
    }

    pub fn cancel(&mut self, id: i64) {
        match self {
            LspBackend::External(c) => c.cancel(id),
            LspBackend::Builtin(_) => {} // answers synchronously; nothing in flight
        }
    }
}

/// An in-process language server for Steel/Scheme. It speaks the same JSON-RPC
/// shapes the editor expects, pushing messages onto a channel just like the
/// external client — but everything runs in Rust, no subprocess.
///
/// It keeps a copy of each open document (full sync — free in-process, and it
/// avoids the drift an incremental copy could accumulate) and offers:
///   * **diagnostics** — a fast delimiter/string/comment balance check, then,
///     once parens balance, the real Steel compiler (`emit_raw_program`) for
///     semantic errors (free identifiers, arity, bad syntax) with spans, and
///   * **completion** — every binding the Steel environment knows (builtins,
///     stdlib, editor commands, the file's own defines) plus special forms.
pub struct SteelLsp {
    tx: UnboundedSender<Value>,
    next_id: i64,
    docs: HashMap<String, String>,
    /// A compile-only engine: used to type-check source, never to run it.
    engine: Engine,
    /// name → (doc, source file, 1-based line) for every bundled define, so
    /// go-to-definition and hover can resolve editor-API symbols. Empty for the
    /// plain `scheme` server (which seeds no editor API).
    defs: HashMap<String, (String, String, usize)>,
}

/// Outcome of a sandboxed diagnostic compile.
enum CompileResult {
    Ok,
    Error(steel::rerrs::SteelErr),
    Panicked,
}

impl SteelLsp {
    /// Create the server and its message receiver, immediately answering the
    /// (implicit) `initialize` so the editor's handshake completes.
    /// `known_globals` are editor-registered Steel commands (e.g. `set-lsp`),
    /// bound so the compiler doesn't flag references to them as undefined.
    ///
    /// `seed_editor_api` distinguishes the two Scheme servers: the `macros-scheme`
    /// server (config / extension code) seeds the whole editor API — the
    /// registered commands plus every name the bundled Scheme library defines —
    /// so editor functions complete and aren't flagged "unknown". The plain
    /// `scheme` server seeds neither: it's a vanilla Steel environment, "just the
    /// interpreter", and references to editor functions are correctly unknown.
    pub fn start(
        seed_editor_api: bool,
        known_globals: &[String],
    ) -> (Self, UnboundedReceiver<Value>) {
        let (tx, rx) = unbounded();
        let mut engine = Engine::new();
        let mut defs = HashMap::new();
        if seed_editor_api {
            for name in known_globals {
                engine.register_value(name, SteelVal::Void);
            }
            // Also seed the names the bundled Scheme library defines (lsp-language,
            // evil-define-key, register-command, …) so user configs that call them
            // aren't flagged "unknown function".
            for name in crate::runtime::defined_names() {
                engine.register_value(&name, SteelVal::Void);
            }
            // …and their docs + source locations, so go-to-definition can jump to
            // the bundled implementation of an editor command referenced by name,
            // and hover can show its doc-comment.
            for d in crate::runtime::builtin_docs() {
                defs.insert(d.name, (d.doc, d.file, d.line));
            }
        }
        let server = Self {
            tx,
            next_id: 1,
            docs: HashMap::new(),
            engine,
            defs,
        };
        // textDocumentSync = 1 (full): the editor resends the whole document on
        // every change, which is free in-process and keeps this server simple.
        // `prepareProvider` makes the editor validate a rename target first, so we
        // can decline (in `prepare_rename`) anything not defined in this document.
        let _ = server.tx.unbounded_send(json!({
            "jsonrpc": "2.0",
            "id": 0,
            "result": { "capabilities": {
                "textDocumentSync": 1,
                "renameProvider": { "prepareProvider": true }
            } }
        }));
        (server, rx)
    }

    fn initialized(&mut self) {}

    fn did_open(&mut self, uri: &str, text: &str) {
        self.docs.insert(uri.to_string(), text.to_string());
        self.publish_diagnostics(uri);
    }

    fn did_change(&mut self, uri: &str, text: &str) {
        self.docs.insert(uri.to_string(), text.to_string());
        self.publish_diagnostics(uri);
    }

    fn did_close(&mut self, uri: &str) {
        self.docs.remove(uri);
    }

    /// Queue a response carrying `result` under a fresh request id, returning
    /// that id — the shared tail of every builtin request handler.
    fn respond(&mut self, result: Value) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        let _ = self
            .tx
            .unbounded_send(json!({ "jsonrpc": "2.0", "id": id, "result": result }));
        id
    }

    fn completion(&mut self, uri: &str, line: usize, character: usize) -> i64 {
        let text = self.docs.get(uri).cloned().unwrap_or_default();
        let items = self.complete(&text, line, character);
        self.respond(json!(items))
    }

    fn publish_diagnostics(&mut self, uri: &str) {
        let text = self.docs.get(uri).cloned().unwrap_or_default();
        let diagnostics = self.compute_diagnostics(&text);
        let _ = self.tx.unbounded_send(json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": { "uri": uri, "diagnostics": diagnostics }
        }));
    }

    /// Diagnostics in layers: (1) delimiter/string balance (precise spots);
    /// (2) the Steel compiler for structural/semantic errors; (3) when the file
    /// otherwise compiles clean, a conservative unbound-operator scan over the
    /// AST (likely typos — reported as warnings).
    fn compute_diagnostics(&mut self, text: &str) -> Vec<Value> {
        let balance = steel_diagnostics(text);
        if !balance.is_empty() {
            return balance;
        }
        let mut diagnostics = match self.try_compile(text) {
            CompileResult::Ok => Vec::new(),
            CompileResult::Error(err) => vec![steel_error_diagnostic(text, &err)],
            // Steel's compiler has panicked on malformed-but-balanced input
            // (`(define)` did before steel-core 0.8.3), which is common while
            // typing. We catch it (see
            // `try_compile`) and report nothing for that transient state rather
            // than crash the editor.
            CompileResult::Panicked => Vec::new(),
        };
        if diagnostics.is_empty() {
            let known = self.known_names();
            let buf = crate::buffer::Buffer::new(text);
            for (name, start, end) in crate::analyze::unbound_operators(text, &known) {
                diagnostics.push(warning(
                    buf.byte_to_position(start),
                    buf.byte_to_position(end),
                    &format!("unknown function: {name}"),
                ));
            }
        }
        diagnostics
    }

    /// Every name the environment knows: special forms plus engine globals.
    fn known_names(&self) -> HashSet<String> {
        let mut set: HashSet<String> = SPECIAL_FORMS.iter().map(|s| s.to_string()).collect();
        for global in self.engine.readable_globals(0) {
            set.insert(global.resolve().to_string());
        }
        set
    }

    /// Compile `text` for diagnostics only, sandboxed: a compiler panic is caught
    /// (it would otherwise take down the editor, since this runs in-process). The
    /// engine stays usable afterwards. The panic hook is silenced for the call so
    /// half-typed forms don't spam stderr.
    fn try_compile(&mut self, text: &str) -> CompileResult {
        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.engine.emit_raw_program_no_path(text.to_string())
        }));
        std::panic::set_hook(previous_hook);
        match outcome {
            Ok(Ok(_)) => CompileResult::Ok,
            Ok(Err(err)) => CompileResult::Error(err),
            Err(_) => CompileResult::Panicked,
        }
    }

    /// Completion candidates: the file's symbols, every environment binding the
    /// engine knows, and special forms — filtered by the prefix at point, with
    /// bindings in scope at the cursor ranked first.
    fn complete(&self, text: &str, line: usize, character: usize) -> Vec<Value> {
        let prefix = prefix_at(text, line, character);
        let in_scope =
            crate::analyze::bindings_in_scope_at(text, lsp_to_byte(text, line, character));

        let mut candidates: HashSet<String> = collect_identifiers(text);
        candidates.extend(SPECIAL_FORMS.iter().map(|s| s.to_string()));
        candidates.extend(in_scope.iter().cloned());
        for global in self.engine.readable_globals(0) {
            candidates.insert(global.resolve().to_string());
        }

        let matching = |want_local: bool| {
            let mut v: Vec<String> = candidates
                .iter()
                .filter(|c| {
                    in_scope.contains(*c) == want_local && *c != &prefix && c.starts_with(&prefix)
                })
                .cloned()
                .collect();
            v.sort();
            v
        };
        // In-scope locals first, then everything else.
        matching(true)
            .into_iter()
            .chain(matching(false))
            // Generous cap: a large single namespace (e.g. the ~180 `magit-*`
            // defines) must not crowd stdlib bindings like `map` out of the
            // results. The completion popup limits its own visible rows.
            .take(500)
            .map(|c| {
                // Tag special forms as keywords (LSP kind 14); leave others untyped.
                let kind = if SPECIAL_FORMS.contains(&c.as_str()) {
                    json!(14)
                } else {
                    Value::Null
                };
                json!({ "label": c, "insertText": c, "kind": kind })
            })
            .collect()
    }

    /// Builtin navigation requests. Returns the request id (a response is queued
    /// on the channel), or `None` for methods the builtin doesn't implement — the
    /// caller treats `None` as "no server support" for this request.
    pub fn request(&mut self, method: &str, params: &Value) -> Option<i64> {
        let uri = params["textDocument"]["uri"].as_str()?.to_string();
        let line = params["position"]["line"].as_u64()? as usize;
        let character = params["position"]["character"].as_u64()? as usize;
        match method {
            // declaration/type/impl all collapse to "the definition" here.
            "textDocument/definition"
            | "textDocument/declaration"
            | "textDocument/typeDefinition"
            | "textDocument/implementation" => Some(self.definition(&uri, line, character)),
            "textDocument/references" => Some(self.references(&uri, line, character)),
            "textDocument/hover" => Some(self.hover(&uri, line, character)),
            "textDocument/prepareRename" => Some(self.prepare_rename(&uri, line, character)),
            "textDocument/rename" => {
                let new_name = params["newName"].as_str().unwrap_or_default().to_string();
                Some(self.rename(&uri, line, character, &new_name))
            }
            _ => None,
        }
    }

    /// Go-to-definition: prefer a definition in the open document (so a config's
    /// own `define`/`define-command` and any overrides win), then the bundled doc
    /// index. A `'name` reference resolves like a bare one — the quote is a word
    /// boundary. Replies with a single `Location`, or `null` when unresolved.
    fn definition(&mut self, uri: &str, line: usize, character: usize) -> i64 {
        let text = self.docs.get(uri).cloned().unwrap_or_default();
        let result = symbol_at(&text, line, character)
            .and_then(|sym| self.locate_definition(&text, uri, &sym))
            .unwrap_or(Value::Null);
        self.respond(result)
    }

    fn locate_definition(&self, text: &str, uri: &str, sym: &str) -> Option<Value> {
        // 1) A definition in the open document (the user's own, or an override).
        for (i, line) in text.lines().enumerate() {
            if crate::runtime::define_name(line).as_deref() == Some(sym) {
                let col = word_occurrences(line, sym).first().copied().unwrap_or(0);
                return Some(location_json(uri, i, col, sym));
            }
        }
        // 2) The bundled library — only when its source is present on disk (a
        // checkout); an installed binary has no on-disk sources, so we decline.
        if let Some((_, file, ln)) = self.defs.get(sym)
            && Path::new(file).exists()
        {
            let uri = path_to_uri(Path::new(file));
            return Some(location_json(&uri, ln.saturating_sub(1), 0, sym));
        }
        None
    }

    /// Hover: the doc-comment for the symbol at point — the open document's own
    /// `;;` comment block if it defines the symbol, else the bundled editor-API
    /// doc. Replies with `null` when there's nothing documented.
    fn hover(&mut self, uri: &str, line: usize, character: usize) -> i64 {
        let text = self.docs.get(uri).cloned().unwrap_or_default();
        let result = symbol_at(&text, line, character)
            .and_then(|sym| self.doc_for(&text, &sym))
            .map(|doc| json!({ "contents": { "kind": "plaintext", "value": doc } }))
            .unwrap_or(Value::Null);
        self.respond(result)
    }

    fn doc_for(&self, text: &str, sym: &str) -> Option<String> {
        // The open document's own definition wins (so a config documents itself),
        // scraped by the same rules as the bundled library.
        let in_file = crate::runtime::scrape_docs(text, "")
            .into_iter()
            .find(|e| e.name == sym && !e.doc.is_empty())
            .map(|e| e.doc);
        in_file.or_else(|| {
            self.defs
                .get(sym)
                .map(|(doc, _, _)| doc.clone())
                .filter(|d| !d.is_empty())
        })
    }

    /// References to the symbol at point, within the open document: every word-
    /// boundary occurrence — which includes quoted `'name` references, since the
    /// quote is a boundary. (Cross-file references aren't tracked here.)
    fn references(&mut self, uri: &str, line: usize, character: usize) -> i64 {
        let text = self.docs.get(uri).cloned().unwrap_or_default();
        let mut locs: Vec<Value> = Vec::new();
        if let Some(sym) = symbol_at(&text, line, character) {
            for (li, l) in text.lines().enumerate() {
                for c in word_occurrences(l, &sym) {
                    locs.push(location_json(uri, li, c, &sym));
                }
            }
        }
        self.respond(json!(locs))
    }

    /// prepareRename: allow renaming only a symbol **defined in this document**
    /// (so a config renames its own commands, never a bundled or external one we
    /// can't fully update). Replies with the token's range, or `null` to decline.
    fn prepare_rename(&mut self, uri: &str, line: usize, character: usize) -> i64 {
        let text = self.docs.get(uri).cloned().unwrap_or_default();
        let result = symbol_at_span(&text, line, character)
            .filter(|(sym, _, _)| defined_in_doc(&text, sym))
            .map(|(_, s, e)| {
                json!({
                    "start": { "line": line, "character": s },
                    "end": { "line": line, "character": e }
                })
            })
            .unwrap_or(Value::Null);
        self.respond(result)
    }

    /// rename: a `WorkspaceEdit` replacing every word-boundary occurrence (quoted
    /// references included) of the symbol within this document. Scoped to the open
    /// document and only for symbols it defines — `prepare_rename` already gated
    /// that, and this re-checks so a direct rename can't escape the guard.
    fn rename(&mut self, uri: &str, line: usize, character: usize, new_name: &str) -> i64 {
        let text = self.docs.get(uri).cloned().unwrap_or_default();
        let mut edits: Vec<Value> = Vec::new();
        if let Some(sym) = symbol_at(&text, line, character)
            && defined_in_doc(&text, &sym)
        {
            let len: usize = sym.chars().map(char::len_utf16).sum();
            for (li, l) in text.lines().enumerate() {
                for c in word_occurrences(l, &sym) {
                    edits.push(json!({
                        "range": {
                            "start": { "line": li, "character": c },
                            "end": { "line": li, "character": c + len }
                        },
                        "newText": new_name
                    }));
                }
            }
        }
        let mut changes = serde_json::Map::new();
        changes.insert(uri.to_string(), Value::Array(edits));
        self.respond(json!({ "changes": changes }))
    }
}

/// Whether any top-level line of `text` defines `sym` (`define` / `define-syntax`
/// / `define-command`). Gates rename to a document's own symbols.
fn defined_in_doc(text: &str, sym: &str) -> bool {
    text.lines()
        .any(|l| crate::runtime::define_name(l).as_deref() == Some(sym))
}

/// The identifier covering `(line, character)` (UTF-16 column), or `None`.
/// Extends over identifier chars in both directions; a leading `'` is a boundary,
/// so a `'name` reference yields `name`.
fn symbol_at(text: &str, line: usize, character: usize) -> Option<String> {
    symbol_at_span(text, line, character).map(|(name, _, _)| name)
}

/// Like [`symbol_at`] but also returns the token's `(start, end)` UTF-16 columns
/// on its line — used to report a rename range. Position/column conversion goes
/// through the canonical rope implementation in [`crate::buffer::Buffer`]; only
/// the identifier-boundary walk is local.
fn symbol_at_span(text: &str, line: usize, character: usize) -> Option<(String, usize, usize)> {
    let line_str = text.lines().nth(line)?;
    let buf = crate::buffer::Buffer::new(line_str);
    let idx = buf.position_to_byte(0, character);
    let mut start = idx.min(line_str.len());
    while let Some(c) = line_str[..start].chars().next_back() {
        if !is_ident_char(c) {
            break;
        }
        start -= c.len_utf8();
    }
    let mut end = idx.min(line_str.len());
    while let Some(c) = line_str[end..].chars().next() {
        if !is_ident_char(c) {
            break;
        }
        end += c.len_utf8();
    }
    if start >= end {
        return None;
    }
    Some((
        line_str[start..end].to_string(),
        buf.byte_to_position(start).1,
        buf.byte_to_position(end).1,
    ))
}

/// UTF-16 column of every word-boundary occurrence of `sym` in a single line.
fn word_occurrences(line: &str, sym: &str) -> Vec<usize> {
    let buf = crate::buffer::Buffer::new(line);
    let mut out = Vec::new();
    let mut word_start: Option<usize> = None;
    // A trailing '\n' sentinel (never an identifier char) flushes a final word.
    for (b, c) in line
        .char_indices()
        .chain(std::iter::once((line.len(), '\n')))
    {
        if is_ident_char(c) {
            word_start.get_or_insert(b);
        } else if let Some(ws) = word_start.take()
            && &line[ws..b] == sym
        {
            out.push(buf.byte_to_position(ws).1);
        }
    }
    out
}

/// A one-line LSP `Location` JSON at `(line, character)` spanning `sym`.
fn location_json(uri: &str, line: usize, character: usize, sym: &str) -> Value {
    let len: usize = sym.chars().map(char::len_utf16).sum();
    json!({
        "uri": uri,
        "range": {
            "start": { "line": line, "character": character },
            "end": { "line": line, "character": character + len }
        }
    })
}

/// Map an LSP position `(line, utf16-col)` to a byte offset in `text`, via the
/// canonical rope conversion in [`crate::buffer::Buffer`]. For repeated
/// conversions over one document build the `Buffer` once instead.
fn lsp_to_byte(text: &str, line: usize, character: usize) -> usize {
    crate::buffer::Buffer::new(text).position_to_byte(line, character)
}

/// A warning-severity (2) LSP diagnostic — used for heuristic findings.
fn warning(start: (usize, usize), end: (usize, usize), message: &str) -> Value {
    diagnostic_with_severity(start, end, 2, message)
}

/// Turn a compiler error into an LSP diagnostic, using its span when present.
fn steel_error_diagnostic(text: &str, err: &steel::rerrs::SteelErr) -> Value {
    let buf = crate::buffer::Buffer::new(text);
    let (start, end) = match err.span() {
        Some(span) => (
            buf.byte_to_position(span.start as usize),
            buf.byte_to_position(span.end as usize),
        ),
        None => ((0, 0), (0, 1)),
    };
    // SteelErr's Display is "Error: Kind: message"; drop the "Error: " prefix.
    let message = err.to_string();
    let message = message.strip_prefix("Error: ").unwrap_or(&message);
    diagnostic(start, end, message)
}

/// Scheme symbol-constituent characters (a superset of identifiers; good enough
/// for completion-prefix and symbol scraping).
fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || "-_?!*+/<>=.:$%&~^".contains(c)
}

/// An LSP diagnostic object at the given LSP positions and severity.
fn diagnostic_with_severity(
    start: (usize, usize),
    end: (usize, usize),
    severity: u8,
    message: &str,
) -> Value {
    json!({
        "range": {
            "start": { "line": start.0, "character": start.1 },
            "end":   { "line": end.0,   "character": end.1 },
        },
        "severity": severity,
        "message": message,
    })
}

/// An error-severity (1) LSP diagnostic object at the given LSP positions.
fn diagnostic(start: (usize, usize), end: (usize, usize), message: &str) -> Value {
    diagnostic_with_severity(start, end, 1, message)
}

/// Balance-check Steel source: unmatched/mismatched `()[]{}`, unterminated
/// strings, and unterminated block comments. Aware of `;` line comments,
/// `"…"` strings (with `\` escapes), `#| … |#` (nestable) block comments,
/// `#;` datum comments, and `#\x` character literals.
fn steel_diagnostics(text: &str) -> Vec<Value> {
    let chars: Vec<char> = text.chars().collect();
    // LSP position (line, utf16-col) before each char, plus an end sentinel.
    let mut pos = Vec::with_capacity(chars.len() + 1);
    let (mut line, mut col) = (0usize, 0usize);
    for &c in &chars {
        pos.push((line, col));
        if c == '\n' {
            line += 1;
            col = 0;
        } else {
            col += c.len_utf16();
        }
    }
    pos.push((line, col));

    let mut diagnostics = Vec::new();
    let mut stack: Vec<usize> = Vec::new(); // indices of open delimiters
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            ';' => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '"' => {
                let start = i;
                i += 1;
                let mut closed = false;
                while i < chars.len() {
                    match chars[i] {
                        '\\' => i += 2,
                        '"' => {
                            i += 1;
                            closed = true;
                            break;
                        }
                        _ => i += 1,
                    }
                }
                if !closed {
                    diagnostics.push(diagnostic(
                        pos[start],
                        pos[chars.len()],
                        "unterminated string",
                    ));
                }
            }
            '#' if i + 1 < chars.len() && chars[i + 1] == '\\' => {
                // Character literal `#\x` — skip `#`, `\`, and the char.
                i = (i + 3).min(chars.len());
            }
            '#' if i + 1 < chars.len() && chars[i + 1] == '|' => {
                let start = i;
                i += 2;
                let mut depth = 1;
                while i < chars.len() && depth > 0 {
                    if chars[i] == '#' && i + 1 < chars.len() && chars[i + 1] == '|' {
                        depth += 1;
                        i += 2;
                    } else if chars[i] == '|' && i + 1 < chars.len() && chars[i + 1] == '#' {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                if depth > 0 {
                    diagnostics.push(diagnostic(
                        pos[start],
                        pos[chars.len()],
                        "unterminated block comment",
                    ));
                }
            }
            '#' if i + 1 < chars.len() && chars[i + 1] == ';' => i += 2, // datum comment marker
            '(' | '[' | '{' => {
                stack.push(i);
                i += 1;
            }
            ')' | ']' | '}' => {
                match stack.pop() {
                    Some(open) => {
                        let expected = match chars[open] {
                            '(' => ')',
                            '[' => ']',
                            _ => '}',
                        };
                        if c != expected {
                            diagnostics.push(diagnostic(
                                pos[i],
                                pos[i + 1],
                                &format!("mismatched '{c}', expected '{expected}'"),
                            ));
                        }
                    }
                    None => diagnostics.push(diagnostic(
                        pos[i],
                        pos[i + 1],
                        &format!("unmatched '{c}'"),
                    )),
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    for open in stack {
        let c = chars[open];
        diagnostics.push(diagnostic(
            pos[open],
            pos[open + 1],
            &format!("unmatched '{c}'"),
        ));
    }
    diagnostics
}

/// Scheme special forms — syntax, not values, so they aren't in the engine's
/// global table and must be offered for completion separately.
const SPECIAL_FORMS: &[&str] = &[
    "define",
    "lambda",
    "let",
    "let*",
    "letrec",
    "if",
    "cond",
    "case",
    "when",
    "unless",
    "begin",
    "set!",
    "quote",
    "quasiquote",
    "unquote",
    "and",
    "or",
    "define-syntax",
    "syntax-rules",
    "require",
    "provide",
    // Steel error handling: a form, not a readable global, so the analyzer's
    // vanilla engine doesn't surface it — list it so `(with-handler …)` (used in
    // the bundled library and configs) isn't flagged "unknown".
    "with-handler",
];

/// The identifier prefix immediately before the LSP position `(line, char)`.
/// The UTF-16 column maps to bytes via the canonical rope conversion.
fn prefix_at(text: &str, line: usize, character: usize) -> String {
    let line_str = text.lines().nth(line).unwrap_or("");
    let end = crate::buffer::Buffer::new(line_str).position_to_byte(0, character);
    let mut start = end;
    while let Some(c) = line_str[..start].chars().next_back() {
        if !is_ident_char(c) {
            break;
        }
        start -= c.len_utf8();
    }
    line_str[start..end].to_string()
}

/// Distinct identifiers appearing in `text` (excluding pure-number tokens).
fn collect_identifiers(text: &str) -> HashSet<String> {
    let mut set = HashSet::new();
    let mut current = String::new();
    for c in text.chars() {
        if is_ident_char(c) {
            current.push(c);
        } else if !current.is_empty() {
            set.insert(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        set.insert(current);
    }
    set.retain(|s| s.chars().next().is_some_and(|c| !c.is_ascii_digit()));
    set
}

/// Synthetic notification the reader thread sends once the server's stdout
/// is gone (process exit, crash, or a protocol error), so the editor can fail
/// in-flight requests and allow a restart. Independent of stderr, which may
/// stay open (or never have been piped) after stdout closes.
pub const EXITED_METHOD: &str = "$/macros/exited";

/// Read Content-Length-framed JSON-RPC messages until EOF, forwarding each,
/// then report the exit.
fn read_loop(stdout: std::process::ChildStdout, tx: UnboundedSender<Value>) {
    read_frames(BufReader::new(stdout), &tx);
    let _ = tx.unbounded_send(json!({ "method": EXITED_METHOD }));
}

/// Largest message body we'll allocate for. `Content-Length` comes straight
/// from the server, so a garbage header must not make us allocate gigabytes;
/// anything bigger is treated as a protocol error (the stream is abandoned,
/// which the editor then handles exactly like the server exiting).
const MAX_FRAME_BYTES: usize = 256 * 1024 * 1024;

/// The framing loop, generic over the reader so it's testable with a `Cursor`.
/// Shared with the DAP transport ([`crate::dap`]) — same wire format.
pub(crate) fn read_frames<R: BufRead>(mut reader: R, tx: &UnboundedSender<Value>) {
    loop {
        let mut content_length = 0usize;
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => return, // EOF or broken pipe
                Ok(_) => {}
            }
            let line = line.trim_end();
            if line.is_empty() {
                break; // blank line ends the header block
            }
            if let Some(value) = line.strip_prefix("Content-Length:") {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }
        if content_length == 0 {
            continue;
        }
        if content_length > MAX_FRAME_BYTES {
            let line = format!("protocol error: Content-Length {content_length} exceeds limit");
            let _ = tx.unbounded_send(json!({ "method": "$/stderr", "params": { "line": line } }));
            return; // the stream is unsynchronised now; treat it as dead
        }
        let mut buf = vec![0u8; content_length];
        if reader.read_exact(&mut buf).is_err() {
            return;
        }
        match serde_json::from_slice::<Value>(&buf) {
            Ok(value) => {
                if tx.unbounded_send(value).is_err() {
                    return; // editor dropped the receiver
                }
            }
            Err(_) => continue,
        }
    }
}

/// Convert a filesystem path to a `file://` URI, percent-encoding exactly what
/// the `url` crate's `Url::from_file_path` encodes (controls, non-ASCII, space,
/// `"`, `#`, `%`, `<`, `>`, `?`, `` ` ``, `{`, `}`) — most servers (rust-analyzer,
/// gopls, …) build their URIs that way, and diagnostics are keyed by URI, so a
/// stricter encoding (e.g. `@` → `%40`) would never match. On Windows, `\`
/// separators become `/` and the drive path gains the leading slash
/// (`file:///C:/…`).
pub fn path_to_uri(path: &Path) -> String {
    let path = path.to_string_lossy();
    let mut out = String::with_capacity(path.len() + 8);
    out.push_str("file://");
    if !path.starts_with('/') {
        out.push('/'); // Windows drive form, e.g. `C:\…`
    }
    for &b in path.as_bytes() {
        match b {
            b'\\' if cfg!(windows) => out.push('/'),
            0x00..=0x20
            | 0x7F..
            | b'"'
            | b'#'
            | b'%'
            | b'<'
            | b'>'
            | b'?'
            | b'`'
            | b'{'
            | b'}'
            | b'\\' => out.push_str(&format!("%{b:02X}")),
            _ => out.push(b as char),
        }
    }
    out
}

/// Canonicalise a server-sent URI so it compares equal to ours: a `file://`
/// URI is decoded and re-encoded with [`path_to_uri`]'s rules (servers differ
/// on what they escape — `%40` vs `@`, `%3A` vs `:`, hex case). Other schemes
/// pass through unchanged.
pub fn normalize_uri(uri: &str) -> String {
    if uri.starts_with("file://") {
        path_to_uri(Path::new(&uri_to_path(uri)))
    } else {
        uri.to_string()
    }
}

/// A spawned server's working directory must exist: otherwise `spawn` fails
/// with `NotFound`, indistinguishable from a missing executable.
pub(crate) fn check_spawn_cwd(cwd: &Path) -> Result<(), String> {
    if cwd.as_os_str().is_empty() || cwd.is_dir() {
        Ok(())
    } else {
        Err(format!(
            "working directory `{}` does not exist (or is not a directory)",
            cwd.display()
        ))
    }
}

/// Read `reader` (a child's stderr) line by line until EOF, passing each line
/// (lossily decoded, without the trailing newline) to `f`. Non-UTF-8 output
/// must not end the loop — dropping the pipe would let the server die on
/// SIGPIPE/EPIPE. Once `f` returns `false` (nobody's listening) the pipe is
/// still drained, just without forwarding.
pub(crate) fn for_each_stderr_line<R: std::io::Read>(reader: R, mut f: impl FnMut(String) -> bool) {
    let mut reader = BufReader::new(reader);
    let mut buf = Vec::new();
    let mut forwarding = true;
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => return,
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return,
        }
        if !forwarding {
            continue;
        }
        if buf.last() == Some(&b'\n') {
            buf.pop();
            if buf.last() == Some(&b'\r') {
                buf.pop();
            }
        }
        forwarding = f(String::from_utf8_lossy(&buf).into_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn parses_two_framed_messages() {
        let a = r#"{"jsonrpc":"2.0","id":1,"result":null}"#;
        let b = r#"{"method":"textDocument/publishDiagnostics"}"#;
        let stream = format!(
            "Content-Length: {}\r\n\r\n{a}Content-Length: {}\r\n\r\n{b}",
            a.len(),
            b.len()
        );

        let (tx, mut rx) = unbounded();
        read_frames(Cursor::new(stream.into_bytes()), &tx); // runs to EOF
        drop(tx);

        let first = futures::executor::block_on(rx.next()).unwrap();
        assert_eq!(first["id"], json!(1));
        let second = futures::executor::block_on(rx.next()).unwrap();
        assert_eq!(second["method"], json!("textDocument/publishDiagnostics"));
        assert!(futures::executor::block_on(rx.next()).is_none()); // closed, drained
    }

    #[test]
    fn oversized_frame_is_a_protocol_error_not_an_allocation() {
        // A bogus multi-gigabyte Content-Length must not be allocated; the
        // reader reports it and abandons the stream (= the server is dead).
        let ok = r#"{"jsonrpc":"2.0","id":1,"result":null}"#;
        let stream = format!(
            "Content-Length: 99999999999\r\n\r\n{{}}Content-Length: {}\r\n\r\n{ok}",
            ok.len()
        );
        let (tx, mut rx) = unbounded();
        read_frames(Cursor::new(stream.into_bytes()), &tx);
        drop(tx);
        let report = futures::executor::block_on(rx.next()).unwrap();
        assert_eq!(report["method"], json!("$/stderr"));
        assert!(
            report["params"]["line"]
                .as_str()
                .unwrap()
                .contains("exceeds")
        );
        assert!(futures::executor::block_on(rx.next()).is_none()); // nothing after it
    }

    #[test]
    fn client_capabilities_advertise_what_we_handle() {
        let caps = client_capabilities();
        assert_eq!(caps["workspace"]["applyEdit"], json!(true));
        assert_eq!(caps["workspace"]["configuration"], json!(true));
        assert_eq!(
            caps["textDocument"]["synchronization"]["didSave"],
            json!(true)
        );
        let code_action = &caps["textDocument"]["codeAction"];
        let kinds = code_action["codeActionLiteralSupport"]["codeActionKind"]["valueSet"]
            .as_array()
            .unwrap();
        for kind in ["quickfix", "refactor", "source", "source.organizeImports"] {
            assert!(kinds.contains(&json!(kind)), "missing kind {kind}");
        }
        assert_eq!(code_action["isPreferredSupport"], json!(true));
        assert_eq!(code_action["resolveSupport"]["properties"], json!(["edit"]));
    }

    #[test]
    fn default_server_replies() {
        let config = json!({ "items": [{ "section": "a" }, { "section": "b" }] });
        assert_eq!(
            default_server_reply("workspace/configuration", &config),
            Ok(json!([null, null]))
        );
        for method in [
            "window/workDoneProgress/create",
            "client/registerCapability",
            "workspace/semanticTokens/refresh",
        ] {
            assert_eq!(default_server_reply(method, &json!({})), Ok(Value::Null));
        }
        let err = default_server_reply("some/unknownThing", &json!({})).unwrap_err();
        assert_eq!(err.0, METHOD_NOT_FOUND);
    }

    #[test]
    fn save_option_shapes() {
        let caps = |sync: Value| json!({ "capabilities": { "textDocumentSync": sync } });
        assert_eq!(parse_save_option(&caps(json!(2))), Some(false));
        assert_eq!(parse_save_option(&caps(json!({ "change": 2 }))), None);
        assert_eq!(
            parse_save_option(&caps(json!({ "save": true }))),
            Some(false)
        );
        assert_eq!(parse_save_option(&caps(json!({ "save": false }))), None);
        assert_eq!(
            parse_save_option(&caps(json!({ "save": { "includeText": true } }))),
            Some(true)
        );
        assert_eq!(parse_save_option(&caps(json!({ "save": {} }))), Some(false));
        assert_eq!(parse_save_option(&json!({ "capabilities": {} })), None);
    }

    #[test]
    fn diagnostics_keep_their_raw_json() {
        let params = json!({ "uri": "file:///a.rs", "diagnostics": [{
            "range": { "start": {"line": 1, "character": 2}, "end": {"line": 1, "character": 5} },
            "severity": 2, "message": "unused", "code": "E0001", "source": "rustc",
            "data": { "fix": 7 }
        }] });
        let (_, diags) = parse_diagnostics(&params);
        assert_eq!(diags[0].raw["code"], json!("E0001"));
        assert_eq!(diags[0].raw["source"], json!("rustc"));
        assert_eq!(diags[0].raw["data"], json!({ "fix": 7 }));
        assert_eq!(diags[0].raw, params["diagnostics"][0]);
    }

    #[test]
    fn edit_less_code_actions_are_marked_for_resolve() {
        let result = json!([
            { "title": "Lazy", "kind": "quickfix", "data": { "id": 3 } },
            { "title": "Eager", "edit": { "changes": {} } },
            { "title": "Bare", "command": "do.thing" }
        ]);
        let actions = parse_code_actions(&result);
        assert_eq!(actions[0].resolve.as_ref(), Some(&result[0]));
        assert!(actions[1].resolve.is_none());
        assert!(actions[2].resolve.is_none());
    }

    #[test]
    fn progress_text_formats_begin_and_report() {
        let begin = json!({ "kind": "begin", "title": "Indexing", "percentage": 0 });
        assert_eq!(
            progress_text("rust", "Indexing", &begin),
            "rust: Indexing 0%"
        );
        let report = json!({ "kind": "report", "message": "3/10", "percentage": 30 });
        assert_eq!(
            progress_text("rust", "Indexing", &report),
            "rust: Indexing 3/10 30%"
        );
    }

    #[test]
    fn text_edits_past_line_end_do_not_eat_the_newline() {
        // A server's "end of line" (a large character) must clamp before the
        // terminator, not swallow it and join the next line.
        let edits = [TextEdit {
            start: (0, 3),
            end: (0, 999),
            new_text: "!".into(),
        }];
        assert_eq!(apply_edits_to_text("abc\r\ndef\n", &edits), "abc!\r\ndef\n");
        let past_eof = [TextEdit {
            start: (9, 0),
            end: (9, 0),
            new_text: "tail".into(),
        }];
        assert_eq!(apply_edits_to_text("ab\ncd", &past_eof), "ab\ncdtail");
    }

    use futures::StreamExt as _;

    #[test]
    fn steel_diagnostics_balance_checks() {
        // Well-formed source is clean.
        assert!(steel_diagnostics("(define (f x) (+ x 1))").is_empty());
        // Parens inside strings and comments don't count.
        assert!(steel_diagnostics(r#"(display "((")  ; ((("#).is_empty());

        // An unclosed form reports the unmatched opener at its position.
        let diags = steel_diagnostics("(define (f x)");
        assert_eq!(diags.len(), 1);
        assert_eq!(
            diags[0]["range"]["start"],
            json!({ "line": 0, "character": 0 })
        );
        assert!(diags[0]["message"].as_str().unwrap().contains("unmatched"));

        // Mismatched delimiter kinds are flagged.
        let diags = steel_diagnostics("(]");
        assert_eq!(diags.len(), 1);
        assert!(diags[0]["message"].as_str().unwrap().contains("mismatched"));
    }

    #[test]
    fn steel_completion_is_environment_aware() {
        let (server, _rx) = SteelLsp::start(true, &["set-lsp".to_string()]);

        // Buffer symbols complete, the prefix itself is excluded.
        let items = server.complete("(define foobar 1)\n(foo", 1, 4);
        let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();
        assert!(labels.contains(&"foobar"), "got {labels:?}");
        assert!(!labels.contains(&"foo"));

        // Special forms complete.
        let items = server.complete("(defi", 0, 5);
        let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();
        assert!(labels.contains(&"define"), "got {labels:?}");

        // Stdlib bindings the engine knows about complete (e.g. `map`).
        let items = server.complete("(ma", 0, 3);
        let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();
        assert!(
            labels.contains(&"map"),
            "expected stdlib `map`, got {labels:?}"
        );

        // A 'symbol command reference completes: the leading quote is a boundary,
        // so the prefix is the name and the candidate inserts after the quote.
        // This is the canonical form for keybindings/commands across the API.
        let src = "(define-key \"m\" \"c\" 'set-l";
        let items = server.complete(src, 0, src.chars().count());
        let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();
        assert!(
            labels.contains(&"set-lsp"),
            "quoted 'set-l should complete set-lsp, got {labels:?}"
        );
    }

    #[test]
    fn builtin_definition_and_references_resolve_symbols() {
        let (mut server, mut rx) = SteelLsp::start(true, &[]);
        let _ = futures::executor::block_on(rx.next()).unwrap(); // initialize

        let uri = "file:///cfg.scm";
        // A command, a quoted reference (keybinding), and a call.
        let text = "(define (my-cmd) 1)\n(bind-key \"m\" \"c\" 'my-cmd)\n(my-cmd)";
        server.did_open(uri, text);
        let _ = futures::executor::block_on(rx.next()).unwrap(); // diagnostics

        // Go-to-definition from the *quoted* reference on line 1 jumps to the
        // define on line 0 — the quote is a word boundary, so symbols resolve.
        let col = text.lines().nth(1).unwrap().find("my-cmd").unwrap();
        let params = json!({"textDocument":{"uri":uri},"position":{"line":1,"character":col}});
        let id = server.request("textDocument/definition", &params).unwrap();
        let resp = futures::executor::block_on(rx.next()).unwrap();
        assert_eq!(resp["id"], json!(id));
        assert_eq!(resp["result"]["uri"], json!(uri));
        assert_eq!(resp["result"]["range"]["start"]["line"], json!(0));

        // References include the define, the quoted use, and the call.
        let id = server.request("textDocument/references", &params).unwrap();
        let resp = futures::executor::block_on(rx.next()).unwrap();
        assert_eq!(resp["id"], json!(id));
        let lines: Vec<i64> = resp["result"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["range"]["start"]["line"].as_i64().unwrap())
            .collect();
        assert!(lines.contains(&0), "define site missing: {lines:?}");
        assert!(
            lines.contains(&1),
            "quoted ref counts as a reference: {lines:?}"
        );
        assert!(lines.contains(&2), "call site missing: {lines:?}");
    }

    #[test]
    fn builtin_hover_shows_doc_comment() {
        let (mut server, mut rx) = SteelLsp::start(true, &[]);
        let _ = futures::executor::block_on(rx.next()).unwrap(); // initialize

        let uri = "file:///cfg.scm";
        // A documented command, referenced (quoted) from a keybinding.
        let text = ";; Greet the user.\n(define (greet) 1)\n(bind-key \"m\" \"c\" 'greet)";
        server.did_open(uri, text);
        let _ = futures::executor::block_on(rx.next()).unwrap(); // diagnostics

        // Hover on the quoted reference shows the symbol's doc-comment.
        let col = text.lines().nth(2).unwrap().find("greet").unwrap();
        let params = json!({"textDocument":{"uri":uri},"position":{"line":2,"character":col}});
        let id = server.request("textDocument/hover", &params).unwrap();
        let resp = futures::executor::block_on(rx.next()).unwrap();
        assert_eq!(resp["id"], json!(id));
        assert_eq!(
            resp["result"]["contents"]["value"],
            json!("Greet the user.")
        );
    }

    #[test]
    fn builtin_rename_scopes_to_document_defines() {
        let (mut server, mut rx) = SteelLsp::start(true, &[]);
        let _ = futures::executor::block_on(rx.next()).unwrap(); // initialize

        let uri = "file:///cfg.scm";
        let text = "(define (greet) 1)\n(bind-key \"m\" \"c\" 'greet)\n(greet)";
        server.did_open(uri, text);
        let _ = futures::executor::block_on(rx.next()).unwrap(); // diagnostics

        let col = text.lines().nth(1).unwrap().find("greet").unwrap();
        let mut pos = json!({"textDocument":{"uri":uri},"position":{"line":1,"character":col}});

        // prepareRename accepts a symbol defined in this document.
        let id = server.request("textDocument/prepareRename", &pos).unwrap();
        let resp = futures::executor::block_on(rx.next()).unwrap();
        assert_eq!(resp["id"], json!(id));
        assert!(
            !resp["result"].is_null(),
            "own symbol is renameable: {resp}"
        );

        // rename rewrites every occurrence — define, quoted reference, and call.
        pos["newName"] = json!("welcome");
        let id = server.request("textDocument/rename", &pos).unwrap();
        let resp = futures::executor::block_on(rx.next()).unwrap();
        assert_eq!(resp["id"], json!(id));
        let edits = resp["result"]["changes"][uri].as_array().unwrap();
        assert_eq!(edits.len(), 3, "define + quoted + call: {edits:?}");
        assert!(edits.iter().all(|e| e["newText"] == json!("welcome")));

        // A symbol NOT defined here (a bundled command) is declined — we'd only
        // half-rename it.
        let text2 = "(bind-key \"m\" \"c\" 'magit-status)";
        server.did_change(uri, text2);
        let _ = futures::executor::block_on(rx.next()).unwrap(); // diagnostics
        let col = text2.find("magit-status").unwrap();
        let pos2 = json!({"textDocument":{"uri":uri},"position":{"line":0,"character":col}});
        let id = server.request("textDocument/prepareRename", &pos2).unwrap();
        let resp = futures::executor::block_on(rx.next()).unwrap();
        assert_eq!(resp["id"], json!(id));
        assert!(
            resp["result"].is_null(),
            "a symbol defined elsewhere isn't renameable here: {resp}"
        );
    }

    #[test]
    fn steel_lsp_reports_semantic_errors() {
        let (mut server, mut rx) = SteelLsp::start(false, &[]);
        let _init = futures::executor::block_on(rx.next()).unwrap();

        // Balanced but malformed special form → a compiler diagnostic with a span.
        server.did_open("file:///s.scm", "(if 1 2 3 4)");
        let msg = futures::executor::block_on(rx.next()).unwrap();
        let diags = msg["params"]["diagnostics"].as_array().unwrap();
        assert_eq!(diags.len(), 1, "expected a semantic diagnostic: {msg}");
        assert!(diags[0]["message"].as_str().unwrap().contains("if"));

        // Valid code using a stdlib function compiles clean.
        server.did_change("file:///s.scm", "(define (f x) (map (lambda (y) y) x))");
        let msg = futures::executor::block_on(rx.next()).unwrap();
        assert!(
            msg["params"]["diagnostics"].as_array().unwrap().is_empty(),
            "valid code should have no diagnostics: {msg}"
        );
    }

    #[test]
    fn steel_lsp_warns_on_unknown_function() {
        let (mut server, mut rx) = SteelLsp::start(false, &[]);
        let _init = futures::executor::block_on(rx.next()).unwrap();

        // Compiles clean, but calls an undefined function → a warning (severity 2).
        server.did_open("file:///u.scm", "(define (f x) (frobnicate x))");
        let msg = futures::executor::block_on(rx.next()).unwrap();
        let diags = msg["params"]["diagnostics"].as_array().unwrap();
        assert_eq!(diags.len(), 1, "{msg}");
        assert_eq!(diags[0]["severity"], json!(2));
        assert!(diags[0]["message"].as_str().unwrap().contains("frobnicate"));

        // Defining it clears the warning (forward reference within the file).
        server.did_change(
            "file:///u.scm",
            "(define (f x) (frobnicate x))\n(define (frobnicate y) y)",
        );
        let msg = futures::executor::block_on(rx.next()).unwrap();
        assert!(
            msg["params"]["diagnostics"].as_array().unwrap().is_empty(),
            "{msg}"
        );
    }

    #[test]
    fn editor_api_is_scoped_to_the_macros_scheme_server() {
        // The split that gives `macros-steel` mode its reason to exist: a call to
        // an editor function is "unknown" on the plain `scheme` server (just the
        // interpreter) but in scope on the editor-aware `macros-scheme` server.
        let editor_call = "(define (f) (buffer-line))";

        // Plain server: editor API not seeded → unknown-function warning.
        let (mut plain, mut rx) = SteelLsp::start(false, &[]);
        let _ = futures::executor::block_on(rx.next()).unwrap();
        plain.did_open("file:///plain.scm", editor_call);
        let msg = futures::executor::block_on(rx.next()).unwrap();
        let diags = msg["params"]["diagnostics"].as_array().unwrap();
        assert_eq!(diags.len(), 1, "plain server should flag editor API: {msg}");
        assert!(
            diags[0]["message"]
                .as_str()
                .unwrap()
                .contains("buffer-line")
        );

        // Editor-aware server: `buffer-line` is a seeded global → clean.
        let (engine, _outcomes) = crate::script::ScriptEngine::new();
        let (mut aware, mut rx) = SteelLsp::start(true, engine.global_names());
        let _ = futures::executor::block_on(rx.next()).unwrap();
        aware.did_open("file:///cfg.macros.scm", editor_call);
        let msg = futures::executor::block_on(rx.next()).unwrap();
        assert!(
            msg["params"]["diagnostics"].as_array().unwrap().is_empty(),
            "macros-scheme server should know the editor API: {msg}"
        );
    }

    #[test]
    fn steel_lsp_survives_compiler_panics() {
        // `(define)` is balanced but malformed. It panicked Steel's compiler
        // before steel-core 0.8.3 (caught by `try_compile`'s sandbox); it is
        // now a plain compile error. Either way: no crash, and the server must
        // keep working.
        let (mut server, mut rx) = SteelLsp::start(false, &[]);
        let _init = futures::executor::block_on(rx.next()).unwrap();

        server.did_open("file:///p.scm", "(define)");
        let msg = futures::executor::block_on(rx.next()).unwrap();
        assert!(msg["params"]["diagnostics"].is_array(), "{msg}");

        // Still alive: a subsequent real error is still reported.
        server.did_change("file:///p.scm", "(if 1 2 3 4)");
        let msg = futures::executor::block_on(rx.next()).unwrap();
        assert!(!msg["params"]["diagnostics"].as_array().unwrap().is_empty());
    }

    #[test]
    fn parse_completion_extracts_detail_and_kind() {
        let result = json!([
            { "label": "foo", "kind": 3,
              "labelDetails": { "detail": "(x: i32)", "description": "bool" } },
            { "label": "bar", "insertText": "bar!", "detail": "a constant" },
            { "label": "begin", "kind": 14 },
        ]);
        let items = parse_completion(&result);
        assert_eq!(items.len(), 3);
        // Function with labelDetails → "fn" + combined signature.
        assert_eq!(items[0].kind.as_deref(), Some("fn"));
        assert_eq!(items[0].detail.as_deref(), Some("(x: i32) bool"));
        // insertText honoured; plain detail fallback; no kind.
        assert_eq!(items[1].insert_text, "bar!");
        assert_eq!(items[1].detail.as_deref(), Some("a constant"));
        assert_eq!(items[1].kind, None);
        // Keyword kind.
        assert_eq!(items[2].kind.as_deref(), Some("kw"));
    }

    #[test]
    fn parse_locations_handles_all_shapes() {
        // Single Location.
        let single = json!({
            "uri": "file:///a.rs",
            "range": { "start": {"line": 3, "character": 5}, "end": {"line": 3, "character": 8} }
        });
        let locs = parse_locations(&single);
        assert_eq!(
            locs,
            vec![Location {
                uri: "file:///a.rs".into(),
                start: (3, 5),
                end: (3, 8)
            }]
        );

        // Array of LocationLink (prefers targetSelectionRange).
        let links = json!([{
            "targetUri": "file:///b.rs",
            "targetRange": { "start": {"line": 0, "character": 0}, "end": {"line": 9, "character": 0} },
            "targetSelectionRange": { "start": {"line": 1, "character": 4}, "end": {"line": 1, "character": 7} }
        }]);
        let locs = parse_locations(&links);
        assert_eq!(
            locs,
            vec![Location {
                uri: "file:///b.rs".into(),
                start: (1, 4),
                end: (1, 7)
            }]
        );

        // Null → empty.
        assert!(parse_locations(&Value::Null).is_empty());
    }

    #[test]
    fn parse_hover_renders_markup_and_arrays() {
        assert_eq!(
            parse_hover(&json!({ "contents": "plain" })).as_deref(),
            Some("plain")
        );
        assert_eq!(
            parse_hover(&json!({ "contents": { "kind": "markdown", "value": "**x**" } }))
                .as_deref(),
            Some("**x**")
        );
        assert_eq!(
            parse_hover(&json!({ "contents": ["a", { "language": "rust", "value": "b" }] }))
                .as_deref(),
            Some("a\nb")
        );
        assert_eq!(parse_hover(&json!({ "contents": "" })), None);
        assert_eq!(parse_hover(&Value::Null), None);
    }

    #[test]
    fn parse_symbols_flattens_hierarchy_and_flat() {
        // Hierarchical DocumentSymbol with children → fallback uri + depth.
        let hierarchical = json!([{
            "name": "Foo", "kind": 23,
            "range": { "start": {"line": 0, "character": 0}, "end": {"line": 5, "character": 0} },
            "selectionRange": { "start": {"line": 0, "character": 7}, "end": {"line": 0, "character": 10} },
            "children": [{
                "name": "bar", "kind": 6,
                "range": { "start": {"line": 1, "character": 4}, "end": {"line": 2, "character": 4} },
                "selectionRange": { "start": {"line": 1, "character": 8}, "end": {"line": 1, "character": 11} }
            }]
        }]);
        let syms = parse_symbols(&hierarchical, "file:///f.rs");
        assert_eq!(syms.len(), 2);
        assert_eq!(
            (syms[0].name.as_str(), syms[0].depth, syms[0].start),
            ("Foo", 0, (0, 7))
        );
        assert_eq!(
            (
                syms[1].name.as_str(),
                syms[1].depth,
                syms[1].container.as_str()
            ),
            ("bar", 1, "Foo")
        );
        assert_eq!(syms[1].uri, "file:///f.rs");

        // Flat SymbolInformation carries its own location.
        let flat = json!([{
            "name": "baz", "kind": 12,
            "location": { "uri": "file:///g.rs", "range": { "start": {"line": 9, "character": 0}, "end": {"line": 9, "character": 3} } }
        }]);
        let syms = parse_symbols(&flat, "file:///f.rs");
        assert_eq!(syms[0].uri, "file:///g.rs");
        assert_eq!(syms[0].start, (9, 0));
    }

    #[test]
    fn parse_workspace_edit_both_shapes_and_apply() {
        // documentChanges shape.
        let dc = json!({
            "documentChanges": [{
                "textDocument": { "uri": "file:///a.rs", "version": 1 },
                "edits": [
                    { "range": { "start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 3} }, "newText": "bar" }
                ]
            }]
        });
        let we = parse_workspace_edit(&dc);
        assert_eq!(we.changes.len(), 1);
        assert_eq!(we.changes[0].0, "file:///a.rs");

        // changes-map shape.
        let map = json!({
            "changes": {
                "file:///a.rs": [
                    { "range": { "start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 3} }, "newText": "bar" }
                ]
            }
        });
        let we = parse_workspace_edit(&map);
        assert_eq!(we.changes[0].0, "file:///a.rs");

        // Applying multiple edits to a line, last-first, stays correct.
        let edits = vec![
            TextEdit {
                start: (0, 0),
                end: (0, 3),
                new_text: "X".into(),
            },
            TextEdit {
                start: (0, 4),
                end: (0, 7),
                new_text: "Y".into(),
            },
        ];
        assert_eq!(apply_edits_to_text("foo bar baz", &edits), "X Y baz");
    }

    #[test]
    fn parse_code_actions_distinguishes_command_and_edit() {
        let result = json!([
            { "title": "Fix it", "edit": { "changes": { "file:///a.rs": [
                { "range": { "start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1} }, "newText": "z" }
            ] } } },
            { "title": "Run", "command": "do.thing", "arguments": [1, 2] },
            { "title": "Nested", "command": { "command": "nested.cmd", "arguments": ["a"] } }
        ]);
        let actions = parse_code_actions(&result);
        assert_eq!(actions.len(), 3);
        assert!(actions[0].edit.is_some() && actions[0].command.is_none());
        assert_eq!(actions[1].command.as_ref().unwrap().0, "do.thing");
        assert_eq!(actions[2].command.as_ref().unwrap().0, "nested.cmd");

        let help = json!({ "signatures": [{ "label": "fn foo(x: i32)" }], "activeSignature": 0 });
        assert_eq!(
            parse_signature_help(&help).as_deref(),
            Some("fn foo(x: i32)")
        );
    }

    #[test]
    fn parse_call_hierarchy_extracts_items() {
        let incoming = json!([{
            "from": {
                "name": "caller", "uri": "file:///c.rs",
                "range": { "start": {"line": 10, "character": 0}, "end": {"line": 12, "character": 0} },
                "selectionRange": { "start": {"line": 10, "character": 3}, "end": {"line": 10, "character": 9} }
            },
            "fromRanges": []
        }]);
        let calls = parse_call_hierarchy(&incoming, "from");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "caller");
        assert_eq!(calls[0].1.start, (10, 3)); // prefers selectionRange
        assert_eq!(calls[0].1.uri, "file:///c.rs");
        // Wrong key / empty → nothing.
        assert!(parse_call_hierarchy(&incoming, "to").is_empty());
        assert!(parse_call_hierarchy(&Value::Null, "from").is_empty());
    }

    #[test]
    fn decode_semantic_tokens_delta_decodes() {
        let legend = vec![
            "function".to_string(),
            "variable".to_string(),
            "keyword".to_string(),
        ];
        // Token A: line 0 col 0 len 3 type function
        // Token B: same line, +4 cols, len 2, type variable
        // Token C: +2 lines, col 1, len 5, type keyword
        let result = json!({ "data": [0,0,3,0,0,  0,4,2,1,0,  2,1,5,2,0] });
        let toks = decode_semantic_data(&semantic_data(&result), &legend);
        assert_eq!(
            toks,
            vec![
                (0, 0, 3, "function".to_string()),
                (0, 4, 2, "variable".to_string()),
                (2, 1, 5, "keyword".to_string()),
            ]
        );

        // Delta: splice [start=5, deleteCount=5] with a new variable token,
        // producing function + variable (the second token replaced).
        let delta = json!({ "resultId": "2", "edits": [{ "start": 5, "deleteCount": 5, "data": [0,4,2,1,0] }] });
        let merged = apply_semantic_delta(&semantic_data(&result), &delta);
        let toks = decode_semantic_data(&merged, &legend);
        assert_eq!(toks[1], (0, 4, 2, "variable".to_string()));
        assert_eq!(semantic_result_id(&delta).as_deref(), Some("2"));

        // Legend parse from an initialize result.
        let init = json!({ "capabilities": { "semanticTokensProvider": {
            "legend": { "tokenTypes": ["a", "b"], "tokenModifiers": [] }
        } } });
        assert_eq!(
            parse_semantic_legend(&init),
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn prepare_rename_all_shapes() {
        // Plain Range.
        let r = json!({ "start": {"line": 2, "character": 4}, "end": {"line": 2, "character": 9} });
        assert_eq!(
            parse_prepare_rename(&r),
            Some(PrepareRename {
                placeholder: None,
                range: Some(((2, 4), (2, 9)))
            })
        );
        // {range, placeholder}.
        let rp = json!({ "range": r, "placeholder": "foo" });
        assert_eq!(
            parse_prepare_rename(&rp).unwrap().placeholder.as_deref(),
            Some("foo")
        );
        // defaultBehavior.
        assert_eq!(
            parse_prepare_rename(&json!({ "defaultBehavior": true })),
            Some(PrepareRename {
                placeholder: None,
                range: None
            })
        );
        // null → not renameable.
        assert_eq!(parse_prepare_rename(&Value::Null), None);
        // capability flag.
        let init = json!({ "capabilities": { "renameProvider": { "prepareProvider": true } } });
        assert!(parse_prepare_rename_support(&init));
        assert!(!parse_prepare_rename_support(
            &json!({ "capabilities": { "renameProvider": true } })
        ));
    }

    #[test]
    fn completion_triggers_parse_from_capabilities() {
        let init = json!({
            "capabilities": { "completionProvider": { "triggerCharacters": [".", "#", "~"] } }
        });
        assert_eq!(parse_completion_triggers(&init), vec![".", "#", "~"]);
        // No completionProvider, or no triggerCharacters → empty.
        assert!(parse_completion_triggers(&json!({ "capabilities": {} })).is_empty());
        assert!(
            parse_completion_triggers(&json!({
                "capabilities": { "completionProvider": {} }
            }))
            .is_empty()
        );
    }

    #[test]
    fn completion_detail_and_label_collapse_newlines() {
        // ocamllsp puts multi-line type signatures in `detail`; the popup renders
        // label+detail on one line via `shape_line`, which panics on a newline.
        let item = &parse_completion(&json!([{
            "label": "map",
            "detail": "('a -> 'b) ->\n  'a list ->\n  'b list",
            "insertText": "map"
        }]))[0];
        assert!(!item.label.contains('\n'));
        let detail = item.detail.as_deref().unwrap();
        assert!(!detail.contains('\n') && !detail.contains('\t'));
        assert_eq!(detail, "('a -> 'b) -> 'a list -> 'b list");

        // A newline in the label is collapsed for display, but a missing
        // insertText still inserts the raw label text.
        let item = &parse_completion(&json!([{ "label": "a\nb" }]))[0];
        assert_eq!(item.label, "a b");
        assert_eq!(item.insert_text, "a\nb");
    }

    #[test]
    fn completion_resolve_and_code_lens() {
        // Resolve fills documentation + additionalTextEdits onto the item.
        let mut item = parse_completion(&json!([{ "label": "foo" }]))[0].clone();
        assert!(item.documentation.is_none() && !item.resolved);
        let resolved = json!({
            "documentation": { "kind": "markdown", "value": "Does the thing." },
            "additionalTextEdits": [
                { "range": { "start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0} }, "newText": "use x;\n" }
            ]
        });
        merge_resolved_completion(&mut item, &resolved);
        assert_eq!(item.documentation.as_deref(), Some("Does the thing."));
        assert_eq!(item.additional_edits.len(), 1);
        assert!(item.resolved);

        // Code lens: inlined command vs needs-resolve.
        let lenses = parse_code_lenses(&json!([
            { "range": { "start": {"line": 3, "character": 0}, "end": {"line": 3, "character": 4} },
              "command": { "title": "3 refs", "command": "x" } },
            { "range": { "start": {"line": 8, "character": 0}, "end": {"line": 8, "character": 4} } }
        ]));
        assert_eq!(lenses.len(), 2);
        assert_eq!(lenses[0].title.as_deref(), Some("3 refs"));
        assert_eq!((lenses[0].line, lenses[1].title.clone()), (3, None));
        assert_eq!(
            parse_code_lens_title(&json!({ "command": { "title": "impl" } })).as_deref(),
            Some("impl")
        );
    }

    #[test]
    fn same_position_inserts_apply_in_array_order() {
        // LSP: inserts at one position appear in the result in array order.
        let ins = |t: &str| TextEdit {
            start: (0, 3),
            end: (0, 3),
            new_text: t.into(),
        };
        assert_eq!(
            apply_edits_to_text("abcdef", &[ins("1"), ins("2"), ins("3")]),
            "abc123def"
        );
        // An insert and a replace starting at the same position: the insert
        // (first in the array) precedes the replacement text.
        let edits = [
            ins("<"),
            TextEdit {
                start: (0, 3),
                end: (0, 4),
                new_text: "D".into(),
            },
        ];
        assert_eq!(apply_edits_to_text("abcdef", &edits), "abc<Def");
    }

    #[test]
    fn completion_text_edit_wins_and_resolved_detail_is_single_line() {
        let item = &parse_completion(&json!([{
            "label": "foo",
            "insertText": "wrong",
            "textEdit": {
                "range": { "start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1} },
                "newText": "right"
            }
        }]))[0];
        assert_eq!(item.insert_text, "right");

        let mut item = parse_completion(&json!([{ "label": "map" }]))[0].clone();
        merge_resolved_completion(&mut item, &json!({ "detail": "'a ->\n  'b" }));
        assert_eq!(item.detail.as_deref(), Some("'a -> 'b"));
    }

    #[test]
    fn stderr_lines_survive_invalid_utf8() {
        let input: &[u8] = b"one\n\xff\xfebad\r\nthree";
        let mut lines = Vec::new();
        for_each_stderr_line(input, |l| {
            lines.push(l);
            true
        });
        assert_eq!(lines, vec!["one", "\u{FFFD}\u{FFFD}bad", "three"]);

        // After the sink stops accepting, the rest is still drained (no panic,
        // no further calls).
        let mut calls = 0;
        for_each_stderr_line(&b"a\nb\nc\n"[..], |_| {
            calls += 1;
            false
        });
        assert_eq!(calls, 1);
    }

    #[test]
    fn spawn_with_missing_cwd_reports_the_directory() {
        let missing = Path::new("/definitely/not/a/real/dir/macros-test");
        let err = LspClient::start("sh", missing).err().unwrap();
        assert!(err.contains("working directory"), "{err}");
        assert!(!err.contains("not found in PATH"), "{err}");
    }

    #[test]
    fn uri_encoding_matches_url_crate_and_normalizes() {
        // Characters the `url` crate leaves alone in file paths stay literal.
        assert_eq!(
            path_to_uri(Path::new("/tmp/a@b+c=d,e;f:g!h$i&j'k(l)m*n[o]p~q.rs")),
            "file:///tmp/a@b+c=d,e;f:g!h$i&j'k(l)m*n[o]p~q.rs"
        );
        assert_eq!(
            path_to_uri(Path::new("/tmp/a b#c?d%e{f}g`h\"i<j>.rs")),
            "file:///tmp/a%20b%23c%3Fd%25e%7Bf%7Dg%60h%22i%3Cj%3E.rs"
        );
        // A server URI escaping more (or less, or lower-case hex) than we do
        // normalises to ours, so diagnostics keyed by it match.
        let ours = path_to_uri(Path::new("/tmp/my dir/@scope/a.ts"));
        assert_eq!(normalize_uri("file:///tmp/my%20dir/%40scope/a.ts"), ours);
        let (uri, _) = parse_diagnostics(&json!({
            "uri": "file:///tmp/my%20dir/%40scope/a.ts", "diagnostics": []
        }));
        assert_eq!(uri, ours);
        assert_eq!(normalize_uri("untitled:Untitled-1"), "untitled:Untitled-1");
    }

    #[test]
    fn uri_path_roundtrip() {
        assert_eq!(uri_to_path("file:///Users/x/a.rs"), "/Users/x/a.rs");
        assert_eq!(uri_to_path("/already/a/path"), "/already/a/path");
    }

    #[test]
    fn uri_percent_encoding_roundtrips() {
        // A plain path is untouched apart from the scheme.
        assert_eq!(
            path_to_uri(Path::new("/Users/x/a.rs")),
            "file:///Users/x/a.rs"
        );
        // Spaces and non-ASCII must be percent-encoded on the way out (that's
        // what rust-analyzer et al. send back) and decoded on the way in.
        let space = Path::new("/Users/x/my project/a b.rs");
        assert_eq!(path_to_uri(space), "file:///Users/x/my%20project/a%20b.rs");
        assert_eq!(
            uri_to_path(&path_to_uri(space)),
            "/Users/x/my project/a b.rs"
        );

        let unicode = Path::new("/tmp/café/naïve.rs");
        assert_eq!(path_to_uri(unicode), "file:///tmp/caf%C3%A9/na%C3%AFve.rs");
        assert_eq!(uri_to_path(&path_to_uri(unicode)), "/tmp/café/naïve.rs");

        // Other reserved characters round-trip too.
        let odd = Path::new("/tmp/a#b?c&d.rs");
        assert_eq!(uri_to_path(&path_to_uri(odd)), "/tmp/a#b?c&d.rs");

        // Decoding tolerates malformed escapes (passed through literally).
        assert_eq!(uri_to_path("file:///x/100%zz"), "/x/100%zz");
        assert_eq!(uri_to_path("file:///x/50%"), "/x/50%");
    }

    #[test]
    fn steel_lsp_drives_the_full_protocol() {
        let (mut server, mut rx) = SteelLsp::start(false, &[]);

        // 1. Handshake: an initialize response advertising full sync.
        let init = futures::executor::block_on(rx.next()).unwrap();
        assert_eq!(init["result"]["capabilities"]["textDocumentSync"], json!(1));

        // 2. didOpen on broken source publishes diagnostics for that URI.
        server.did_open("file:///x.scm", "(define (f");
        let published = futures::executor::block_on(rx.next()).unwrap();
        assert_eq!(
            published["method"],
            json!("textDocument/publishDiagnostics")
        );
        assert_eq!(published["params"]["uri"], json!("file:///x.scm"));
        assert!(
            !published["params"]["diagnostics"]
                .as_array()
                .unwrap()
                .is_empty()
        );

        // 3. Fixing the source republishes an empty (cleared) diagnostic list.
        server.did_change("file:///x.scm", "(define (f x) x)");
        let cleared = futures::executor::block_on(rx.next()).unwrap();
        assert!(
            cleared["params"]["diagnostics"]
                .as_array()
                .unwrap()
                .is_empty()
        );

        // 4. A completion request gets a matching-id response.
        let id = server.completion("file:///x.scm", 0, 9);
        let response = futures::executor::block_on(rx.next()).unwrap();
        assert_eq!(response["id"], json!(id));
        assert!(response["result"].is_array());
    }

    #[test]
    fn editor_scheme_files_have_no_false_positives() {
        // The editor's own config uses `set-lsp`, `define-key`, etc. With the
        // real registered globals, the analyzer must not flag them.
        let (engine, _outcomes) = crate::script::ScriptEngine::new();
        let (mut server, mut rx) = SteelLsp::start(true, engine.global_names());
        let _init = futures::executor::block_on(rx.next()).unwrap();
        for src in [
            include_str!("../scheme/init.scm"),
            include_str!("../scheme/fundamental.scm"),
            include_str!("../scheme/steel-mode.scm"),
        ] {
            server.did_open("file:///cfg.scm", src);
            let msg = futures::executor::block_on(rx.next()).unwrap();
            assert!(
                msg["params"]["diagnostics"].as_array().unwrap().is_empty(),
                "config should be clean: {msg}"
            );
        }
    }

    #[test]
    fn user_config_calling_library_functions_is_clean() {
        // A realistic personal config: calls Scheme-library functions defined in
        // other bundled files (lsp-language, evil-mode, evil-define-keys-for-modes)
        // — the analyzer must know them, not flag "unknown function".
        let (engine, _outcomes) = crate::script::ScriptEngine::new();
        let (mut server, mut rx) = SteelLsp::start(true, engine.global_names());
        let _init = futures::executor::block_on(rx.next()).unwrap();
        let config = r#"
            (evil-mode)
            (lsp-language "rust"
              'extensions     '("rs")
              'command        "rust-analyzer"
              'format-on-save 'lsp
              'inlay-hints    #t)
            (evil-define-keys-for-modes "normal" '("rust")
              (list (cons "g g" lsp-find-definition)
                    (cons "g r" lsp-rename)))
            (define (leader keys cmd)
              (evil-bind-key "evil-normal" (string-append "space " keys) cmd))
            (leader "f f" find-file)
        "#;
        server.did_open("file:///init.scm", config);
        let msg = futures::executor::block_on(rx.next()).unwrap();
        let diags = msg["params"]["diagnostics"].as_array().unwrap();
        assert!(diags.is_empty(), "user config should be clean: {diags:?}");
    }

    #[test]
    #[ignore = "spawns the real rust-analyzer; run with --ignored"]
    fn rust_analyzer_responds_to_initialize() {
        let root = std::env::current_dir().unwrap();
        let (_client, mut rx) =
            LspClient::start("rust-analyzer", &root).expect("rust-analyzer should spawn");
        // Blocks until the server sends its first framed message — the
        // initialize response. Exercises spawn + framing + handshake end to end.
        let message = futures::executor::block_on(rx.next()).expect("a message from the server");
        assert_eq!(message["jsonrpc"], json!("2.0"));
    }
}
