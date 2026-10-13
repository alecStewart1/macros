use std::sync::{Arc, LazyLock, Mutex, OnceLock};

use base64::Engine as _;
use regex::Regex;
use ropey::Rope;
use sha2::Digest as _;
use steel::rvals::{IntoSteelVal, SteelVal};
use steel::steel_vm::engine::Engine;
use steel::steel_vm::register_fn::RegisterFn;

use super::*;

/// The nullary commands: each Steel name pushes its variant. Shared by engine
/// registration (on the thread), `(command-list)`, and the handle's fast-path
/// table; built once.
pub(crate) fn nullary_commands() -> &'static [(&'static str, EditorCommand)] {
    static COMMANDS: LazyLock<Vec<(&'static str, EditorCommand)>> = LazyLock::new(build_nullary);
    &COMMANDS
}

fn build_nullary() -> Vec<(&'static str, EditorCommand)> {
    use EditorCommand::*;
    let mut out = vec![
        ("forward-char", ForwardChar),
        ("backward-char", BackwardChar),
        ("next-line", NextLine),
        ("previous-line", PreviousLine),
        ("forward-word", ForwardWord),
        ("forward-word-start", ForwardWordStart),
        ("backward-word", BackwardWord),
        ("backward-word-start", BackwardWordStart),
        ("forward-word-end", ForwardWordEnd),
        ("forward-bigword-start", ForwardBigwordStart),
        ("backward-bigword-start", BackwardBigwordStart),
        ("forward-bigword-end", ForwardBigwordEnd),
        ("indent-region", IndentRegion),
        ("outdent-region", OutdentRegion),
        ("reindent-region", ReindentRegion),
        ("json-pretty-print-buffer", JsonPrettyPrintBuffer),
        ("json-pretty-print", JsonPrettyPrintRegion),
        ("move-beginning-of-line", BeginningOfLine),
        ("move-end-of-line", EndOfLine),
        ("beginning-of-buffer", BeginningOfBuffer),
        ("end-of-buffer", EndOfBuffer),
        ("set-mark", SetMark),
        ("exchange-point-and-mark", ExchangePointAndMark),
        ("mark-whole-buffer", MarkWholeBuffer),
        ("newline", Newline),
        ("open-line", OpenLine),
        ("indent-for-tab-command", IndentForTab),
        ("delete-char", DeleteChar),
        ("delete-backward-char", DeleteBackwardChar),
        ("transpose-chars", TransposeChars),
        ("kill-line", KillLine),
        ("kill-word", KillWord),
        ("backward-kill-word", BackwardKillWord),
        ("kill-region", KillRegion),
        ("kill-ring-save", KillRingSave),
        ("yank", Yank),
        ("yank-pop", YankPop),
        ("clipboard-copy", ClipboardCopy),
        ("clipboard-cut", ClipboardCut),
        ("clipboard-paste", ClipboardPaste),
        ("undo", Undo),
        ("redo", Redo),
        ("scroll-up", ScrollUp),
        ("scroll-down", ScrollDown),
        ("recenter", Recenter),
        ("keyboard-quit", KeyboardQuit),
        ("execute-extended-command", ExecuteExtendedCommand),
        ("list-messages", ListMessages),
        ("list-lsp-log", ListLspLog),
        ("check-for-updates", CheckForUpdates),
        ("eval-buffer", EvalBuffer),
        ("eval-last-sexp", EvalLastSexp),
        ("eval-expression", EvalExpression),
        ("find-file", FindFile),
        ("switch-buffer", SwitchBuffer),
        ("next-buffer", NextBuffer),
        ("previous-buffer", PreviousBuffer),
        ("save-buffer", SaveBuffer),
        ("save-some-buffers", SaveSomeBuffers),
        ("revert-buffer", RevertBuffer),
        ("line-numbers-mode", ToggleLineNumbers),
        ("scroll-bar-mode", ToggleScrollBar),
        ("word-wrap-mode", ToggleWordWrap),
        ("magit-gutter-mode", ToggleGitGutter),
        ("magit-gutter-refresh", GitGutterRefresh),
        ("electric-pair-mode", ToggleElectricPair),
        ("snippet-next-field", SnippetNextField),
        ("snippet-prev-field", SnippetPrevField),
        ("mc-add-cursor-below", McAddBelow),
        ("mc-add-cursor-above", McAddAbove),
        ("mc-mark-next-like-this", McMarkNext),
        ("mc-clear", McClear),
        ("kill-buffer", KillBuffer),
        ("mcp-server-info", McpServerInfo),
        ("kill-buffer-and-window", KillBufferAndWindow),
        ("quit", Quit),
        ("lsp-find-definition", LspDefinition),
        ("lsp-find-declaration", LspDeclaration),
        ("lsp-find-type-definition", LspTypeDefinition),
        ("lsp-find-implementation", LspImplementation),
        ("lsp-find-references", LspReferences),
        ("lsp-hover", LspHover),
        ("lsp-signature-help", LspSignatureHelp),
        ("lsp-document-symbols", LspDocumentSymbols),
        ("lsp-code-action", LspCodeAction),
        ("lsp-format-buffer", LspFormatBuffer),
        ("lsp-restart", LspRestart),
        ("lsp-call-hierarchy-incoming", LspCallHierarchyIncoming),
        ("lsp-call-hierarchy-outgoing", LspCallHierarchyOutgoing),
        ("lsp-rename", LspPrepareRename),
        ("lsp-code-lens", LspCodeLens),
        ("lsp-xref-visit", LspXrefVisit),
        ("lsp-apply-code-action", LspApplyCodeAction),
        ("xref-push-marker", XrefPushMarker),
        ("xref-pop-marker", XrefPopMarker),
        ("clear-overlay-faces", ClearOverlayFaces),
        ("clear-overlay-hides", ClearOverlayHides),
        ("clear-line-suffixes", ClearLineSuffixes),
        ("completion-at-point", CompletionAtPoint),
        ("completion-next", CompletionNext),
        ("completion-previous", CompletionPrevious),
        ("completion-accept", CompletionAccept),
        ("completion-abort", CompletionAbort),
        ("helm-M-x", HelmMx),
        ("helm-next", HelmNext),
        ("helm-previous", HelmPrevious),
        ("helm-accept", HelmAccept),
        ("helm-abort", HelmAbort),
        ("helm-actions", HelmActions),
        ("helm-mark", HelmToggleMark),
        ("enable-lsp", EnableLsp),
        ("macros-steel-mode", EnterMacrosSteel),
        ("helix-mode", HelixMode),
        ("helix-disable", HelixDisable),
        ("helix-normal-state", HelixNormalState),
        ("evil-mode", EvilMode),
        ("evil-normal-state", EvilNormalState),
        ("evil-insert-state", EvilInsertState),
        ("evil-visual-state", EvilVisualState),
        ("evil-visual-line-state", EvilVisualLineState),
        ("evil-visual-insert", EvilVisualInsert),
        ("evil-visual-append", EvilVisualAppend),
        ("evil-visual-block-state", EvilVisualBlockState),
        ("evil-block-insert", EvilBlockInsert),
        ("evil-block-append", EvilBlockAppend),
        ("evil-block-delete", EvilBlockDelete),
        ("evil-block-change", EvilBlockChange),
        ("evil-block-yank", EvilBlockYank),
        ("evil-disable", EvilDisable),
        ("evil-scroll-down", EvilScrollDown),
        ("evil-scroll-up", EvilScrollUp),
        ("evil-search-forward", EvilSearchForward),
        ("evil-search-backward", EvilSearchBackward),
        ("evil-search-next", EvilSearchNext),
        ("evil-search-previous", EvilSearchPrevious),
        ("evil-ex-command", EvilExCommand),
        ("evil-paste-after", EvilPasteAfter),
        ("evil-paste-before", EvilPasteBefore),
        ("evil-replace-char", EvilReplaceChar),
        ("evil-set-linewise", EvilSetLinewise),
        ("evil-set-charwise", EvilSetCharwise),
        ("count-repeat-step", CountRepeatStep),
        ("replay-step", ReplayStep),
        ("evil-repeat", EvilRepeat),
        ("evil-reselect-visual", EvilReselectVisual),
        ("evil-jump-backward", EvilJumpBackward),
        ("evil-jump-forward", EvilJumpForward),
        ("windmove-left", WindmoveLeft),
        ("windmove-down", WindmoveDown),
        ("windmove-up", WindmoveUp),
        ("windmove-right", WindmoveRight),
        ("previous-window-any-frame", PreviousWindow),
        ("evil-window-mru", LastWindow),
        ("evil-window-top-left", TopLeftWindow),
        ("evil-window-bottom-right", BottomRightWindow),
        ("balance-windows", BalanceWindows),
        ("evil-window-set-height", MaximizeWindowHeight),
        ("evil-window-set-width", MaximizeWindowWidth),
        ("evil-window-move-far-left", MoveWindowFarLeft),
        ("evil-window-move-very-bottom", MoveWindowFarDown),
        ("evil-window-move-very-top", MoveWindowFarUp),
        ("evil-window-move-far-right", MoveWindowFarRight),
        ("evil-window-exchange", ExchangeWindow),
        ("evil-window-rotate-downwards", RotateWindowsDown),
        ("evil-window-rotate-upwards", RotateWindowsUp),
        ("evil-window-new", NewWindowBelow),
        ("evil-window-vnew", NewWindowRight),
        ("helix-paste-after", HelixPasteAfter),
        ("helix-paste-before", HelixPasteBefore),
        ("isearch-forward", IsearchForward),
        ("isearch-backward", IsearchBackward),
        ("split-window-below", SplitWindowBelow),
        ("split-window-right", SplitWindowRight),
        ("other-window", OtherWindow),
        ("delete-window", DeleteWindow),
        ("delete-other-windows", DeleteOtherWindows),
        ("enlarge-window", EnlargeWindow),
        ("shrink-window", ShrinkWindow),
        ("enlarge-window-horizontally", EnlargeWindowHorizontally),
        ("shrink-window-horizontally", ShrinkWindowHorizontally),
        ("start-kbd-macro", StartKbdMacro),
        ("end-kbd-macro", EndKbdMacro),
        ("call-last-kbd-macro", CallLastKbdMacro),
    ];
    // Grid buffer commands (scheme/grid.scm binds them) — one table so the
    // command set lives with the grid code.
    out.extend(
        crate::grid::cmd::GRID_COMMANDS
            .iter()
            .map(|&(name, cmd)| (name, Grid(cmd))),
    );
    out
}

/// Build the Steel engine and register every command/bridge function. Runs on
/// the engine thread (the engine is `!Send`).
pub(crate) fn build_engine(shared: &EngineShared) -> (Engine, Vec<String>) {
    let EngineShared {
        queue,
        snapshot,
        markers,
        ..
    } = shared;
    let mut engine = Engine::new();
    let mut names: Vec<String> = Vec::new();
    let mut api: Vec<ApiEntry> = Vec::new();
    // Filled once registration finishes; `api-reference` reads it lazily.
    let api_cell: Arc<OnceLock<Vec<ApiEntry>>> = Arc::new(OnceLock::new());
    let mut reg = Registrar {
        engine: &mut engine,
        names: &mut names,
        api: &mut api,
        section: Section::Commands,
    };
    for (name, command) in nullary_commands() {
        register_nullary(&mut reg, queue, name, command.clone());
    }
    prims::editing::register(&mut reg, shared);
    prims::grid::register(&mut reg, shared);
    prims::prompts::register(&mut reg, shared);
    prims::keymaps::register(&mut reg, shared);
    prims::languages::register(&mut reg, shared);
    prims::faces::register(&mut reg, shared);
    prims::help::register(&mut reg, shared, &api_cell);
    prims::text::register(&mut reg, shared);
    prims::plotting::register(&mut reg, shared);
    prims::spell::register(&mut reg, shared);
    prims::modal::register(&mut reg, shared);
    prims::tree_sitter::register(&mut reg, shared);
    prims::lsp::register(&mut reg, shared);
    prims::options::register(&mut reg, shared);
    prims::buffers::register(&mut reg, shared);
    prims::decorations::register(&mut reg, shared);
    prims::files::register(&mut reg, shared);
    prims::hooks::register(&mut reg, shared);
    prims::special_buffers::register(&mut reg, shared);
    prims::ui::register(&mut reg, shared);
    prims::overlays::register(&mut reg, shared);
    prims::time::register(&mut reg, shared);
    prims::search::register(&mut reg, shared);
    prims::git::register(&mut reg, shared);
    prims::process::register(&mut reg, shared);
    // --- Async (subprocess streaming, timers) -----------------------------
    // Processes, terminals, DAP sessions, HTTP streams and async sockets draw
    // their ids from `shared.ids`. Each lives in its own editor-side map keyed
    // by id, so it needn't share the marker counter.
    let async_ids = &shared.ids;
    // --- DAP (Debug Adapter Protocol) --------------------------------------
    reg.section(Section::Dap);
    {
        // `(dap-start program args cwd on-message on-exit)` → a session id
        // (used by dap-send / dap-kill). The adapter speaks Content-Length-
        // framed DAP JSON on stdio; every incoming message — response, event,
        // or reverse request — reaches `on-message` as raw JSON text (parse
        // with `string->jsexpr`). The transport is protocol-free: dap.scm
        // stamps `seq` and owns the whole state machine. `cwd` "" = inherit;
        // on-exit "" = none (it receives the exit code as a string).
        let q = queue.clone();
        let ids = async_ids.clone();
        reg.register_fn(
            "dap-start",
            move |program: String,
                  StringList(args): StringList,
                  cwd: String,
                  on_message: Callback,
                  on_exit: Callback|
                  -> usize {
                let id = ids.next();
                q.push(EditorCommand::DapStart(Box::new(DapStartArgs {
                    id,
                    program,
                    args,
                    env: crate::process::current(),
                    cwd,
                    on_message: on_message.0,
                    on_exit: Some(on_exit.0).filter(|s| !s.is_empty()),
                })));
                id as usize
            },
        );
    }
    {
        // `(dap-connect host port on-message on-exit)` → a session id, for
        // adapters that serve DAP over a socket instead of stdio (delve's
        // `dlv dap`); Scheme spawns the server itself (make-process) and
        // connects once it announces its port.
        let q = queue.clone();
        let ids = async_ids.clone();
        reg.register_fn(
            "dap-connect",
            move |host: String,
                  port: usize,
                  on_message: Callback,
                  on_exit: Callback|
                  -> Result<usize, String> {
                // Reject rather than silently wrap (70000 as u16 = 4464).
                let port = u16::try_from(port)
                    .ok()
                    .filter(|&p| p != 0)
                    .ok_or_else(|| format!("dap-connect: invalid port {port}"))?;
                let id = ids.next();
                q.push(EditorCommand::DapConnect {
                    id,
                    host,
                    port,
                    on_message: on_message.0,
                    on_exit: Some(on_exit.0).filter(|s| !s.is_empty()),
                });
                Ok(id as usize)
            },
        );
    }
    register_binary(&mut reg, queue, "dap-send", |id, message| {
        EditorCommand::DapSend { id, message }
    });
    register_unary(&mut reg, queue, "dap-kill", |id| EditorCommand::DapKill {
        id,
    });
    reg.section(Section::Http);
    {
        // `(make-http-stream url headers body on-line on-done)` → a stream id
        // (used by http-stream-cancel). `headers` is a flat list of alternating
        // key/value strings: (list "x-api-key" key "anthropic-version" "..."). A
        // POST with the given JSON `body`; `on-line` is called per response line
        // (SSE `data:`/`event:` lines arrive verbatim), `on-done` with the id when
        // the stream closes. Callbacks may be "name" strings or 'name symbols.
        let q = queue.clone();
        let ids = async_ids.clone();
        reg.register_fn(
            "make-http-stream",
            move |url: String,
                  StringList(flat): StringList,
                  body: String,
                  on_line: Callback,
                  on_done: Callback|
                  -> usize {
                let id = ids.next();
                let headers = flat
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|kv| (kv[0].clone(), kv[1].clone()))
                    .collect();
                let Callback(on_line) = on_line;
                let Callback(on_done) = on_done;
                q.push(EditorCommand::HttpStream(Box::new(HttpStreamArgs {
                    id,
                    url,
                    headers,
                    body,
                    on_line,
                    on_done,
                })));
                id as usize
            },
        );
    }
    register_unary(&mut reg, queue, "http-stream-cancel", |id| {
        EditorCommand::HttpStreamCancel { id }
    });
    {
        // `(http-request method url headers body on-response)` → #t once queued.
        // A one-shot request of any verb ("GET"/"POST"/"PATCH"/"DELETE"); the
        // streaming `make-http-stream` is POST-only and hides the status code.
        // `headers` is a flat alternating key/value string list; `body` "" means
        // no body. `on-response` (a "name" string or 'name symbol) is called once
        // with "<status>\n<body>" — status on the first line (0 = transport
        // failure), body after the first newline. Parse it in Scheme with
        // `string->jsexpr` after splitting off the status line.
        //
        // `(http-request-full …)` takes the same arguments but delivers
        // "<status>\n<Header: value>\n…\n\n<hex-body>": the status line, every
        // response header, a blank line, then the body HEX-encoded — so you can
        // read `Link`/`ETag`/rate-limit headers and download binary content
        // (images, attachments) that the text payload would corrupt.
        for (name, full) in [("http-request", false), ("http-request-full", true)] {
            let q = queue.clone();
            reg.register_fn(
                name,
                move |method: String,
                      url: String,
                      StringList(flat): StringList,
                      body: String,
                      on_response: Callback|
                      -> bool {
                    let headers = flat
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|kv| (kv[0].clone(), kv[1].clone()))
                        .collect();
                    let Callback(on_response) = on_response;
                    q.push(EditorCommand::HttpRequest(Box::new(HttpRequestArgs {
                        method,
                        url,
                        headers,
                        body,
                        on_response,
                        full,
                    })));
                    true
                },
            );
        }
    }
    {
        // `(http-serve host port content-type body on-request)` → the bound port
        // (the handle for `http-server-stop`), or 0 if the bind failed. Pass port
        // 0 for an OS-assigned ephemeral port (an OAuth loopback wants
        // host="127.0.0.1", port=0). The socket is bound here so the port is known
        // synchronously; the accept loop runs on a worker thread. Each connection
        // gets one fixed response (status 200, the given `content-type` + `body`);
        // `on-request` is called with the request target ("/path?query") so Scheme
        // can pull the OAuth code/state out of the query string. Serves until
        // `http-server-stop` is called.
        let q = queue.clone();
        reg.register_fn(
            "http-serve",
            move |host: String,
                  Port(port): Port,
                  content_type: String,
                  body: String,
                  on_request: Callback|
                  -> usize {
                let listener = match std::net::TcpListener::bind((host.as_str(), port)) {
                    Ok(l) => l,
                    Err(_) => return 0,
                };
                let bound = listener.local_addr().map(|a| a.port()).unwrap_or(0);
                if bound == 0 {
                    return 0;
                }
                let Callback(on_request) = on_request;
                q.push(EditorCommand::HttpServe(Box::new(HttpServeArgs {
                    listener: Arc::new(Mutex::new(Some(listener))),
                    port: bound,
                    content_type,
                    body,
                    on_request,
                })));
                bound as usize
            },
        );
    }
    register_unary(&mut reg, queue, "http-server-stop", |Port(port): Port| {
        EditorCommand::HttpServerStop { port }
    });
    // --- Crypto / encoding (OAuth PKCE + general use) ---------------------
    reg.section(Section::Crypto);
    // Binary is exchanged with Scheme as hex strings (always valid UTF-8);
    // `hex->base64url` bridges hex into base64url so PKCE composes cleanly:
    //   (define verifier  (hex->base64url (random-bytes 32)))
    //   (define challenge (hex->base64url (sha256 verifier)))
    //   (define state     (hex->base64url (random-bytes 32)))
    reg.register_fn("random-bytes", |NonNeg(n): NonNeg| -> String {
        // Key/nonce material, not bulk data: cap so a typo can't OOM the editor.
        if n > 1 << 20 {
            return String::new();
        }
        let mut buf = vec![0u8; n];
        match getrandom::getrandom(&mut buf) {
            Ok(()) => hex::encode(&buf),
            Err(_) => String::new(),
        }
    });
    reg.register_fn("sha256", |input: String| -> String {
        let mut hasher = sha2::Sha256::new();
        hasher.update(input.as_bytes());
        hex::encode(hasher.finalize())
    });
    reg.register_fn("base64url-encode", |input: String| -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(input.as_bytes())
    });
    reg.register_fn("hex->base64url", |input: String| -> String {
        match hex::decode(input.trim()) {
            Ok(bytes) => base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes),
            Err(_) => String::new(),
        }
    });
    // `(base64url-hex b64)` → hex of the decoded bytes, or "" on garbage.
    // The inverse of `hex->base64url`, but deliberately liberal in what it
    // accepts: both the URL-safe and standard alphabets, with or without `=`
    // padding, ignoring ASCII whitespace — so one primitive covers Gmail
    // body data (URL-safe), MIME/RFC 2047 words (standard, padded, often
    // line-wrapped), and anything a Scheme package meets in the wild.
    reg.register_fn("base64url-hex", |input: String| -> String {
        let normalized: String = input
            .chars()
            .filter(|c| !c.is_ascii_whitespace() && *c != '=')
            .map(|c| match c {
                '-' => '+',
                '_' => '/',
                c => c,
            })
            .collect();
        match base64::engine::general_purpose::STANDARD_NO_PAD.decode(normalized.as_bytes()) {
            Ok(bytes) => hex::encode(bytes),
            Err(_) => String::new(),
        }
    });
    // `(decode-charset hex label)` → the bytes decoded to a UTF-8 string.
    // `label` is a WHATWG encoding label ("utf-8", "windows-1252",
    // "iso-8859-1", "shift_jis", …); an unknown label falls back to UTF-8.
    // Decoding is lossy (U+FFFD for malformed sequences) — text extraction,
    // not validation. Bytes cross the boundary as hex, the same interchange
    // as `random-bytes` / `hex->base64url`.
    reg.register_fn("decode-charset", |input: String, label: String| -> String {
        let Ok(bytes) = hex::decode(input.trim()) else {
            return String::new();
        };
        let encoding =
            encoding_rs::Encoding::for_label(label.trim().as_bytes()).unwrap_or(encoding_rs::UTF_8);
        let (text, _, _) = encoding.decode(&bytes);
        text.into_owned()
    });
    reg.section(Section::Http);
    // `(url-decode s)` → `s` with %XX escapes decoded (RFC 3986; `+` stays a
    // plus — this is for URL components, not form bodies). Bytes are
    // reassembled as UTF-8 (lossy), so a percent-encoded multibyte character
    // round-trips. A malformed escape is kept literally.
    reg.register_fn("url-decode", |s: String| -> String {
        let bytes = s.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            let hex = |b: u8| (b as char).to_digit(16);
            if bytes[i] == b'%'
                && i + 2 < bytes.len()
                && let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
            {
                out.push((h * 16 + l) as u8);
                i += 3;
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    });
    reg.register_fn("open-url", |url: String| -> bool {
        crate::update::open_url(&url).is_ok()
    });
    // --- client sockets (blocking, engine thread) ------------------------
    reg.section(Section::Sockets);
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
    // --- SQLite (generic embedded database; bundled, with FTS5) -----------
    reg.section(Section::Sqlite);
    // A real database for Scheme packages — a full-text search index, a
    // relational store, whatever. Blocking, engine-thread (like the socket
    // family). Bundled SQLite ships FTS5, so `CREATE VIRTUAL TABLE … USING
    // fts5(…)` + `… MATCH ?` give BM25-ranked full-text search with no extra
    // dependency. Params are bound (never string-spliced), so untrusted text
    // is injection-safe.
    //
    //   (sqlite-open path)          → db id, 0 on failure (":memory:" ok)
    //   (sqlite-exec db sql params) → #t; runs one or more statements. With an
    //     EMPTY params list, `sql` may contain several `;`-separated statements
    //     (schema setup); with params, it is ONE statement and `params` (a flat
    //     string list) binds ?1, ?2, … in order.
    //   (sqlite-query db sql params)→ rows of a SELECT: a list of strings, one
    //     per row, columns joined by US (0x1f); NULLs become "". Bind like exec.
    //   (sqlite-close db)           → #t if the id existed
    //   (sqlite-last-error)         → the most recent failure text
    reg.register_fn("sqlite-open", |path: String| -> usize {
        let conn =
            rusqlite::Connection::open(&path).map_err(|e| format!("sqlite-open {path}: {e}"));
        or_report(conn.map(|c| SQLITE.insert(c)), 0)
    });
    reg.register_fn(
        "sqlite-exec",
        |id: usize, sql: String, StringList(ps): StringList| -> bool {
            let conns = SQLITE.lock();
            let Some(conn) = conns.get(&id) else {
                set_last_error(format!("sqlite-exec: no db {id}"));
                return false;
            };
            let result = if ps.is_empty() {
                // No binds → allow a multi-statement batch (schema setup).
                conn.execute_batch(&sql)
            } else {
                conn.execute(&sql, rusqlite::params_from_iter(ps.iter()))
                    .map(|_| ())
            };
            or_report(
                result
                    .map(|()| true)
                    .map_err(|e| format!("sqlite-exec: {e}")),
                false,
            )
        },
    );
    reg.register_fn(
        "sqlite-query",
        |id: usize, sql: String, StringList(ps): StringList| -> Vec<String> {
            let conns = SQLITE.lock();
            let Some(conn) = conns.get(&id) else {
                set_last_error(format!("sqlite-query: no db {id}"));
                return Vec::new();
            };
            let mut stmt = match conn.prepare(&sql) {
                Ok(s) => s,
                Err(e) => {
                    set_last_error(format!("sqlite-query: {e}"));
                    return Vec::new();
                }
            };
            let ncol = stmt.column_count();
            let rows = stmt.query_map(rusqlite::params_from_iter(ps.iter()), |row| {
                let mut cells: Vec<String> = Vec::with_capacity(ncol);
                for i in 0..ncol {
                    // Coerce any column type to text; NULL → "".
                    let v: rusqlite::types::Value = row.get(i)?;
                    cells.push(match v {
                        rusqlite::types::Value::Null => String::new(),
                        rusqlite::types::Value::Integer(n) => n.to_string(),
                        rusqlite::types::Value::Real(f) => f.to_string(),
                        rusqlite::types::Value::Text(s) => s,
                        rusqlite::types::Value::Blob(b) => String::from_utf8_lossy(&b).into_owned(),
                    });
                }
                Ok(cells.join("\u{1f}"))
            });
            let rows = rows
                .map(|iter| iter.filter_map(|r| r.ok()).collect())
                .map_err(|e| format!("sqlite-query: {e}"));
            or_report(rows, Vec::new())
        },
    );
    reg.register_fn("sqlite-close", |id: usize| -> bool { SQLITE.remove(id) });
    reg.register_fn("sqlite-last-error", last_error);
    reg.section(Section::Sockets);
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
    reg.section(Section::Sockets);
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
    // --- ANVIL REPL (a second, independent Steel interpreter) -------------
    reg.section(Section::Anvil);
    {
        // `(anvil-eval source on-result)` evaluates `source` in the separate
        // REPL image; the printed result is delivered to the Scheme function
        // named `on-result` (in this engine) as a single string argument.
        register_binary(&mut reg, queue, "anvil-eval", |source, on_result| {
            EditorCommand::AnvilEval { source, on_result }
        });
    }
    {
        // `(anvil-interrupt)` cancels the REPL's current evaluation;
        // `(anvil-reset)` rebuilds the REPL image from scratch.
        register_nullary(
            &mut reg,
            queue,
            "anvil-interrupt",
            EditorCommand::AnvilInterrupt,
        );
        register_nullary(&mut reg, queue, "anvil-reset", EditorCommand::AnvilReset);
        // SLIME-style interaction commands. The editor computes the buffer
        // context (the form at point, the symbol at point, the file path) since
        // that needs the live buffer; the result streams back to the REPL.
        register_nullary(
            &mut reg,
            queue,
            "anvil-eval-defun",
            EditorCommand::AnvilEvalDefun,
        );
        register_nullary(
            &mut reg,
            queue,
            "anvil-load-file",
            EditorCommand::AnvilLoadFile,
        );
        register_nullary(&mut reg, queue, "anvil-doc", EditorCommand::AnvilDoc);
        register_nullary(
            &mut reg,
            queue,
            "anvil-macroexpand",
            EditorCommand::AnvilMacroexpand,
        );
        register_unary(
            &mut reg,
            queue,
            "anvil-apropos",
            EditorCommand::AnvilApropos,
        );
        register_nullary(
            &mut reg,
            queue,
            "anvil-signature",
            EditorCommand::AnvilSignature,
        );
    }
    reg.section(Section::BufferReads);
    // Read bridges over the editor's published snapshot (see `set_snapshot`).
    register_snapshot_read(&mut reg, snapshot, "buffer-line", |s| s.line);
    register_snapshot_read(&mut reg, snapshot, "current-buffer", |s| s.name.clone());
    // The active buffer's directory — its visited file's parent, or the cwd
    // for fileless buffers (Emacs's buffer-local `default-directory`).
    register_snapshot_read(&mut reg, snapshot, "buffer-directory", |s| {
        s.directory.clone()
    });
    // The visited file's full path, or "" for a buffer with no file (Emacs's
    // `buffer-file-name`).
    register_snapshot_read(&mut reg, snapshot, "buffer-file-name", |s| s.path.clone());
    // Point as a byte offset (Emacs `(point)`).
    register_snapshot_read(&mut reg, snapshot, "point", |s| s.point);
    {
        // The whole buffer (Emacs `(buffer-string)`). Clone the cheap rope handle
        // under the lock, then materialize the String outside it so the snapshot
        // lock isn't held for a full-buffer copy.
        let shared = snapshot.clone();
        reg.register_fn("buffer-string", move || -> String {
            let rope = shared.load().text.clone();
            rope.to_string()
        });
    }
    // End of buffer as a byte offset (Emacs `(point-max)`). O(1) on the rope, no
    // text materialized.
    register_snapshot_read(&mut reg, snapshot, "point-max", |s| s.text.len_bytes());
    {
        // `(position->byte line character)` → byte offset of an LSP position
        // (0-based line, UTF-16 column) in the active buffer. Lets Scheme LSP
        // handlers turn server line/col coordinates into the byte offsets the
        // overlay/fold/goto primitives expect.
        let shared = snapshot.clone();
        reg.register_fn(
            "position->byte",
            move |line: usize, character: usize| -> usize {
                let rope = shared.load().text.clone();
                // A line past the end clamps to the end of the buffer.
                if line >= rope.len_lines() {
                    return rope.len_bytes();
                }
                // Jump straight to the line (O(log n)), then walk only its
                // chars to resolve the UTF-16 column: stop at the column, at
                // the line's newline, or (last line) at end of buffer.
                let mut byte = rope.line_to_byte(line);
                let mut col = 0usize;
                for c in rope.line(line).chars() {
                    if col >= character || c == '\n' {
                        return byte;
                    }
                    col += c.len_utf16();
                    byte += c.len_utf8();
                }
                byte
            },
        );
    }
    // `(lsp-server-ready?)` → #t if the active buffer has a ready LSP server.
    register_snapshot_read(&mut reg, snapshot, "lsp-server-ready?", |s| s.lsp_ready);
    reg.section(Section::BufferReads);
    {
        // `(line-bounds)` → `(start end)` byte offsets of the line at point
        // (end points at the line's terminating newline, or buffer end).
        let shared = snapshot.clone();
        reg.register_fn("line-bounds", move || -> Vec<usize> {
            let (rope, p) = {
                let snap = shared.load();
                let p = snap.point.min(snap.text.len_bytes());
                (snap.text.clone(), p)
            };
            let line = rope.byte_to_line(p);
            let start = rope.line_to_byte(line);
            let line_str = rope.line(line).to_string();
            let end = start + line_str.find('\n').unwrap_or(line_str.len());
            vec![start, end]
        });
    }
    // `(line-count)` → total buffer lines. O(1) on the rope. Lets Scheme fold
    // logic walk lines by index without materializing the whole buffer.
    register_snapshot_read(&mut reg, snapshot, "line-count", |s| s.text.len_lines());
    {
        // `(line-indentation line)` → leading-whitespace width (spaces, tabs count
        // as one column each) of buffer `line`, or -1 for a blank/whitespace-only
        // line so fold logic can skip blanks. The indentation-based folder builds
        // fold ranges from this.
        let shared = snapshot.clone();
        reg.register_fn("line-indentation", move |line: usize| -> isize {
            let rope = shared.load().text.clone();
            if line >= rope.len_lines() {
                return -1;
            }
            let mut indent = 0;
            for c in rope.line(line).chars() {
                match c {
                    ' ' | '\t' => indent += 1,
                    // End of line (or a blank/whitespace-only line) before any
                    // content: report -1 so fold logic skips it.
                    '\n' | '\r' => return -1,
                    _ => return indent,
                }
            }
            -1
        });
    }
    reg.section(Section::BufferReads);
    {
        // Bytes `[a, b)` of the buffer, clamped (Emacs `(buffer-substring a b)`).
        let shared = snapshot.clone();
        reg.register_fn("buffer-substring", move |a: isize, b: isize| -> String {
            let (a, b) = (scheme_pos(a), scheme_pos(b));
            let rope = shared.load().text.clone();
            // Snap to char boundaries so we never slice mid-codepoint.
            let (start, end) = (snap_byte(&rope, a), snap_byte(&rope, b));
            if start >= end {
                return String::new();
            }
            rope.byte_slice(start..end).to_string()
        });
    }
    {
        // The active region's byte range, or (-1, -1) when no region is active.
        register_snapshot_read(&mut reg, snapshot, "region-beginning", |s| {
            s.region.map_or(-1, |(a, _)| a as isize)
        });
        register_snapshot_read(&mut reg, snapshot, "region-end", |s| {
            s.region.map_or(-1, |(_, b)| b as isize)
        });
        register_snapshot_read(&mut reg, snapshot, "region-active?", |s| s.region.is_some());
        // `(selections)` → a flat list `(a0 h0 a1 h1 …)` of every selection's
        // anchor/head byte offsets, primary first (Helix multiple selections).
        // Flat (not pairs) so it round-trips straight into `(set-selections …)`.
        register_snapshot_read(&mut reg, snapshot, "selections", |s| {
            s.selections
                .iter()
                .flat_map(|&(a, h)| [a, h])
                .collect::<Vec<usize>>()
        });
        // The selected text, or "" when no region is active (Emacs has no single
        // primitive for this; it's `(buffer-substring (region-beginning) …)`).
        let shared = snapshot.clone();
        reg.register_fn("region-text", move || -> String {
            let (rope, region) = {
                let snap = shared.load();
                (snap.text.clone(), snap.region)
            };
            match region {
                Some((a, b)) if b <= rope.len_bytes() && a < b => rope
                    .byte_slice(snap_byte(&rope, a)..snap_byte(&rope, b))
                    .to_string(),
                _ => String::new(),
            }
        });
    }
    {
        // Byte offset of the next occurrence of `needle` at/after point, or -1.
        let shared = snapshot.clone();
        reg.register_fn("search-forward", move |needle: String| -> isize {
            let (rope, p) = {
                let snap = shared.load();
                (snap.text.clone(), snap.point)
            };
            let from = snap_byte(&rope, p);
            let tail = rope.byte_slice(from..).to_string();
            match tail.find(&needle) {
                Some(off) => (from + off) as isize,
                None => -1,
            }
        });
    }
    // --- Markers (positions that move with edits) -------------------------
    reg.section(Section::Markers);
    {
        // `(make-marker pos)` → a marker id (allocated synchronously).
        let m = markers.clone();
        reg.register_fn("make-marker", move |pos: isize| -> usize {
            m.lock().unwrap().alloc(scheme_pos(pos)) as usize
        });
    }
    {
        // `(marker-position id)` → its current byte position, or -1.
        let m = markers.clone();
        reg.register_fn("marker-position", move |id: u64| -> isize {
            m.lock()
                .unwrap()
                .positions
                .get(&id)
                .map_or(-1, |p| *p as isize)
        });
    }
    {
        let m = markers.clone();
        reg.register_fn("set-marker", move |id: u64, pos: isize| {
            m.lock().unwrap().positions.insert(id, scheme_pos(pos));
        });
    }
    {
        let m = markers.clone();
        reg.register_fn("delete-marker", move |id: u64| {
            m.lock().unwrap().positions.remove(&id);
        });
    }
    // --- Pure position motions (compute, don't move) ----------------------
    // These resolve a *new byte offset* from a given one against the published
    // buffer, reusing the editor's grapheme/word/line logic. Plain motions
    // (`forward-char`, …) move the primary cursor as a side effect; these are
    // pure, so Helix's selection-first commands can compute a fresh head for
    // *every* selection and hand the lot back via `(set-selections …)`.
    macro_rules! register_pos_fn {
        ($name:literal, $f:expr) => {{
            let shared = snapshot.clone();
            reg.register_fn($name, move |pos: isize| -> usize {
                let pos = scheme_pos(pos);
                let buf = crate::buffer::Buffer::from_rope(shared.load().text.clone());
                let f: fn(&crate::buffer::Buffer, usize) -> usize = $f;
                f(&buf, pos.min(buf.len()))
            });
        }};
    }
    reg.section(Section::Motions);
    register_pos_fn!("grapheme-forward", |b, p| b.next_grapheme(p));
    register_pos_fn!("grapheme-backward", |b, p| b.prev_grapheme(p));
    register_pos_fn!("word-forward", |b, p| b.forward_word(p));
    register_pos_fn!("word-forward-start", |b, p| b.forward_word_start(p));
    register_pos_fn!("word-backward", |b, p| b.backward_word(p));
    register_pos_fn!("word-backward-start", |b, p| b.backward_word_start(p));
    register_pos_fn!("word-forward-end", |b, p| b.forward_word_end(p));
    register_pos_fn!("bigword-forward-start", |b, p| b.forward_bigword_start(p));
    register_pos_fn!("bigword-backward-start", |b, p| b.backward_bigword_start(p));
    register_pos_fn!("bigword-forward-end", |b, p| b.forward_bigword_end(p));
    register_pos_fn!("word-backward-end", |b, p| b.backward_word_end(p));
    register_pos_fn!("bigword-backward-end", |b, p| b.backward_bigword_end(p));
    {
        // `(buffer-regex-matches pattern start end)` → a flat list
        // `(s0 e0 s1 e1 …)` of the BUFFER byte ranges of every match inside
        // [start, end). Offsets are buffer bytes, so callers never mix the
        // char-indexed string accessors with regex byte offsets.
        let shared = snapshot.clone();
        reg.register_fn(
            "buffer-regex-matches",
            move |pattern: String, start: usize, end: usize| -> Vec<usize> {
                let Ok(re) = regex(&pattern) else {
                    return Vec::new();
                };
                let rope = shared.load().text.clone();
                let (a, b) = (snap_byte(&rope, start), snap_byte(&rope, end));
                if a >= b {
                    return Vec::new();
                }
                let text = rope.byte_slice(a..b).to_string();
                re.find_iter(&text)
                    .flat_map(|m| [a + m.start(), a + m.end()])
                    .collect()
            },
        );
    }
    {
        // `(regex-search-pos pattern from forward?)` → byte offset of the next
        // match strictly after (forward) / before (backward) `from`, wrapping
        // around the buffer like `n`/`N`; -1 when none (or a bad regex).
        let shared = snapshot.clone();
        reg.register_fn(
            "regex-search-pos",
            move |pattern: String, from: usize, forward: bool| -> isize {
                let Ok(re) = regex(&pattern) else {
                    return -1;
                };
                let text = shared.load().text.to_string();
                let starts: Vec<usize> = re.find_iter(&text).map(|m| m.start()).collect();
                let hit = if forward {
                    starts.iter().find(|&&s| s > from).or(starts.first())
                } else {
                    starts.iter().rev().find(|&&s| s < from).or(starts.last())
                };
                hit.map_or(-1, |&h| h as isize)
            },
        );
    }
    register_pos_fn!("line-start-pos", |b, p| b.line_range_at(p).start);
    register_pos_fn!("line-end-pos", |b, p| b.line_range_at(p).end);
    register_pos_fn!("line-of-pos", |b, p| b.line_index_at_byte(p));
    // `(char-column pos)` → the CHARACTER column of `pos` on its line. Vertical
    // motion must carry a char column, never a byte one: a byte column landing
    // on a line with multibyte text points inside a character, and the rope
    // refuses to slice there. Paired with `pos-at-char-column` below.
    register_pos_fn!("char-column", |b, p| b.char_column(p));
    {
        // `(line-start-of-line n)` → first byte of (0-based) line `n`, clamped.
        let shared = snapshot.clone();
        reg.register_fn("line-start-of-line", move |line: usize| -> usize {
            let buf = crate::buffer::Buffer::from_rope(shared.load().text.clone());
            let last = buf.line_count().saturating_sub(1);
            buf.line_range(line.min(last)).start
        });
    }
    {
        // `(pos-at-char-column line col)` → the byte offset of character column
        // `col` on (0-based) `line`, clamped to that line's end. The inverse of
        // `char-column`; together they are how Scheme does vertical motion
        // without ever synthesising a mid-codepoint offset.
        let shared = snapshot.clone();
        reg.register_fn(
            "pos-at-char-column",
            move |line: usize, col: usize| -> usize {
                let buf = crate::buffer::Buffer::from_rope(shared.load().text.clone());
                buf.byte_at_char_column(line, col)
            },
        );
    }
    // --- Regexp (over strings, and over the buffer from point) ------------
    reg.section(Section::Regexp);
    reg.register_fn("string-match", move |pattern: String, s: String| -> isize {
        regex(&pattern)
            .ok()
            .and_then(|re| re.find(&s).map(|m| m.start() as isize))
            .unwrap_or(-1)
    });
    reg.register_fn(
        "regexp-captures",
        move |pattern: String, s: String| -> Vec<String> {
            // Group 0 = whole match, then each capture group (absent group = "").
            match regex(&pattern).ok().and_then(|re| re.captures(&s)) {
                Some(caps) => caps
                    .iter()
                    .map(|g| g.map(|m| m.as_str().to_string()).unwrap_or_default())
                    .collect(),
                None => Vec::new(),
            }
        },
    );
    reg.register_fn(
        "replace-regexp-in-string",
        move |pattern: String, replacement: String, s: String| -> String {
            // `$1` / `${name}` expansion (regex-crate syntax).
            match regex(&pattern) {
                Ok(re) => re.replace_all(&s, replacement.as_str()).into_owned(),
                Err(_) => s,
            }
        },
    );
    {
        // `(buffer-search-all regex start end)` → every match of `regex` in the
        // active buffer whose bytes lie within `[start, end)`, as a list of
        // `(mstart mend "match" "group1" …)` records (BYTE offsets; an unmatched
        // group is ""). `^`/`$` are line anchors (multi-line), evaluated against
        // the real line boundaries even when `start`/`end` fall mid-line. An
        // invalid regex returns its error message as a string instead. Reads
        // the published snapshot rope directly — no whole-buffer string crosses
        // into Scheme (query-replace / occur on big files).
        let shared = snapshot.clone();
        reg.register_fn(
            "buffer-search-all",
            move |pattern: String, start: usize, end: usize| -> SteelVal {
                let re = match ml_regex(&pattern) {
                    Ok(re) => re,
                    Err(err) => return SteelVal::StringV(err.to_string().into()),
                };
                let rope = shared.load().text.clone();
                buffer_search_all(&rope, &re, start, end)
            },
        );
    }
    // `(regexp-search-lines regex lines)` → for each string in `lines`, the
    // list of its non-empty matches as `(start end)` BYTE offsets. One call and
    // one regex compile for a whole result buffer (occur / grep highlighting);
    // an invalid regex yields `()` for every line.
    reg.register_fn(
        "regexp-search-lines",
        |pattern: String, StringList(lines): StringList| -> SteelVal {
            let re = ml_regex(&pattern).ok();
            let per_line: Vec<SteelVal> = lines
                .iter()
                .map(|line| {
                    let spans: Vec<SteelVal> = re
                        .iter()
                        .flat_map(|re| re.find_iter(line))
                        .filter(|m| m.end() > m.start())
                        .map(|m| {
                            SteelVal::ListV(
                                vec![
                                    SteelVal::IntV(m.start() as isize),
                                    SteelVal::IntV(m.end() as isize),
                                ]
                                .into(),
                            )
                        })
                        .collect();
                    SteelVal::ListV(spans.into())
                })
                .collect();
            SteelVal::ListV(per_line.into())
        },
    );
    {
        // `(re-search-forward pattern)` → `(start end)` of the next match at/after
        // point, or `()` if none. Scheme drives point/edits from the result.
        let shared = snapshot.clone();
        reg.register_fn("re-search-forward", move |pattern: String| -> Vec<usize> {
            let (rope, p) = {
                let snap = shared.load();
                (snap.text.clone(), snap.point)
            };
            let from = snap_byte(&rope, p);
            match re_search_forward_rope(&rope, from, &pattern) {
                Some((start, end)) => vec![start, end],
                None => Vec::new(),
            }
        });
    }
    // Session persistence (savehist / desktop) — see src/script/session.rs.
    reg.section(Section::Session);
    register_session(&mut reg, shared);
    // The unlocked authinfo.gpg store behind scheme/secrets.scm.
    reg.section(Section::Secrets);
    crate::authinfo::register(&mut reg);
    let _ = api_cell.set(api);
    (engine, names)
}

