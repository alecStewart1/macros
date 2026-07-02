use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use regex::Regex;
use ropey::Rope;
use steel::rvals::{IntoSteelVal, SteelVal};
use steel::steel_vm::engine::Engine;
use steel::steel_vm::register_fn::RegisterFn;

use super::*;

/// The nullary commands: each Steel name pushes its variant. Shared by engine
/// registration (on the thread) and the handle's fast-path table.
pub(crate) fn nullary_commands() -> Vec<(&'static str, EditorCommand)> {
    use EditorCommand::*;
    vec![
        ("forward-char", ForwardChar),
        ("backward-char", BackwardChar),
        ("next-line", NextLine),
        ("previous-line", PreviousLine),
        ("forward-word", ForwardWord),
        ("forward-word-start", ForwardWordStart),
        ("backward-word", BackwardWord),
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
    ]
}

/// Build the Steel engine and register every command/bridge function. Runs on
/// the engine thread (the engine is `!Send`).
pub(crate) fn build_engine(
    queue: &Queue,
    buffers: &BufferNames,
    buffer_paths: &BufferPaths,
    snapshot: &SharedSnapshot,
    markers: &Markers,
    options: &Options,
) -> (Engine, Vec<String>) {
    let mut engine = Engine::new();
    let mut names: Vec<String> = Vec::new();
    let mut reg = Registrar {
        engine: &mut engine,
        names: &mut names,
    };
    for (name, command) in nullary_commands() {
        register_nullary(&mut reg, queue, name, command);
    }

    // Commands carrying a payload.
    register_unary(&mut reg, queue, "insert", EditorCommand::Insert);
    // `message` accepts any value, not just strings: strings render verbatim,
    // everything else via its Steel display — so (message (+ 1 2)) shows "3"
    // rather than raising a ConversionError.
    {
        let q = queue.clone();
        reg.register_fn("message", move |arg: SteelVal| {
            let text = match arg {
                SteelVal::StringV(s) => s.to_string(),
                other => other.to_string(),
            };
            q.lock().unwrap().push(EditorCommand::Message(text));
        });
    }
    // Shadow of every binding (mode, chord, command), for help/discoverability
    // read bridges. Written alongside the DefineKey command; lives on the engine
    // thread with the read fns.
    let bindings: Arc<Mutex<Vec<(String, String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    {
        let q = queue.clone();
        let shadow = bindings.clone();
        reg.register_fn(
            "define-key",
            move |mode: String, chord: String, command: Callback| {
                // The command may be a "name" string OR a 'name symbol (the
                // canonical config form). Both resolve to the same name; it's
                // looked up and run by name at keypress time.
                let Callback(command) = command;
                {
                    let mut s = shadow.lock().unwrap();
                    // Last definition for a (mode, chord) wins (matches the keymap).
                    s.retain(|(m, c, _)| !(m == &mode && c == &chord));
                    s.push((mode.clone(), chord.clone(), command.clone()));
                }
                q.lock()
                    .unwrap()
                    .push(EditorCommand::DefineKey(mode, chord, command));
            },
        );
    }
    {
        // `(key-binding mode chord)` → the command bound there, or "".
        let shadow = bindings.clone();
        reg.register_fn(
            "key-binding",
            move |mode: String, chord: String| -> String {
                shadow
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|(m, c, _)| m == &mode && c == &chord)
                    .map(|(_, _, cmd)| cmd.clone())
                    .unwrap_or_default()
            },
        );
    }
    {
        // `(keys-with-prefix mode prefix)` → "chord\tcommand" lines whose chord
        // starts with `prefix` (which-key / describe-prefix).
        let shadow = bindings.clone();
        reg.register_fn(
            "keys-with-prefix",
            move |mode: String, prefix: String| -> Vec<String> {
                shadow
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|(m, c, _)| m == &mode && c.starts_with(&prefix))
                    .map(|(_, c, cmd)| format!("{c}\t{cmd}"))
                    .collect()
            },
        );
    }
    {
        // `(command-list)` → all nullary command names (M-x / apropos).
        let names: Vec<String> = nullary_commands()
            .iter()
            .map(|(n, _)| n.to_string())
            .collect();
        reg.register_fn("command-list", move || -> Vec<String> { names.clone() });
    }
    {
        // `(bundled-languages)` → every language id with a bundled tree-sitter
        // grammar. language-modes.scm reads this to generate an `<id>-mode` M-x
        // command per language (so `M-x scala-mode` switches the buffer's mode).
        let langs: Vec<String> = crate::highlight::BUNDLED_LANGUAGES
            .iter()
            .map(|s| s.to_string())
            .collect();
        reg.register_fn("bundled-languages", move || -> Vec<String> {
            langs.clone()
        });
    }
    {
        // `(theme-names)` → every bundled theme name (the stem of each
        // scheme/themes/*.scm). The `load-theme` wrapper in core.scm reads this to
        // complete theme names — in the M-x prompt and the in-buffer argument CAPF.
        reg.register_fn("theme-names", || -> Vec<String> {
            crate::runtime::theme_names()
        });
    }
    {
        // `(installed-languages)` → ids of grammars downloaded + compiled at
        // runtime (M-x install-language). grammar.scm reads this alongside
        // `(bundled-languages)` so installed languages also get `<id>-mode`.
        reg.register_fn("installed-languages", || -> Vec<String> {
            crate::grammar::installed_ids()
        });
    }
    {
        // `(grammar-install id repo rev extensions symbol subdir)` → queue an
        // async download + compile + load of a tree-sitter grammar. `extensions`
        // is a whitespace-separated string of bare extensions; `symbol` and
        // `subdir` are "" for the defaults (parser symbol = id, grammar at repo
        // root). The Scheme `install-language` wrapper builds these from nicer
        // args. Returns immediately; progress/result echoes to the minibuffer
        // (see Editor::install_grammar).
        let q = queue.clone();
        reg.register_fn(
            "grammar-install",
            move |id: String,
                  repo: String,
                  rev: String,
                  extensions: String,
                  symbol: String,
                  subdir: String| {
                q.lock()
                    .unwrap()
                    .push(EditorCommand::InstallGrammar(Box::new(GrammarInstall {
                        id,
                        repo,
                        rev,
                        extensions,
                        symbol,
                        subdir,
                    })));
            },
        );
    }
    {
        // The doc index: name → (doc, file, line). Steel has no runtime
        // docstrings, so the `;;` block preceding each top-level define IS its
        // documentation; file+line let the picker jump to the implementation.
        // Seeded at startup from the bundled library and grown at runtime as
        // packages load (`index-doc-file`), so built-ins and dynamically loaded
        // packages are searched uniformly. Keyed by name → an UPSERT (a
        // redefinition or a package reload overwrites the entry), so the index
        // stays flat no matter how often a file is reloaded in a session.
        //
        // Powers `(function-doc name)`, `(function-list)`, `(function-source
        // name)` → describe-function / apropos / helm-apropos.
        type DocIndex = HashMap<String, (String, String, usize)>;
        let docs: Arc<RwLock<DocIndex>> = Arc::new(RwLock::new(
            crate::runtime::builtin_docs()
                .into_iter()
                .map(|e| (e.name, (e.doc, e.file, e.line)))
                .collect(),
        ));

        // `(function-doc name)` → the doc comment, or "".
        let d = docs.clone();
        reg.register_fn("function-doc", move |name: String| -> String {
            d.read()
                .unwrap()
                .get(&name)
                .map(|(doc, _, _)| doc.clone())
                .unwrap_or_default()
        });

        // `(function-list)` → every indexed name, sorted.
        let d = docs.clone();
        reg.register_fn("function-list", move || -> Vec<String> {
            let mut names: Vec<String> = d.read().unwrap().keys().cloned().collect();
            names.sort();
            names
        });

        // `(function-source name)` → `(file line)` (1-based line), or `'()` when
        // the name is unknown or carries no location. The jump action opens
        // `file` at `line`, or falls back to the doc when the file is absent.
        let d = docs.clone();
        reg.register_fn("function-source", move |name: String| -> Vec<SteelVal> {
            match d.read().unwrap().get(&name) {
                Some((_, file, line)) if !file.is_empty() => vec![
                    SteelVal::StringV(file.clone().into()),
                    SteelVal::IntV(*line as isize),
                ],
                _ => vec![],
            }
        });

        // `(index-doc-file path)` → scrape `path` from disk and merge its
        // documented defines into the index (upsert by name). Called as a package
        // loads so its commented functions/variables become searchable and
        // jumpable. Returns the number of defines indexed (0 if unreadable).
        let d = docs.clone();
        reg.register_fn("index-doc-file", move |path: String| -> usize {
            let Ok(src) = std::fs::read_to_string(&path) else {
                return 0;
            };
            let entries = crate::runtime::scrape_docs(&src, &path);
            let n = entries.len();
            let mut idx = d.write().unwrap();
            for e in entries {
                idx.insert(e.name, (e.doc, e.file, e.line));
            }
            n
        });
    }
    {
        // `(macros-version)` → the editor's package version (from Cargo.toml,
        // baked in at compile time). Powers the `version` command.
        reg.register_fn("macros-version", || -> String {
            env!("CARGO_PKG_VERSION").to_string()
        });
    }
    {
        // `(diff-strings old new)` → line-level diff ops, one per line as
        // "tag\told_start\told_end\tnew_start\tnew_end" with 0-based,
        // end-exclusive LINE indices. tag ∈ equal|delete|insert|replace.
        // Pure function (no editor state); powers scheme/ediff.scm — Scheme
        // merges adjacent non-equal ops into navigable hunks.
        reg.register_fn(
            "diff-strings",
            move |old: String, new: String| -> Vec<String> {
                let diff = similar::TextDiff::from_lines(&old, &new);
                diff.ops()
                    .iter()
                    .map(|op| {
                        let (tag, old_range, new_range) = op.as_tag_tuple();
                        let tag = match tag {
                            similar::DiffTag::Equal => "equal",
                            similar::DiffTag::Delete => "delete",
                            similar::DiffTag::Insert => "insert",
                            similar::DiffTag::Replace => "replace",
                        };
                        format!(
                            "{tag}\t{}\t{}\t{}\t{}",
                            old_range.start, old_range.end, new_range.start, new_range.end
                        )
                    })
                    .collect()
            },
        );
    }
    {
        // `(diff-inline old new)` → word-level refinement between two single
        // lines, one change per line as "tag\ttext" (tag ∈ equal|delete|insert).
        // Used to highlight intra-line changes within an ediff hunk.
        reg.register_fn(
            "diff-inline",
            move |old: String, new: String| -> Vec<String> {
                let diff = similar::TextDiff::from_words(&old, &new);
                diff.iter_all_changes()
                    .map(|c| {
                        let tag = match c.tag() {
                            similar::ChangeTag::Equal => "equal",
                            similar::ChangeTag::Delete => "delete",
                            similar::ChangeTag::Insert => "insert",
                        };
                        format!("{tag}\t{}", c.value().replace('\n', " "))
                    })
                    .collect()
            },
        );
    }
    register_binary(&mut reg, queue, "set-face", EditorCommand::SetFace);
    {
        let q = queue.clone();
        reg.register_fn("set-face-underline", move |name: String, on: bool| {
            q.lock()
                .unwrap()
                .push(EditorCommand::SetFaceUnderline(name, on));
        });
    }
    register_unary(
        &mut reg,
        queue,
        "clipboard-set",
        EditorCommand::ClipboardSet,
    );
    {
        // `load-theme-impl` accepts a string or a symbol — `(load-theme "x")` and
        // `(load-theme 'x)` both reach here via the core.scm wrapper. Any other
        // value is stringified, so a bad arg echoes "No theme named …" rather than
        // failing to type-check. The wrapper adds the no-arg interactive prompt.
        let q = queue.clone();
        reg.register_fn("load-theme-impl", move |value: SteelVal| {
            let name = steel_string(&value).unwrap_or_else(|| value.to_string());
            q.lock().unwrap().push(EditorCommand::LoadTheme(name));
        });
    }
    register_binary(&mut reg, queue, "set-lsp", EditorCommand::SetLsp);
    register_binary(
        &mut reg,
        queue,
        "set-extension",
        EditorCommand::SetExtension,
    );
    register_binary(
        &mut reg,
        queue,
        "set-language-id",
        EditorCommand::SetLanguageId,
    );
    {
        // `set-option` accepts a number, boolean, or string; the value is coerced
        // to the option's declared type when applied (see `Settings::set`).
        let q = queue.clone();
        reg.register_fn("set-option", move |key: String, value: SteelVal| {
            q.lock()
                .unwrap()
                .push(EditorCommand::SetOption(key, option_from_steelval(&value)));
        });
    }
    {
        // Read-side bridge: Scheme can query the open buffers (for pickers).
        let shared = buffers.clone();
        reg.register_fn("buffer-names", move || -> Vec<String> {
            shared.lock().unwrap().clone()
        });
    }
    {
        // `(buffer-file-name-of name)` → that buffer's absolute file path, or ""
        // if the buffer has no file (or doesn't exist). Unlike `buffer-file-name`
        // (active buffer only), this resolves any open buffer by name.
        let shared = buffer_paths.clone();
        reg.register_fn("buffer-file-name-of", move |name: String| -> String {
            shared
                .lock()
                .unwrap()
                .get(&name)
                .cloned()
                .unwrap_or_default()
        });
    }
    {
        // Read-side bridge: `(get-option "name")` returns the option's current
        // value as a native bool / number / string, or #f if it isn't set.
        let opts = options.clone();
        reg.register_fn("get-option", move |key: String| -> SteelVal {
            opts.lock()
                .unwrap()
                .get(&key)
                .map_or(SteelVal::BoolV(false), option_to_steelval)
        });
    }
    // `(option-list)` → every option key Rust knows about, so helm-apropos can
    // surface options ("font" → font-size) next to functions and variables.
    reg.register_fn("option-list", || -> Vec<String> {
        crate::settings::KNOWN_OPTIONS
            .iter()
            .map(|s| s.to_string())
            .collect()
    });
    register_unary(
        &mut reg,
        queue,
        "switch-to-buffer",
        EditorCommand::SwitchToBufferNamed,
    );
    register_unary(
        &mut reg,
        queue,
        "kill-buffer-named",
        EditorCommand::KillBufferNamed,
    );
    register_unary(
        &mut reg,
        queue,
        "set-major-mode",
        EditorCommand::SetMajorMode,
    );
    register_unary(&mut reg, queue, "open-file", EditorCommand::OpenFile);
    register_unary(&mut reg, queue, "lsp-rename-to", EditorCommand::LspRename);
    register_unary(
        &mut reg,
        queue,
        "lsp-workspace-symbol-go",
        EditorCommand::LspWorkspaceSymbol,
    );
    {
        let q = queue.clone();
        // (lsp-request method kind params callback) — the generic bridge.
        reg.register_fn(
            "lsp-request",
            move |method: String, kind: String, params: String, callback: Callback| {
                let callback = callback.0;
                q.lock().unwrap().push(EditorCommand::LspRequest {
                    method,
                    kind,
                    params,
                    callback,
                });
            },
        );
    }
    register_ternary(&mut reg, queue, "lsp-goto", |uri, line, character| {
        EditorCommand::LspGoto {
            uri,
            line,
            character,
        }
    });
    register_unary(
        &mut reg,
        queue,
        "lsp-apply-workspace-edit",
        EditorCommand::LspApplyWorkspaceEdit,
    );
    register_unary(
        &mut reg,
        queue,
        "lsp-apply-text-edits",
        EditorCommand::LspApplyTextEdits,
    );
    register_unary(
        &mut reg,
        queue,
        "open-file-other-window",
        EditorCommand::OpenFileOtherWindow,
    );
    register_unary(
        &mut reg,
        queue,
        "select-window",
        EditorCommand::SelectWindow,
    );
    register_unary(
        &mut reg,
        queue,
        "split-window-right-ratio",
        EditorCommand::SplitWindowRightRatio,
    );
    {
        // `(set-line-icon line col "folder")` attaches a bundled icon (asset
        // `icons/<name>.svg`) at character column `col` of `line` in the active
        // buffer; `(clear-line-icons)` removes them all. Used by the file tree.
        register_ternary(&mut reg, queue, "set-line-icon", EditorCommand::SetLineIcon);
        register_nullary(
            &mut reg,
            queue,
            "clear-line-icons",
            EditorCommand::ClearLineIcons,
        );
    }
    {
        // `(set-line-image line "/abs/path.png" [max-width])` displays a decoded
        // raster image on rows reserved below `line` of the active buffer;
        // `(clear-line-images)` removes them all. `max-width` (px) caps the
        // displayed width — 0 uses the default cap. The optional arg is provided
        // by the `set-line-image` Scheme wrapper (core.scm); this is the impl.
        // Drives Org's inline-image / LaTeX-preview display and the roam graph.
        let q = queue.clone();
        reg.register_fn(
            "set-line-image-impl",
            move |line: usize, path: String, max_width: usize| {
                q.lock().unwrap().push(EditorCommand::SetLineImage {
                    line,
                    path,
                    max_width,
                });
            },
        );
        register_nullary(
            &mut reg,
            queue,
            "clear-line-images",
            EditorCommand::ClearLineImages,
        );
        // `(set-line-indent line cols)` gives buffer `line` a virtual left indent
        // of `cols` columns (display-only); `(clear-line-indents)` removes them
        // all. Drives Org's org-indent-mode look.
        register_binary(&mut reg, queue, "set-line-indent", |line, cols| {
            EditorCommand::SetLineIndent { line, cols }
        });
        register_nullary(
            &mut reg,
            queue,
            "clear-line-indents",
            EditorCommand::ClearLineIndents,
        );
        // `(set-line-scale line scale)` renders buffer `line` at `scale`× the
        // base font size (e.g. a heading); `(clear-line-scales)` resets all.
        let q = queue.clone();
        reg.register_fn("set-line-scale", move |line: usize, scale: f64| {
            q.lock().unwrap().push(EditorCommand::SetLineScale {
                line,
                scale: scale as f32,
            });
        });
        register_nullary(
            &mut reg,
            queue,
            "clear-line-scales",
            EditorCommand::ClearLineScales,
        );
    }
    {
        // `(window-list)` → pane ids in cycle order; `(selected-window)` → the
        // focused pane id. Read bridges over the snapshot.
        register_snapshot_read(&mut reg, snapshot, "window-list", |s| s.windows.clone());
        register_snapshot_read(&mut reg, snapshot, "selected-window", |s| s.selected_window);
        {
            // `(get-buffer-window name)` → the pane id displaying buffer `name`,
            // or -1 if it isn't shown in any window (Emacs's `get-buffer-window`).
            let shared = snapshot.clone();
            reg.register_fn("get-buffer-window", move |name: String| -> isize {
                shared
                    .load()
                    .window_buffers
                    .iter()
                    .find(|(_, b)| *b == name)
                    .map_or(-1, |(id, _)| *id as isize)
            });
        }
        // `(visible-lines)` → (first-line last-line) currently on screen.
        register_snapshot_read(&mut reg, snapshot, "visible-lines", |s| {
            let (a, b) = s.visible_lines;
            vec![a, b]
        });
        register_snapshot_read(&mut reg, snapshot, "window-cursor-row", |s| {
            s.cursor_screen_row as i64
        });
    }
    // State bridges for dynamic sources (run on the engine thread — blocking
    // I/O here never touches the UI). All pure value-returning functions.
    reg.register_fn("current-directory", || -> String {
        std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    reg.register_fn("directory-of", |path: String| -> String {
        if path.ends_with('/') {
            return path;
        }
        std::path::Path::new(&path)
            .parent()
            .map(|d| {
                let mut s = d.to_string_lossy().into_owned();
                if !s.ends_with('/') {
                    s.push('/');
                }
                s
            })
            .unwrap_or_default()
    });
    reg.register_fn("list-directory", |path: String| -> Vec<String> {
        let mut entries = Vec::new();
        if let Ok(read) = std::fs::read_dir(&path) {
            for entry in read.flatten() {
                let mut full = entry.path().to_string_lossy().into_owned();
                if entry.path().is_dir() {
                    full.push('/');
                }
                entries.push(full);
            }
        }
        entries.sort();
        entries
    });
    register_binary(&mut reg, queue, "add-hook", EditorCommand::AddHook);
    register_binary(&mut reg, queue, "remove-hook", EditorCommand::RemoveHook);
    register_unary(
        &mut reg,
        queue,
        "add-completion-at-point-function",
        EditorCommand::AddCapf,
    );
    {
        // `(watch-directories owner dirs)` — replace `owner`'s watched dir set so
        // a change under any of them fires `directory-changed-hook` (dired-sidebar /
        // dired live-refresh). Replace semantics: pass `'()` to drop them.
        let q = queue.clone();
        reg.register_fn("watch-directories", move |owner: String, dirs: SteelVal| {
            q.lock().unwrap().push(EditorCommand::WatchDirectories(
                owner,
                steel_string_list(&dirs),
            ));
        });
    }
    {
        // `(watch-tree owner dirs)` — like `watch-directories` but recursive: a
        // change anywhere under `dirs` fires `directory-changed-hook` with the
        // watched root (magit watches its repo root). `'()` drops the watch.
        let q = queue.clone();
        reg.register_fn("watch-tree", move |owner: String, dirs: SteelVal| {
            q.lock()
                .unwrap()
                .push(EditorCommand::WatchTree(owner, steel_string_list(&dirs)));
        });
    }
    {
        // `(helm sources)` where sources is a list of `(name candidates action)`.
        let q = queue.clone();
        reg.register_fn("helm", move |sources: SteelVal| {
            q.lock()
                .unwrap()
                .push(EditorCommand::Helm(parse_helm_sources(&sources)));
        });
        // `(helm-from initial sources)` — Helm with a pre-filled input.
        let q = queue.clone();
        reg.register_fn("helm-from", move |initial: String, sources: SteelVal| {
            q.lock().unwrap().push(EditorCommand::HelmFrom(
                initial,
                parse_helm_sources(&sources),
            ));
        });
    }
    // --- Special buffers / transient (the magit primitives) ---------------
    // `(render-buffer name mode text faces)` — faces is a list of
    // `(line-number face-name)`. Replaces/creates a read-only buffer. The
    // `-quiet` variant refreshes in place without switching/focusing (sidebar
    // auto-refresh).
    register_render_buffer(&mut reg, queue, "render-buffer", false);
    register_render_buffer(&mut reg, queue, "render-buffer-quiet", true);
    register_unary(&mut reg, queue, "goto-line", EditorCommand::GotoLine);
    register_unary(
        &mut reg,
        queue,
        "scroll-cursor-to-row",
        EditorCommand::ScrollCursorToRow,
    );
    register_ternary(
        &mut reg,
        queue,
        "diff-buffers",
        |a, b, callback: Callback| EditorCommand::DiffBuffers {
            a,
            b,
            callback: callback.0,
        },
    );
    register_unary(&mut reg, queue, "buffer-clear-highlights", |name| {
        EditorCommand::BufferClearHighlights { name }
    });
    register_unary(
        &mut reg,
        queue,
        "snippet-expand",
        EditorCommand::SnippetExpand,
    );
    register_unary(
        &mut reg,
        queue,
        "avy-show-hints",
        EditorCommand::AvyShowHints,
    );
    register_nullary(
        &mut reg,
        queue,
        "avy-clear-hints",
        EditorCommand::AvyClearHints,
    );
    {
        // `(buffer-copy-lines src ss se dst ds de)` — copy a line range between
        // named buffers (ediff hunk copy). Six args, so registered directly.
        let q = queue.clone();
        reg.register_fn(
            "buffer-copy-lines",
            move |src: String, ss: usize, se: usize, dst: String, ds: usize, de: usize| {
                q.lock().unwrap().push(EditorCommand::BufferCopyLines {
                    src,
                    src_start: ss,
                    src_end: se,
                    dst,
                    dst_start: ds,
                    dst_end: de,
                });
            },
        );
    }
    {
        // `(buffer-highlight-lines name start end face)` — paint a face on a line
        // range of any named buffer (works on the non-selected ediff pane too).
        let q = queue.clone();
        reg.register_fn(
            "buffer-highlight-lines",
            move |name: String, start: usize, end: usize, face: String| {
                q.lock().unwrap().push(EditorCommand::BufferHighlightLines {
                    name,
                    start,
                    end,
                    face,
                });
            },
        );
    }
    // `(transient title rows)` — `rows` is a list of `(key label command)`, or
    // a list of `(column-title (key label command) …)` for columns. In the
    // `-sticky` variant selecting a row keeps the popup open — the row's
    // command re-opens it (to refresh state) or calls `transient-quit`, for
    // menus where you toggle several options before dismissing.
    register_transient(&mut reg, queue, "transient", false);
    register_transient(&mut reg, queue, "transient-sticky", true);
    register_nullary(
        &mut reg,
        queue,
        "transient-quit",
        EditorCommand::TransientQuit,
    );
    register_unary(&mut reg, queue, "goto-char", EditorCommand::GotoChar);
    register_binary(&mut reg, queue, "delete-region", |start, end| {
        EditorCommand::DeleteRegion { start, end }
    });
    register_binary(&mut reg, queue, "kill-region-between", |start, end| {
        EditorCommand::KillRegionBetween { start, end }
    });
    register_binary(&mut reg, queue, "set-region", |start, end| {
        EditorCommand::SetRegion { start, end }
    });
    {
        // `(set-selections '(a0 h0 a1 h1 …))` — replace the whole selection set
        // from a flat list of alternating anchor/head byte offsets (the first
        // pair is the primary). The Scheme reader returns selections in the same
        // flat shape (see the `selections` read), so a motion is read → map →
        // set-selections. An odd trailing element is ignored.
        let q = queue.clone();
        reg.register_fn("set-selections", move |flat: SteelVal| {
            let v = steel_usize_list(&flat);
            let pairs = v.chunks_exact(2).map(|c| (c[0], c[1])).collect();
            q.lock().unwrap().push(EditorCommand::SetSelections(pairs));
        });
    }
    register_binary(
        &mut reg,
        queue,
        "set-lsp-format-on-save",
        EditorCommand::SetLspFormatOnSave,
    );
    register_unary(
        &mut reg,
        queue,
        "lsp-semantic-tokens",
        EditorCommand::LspSemanticTokens,
    );
    register_ternary(&mut reg, queue, "replace-region", |start, end, text| {
        EditorCommand::ReplaceRegion { start, end, text }
    });
    register_binary(&mut reg, queue, "insert-at", |pos, text| {
        EditorCommand::InsertAt { pos, text }
    });
    register_unary(
        &mut reg,
        queue,
        "lsp-semantic-tokens-range",
        EditorCommand::LspSemanticTokensRange,
    );
    register_unary(
        &mut reg,
        queue,
        "lsp-semantic-tokens-delta",
        EditorCommand::LspSemanticTokensDelta,
    );
    register_binary(
        &mut reg,
        queue,
        "tree-sitter-query",
        |query, callback: Callback| EditorCommand::TreeSitterQuery {
            query,
            callback: callback.0,
        },
    );
    register_binary(
        &mut reg,
        queue,
        "tree-sitter-node-at",
        |byte, callback: Callback| EditorCommand::TreeSitterNodeAt {
            byte,
            callback: callback.0,
        },
    );
    register_nullary(
        &mut reg,
        queue,
        "undo-boundary",
        EditorCommand::UndoBoundary,
    );
    register_unary(
        &mut reg,
        queue,
        "undo-tree-dump",
        EditorCommand::UndoTreeDump,
    );
    register_binary(&mut reg, queue, "buffer-local-set", |key, value| {
        EditorCommand::BufferLocalSet { key, value }
    });
    {
        // Backing primitive for the `completing-read` Scheme wrapper (which makes
        // `initial` optional). `candidates` may be a static list OR a Scheme fn
        // name (string/symbol) — re-queried per keystroke with the current input,
        // the minibuffer equivalent of a dynamic Helm source.
        let q = queue.clone();
        reg.register_fn(
            "completing-read-impl",
            move |prompt: String, candidates: SteelVal, callback: Callback, initial: String| {
                let callback = callback.0;
                let (candidates, dynamic) = match steel_string(&candidates) {
                    Some(func) => (Vec::new(), Some(func)),
                    None => (steel_string_list(&candidates), None),
                };
                q.lock().unwrap().push(EditorCommand::CompletingRead {
                    prompt,
                    candidates,
                    dynamic,
                    initial,
                    callback,
                });
            },
        );
    }
    register_binary(
        &mut reg,
        queue,
        "read-file-name",
        |prompt, callback: Callback| EditorCommand::ReadFileName {
            prompt,
            callback: callback.0,
        },
    );
    // `(y-or-n-p prompt callback)`; the `-sticky` variant can't be aborted
    // (C-g/Escape) — it stays in the echo area until answered, for prompts that
    // must resolve a pending request (a dropped read can't strand the requester).
    register_y_or_n_p(&mut reg, queue, "y-or-n-p", false);
    register_y_or_n_p(&mut reg, queue, "y-or-n-p-sticky", true);
    {
        // Value of buffer-local `key` on the active buffer, or "".
        let shared = snapshot.clone();
        reg.register_fn("buffer-local-get", move |key: String| -> String {
            let snap = shared.load();
            snap.locals
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        });
    }
    // Backing primitive for the `read-string` Scheme wrapper (which makes the
    // initial value optional). Callback is a Steel fn run with the entered text.
    register_ternary(
        &mut reg,
        queue,
        "read-string-impl",
        |prompt, callback: Callback, initial| EditorCommand::ReadString {
            prompt,
            initial,
            callback: callback.0,
        },
    );
    register_binary(&mut reg, queue, "append-to-buffer", |name, text| {
        EditorCommand::AppendToBuffer { name, text }
    });
    register_unary(
        &mut reg,
        queue,
        "fontify-org-buffer",
        EditorCommand::FontifyOrgBuffer,
    );
    register_unary(
        &mut reg,
        queue,
        "set-modeline-project",
        EditorCommand::SetModelineProject,
    );
    register_unary(
        &mut reg,
        queue,
        "set-modeline-git",
        EditorCommand::SetModelineGit,
    );
    // `(set-mode-line-slot key value)` → write/clear a named mode-line register
    // (empty value clears). The general form behind project/git indicators.
    register_binary(
        &mut reg,
        queue,
        "set-mode-line-slot",
        EditorCommand::SetModeLineSlot,
    );
    {
        // `(set-mode-line-format segs)` — replace the mode-line layout. `segs` is
        // a list of segment tokens (bare strings = literals, or `ml-field`/
        // `ml-slot`/`ml-text` tab-encoded tokens). Defined from scheme/modeline.scm.
        let q = queue.clone();
        reg.register_fn("set-mode-line-format", move |segs: SteelVal| {
            q.lock()
                .unwrap()
                .push(EditorCommand::SetModeLineFormat(steel_string_list(&segs)));
        });
    }
    {
        // `(set-titlebar-dropdown items active callback)` — publish the
        // title-bar dropdown (the perspectives switcher). `items` is a list of
        // display labels, `active` the one shown in the bar; clicking an item
        // calls `(callback label)`. An empty list removes the control. Driven
        // from scheme/perspectives.scm.
        let q = queue.clone();
        reg.register_fn(
            "set-titlebar-dropdown",
            move |items: SteelVal, active: String, callback: Callback| {
                q.lock().unwrap().push(EditorCommand::SetTitlebarDropdown {
                    items: steel_string_list(&items),
                    active,
                    callback: callback.0,
                });
            },
        );
    }
    // --- Overlays / text properties / folds -------------------------------
    register_ternary(&mut reg, queue, "overlay-set-face", |start, end, face| {
        EditorCommand::OverlayFace { start, end, face }
    });
    register_binary(&mut reg, queue, "overlay-hide", |start, end| {
        EditorCommand::OverlayHide { start, end }
    });
    register_binary(&mut reg, queue, "fold-lines", |start_line, end_line| {
        EditorCommand::FoldLines {
            start_line,
            end_line,
        }
    });
    register_nullary(&mut reg, queue, "unfold-all", EditorCommand::UnfoldAll);
    // `(set-fold-marker line open?)` paints a gutter chevron on `line`; the click
    // is hit-tested back to `(fold-toggle-at-line line)`. `(clear-fold-markers)`
    // removes them all (modes re-declare markers on refresh).
    register_binary(&mut reg, queue, "set-fold-marker", |line, open| {
        EditorCommand::SetFoldMarker { line, open }
    });
    register_nullary(
        &mut reg,
        queue,
        "clear-fold-markers",
        EditorCommand::ClearFoldMarkers,
    );
    register_nullary(
        &mut reg,
        queue,
        "clear-overlays",
        EditorCommand::ClearOverlays,
    );
    {
        let q = queue.clone();
        reg.register_fn(
            "put-text-property",
            move |start: usize, end: usize, key: String, value: String| {
                q.lock().unwrap().push(EditorCommand::PutTextProperty {
                    start,
                    end,
                    key,
                    value,
                });
            },
        );
    }
    register_ternary(&mut reg, queue, "set-line-suffix", |line, text, face| {
        EditorCommand::SetLineSuffix { line, text, face }
    });
    {
        // Value of text property `key` at byte `pos`, or "" — read over the
        // published snapshot (Scheme writes via put-text-property, reads next cmd).
        let shared = snapshot.clone();
        reg.register_fn(
            "get-text-property",
            move |pos: usize, key: String| -> String {
                let snap = shared.load();
                snap.props
                    .iter()
                    .find(|(s, e, k, _)| *k == key && pos >= *s && pos < *e)
                    .map(|(_, _, _, v)| v.clone())
                    .unwrap_or_default()
            },
        );
    }
    // --- File-system primitives (pure, run on the engine thread) ----------
    reg.register_fn("file-exists?", |path: String| -> bool {
        std::path::Path::new(&path).exists()
    });
    reg.register_fn("file-directory?", |path: String| -> bool {
        std::path::Path::new(&path).is_dir()
    });
    // `(packages-directory)` → where `package-install` clones git packages:
    // the active config dir's `packages/` subdirectory. Resolved in Rust at call
    // time (the same config-dir logic that finds init.scm), so Scheme needn't
    // thread the path through a global.
    reg.register_fn("packages-directory", || -> String {
        crate::runtime::config_directory()
            .join("packages")
            .to_string_lossy()
            .into_owned()
    });
    // `(register-macros-scheme-dir dir)` → treat `*.scm` under `dir` as editor
    // extension code (the `macros-steel` mode + editor-aware `macros-scheme` LSP),
    // even though it's outside the config dir. `package-install-local` calls this
    // so a local working tree's sources resolve correctly on open.
    reg.register_fn("register-macros-scheme-dir", |dir: String| {
        crate::runtime::register_local_scheme_dir(std::path::PathBuf::from(dir));
    });
    reg.register_fn("parent-directory", |path: String| -> String {
        // Parent of a path, trailing slash trimmed first so a directory's own
        // parent is returned (Emacs `(file-name-directory (directory-file-name …))`).
        let trimmed = path.trim_end_matches('/');
        std::path::Path::new(trimmed)
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    reg.register_fn("read-file", |path: String| -> String {
        std::fs::read_to_string(&path).unwrap_or_default()
    });
    reg.register_fn("write-file", |path: String, text: String| -> bool {
        // Create parent directories first (Emacs's `make-directory ... t` before
        // writing). Without this, the very first write to a fresh config dir like
        // ~/.config/macros/ fails silently and persistence (recent projects,
        // recentf, saveplace) never takes hold.
        if let Some(parent) = std::path::Path::new(&path).parent()
            && !parent.as_os_str().is_empty()
        {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&path, text).is_ok()
    });
    // --- Identity + time (org-roam node IDs, timestamps) ------------------
    // `(org-id-new)` → a fresh v4 UUID string (org-roam node IDs).
    reg.register_fn("org-id-new", || -> String {
        uuid::Uuid::new_v4().to_string()
    });
    // `(current-time fmt)` → local time formatted with a `strftime`-style string
    // (Emacs `format-time-string`), e.g. `(current-time "%Y%m%d%H%M%S")` for a
    // node filename, `(current-time "%Y-%m-%d")` for an agenda date. An invalid
    // format yields "" rather than panicking (write! returns Err, never unwinds).
    reg.register_fn("current-time", |fmt: String| -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        let _ = write!(out, "{}", chrono::Local::now().format(&fmt));
        out
    });
    // --- Fast search (ripgrep as a library, Scheme-configurable) ----------
    // The config lives entirely on this (engine) thread; the register_fns share
    // it. Users tune it in init.scm: (search-exclude "target"),
    // (set-search-option "hidden" "true"), etc.
    {
        let search: Arc<Mutex<SearchConfig>> = Arc::new(Mutex::new(SearchConfig::default()));
        {
            let s = search.clone();
            reg.register_fn("search-exclude", move |glob: String| {
                s.lock().unwrap().excludes.push(glob);
            });
        }
        {
            let s = search.clone();
            reg.register_fn("search-include", move |glob: String| {
                s.lock().unwrap().includes.push(glob);
            });
        }
        {
            let s = search.clone();
            reg.register_fn("search-reset", move || {
                *s.lock().unwrap() = SearchConfig::default();
            });
        }
        {
            let s = search.clone();
            reg.register_fn("set-search-option", move |key: String, value: SteelVal| {
                let v = option_from_steelval(&value);
                let mut c = s.lock().unwrap();
                match key.as_str() {
                    "hidden" => c.hidden = v.truthy(),
                    "follow-symlinks" => c.follow_links = v.truthy(),
                    "respect-gitignore" => c.respect_gitignore = v.truthy(),
                    "max-depth" => c.max_depth = v.as_usize(),
                    _ => {}
                }
            });
        }
        {
            // `(walk-files dir)` → file paths under `dir`, gitignore-aware.
            let s = search.clone();
            reg.register_fn("walk-files", move |dir: String| -> Vec<String> {
                let config = s.lock().unwrap().clone();
                build_walk(&dir, &config)
                    .filter_map(|e| e.ok())
                    .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
                    .map(|e| e.path().to_string_lossy().into_owned())
                    .collect()
            });
        }
        {
            // `(grep-files dir regex)` → `"path:line:text"` matches.
            let s = search.clone();
            reg.register_fn(
                "grep-files",
                move |dir: String, pattern: String| -> Vec<String> {
                    let config = s.lock().unwrap().clone();
                    let Ok(matcher) = grep::regex::RegexMatcher::new(&pattern) else {
                        return Vec::new();
                    };
                    let mut searcher = grep::searcher::SearcherBuilder::new()
                        .line_number(true)
                        .build();
                    let mut results: Vec<String> = Vec::new();
                    for entry in build_walk(&dir, &config).filter_map(|e| e.ok()) {
                        if !entry.file_type().is_some_and(|t| t.is_file()) {
                            continue;
                        }
                        let path = entry.path().to_path_buf();
                        let path_str = path.to_string_lossy().into_owned();
                        let out = &mut results;
                        let _ = searcher.search_path(
                            &matcher,
                            &path,
                            grep::searcher::sinks::UTF8(move |lnum, line| {
                                out.push(format!("{}:{}:{}", path_str, lnum, line.trim_end()));
                                Ok(true)
                            }),
                        );
                    }
                    results
                },
            );
        }
    }
    // gix bridge: pure-Rust git queries (no `git` subprocess), exposed the same
    // blocking-on-the-engine-thread way as `walk-files`/`grep-files`. Errors
    // (not a repo, unborn HEAD, untracked file) collapse to an empty list.
    {
        // `(gix-toplevel dir)` → absolute work-tree root for the repo enclosing
        // `dir`, or "" if `dir` isn't in a git work tree. The native form of
        // `git rev-parse --show-toplevel` behind magit's `magit-toplevel`.
        reg.register_fn("gix-toplevel", |dir: String| -> String {
            ::gix::discover(&dir)
                .ok()
                .and_then(|repo| repo.workdir().map(|p| p.to_string_lossy().into_owned()))
                .unwrap_or_default()
        });
        // `(gix-branch dir)` → short name of the checked-out branch ("main"), or
        // "" outside a repo / with a detached HEAD. `git rev-parse --abbrev-ref HEAD`.
        reg.register_fn("gix-branch", |dir: String| -> String { gix_branch(&dir) });
        // `(gix-log dir n)` → up to `n` recent HEAD commits, newest first, each
        // "shorthash\tauthor\tYYYY-MM-DD\tsummary".
        reg.register_fn("gix-log", |dir: String, max: usize| -> Vec<String> {
            gix_log(&dir, max).unwrap_or_default()
        });
        // `(gix-blame dir file)` → one "line\tsha\tauthor\tepoch\tsummary" per
        // line of `file` (absolute or relative to `dir`) as committed at HEAD —
        // or an empty list when the file differs from HEAD (caller falls back to
        // `git blame`; see gix_blame's alignment guard).
        reg.register_fn("gix-blame", |dir: String, file: String| -> Vec<String> {
            gix_blame(&dir, &file).unwrap_or_default()
        });
        // `(gix-branches dir)` / `(gix-remote-branches dir)` / `(gix-tags dir)`
        // → sorted short ref names, the native form of `git branch [-r]` / `git tag`.
        reg.register_fn("gix-branches", |dir: String| -> Vec<String> {
            gix_ref_names(&dir, RefKind::Local).unwrap_or_default()
        });
        reg.register_fn("gix-remote-branches", |dir: String| -> Vec<String> {
            gix_ref_names(&dir, RefKind::Remote).unwrap_or_default()
        });
        reg.register_fn("gix-tags", |dir: String| -> Vec<String> {
            gix_ref_names(&dir, RefKind::Tag).unwrap_or_default()
        });
        // `(gix-remotes dir)` → configured remote names.
        reg.register_fn("gix-remotes", |dir: String| -> Vec<String> {
            gix_remotes(&dir).unwrap_or_default()
        });
        // `(gix-state dir)` → "merge" / "rebase" / "" (in-progress operation).
        reg.register_fn("gix-state", |dir: String| -> String { gix_state(&dir) });
        // `(gix-head-subject dir)` / `(gix-head-message dir)` → HEAD commit %s / %B.
        reg.register_fn("gix-head-subject", |dir: String| -> String {
            gix_head_message(&dir, false)
        });
        reg.register_fn("gix-head-message", |dir: String| -> String {
            gix_head_message(&dir, true)
        });
        // `(gix-log-oneline dir n)` → up to n "<shorthash> <summary>" lines.
        reg.register_fn(
            "gix-log-oneline",
            |dir: String, max: usize| -> Vec<String> {
                gix_log_oneline(&dir, max).unwrap_or_default()
            },
        );
        // `(gix-diff dir)` / `(gix-diff-cached dir)` → `git diff` / `git diff
        // --cached`-format text (unstaged: index↔worktree; staged: HEAD↔index).
        reg.register_fn("gix-diff", |dir: String| -> String {
            gix_diff_text(&dir, false).unwrap_or_default()
        });
        reg.register_fn("gix-diff-cached", |dir: String| -> String {
            gix_diff_text(&dir, true).unwrap_or_default()
        });
        // `(gix-show dir rev)` → `git show <rev>`-format commit view (header + diff).
        reg.register_fn("gix-show", |dir: String, rev: String| -> String {
            gix_show(&dir, &rev).unwrap_or_default()
        });
        // `(gix-log-decorated dir all? n)` → `git log --oneline --decorate [--all]`.
        reg.register_fn(
            "gix-log-decorated",
            |dir: String, all: bool, max: usize| -> Vec<String> {
                gix_log_decorated(&dir, all, max).unwrap_or_default()
            },
        );
        // `(gix-log-range dir tip hidden n)` → `git log --oneline hidden..tip`.
        reg.register_fn(
            "gix-log-range",
            |dir: String, tip: String, hidden: String, max: usize| -> Vec<String> {
                gix_log_range(&dir, &tip, &hidden, max).unwrap_or_default()
            },
        );
        // `(gix-stash-list dir)` → `git stash list`-format lines (refs/stash reflog).
        reg.register_fn("gix-stash-list", |dir: String| -> Vec<String> {
            gix_stash_list(&dir).unwrap_or_default()
        });
        // `(gix-log-file dir file n)` → `git log --oneline --decorate -n N -- file`.
        reg.register_fn(
            "gix-log-file",
            |dir: String, file: String, max: usize| -> Vec<String> {
                gix_log_file(&dir, &file, max).unwrap_or_default()
            },
        );
        // `(gix-status dir)` → `git status --porcelain --branch --untracked-files=all`
        // lines (header first). Renames read as delete+add (`--no-renames`).
        reg.register_fn("gix-status", |dir: String| -> Vec<String> {
            gix_status(&dir).unwrap_or_default()
        });
    }
    // `(call-process program args)` — run a subprocess, return stdout. Blocks on
    // the engine thread (never the UI), the same model as `list-directory`.
    reg.register_fn(
        "call-process",
        move |program: String, args: SteelVal| -> String {
            let args = steel_string_list(&args);
            std::process::Command::new(&program)
                .args(&args)
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                .unwrap_or_default()
        },
    );
    // `(call-process-full program args input)` → `(exit-code stdout stderr)`,
    // writing `input` to stdin (empty = none). Used for `git apply -` (hunk
    // staging) and to surface git errors (stderr + non-zero code).
    reg.register_fn(
        "call-process-full",
        move |program: String, args: SteelVal, input: String| -> Vec<String> {
            use std::io::Write;
            use std::process::Stdio;
            let args = steel_string_list(&args);
            let spawn = std::process::Command::new(&program)
                .args(&args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn();
            let mut child = match spawn {
                Ok(c) => c,
                Err(e) => return vec!["-1".into(), String::new(), e.to_string()],
            };
            // Write stdin then close it (drop) so the child sees EOF — otherwise
            // a filter like `git apply -` blocks waiting for input.
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(input.as_bytes());
            }
            match child.wait_with_output() {
                Ok(o) => vec![
                    o.status
                        .code()
                        .map_or_else(|| "-1".into(), |c| c.to_string()),
                    String::from_utf8_lossy(&o.stdout).into_owned(),
                    String::from_utf8_lossy(&o.stderr).into_owned(),
                ],
                Err(e) => vec!["-1".into(), String::new(), e.to_string()],
            }
        },
    );
    // `(getenv name)` → the variable's value, or "" if unset. Reads the same
    // process environment that all subprocess spawns inherit (see
    // `import_shell_env`), so config can inspect the resolved PATH.
    reg.register_fn("getenv", |name: String| -> String {
        std::env::var(&name).unwrap_or_default()
    });
    // `(setenv name value)` → set a process env var, visible to every
    // subsequently spawned subprocess (LSP servers, call-process, make-process).
    // Lets a config extend PATH or pin a toolchain, e.g.
    //   (setenv "PATH" (string-append (getenv "PATH") ":" ...))
    // Runs on the single engine thread during `init.scm` eval, before any
    // process is spawned from Scheme, so the mutation is sound.
    reg.register_fn("setenv", |name: String, value: String| {
        // SAFETY: the engine is single-threaded and config eval happens at
        // startup before subprocess threads read the environment.
        unsafe { std::env::set_var(&name, &value) };
    });
    // --- Async (subprocess streaming, timers) -----------------------------
    // Dedicated id source for processes/terminals. Each lives in its own
    // editor-side map keyed by id, so it needn't share the marker counter.
    let async_ids = IdGen::default();
    {
        // `(make-process program args on-output on-exit)` → a process id (used by
        // process-send-string / kill-process). on-exit "" = none; it receives the
        // exit code as a string.
        let q = queue.clone();
        let ids = async_ids.clone();
        reg.register_fn(
            "make-process",
            move |program: String,
                  args: SteelVal,
                  on_output: Callback,
                  on_exit: Callback|
                  -> usize {
                let id = ids.next();
                let args = steel_string_list(&args);
                let on_output = on_output.0;
                let on_exit = Some(on_exit.0).filter(|s| !s.is_empty());
                q.lock().unwrap().push(EditorCommand::MakeProcess {
                    id,
                    program,
                    args,
                    on_output,
                    on_exit,
                });
                id as usize
            },
        );
    }
    register_binary(&mut reg, queue, "process-send-string", |id, text| {
        EditorCommand::SendProcess { id, text }
    });
    register_unary(&mut reg, queue, "kill-process", |id| {
        EditorCommand::KillProcess { id }
    });
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
                  headers: SteelVal,
                  body: String,
                  on_line: Callback,
                  on_done: Callback|
                  -> usize {
                let id = ids.next();
                let flat = steel_string_list(&headers);
                let headers = flat
                    .chunks_exact(2)
                    .map(|kv| (kv[0].clone(), kv[1].clone()))
                    .collect();
                let Callback(on_line) = on_line;
                let Callback(on_done) = on_done;
                q.lock()
                    .unwrap()
                    .push(EditorCommand::HttpStream(Box::new(HttpStreamArgs {
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
    // --- ANVIL REPL (a second, independent Steel interpreter) -------------
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
    {
        // `(make-terminal name program args)` → a terminal id. Empty `program`
        // runs the user's login shell. The terminal is shown in a `term-mode`
        // buffer named `name`; switching to that buffer later reuses it.
        let q = queue.clone();
        let ids = async_ids.clone();
        reg.register_fn(
            "make-terminal",
            move |name: String, program: String, args: SteelVal| -> usize {
                let id = ids.next();
                q.lock().unwrap().push(EditorCommand::MakeTerminal {
                    id,
                    name,
                    program,
                    args: steel_string_list(&args),
                });
                id as usize
            },
        );
    }
    register_binary(&mut reg, queue, "run-with-timer", |ms, callback| {
        EditorCommand::RunWithTimer { ms, callback }
    });
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
    // `(evil-enabled?)` → #t when modal (evil) editing is active.
    register_snapshot_read(&mut reg, snapshot, "evil-enabled?", |s| s.evil_enabled);
    // `(helix-enabled?)` → #t when Helix (selection-first) modal editing is on.
    register_snapshot_read(&mut reg, snapshot, "helix-enabled?", |s| s.helix_enabled);
    // `(diagnostics)` → the active buffer's LSP diagnostics, each a record
    // "line\tcol\tseverity\tmessage" (line/col 0-based). The library splits it.
    register_snapshot_read(&mut reg, snapshot, "diagnostics", |s| s.diagnostics.clone());
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
    // Escape a string for literal use in a regexp (Emacs `regexp-quote`).
    reg.register_fn("regexp-quote", |s: String| -> String { regex::escape(&s) });
    // `(fuzzy-match pattern candidates)` — rank `candidates` against `pattern`
    // with the same nucleo matcher Helm uses, best-match first; non-matches are
    // dropped. An empty pattern returns the list unchanged. Lets a Scheme-driven
    // Helm filter do compiled-speed scoring instead of reimplementing it.
    reg.register_fn(
        "fuzzy-match",
        |pattern: String, candidates: SteelVal| -> Vec<String> {
            fuzzy_rank(&pattern, &steel_string_list(&candidates))
        },
    );
    {
        // Bytes `[a, b)` of the buffer, clamped (Emacs `(buffer-substring a b)`).
        let shared = snapshot.clone();
        reg.register_fn("buffer-substring", move |a: usize, b: usize| -> String {
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
    {
        // `(make-marker pos)` → a marker id (allocated synchronously).
        let m = markers.clone();
        reg.register_fn("make-marker", move |pos: usize| -> usize {
            m.lock().unwrap().alloc(pos) as usize
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
        reg.register_fn("set-marker", move |id: u64, pos: usize| {
            m.lock().unwrap().positions.insert(id, pos);
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
            reg.register_fn($name, move |pos: usize| -> usize {
                let buf = crate::buffer::Buffer::from_rope(shared.load().text.clone());
                let f: fn(&crate::buffer::Buffer, usize) -> usize = $f;
                f(&buf, pos.min(buf.len()))
            });
        }};
    }
    register_pos_fn!("grapheme-forward", |b, p| b.next_grapheme(p));
    register_pos_fn!("grapheme-backward", |b, p| b.prev_grapheme(p));
    register_pos_fn!("word-forward", |b, p| b.forward_word(p));
    register_pos_fn!("word-forward-start", |b, p| b.forward_word_start(p));
    register_pos_fn!("word-backward", |b, p| b.backward_word(p));
    register_pos_fn!("line-start-pos", |b, p| b.line_range_at(p).start);
    register_pos_fn!("line-end-pos", |b, p| b.line_range_at(p).end);
    register_pos_fn!("line-of-pos", |b, p| b.line_index_at_byte(p));
    {
        // `(line-start-of-line n)` → first byte of (0-based) line `n`, clamped.
        let shared = snapshot.clone();
        reg.register_fn("line-start-of-line", move |line: usize| -> usize {
            let buf = crate::buffer::Buffer::from_rope(shared.load().text.clone());
            let last = buf.line_count().saturating_sub(1);
            buf.line_range(line.min(last)).start
        });
    }
    // --- Regexp (over strings, and over the buffer from point) ------------
    reg.register_fn("string-match", move |pattern: String, s: String| -> isize {
        Regex::new(&pattern)
            .ok()
            .and_then(|re| re.find(&s).map(|m| m.start() as isize))
            .unwrap_or(-1)
    });
    reg.register_fn(
        "regexp-captures",
        move |pattern: String, s: String| -> Vec<String> {
            // Group 0 = whole match, then each capture group (absent group = "").
            match Regex::new(&pattern).ok().and_then(|re| re.captures(&s)) {
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
            match Regex::new(&pattern) {
                Ok(re) => re.replace_all(&s, replacement.as_str()).into_owned(),
                Err(_) => s,
            }
        },
    );
    {
        // `(re-search-forward pattern)` → `(start end)` of the next match at/after
        // point, or `()` if none. Scheme drives point/edits from the result.
        let shared = snapshot.clone();
        reg.register_fn("re-search-forward", move |pattern: String| -> Vec<usize> {
            let Ok(re) = Regex::new(&pattern) else {
                return Vec::new();
            };
            let (rope, p) = {
                let snap = shared.load();
                (snap.text.clone(), snap.point)
            };
            let from = snap_byte(&rope, p);
            let tail = rope.byte_slice(from..).to_string();
            match re.find(&tail) {
                Some(m) => vec![from + m.start(), from + m.end()],
                None => Vec::new(),
            }
        });
    }
    (engine, names)
}

/// Registers Steel functions into the engine while recording every registered
/// name. The global-name list (consumed by the built-in Steel LSP and the
/// minibuffer symbol completion via [`ScriptEngine::global_names`]) is thus
/// derived from registration itself and can never drift from it.
pub(crate) struct Registrar<'a> {
    engine: &'a mut Engine,
    names: &'a mut Vec<String>,
}

impl Registrar<'_> {
    fn register_fn<FN, ARGS, RET>(&mut self, name: &'static str, func: FN)
    where
        Engine: RegisterFn<FN, ARGS, RET>,
    {
        self.names.push(name.to_string());
        self.engine.register_fn(name, func);
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
    reg.register_fn(name, move || q.lock().unwrap().push(cmd.clone()));
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
    reg.register_fn(name, move |arg: T| q.lock().unwrap().push(make(arg)));
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
    reg.register_fn(name, move |a: T, b: U| q.lock().unwrap().push(make(a, b)));
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
    reg.register_fn(name, move |a: T, b: U, c: V| {
        q.lock().unwrap().push(make(a, b, c))
    });
}

/// Register one of the `y-or-n-p` / `y-or-n-p-sticky` pair: a yes/no prompt
/// calling `callback` with "yes"/"no"; `sticky` prompts can't be aborted.
pub(crate) fn register_y_or_n_p(
    reg: &mut Registrar,
    queue: &Queue,
    name: &'static str,
    sticky: bool,
) {
    let q = queue.clone();
    reg.register_fn(name, move |prompt: String, callback: Callback| {
        q.lock().unwrap().push(EditorCommand::YesOrNo {
            prompt,
            callback: callback.0,
            sticky,
        });
    });
}

/// Register one of the `render-buffer` / `render-buffer-quiet` pair — same
/// signature, differing only in whether the rendered buffer is switched to.
pub(crate) fn register_render_buffer(
    reg: &mut Registrar,
    queue: &Queue,
    fn_name: &'static str,
    quiet: bool,
) {
    let q = queue.clone();
    reg.register_fn(
        fn_name,
        move |name: String, mode: String, text: String, faces: SteelVal| {
            let line_faces = parse_line_faces(&faces);
            let cmd = if quiet {
                EditorCommand::RenderBufferQuiet {
                    name,
                    mode,
                    text,
                    line_faces,
                }
            } else {
                EditorCommand::RenderBuffer {
                    name,
                    mode,
                    text,
                    line_faces,
                }
            };
            q.lock().unwrap().push(cmd);
        },
    );
}

/// Register one of the `transient` / `transient-sticky` pair, differing only
/// in whether selecting a row keeps the popup open.
pub(crate) fn register_transient(
    reg: &mut Registrar,
    queue: &Queue,
    name: &'static str,
    sticky: bool,
) {
    let q = queue.clone();
    reg.register_fn(name, move |title: String, rows: SteelVal| {
        q.lock().unwrap().push(EditorCommand::Transient {
            title,
            columns: parse_transient_columns(&rows),
            sticky,
        });
    });
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

/// Snap a byte index down to the nearest UTF-8 char boundary in `rope`, clamped
/// to its length. Slicing a `Rope` panics on a non-boundary index, so the
/// text-slicing bridges route caller-supplied offsets through this first — the
/// boundary safety the old `String`-slicing bridges did by hand.
pub(crate) fn snap_byte(rope: &Rope, byte: usize) -> usize {
    let byte = byte.min(rope.len_bytes());
    rope.char_to_byte(rope.byte_to_char(byte))
}
