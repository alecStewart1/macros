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

/// A diagnostic from the language server, already mapped to buffer byte offsets.
#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub range: Range<usize>,
    /// LSP severity: 1=error, 2=warning, 3=info, 4=hint.
    pub severity: u8,
    pub message: String,
}

/// A minimal LSP client: one server process, JSON-RPC over stdio. A background
/// thread reads framed messages and forwards them as JSON `Value`s over a
/// channel; the editor drives requests/notifications from the main thread.
pub struct LspClient {
    #[allow(dead_code)] // kept alive so the child isn't reaped; killed on Drop.
    child: Child,
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
        let mut child = match Command::new(program)
            .args(parts)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
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
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    let msg = json!({ "method": "$/stderr", "params": { "line": line } });
                    if tx_err.unbounded_send(msg).is_err() {
                        break;
                    }
                }
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
            child,
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
                "capabilities": {
                    "workspace": {
                        "applyEdit": false,
                        "symbol": {},
                        "workspaceEdit": { "documentChanges": true },
                        "executeCommand": {}
                    },
                    "textDocument": {
                        "synchronization": { "didSave": false },
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
                        "codeAction": {},
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
                }
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
    pub fn respond_null(&mut self, id: Value) {
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "result": Value::Null }));
    }
}

/// A diagnostic in LSP coordinates (line, UTF-16 column), before mapping to a
/// specific buffer's byte offsets.
#[derive(Clone, Debug)]
pub struct RawDiagnostic {
    pub start: (usize, usize),
    pub end: (usize, usize),
    pub severity: u8,
    pub message: String,
}