/// The pure core of `re-search-forward`: the first match of `pattern` in
/// `text` starting at byte `from`, as byte offsets into `text`. The whole text
/// is searched (from `from`), not a slice, so `^`, `\b` and lookbehind-ish
/// anchors still see the context before point. `^`/`$` match at line
/// boundaries, as in Emacs. `None` on no match or an invalid pattern.
pub(crate) fn re_search_forward_in(
    text: &str,
    from: usize,
    pattern: &str,
) -> Option<(usize, usize)> {
    let re = ml_regex(pattern).ok()?;
    if from > text.len() || !text.is_char_boundary(from) {
        return None;
    }
    re.find_at(text, from).map(|m| (m.start(), m.end()))
}

/// [`re_search_forward_in`] over a rope, without materializing the text
/// before point: only `[c, len)` is copied, `c` being the start of the
/// codepoint just before `from`. That is exactly as much context as the whole
/// text would give — every regex-crate look-around (`^`, `$`, `\b`, `\<`, …)
/// inspects at most one codepoint on either side — and since `from` stays
/// past the slice start, `\A` still can't match there (the whole-text search
/// only allows it at byte 0). `from` must be a char boundary.
pub(crate) fn re_search_forward_rope(
    rope: &Rope,
    from: usize,
    pattern: &str,
) -> Option<(usize, usize)> {
    let base = match rope.byte_to_char(from) {
        0 => 0,
        c => rope.char_to_byte(c - 1),
    };
    let text = rope.byte_slice(base..).to_string();
    re_search_forward_in(&text, from - base, pattern).map(|(s, e)| (base + s, base + e))
}

