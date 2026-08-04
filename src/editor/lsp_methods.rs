//! LSP request/response methods for [`Editor`] (extracted from the editor
//! module for size). These extend the same `Editor` type; cross-module
//! entry points (called from `mod.rs`'s command dispatch) are `pub(super)`.

use std::path::{Path, PathBuf};

use futures::StreamExt;
use gpui::Context;
use serde_json::{Value, json};

use super::*;

/// The final path component of `path`, falling back to the whole string when
/// there is no `/` (matches the bare `rsplit('/').next().unwrap_or(path)` idiom).
fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Whether a `set-lsp` command string selects the in-process Steel/Scheme
/// server (`"builtin"` or `"builtin:<variant>"`) rather than an external
/// shell command.
fn is_builtin_lsp(command: &str) -> bool {
    command == "builtin" || command.starts_with("builtin:")
}

/// One pending `textDocument/didChange`: the server's language plus the
/// pre-edit `(line, character)` bounds of the range being replaced.
pub(super) type LspChange = (String, (usize, usize), (usize, usize));

/// Cap on the `*lsp-log*` scrollback ring (captured server stderr).
const LSP_LOG_CAP: usize = 2000;

/// Cap on the xref jump-back ring (`pop-tag-mark` locations).
const XREF_MARKER_RING_CAP: usize = 64;

impl Editor {
    /// Start a language server for the active buffer (if one is configured for
    /// its file type and isn't already running), and `didOpen` the buffer.
    pub(super) fn maybe_start_lsp(&mut self, cx: &mut Context<Self>) {
        let Some(language) = self.active_language() else {
            return;
        };

        // Already have a server for this language: just open the buffer in it.
        if self.servers.contains_key(&language) {
            self.lsp_open_active(&language);
            return;
        }

        let Some(command) = self.lsp_commands.get(&language).cloned() else {
            return; // no server configured for this language
        };

        // `"builtin"` selects the in-process Steel/Scheme server; anything else
        // is a shell command for an external server (the usual case).
        let started = if is_builtin_lsp(&command) {
            // Only the editor-aware `macros-scheme` server gets the editor API
            // seeded; the plain `scheme` server stays a vanilla interpreter.
            let seed = language == "macros-scheme";
            let globals: &[String] = if seed {
                self.script.global_names()
            } else {
                &[]
            };
            let (server, rx) = SteelLsp::start(seed, globals);
            Ok((LspBackend::Builtin(Box::new(server)), rx))
        } else {
            let root = self
                .active
                .path
                .as_ref()
                .and_then(|p| p.parent())
                .map(Path::to_path_buf)
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_default();
            LspClient::start(&command, &root).map(|(c, rx)| (LspBackend::External(Box::new(c)), rx))
        };
        let (client, mut rx) = match started {
            Ok(v) => v,
            Err(reason) => {
                // Record the cause in *lsp-log* (persistent) as well as the echo
                // area (transient) — a failed spawn is otherwise invisible.
                let line = format!("[{language}] could not start `{command}`: {reason}");
                push_capped(&mut self.lsp_log, line.clone(), LSP_LOG_CAP);
                self.append_to_log_buffer("*lsp-log*", &line);
                self.echo(format!("Could not start {command}: {reason}"), cx);
                return;
            }
        };

        // Forward this server's messages to the editor, tagged with its language.
        let lang = language.clone();
        let task = cx.spawn(async move |this, cx| {
            while let Some(message) = rx.next().await {
                if this
                    .update(cx, |editor, cx| {
                        editor.on_lsp_message(lang.clone(), message, cx)
                    })
                    .is_err()
                {
                    break; // editor dropped
                }
            }
        });

        self.servers.insert(
            language.clone(),
            LspServer {
                client,
                ready: false,
                sync_kind: lsp::SyncKind::Full,
                open: HashSet::new(),
                versions: HashMap::new(),
                pending_completion: None,
                pending: HashMap::new(),
                semantic_legend: Vec::new(),
                prepare_rename: false,
                completion_triggers: Vec::new(),
                semantic_state: HashMap::new(),
                _task: task,
            },
        );
        self.echo(format!("Started {command} ({language})"), cx);
        // For external servers, optionally pop `*lsp-log*` in a split so the
        // startup stderr is visible while editing (only on an actual spawn).
        let external = !is_builtin_lsp(&command);
        if external && cx.global::<crate::settings::Settings>().lsp_log_on_start() {
            self.open_lsp_log_split(cx);
        }
    }

    /// `didOpen` the active buffer into its language's server, if ready & unseen.
    pub(super) fn lsp_open_active(&mut self, language: &str) {
        let Some(uri) = self.active_uri() else {
            return;
        };
        let text = self.active.buffer.full_text();
        // The server is keyed by `language` (e.g. "typescript"), but the wire
        // `languageId` must reflect the actual file kind (e.g. "typescriptreact"
        // for .tsx) so the server enables JSX parsing.
        let language_id = self.active_language_id(language);
        if let Some(server) = self.servers.get_mut(language) {
            if !server.ready || server.open.contains(&uri) {
                return;
            }
            server.client.did_open(&uri, &language_id, 1, &text);
            server.open.insert(uri.clone());
            server.versions.insert(uri, 1);
        }
    }

    /// The `(language, start, end)` an about-to-happen replacement of
    /// `[start_byte, end_byte)` should be reported to `lsp_did_change` with, or
    /// `None` when the buffer has no language server language. MUST be called
    /// *before* the rope is mutated — the positions are the pre-edit ones.
    /// Shared by `edit()` and the undo/redo paths, which mutate the rope
    /// directly.
    pub(super) fn lsp_change_positions(
        &self,
        start_byte: usize,
        end_byte: usize,
    ) -> Option<LspChange> {
        let language = self.active_language()?;
        let start = self.active.buffer.byte_to_position(start_byte);
        let end = self.active.buffer.byte_to_position(end_byte);
        Some((language, start, end))
    }