/// The text-sync kind the server wants, from its `initialize` result:
/// 1 = full document, 2 = incremental. `textDocumentSync` may be a bare number
/// or an object with a `change` field. Defaults to full (the safe choice).
pub fn parse_sync_kind(result: &Value) -> u8 {
    let sync = &result["capabilities"]["textDocumentSync"];
    if let Some(n) = sync.as_u64() {
        return n as u8;
    }
    sync["change"].as_u64().unwrap_or(1) as u8
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

/// The narrower `selectionRange` (the identifier) when present, else the full
/// `range` — for `DocumentSymbol`/`CallHierarchyItem`, which carry both.
fn selection_or_range(v: &Value) -> &Value {
    if v.get("selectionRange").is_some() {
        &v["selectionRange"]
    } else {
        &v["range"]
    }
}

fn position(value: &Value) -> (usize, usize) {
    serde_json::from_value::<WirePos>(value.clone()).map_or((0, 0), Into::into)
}

/// Parse a `Range` object into `(start, end)` `(line, col)` tuples. Missing or
/// malformed endpoints become `(0, 0)`.
fn range(value: &Value) -> ((usize, usize), (usize, usize)) {
    serde_json::from_value::<WireRange>(value.clone()).map_or(((0, 0), (0, 0)), Into::into)
}

/// Parse a `textDocument/publishDiagnostics` params object into `(uri, diags)`.
pub fn parse_diagnostics(params: &Value) -> (String, Vec<RawDiagnostic>) {
    let uri = params["uri"].as_str().unwrap_or_default().to_string();
    let diagnostics = params["diagnostics"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|d| {
                    let (start, end) = range(&d["range"]);
                    RawDiagnostic {
                        start,
                        end,
                        severity: d["severity"].as_u64().unwrap_or(1) as u8,
                        message: d["message"].as_str().unwrap_or_default().to_string(),
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

fn parse_completion_item(item: &Value) -> Option<CompletionItem> {
    let label = item["label"].as_str()?.to_string();
    let insert_text = item["insertText"]
        .as_str()
        .or_else(|| item["textEdit"]["newText"].as_str())
        .unwrap_or(label.as_str())
        .to_string();
    let kind = item["kind"]
        .as_u64()
        .and_then(completion_kind)
        .map(str::to_string);
    Some(CompletionItem {
        label,
        insert_text,
        detail: completion_detail(item),
        kind,
        documentation: parse_documentation(&item["documentation"]),
        additional_edits: parse_text_edits(&item["additionalTextEdits"]),
        data: item.clone(),
        resolved: false,
    })
}

/// Merge a `completionItem/resolve` result into `item` — servers fill in
/// `documentation` and `additionalTextEdits` (auto-imports) lazily.
pub fn merge_resolved_completion(item: &mut CompletionItem, resolved: &Value) {
    if let Some(doc) = parse_documentation(&resolved["documentation"]) {
        item.documentation = Some(doc);
    }
    let edits = parse_text_edits(&resolved["additionalTextEdits"]);
    if !edits.is_empty() {
        item.additional_edits = edits;
    }
    if let Some(detail) = completion_detail(resolved) {
        item.detail = Some(detail);
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

/// A code lens: where it sits, its title (if the command is already present),
/// and the raw lens (echoed to `codeLens/resolve` when the command is missing).
#[derive(Clone, Debug)]
pub struct CodeLens {
    pub line: usize,
    pub character: usize,
    pub title: Option<String>,
    pub data: Value,
}

/// Parse a `textDocument/codeLens` result.
pub fn parse_code_lenses(result: &Value) -> Vec<CodeLens> {
    result
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|lens| CodeLens {
                    line: lens["range"]["start"]["line"].as_u64().unwrap_or(0) as usize,
                    character: lens["range"]["start"]["character"].as_u64().unwrap_or(0) as usize,
                    title: lens["command"]["title"].as_str().map(str::to_string),
                    data: lens.clone(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The title of a resolved code lens (`codeLens/resolve` fills in `command`).
pub fn parse_code_lens_title(resolved: &Value) -> Option<String> {
    resolved["command"]["title"].as_str().map(str::to_string)
}

/// The signature/type annotation, preferring `labelDetails` (params + return or
/// module) and falling back to `detail`.
fn completion_detail(item: &Value) -> Option<String> {
    let label_details = &item["labelDetails"];
    let detail = label_details["detail"].as_str().unwrap_or("");
    let description = label_details["description"].as_str().unwrap_or("");
    let combined = format!("{detail} {description}");
    let combined = combined.trim();
    if !combined.is_empty() {
        return Some(combined.to_string());
    }
    item["detail"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
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

/// Parse a `documentSymbol` (nested `DocumentSymbol[]` or flat
/// `SymbolInformation[]`) or `workspace/symbol` result. `fallback_uri` is used
/// for `DocumentSymbol`s, which carry only ranges, not URIs.
pub fn parse_symbols(result: &Value, fallback_uri: &str) -> Vec<SymbolInfo> {
    fn push(
        item: &Value,
        fallback_uri: &str,
        container: &str,
        depth: usize,
        out: &mut Vec<SymbolInfo>,
    ) {
        let name = item["name"].as_str().unwrap_or("").to_string();
        let kind = item["kind"].as_u64().unwrap_or(0);
        if let Some(loc) = item.get("location") {
            // `SymbolInformation`: a flat item carrying its own location.
            out.push(SymbolInfo {
                name,
                kind,
                uri: loc["uri"].as_str().unwrap_or(fallback_uri).to_string(),
                start: position(&loc["range"]["start"]),
                container: item["containerName"]
                    .as_str()
                    .unwrap_or(container)
                    .to_string(),
                depth,
            });
        } else {
            // `DocumentSymbol`: hierarchical, ranges only.
            let range = selection_or_range(item);
            out.push(SymbolInfo {
                name: name.clone(),
                kind,
                uri: fallback_uri.to_string(),
                start: position(&range["start"]),
                container: container.to_string(),
                depth,
            });
            if let Some(children) = item["children"].as_array() {
                for child in children {
                    push(child, fallback_uri, &name, depth + 1, out);
                }
            }
        }
    }
    let mut out = Vec::new();
    if let Some(arr) = result.as_array() {
        for item in arr {
            push(item, fallback_uri, "", 0, &mut out);
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

fn parse_one_edit(v: &Value) -> Option<TextEdit> {
    let (start, end) = range(v.get("range")?);
    Some(TextEdit {
        start,
        end,
        new_text: v["newText"].as_str().unwrap_or("").to_string(),
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
}

/// Parse a `(Command | CodeAction)[]` result.
pub fn parse_code_actions(result: &Value) -> Vec<CodeAction> {
    let Some(arr) = result.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|item| {
            let title = item["title"].as_str()?.to_string();
            // A bare `Command` has a string `command`; a `CodeAction` may have an
            // `edit` and/or a nested `command` object.
            let (edit, command) = if let Some(cmd) = item["command"].as_str() {
                (None, Some((cmd.to_string(), item["arguments"].clone())))
            } else {
                let edit = item
                    .get("edit")
                    .filter(|e| !e.is_null())
                    .map(parse_workspace_edit);
                let command = item["command"]["command"]
                    .as_str()
                    .map(|c| (c.to_string(), item["command"]["arguments"].clone()));
                (edit, command)
            };
            Some(CodeAction {
                title,
                edit,
                command,
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
    for group in data.chunks_exact(5) {
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
    let Some(arr) = result.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|call| {
            let item = call.get(key)?;
            let range = selection_or_range(item);
            Some((
                item["name"].as_str().unwrap_or("?").to_string(),
                Location {
                    uri: item["uri"].as_str().unwrap_or("").to_string(),
                    start: position(&range["start"]),
                    end: position(&range["end"]),
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
    let mut spans: Vec<(usize, usize, &str)> = edits
        .iter()
        .map(|e| {
            let a = lsp_to_byte(text, e.start.0, e.start.1);
            let b = lsp_to_byte(text, e.end.0, e.end.1);
            (a.min(b), a.max(b), e.new_text.as_str())
        })
        .collect();
    // Descending by start (then end) so splicing one span never shifts another.
    spans.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    let mut out = text.to_string();
    for (start, end, new) in spans {
        let start = start.min(out.len());
        let end = end.min(out.len());
        if start <= end {
            out.replace_range(start..end, new);
        }
    }
    out
}

/// Convert a `file://` URI to a filesystem path string (inverse of
/// [`path_to_uri`] for local paths).
pub fn uri_to_path(uri: &str) -> String {
    uri.strip_prefix("file://").unwrap_or(uri).to_string()
}

impl Drop for LspClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
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

    pub fn respond_null(&mut self, id: Value) {
        match self {
            LspBackend::External(c) => c.respond_null(id),
            LspBackend::Builtin(_) => {} // never sends server→client requests
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

    fn completion(&mut self, uri: &str, line: usize, character: usize) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        let text = self.docs.get(uri).cloned().unwrap_or_default();
        let items = self.complete(&text, line, character);
        let _ = self
            .tx
            .unbounded_send(json!({ "jsonrpc": "2.0", "id": id, "result": items }));
        id
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
            // Steel's compiler can panic on some malformed-but-balanced input
            // (e.g. `(define)`), which is common while typing. We catch it (see
            // `try_compile`) and report nothing for that transient state rather
            // than crash the editor.
            CompileResult::Panicked => Vec::new(),
        };
        if diagnostics.is_empty() {
            let known = self.known_names();
            for (name, start, end) in crate::analyze::unbound_operators(text, &known) {
                diagnostics.push(warning(
                    byte_to_lsp(text, start),
                    byte_to_lsp(text, end),
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
        let id = self.next_id;
        self.next_id += 1;
        let text = self.docs.get(uri).cloned().unwrap_or_default();
        let result = symbol_at(&text, line, character)
            .and_then(|sym| self.locate_definition(&text, uri, &sym))
            .unwrap_or(Value::Null);
        let _ = self
            .tx
            .unbounded_send(json!({ "jsonrpc": "2.0", "id": id, "result": result }));
        id
    }

    fn locate_definition(&self, text: &str, uri: &str, sym: &str) -> Option<Value> {
        // 1) A definition in the open document (the user's own, or an override).
        for (i, line) in text.lines().enumerate() {
            if crate::runtime::define_name(line).as_deref() == Some(sym) {
                let col = word_occurrences(line, sym).first().map_or(0, |&(_, c)| c);
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
        let id = self.next_id;
        self.next_id += 1;
        let text = self.docs.get(uri).cloned().unwrap_or_default();
        let result = symbol_at(&text, line, character)
            .and_then(|sym| self.doc_for(&text, &sym))
            .map(|doc| json!({ "contents": { "kind": "plaintext", "value": doc } }))
            .unwrap_or(Value::Null);
        let _ = self
            .tx
            .unbounded_send(json!({ "jsonrpc": "2.0", "id": id, "result": result }));
        id
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
        let id = self.next_id;
        self.next_id += 1;
        let text = self.docs.get(uri).cloned().unwrap_or_default();
        let mut locs: Vec<Value> = Vec::new();
        if let Some(sym) = symbol_at(&text, line, character) {
            for (li, l) in text.lines().enumerate() {
                for (_, c) in word_occurrences(l, &sym) {
                    locs.push(location_json(uri, li, c, &sym));
                }
            }
        }
        let _ = self
            .tx
            .unbounded_send(json!({ "jsonrpc": "2.0", "id": id, "result": locs }));
        id
    }

    /// prepareRename: allow renaming only a symbol **defined in this document**
    /// (so a config renames its own commands, never a bundled or external one we
    /// can't fully update). Replies with the token's range, or `null` to decline.
    fn prepare_rename(&mut self, uri: &str, line: usize, character: usize) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
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
        let _ = self
            .tx
            .unbounded_send(json!({ "jsonrpc": "2.0", "id": id, "result": result }));
        id
    }

    /// rename: a `WorkspaceEdit` replacing every word-boundary occurrence (quoted
    /// references included) of the symbol within this document. Scoped to the open
    /// document and only for symbols it defines — `prepare_rename` already gated
    /// that, and this re-checks so a direct rename can't escape the guard.
    fn rename(&mut self, uri: &str, line: usize, character: usize, new_name: &str) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        let text = self.docs.get(uri).cloned().unwrap_or_default();
        let mut edits: Vec<Value> = Vec::new();
        if let Some(sym) = symbol_at(&text, line, character)
            && defined_in_doc(&text, &sym)
        {
            let len: usize = sym.chars().map(char::len_utf16).sum();
            for (li, l) in text.lines().enumerate() {
                for (_, c) in word_occurrences(l, &sym) {
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
        let _ = self.tx.unbounded_send(
            json!({ "jsonrpc": "2.0", "id": id, "result": { "changes": changes } }),
        );
        id
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
/// on its line — used to report a rename range.
fn symbol_at_span(text: &str, line: usize, character: usize) -> Option<(String, usize, usize)> {
    let chars: Vec<char> = text.lines().nth(line)?.chars().collect();
    // UTF-16 column → char index.
    let mut utf16 = 0usize;
    let mut idx = chars.len();
    for (i, c) in chars.iter().enumerate() {
        if utf16 >= character {
            idx = i;
            break;
        }
        utf16 += c.len_utf16();
    }
    let mut start = idx.min(chars.len());
    while start > 0 && is_ident_char(chars[start - 1]) {
        start -= 1;
    }
    let mut end = idx.min(chars.len());
    while end < chars.len() && is_ident_char(chars[end]) {
        end += 1;
    }
    if start >= end {
        return None;
    }
    let name: String = chars[start..end].iter().collect();
    let start16: usize = chars[..start].iter().map(|c| c.len_utf16()).sum();
    let span16: usize = chars[start..end].iter().map(|c| c.len_utf16()).sum();
    Some((name, start16, start16 + span16))
}

/// `(0, utf16-col)` of every word-boundary occurrence of `sym` in a single line.
/// (The line index is the caller's; this returns 0 in that slot.)
fn word_occurrences(line: &str, sym: &str) -> Vec<(usize, usize)> {
    let chars: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let (mut i, mut col16) = (0usize, 0usize);
    while i < chars.len() {
        if is_ident_char(chars[i]) {
            let start16 = col16;
            let mut j = i;
            let mut word = String::new();
            while j < chars.len() && is_ident_char(chars[j]) {
                word.push(chars[j]);
                col16 += chars[j].len_utf16();
                j += 1;
            }
            if word == sym {
                out.push((0, start16));
            }
            i = j;
        } else {
            col16 += chars[i].len_utf16();
            i += 1;
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

/// Map an LSP position `(line, utf16-col)` to a byte offset in `text`.
fn lsp_to_byte(text: &str, line: usize, character: usize) -> usize {
    let (mut current_line, mut col) = (0usize, 0usize);
    for (i, c) in text.char_indices() {
        if current_line == line && col >= character {
            return i;
        }
        if c == '\n' {
            if current_line == line {
                return i; // target column past end of line
            }
            current_line += 1;
            col = 0;
        } else {
            col += c.len_utf16();
        }
    }
    text.len()
}

/// A warning-severity (2) LSP diagnostic — used for heuristic findings.
fn warning(start: (usize, usize), end: (usize, usize), message: &str) -> Value {
    diagnostic_with_severity(start, end, 2, message)
}

/// Map a byte offset in `text` to an LSP position `(line, utf16-col)`.
fn byte_to_lsp(text: &str, byte: usize) -> (usize, usize) {
    let byte = byte.min(text.len());
    let (mut line, mut col) = (0usize, 0usize);
    for (i, c) in text.char_indices() {
        if i >= byte {
            break;
        }
        if c == '\n' {
            line += 1;
            col = 0;
        } else {
            col += c.len_utf16();
        }
    }
    (line, col)
}

/// Turn a compiler error into an LSP diagnostic, using its span when present.
fn steel_error_diagnostic(text: &str, err: &steel::rerrs::SteelErr) -> Value {
    let (start, end) = match err.span() {
        Some(span) => (
            byte_to_lsp(text, span.start as usize),
            byte_to_lsp(text, span.end as usize),
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
fn prefix_at(text: &str, line: usize, character: usize) -> String {
    let line_str = text.lines().nth(line).unwrap_or("");
    // Map the UTF-16 column to a char index within the line.
    let mut utf16 = 0usize;
    let mut idx = 0usize;
    for (ci, c) in line_str.chars().enumerate() {
        if utf16 >= character {
            idx = ci;
            break;
        }
        utf16 += c.len_utf16();
        idx = ci + 1;
    }
    let trailing: String = line_str
        .chars()
        .take(idx)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .take_while(|c| is_ident_char(*c))
        .collect();
    trailing.chars().rev().collect()
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

/// Read Content-Length-framed JSON-RPC messages until EOF, forwarding each.
fn read_loop(stdout: std::process::ChildStdout, tx: UnboundedSender<Value>) {
    read_frames(BufReader::new(stdout), tx);
}

/// The framing loop, generic over the reader so it's testable with a `Cursor`.
fn read_frames<R: BufRead>(mut reader: R, tx: UnboundedSender<Value>) {
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

/// Convert a filesystem path to a `file://` URI (good enough for local paths).
pub fn path_to_uri(path: &Path) -> String {
    format!("file://{}", path.to_string_lossy())
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
        read_frames(Cursor::new(stream.into_bytes()), tx); // runs to EOF, drops tx

        let first = futures::executor::block_on(rx.next()).unwrap();
        assert_eq!(first["id"], json!(1));
        let second = futures::executor::block_on(rx.next()).unwrap();
        assert_eq!(second["method"], json!("textDocument/publishDiagnostics"));
        assert!(futures::executor::block_on(rx.next()).is_none()); // closed, drained
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
        // `(define)` is balanced but panics Steel's compiler — the sandbox must
        // catch it (no diagnostic, no crash) and the server must keep working.
        let (mut server, mut rx) = SteelLsp::start(false, &[]);
        let _init = futures::executor::block_on(rx.next()).unwrap();

        server.did_open("file:///p.scm", "(define)");
        let msg = futures::executor::block_on(rx.next()).unwrap();
        assert!(msg["params"]["diagnostics"].as_array().unwrap().is_empty());

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
    fn uri_path_roundtrip() {
        assert_eq!(uri_to_path("file:///Users/x/a.rs"), "/Users/x/a.rs");
        assert_eq!(uri_to_path("/already/a/path"), "/already/a/path");
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