/// How many compiled regexes [`regex`] / [`ml_regex`] keep per thread.
const REGEX_CACHE_SIZE: usize = 8;

/// `pattern` compiled (with `^`/`$` as line anchors when `multi_line`),
/// reusing a recent compile: Scheme calls the regex primitives with the same
/// few patterns over and over (a search loop, a highlighter per keystroke), and
/// compiling dominates a match on short text. A tiny most-recent-first list
/// per thread; errors aren't cached.
fn cached_regex(pattern: &str, multi_line: bool) -> Result<Regex, regex::Error> {
    thread_local! {
        static CACHE: std::cell::RefCell<Vec<(String, bool, Regex)>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(i) = cache
            .iter()
            .position(|(p, ml, _)| *ml == multi_line && p == pattern)
        {
            let hit = cache.remove(i);
            let re = hit.2.clone();
            cache.insert(0, hit);
            return Ok(re);
        }
        let re = regex::RegexBuilder::new(pattern)
            .multi_line(multi_line)
            .build()?;
        cache.truncate(REGEX_CACHE_SIZE - 1);
        cache.insert(0, (pattern.to_string(), multi_line, re.clone()));
        Ok(re)
    })
}

/// `pattern` compiled with default flags (see [`cached_regex`]).
pub(crate) fn regex(pattern: &str) -> Result<Regex, regex::Error> {
    cached_regex(pattern, false)
}

