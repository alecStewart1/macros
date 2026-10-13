//! The in-process Steel/Scheme language server ([`SteelLsp`]): the backend
//! behind `(set-lsp "scheme" "builtin")`, driven through the same
//! [`LanguageServer`] trait as an external server.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use serde_json::{Value, json};
use steel::rvals::SteelVal;
use steel::steel_vm::engine::Engine;

use super::{LanguageServer, path_to_uri};
use crate::jsonrpc;

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
        let _ = server.tx.unbounded_send(jsonrpc::response(
            0,
            json!({ "capabilities": {
                "textDocumentSync": 1,
                "renameProvider": { "prepareProvider": true }
            } }),
        ));
        (server, rx)
    }

    /// An open document's text (`""` for one we don't have).
    fn doc(&self, uri: &str) -> &str {
        self.docs.get(uri).map_or("", String::as_str)
    }

    /// Store a document's new full text (`didOpen` / full-sync `didChange`)
    /// and publish its diagnostics.
    fn set_document(&mut self, uri: &str, text: &str) {
        let diagnostics = self.compute_diagnostics(text);
        self.docs.insert(uri.to_string(), text.to_string());
        let _ = self.tx.unbounded_send(jsonrpc::notification(
            "textDocument/publishDiagnostics",
            json!({ "uri": uri, "diagnostics": diagnostics }),
        ));
    }

    /// Queue a response carrying `result` under a fresh request id, returning
    /// that id — the shared tail of every builtin request handler.
    fn respond(&mut self, result: Value) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        let _ = self.tx.unbounded_send(jsonrpc::response(id, result));
        id
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
    fn handle_request(&mut self, method: &str, params: &Value) -> Option<i64> {
        let uri = params["textDocument"]["uri"].as_str()?;
        let line = params["position"]["line"].as_u64()? as usize;
        let character = params["position"]["character"].as_u64()? as usize;
        match method {
            // declaration/type/impl all collapse to "the definition" here.
            "textDocument/definition"
            | "textDocument/declaration"
            | "textDocument/typeDefinition"
            | "textDocument/implementation" => Some(self.definition(uri, line, character)),
            "textDocument/references" => Some(self.references(uri, line, character)),
            "textDocument/hover" => Some(self.hover(uri, line, character)),
            "textDocument/prepareRename" => Some(self.prepare_rename(uri, line, character)),
            "textDocument/rename" => {
                let new_name = params["newName"].as_str().unwrap_or_default();
                Some(self.rename(uri, line, character, new_name))
            }
            _ => None,
        }
    }

    /// Go-to-definition: prefer a definition in the open document (so a config's
    /// own `define`/`define-command` and any overrides win), then the bundled doc
    /// index. A `'name` reference resolves like a bare one — the quote is a word
    /// boundary. Replies with a single `Location`, or `null` when unresolved.
    fn definition(&mut self, uri: &str, line: usize, character: usize) -> i64 {
        let text = self.doc(uri);
        let result = symbol_at(text, line, character)
            .and_then(|sym| self.locate_definition(text, uri, &sym))
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
        let text = self.doc(uri);
        let result = symbol_at(text, line, character)
            .and_then(|sym| self.doc_for(text, &sym))
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
        let text = self.doc(uri);
        let mut locs: Vec<Value> = Vec::new();
        if let Some(sym) = symbol_at(text, line, character) {
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
        let text = self.doc(uri);
        let result = symbol_at_span(text, line, character)
            .filter(|(sym, _, _)| defined_in_doc(text, sym))
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
        let text = self.doc(uri);
        let mut edits: Vec<Value> = Vec::new();
        if let Some(sym) = symbol_at(text, line, character)
            && defined_in_doc(text, &sym)
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

impl LanguageServer for SteelLsp {
    fn did_open(&mut self, uri: &str, _language_id: &str, _version: i32, text: &str) {
        self.set_document(uri, text);
    }

    fn did_change(&mut self, uri: &str, _version: i32, text: &str) {
        self.set_document(uri, text);
    }

    fn completion(&mut self, uri: &str, line: usize, character: usize) -> i64 {
        let items = self.complete(self.doc(uri), line, character);
        self.respond(json!(items))
    }

    fn request(&mut self, method: &str, params: Value) -> Option<i64> {
        self.handle_request(method, &params)
    }

    fn did_close(&mut self, uri: &str) {
        self.docs.remove(uri);
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

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

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
        server.set_document(uri, text);
        let _ = futures::executor::block_on(rx.next()).unwrap(); // diagnostics

        // Go-to-definition from the *quoted* reference on line 1 jumps to the
        // define on line 0 — the quote is a word boundary, so symbols resolve.
        let col = text.lines().nth(1).unwrap().find("my-cmd").unwrap();
        let params = json!({"textDocument":{"uri":uri},"position":{"line":1,"character":col}});
        let id = server
            .handle_request("textDocument/definition", &params)
            .unwrap();
        let resp = futures::executor::block_on(rx.next()).unwrap();
        assert_eq!(resp["id"], json!(id));
        assert_eq!(resp["result"]["uri"], json!(uri));
        assert_eq!(resp["result"]["range"]["start"]["line"], json!(0));

        // References include the define, the quoted use, and the call.
        let id = server
            .handle_request("textDocument/references", &params)
            .unwrap();
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
        server.set_document(uri, text);
        let _ = futures::executor::block_on(rx.next()).unwrap(); // diagnostics

        // Hover on the quoted reference shows the symbol's doc-comment.
        let col = text.lines().nth(2).unwrap().find("greet").unwrap();
        let params = json!({"textDocument":{"uri":uri},"position":{"line":2,"character":col}});
        let id = server
            .handle_request("textDocument/hover", &params)
            .unwrap();
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
        server.set_document(uri, text);
        let _ = futures::executor::block_on(rx.next()).unwrap(); // diagnostics

        let col = text.lines().nth(1).unwrap().find("greet").unwrap();
        let mut pos = json!({"textDocument":{"uri":uri},"position":{"line":1,"character":col}});

        // prepareRename accepts a symbol defined in this document.
        let id = server
            .handle_request("textDocument/prepareRename", &pos)
            .unwrap();
        let resp = futures::executor::block_on(rx.next()).unwrap();
        assert_eq!(resp["id"], json!(id));
        assert!(
            !resp["result"].is_null(),
            "own symbol is renameable: {resp}"
        );

        // rename rewrites every occurrence — define, quoted reference, and call.
        pos["newName"] = json!("welcome");
        let id = server.handle_request("textDocument/rename", &pos).unwrap();
        let resp = futures::executor::block_on(rx.next()).unwrap();
        assert_eq!(resp["id"], json!(id));
        let edits = resp["result"]["changes"][uri].as_array().unwrap();
        assert_eq!(edits.len(), 3, "define + quoted + call: {edits:?}");
        assert!(edits.iter().all(|e| e["newText"] == json!("welcome")));

        // A symbol NOT defined here (a bundled command) is declined — we'd only
        // half-rename it.
        let text2 = "(bind-key \"m\" \"c\" 'magit-status)";
        server.set_document(uri, text2);
        let _ = futures::executor::block_on(rx.next()).unwrap(); // diagnostics
        let col = text2.find("magit-status").unwrap();
        let pos2 = json!({"textDocument":{"uri":uri},"position":{"line":0,"character":col}});
        let id = server
            .handle_request("textDocument/prepareRename", &pos2)
            .unwrap();
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
        server.set_document("file:///s.scm", "(if 1 2 3 4)");
        let msg = futures::executor::block_on(rx.next()).unwrap();
        let diags = msg["params"]["diagnostics"].as_array().unwrap();
        assert_eq!(diags.len(), 1, "expected a semantic diagnostic: {msg}");
        assert!(diags[0]["message"].as_str().unwrap().contains("if"));

        // Valid code using a stdlib function compiles clean.
        server.set_document("file:///s.scm", "(define (f x) (map (lambda (y) y) x))");
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
        server.set_document("file:///u.scm", "(define (f x) (frobnicate x))");
        let msg = futures::executor::block_on(rx.next()).unwrap();
        let diags = msg["params"]["diagnostics"].as_array().unwrap();
        assert_eq!(diags.len(), 1, "{msg}");
        assert_eq!(diags[0]["severity"], json!(2));
        assert!(diags[0]["message"].as_str().unwrap().contains("frobnicate"));

        // Defining it clears the warning (forward reference within the file).
        server.set_document(
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
        plain.set_document("file:///plain.scm", editor_call);
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
        aware.set_document("file:///cfg.macros.scm", editor_call);
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

        server.set_document("file:///p.scm", "(define)");
        let msg = futures::executor::block_on(rx.next()).unwrap();
        assert!(msg["params"]["diagnostics"].is_array(), "{msg}");

        // Still alive: a subsequent real error is still reported.
        server.set_document("file:///p.scm", "(if 1 2 3 4)");
        let msg = futures::executor::block_on(rx.next()).unwrap();
        assert!(!msg["params"]["diagnostics"].as_array().unwrap().is_empty());
    }

    #[test]
    fn steel_lsp_drives_the_full_protocol() {
        let (mut server, mut rx) = SteelLsp::start(false, &[]);

        // 1. Handshake: an initialize response advertising full sync.
        let init = futures::executor::block_on(rx.next()).unwrap();
        assert_eq!(init["result"]["capabilities"]["textDocumentSync"], json!(1));

        // 2. didOpen on broken source publishes diagnostics for that URI.
        server.set_document("file:///x.scm", "(define (f");
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
        server.set_document("file:///x.scm", "(define (f x) x)");
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
            include_str!("../../scheme/init.scm"),
            include_str!("../../scheme/fundamental.scm"),
            include_str!("../../scheme/steel-mode.scm"),
        ] {
            server.set_document("file:///cfg.scm", src);
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
        server.set_document("file:///init.scm", config);
        let msg = futures::executor::block_on(rx.next()).unwrap();
        let diags = msg["params"]["diagnostics"].as_array().unwrap();
        assert!(diags.is_empty(), "user config should be clean: {diags:?}");
    }
}