    /// Notify the active buffer's server of an edit. `start`/`end` are the
    /// pre-edit LSP positions of the replaced range; `text` is the new content.
    pub(super) fn lsp_did_change(
        &mut self,
        language: &str,
        start: (usize, usize),
        end: (usize, usize),
        text: &str,
    ) {
        let Some(uri) = self.active_uri() else {
            return;
        };
        let Some((sync_kind, open)) = self
            .servers
            .get(language)
            .map(|s| (s.sync_kind, s.open.contains(&uri)))
        else {
            return;
        };
        if !open {
            return;
        }
        match sync_kind {
            // Full-document sync: coalesce. Mark the active doc dirty and wake the
            // debounce task instead of serializing + shipping the whole buffer on
            // every keystroke. `flush_lsp_fullsync` runs before any outbound
            // request and before a buffer swap, so the server never acts on a
            // stale document.
            lsp::SyncKind::Full => {
                self.lsp_fullsync_dirty = Some((language.to_string(), uri));
                let _ = self.lsp_flush_tx.unbounded_send(());
            }
            // Incremental sync: send just the changed range, per edit.
            lsp::SyncKind::Incremental => {
                if let Some(server) = self.servers.get_mut(language) {
                    let version = server.versions.entry(uri.clone()).or_insert(1);
                    *version += 1;
                    let version = *version;
                    server
                        .client
                        .did_change_incremental(&uri, version, start, end, text);
                }
            }
            lsp::SyncKind::None => {}
        }
    }

    /// Send the pending full-sync document to its server as one `did_change`.
    /// A cheap no-op when nothing is dirty. Needs no `cx`: it reads the active
    /// buffer and emits an LSP notification. Callers must ensure `self.active.buffer` is
    /// still the dirty document — true everywhere except mid-buffer-swap, which
    /// is why `snapshot_active` flushes before it takes the buffer.
    pub(super) fn flush_lsp_fullsync(&mut self) {
        let Some((language, uri)) = self.lsp_fullsync_dirty.take() else {
            return;
        };
        let full = self.active.buffer.full_text();
        if let Some(server) = self.servers.get_mut(&language) {
            if !server.open.contains(&uri) {
                return;
            }
            let version = server.versions.entry(uri.clone()).or_insert(1);
            *version += 1;
            let version = *version;
            server.client.did_change(&uri, version, &full);
        }
    }

    pub(super) fn on_lsp_message(
        &mut self,
        language: String,
        message: Value,
        cx: &mut Context<Self>,
    ) {
        // Notifications and server→client requests.
        if let Some(method) = message["method"].as_str() {
            if let Some(id) = message.get("id").cloned() {
                if let Some(server) = self.servers.get_mut(&language) {
                    server.client.respond_null(id);
                }
            } else if method == "textDocument/publishDiagnostics" {
                let (uri, diagnostics) = lsp::parse_diagnostics(&message["params"]);
                // Diagnostics are keyed only by URI, and each server runs its own
                // async task — so after a language switch (e.g. `scheme` →
                // `macros-scheme` via `macros-steel-mode`) the OLD server's
                // late-arriving publish could clobber the new one's. For the active
                // buffer, accept diagnostics only from its CURRENT language server;
                // a stale server's are dropped. (Background buffers — a different
                // URI — are unaffected.)
                let stale_for_active = self.active_uri().as_deref() == Some(&uri)
                    && self.active_language().as_deref() != Some(&language);
                if !stale_for_active {
                    self.diagnostics.insert(uri, diagnostics);
                    cx.notify();
                }
            } else if method == "$/stderr" {
                // Server stderr, captured into `*lsp-log*` (tagged by language)
                // and appended live to the buffer if it's open.
                if let Some(line) = message["params"]["line"].as_str() {
                    let line = format!("[{language}] {line}");
                    push_capped(&mut self.lsp_log, line.clone(), LSP_LOG_CAP);
                    self.append_to_log_buffer("*lsp-log*", &line);
                    cx.notify();
                }
            }
            return;
        }

        // Responses to our requests.
        if message.get("id").is_none() {
            return;
        }
        let is_init = self.servers.get(&language).is_some_and(|s| !s.ready);
        if is_init {
            let sync_kind = lsp::parse_sync_kind(&message["result"]);
            let legend = lsp::parse_semantic_legend(&message["result"]);
            let prepare_rename = lsp::parse_prepare_rename_support(&message["result"]);
            let completion_triggers = lsp::parse_completion_triggers(&message["result"]);
            if let Some(server) = self.servers.get_mut(&language) {
                server.ready = true;
                server.sync_kind = sync_kind;
                server.semantic_legend = legend;
                server.prepare_rename = prepare_rename;
                server.completion_triggers = completion_triggers;
                server.client.initialized();
            }
            // Open the buffer that triggered the server (if it's this language).
            if self.active_language().as_deref() == Some(&language) {
                self.lsp_open_active(&language);
            }
            cx.notify();
            return;
        }

        // A completion response?
        let completion =
            self.servers
                .get_mut(&language)
                .and_then(|server| match server.pending_completion {
                    Some((id, _)) if Some(id) == message["id"].as_i64() => {
                        server.pending_completion.take()
                    }
                    _ => None,
                });
        if let Some((_, range)) = completion {
            let items = lsp::parse_completion(&message["result"]);
            self.show_completion(items, range, cx);
            return;
        }

        // A navigation / rename / symbols / … response?
        let pending = message["id"].as_i64().and_then(|id| {
            self.servers
                .get_mut(&language)
                .and_then(|server| server.pending.remove(&id))
        });
        if let Some(kind) = pending {
            self.handle_lsp_response(kind, &message["result"], cx);
        }
    }