/// `pattern` compiled with `^`/`$` matching at line boundaries, as in Emacs
/// (see [`cached_regex`]).
pub(crate) fn ml_regex(pattern: &str) -> Result<Regex, regex::Error> {
    cached_regex(pattern, true)
}

/// Registers Steel functions into the engine while recording every registered
/// name. The global-name list (consumed by the built-in Steel LSP and the
/// minibuffer symbol completion via [`ScriptEngine::global_names`]) is thus
/// derived from registration itself and can never drift from it.
///
/// It also records each primitive's category (the current [`Registrar::section`])
/// and its argument/return types for the package-API reference ([`super::api`]).
pub(crate) struct Registrar<'a> {
    engine: &'a mut Engine,
    names: &'a mut Vec<String>,
    api: &'a mut Vec<ApiEntry>,
    section: Section,
}

impl Registrar<'_> {
    pub(crate) fn register_fn<FN, ARGS, RET>(&mut self, name: &'static str, func: FN)
    where
        Engine: RegisterFn<FN, ARGS, RET>,
    {
        self.names.push(name.to_string());
        self.api.push(ApiEntry::new(
            name,
            self.section,
            std::any::type_name::<ARGS>(),
            std::any::type_name::<RET>(),
        ));
        self.engine.register_fn(name, func);
    }

    /// File every following registration under `section` in the API reference.
    pub(crate) fn section(&mut self, section: Section) {
        self.section = section;
    }
}

