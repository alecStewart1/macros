use std::sync::{Arc, LazyLock, OnceLock};

use regex::Regex;
use ropey::Rope;
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
        queue, snapshot, ..
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
    prims::dap::register(&mut reg, shared);
    prims::http::register(&mut reg, shared);
    prims::crypto::register(&mut reg, shared);
    prims::sockets::register(&mut reg, shared);
    prims::sqlite::register(&mut reg, shared);
    prims::anvil::register(&mut reg, shared);
    prims::buffer_reads::register(&mut reg, shared);
    prims::markers::register(&mut reg, shared);
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