    /// Dispatch a finished LSP request's `result` to the right handler.
    pub(super) fn handle_lsp_response(
        &mut self,
        kind: PendingRequest,
        result: &Value,
        cx: &mut Context<Self>,
    ) {
        use PendingRequest::*;
        match kind {
            Script(callback) => {
                // Deliver the raw result JSON to the Steel callback; Scheme
                // parses it with `string->jsexpr` and renders however it likes.
                let json = result.to_string();
                self.script.dispatch_action(&callback, Some(&json));
            }
            Goto(what) => {
                let locations = lsp::parse_locations(result);
                self.goto_locations(what, locations, cx);
            }
            References => {
                let locations = lsp::parse_locations(result);
                self.goto_locations("references", locations, cx);
            }
            DocumentSymbols | WorkspaceSymbols => {
                let uri = self.active_uri().unwrap_or_default();
                let symbols = lsp::parse_symbols(result, &uri);
                self.show_symbols(symbols, cx);
            }
            Hover => match lsp::parse_hover(result) {
                Some(text) => self.show_hover(text, cx),
                None => self.echo("No documentation at point", cx),
            },
            SignatureHelp => match lsp::parse_signature_help(result) {
                Some(text) => self.echo(text, cx),
                None => self.echo("No signature help", cx),
            },
            Rename => {
                let edit = lsp::parse_workspace_edit(result);
                self.apply_workspace_edit(edit, cx);
            }
            Format => {
                let edits = lsp::parse_text_edits(result);
                self.apply_format_edits(edits, cx);
            }
            CodeAction => {
                let actions = lsp::parse_code_actions(result);
                self.show_code_actions(actions, cx);
            }
            SemanticTokens { callback, kind } => {
                let (Some(language), Some(uri)) = (self.active_language(), self.active_uri())
                else {
                    return;
                };
                let Some(server) = self.servers.get_mut(&language) else {
                    return;
                };
                // Resolve the packed token data per request kind, caching it (with
                // the resultId) so a later delta can splice onto it.
                let data = match kind {
                    SemanticKind::Range => lsp::semantic_data(result),
                    SemanticKind::Full => {
                        let data = lsp::semantic_data(result);
                        server
                            .semantic_state
                            .insert(uri.clone(), (lsp::semantic_result_id(result), data.clone()));
                        data
                    }
                    SemanticKind::Delta => {
                        let prev = server
                            .semantic_state
                            .get(&uri)
                            .map(|(_, d)| d.clone())
                            .unwrap_or_default();
                        let merged = lsp::apply_semantic_delta(&prev, result);
                        server.semantic_state.insert(
                            uri.clone(),
                            (lsp::semantic_result_id(result), merged.clone()),
                        );
                        merged
                    }
                };
                let tokens = lsp::decode_semantic_data(&data, &server.semantic_legend);
                let records: Vec<String> = tokens
                    .iter()
                    .map(|(line, col, len, ty)| format!("{line}\t{col}\t{len}\t{ty}"))
                    .collect();
                self.script
                    .dispatch_action(&callback, Some(&records.join("\n")));
            }
            PrepareRename => match lsp::parse_prepare_rename(result) {
                None => self.echo("Cannot rename this symbol", cx),
                Some(prep) => {
                    let placeholder = prep.placeholder.unwrap_or_else(|| match prep.range {
                        Some((s, e)) => {
                            let sb = self.active.buffer.position_to_byte(s.0, s.1);
                            let eb = self.active.buffer.position_to_byte(e.0, e.1);
                            self.active.buffer.slice_to_string(sb..eb)
                        }
                        None => self.identifier_at_point(),
                    });
                    self.open_minibuffer(
                        "Rename to: ",
                        MinibufferPurpose::SchemeCallback("lsp-rename-to".into()),
                        &placeholder,
                        cx,
                    );
                }
            },
            ResolveCompletion(label) => {
                if let Some(popup) = self.completion_popup.as_mut() {
                    for item in popup.items.iter_mut().filter(|i| i.label == label) {
                        lsp::merge_resolved_completion(item, result);
                    }
                    cx.notify();
                }
            }
            CodeLens => self.render_code_lenses(result, cx),
            ResolveCodeLens(line, character) => {
                if let Some(title) = lsp::parse_code_lens_title(result) {
                    self.set_code_lens(line, character, &title, cx);
                }
            }
            CallHierarchyPrepare(incoming) => self.lsp_call_hierarchy_calls(incoming, result, cx),
            CallHierarchyCalls(incoming) => {
                let key = if incoming { "from" } else { "to" };
                let calls = lsp::parse_call_hierarchy(result, key);
                if calls.is_empty() {
                    self.echo("No calls found", cx);
                    return;
                }
                let direction = if incoming { "Incoming" } else { "Outgoing" };
                let mut rows = Vec::new();
                let mut targets = Vec::new();
                for (name, loc) in &calls {
                    let short = lsp::uri_to_path(&loc.uri);
                    let short = basename(&short);
                    rows.push(format!("{name}  —  {short}:{}", loc.start.0 + 1));
                    targets.push((loc.uri.clone(), loc.start.0, loc.start.1));
                }
                self.xref_targets = targets;
                let text = format!("{direction} calls ({})\n\n{}", calls.len(), rows.join("\n"));
                self.render_buffer(
                    "*lsp-xref*".into(),
                    "lsp-xref-mode".into(),
                    text,
                    Vec::new(),
                    cx,
                );
            }
        }
    }

    /// Send a semantic-tokens request (`method`/`params` built by the caller) and
    /// route the result to `callback` as `"line\tcol\tlength\ttype"` records.
    pub(super) fn semantic_send(
        &mut self,
        method: &str,
        params: Value,
        kind: SemanticKind,
        callback: String,
        cx: &mut Context<Self>,
    ) {
        if !self.lsp_dispatch(
            method,
            params,
            PendingRequest::SemanticTokens { callback, kind },
        ) {
            self.echo(NO_LSP_FOR_BUFFER, cx);
        }
    }

    /// `semanticTokens/full` — whole document; caches data for later deltas.
    pub(super) fn lsp_semantic_tokens(&mut self, callback: String, cx: &mut Context<Self>) {
        let Some(uri) = self.active_uri() else { return };
        let params = json!({ "textDocument": { "uri": uri } });
        self.semantic_send(
            "textDocument/semanticTokens/full",
            params,
            SemanticKind::Full,
            callback,
            cx,
        );
    }

    /// `semanticTokens/range` — only a window around point (cheap on big files).
    pub(super) fn lsp_semantic_tokens_range(&mut self, callback: String, cx: &mut Context<Self>) {
        let Some(uri) = self.active_uri() else { return };
        let point_line = self.active.buffer.line_index_at_byte(self.point());
        let last = self.active.buffer.line_count().saturating_sub(1);
        let start = point_line.saturating_sub(200);
        let end = (point_line + 200).min(last);
        let params = json!({
            "textDocument": { "uri": uri },
            "range": {
                "start": { "line": start, "character": 0 },
                "end": { "line": end, "character": 0 }
            }
        });
        self.semantic_send(
            "textDocument/semanticTokens/range",
            params,
            SemanticKind::Range,
            callback,
            cx,
        );
    }

    /// `semanticTokens/full/delta` when we have a cached resultId, else a full
    /// request — the efficient live-refresh path.
    pub(super) fn lsp_semantic_tokens_delta(&mut self, callback: String, cx: &mut Context<Self>) {
        let Some(uri) = self.active_uri() else { return };
        let prev_id = self
            .active_language()
            .and_then(|l| self.servers.get(&l))
            .and_then(|s| s.semantic_state.get(&uri))
            .and_then(|(id, _)| id.clone());
        match prev_id {
            Some(id) => {
                let params = json!({
                    "textDocument": { "uri": uri },
                    "previousResultId": id
                });
                self.semantic_send(
                    "textDocument/semanticTokens/full/delta",
                    params,
                    SemanticKind::Delta,
                    callback,
                    cx,
                );
            }
            None => self.lsp_semantic_tokens(callback, cx),
        }
    }