/// Register a Steel function `name` that, when called, enqueues a clone of `cmd`.
pub(crate) fn register_nullary(
    reg: &mut Registrar,
    queue: &Queue,
    name: &'static str,
    cmd: EditorCommand,
) {
    let q = queue.clone();
    reg.register_fn(name, move || q.push(cmd.clone()));
}

/// Register a one-argument Steel command: `name` enqueues the command built by
/// `make` from its argument. Collapses the `{ let q = queue.clone(); ... push }`
/// boilerplate for the many single-payload commands.
pub(crate) fn register_unary<T>(
    reg: &mut Registrar,
    queue: &Queue,
    name: &'static str,
    make: impl Fn(T) -> EditorCommand + Send + Sync + 'static,
) where
    T: steel::rvals::FromSteelVal + 'static,
{
    let q = queue.clone();
    reg.register_fn(name, move |arg: T| q.push(make(arg)));
}

/// Register a two-argument Steel command (see [`register_unary`]).
pub(crate) fn register_binary<T, U>(
    reg: &mut Registrar,
    queue: &Queue,
    name: &'static str,
    make: impl Fn(T, U) -> EditorCommand + Send + Sync + 'static,
) where
    T: steel::rvals::FromSteelVal + 'static,
    U: steel::rvals::FromSteelVal + 'static,
{
    let q = queue.clone();
    reg.register_fn(name, move |a: T, b: U| q.push(make(a, b)));
}

/// Register a three-argument Steel command (see [`register_unary`]).
pub(crate) fn register_ternary<T, U, V>(
    reg: &mut Registrar,
    queue: &Queue,
    name: &'static str,
    make: impl Fn(T, U, V) -> EditorCommand + Send + Sync + 'static,
) where
    T: steel::rvals::FromSteelVal + 'static,
    U: steel::rvals::FromSteelVal + 'static,
    V: steel::rvals::FromSteelVal + 'static,
{
    let q = queue.clone();
    reg.register_fn(name, move |a: T, b: U, c: V| q.push(make(a, b, c)));
}

/// Register a nullary Steel *read* bridge: `name` returns a value derived from
/// the editor's published snapshot. Collapses the repeated
/// `{ let shared = snapshot.clone(); engine.register_fn(name, move || …) }`
/// boilerplate for the many single-snapshot readers. `get` runs under the
/// snapshot lock and must produce an owned value (clone out anything borrowed).
pub(crate) fn register_snapshot_read<T: IntoSteelVal + 'static>(
    reg: &mut Registrar,
    snapshot: &SharedSnapshot,
    name: &'static str,
    get: impl Fn(&EditorSnapshot) -> T + Send + Sync + 'static,
) {
    let shared = snapshot.clone();
    reg.register_fn(name, move || get(&shared.load()));
}