    /// The identifier (word of `[A-Za-z0-9_]`) surrounding point, for rename
    /// prefill when the server gives no placeholder.
    pub(super) fn identifier_at_point(&self) -> String {
        let text = self.active.buffer.full_text();
        let point = self.point().min(text.len());
        let is_ident = |c: char| c.is_alphanumeric() || c == '_';
        let mut start = point;
        for (i, c) in text[..point].char_indices().rev() {
            if is_ident(c) {
                start = i;
            } else {
                break;
            }
        }
        let mut end = point;
        for (i, c) in text[point..].char_indices() {
            if is_ident(c) {
                end = point + i + c.len_utf8();
            } else {
                break;
            }
        }
        text[start..end].to_string()
    }

    /// `lsp-rename`: validate the position with `prepareRename` (when supported),
    /// then open the rename prompt prefilled with the symbol; otherwise prompt
    /// directly.
    pub(super) fn lsp_prepare_rename(&mut self, cx: &mut Context<Self>) {
        let supports = self
            .active_language()
            .and_then(|l| self.servers.get(&l))
            .is_some_and(|s| s.prepare_rename);
        if supports {
            self.lsp_send(
                "textDocument/prepareRename",
                PendingRequest::PrepareRename,
                Self::text_doc_position,
                cx,
            );
        } else if self.active_lsp_ready() {
            let placeholder = self.identifier_at_point();
            self.open_minibuffer(
                "Rename to: ",
                MinibufferPurpose::SchemeCallback("lsp-rename-to".into()),
                &placeholder,
                cx,
            );
        } else {
            self.echo(NO_LSP_FOR_BUFFER, cx);
        }
    }

    /// `lsp-code-lens`: request lenses; resolved titles render as virtual text.
    pub(super) fn lsp_code_lens(&mut self, cx: &mut Context<Self>) {
        self.lsp_send(
            "textDocument/codeLens",
            PendingRequest::CodeLens,
            |uri, _, _| json!({ "textDocument": { "uri": uri } }),
            cx,
        );
    }

    /// Render code lenses: those with a command show immediately; the rest are
    /// resolved via `codeLens/resolve` (the lens echoed back as a `Value`).
    pub(super) fn render_code_lenses(&mut self, result: &Value, cx: &mut Context<Self>) {
        for lens in lsp::parse_code_lenses(result) {
            match lens.title {
                Some(title) => self.set_code_lens(lens.line, lens.character, &title, cx),
                None => {
                    let kind = PendingRequest::ResolveCodeLens(lens.line, lens.character);
                    self.lsp_dispatch("codeLens/resolve", lens.data, kind);
                }
            }
        }
    }

    /// Show a code lens as end-of-line virtual text.
    pub(super) fn set_code_lens(
        &mut self,
        line: usize,
        _character: usize,
        title: &str,
        cx: &mut Context<Self>,
    ) {
        self.active.overlays.suffixes.insert(
            line,
            (format!("  » {title}").into(), "lsp-code-lens".into()),
        );
        cx.notify();
    }

    /// Resolve the currently-selected completion item (documentation +
    /// auto-import edits) if it hasn't been resolved yet.
    pub(super) fn completion_resolve_selected(&mut self) {
        let filtered = self.popup_filtered();
        let Some(popup) = self.completion_popup.as_ref() else {
            return;
        };
        let Some(item) = filtered.get(popup.selected.min(filtered.len().saturating_sub(1))) else {
            return;
        };
        if item.resolved {
            return;
        }
        let label = item.label.clone();
        let data = item.data.clone();
        self.lsp_dispatch(
            "completionItem/resolve",
            data,
            PendingRequest::ResolveCompletion(label),
        );
    }

    /// Accept a completion that carries `additionalTextEdits` (e.g. an
    /// auto-import): apply them together with the insertion in one pass so byte
    /// offsets stay consistent, then place the cursor after the inserted text.
    pub(super) fn accept_completion_with_edits(
        &mut self,
        anchor: usize,
        point: usize,
        choice: &CompletionItem,
        cx: &mut Context<Self>,
    ) {
        let full = self.active.buffer.full_text();
        let main = lsp::TextEdit {
            start: self.active.buffer.byte_to_position(anchor),
            end: self.active.buffer.byte_to_position(point),
            new_text: choice.insert_text.clone(),
        };
        let mut edits = choice.additional_edits.clone();
        edits.push(main);
        let new_text = lsp::apply_edits_to_text(&full, &edits);
        // Cursor lands at the end of the inserted completion, shifted by any
        // edits that fall entirely before the anchor (e.g. an import at the top).
        let mut shift: isize = 0;
        for e in &choice.additional_edits {
            let s = self.active.buffer.position_to_byte(e.start.0, e.start.1);
            let en = self.active.buffer.position_to_byte(e.end.0, e.end.1);
            if en <= anchor {
                shift += e.new_text.len() as isize - (en as isize - s as isize);
            }
        }
        let cursor =
            ((anchor as isize + shift) as usize + choice.insert_text.len()).min(new_text.len());
        self.replace_whole_buffer(&new_text, cursor, cx);
        cx.notify();
    }

    /// `prepareCallHierarchy` at point (incoming/outgoing call hierarchy).
    pub(super) fn lsp_call_hierarchy(&mut self, incoming: bool, cx: &mut Context<Self>) {
        self.lsp_send(
            "textDocument/prepareCallHierarchy",
            PendingRequest::CallHierarchyPrepare(incoming),
            Self::text_doc_position,
            cx,
        );
    }

    /// Second leg: send `callHierarchy/incoming|outgoingCalls` with the prepared
    /// item echoed back verbatim (a `serde_json::Value`, so integer fields stay
    /// integers — unlike a Scheme round-trip).
    pub(super) fn lsp_call_hierarchy_calls(
        &mut self,
        incoming: bool,
        prepared: &Value,
        cx: &mut Context<Self>,
    ) {
        let Some(item) = prepared.as_array().and_then(|a| a.first()).cloned() else {
            self.echo("No call hierarchy at point", cx);
            return;
        };
        let method = if incoming {
            "callHierarchy/incomingCalls"
        } else {
            "callHierarchy/outgoingCalls"
        };
        let kind = PendingRequest::CallHierarchyCalls(incoming);
        self.lsp_dispatch(method, json!({ "item": item }), kind);
    }

    // --- LSP navigation / rename / symbols / formatting ---------------------

    /// Send `params` for `method` to the active buffer's ready+open language
    /// server, recording `kind` so the response routes back. Returns whether it
    /// was sent (no echo — callers decide whether to report failure).
    pub(super) fn lsp_dispatch(
        &mut self,
        method: &str,
        params: Value,
        kind: PendingRequest,
    ) -> bool {
        // The server must have every edit before it answers this request.
        self.flush_lsp_fullsync();
        let (Some(language), Some(uri)) = (self.active_language(), self.active_uri()) else {
            return false;
        };
        self.servers
            .get_mut(&language)
            .filter(|s| s.ready && s.open.contains(&uri))
            .and_then(|server| {
                server
                    .client
                    .request(method, params)
                    .map(|id| server.pending.insert(id, kind))
            })
            .is_some()
    }

    /// Like [`lsp_dispatch`](Self::lsp_dispatch) but builds position params from
    /// point via `build` and echoes on failure. The common entry point.
    pub(super) fn lsp_send(
        &mut self,
        method: &str,
        kind: PendingRequest,
        build: impl FnOnce(&str, usize, usize) -> Value,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(uri) = self.active_uri() else {
            self.echo(NO_LSP_FOR_BUFFER, cx);
            return false;
        };
        let (line, character) = self.active.buffer.byte_to_position(self.point());
        let params = build(&uri, line, character);
        let sent = self.lsp_dispatch(method, params, kind);
        if !sent {
            self.echo(NO_LSP_FOR_BUFFER, cx);
        }
        sent
    }