/// Matches of `re` inside bytes `[start, end)` of `rope` — the worker behind
/// `buffer-search-all`. Searches the enclosing whole lines so `^`/`$`/`\b`
/// see real context, then keeps only matches fully inside the window.
pub(crate) fn buffer_search_all(rope: &Rope, re: &Regex, start: usize, end: usize) -> SteelVal {
    let (s, e) = (snap_byte(rope, start), snap_byte(rope, end));
    if s > e {
        return SteelVal::ListV(Vec::<SteelVal>::new().into());
    }
    let from = rope.line_to_byte(rope.byte_to_line(s));
    let to = {
        let line = rope.byte_to_line(e);
        let line_start = rope.line_to_byte(line);
        let line_str = rope.line(line).to_string();
        line_start + line_str.find('\n').unwrap_or(line_str.len())
    }
    .max(e);
    let text = rope.byte_slice(from..to).to_string();
    let out: Vec<SteelVal> = re
        .captures_iter(&text)
        .filter_map(|caps| {
            let m = caps.get(0)?;
            let (ms, me) = (from + m.start(), from + m.end());
            if ms < s || me > e {
                return None;
            }
            let mut rec = vec![SteelVal::IntV(ms as isize), SteelVal::IntV(me as isize)];
            rec.extend(
                caps.iter()
                    .map(|g| SteelVal::StringV(g.map_or("", |g| g.as_str()).into())),
            );
            Some(SteelVal::ListV(rec.into()))
        })
        .collect();
    SteelVal::ListV(out.into())
}