    pub(super) fn text_doc_position(uri: &str, line: usize, character: usize) -> Value {
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        })
    }

    /// `M-.` / go-to-definition (and the declaration/type/impl variants).
    pub(super) fn lsp_goto(
        &mut self,
        method: &'static str,
        what: &'static str,
        cx: &mut Context<Self>,
    ) {
        self.lsp_send(
            method,
            PendingRequest::Goto(what),
            Self::text_doc_position,
            cx,
        );
    }

    /// `M-?` / find references (includes the declaration).
    pub(super) fn lsp_references(&mut self, cx: &mut Context<Self>) {
        self.lsp_send(
            "textDocument/references",
            PendingRequest::References,
            |uri, line, character| {
                json!({
                    "textDocument": { "uri": uri },
                    "position": { "line": line, "character": character },
                    "context": { "includeDeclaration": true }
                })
            },
            cx,
        );
    }

    pub(super) fn lsp_hover(&mut self, cx: &mut Context<Self>) {
        self.lsp_send(
            "textDocument/hover",
            PendingRequest::Hover,
            Self::text_doc_position,
            cx,
        );
    }

    pub(super) fn lsp_signature_help(&mut self, cx: &mut Context<Self>) {
        self.lsp_send(
            "textDocument/signatureHelp",
            PendingRequest::SignatureHelp,
            Self::text_doc_position,
            cx,
        );
    }

    pub(super) fn lsp_document_symbols(&mut self, cx: &mut Context<Self>) {
        self.lsp_send(
            "textDocument/documentSymbol",
            PendingRequest::DocumentSymbols,
            |uri, _, _| json!({ "textDocument": { "uri": uri } }),
            cx,
        );
    }

    /// `workspace/symbol` for a query read from the minibuffer.
    pub(super) fn lsp_workspace_symbol(&mut self, query: String, cx: &mut Context<Self>) {
        let params = json!({ "query": query });
        if !self.lsp_dispatch("workspace/symbol", params, PendingRequest::WorkspaceSymbols) {
            self.echo(NO_LSP_FOR_BUFFER, cx);
        }
    }

    /// Rename the symbol at point to `new_name` across the workspace.
    pub(super) fn lsp_rename(&mut self, new_name: String, cx: &mut Context<Self>) {
        self.lsp_send(
            "textDocument/rename",
            PendingRequest::Rename,
            move |uri, line, character| {
                json!({
                    "textDocument": { "uri": uri },
                    "position": { "line": line, "character": character },
                    "newName": new_name
                })
            },
            cx,
        );
    }

    /// Format the active buffer (`textDocument/formatting`).
    pub(super) fn lsp_format(&mut self, cx: &mut Context<Self>) {
        let tab_width = cx.global::<crate::settings::Settings>().tab_width();
        self.lsp_send(
            "textDocument/formatting",
            PendingRequest::Format,
            move |uri, _, _| {
                json!({
                    "textDocument": { "uri": uri },
                    "options": { "tabSize": tab_width, "insertSpaces": true }
                })
            },
            cx,
        );
    }

    /// Request code actions for the selection (or the point's line), passing any
    /// diagnostics in range as context.
    pub(super) fn lsp_code_action(&mut self, cx: &mut Context<Self>) {
        let (start, end) = if self.active.mark_active {
            (
                self.active.selected_range.start,
                self.active.selected_range.end,
            )
        } else {
            let line = self.active.buffer.line_range_at(self.point());
            (line.start, line.end)
        };
        let start_pos = self.active.buffer.byte_to_position(start);
        let end_pos = self.active.buffer.byte_to_position(end);
        let context_diags: Vec<Value> = self
            .active_diagnostics()
            .into_iter()
            .filter(|d| d.range.start < end && d.range.end > start || d.range.start == start)
            .map(|d| {
                let s = self.active.buffer.byte_to_position(d.range.start);
                let e = self.active.buffer.byte_to_position(d.range.end);
                json!({
                    "range": {
                        "start": { "line": s.0, "character": s.1 },
                        "end": { "line": e.0, "character": e.1 }
                    },
                    "severity": d.severity as u8,
                    "message": d.message
                })
            })
            .collect();
        self.lsp_send(
            "textDocument/codeAction",
            PendingRequest::CodeAction,
            move |uri, _, _| {
                json!({
                    "textDocument": { "uri": uri },
                    "range": {
                        "start": { "line": start_pos.0, "character": start_pos.1 },
                        "end": { "line": end_pos.0, "character": end_pos.1 }
                    },
                    "context": { "diagnostics": context_diags }
                })
            },
            cx,
        );
    }

    // --- result handlers ----------------------------------------------------

    /// Go to a single location, or offer multiple in a Helm picker (with a
    /// source preview pane) — RET jumps to the chosen one.
    pub(super) fn goto_locations(
        &mut self,
        what: &str,
        locations: Vec<lsp::Location>,
        cx: &mut Context<Self>,
    ) {
        match locations.len() {
            0 => self.echo(format!("No {what} found"), cx),
            1 => {
                let loc = &locations[0];
                self.goto_file_position(loc.uri.clone(), loc.start.0, loc.start.1, cx);
            }
            _ => self.helm_locations(what, locations, cx),
        }
    }

    /// Open a Helm picker over `locations` (parallel candidate rows
    /// `file:line:col: source`), jumping to the chosen one on RET. The preview
    /// pane shows the source around the selected location.
    fn helm_locations(
        &mut self,
        what: &str,
        locations: Vec<lsp::Location>,
        cx: &mut Context<Self>,
    ) {
        let mut file_cache: HashMap<String, Vec<String>> = HashMap::new();
        let mut candidates = Vec::new();
        for loc in &locations {
            let path = lsp::uri_to_path(&loc.uri);
            let lines = file_cache.entry(path.clone()).or_insert_with(|| {
                std::fs::read_to_string(&path)
                    .unwrap_or_default()
                    .lines()
                    .map(str::to_string)
                    .collect()
            });
            let short = basename(&path);
            let text = lines.get(loc.start.0).map_or("", |s| s.trim());
            candidates.push(format!(
                "{short}:{}:{}: {text}",
                loc.start.0 + 1,
                loc.start.1 + 1
            ));
        }
        let source = HelmSource {
            name: format!("{what} ({})", locations.len()),
            candidates,
            annotations: HashMap::new(),
            dynamic: None,
            raw: false,
            action: HelmAction::GotoLocation(locations),
            preview: None,
        };
        self.helm_open(format!("{what}: "), vec![source], String::new(), cx);
    }

    /// The source-context preview for an LSP location: a windowed slice of the
    /// file with the target line marked, shown in the Helm preview pane.
    pub(super) fn location_preview(&self, loc: &lsp::Location) -> String {
        let path = lsp::uri_to_path(&loc.uri);
        let content = std::fs::read_to_string(&path).unwrap_or_default();
        let lines: Vec<&str> = content.lines().collect();
        let target = loc.start.0;
        const CONTEXT: usize = 8;
        let start = target.saturating_sub(CONTEXT);
        let end = (target + CONTEXT + 1).min(lines.len());
        let short = basename(&path);
        let mut out = vec![format!("{short}:{}", target + 1), String::new()];
        for (i, line) in lines.iter().enumerate().take(end).skip(start) {
            let marker = if i == target { ">" } else { " " };
            out.push(format!("{marker} {:>4}  {line}", i + 1));
        }
        out.join("\n")
    }

    /// Open `uri` (or stay if it's the active buffer) and move point to the LSP
    /// position. Cross-file jumps read the file synchronously (source files are
    /// small) so point can land precisely once the buffer is installed.
    pub(super) fn goto_file_position(
        &mut self,
        uri: String,
        line: usize,
        character: usize,
        cx: &mut Context<Self>,
    ) {
        // Remember where we are so `xref-pop-marker` can return here.
        self.xref_push_marker();
        let path = PathBuf::from(lsp::uri_to_path(&uri));
        let same = self.active.path.as_deref() == Some(path.as_path());
        if !same && !self.visit_path(&path, || uri.clone(), cx) {
            return; // unreadable target — stay put (the error was echoed)
        }
        let offset = self.active.buffer.position_to_byte(line, character);
        self.move_to(offset, cx);
    }

    /// Make `path` the active buffer when it isn't already: prefer an already-open
    /// inactive buffer (just activate it), otherwise read the file from disk and
    /// open it. `fallback_name` supplies the buffer name when `path` has no file
    /// component. The caller is responsible for the "is it already active?" check
    /// and for positioning point afterward. Returns whether the buffer was
    /// switched — an unreadable file (permissions, non-UTF-8, …) is reported and
    /// leaves the editor unchanged, so a later save can't truncate it.
    fn visit_path(
        &mut self,
        path: &Path,
        fallback_name: impl FnOnce() -> String,
        cx: &mut Context<Self>,
    ) -> bool {
        if let Some(index) = self
            .inactive
            .iter()
            .position(|d| d.path.as_deref() == Some(path))
        {
            self.activate(index, true, cx);
            return true;
        }
        let contents = match std::fs::read_to_string(path) {
            Ok(contents) => contents,
            // Missing file → visit it as a fresh (empty) buffer, matching
            // `find_file`'s new-file flow.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(err) => {
                self.echo(format!("Could not open {}: {err}", path.display()), cx);
                return false;
            }
        };
        let name = path
            .file_name()
            .map_or_else(fallback_name, |f| f.to_string_lossy().into_owned());
        self.open_file(name, path.to_path_buf(), contents, cx);
        true
    }

    // --- generic Scheme bridge + jump-back stack ----------------------------

    /// Push the current location onto the jump-back ring (`pop-tag-mark`).
    pub(super) fn xref_push_marker(&mut self) {
        self.xref_marker_stack
            .push((self.active.path.clone(), self.point()));
        // Bound the ring so it can't grow without limit.
        if self.xref_marker_stack.len() > XREF_MARKER_RING_CAP {
            self.xref_marker_stack.remove(0);
        }
    }

    /// Pop the jump-back ring and return there (Emacs `pop-tag-mark`, vim C-o).
    pub(super) fn xref_pop_marker(&mut self, cx: &mut Context<Self>) {
        let Some((path, offset)) = self.xref_marker_stack.pop() else {
            self.echo("No further jump-back markers", cx);
            return;
        };
        if let Some(path) = path
            && self.active.path.as_deref() != Some(path.as_path())
            && !self.visit_path(&path, String::default, cx)
        {
            return; // unreadable target — stay put (the error was echoed)
        }
        let offset = offset.min(self.active.buffer.len());
        self.move_to(offset, cx);
    }

    /// The active buffer's LSP language id + URI, or `None` (with an echo) when
    /// there's no server-eligible buffer.
    pub(super) fn lsp_language_uri(&self) -> Option<(String, String)> {
        Some((self.active_language()?, self.active_uri()?))
    }

    /// Base request params for `kind`, with the active buffer's URI/position/
    /// range filled in. `extra` (a JSON object) is merged over the base; for
    /// `"raw"` the base is empty and `extra` is used verbatim.
    pub(super) fn lsp_build_params(&self, kind: &str, extra: Value, uri: &str) -> Value {
        let (line, character) = self.active.buffer.byte_to_position(self.point());
        let position = json!({ "line": line, "character": character });
        let document_range = {
            let (el, ec) = self
                .active
                .buffer
                .byte_to_position(self.active.buffer.len());
            json!({ "start": { "line": 0, "character": 0 }, "end": { "line": el, "character": ec } })
        };
        let region_range = {
            let (s, e) = if self.active.mark_active {
                (
                    self.active.selected_range.start,
                    self.active.selected_range.end,
                )
            } else {
                let l = self.active.buffer.line_range_at(self.point());
                (l.start, l.end)
            };
            let s = self.active.buffer.byte_to_position(s);
            let e = self.active.buffer.byte_to_position(e);
            json!({ "start": { "line": s.0, "character": s.1 }, "end": { "line": e.0, "character": e.1 } })
        };
        let mut params = match kind {
            "document" => json!({ "textDocument": { "uri": uri } }),
            "range" => json!({ "textDocument": { "uri": uri }, "range": document_range }),
            "region" => json!({
                "textDocument": { "uri": uri },
                "range": region_range,
                "options": { "tabSize": 4, "insertSpaces": true }
            }),
            "positions" => json!({ "textDocument": { "uri": uri }, "positions": [position] }),
            "raw" => json!({}),
            // "position" and anything unknown → textDocument + position.
            _ => json!({ "textDocument": { "uri": uri }, "position": position }),
        };
        if let (Some(base), Some(ext)) = (params.as_object_mut(), extra.as_object()) {
            for (k, v) in ext {
                base.insert(k.clone(), v.clone());
            }
        } else if kind == "raw" {
            params = extra;
        }
        params
    }

    /// The generic `(lsp-request method kind params callback)` bridge: send any
    /// request and deliver its result JSON to the Steel `callback`.
    pub(super) fn lsp_script_request(
        &mut self,
        method: String,
        kind: String,
        params_json: String,
        callback: String,
        cx: &mut Context<Self>,
    ) {
        let Some((_, uri)) = self.lsp_language_uri() else {
            self.echo(NO_LSP_FOR_BUFFER, cx);
            return;
        };
        let extra: Value = serde_json::from_str(&params_json).unwrap_or(Value::Null);
        let params = self.lsp_build_params(&kind, extra, &uri);
        if !self.lsp_dispatch(&method, params, PendingRequest::Script(callback)) {
            self.echo(NO_LSP_FOR_BUFFER, cx);
        }
    }

    /// `(lsp-apply-workspace-edit json)`: apply a `WorkspaceEdit` (from a Scheme
    /// handler) across files.
    pub(super) fn lsp_apply_workspace_edit_json(
        &mut self,
        json_text: String,
        cx: &mut Context<Self>,
    ) {
        let value: Value = serde_json::from_str(&json_text).unwrap_or(Value::Null);
        let edit = lsp::parse_workspace_edit(&value);
        self.apply_workspace_edit(edit, cx);
    }

    /// `(lsp-apply-text-edits json)`: apply `TextEdit[]` to the active buffer.
    pub(super) fn lsp_apply_text_edits_json(&mut self, json_text: String, cx: &mut Context<Self>) {
        let value: Value = serde_json::from_str(&json_text).unwrap_or(Value::Null);
        let edits = lsp::parse_text_edits(&value);
        self.apply_format_edits(edits, cx);
    }

    /// Render document/workspace symbols into `*lsp-symbols*` (RET visits).
    pub(super) fn show_symbols(&mut self, symbols: Vec<lsp::SymbolInfo>, cx: &mut Context<Self>) {
        if symbols.is_empty() {
            self.echo("No symbols", cx);
            return;
        }
        let mut rows = Vec::new();
        let mut targets = Vec::new();
        for sym in &symbols {
            let indent = "  ".repeat(sym.depth);
            let kind = lsp::symbol_kind_name(sym.kind);
            let container = if sym.container.is_empty() {
                String::new()
            } else {
                format!("  ({})", sym.container)
            };
            rows.push(format!("{indent}{kind}  {}{container}", sym.name));
            targets.push((sym.uri.clone(), sym.start.0, sym.start.1));
        }
        self.xref_targets = targets;
        let text = format!("Symbols ({})\n\n{}", symbols.len(), rows.join("\n"));
        self.render_buffer(
            "*lsp-symbols*".into(),
            "lsp-xref-mode".into(),
            text,
            Vec::new(),
            cx,
        );
    }

    /// RET in an `*lsp-xref*` / `*lsp-symbols*` buffer: visit row N's target.
    /// The first two rows are the header + blank line, so row 0 maps to target 0.
    pub(super) fn lsp_xref_visit(&mut self, cx: &mut Context<Self>) {
        let row = self.active.buffer.line_index_at_byte(self.point());
        let Some(index) = row.checked_sub(2) else {
            return; // header / blank line
        };
        if let Some((uri, line, character)) = self.xref_targets.get(index).cloned() {
            self.goto_file_position(uri, line, character, cx);
        }
    }

    /// Show hover documentation in the echo area (minibuffer), Emacs eldoc-style.
    /// Multi-line docs are collapsed to a single line: markdown code fences and
    /// blank lines are dropped and the rest joined, so the signature and summary
    /// stay readable on one line.
    pub(super) fn show_hover(&mut self, text: String, cx: &mut Context<Self>) {
        let line = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with("```"))
            .collect::<Vec<_>>()
            .join("  ");
        if line.is_empty() {
            self.echo("No documentation at point", cx);
        } else {
            self.echo(line, cx);
        }
    }

    /// Apply the formatting edits to the active buffer (whole-buffer replace,
    /// one undo step), keeping point clamped. When this format was triggered by
    /// format-on-save, the write is re-entered afterwards.
    pub(super) fn apply_format_edits(&mut self, edits: Vec<lsp::TextEdit>, cx: &mut Context<Self>) {
        let current = self.active.buffer.full_text();
        let formatted = lsp::apply_edits_to_text(&current, &edits);
        if edits.is_empty() || formatted == current {
            if !self.formatting_for_save {
                self.echo("No formatting changes", cx);
            }
        } else {
            let point = self.point();
            self.replace_whole_buffer(&formatted, point, cx);
            if !self.formatting_for_save {
                self.echo("Formatted buffer", cx);
            }
        }
        // Re-enter the deferred save (the flag stops it formatting again).
        if self.formatting_for_save {
            self.save_buffer(cx);
        }
    }

    /// Apply a workspace edit (rename / code action): the active buffer in place
    /// (undoable), other files on disk (and refreshed if open).
    pub(super) fn apply_workspace_edit(
        &mut self,
        edit: lsp::WorkspaceEdit,
        cx: &mut Context<Self>,
    ) {
        if edit.changes.is_empty() {
            self.echo("Nothing to change", cx);
            return;
        }
        let mut files = 0;
        let mut total = 0;
        // Files whose disk write (or read) failed — reported in the echo so a
        // partially-applied rename is never silently presented as a success.
        let mut failed: Vec<String> = Vec::new();
        for (uri, edits) in &edit.changes {
            let path = PathBuf::from(lsp::uri_to_path(uri));
            total += edits.len();
            if self.active.path.as_deref() == Some(path.as_path()) {
                // Active buffer: edit in place so it's undoable + reparsed.
                let current = self.active.buffer.full_text();
                let updated = lsp::apply_edits_to_text(&current, edits);
                if updated != current {
                    let point = self.point();
                    self.replace_whole_buffer(&updated, point, cx);
                }
            } else if let Some(doc) = self
                .inactive
                .iter_mut()
                .find(|d| d.path.as_deref() == Some(path.as_path()))
            {
                // Open but not active: rewrite its buffer and persist to disk.
                let current = doc.buffer.full_text();
                let updated = lsp::apply_edits_to_text(&current, edits);
                let len = doc.buffer.len();
                doc.buffer.replace(0..len, &updated);
                match std::fs::write(&path, &updated) {
                    // Only a successful write makes the buffer clean; on failure
                    // it keeps its (edited) unsaved state so nothing is lost.
                    Ok(()) => doc.modified = false,
                    Err(err) => {
                        doc.modified = true;
                        failed.push(format!("{}: {err}", path.display()));
                    }
                }
            } else {
                // Not open: apply on disk. An unreadable existing file must NOT
                // be treated as empty — the edits would then overwrite it.
                let current = match std::fs::read_to_string(&path) {
                    Ok(text) => text,
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
                    Err(err) => {
                        failed.push(format!("{}: {err}", path.display()));
                        continue;
                    }
                };
                let updated = lsp::apply_edits_to_text(&current, edits);
                if let Err(err) = std::fs::write(&path, &updated) {
                    failed.push(format!("{}: {err}", path.display()));
                    continue;
                }
            }
            files += 1;
        }
        let mut message = format!("Applied {total} edit(s) across {files} file(s)");
        if !failed.is_empty() {
            message.push_str(&format!(
                "; {} file(s) failed: {}",
                failed.len(),
                failed.join("; ")
            ));
        }
        self.echo(message, cx);
    }

    /// List code actions in `*lsp-actions*` (RET applies). Edit-only actions
    /// apply immediately; command actions run via `workspace/executeCommand`.
    pub(super) fn show_code_actions(
        &mut self,
        actions: Vec<lsp::CodeAction>,
        cx: &mut Context<Self>,
    ) {
        if actions.is_empty() {
            self.echo("No code actions available", cx);
            return;
        }
        let candidates: Vec<String> = actions
            .iter()
            .enumerate()
            .map(|(i, a)| format!("{}. {}", i + 1, a.title))
            .collect();
        // Kept in sync for the buffer-based RET path (`lsp-apply-code-action`),
        // which some keymaps may still reach.
        self.code_actions = actions.clone();
        let source = HelmSource {
            name: format!("Code actions ({})", actions.len()),
            candidates,
            annotations: HashMap::new(),
            dynamic: None,
            raw: false,
            action: HelmAction::ApplyCodeAction(actions),
            preview: None,
        };
        self.helm_open("Code action: ".into(), vec![source], String::new(), cx);
    }

    /// A preview of what a code action will do: its title plus a per-file summary
    /// of the edits (and any server command), shown in the Helm preview pane.
    pub(super) fn code_action_preview(action: &lsp::CodeAction) -> String {
        let mut out = vec![action.title.clone(), String::new()];
        match &action.edit {
            Some(edit) if !edit.changes.is_empty() => {
                for (uri, edits) in &edit.changes {
                    let path = lsp::uri_to_path(uri);
                    let short = basename(&path);
                    out.push(format!("{short} — {} edit(s)", edits.len()));
                    for e in edits {
                        let new = e.new_text.replace('\n', "\\n");
                        let new = if new.chars().count() > 60 {
                            format!("{}…", new.chars().take(60).collect::<String>())
                        } else {
                            new
                        };
                        out.push(format!("  {}: {new}", e.start.0 + 1));
                    }
                }
            }
            _ => {}
        }
        if let Some((command, _)) = &action.command {
            out.push(format!("Command: {command}"));
        }
        if out.len() == 2 {
            out.push("(no preview available)".to_string());
        }
        out.join("\n")
    }

    /// RET in `*lsp-actions*`: apply the action on the current row.
    pub(super) fn lsp_apply_code_action(&mut self, cx: &mut Context<Self>) {
        let row = self.active.buffer.line_index_at_byte(self.point());
        let Some(index) = row.checked_sub(2) else {
            return;
        };
        let Some(action) = self.code_actions.get(index).cloned() else {
            return;
        };
        // Return to the source buffer before applying, so edits land there (the
        // `*lsp-actions*` buffer is active when this runs).
        if let Some(idx) = self.inactive.iter().position(|d| d.special.is_none()) {
            self.activate(idx, true, cx);
        }
        self.apply_code_action(action, cx);
    }

    /// Apply a resolved code action to the current buffer: its workspace edit
    /// and/or the server `command`. The caller must have the source buffer
    /// active.
    pub(super) fn apply_code_action(&mut self, action: lsp::CodeAction, cx: &mut Context<Self>) {
        let had_edit = action.edit.is_some();
        if let Some(edit) = action.edit {
            self.apply_workspace_edit(edit, cx);
        }
        if let Some((command, arguments)) = action.command {
            self.lsp_execute_command(command, arguments, cx);
        } else if !had_edit {
            self.echo("Code action had no edit", cx);
        }
    }

    /// Send `workspace/executeCommand` (a code action's server command). No
    /// response is tracked; the server applies its effect via diagnostics/edits.
    pub(super) fn lsp_execute_command(
        &mut self,
        command: String,
        arguments: Value,
        cx: &mut Context<Self>,
    ) {
        let Some(language) = self.active_language() else {
            return;
        };
        let params = json!({ "command": command, "arguments": arguments });
        if let Some(server) = self.servers.get_mut(&language)
            && server.ready
        {
            server.client.request("workspace/executeCommand", params);
            self.echo("Ran code action", cx);
        }
    }
}