/// Snap a byte index down to the nearest UTF-8 char boundary in `rope`, clamped
/// to its length. Slicing a `Rope` panics on a non-boundary index, so the
/// text-slicing bridges route caller-supplied offsets through this first — the
/// boundary safety the old `String`-slicing bridges did by hand.
pub(crate) fn snap_byte(rope: &Rope, byte: usize) -> usize {
    let byte = byte.min(rope.len_bytes());
    rope.char_to_byte(rope.byte_to_char(byte))
}

#[cfg(test)]
mod regex_tests {
    use super::*;

    /// The rope search sees the same context as the whole-text one: line and
    /// word anchors right at point, `\A` only at byte 0, multibyte text.
    #[test]
    fn rope_search_matches_whole_text_search() {
        let text = "foo bar\nbaz qux\n\u{e9}t\u{e9} foo\n";
        let rope = Rope::from_str(text);
        let patterns = [
            "^b",
            r"\bar",
            r"\Bar",
            "foo",
            r"\Afoo",
            r"\A",
            "$",
            r"(?m)^\w+$",
            "t\u{e9}",
            r"\b",
            "x*",
            "^",
            r"\z",
            "(",
        ];
        for pattern in patterns {
            for from in (0..=text.len()).filter(|&i| text.is_char_boundary(i)) {
                assert_eq!(
                    re_search_forward_rope(&rope, from, pattern),
                    re_search_forward_in(text, from, pattern),
                    "{pattern:?} from {from}"
                );
            }
        }
    }

    #[test]
    fn regex_cache_keys_on_flags() {
        let ml = ml_regex("^b").unwrap();
        let plain = regex("^b").unwrap();
        assert!(ml.is_match("a\nb"));
        assert!(!plain.is_match("a\nb"));
        assert!(regex("(").is_err());
        for i in 0..20 {
            assert!(regex(&format!("a{{{i}}}")).is_ok());
        }
        assert!(ml_regex("^b").unwrap().is_match("a\nb"));
    }
}
