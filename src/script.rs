use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, RwLock};

use arc_swap::ArcSwap;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};
use regex::Regex;
use ropey::Rope;
use steel::rvals::{IntoSteelVal, SteelVal};
use steel::steel_vm::engine::Engine;
use steel::steel_vm::register_fn::RegisterFn;

/// A command requested by a script, applied to the editor by Rust.
///
/// Steel runs without direct access to GPUI's `Context`, so scripts never mutate
/// the editor in place. Instead, registered functions enqueue `EditorCommand`s
/// that the editor drains and applies after evaluation — the same decoupling
/// Emacs gets from its command loop. Most commands are nullary; a few carry a
/// string payload.
#[derive(Debug, Clone)]
pub enum EditorCommand {
    // Motion
    ForwardChar,
    BackwardChar,
    NextLine,
    PreviousLine,
    ForwardWord,
    ForwardWordStart,
    BackwardWord,
    BeginningOfLine,
    EndOfLine,
    BeginningOfBuffer,
    EndOfBuffer,
    // Region / mark
    SetMark,
    ExchangePointAndMark,
    MarkWholeBuffer,
    ExtendBackwardChar,
    ExtendForwardChar,
    // Insertion
    Insert(String),
    Newline,
    OpenLine,
    IndentForTab,
    // Deletion / killing
    DeleteChar,
    DeleteBackwardChar,
    TransposeChars,
    KillLine,
    KillWord,
    BackwardKillWord,
    KillRegion,
    KillRingSave,
    Yank,
    YankPop,
    /// System-clipboard copy / cut / paste (macOS cmd-c/x/v), kept in sync with
    /// the kill ring.
    ClipboardCopy,
    ClipboardCut,
    ClipboardPaste,
    /// Put an arbitrary string on the system clipboard (Emacs `kill-new`).
    ClipboardSet(String),
    // Undo
    Undo,
    /// Redo the most recently created branch off the current undo-tree node.
    Redo,
    /// Dump the undo tree as text to Steel `callback` (one node per line) — the
    /// vundo visualizer reads this to draw the branch structure.
    UndoTreeDump(String),
    // View
    ScrollUp,
    ScrollDown,
    Recenter,
    // Misc
    KeyboardQuit,
    ExecuteExtendedCommand,
    /// Show the echo-area scrollback (`*Messages*`).
    ListMessages,
    /// Show captured language-server stderr (`*lsp-log*`).
    ListLspLog,
    /// Force an update check against macros.dev and install a newer release if
    /// the user confirms (Emacs-style, echo-area driven). See `src/update.rs`.
    CheckForUpdates,
    // Steel interaction (steel-mode)
    EvalBuffer,
    EvalLastSexp,
    /// Read a Steel expression in the minibuffer and evaluate it (Emacs `M-:`).
    EvalExpression,
    // Buffers / files
    FindFile,
    /// Open a file by path (a Helm action / programmatic open).
    OpenFile(String),
    SwitchBuffer,
    /// Switch to an open buffer by name (a Helm action / programmatic switch).
    SwitchToBufferNamed(String),
    /// Kill a buffer by name (active or inactive).
    KillBufferNamed(String),
    /// Set the active (editable) buffer's major mode by name, so its keymap
    /// applies (e.g. shell-mode rebinding RET). Does not make the buffer special.
    SetMajorMode(String),
    NextBuffer,
    PreviousBuffer,
    SaveBuffer,
    /// Save every modified file-visiting buffer (Emacs `save-some-buffers`).
    SaveSomeBuffers,
    RevertBuffer,
    ToggleLineNumbers,
    ToggleScrollBar,
    ToggleWordWrap,
    /// Toggle electric-pair-mode: auto-insert the closing delimiter on a typed
    /// opener (and wrap an active region). Off by default.
    ToggleElectricPair,
    SelectWindow(usize),
    OpenFileOtherWindow(String),
    SetLineIcon(usize, usize, String),
    ClearLineIcons,
    /// Display a raster image (decoded from `path`) on rows reserved below buffer
    /// `line` — the Org inline-image primitive.
    SetLineImage {
        line: usize,
        path: String,
        max_width: usize,
    },
    ClearLineImages,
    /// Virtual left indentation (in columns) for buffer `line` — display-only, the
    /// Org `org-indent-mode` primitive. The buffer text is untouched.
    SetLineIndent {
        line: usize,
        cols: usize,
    },
    ClearLineIndents,
    /// Whole-line font-size multiplier for buffer `line` (1.0 = unscaled) —
    /// markdown/org headings render larger. Display-only.
    SetLineScale {
        line: usize,
        scale: f32,
    },
    ClearLineScales,
    KillBuffer,
    /// Kill the active buffer and close its window (Emacs `kill-buffer-and-window`).
    KillBufferAndWindow,
    Quit,
    // LSP navigation / editing
    LspDefinition,
    LspDeclaration,
    LspTypeDefinition,
    LspImplementation,
    LspReferences,
    LspHover,
    LspSignatureHelp,
    LspDocumentSymbols,
    LspCodeAction,
    LspFormatBuffer,
    LspCallHierarchyIncoming,
    LspCallHierarchyOutgoing,
    /// Validate + prompt rename (`prepareRename` then `rename`).
    LspPrepareRename,
    /// Request + render code lenses (resolving those missing a command).
    LspCodeLens,
    /// RET in an `*lsp-xref*` / `*lsp-symbols*` list: visit the row's target.
    LspXrefVisit,
    /// RET in `*lsp-actions*`: apply the row's code action.
    LspApplyCodeAction,
    /// Rename the symbol at point to the given name (read via `read-string`).
    LspRename(String),
    /// `workspace/symbol` for the given query (read via `read-string`).
    LspWorkspaceSymbol(String),
    /// Generic LSP bridge: send `method` with params built from `kind`
    /// ("position"|"document"|"range"|"region"|"positions"|"raw") merged with
    /// `params` (a JSON object string), delivering the result JSON to Steel fn
    /// `callback`. This exposes the whole protocol to Scheme.
    LspRequest {
        method: String,
        kind: String,
        params: String,
        callback: String,
    },
    /// Jump to `uri` at `(line, character)` (a Scheme handler visiting a result).
    LspGoto {
        uri: String,
        line: usize,
        character: usize,
    },
    /// Apply a `WorkspaceEdit` (JSON) across files (rename / code action).
    LspApplyWorkspaceEdit(String),
    /// Apply `TextEdit[]` (JSON) to the active buffer (formatting).
    LspApplyTextEdits(String),
    /// Push the current location onto the jump-back ring.
    XrefPushMarker,
    /// Pop the jump-back ring and return there (Emacs `pop-tag-mark`).
    XrefPopMarker,
    /// Enable/disable LSP format-on-save for a language.
    SetLspFormatOnSave(String, bool),
    /// Request `textDocument/semanticTokens/full`, decode it, and deliver the
    /// tokens (one `"line\tcol\tlength\ttype"` record per line) to Steel `callback`.
    LspSemanticTokens(String),
    /// Like `LspSemanticTokens` but only the viewport window (`/range`).
    LspSemanticTokensRange(String),
    /// Like `LspSemanticTokens` but via `/full/delta` when possible (live).
    LspSemanticTokensDelta(String),
    // Tree-sitter (the live parse tree, queried on the UI thread).
    /// Run tree-sitter query `query` over the active buffer's parse tree and
    /// deliver the captures to Steel fn `callback` as one
    /// `"capture\tstart\tend\tkind"` record per line (byte offsets). Empty when
    /// there's no grammar/tree or the query won't compile. Gives Scheme the whole
    /// parse tree — find brackets, strings, comments, functions, etc.
    TreeSitterQuery {
        query: String,
        callback: String,
    },
    /// Deliver the node chain covering `byte` (innermost first) to Steel fn
    /// `callback`, one `"kind\tstart\tend"` record per line — "what am I inside?".
    TreeSitterNodeAt {
        byte: usize,
        callback: String,
    },
    // LSP / completion popup
    CompletionAtPoint,
    CompletionNext,
    CompletionPrevious,
    CompletionAccept,
    CompletionAbort,
    // Helm (selection framework)
    /// Open a Helm session over Scheme-provided sources.
    Helm(Vec<HelmSourceSpec>),
    /// Like `Helm`, but with an initial input pre-filled (e.g. a directory for
    /// the file navigator, so top-level fuzzy + TAB-descend work).
    HelmFrom(String, Vec<HelmSourceSpec>),
    /// Built-in: Helm over all command names, running the chosen command.
    HelmMx,
    HelmNext,
    HelmPrevious,
    HelmAccept,
    HelmAbort,
    /// Open the action menu for the selected candidate (multi-action sources).
    HelmActions,
    /// Toggle the selected candidate's multi-select mark (`C-SPC`).
    HelmToggleMark,
    // Evil (modal editing)
    EvilMode,
    EvilNormalState,
    EvilInsertState,
    EvilVisualState,
    /// Vim line-wise Visual (`V`): the selection always covers whole lines.
    EvilVisualLineState,
    /// Block insert/append (`I`/`A` in Visual line): drop a cursor at the start
    /// (or end) of every selected line and switch to Insert, so one edit
    /// replicates to all of them.
    EvilVisualInsert,
    EvilVisualAppend,
    EvilDisable,
    /// Scroll half a page down / up (evil `C-d` / `C-u`).
    EvilScrollDown,
    EvilScrollUp,
    EvilSearchForward,
    EvilSearchBackward,
    EvilSearchNext,
    EvilSearchPrevious,
    EvilExCommand,
    /// Vim paste after (`p`) / before (`P`) the cursor, honoring the linewise
    /// flag set by the most recent yank/delete.
    EvilPasteAfter,
    EvilPasteBefore,
    /// Mark the most recent yank/delete as line-wise / character-wise, so the
    /// next `p`/`P` knows whether to open a new line or insert inline.
    EvilSetLinewise,
    EvilSetCharwise,
    /// Emacs incremental search (`C-s` / `C-r`).
    IsearchForward,
    IsearchBackward,
    // Modes / hooks
    /// Start the language server for the current buffer (mode-hook friendly).
    EnableLsp,
    /// Run a named function when a mode is enabled: `(add-hook "steel" "enable-lsp")`.
    AddHook(String, String),
    /// Remove a previously added mode hook.
    RemoveHook(String, String),
    /// Register a Scheme completion-at-point function (tried by `M-x
    /// completion-at-point` before LSP): `(add-completion-at-point-function "f")`.
    AddCapf(String),
    /// Replace the set of directories watched on behalf of `owner` with `dirs`
    /// (`(watch-directories "treemacs" (list ...))`). Owner-namespaced with
    /// replace semantics so a view re-declares its full watch set each render and
    /// never has to unwatch individual paths; pass `'()` to drop them all (e.g. on
    /// quit). A change under any watched dir fires `directory-changed-hook`.
    WatchDirectories(String, Vec<String>),
    /// Configure a language server: `(set-lsp "rust" "rust-analyzer")`.
    SetLsp(String, String),
    /// Map a file extension to a server key / config language:
    /// `(set-extension "rs" "rust")`.
    SetExtension(String, String),
    /// Map a file extension to the wire `languageId` sent in `didOpen`:
    /// `(set-language-id "tsx" "typescriptreact")`. Lets several extensions
    /// share one server yet announce distinct ids.
    SetLanguageId(String, String),
    /// Set a tunable option: `(set-option "tab-width" 4)`. The value may be a
    /// number, boolean, or string; it is coerced to the option's declared type
    /// when applied (see [`crate::settings::Settings::set`]).
    SetOption(String, crate::settings::OptionValue),
    Message(String),
    /// Bind a key sequence to a command in a named mode's keymap:
    /// `(define-key "steel" "ctrl-c ctrl-e" "eval-last-sexp")`.
    DefineKey(String, String, String),
    // Theming
    /// Set a face's color: `(set-face "comment" "#6a9955")`.
    SetFace(String, String),
    /// Load a built-in theme by name: `(load-theme "macros-light")`.
    LoadTheme(String),
    // Special (read-only, program-generated) buffers — the magit pattern.
    /// Render `text` into a read-only buffer `name` with major mode `mode` and
    /// per-line faces `(line, face)`, switching to it (creating it if needed,
    /// refreshing in place — point preserved — if already shown).
    RenderBuffer {
        name: String,
        mode: String,
        text: String,
        line_faces: Vec<(usize, String)>,
    },
    /// Like [`RenderBuffer`], but refresh the buffer wherever it already lives
    /// without switching to it or moving focus — the auto-refresh path for the
    /// treemacs/dired sidebar when a watched directory changes on disk.
    RenderBufferQuiet {
        name: String,
        mode: String,
        text: String,
        line_faces: Vec<(usize, String)>,
    },
    /// Move point to the start of (0-based) line `n` in the active buffer.
    GotoLine(usize),
    /// Move point to byte offset `n` (Emacs `goto-char`).
    GotoChar(usize),
    /// Read a line in the minibuffer, then call Steel fn `callback` with it
    /// (Emacs `read-string`, callback-style to fit the async engine). `initial`
    /// pre-fills the input (empty for none).
    ReadString {
        prompt: String,
        initial: String,
        callback: String,
    },
    /// Append `text` to the end of buffer `name`, creating it if needed and
    /// switching to it (Emacs `append-to-buffer`; an output-buffer primitive).
    AppendToBuffer {
        name: String,
        text: String,
    },
    /// Diff two buffers by name (the `similar` crate) and call Steel fn
    /// `callback` with one "tag\tas\tae\tbs\tbe\tabs\tabe\tbbs\tbbe" record per
    /// non-equal hunk — line ranges + byte ranges for each side. Powers the
    /// interactive ediff two-pane merge (cross-buffer, so it can't be done from
    /// the active-buffer snapshot in Scheme).
    DiffBuffers {
        a: String,
        b: String,
        callback: String,
    },
    /// Replace lines [dst_start,dst_end) of buffer `dst` with lines
    /// [src_start,src_end) of buffer `src` (ediff "copy this hunk A->B").
    BufferCopyLines {
        src: String,
        src_start: usize,
        src_end: usize,
        dst: String,
        dst_start: usize,
        dst_end: usize,
    },
    /// Paint a face over lines [start,end) of buffer `name`, whether or not it's
    /// the active buffer (ediff hunk highlight in the non-selected pane).
    BufferHighlightLines {
        name: String,
        start: usize,
        end: usize,
        face: String,
    },
    /// Clear all face overlays from buffer `name` (active or inactive).
    BufferClearHighlights {
        name: String,
    },
    /// Expand a yasnippet-style template at point: insert it, drop tab-stop field
    /// markers ($1/$2/${n:default}/$0), and jump to the first field.
    SnippetExpand(String),
    /// Jump to the next / previous snippet field in the active session.
    SnippetNextField,
    SnippetPrevField,
    /// Show avy jump hints: one "byte\tlabel" record per line; the labels paint
    /// over the buffer at those byte offsets until cleared.
    AvyShowHints(String),
    /// Clear all avy jump hints.
    AvyClearHints,
    /// multiple-cursors: add a secondary cursor below / above the primary, at the
    /// next occurrence of the word at point, or clear all secondary cursors.
    McAddBelow,
    McAddAbove,
    McMarkNext,
    McClear,
    /// Set the mode-line project indicator (`None`/empty clears it).
    SetModelineProject(String),
    // Transient popup (the magit / `transient.el` pattern).
    /// Open a transient popup: a title plus `(key, label, command)` rows.
    Transient {
        title: String,
        columns: Vec<TransientColumn>,
    },
    /// Close any open transient popup.
    TransientQuit,
    // Windows (split panes).
    SplitWindowBelow,
    SplitWindowRight,
    /// Split the selected window side by side, the existing pane keeping this
    /// fraction of the width — a narrow sidebar (e.g. 0.2 for treemacs).
    SplitWindowRightRatio(f64),
    OtherWindow,
    DeleteWindow,
    DeleteOtherWindows,
    EnlargeWindow,
    ShrinkWindow,
    EnlargeWindowHorizontally,
    ShrinkWindowHorizontally,
    // Keyboard macros.
    StartKbdMacro,
    EndKbdMacro,
    CallLastKbdMacro,
    // Overlays / text properties / folds.
    /// Layer `face` over the byte range — sub-line highlight (Emacs overlay).
    OverlayFace {
        start: usize,
        end: usize,
        face: String,
    },
    /// Hide a byte range from display without changing buffer text.
    OverlayHide {
        start: usize,
        end: usize,
    },
    /// Hide buffer lines `[start_line, end_line)` (folding / invisibility).
    FoldLines {
        start_line: usize,
        end_line: usize,
    },
    /// Unfold everything.
    UnfoldAll,
    /// Attach a fold chevron to buffer `line`'s gutter. `open` picks the glyph
    /// (down = expanded, right = collapsed); the click hit-test calls back into
    /// Scheme's `fold-toggle-at-line`. Policy (which lines, what range) is Scheme.
    SetFoldMarker {
        line: usize,
        open: bool,
    },
    /// Remove every fold chevron from the active buffer's gutter.
    ClearFoldMarkers,
    /// Remove all overlays/folds/properties from the active buffer.
    ClearOverlays,
    /// Remove only the sub-line overlay faces (keeps folds/props/suffixes/hides)
    /// — for repainting semantic tokens / document highlights without nuking the
    /// rest, or another feature's concealment.
    ClearOverlayFaces,
    /// Remove only the `overlay-hide` concealment ranges (keeps faces/folds) —
    /// for refreshing markdown/org heading-marker hiding.
    ClearOverlayHides,
    /// Remove only the line suffixes (virtual text) — for refreshing inlay hints.
    ClearLineSuffixes,
    /// Attach a text property `(key, value)` to a byte range.
    PutTextProperty {
        start: usize,
        end: usize,
        key: String,
        value: String,
    },
    /// Virtual text appended after `line` (inlay hint / blame / inline note).
    SetLineSuffix {
        line: usize,
        text: String,
        face: String,
    },
    // Async.
    /// Spawn `program` async under `id`; call Steel fn `on_output` per stdout
    /// line, and `on_exit` (if non-empty) with the exit code (Emacs `make-process`).
    MakeProcess {
        id: u64,
        program: String,
        args: Vec<String>,
        on_output: String,
        on_exit: Option<String>,
    },
    /// Write `text` to process `id`'s stdin (Emacs `process-send-string`).
    SendProcess {
        id: u64,
        text: String,
    },
    /// Kill process `id` (Emacs `kill-process`).
    KillProcess {
        id: u64,
    },
    // ANVIL REPL — a second, independent Steel interpreter (see `crate::anvil`).
    /// Evaluate `source` in the ANVIL REPL engine; deliver the printed result to
    /// the Scheme callback `on_result` (SLIME's `slime-eval-async`).
    AnvilEval {
        source: String,
        on_result: String,
    },
    /// Cancel the ANVIL REPL's current evaluation (SLIME's `C-c C-c`).
    AnvilInterrupt,
    /// Rebuild the ANVIL REPL engine — a fresh image (SLIME's restart).
    AnvilReset,
    /// Evaluate the top-level form at point in the ANVIL image (SLIME `C-M-x`).
    /// The editor computes the form's bounds from the buffer and point.
    AnvilEvalDefun,
    /// `(load …)` the active buffer's file into the ANVIL image (SLIME `C-c C-l`).
    AnvilLoadFile,
    /// Show the ANVIL image's docstring for the symbol at point (SLIME `C-c C-d`).
    AnvilDoc,
    /// Macroexpand the region (or the form at point) against the image's macros
    /// (SLIME `C-c RET`).
    AnvilMacroexpand,
    /// List live image bindings whose name contains `pattern` (SLIME `apropos`).
    AnvilApropos(String),
    // Terminal emulator.
    /// Open a terminal running `program` (login shell if empty) in a new
    /// `term-mode` buffer named `name`, backed by terminal `id`.
    MakeTerminal {
        id: u64,
        name: String,
        program: String,
        args: Vec<String>,
    },
    /// Call Steel fn `callback` after `ms` milliseconds (Emacs `run-with-timer`).
    RunWithTimer {
        ms: u64,
        callback: String,
    },
    // Programmatic editing (arbitrary ranges in the active buffer).
    /// Delete bytes `[start, end)`.
    DeleteRegion {
        start: usize,
        end: usize,
    },
    /// Kill bytes `[start, end)` into the kill ring (like `kill-region` but on an
    /// explicit range rather than the active region).
    KillRegionBetween {
        start: usize,
        end: usize,
    },
    /// Replace bytes `[start, end)` with `text`.
    ReplaceRegion {
        start: usize,
        end: usize,
        text: String,
    },
    /// Insert `text` at byte `pos`.
    InsertAt {
        pos: usize,
        text: String,
    },
    /// Select the byte range `[start, end)` (activates the mark). Lets Scheme
    /// set an arbitrary region, e.g. expand-region / LSP selection ranges.
    SetRegion {
        start: usize,
        end: usize,
    },
    /// Close the current undo group so the next edit starts a new one.
    UndoBoundary,
    /// Set a buffer-local variable on the active buffer.
    BufferLocalSet {
        key: String,
        value: String,
    },
    // Minibuffer reads that call back into Scheme.
    /// `completing-read`: prompt with completion, then call `callback`.
    /// `candidates` is the static list; `dynamic` is a Scheme fn name re-queried
    /// per keystroke when the caller passed a function. `initial` pre-fills input.
    CompletingRead {
        prompt: String,
        candidates: Vec<String>,
        dynamic: Option<String>,
        initial: String,
        callback: String,
    },
    /// `read-file-name`: prompt for a file path, then call `callback`.
    ReadFileName {
        prompt: String,
        callback: String,
    },
    /// `y-or-n-p`: yes/no prompt, then call `callback` with "yes"/"no".
    YesOrNo {
        prompt: String,
        callback: String,
    },
}

/// One row of a transient popup: a key, its description, and the Steel command
/// to run when it's pressed.
#[derive(Debug, Clone)]
pub struct TransientItem {
    pub key: String,
    pub label: String,
    pub command: String,
}

/// A column of transient rows, optionally headed by a title. A flat
/// `(transient title rows)` is one untitled column; the grouped form gives
/// several titled columns laid out side by side (magit-style).
#[derive(Debug, Clone)]
pub struct TransientColumn {
    pub title: Option<String>,
    pub rows: Vec<TransientItem>,
}

/// A Helm source's candidates: a fixed list (each candidate carrying an optional
/// marginalia-style annotation), or the name of a Steel function re-queried with
/// the current pattern as it changes (dynamic sources).
#[derive(Debug, Clone)]
pub enum HelmCandidates {
    Static(Vec<(String, Option<String>)>),
    /// A Scheme fn re-queried per keystroke; its result is then filtered/ranked
    /// by Rust against the pattern (the fn supplies the pool).
    Dynamic(String),
    /// Like [`HelmCandidates::Dynamic`], but the fn's result is shown verbatim —
    /// Rust does no further filtering. The fn is the sole filter authority, so a
    /// Helm picker can be driven entirely from Scheme (e.g. via `fuzzy-match`).
    DynamicRaw(String),
}

/// A Helm source: a named group of candidates plus one or more actions. Each
/// action is `(label, function-name)`; the first is the default (RET), the rest
/// reachable via the TAB action menu. The chosen candidate string is passed to
/// the action function.
#[derive(Debug, Clone)]
pub struct HelmSourceSpec {
    pub name: String,
    pub candidates: HelmCandidates,
    pub actions: Vec<(String, String)>,
    /// Optional Scheme fn name; given the selected candidate it returns the
    /// preview as a list of lines, shown in a side pane. `None` = no preview.
    pub preview: Option<String>,
}

type Queue = Arc<Mutex<Vec<EditorCommand>>>;
type BufferNames = Arc<Mutex<Vec<String>>>;
/// A snapshot of the active buffer, published by the editor before it dispatches
/// a command so the read bridges (`buffer-string`, `point`, `region-text`, …)
/// see the buffer as of the moment the command was invoked — Emacs's command
/// semantics. The text is held as a [`Rope`] handle, so publishing is an O(1)
/// copy-on-write clone rather than a full-buffer copy, and still gives Scheme
/// the full read surface Emacs Lisp expects.
#[derive(Default, Clone)]
pub struct EditorSnapshot {
    /// Active buffer name.
    pub name: String,
    /// Point's line (0-based).
    pub line: usize,
    /// Buffer's directory — visited file's parent, else the process cwd
    /// (Emacs's buffer-local `default-directory`).
    pub directory: String,
    /// Point as a byte offset.
    pub point: usize,
    /// The full buffer text. A [`Rope`] handle, not an owned `String`: the editor
    /// publishes it with an O(1) copy-on-write clone (no per-command full-text
    /// copy), and the read bridges materialize only the slice they need, outside
    /// the snapshot lock.
    pub text: Rope,
    /// The active region's byte range, if the mark is active.
    pub region: Option<(usize, usize)>,
    /// Text properties `(start, end, key, value)`, for `get-text-property`.
    pub props: Vec<(usize, usize, String, String)>,
    /// Buffer-local variables `(key, value)`, for `buffer-local-get`.
    pub locals: Vec<(String, String)>,
    /// Active-buffer LSP diagnostics, one record each as
    /// `"line\tcol\tseverity\tmessage"` (line/col 0-based), for `(diagnostics)`.
    pub diagnostics: Vec<String>,
    /// Pane (window) ids in cycle order, for `(window-list)`.
    pub windows: Vec<usize>,
    /// The selected pane (window) id, for `(selected-window)`.
    pub selected_window: usize,
    /// Whether the active buffer has a ready, opened LSP server, for
    /// `(lsp-server-ready?)` — lets Scheme guard LSP calls.
    pub lsp_ready: bool,
    /// Approximate first/last buffer line currently on screen, for `(visible-lines)`
    /// — avy scopes its jump hints to what the user can see.
    pub visible_lines: (usize, usize),
}

/// The published editor snapshot. An [`ArcSwap`] rather than a `Mutex`: the
/// editor publishes a new snapshot with a single atomic store and the Scheme
/// read bridges `load()` it without ever taking a lock, so a read bridge and a
/// publish never block each other (and neither blocks the UI thread).
type SharedSnapshot = Arc<ArcSwap<EditorSnapshot>>;

/// Markers: byte positions that move as the buffer is edited (Emacs markers).
/// Shared so `make-marker` can allocate an id and return it synchronously from
/// the engine thread, while the editor adjusts positions on every edit.
#[derive(Default)]
struct MarkerStore {
    next: u64,
    positions: HashMap<u64, usize>,
}
type Markers = Arc<Mutex<MarkerStore>>;

/// The applied editor options, published by the editor after every option change
/// so Scheme can read them back via `(get-option "name")`.
type Options = Arc<Mutex<HashMap<String, crate::settings::OptionValue>>>;

/// A monotonic id source shared among the `make-process` / `make-terminal`
/// bridges on the engine thread. Each lives in its own editor-side map keyed by
/// id, so they don't need to share the marker counter (which is for markers
/// only) — a dedicated atomic keeps the concern separate.
#[derive(Clone, Default)]
struct IdGen(Arc<std::sync::atomic::AtomicU64>);
impl IdGen {
    fn next(&self) -> u64 {
        self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
    }
}

/// Scheme-configurable options for the ripgrep-library search primitives
/// (`walk-files` / `grep-files`). Lives on the engine thread, shared among the
/// search register_fns. Defaults match ripgrep's sensible behaviour
/// (gitignore-aware, skip hidden).
#[derive(Clone)]
struct SearchConfig {
    /// Extra exclude globs, e.g. `"target"`, `"*.lock"` (added as `!glob`).
    excludes: Vec<String>,
    /// Whitelist globs — if non-empty, only matching files are walked.
    includes: Vec<String>,
    hidden: bool,
    follow_links: bool,
    respect_gitignore: bool,
    max_depth: Option<usize>,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            excludes: Vec::new(),
            includes: Vec::new(),
            hidden: false,
            follow_links: false,
            respect_gitignore: true,
            max_depth: None,
        }
    }
}

/// Build ripgrep's gitignore-aware walker for `dir` under the current config.
fn build_walk(dir: &str, config: &SearchConfig) -> ignore::Walk {
    let mut wb = ignore::WalkBuilder::new(dir);
    wb.hidden(!config.hidden)
        .git_ignore(config.respect_gitignore)
        .git_global(config.respect_gitignore)
        .git_exclude(config.respect_gitignore)
        .ignore(config.respect_gitignore)
        .follow_links(config.follow_links)
        .max_depth(config.max_depth);
    if !config.includes.is_empty() || !config.excludes.is_empty() {
        let mut ob = ignore::overrides::OverrideBuilder::new(dir);
        for inc in &config.includes {
            let _ = ob.add(inc);
        }
        for exc in &config.excludes {
            let _ = ob.add(&format!("!{exc}"));
        }
        if let Ok(ov) = ob.build() {
            wb.overrides(ov);
        }
    }
    wb.build()
}

/// The outcome of a blocking eval: enqueued commands + the last value.
/// Only [`ScriptEngine::eval`] (tests / REPL) builds this; startup uses
/// [`ScriptEngine::eval_collecting`], which keeps commands even on error.
#[allow(dead_code)]
pub struct EvalResult {
    pub commands: Vec<EditorCommand>,
    pub value: Option<String>,
}

/// Something the engine thread sends back to the editor's foreground drain task.
pub enum ScriptOutcome {
    /// A dispatched eval/action: commands to apply + an optional echo message.
    Commands {
        commands: Vec<EditorCommand>,
        message: Option<String>,
    },
    /// Dynamic Helm candidates for `source`, computed for query `query_id`
    /// (stale results — id mismatch — are discarded by the editor). Each entry is
    /// a candidate plus an optional marginalia-style annotation.
    Candidates {
        source: usize,
        query_id: u64,
        candidates: Vec<(String, Option<String>)>,
    },
    /// Completion-at-point results from Scheme CAPFs: the byte range to replace
    /// and the candidate items (label + optional annotation). Empty = no CAPF
    /// produced anything, so the editor falls back to LSP.
    Capf {
        start: usize,
        end: usize,
        items: Vec<(String, Option<String>)>,
    },
}

struct RawResult {
    commands: Vec<EditorCommand>,
    value: Option<String>,
    error: Option<String>,
}

/// Where a job's result goes: back to a blocking caller, or to the async drain.
enum Reply {
    Sync(Sender<RawResult>),
    Async,
}

enum Task {
    Eval(String),
    Call(String, Option<String>),
    /// Compute dynamic Helm candidates: call `function` with the pattern.
    Candidates {
        source: usize,
        query_id: u64,
        function: String,
        pattern: String,
    },
    /// Run completion-at-point functions in order until one yields candidates.
    Capf {
        functions: Vec<String>,
        prefix: String,
        start: usize,
        end: usize,
    },
}

struct Job {
    task: Task,
    interactive: bool,
    reply: Reply,
}

/// A handle to the Steel interpreter, which runs on its OWN thread so a slow or
/// runaway script can never freeze the UI (the editor's command loop solves
/// Emacs's "elisp blocks the main loop" problem). The engine is `!Send`, so it
/// is created on and never leaves that thread — only plain data (`String`,
/// `EditorCommand`) crosses the channel boundary. The editor talks to it two
/// ways: **blocking** (startup/config, where command ordering matters, and
/// tests) and **dispatched** (interactive commands, drained on the foreground).
pub struct ScriptEngine {
    jobs: Sender<Job>,
    command_names: Vec<String>,
    global_names: Vec<String>,
    /// Fast path: a nullary command name → its `EditorCommand`, so trivial
    /// keybindings (motion, etc.) apply synchronously without a thread hop.
    builtins: HashMap<String, EditorCommand>,
    buffers: BufferNames,
    point: SharedSnapshot,
    markers: Markers,
    options: Options,
}

impl ScriptEngine {
    /// Spawn the engine thread; returns the handle and the receiver of async
    /// outcomes (the editor drains it on a foreground task).
    pub fn new() -> (Self, UnboundedReceiver<ScriptOutcome>) {
        let nullary = nullary_commands();
        let command_names: Vec<String> = nullary.iter().map(|(name, _)| name.to_string()).collect();
        let mut global_names = command_names.clone();
        global_names.extend(
            [
                "insert",
                "message",
                "define-key",
                "set-face",
                "load-theme",
                "set-lsp",
                "set-extension",
                "set-language-id",
                "set-option",
                "get-option",
                "option-list",
                "add-hook",
                "remove-hook",
                "helm",
                "helm-from",
                "buffer-names",
                "switch-to-buffer",
                "kill-buffer-named",
                "set-major-mode",
                "key-binding",
                "keys-with-prefix",
                "command-list",
                "function-doc",
                "function-list",
                "function-source",
                "index-doc-file",
                "macros-version",
                "open-file",
                "lsp-rename-to",
                "lsp-workspace-symbol-go",
                "lsp-request",
                "lsp-goto",
                "lsp-apply-workspace-edit",
                "lsp-apply-text-edits",
                "open-file-other-window",
                "select-window",
                "window-list",
                "selected-window",
                "set-line-icon",
                "clear-line-icons",
                "set-line-image-impl",
                "clear-line-images",
                "set-line-indent",
                "clear-line-indents",
                "current-directory",
                "directory-of",
                "list-directory",
                "render-buffer",
                "call-process",
                "call-process-full",
                "getenv",
                "setenv",
                "goto-line",
                "transient",
                "transient-quit",
                "buffer-line",
                "current-buffer",
                "buffer-directory",
                "point",
                "point-max",
                "position->byte",
                "lsp-server-ready?",
                "line-bounds",
                "diagnostics",
                "regexp-quote",
                "buffer-string",
                "buffer-substring",
                "region-beginning",
                "region-end",
                "region-active?",
                "region-text",
                "search-forward",
                "goto-char",
                "read-string-impl",
                "append-to-buffer",
                "set-modeline-project",
                "file-exists?",
                "file-directory?",
                "packages-directory",
                "parent-directory",
                "read-file",
                "write-file",
                "org-id-new",
                "current-time",
                "walk-files",
                "grep-files",
                "search-exclude",
                "search-include",
                "search-reset",
                "set-search-option",
                "overlay-set-face",
                "overlay-hide",
                "fold-lines",
                "unfold-all",
                "set-fold-marker",
                "clear-fold-markers",
                "line-count",
                "line-indentation",
                "clear-overlays",
                "put-text-property",
                "get-text-property",
                "set-line-suffix",
                "make-process",
                "process-send-string",
                "kill-process",
                "anvil-eval",
                "anvil-interrupt",
                "anvil-reset",
                "anvil-eval-defun",
                "anvil-load-file",
                "anvil-doc",
                "anvil-macroexpand",
                "anvil-apropos",
                "run-with-timer",
                "delete-region",
                "kill-region-between",
                "replace-region",
                "insert-at",
                "set-region",
                "set-lsp-format-on-save",
                "lsp-semantic-tokens",
                "lsp-semantic-tokens-range",
                "lsp-semantic-tokens-delta",
                "tree-sitter-query",
                "tree-sitter-node-at",
                "undo-boundary",
                "string-match",
                "regexp-captures",
                "replace-regexp-in-string",
                "re-search-forward",
                "buffer-local-set",
                "buffer-local-get",
                "completing-read-impl",
                "read-file-name",
                "y-or-n-p",
                "make-marker",
                "marker-position",
                "set-marker",
                "delete-marker",
            ]
            .iter()
            .map(|s| s.to_string()),
        );
        let builtins: HashMap<String, EditorCommand> = nullary
            .into_iter()
            .map(|(n, c)| (n.to_string(), c))
            .collect();

        let buffers: BufferNames = Arc::new(Mutex::new(Vec::new()));
        let point: SharedSnapshot = Arc::new(ArcSwap::from_pointee(EditorSnapshot::default()));
        let markers: Markers = Arc::new(Mutex::new(MarkerStore::default()));
        let options: Options = Arc::new(Mutex::new(HashMap::new()));
        let (jobs, jobs_rx) = channel::<Job>();
        let (outcomes, outcomes_rx) = unbounded::<ScriptOutcome>();

        let thread_buffers = buffers.clone();
        let thread_point = point.clone();
        let thread_markers = markers.clone();
        let thread_options = options.clone();
        std::thread::spawn(move || {
            engine_thread(
                jobs_rx,
                outcomes,
                thread_buffers,
                thread_point,
                thread_markers,
                thread_options,
            )
        });

        (
            Self {
                jobs,
                command_names,
                global_names,
                builtins,
                buffers,
                point,
                markers,
                options,
            },
            outcomes_rx,
        )
    }

    /// Shift markers after an edit replacing `[start, old_end)` with `new_len`
    /// bytes: markers inside the deleted span collapse to `start`, markers after
    /// it shift by the length delta.
    pub fn adjust_markers(&self, start: usize, old_end: usize, new_len: usize) {
        let delta = new_len as isize - (old_end - start) as isize;
        let mut store = self.markers.lock().unwrap();
        for pos in store.positions.values_mut() {
            if *pos >= old_end {
                *pos = (*pos as isize + delta).max(0) as usize;
            } else if *pos > start {
                *pos = start;
            }
        }
    }

    /// Drop all markers (e.g. on a buffer switch — markers are buffer-positions).
    pub fn clear_markers(&self) {
        let mut store = self.markers.lock().unwrap();
        store.positions.clear();
    }

    /// Create a marker at `pos`, returning its id. The snippet session uses these
    /// to track tab-stop fields, which then ride along on `adjust_markers`.
    pub fn add_marker(&self, pos: usize) -> u64 {
        let mut store = self.markers.lock().unwrap();
        let id = store.next;
        store.next += 1;
        store.positions.insert(id, pos);
        id
    }

    /// Current byte position of marker `id`, if it still exists.
    pub fn marker_pos(&self, id: u64) -> Option<usize> {
        self.markers.lock().unwrap().positions.get(&id).copied()
    }

    /// Remove marker `id` (snippet field cleanup).
    pub fn remove_marker(&self, id: u64) {
        self.markers.lock().unwrap().positions.remove(&id);
    }

    /// The nullary command names, for `M-x` completion.
    pub fn command_names(&self) -> &[String] {
        &self.command_names
    }

    /// Every registered global name, for the built-in Steel LSP.
    pub fn global_names(&self) -> &[String] {
        &self.global_names
    }

    /// Update the buffer list Scheme sees via `(buffer-names)`.
    pub fn set_buffer_names(&self, names: Vec<String>) {
        *self.buffers.lock().unwrap() = names;
    }

    /// Publish the current editor options so Scheme can read them via
    /// `(get-option "name")`. Called after any option change.
    pub fn set_options(&self, options: HashMap<String, crate::settings::OptionValue>) {
        *self.options.lock().unwrap() = options;
    }

    /// Publish the active buffer name and point line for `(current-buffer)` /
    /// `(buffer-line)`. Called just before dispatching so a command reads point
    /// as it was when invoked.
    pub fn set_snapshot(&self, snapshot: EditorSnapshot) {
        self.point.store(Arc::new(snapshot));
    }

    /// If `expr` is a bare `(name)` naming a nullary builtin, return its command
    /// so the editor can apply it synchronously (instant motion, etc.) without a
    /// thread round-trip. Returns `None` for user functions / argument calls,
    /// which go to the engine thread.
    /// Recognise one or more bare `(builtin)` forms — `(forward-char)` or a
    /// sequence like `(set-mark) (move-end-of-line)` — and return their commands
    /// in order, so a motion/edit keybinding applies synchronously instead of
    /// paying a thread hop. All-or-nothing: if any form takes an argument, is a
    /// string, nests, or names a non-builtin, returns `None` and the whole
    /// expression goes to the engine thread (where its semantics are exact).
    pub fn builtins_for(&self, expr: &str) -> Option<Vec<EditorCommand>> {
        let mut out = Vec::new();
        let mut rest = expr.trim();
        while !rest.is_empty() {
            // Each form is `(name)` with no nested parens, so the first `)`
            // closes it. Anything with a string/arg fails the char check below
            // and drops the whole expression to the engine.
            let inner = rest.strip_prefix('(')?;
            let close = inner.find(')')?;
            let name = inner[..close].trim();
            if name.is_empty() || name.contains(|c: char| c.is_whitespace() || c == '(' || c == '"')
            {
                return None;
            }
            out.push(self.builtins.get(name)?.clone());
            rest = inner[close + 1..].trim();
        }
        (!out.is_empty()).then_some(out)
    }

    /// Blocking eval — tests and the REPL. Startup config uses
    /// [`Self::eval_collecting`] (which keeps partial commands on error).
    #[allow(dead_code)]
    pub fn eval(&self, source: &str) -> Result<EvalResult, String> {
        let (tx, rx) = channel();
        self.jobs
            .send(Job {
                task: Task::Eval(source.to_string()),
                interactive: false,
                reply: Reply::Sync(tx),
            })
            .map_err(|_| "script engine stopped".to_string())?;
        let raw = rx.recv().map_err(|_| "script engine stopped".to_string())?;
        match raw.error {
            Some(error) => Err(error),
            None => Ok(EvalResult {
                commands: raw.commands,
                value: raw.value,
            }),
        }
    }

    /// Like [`Self::eval`] but never throws the commands away on error: returns
    /// everything queued *before* the failure together with the error message.
    /// Startup config uses this so one bad form in `init.scm` can't silently drop
    /// the rest of the file — including the `(evil-mode)` that usually precedes
    /// it (Emacs `init.el` semantics: apply up to the error, then report it).
    pub fn eval_collecting(&self, source: &str) -> (Vec<EditorCommand>, Option<String>) {
        let (tx, rx) = channel();
        if self
            .jobs
            .send(Job {
                task: Task::Eval(source.to_string()),
                interactive: false,
                reply: Reply::Sync(tx),
            })
            .is_err()
        {
            return (Vec::new(), Some("script engine stopped".to_string()));
        }
        match rx.recv() {
            Ok(raw) => (raw.commands, raw.error),
            Err(_) => (Vec::new(), Some("script engine stopped".to_string())),
        }
    }

    /// Blocking action call — used by tests; the editor uses `dispatch_action`.
    #[allow(dead_code)]
    pub fn call_action(&self, name: &str, candidate: Option<&str>) -> Vec<EditorCommand> {
        let (tx, rx) = channel();
        if self
            .jobs
            .send(Job {
                task: Task::Call(name.to_string(), candidate.map(str::to_string)),
                interactive: false,
                reply: Reply::Sync(tx),
            })
            .is_err()
        {
            return Vec::new();
        }
        rx.recv().map(|raw| raw.commands).unwrap_or_default()
    }

    /// Async eval — runs on the engine thread; the outcome is delivered to the
    /// foreground drain task. A slow script never blocks the UI.
    pub fn dispatch(&self, source: &str, interactive: bool) {
        let _ = self.jobs.send(Job {
            task: Task::Eval(source.to_string()),
            interactive,
            reply: Reply::Async,
        });
    }

    /// Async action call (Helm actions, mode hooks).
    pub fn dispatch_action(&self, name: &str, candidate: Option<&str>) {
        let _ = self.jobs.send(Job {
            task: Task::Call(name.to_string(), candidate.map(str::to_string)),
            interactive: false,
            reply: Reply::Async,
        });
    }

    /// Compute a dynamic Helm source's candidates on the engine thread; the
    /// result arrives as `ScriptOutcome::Candidates` for `query_id`.
    pub fn dispatch_candidates(&self, source: usize, query_id: u64, function: &str, pattern: &str) {
        let _ = self.jobs.send(Job {
            task: Task::Candidates {
                source,
                query_id,
                function: function.to_string(),
                pattern: pattern.to_string(),
            },
            interactive: false,
            reply: Reply::Async,
        });
    }

    /// Run the registered completion-at-point functions on the engine thread,
    /// over the prefix `buffer[start..end]`. The result arrives as
    /// `ScriptOutcome::Capf`; empty means the editor should fall back to LSP.
    pub fn dispatch_capf(&self, functions: Vec<String>, prefix: &str, start: usize, end: usize) {
        let _ = self.jobs.send(Job {
            task: Task::Capf {
                functions,
                prefix: prefix.to_string(),
                start,
                end,
            },
            interactive: false,
            reply: Reply::Async,
        });
    }
}

/// The engine thread: owns the `!Send` `Engine` for its whole life, processing
/// jobs FIFO so results stay ordered.
fn engine_thread(
    jobs: Receiver<Job>,
    outcomes: UnboundedSender<ScriptOutcome>,
    buffers: BufferNames,
    point: SharedSnapshot,
    markers: Markers,
    options: Options,
) {
    let queue: Queue = Arc::new(Mutex::new(Vec::new()));
    let mut engine = build_engine(&queue, &buffers, &point, &markers, &options);

    while let Ok(Job {
        task,
        interactive,
        reply,
    }) = jobs.recv()
    {
        let raw = match task {
            // Dynamic Helm candidates: call the fn, parse a string list, reply
            // directly on the async channel (no command/value semantics).
            Task::Candidates {
                source,
                query_id,
                function,
                pattern,
            } => {
                let arg = pattern.into_steelval().unwrap_or(SteelVal::Void);
                let candidates = engine
                    .call_function_by_name_with_args(&function, vec![arg])
                    .ok()
                    .map(|value| steel_candidate_list(&value))
                    .unwrap_or_default();
                let _ = outcomes.unbounded_send(ScriptOutcome::Candidates {
                    source,
                    query_id,
                    candidates,
                });
                continue;
            }
            // Completion-at-point: call each registered CAPF with the prefix; the
            // first one returning a non-empty candidate list wins. A CAPF returns
            // either a flat candidate list, or `(start end (cand …))` to override
            // the replaced byte range. Reply on the async channel.
            Task::Capf {
                functions,
                prefix,
                start,
                end,
            } => {
                let mut out_start = start;
                let mut out_end = end;
                let mut items: Vec<(String, Option<String>)> = Vec::new();
                for function in &functions {
                    let arg = prefix.clone().into_steelval().unwrap_or(SteelVal::Void);
                    let Ok(value) = engine.call_function_by_name_with_args(function, vec![arg])
                    else {
                        continue;
                    };
                    let (s, e, parsed) = parse_capf_result(&value, start, end);
                    if !parsed.is_empty() {
                        out_start = s;
                        out_end = e;
                        items = parsed;
                        break;
                    }
                }
                let _ = outcomes.unbounded_send(ScriptOutcome::Capf {
                    start: out_start,
                    end: out_end,
                    items,
                });
                continue;
            }
            Task::Eval(source) => match engine.run(source) {
                Ok(values) => RawResult {
                    commands: drain(&queue),
                    value: values.last().and_then(format_value),
                    error: None,
                },
                Err(error) => RawResult {
                    commands: drain(&queue),
                    value: None,
                    error: Some(error.to_string()),
                },
            },
            Task::Call(name, arg) => {
                let args: Vec<SteelVal> = arg
                    .and_then(|c| c.into_steelval().ok())
                    .into_iter()
                    .collect();
                let error = engine
                    .call_function_by_name_with_args(&name, args)
                    .err()
                    .map(|e| e.to_string());
                RawResult {
                    commands: drain(&queue),
                    value: None,
                    error,
                }
            }
        };

        match reply {
            Reply::Sync(tx) => {
                let _ = tx.send(raw);
            }
            Reply::Async => {
                let RawResult {
                    commands,
                    value,
                    error,
                } = raw;
                let message = if let Some(error) = error {
                    Some(format!("error: {error}"))
                } else if interactive {
                    Some(value.map_or_else(|| "Done".to_string(), |v| format!("=> {v}")))
                } else {
                    None
                };
                let _ = outcomes.unbounded_send(ScriptOutcome::Commands { commands, message });
            }
        }
    }
}

fn drain(queue: &Queue) -> Vec<EditorCommand> {
    queue.lock().unwrap().drain(..).collect()
}

/// The nullary commands: each Steel name pushes its variant. Shared by engine
/// registration (on the thread) and the handle's fast-path table.
fn nullary_commands() -> Vec<(&'static str, EditorCommand)> {
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
        ("evil-mode", EvilMode),
        ("evil-normal-state", EvilNormalState),
        ("evil-insert-state", EvilInsertState),
        ("evil-visual-state", EvilVisualState),
        ("evil-visual-line-state", EvilVisualLineState),
        ("evil-visual-insert", EvilVisualInsert),
        ("evil-visual-append", EvilVisualAppend),
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
fn build_engine(
    queue: &Queue,
    buffers: &BufferNames,
    point: &SharedSnapshot,
    markers: &Markers,
    options: &Options,
) -> Engine {
    let mut engine = Engine::new();
    for (name, command) in nullary_commands() {
        register_nullary(&mut engine, queue, name, command);
    }

    // Commands carrying a payload.
    register_unary(&mut engine, queue, "insert", EditorCommand::Insert);
    register_unary(&mut engine, queue, "message", EditorCommand::Message);
    // Shadow of every binding (mode, chord, command), for help/discoverability
    // read bridges. Written alongside the DefineKey command; lives on the engine
    // thread with the read fns.
    let bindings: Arc<Mutex<Vec<(String, String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    {
        let q = queue.clone();
        let shadow = bindings.clone();
        engine.register_fn(
            "define-key",
            move |mode: String, chord: String, command: String| {
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
        engine.register_fn(
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
        engine.register_fn(
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
        engine.register_fn("command-list", move || -> Vec<String> { names.clone() });
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
        engine.register_fn("function-doc", move |name: String| -> String {
            d.read()
                .unwrap()
                .get(&name)
                .map(|(doc, _, _)| doc.clone())
                .unwrap_or_default()
        });

        // `(function-list)` → every indexed name, sorted.
        let d = docs.clone();
        engine.register_fn("function-list", move || -> Vec<String> {
            let mut names: Vec<String> = d.read().unwrap().keys().cloned().collect();
            names.sort();
            names
        });

        // `(function-source name)` → `(file line)` (1-based line), or `'()` when
        // the name is unknown or carries no location. The jump action opens
        // `file` at `line`, or falls back to the doc when the file is absent.
        let d = docs.clone();
        engine.register_fn("function-source", move |name: String| -> Vec<SteelVal> {
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
        engine.register_fn("index-doc-file", move |path: String| -> usize {
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
        engine.register_fn("macros-version", || -> String {
            env!("CARGO_PKG_VERSION").to_string()
        });
    }
    {
        // `(diff-strings old new)` → line-level diff ops, one per line as
        // "tag\told_start\told_end\tnew_start\tnew_end" with 0-based,
        // end-exclusive LINE indices. tag ∈ equal|delete|insert|replace.
        // Pure function (no editor state); powers scheme/ediff.scm — Scheme
        // merges adjacent non-equal ops into navigable hunks.
        engine.register_fn(
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
        engine.register_fn(
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
    register_binary(&mut engine, queue, "set-face", EditorCommand::SetFace);
    register_unary(
        &mut engine,
        queue,
        "clipboard-set",
        EditorCommand::ClipboardSet,
    );
    register_unary(&mut engine, queue, "load-theme", EditorCommand::LoadTheme);
    register_binary(&mut engine, queue, "set-lsp", EditorCommand::SetLsp);
    register_binary(
        &mut engine,
        queue,
        "set-extension",
        EditorCommand::SetExtension,
    );
    register_binary(
        &mut engine,
        queue,
        "set-language-id",
        EditorCommand::SetLanguageId,
    );
    {
        // `set-option` accepts a number, boolean, or string; the value is coerced
        // to the option's declared type when applied (see `Settings::set`).
        let q = queue.clone();
        engine.register_fn("set-option", move |key: String, value: SteelVal| {
            q.lock()
                .unwrap()
                .push(EditorCommand::SetOption(key, option_from_steelval(&value)));
        });
    }
    {
        // Read-side bridge: Scheme can query the open buffers (for pickers).
        let shared = buffers.clone();
        engine.register_fn("buffer-names", move || -> Vec<String> {
            shared.lock().unwrap().clone()
        });
    }
    {
        // Read-side bridge: `(get-option "name")` returns the option's current
        // value as a native bool / number / string, or #f if it isn't set.
        let opts = options.clone();
        engine.register_fn("get-option", move |key: String| -> SteelVal {
            opts.lock()
                .unwrap()
                .get(&key)
                .map_or(SteelVal::BoolV(false), option_to_steelval)
        });
    }
    // `(option-list)` → every option key Rust knows about, so helm-apropos can
    // surface options ("font" → font-size) next to functions and variables.
    engine.register_fn("option-list", || -> Vec<String> {
        crate::settings::KNOWN_OPTIONS
            .iter()
            .map(|s| s.to_string())
            .collect()
    });
    register_unary(
        &mut engine,
        queue,
        "switch-to-buffer",
        EditorCommand::SwitchToBufferNamed,
    );
    register_unary(
        &mut engine,
        queue,
        "kill-buffer-named",
        EditorCommand::KillBufferNamed,
    );
    register_unary(
        &mut engine,
        queue,
        "set-major-mode",
        EditorCommand::SetMajorMode,
    );
    register_unary(&mut engine, queue, "open-file", EditorCommand::OpenFile);
    register_unary(
        &mut engine,
        queue,
        "lsp-rename-to",
        EditorCommand::LspRename,
    );
    register_unary(
        &mut engine,
        queue,
        "lsp-workspace-symbol-go",
        EditorCommand::LspWorkspaceSymbol,
    );
    {
        let q = queue.clone();
        // (lsp-request method kind params callback) — the generic bridge.
        engine.register_fn(
            "lsp-request",
            move |method: String, kind: String, params: String, callback: String| {
                q.lock().unwrap().push(EditorCommand::LspRequest {
                    method,
                    kind,
                    params,
                    callback,
                });
            },
        );
    }
    register_ternary(&mut engine, queue, "lsp-goto", |uri, line, character| {
        EditorCommand::LspGoto {
            uri,
            line,
            character,
        }
    });
    register_unary(
        &mut engine,
        queue,
        "lsp-apply-workspace-edit",
        EditorCommand::LspApplyWorkspaceEdit,
    );
    register_unary(
        &mut engine,
        queue,
        "lsp-apply-text-edits",
        EditorCommand::LspApplyTextEdits,
    );
    register_unary(
        &mut engine,
        queue,
        "open-file-other-window",
        EditorCommand::OpenFileOtherWindow,
    );
    register_unary(
        &mut engine,
        queue,
        "select-window",
        EditorCommand::SelectWindow,
    );
    register_unary(
        &mut engine,
        queue,
        "split-window-right-ratio",
        EditorCommand::SplitWindowRightRatio,
    );
    {
        // `(set-line-icon line col "folder")` attaches a bundled icon (asset
        // `icons/<name>.svg`) at character column `col` of `line` in the active
        // buffer; `(clear-line-icons)` removes them all. Used by the file tree.
        register_ternary(
            &mut engine,
            queue,
            "set-line-icon",
            EditorCommand::SetLineIcon,
        );
        let q = queue.clone();
        engine.register_fn("clear-line-icons", move || {
            q.lock().unwrap().push(EditorCommand::ClearLineIcons);
        });
    }
    {
        // `(set-line-image line "/abs/path.png" [max-width])` displays a decoded
        // raster image on rows reserved below `line` of the active buffer;
        // `(clear-line-images)` removes them all. `max-width` (px) caps the
        // displayed width — 0 uses the default cap. The optional arg is provided
        // by the `set-line-image` Scheme wrapper (core.scm); this is the impl.
        // Drives Org's inline-image / LaTeX-preview display and the roam graph.
        let q = queue.clone();
        engine.register_fn(
            "set-line-image-impl",
            move |line: usize, path: String, max_width: usize| {
                q.lock().unwrap().push(EditorCommand::SetLineImage {
                    line,
                    path,
                    max_width,
                });
            },
        );
        let q = queue.clone();
        engine.register_fn("clear-line-images", move || {
            q.lock().unwrap().push(EditorCommand::ClearLineImages);
        });
        // `(set-line-indent line cols)` gives buffer `line` a virtual left indent
        // of `cols` columns (display-only); `(clear-line-indents)` removes them
        // all. Drives Org's org-indent-mode look.
        let q = queue.clone();
        engine.register_fn("set-line-indent", move |line: usize, cols: usize| {
            q.lock()
                .unwrap()
                .push(EditorCommand::SetLineIndent { line, cols });
        });
        let q = queue.clone();
        engine.register_fn("clear-line-indents", move || {
            q.lock().unwrap().push(EditorCommand::ClearLineIndents);
        });
        // `(set-line-scale line scale)` renders buffer `line` at `scale`× the
        // base font size (e.g. a heading); `(clear-line-scales)` resets all.
        let q = queue.clone();
        engine.register_fn("set-line-scale", move |line: usize, scale: f64| {
            q.lock().unwrap().push(EditorCommand::SetLineScale {
                line,
                scale: scale as f32,
            });
        });
        let q = queue.clone();
        engine.register_fn("clear-line-scales", move || {
            q.lock().unwrap().push(EditorCommand::ClearLineScales);
        });
    }
    {
        // `(window-list)` → pane ids in cycle order; `(selected-window)` → the
        // focused pane id. Read bridges over the snapshot.
        register_snapshot_read(&mut engine, point, "window-list", |s| s.windows.clone());
        register_snapshot_read(&mut engine, point, "selected-window", |s| s.selected_window);
        // `(visible-lines)` → (first-line last-line) currently on screen.
        register_snapshot_read(&mut engine, point, "visible-lines", |s| {
            let (a, b) = s.visible_lines;
            vec![a, b]
        });
    }
    // State bridges for dynamic sources (run on the engine thread — blocking
    // I/O here never touches the UI). All pure value-returning functions.
    engine.register_fn("current-directory", || -> String {
        std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    engine.register_fn("directory-of", |path: String| -> String {
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
    engine.register_fn("list-directory", |path: String| -> Vec<String> {
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
    register_binary(&mut engine, queue, "add-hook", EditorCommand::AddHook);
    register_binary(&mut engine, queue, "remove-hook", EditorCommand::RemoveHook);
    register_unary(
        &mut engine,
        queue,
        "add-completion-at-point-function",
        EditorCommand::AddCapf,
    );
    {
        // `(watch-directories owner dirs)` — replace `owner`'s watched dir set so
        // a change under any of them fires `directory-changed-hook` (treemacs /
        // dired live-refresh). Replace semantics: pass `'()` to drop them.
        let q = queue.clone();
        engine.register_fn("watch-directories", move |owner: String, dirs: SteelVal| {
            q.lock()
                .unwrap()
                .push(EditorCommand::WatchDirectories(owner, steel_string_list(&dirs)));
        });
    }
    {
        // `(helm sources)` where sources is a list of `(name candidates action)`.
        let q = queue.clone();
        engine.register_fn("helm", move |sources: SteelVal| {
            let parsed = steel_list(&sources)
                .unwrap_or_default()
                .iter()
                .filter_map(parse_helm_source)
                .collect();
            q.lock().unwrap().push(EditorCommand::Helm(parsed));
        });
    }
    {
        // `(helm-from initial sources)` — Helm with a pre-filled input.
        let q = queue.clone();
        engine.register_fn("helm-from", move |initial: String, sources: SteelVal| {
            let parsed = steel_list(&sources)
                .unwrap_or_default()
                .iter()
                .filter_map(parse_helm_source)
                .collect();
            q.lock()
                .unwrap()
                .push(EditorCommand::HelmFrom(initial, parsed));
        });
    }
    // --- Special buffers / transient (the magit primitives) ---------------
    {
        // `(render-buffer name mode text faces)` — faces is a list of
        // `(line-number face-name)`. Replaces/creates a read-only buffer.
        let q = queue.clone();
        engine.register_fn(
            "render-buffer",
            move |name: String, mode: String, text: String, faces: SteelVal| {
                let line_faces = parse_line_faces(&faces);
                q.lock().unwrap().push(EditorCommand::RenderBuffer {
                    name,
                    mode,
                    text,
                    line_faces,
                });
            },
        );
    }
    {
        // `(render-buffer-quiet name mode text faces)` — like render-buffer but
        // refresh in place without switching/focusing (sidebar auto-refresh).
        let q = queue.clone();
        engine.register_fn(
            "render-buffer-quiet",
            move |name: String, mode: String, text: String, faces: SteelVal| {
                let line_faces = parse_line_faces(&faces);
                q.lock().unwrap().push(EditorCommand::RenderBufferQuiet {
                    name,
                    mode,
                    text,
                    line_faces,
                });
            },
        );
    }
    register_unary(&mut engine, queue, "goto-line", EditorCommand::GotoLine);
    register_ternary(&mut engine, queue, "diff-buffers", |a, b, callback| {
        EditorCommand::DiffBuffers { a, b, callback }
    });
    register_unary(&mut engine, queue, "buffer-clear-highlights", |name| {
        EditorCommand::BufferClearHighlights { name }
    });
    register_unary(
        &mut engine,
        queue,
        "snippet-expand",
        EditorCommand::SnippetExpand,
    );
    register_unary(
        &mut engine,
        queue,
        "avy-show-hints",
        EditorCommand::AvyShowHints,
    );
    register_nullary(
        &mut engine,
        queue,
        "avy-clear-hints",
        EditorCommand::AvyClearHints,
    );
    {
        // `(buffer-copy-lines src ss se dst ds de)` — copy a line range between
        // named buffers (ediff hunk copy). Six args, so registered directly.
        let q = queue.clone();
        engine.register_fn(
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
        engine.register_fn(
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
    {
        // `(transient title rows)` — `rows` is a list of `(key label command)`,
        // or a list of `(column-title (key label command) …)` for columns.
        let q = queue.clone();
        engine.register_fn("transient", move |title: String, rows: SteelVal| {
            let columns = parse_transient_columns(&rows);
            q.lock()
                .unwrap()
                .push(EditorCommand::Transient { title, columns });
        });
    }
    register_nullary(
        &mut engine,
        queue,
        "transient-quit",
        EditorCommand::TransientQuit,
    );
    register_unary(&mut engine, queue, "goto-char", EditorCommand::GotoChar);
    register_binary(&mut engine, queue, "delete-region", |start, end| {
        EditorCommand::DeleteRegion { start, end }
    });
    register_binary(&mut engine, queue, "kill-region-between", |start, end| {
        EditorCommand::KillRegionBetween { start, end }
    });
    register_binary(&mut engine, queue, "set-region", |start, end| {
        EditorCommand::SetRegion { start, end }
    });
    register_binary(
        &mut engine,
        queue,
        "set-lsp-format-on-save",
        EditorCommand::SetLspFormatOnSave,
    );
    register_unary(
        &mut engine,
        queue,
        "lsp-semantic-tokens",
        EditorCommand::LspSemanticTokens,
    );
    register_ternary(&mut engine, queue, "replace-region", |start, end, text| {
        EditorCommand::ReplaceRegion { start, end, text }
    });
    register_binary(&mut engine, queue, "insert-at", |pos, text| {
        EditorCommand::InsertAt { pos, text }
    });
    register_unary(
        &mut engine,
        queue,
        "lsp-semantic-tokens-range",
        EditorCommand::LspSemanticTokensRange,
    );
    register_unary(
        &mut engine,
        queue,
        "lsp-semantic-tokens-delta",
        EditorCommand::LspSemanticTokensDelta,
    );
    register_binary(
        &mut engine,
        queue,
        "tree-sitter-query",
        |query, callback| EditorCommand::TreeSitterQuery { query, callback },
    );
    register_binary(
        &mut engine,
        queue,
        "tree-sitter-node-at",
        |byte, callback| EditorCommand::TreeSitterNodeAt { byte, callback },
    );
    register_nullary(
        &mut engine,
        queue,
        "undo-boundary",
        EditorCommand::UndoBoundary,
    );
    register_unary(
        &mut engine,
        queue,
        "undo-tree-dump",
        EditorCommand::UndoTreeDump,
    );
    register_binary(&mut engine, queue, "buffer-local-set", |key, value| {
        EditorCommand::BufferLocalSet { key, value }
    });
    {
        // Backing primitive for the `completing-read` Scheme wrapper (which makes
        // `initial` optional). `candidates` may be a static list OR a Scheme fn
        // name (string/symbol) — re-queried per keystroke with the current input,
        // the minibuffer equivalent of a dynamic Helm source.
        let q = queue.clone();
        engine.register_fn(
            "completing-read-impl",
            move |prompt: String, candidates: SteelVal, callback: String, initial: String| {
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
    register_binary(&mut engine, queue, "read-file-name", |prompt, callback| {
        EditorCommand::ReadFileName { prompt, callback }
    });
    register_binary(&mut engine, queue, "y-or-n-p", |prompt, callback| {
        EditorCommand::YesOrNo { prompt, callback }
    });
    {
        // Value of buffer-local `key` on the active buffer, or "".
        let shared = point.clone();
        engine.register_fn("buffer-local-get", move |key: String| -> String {
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
    {
        let q = queue.clone();
        engine.register_fn(
            "read-string-impl",
            move |prompt: String, callback: String, initial: String| {
                q.lock().unwrap().push(EditorCommand::ReadString {
                    prompt,
                    initial,
                    callback,
                });
            },
        );
    }
    register_binary(&mut engine, queue, "append-to-buffer", |name, text| {
        EditorCommand::AppendToBuffer { name, text }
    });
    register_unary(
        &mut engine,
        queue,
        "set-modeline-project",
        EditorCommand::SetModelineProject,
    );
    // --- Overlays / text properties / folds -------------------------------
    register_ternary(
        &mut engine,
        queue,
        "overlay-set-face",
        |start, end, face| EditorCommand::OverlayFace { start, end, face },
    );
    register_binary(&mut engine, queue, "overlay-hide", |start, end| {
        EditorCommand::OverlayHide { start, end }
    });
    register_binary(&mut engine, queue, "fold-lines", |start_line, end_line| {
        EditorCommand::FoldLines {
            start_line,
            end_line,
        }
    });
    register_nullary(&mut engine, queue, "unfold-all", EditorCommand::UnfoldAll);
    // `(set-fold-marker line open?)` paints a gutter chevron on `line`; the click
    // is hit-tested back to `(fold-toggle-at-line line)`. `(clear-fold-markers)`
    // removes them all (modes re-declare markers on refresh).
    register_binary(&mut engine, queue, "set-fold-marker", |line, open| {
        EditorCommand::SetFoldMarker { line, open }
    });
    register_nullary(
        &mut engine,
        queue,
        "clear-fold-markers",
        EditorCommand::ClearFoldMarkers,
    );
    register_nullary(
        &mut engine,
        queue,
        "clear-overlays",
        EditorCommand::ClearOverlays,
    );
    {
        let q = queue.clone();
        engine.register_fn(
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
    register_ternary(&mut engine, queue, "set-line-suffix", |line, text, face| {
        EditorCommand::SetLineSuffix { line, text, face }
    });
    {
        // Value of text property `key` at byte `pos`, or "" — read over the
        // published snapshot (Scheme writes via put-text-property, reads next cmd).
        let shared = point.clone();
        engine.register_fn(
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
    engine.register_fn("file-exists?", |path: String| -> bool {
        std::path::Path::new(&path).exists()
    });
    engine.register_fn("file-directory?", |path: String| -> bool {
        std::path::Path::new(&path).is_dir()
    });
    // `(packages-directory)` → where `package-install` clones git packages:
    // the active config dir's `packages/` subdirectory. Resolved in Rust at call
    // time (the same config-dir logic that finds init.scm), so Scheme needn't
    // thread the path through a global.
    engine.register_fn("packages-directory", || -> String {
        crate::runtime::config_directory()
            .join("packages")
            .to_string_lossy()
            .into_owned()
    });
    engine.register_fn("parent-directory", |path: String| -> String {
        // Parent of a path, trailing slash trimmed first so a directory's own
        // parent is returned (Emacs `(file-name-directory (directory-file-name …))`).
        let trimmed = path.trim_end_matches('/');
        std::path::Path::new(trimmed)
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    engine.register_fn("read-file", |path: String| -> String {
        std::fs::read_to_string(&path).unwrap_or_default()
    });
    engine.register_fn("write-file", |path: String, text: String| -> bool {
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
    engine.register_fn("org-id-new", || -> String {
        uuid::Uuid::new_v4().to_string()
    });
    // `(current-time fmt)` → local time formatted with a `strftime`-style string
    // (Emacs `format-time-string`), e.g. `(current-time "%Y%m%d%H%M%S")` for a
    // node filename, `(current-time "%Y-%m-%d")` for an agenda date. An invalid
    // format yields "" rather than panicking (write! returns Err, never unwinds).
    engine.register_fn("current-time", |fmt: String| -> String {
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
            engine.register_fn("search-exclude", move |glob: String| {
                s.lock().unwrap().excludes.push(glob);
            });
        }
        {
            let s = search.clone();
            engine.register_fn("search-include", move |glob: String| {
                s.lock().unwrap().includes.push(glob);
            });
        }
        {
            let s = search.clone();
            engine.register_fn("search-reset", move || {
                *s.lock().unwrap() = SearchConfig::default();
            });
        }
        {
            let s = search.clone();
            engine.register_fn("set-search-option", move |key: String, value: SteelVal| {
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
            engine.register_fn("walk-files", move |dir: String| -> Vec<String> {
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
            engine.register_fn(
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
    // `(call-process program args)` — run a subprocess, return stdout. Blocks on
    // the engine thread (never the UI), the same model as `list-directory`.
    engine.register_fn(
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
    engine.register_fn(
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
    engine.register_fn("getenv", |name: String| -> String {
        std::env::var(&name).unwrap_or_default()
    });
    // `(setenv name value)` → set a process env var, visible to every
    // subsequently spawned subprocess (LSP servers, call-process, make-process).
    // Lets a config extend PATH or pin a toolchain, e.g.
    //   (setenv "PATH" (string-append (getenv "PATH") ":" ...))
    // Runs on the single engine thread during `init.scm` eval, before any
    // process is spawned from Scheme, so the mutation is sound.
    engine.register_fn("setenv", |name: String, value: String| {
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
        engine.register_fn(
            "make-process",
            move |program: String, args: SteelVal, on_output: String, on_exit: String| -> usize {
                let id = ids.next();
                let args = steel_string_list(&args);
                let on_exit = if on_exit.is_empty() {
                    None
                } else {
                    Some(on_exit)
                };
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
    register_binary(&mut engine, queue, "process-send-string", |id, text| {
        EditorCommand::SendProcess { id, text }
    });
    register_unary(&mut engine, queue, "kill-process", |id| {
        EditorCommand::KillProcess { id }
    });
    // --- ANVIL REPL (a second, independent Steel interpreter) -------------
    {
        // `(anvil-eval source on-result)` evaluates `source` in the separate
        // REPL image; the printed result is delivered to the Scheme function
        // named `on-result` (in this engine) as a single string argument.
        let q = queue.clone();
        engine.register_fn("anvil-eval", move |source: String, on_result: String| {
            q.lock()
                .unwrap()
                .push(EditorCommand::AnvilEval { source, on_result });
        });
    }
    {
        // `(anvil-interrupt)` cancels the REPL's current evaluation.
        let q = queue.clone();
        engine.register_fn("anvil-interrupt", move || {
            q.lock().unwrap().push(EditorCommand::AnvilInterrupt);
        });
    }
    {
        // `(anvil-reset)` rebuilds the REPL image from scratch.
        let q = queue.clone();
        engine.register_fn("anvil-reset", move || {
            q.lock().unwrap().push(EditorCommand::AnvilReset);
        });
    }
    {
        // SLIME-style interaction commands. The editor computes the buffer
        // context (the form at point, the symbol at point, the file path) since
        // that needs the live buffer; the result streams back to the REPL.
        let q = queue.clone();
        engine.register_fn("anvil-eval-defun", move || {
            q.lock().unwrap().push(EditorCommand::AnvilEvalDefun);
        });
        let q = queue.clone();
        engine.register_fn("anvil-load-file", move || {
            q.lock().unwrap().push(EditorCommand::AnvilLoadFile);
        });
        let q = queue.clone();
        engine.register_fn("anvil-doc", move || {
            q.lock().unwrap().push(EditorCommand::AnvilDoc);
        });
        let q = queue.clone();
        engine.register_fn("anvil-macroexpand", move || {
            q.lock().unwrap().push(EditorCommand::AnvilMacroexpand);
        });
        let q = queue.clone();
        engine.register_fn("anvil-apropos", move |pattern: String| {
            q.lock().unwrap().push(EditorCommand::AnvilApropos(pattern));
        });
    }
    {
        // `(make-terminal name program args)` → a terminal id. Empty `program`
        // runs the user's login shell. The terminal is shown in a `term-mode`
        // buffer named `name`; switching to that buffer later reuses it.
        let q = queue.clone();
        let ids = markers.clone(); // shared monotonic id source
        engine.register_fn(
            "make-terminal",
            move |name: String, program: String, args: SteelVal| -> usize {
                let id = {
                    let mut store = ids.lock().unwrap();
                    store.next += 1;
                    store.next
                };
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
    register_binary(&mut engine, queue, "run-with-timer", |ms, callback| {
        EditorCommand::RunWithTimer { ms, callback }
    });
    // Read bridges over the editor's published snapshot (see `set_snapshot`).
    register_snapshot_read(&mut engine, point, "buffer-line", |s| s.line);
    register_snapshot_read(&mut engine, point, "current-buffer", |s| s.name.clone());
    // The active buffer's directory — its visited file's parent, or the cwd
    // for fileless buffers (Emacs's buffer-local `default-directory`).
    register_snapshot_read(&mut engine, point, "buffer-directory", |s| {
        s.directory.clone()
    });
    // Point as a byte offset (Emacs `(point)`).
    register_snapshot_read(&mut engine, point, "point", |s| s.point);
    {
        // The whole buffer (Emacs `(buffer-string)`). Clone the cheap rope handle
        // under the lock, then materialize the String outside it so the snapshot
        // lock isn't held for a full-buffer copy.
        let shared = point.clone();
        engine.register_fn("buffer-string", move || -> String {
            let rope = shared.load().text.clone();
            rope.to_string()
        });
    }
    // End of buffer as a byte offset (Emacs `(point-max)`). O(1) on the rope, no
    // text materialized.
    register_snapshot_read(&mut engine, point, "point-max", |s| s.text.len_bytes());
    {
        // `(position->byte line character)` → byte offset of an LSP position
        // (0-based line, UTF-16 column) in the active buffer. Lets Scheme LSP
        // handlers turn server line/col coordinates into the byte offsets the
        // overlay/fold/goto primitives expect.
        let shared = point.clone();
        engine.register_fn(
            "position->byte",
            move |line: usize, character: usize| -> usize {
                let rope = shared.load().text.clone();
                let (mut cur_line, mut col) = (0usize, 0usize);
                let mut byte = 0usize;
                for c in rope.chars() {
                    if cur_line == line && col >= character {
                        return byte;
                    }
                    if c == '\n' {
                        if cur_line == line {
                            return byte;
                        }
                        cur_line += 1;
                        col = 0;
                    } else {
                        col += c.len_utf16();
                    }
                    byte += c.len_utf8();
                }
                rope.len_bytes()
            },
        );
    }
    // `(lsp-server-ready?)` → #t if the active buffer has a ready LSP server.
    register_snapshot_read(&mut engine, point, "lsp-server-ready?", |s| s.lsp_ready);
    // `(diagnostics)` → the active buffer's LSP diagnostics, each a record
    // "line\tcol\tseverity\tmessage" (line/col 0-based). The library splits it.
    register_snapshot_read(&mut engine, point, "diagnostics", |s| s.diagnostics.clone());
    {
        // `(line-bounds)` → `(start end)` byte offsets of the line at point
        // (end points at the line's terminating newline, or buffer end).
        let shared = point.clone();
        engine.register_fn("line-bounds", move || -> Vec<usize> {
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
    register_snapshot_read(&mut engine, point, "line-count", |s| s.text.len_lines());
    {
        // `(line-indentation line)` → leading-whitespace width (spaces, tabs count
        // as one column each) of buffer `line`, or -1 for a blank/whitespace-only
        // line so fold logic can skip blanks. The indentation-based folder builds
        // fold ranges from this.
        let shared = point.clone();
        engine.register_fn("line-indentation", move |line: usize| -> isize {
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
    engine.register_fn("regexp-quote", |s: String| -> String { regex::escape(&s) });
    // `(fuzzy-match pattern candidates)` — rank `candidates` against `pattern`
    // with the same nucleo matcher Helm uses, best-match first; non-matches are
    // dropped. An empty pattern returns the list unchanged. Lets a Scheme-driven
    // Helm filter do compiled-speed scoring instead of reimplementing it.
    engine.register_fn(
        "fuzzy-match",
        |pattern: String, candidates: SteelVal| -> Vec<String> {
            fuzzy_rank(&pattern, &steel_string_list(&candidates))
        },
    );
    {
        // Bytes `[a, b)` of the buffer, clamped (Emacs `(buffer-substring a b)`).
        let shared = point.clone();
        engine.register_fn("buffer-substring", move |a: usize, b: usize| -> String {
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
        register_snapshot_read(&mut engine, point, "region-beginning", |s| {
            s.region.map_or(-1, |(a, _)| a as isize)
        });
        register_snapshot_read(&mut engine, point, "region-end", |s| {
            s.region.map_or(-1, |(_, b)| b as isize)
        });
        register_snapshot_read(&mut engine, point, "region-active?", |s| s.region.is_some());
        // The selected text, or "" when no region is active (Emacs has no single
        // primitive for this; it's `(buffer-substring (region-beginning) …)`).
        let shared = point.clone();
        engine.register_fn("region-text", move || -> String {
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
        let shared = point.clone();
        engine.register_fn("search-forward", move |needle: String| -> isize {
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
        engine.register_fn("make-marker", move |pos: usize| -> usize {
            let mut store = m.lock().unwrap();
            store.next += 1;
            let id = store.next;
            store.positions.insert(id, pos);
            id as usize
        });
    }
    {
        // `(marker-position id)` → its current byte position, or -1.
        let m = markers.clone();
        engine.register_fn("marker-position", move |id: u64| -> isize {
            m.lock()
                .unwrap()
                .positions
                .get(&id)
                .map_or(-1, |p| *p as isize)
        });
    }
    {
        let m = markers.clone();
        engine.register_fn("set-marker", move |id: u64, pos: usize| {
            m.lock().unwrap().positions.insert(id, pos);
        });
    }
    {
        let m = markers.clone();
        engine.register_fn("delete-marker", move |id: u64| {
            m.lock().unwrap().positions.remove(&id);
        });
    }
    // --- Regexp (over strings, and over the buffer from point) ------------
    engine.register_fn("string-match", move |pattern: String, s: String| -> isize {
        Regex::new(&pattern)
            .ok()
            .and_then(|re| re.find(&s).map(|m| m.start() as isize))
            .unwrap_or(-1)
    });
    engine.register_fn(
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
    engine.register_fn(
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
        let shared = point.clone();
        engine.register_fn("re-search-forward", move |pattern: String| -> Vec<usize> {
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
    engine
}

/// Parse a list of `(line-number face-name)` pairs for `render-buffer`.
fn parse_line_faces(value: &SteelVal) -> Vec<(usize, String)> {
    steel_list(value)
        .unwrap_or_default()
        .iter()
        .filter_map(|item| {
            let pair = steel_list(item)?;
            let line = match pair.first()? {
                SteelVal::IntV(n) => (*n).max(0) as usize,
                _ => return None,
            };
            Some((line, steel_string(pair.get(1)?)?))
        })
        .collect()
}

/// Parse a list of `(key label command)` rows for `transient`.
fn parse_transient_item(item: &SteelVal) -> Option<TransientItem> {
    let row = steel_list(item)?;
    Some(TransientItem {
        key: steel_string(row.first()?)?,
        label: steel_string(row.get(1)?)?,
        command: steel_string(row.get(2)?)?,
    })
}

/// Parse `transient`'s rows argument into columns. Two accepted shapes:
///   flat:    `(list (k l c) (k l c) …)`            → one untitled column
///   grouped: `(list (title (k l c) …) (title …))`  → titled columns
/// A group is recognised by its second element being a list (a row) rather than
/// a string (a key), so the two forms are unambiguous.
fn parse_transient_columns(value: &SteelVal) -> Vec<TransientColumn> {
    let elements = steel_list(value).unwrap_or_default();
    let grouped = elements
        .first()
        .and_then(steel_list)
        .and_then(|first| first.get(1).map(|second| steel_list(second).is_some()))
        .unwrap_or(false);
    if !grouped {
        return vec![TransientColumn {
            title: None,
            rows: elements.iter().filter_map(parse_transient_item).collect(),
        }];
    }
    elements
        .iter()
        .filter_map(|group| {
            let group = steel_list(group)?;
            let title = steel_string(group.first()?);
            let rows = group
                .iter()
                .skip(1)
                .filter_map(parse_transient_item)
                .collect();
            Some(TransientColumn { title, rows })
        })
        .collect()
}

fn steel_string(value: &SteelVal) -> Option<String> {
    match value {
        SteelVal::StringV(s) => Some(s.to_string()),
        SteelVal::SymbolV(s) => Some(s.to_string()),
        _ => None,
    }
}

/// Convert a value passed to `(set-option …)` into an untyped [`OptionValue`].
/// Numbers, booleans, and strings map to the matching variant; anything else is
/// stringified. The real coercion to the option's declared type happens later in
/// `Settings::set`.
fn option_from_steelval(value: &SteelVal) -> crate::settings::OptionValue {
    use crate::settings::OptionValue;
    match value {
        SteelVal::BoolV(b) => OptionValue::Bool(*b),
        SteelVal::IntV(i) => OptionValue::Int(*i as i64),
        SteelVal::NumV(f) => OptionValue::Float(*f),
        SteelVal::StringV(s) => OptionValue::Str(s.to_string()),
        SteelVal::SymbolV(s) => OptionValue::Str(s.to_string()),
        other => OptionValue::Str(other.to_string()),
    }
}

/// Convert a stored [`OptionValue`] back into a Steel value for `(get-option …)`.
fn option_to_steelval(v: &crate::settings::OptionValue) -> SteelVal {
    use crate::settings::OptionValue;
    match v {
        OptionValue::Bool(b) => SteelVal::BoolV(*b),
        OptionValue::Int(i) => SteelVal::IntV(*i as isize),
        OptionValue::Float(f) => SteelVal::NumV(*f),
        OptionValue::Str(s) => SteelVal::StringV(s.clone().into()),
    }
}

fn steel_list(value: &SteelVal) -> Option<Vec<SteelVal>> {
    match value {
        SteelVal::ListV(list) => Some(list.iter().cloned().collect()),
        _ => None,
    }
}

/// Rank `candidates` against `pattern` with nucleo (best first), dropping
/// non-matches. An empty pattern keeps the list as-is. The matcher matches Helm's
/// own (`helm_filter`), so a Scheme filter ranks identically to a native source.
fn fuzzy_rank(pattern: &str, candidates: &[String]) -> Vec<String> {
    if pattern.is_empty() {
        return candidates.to_vec();
    }
    let mut matcher = Matcher::new(Config::DEFAULT);
    let parsed = Pattern::parse(pattern, CaseMatching::Smart, Normalization::Smart);
    parsed
        .match_list(candidates.iter(), &mut matcher)
        .into_iter()
        .map(|(c, _score)| c.clone())
        .collect()
}

/// A list of strings (anything that isn't a string is dropped).
fn steel_string_list(value: &SteelVal) -> Vec<String> {
    steel_list(value)
        .map(|items| items.iter().filter_map(steel_string).collect())
        .unwrap_or_default()
}

/// A list of completion candidates, each either a bare string or a
/// `(candidate annotation)` pair (the marginalia form). Non-conforming entries
/// are dropped.
fn steel_candidate_list(value: &SteelVal) -> Vec<(String, Option<String>)> {
    steel_list(value)
        .map(|items| items.iter().filter_map(steel_candidate).collect())
        .unwrap_or_default()
}

/// Recognize the `(raw "fn")` source form: a list whose head is the symbol
/// `raw` and whose second element is a Scheme fn name. Returns that name. Used to
/// tag a self-filtered dynamic Helm source; anything else parses as static
/// candidates. The head must be a *symbol* so a candidate list whose first entry
/// happens to be the string `"raw"` is not misread.
fn steel_raw_dynamic(value: &SteelVal) -> Option<String> {
    let items = steel_list(value)?;
    match items.first()? {
        SteelVal::SymbolV(s) if s.as_str() == "raw" => steel_string(items.get(1)?),
        _ => None,
    }
}

/// One candidate: a string `"cand"`, or a `(cand)` / `(cand annotation)` list.
fn steel_candidate(value: &SteelVal) -> Option<(String, Option<String>)> {
    if let Some(s) = steel_string(value) {
        return Some((s, None));
    }
    let pair = steel_list(value)?;
    let cand = steel_string(pair.first()?)?;
    let annotation = pair.get(1).and_then(steel_string);
    Some((cand, annotation))
}

/// Parse a CAPF function's return value into `(start, end, items)`. Accepted
/// shapes: a flat candidate list `((cand …))` (keeping the default range), or
/// `(start end candidate-list)` to also override the replaced byte range.
fn parse_capf_result(
    value: &SteelVal,
    default_start: usize,
    default_end: usize,
) -> (usize, usize, Vec<(String, Option<String>)>) {
    // `(start end candidate-list)` — integers in the first two slots.
    if let Some(items) = steel_list(value)
        && let (Some(SteelVal::IntV(s)), Some(SteelVal::IntV(e)), Some(cands)) =
            (items.first(), items.get(1), items.get(2))
    {
        return (
            (*s).max(0) as usize,
            (*e).max(0) as usize,
            steel_candidate_list(cands),
        );
    }
    // Otherwise treat the whole value as a flat candidate list.
    (default_start, default_end, steel_candidate_list(value))
}

/// Parse one `(name candidates actions)` source spec.
///   * `candidates` — a list (static) or a string/symbol (a dynamic fn name).
///   * `actions` — a string/symbol (single action) or a list of `(label fn)`.
fn parse_helm_source(value: &SteelVal) -> Option<HelmSourceSpec> {
    let items = steel_list(value)?;
    let name = steel_string(items.first()?)?;

    let candidates = match items.get(1)? {
        // `(raw "fn")` — a self-filtered dynamic source (leading `raw` symbol);
        // otherwise a plain list is static candidates.
        list @ SteelVal::ListV(_) => match steel_raw_dynamic(list) {
            Some(function) => HelmCandidates::DynamicRaw(function),
            None => HelmCandidates::Static(steel_candidate_list(list)),
        },
        other => HelmCandidates::Dynamic(steel_string(other)?),
    };

    let actions = match items.get(2)? {
        SteelVal::ListV(_) => steel_list(items.get(2)?)?
            .iter()
            .filter_map(|pair| {
                let pair = steel_list(pair)?;
                Some((steel_string(pair.first()?)?, steel_string(pair.get(1)?)?))
            })
            .collect(),
        single => vec![("default".to_string(), steel_string(single)?)],
    };
    if actions.is_empty() {
        return None;
    }

    // Optional 4th element: a preview fn name.
    let preview = items.get(3).and_then(steel_string);

    Some(HelmSourceSpec {
        name,
        candidates,
        actions,
        preview,
    })
}

/// Format a Steel value for the echo area, treating void as "no value".
pub(crate) fn format_value(value: &SteelVal) -> Option<String> {
    match value {
        SteelVal::Void => None,
        other => Some(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helm_parses_sources_from_scheme() {
        let (engine, _rx) = ScriptEngine::new();
        let result = engine
            .eval(r#"(helm (list (list "Files" (list "a.txt" "b.txt") "open-file")))"#)
            .unwrap();
        match result.commands.as_slice() {
            [EditorCommand::Helm(sources)] => {
                assert_eq!(sources.len(), 1);
                assert_eq!(sources[0].name, "Files");
                assert!(matches!(
                    &sources[0].candidates,
                    HelmCandidates::Static(c)
                        if c == &[("a.txt".to_string(), None), ("b.txt".to_string(), None)]
                ));
                assert_eq!(
                    sources[0].actions,
                    [("default".to_string(), "open-file".to_string())]
                );
            }
            other => panic!("expected one Helm command, got {other:?}"),
        }
    }

    #[test]
    fn helm_parses_dynamic_source_and_multiple_actions() {
        let (engine, _rx) = ScriptEngine::new();
        // Candidates as a function name (dynamic); actions as a (label fn) list.
        let result = engine
            .eval(
                r#"(helm (list (list "Files" "list-them"
                       (list (list "open" "open-file") (list "delete" "rm-file")))))"#,
            )
            .unwrap();
        match result.commands.as_slice() {
            [EditorCommand::Helm(sources)] => {
                assert!(matches!(
                    &sources[0].candidates,
                    HelmCandidates::Dynamic(f) if f == "list-them"
                ));
                assert_eq!(
                    sources[0].actions,
                    [
                        ("open".to_string(), "open-file".to_string()),
                        ("delete".to_string(), "rm-file".to_string()),
                    ]
                );
            }
            other => panic!("expected one Helm command, got {other:?}"),
        }
    }

    #[test]
    fn fuzzy_rank_orders_and_filters() {
        let cands = vec![
            "foo/bar.rs".to_string(),
            "fxxxbxxxr.rs".to_string(),
            "unrelated.txt".to_string(),
        ];
        // Subsequence matches only; the non-match is dropped, tighter ranks first.
        let ranked = fuzzy_rank("fbr", &cands);
        assert_eq!(ranked.first().map(String::as_str), Some("foo/bar.rs"));
        assert!(!ranked.iter().any(|c| c == "unrelated.txt"));
        // Empty pattern is identity (order preserved).
        assert_eq!(fuzzy_rank("", &cands), cands);
    }

    #[test]
    fn helm_parses_raw_dynamic_source() {
        let (engine, _rx) = ScriptEngine::new();
        // `(raw "fn")` candidates form → a self-filtered dynamic source.
        let result = engine
            .eval(r#"(helm (list (list "Files" (list 'raw "my-filter") "open-file")))"#)
            .unwrap();
        match result.commands.as_slice() {
            [EditorCommand::Helm(sources)] => assert!(matches!(
                &sources[0].candidates,
                HelmCandidates::DynamicRaw(f) if f == "my-filter"
            )),
            other => panic!("expected one Helm command, got {other:?}"),
        }
    }

    #[test]
    fn helm_raw_marker_does_not_capture_string_candidates() {
        let (engine, _rx) = ScriptEngine::new();
        // A static list whose first entry is the *string* "raw" stays static —
        // only a leading `raw` symbol marks a self-filtered source.
        let result = engine
            .eval(r#"(helm (list (list "S" (list "raw" "cooked") "open-file")))"#)
            .unwrap();
        match result.commands.as_slice() {
            [EditorCommand::Helm(sources)] => assert!(matches!(
                &sources[0].candidates,
                HelmCandidates::Static(c)
                    if c == &[("raw".to_string(), None), ("cooked".to_string(), None)]
            )),
            other => panic!("expected one Helm command, got {other:?}"),
        }
    }

    #[test]
    fn add_and_remove_hook_parse() {
        let (engine, _rx) = ScriptEngine::new();
        let added = engine.eval(r#"(add-hook "steel" "enable-lsp")"#).unwrap();
        assert!(matches!(
            added.commands.as_slice(),
            [EditorCommand::AddHook(m, f)] if m == "steel" && f == "enable-lsp"
        ));
        let removed = engine
            .eval(r#"(remove-hook "steel" "enable-lsp")"#)
            .unwrap();
        assert!(matches!(
            removed.commands.as_slice(),
            [EditorCommand::RemoveHook(m, f)] if m == "steel" && f == "enable-lsp"
        ));
    }

    #[test]
    fn evil_bind_command_by_identity() {
        let (engine, _rx) = ScriptEngine::new();
        engine
            .eval(include_str!("../scheme/core.scm"))
            .expect("core.scm");
        engine
            .eval(include_str!("../scheme/evil.scm"))
            .expect("evil.scm");

        // Bind by IDENTITY: a bare command procedure (a builtin here), not a
        // "name" string — so editor tooling sees a real reference.
        engine
            .eval(r#"(evil-define-key "normal" "rust" "space g l" lsp-find-references)"#)
            .expect("bind by identity");
        // The keypress evals the stored dispatcher, which calls the stored
        // procedure, which enqueues the real command.
        let cmds = engine
            .eval(r#"(evil-run-key "evil-normal:rust space g l")"#)
            .unwrap()
            .commands;
        assert!(
            cmds.iter()
                .any(|c| matches!(c, EditorCommand::LspReferences)),
            "identity binding should dispatch LspReferences: {cmds:?}"
        );

        // Back-compat: a "name" string still binds directly (no table entry).
        let cmds = engine
            .eval(r#"(evil-define-key "normal" "go" "space g l" "lsp-find-references")"#)
            .unwrap()
            .commands;
        assert!(
            cmds.iter()
                .any(|c| matches!(c, EditorCommand::DefineKey(m, s, cmd)
                    if m == "evil-normal:go" && s == "space g l" && cmd == "lsp-find-references")),
            "name-string binding should define-key directly: {cmds:?}"
        );
    }

    #[test]
    fn evil_leader_key_configurable() {
        let (engine, _rx) = ScriptEngine::new();
        engine
            .eval(include_str!("../scheme/core.scm"))
            .expect("core.scm");
        engine
            .eval(include_str!("../scheme/evil.scm"))
            .expect("evil.scm");

        // Default leader is SPC: a global leader binding lands under "space …".
        let cmds = engine
            .eval(r#"(evil-leader-set "n n" "next-buffer")"#)
            .unwrap()
            .commands;
        assert!(
            cmds.iter().any(|c| matches!(c, EditorCommand::DefineKey(m, s, cmd)
                if m == "evil-normal" && s == "space n n" && cmd == "next-buffer")),
            "default leader should expand to 'space n n': {cmds:?}"
        );

        // Changing the leader moves subsequent bindings to the new prefix; a
        // per-mode leader binding scopes to "evil-normal:<mode>".
        engine.eval(r#"(evil-set-leader "comma")"#).expect("set leader");
        let cmds = engine
            .eval(
                r#"(evil-leader-set-for-modes '("rust")
                     (list (cons "g r" "lsp-rename")))"#,
            )
            .unwrap()
            .commands;
        assert!(
            cmds.iter().any(|c| matches!(c, EditorCommand::DefineKey(m, s, cmd)
                if m == "evil-normal:rust" && s == "comma g r" && cmd == "lsp-rename")),
            "reconfigured leader should expand to 'comma g r' in rust map: {cmds:?}"
        );
    }

    #[test]
    fn evil_collection_overlays_special_modes() {
        let (engine, _rx) = ScriptEngine::new();
        engine
            .eval(include_str!("../scheme/core.scm"))
            .expect("core.scm");
        // evil.scm calls `(evil-collection-init)` at load, populating the
        // `evil-normal:<mode>` overlay keymaps for every special mode.
        let cmds = engine
            .eval(include_str!("../scheme/evil.scm"))
            .unwrap()
            .commands;
        for (mode, key, cmd) in [
            ("evil-normal:magit-status", "j", "next-line"),
            ("evil-normal:magit-status", "k", "previous-line"),
            ("evil-normal:dired-mode", "k", "previous-line"),
        ] {
            assert!(
                cmds.iter().any(|c| matches!(c, EditorCommand::DefineKey(m, s, x)
                    if m == mode && s == key && x == cmd)),
                "evil-collection-init should bind {key}->{cmd} in {mode}"
            );
        }

        // The overlay is configurable from Scheme — extend the mode list and
        // re-run, and a newly added mode gets the same motion.
        let cmds = engine
            .eval(
                r#"(begin
                     (set! *evil-collection-modes* (cons "my-mode" *evil-collection-modes*))
                     (evil-collection-init))"#,
            )
            .unwrap()
            .commands;
        assert!(
            cmds.iter().any(|c| matches!(c, EditorCommand::DefineKey(m, s, x)
                if m == "evil-normal:my-mode" && s == "j" && x == "next-line")),
            "re-running evil-collection-init should cover newly added modes"
        );
    }

    #[test]
    fn isearch_commands_dispatch() {
        let (engine, _rx) = ScriptEngine::new();
        assert!(matches!(
            engine
                .eval("(isearch-forward)")
                .unwrap()
                .commands
                .as_slice(),
            [EditorCommand::IsearchForward]
        ));
        assert!(matches!(
            engine
                .eval("(isearch-backward)")
                .unwrap()
                .commands
                .as_slice(),
            [EditorCommand::IsearchBackward]
        ));
    }

    #[test]
    fn tutorial_loads_and_opens() {
        let (engine, _rx) = ScriptEngine::new();
        engine
            .eval(include_str!("../scheme/core.scm"))
            .expect("core.scm");
        // The whole tutorial buffer text must parse as a Steel string literal.
        engine
            .eval(include_str!("../scheme/tutorial.scm"))
            .expect("tutorial.scm");
        // help-with-tutorial should build the *TUTORIAL* buffer and move point.
        let cmds = engine.eval("(help-with-tutorial)").unwrap().commands;
        assert!(
            cmds.iter().any(
                |c| matches!(c, EditorCommand::AppendToBuffer { name, .. } if name == "*TUTORIAL*")
            ),
            "help-with-tutorial should append to *TUTORIAL*: {cmds:?}"
        );
    }

    #[test]
    fn version_command_echoes_package_version() {
        let (engine, _rx) = ScriptEngine::new();
        // The `version` command's body (help.scm) over the `macros-version`
        // builtin echoes "Macros <pkg-version>" via the message command.
        let expected = format!("Macros {}", env!("CARGO_PKG_VERSION"));
        assert!(
            matches!(
                engine
                    .eval(r#"(message (string-append "Macros " (macros-version)))"#)
                    .unwrap()
                    .commands
                    .as_slice(),
                [EditorCommand::Message(m)] if *m == expected
            ),
            "version should echo {expected:?}"
        );
    }

    #[test]
    fn eval_collecting_keeps_commands_before_a_runtime_error() {
        let (engine, _rx) = ScriptEngine::new();
        // A *runtime* error in a later form must NOT discard the commands that
        // already ran — otherwise a bad call near the bottom of init.scm would
        // drop the `(evil-mode)` near the top, leaving the user in plain
        // (non-modal) editing. (Steel compiles the whole file first, so
        // compile-time errors — unbound symbols, syntax — still abort everything;
        // that's inherent and can't be salvaged here.)
        let (commands, error) = engine.eval_collecting("(evil-mode) (car '())");
        assert!(
            matches!(commands.as_slice(), [EditorCommand::EvilMode]),
            "got {commands:?}"
        );
        assert!(error.is_some(), "car of '() should surface a runtime error");
        // A clean program reports no error and still returns its commands.
        let (ok_cmds, ok_err) = engine.eval_collecting("(evil-mode)");
        assert!(matches!(ok_cmds.as_slice(), [EditorCommand::EvilMode]));
        assert!(ok_err.is_none());
    }

    #[test]
    fn evil_state_and_composite_operators() {
        let (engine, _rx) = ScriptEngine::new();
        // State commands enqueue.
        assert!(matches!(
            engine.eval("(evil-mode)").unwrap().commands.as_slice(),
            [EditorCommand::EvilMode]
        ));
        // The composite-operator pattern evil.scm uses: one Scheme fn enqueues
        // several commands, applied in order (here: `a` = append).
        engine
            .eval("(define (evil-append) (forward-char) (evil-insert-state))")
            .unwrap();
        assert!(matches!(
            engine.eval("(evil-append)").unwrap().commands.as_slice(),
            [EditorCommand::ForwardChar, EditorCommand::EvilInsertState]
        ));
    }

    #[test]
    fn evil_visual_line_commands_resolve() {
        let (engine, _rx) = ScriptEngine::new();
        // `V` enters line-wise Visual.
        assert!(matches!(
            engine
                .eval("(evil-visual-line-state)")
                .unwrap()
                .commands
                .as_slice(),
            [EditorCommand::EvilVisualLineState]
        ));
        // Blockwise insert (`I`) and append (`A`) over the selection.
        assert!(matches!(
            engine.eval("(evil-visual-insert)").unwrap().commands.as_slice(),
            [EditorCommand::EvilVisualInsert]
        ));
        assert!(matches!(
            engine.eval("(evil-visual-append)").unwrap().commands.as_slice(),
            [EditorCommand::EvilVisualAppend]
        ));
        // The line-wise delete/yank pattern from evil.scm: kill, mark line-wise,
        // return to Normal (so a following `p` pastes the vim way).
        engine
            .eval(
                "(define (evil-visual-line-delete) \
                   (kill-region) (evil-set-linewise) (evil-normal-state))",
            )
            .unwrap();
        assert!(matches!(
            engine
                .eval("(evil-visual-line-delete)")
                .unwrap()
                .commands
                .as_slice(),
            [
                EditorCommand::KillRegion,
                EditorCommand::EvilSetLinewise,
                EditorCommand::EvilNormalState
            ]
        ));
    }

    #[test]
    fn buffer_list_bridge_and_helm_buffers_app() {
        let (engine, _rx) = ScriptEngine::new();
        engine.set_buffer_names(vec!["*scratch*".to_string(), "main.scm".to_string()]);

        // Scheme can read the live buffer list.
        assert_eq!(
            engine
                .eval("(length (buffer-names))")
                .unwrap()
                .value
                .as_deref(),
            Some("2")
        );

        // `switch-to-buffer` is a command taking the chosen name.
        let switched = engine.eval(r#"(switch-to-buffer "main.scm")"#).unwrap();
        assert!(matches!(
            switched.commands.as_slice(),
            [EditorCommand::SwitchToBufferNamed(n)] if n == "main.scm"
        ));

        // The whole `helm-buffers` app (verbatim from helm.scm) composes the two
        // primitives into a Helm session — fully in Scheme.
        engine
            .eval(
                r#"(define (helm-buffers)
                     (helm (list (list "Buffers" (buffer-names) "switch-to-buffer"))))"#,
            )
            .unwrap();
        match engine.eval("(helm-buffers)").unwrap().commands.as_slice() {
            [EditorCommand::Helm(sources)] => {
                assert_eq!(sources[0].name, "Buffers");
                assert!(matches!(
                    &sources[0].candidates,
                    HelmCandidates::Static(c)
                        if c == &[("*scratch*".to_string(), None), ("main.scm".to_string(), None)]
                ));
                assert_eq!(
                    sources[0].actions,
                    [("default".to_string(), "switch-to-buffer".to_string())]
                );
            }
            other => panic!("expected one Helm command, got {other:?}"),
        }
    }

    #[test]
    fn call_action_runs_named_function_with_candidate() {
        let (engine, _rx) = ScriptEngine::new();
        // An action is just a named Steel function taking the chosen candidate.
        engine.eval("(define (act x) (message x))").unwrap();
        let commands = engine.call_action("act", Some("chosen"));
        assert!(matches!(commands.as_slice(), [EditorCommand::Message(m)] if m == "chosen"));
    }

    #[test]
    fn dispatch_delivers_outcome_off_thread() {
        use futures::StreamExt as _;
        let (engine, mut outcomes) = ScriptEngine::new();
        // Async dispatch: the engine runs on its thread and the outcome arrives
        // on the channel the editor drains (no UI-thread blocking).
        engine.dispatch(r#"(message "hi")"#, false);
        match futures::executor::block_on(outcomes.next()).unwrap() {
            ScriptOutcome::Commands { commands, message } => {
                assert!(matches!(commands.as_slice(), [EditorCommand::Message(m)] if m == "hi"));
                assert!(message.is_none());
            }
            _ => panic!("expected Commands"),
        }

        // Interactive dispatch carries the formatted value as the echo message.
        engine.dispatch("(+ 1 2)", true);
        match futures::executor::block_on(outcomes.next()).unwrap() {
            ScriptOutcome::Commands { message, .. } => assert_eq!(message.as_deref(), Some("=> 3")),
            _ => panic!("expected Commands"),
        }
    }

    #[test]
    fn dynamic_candidates_dispatch() {
        use futures::StreamExt as _;
        let (engine, mut outcomes) = ScriptEngine::new();
        engine
            .eval(r#"(define (cands p) (list (string-append p "x") "static"))"#)
            .unwrap();
        engine.dispatch_candidates(2, 7, "cands", "pre");
        match futures::executor::block_on(outcomes.next()).unwrap() {
            ScriptOutcome::Candidates {
                source,
                query_id,
                candidates,
            } => {
                assert_eq!(source, 2);
                assert_eq!(query_id, 7);
                assert_eq!(
                    candidates,
                    vec![("prex".to_string(), None), ("static".to_string(), None)]
                );
            }
            _ => panic!("expected Candidates"),
        }
    }

    #[test]
    fn dynamic_candidates_carry_annotations() {
        use futures::StreamExt as _;
        let (engine, mut outcomes) = ScriptEngine::new();
        // A candidate may be a bare string or a (candidate annotation) pair.
        engine
            .eval(r#"(define (cands p) (list (list "switch-to-buffer" "C-x b") "plain"))"#)
            .unwrap();
        engine.dispatch_candidates(0, 1, "cands", "");
        match futures::executor::block_on(outcomes.next()).unwrap() {
            ScriptOutcome::Candidates { candidates, .. } => {
                assert_eq!(
                    candidates,
                    vec![
                        ("switch-to-buffer".to_string(), Some("C-x b".to_string())),
                        ("plain".to_string(), None),
                    ]
                );
            }
            _ => panic!("expected Candidates"),
        }
    }

    #[test]
    fn capf_dispatch_returns_items_and_range() {
        use futures::StreamExt as _;
        let (engine, mut outcomes) = ScriptEngine::new();
        // A CAPF returning (start end candidate-list), candidates with annotations.
        engine
            .eval(
                r#"(define (my-capf prefix)
                     (list 3 7 (list (list "lambda" "kw") "let")))"#,
            )
            .unwrap();
        engine.dispatch_capf(vec!["my-capf".to_string()], "le", 3, 7);
        match futures::executor::block_on(outcomes.next()).unwrap() {
            ScriptOutcome::Capf { start, end, items } => {
                assert_eq!((start, end), (3, 7));
                assert_eq!(
                    items,
                    vec![
                        ("lambda".to_string(), Some("kw".to_string())),
                        ("let".to_string(), None),
                    ]
                );
            }
            _ => panic!("expected Capf"),
        }
    }

    #[test]
    fn capf_falls_through_to_next_function() {
        use futures::StreamExt as _;
        let (engine, mut outcomes) = ScriptEngine::new();
        // The first CAPF declines (empty); the second supplies a flat list and
        // keeps the default byte range.
        engine.eval(r"(define (capf-a prefix) '())").unwrap();
        engine
            .eval(r#"(define (capf-b prefix) (list "alpha" "beta"))"#)
            .unwrap();
        engine.dispatch_capf(vec!["capf-a".to_string(), "capf-b".to_string()], "a", 2, 5);
        match futures::executor::block_on(outcomes.next()).unwrap() {
            ScriptOutcome::Capf { start, end, items } => {
                assert_eq!((start, end), (2, 5));
                assert_eq!(
                    items,
                    vec![("alpha".to_string(), None), ("beta".to_string(), None)]
                );
            }
            _ => panic!("expected Capf"),
        }
    }

    #[test]
    fn builtin_fast_path_recognises_nullary_calls() {
        let (engine, _outcomes) = ScriptEngine::new();
        assert!(matches!(
            engine.builtins_for("(forward-char)").as_deref(),
            Some([EditorCommand::ForwardChar])
        ));
        assert!(matches!(
            engine.builtins_for("  (undo)  ").as_deref(),
            Some([EditorCommand::Undo])
        ));
        // A sequence of bare builtins is fast-pathed in order.
        assert!(matches!(
            engine.builtins_for("(forward-char) (undo)").as_deref(),
            Some([EditorCommand::ForwardChar, EditorCommand::Undo])
        ));
        // User functions / argument calls are NOT fast-pathed (go to the thread).
        assert!(engine.builtins_for("(my-fn)").is_none());
        assert!(engine.builtins_for(r#"(insert "x")"#).is_none());
        // A non-builtin anywhere in the sequence drops the whole thing.
        assert!(engine.builtins_for("(forward-char) (my-fn)").is_none());
        assert!(engine.builtins_for("").is_none());
    }

    #[test]
    fn eval_returns_value_and_commands() {
        let (engine, _rx) = ScriptEngine::new();

        // A pure expression yields a printable value, no commands.
        let result = engine.eval("(+ 1 2)").unwrap();
        assert_eq!(result.value.as_deref(), Some("3"));
        assert!(result.commands.is_empty());

        // A command call enqueues it and yields a void (no) value.
        let result = engine.eval(r#"(message "hi")"#).unwrap();
        assert!(matches!(result.commands.as_slice(), [EditorCommand::Message(m)] if m == "hi"));
        assert_eq!(result.value, None);
    }

    #[test]
    fn set_auto_update_maps_to_option() {
        let (engine, _rx) = ScriptEngine::new();
        // Mirrors the bundled `set-auto-update` in core.scm: a thin wrapper that
        // flips the "auto-update" option (now passed as a native boolean).
        use crate::settings::OptionValue;
        let def = r#"(define (set-auto-update enable)
                       (set-option "auto-update" enable))"#;

        let result = engine.eval(&format!("{def} (set-auto-update #f)")).unwrap();
        assert!(matches!(
            result.commands.as_slice(),
            [EditorCommand::SetOption(k, v)] if k == "auto-update" && matches!(v, OptionValue::Bool(false))
        ));

        let result = engine.eval(&format!("{def} (set-auto-update #t)")).unwrap();
        assert!(matches!(
            result.commands.as_slice(),
            [EditorCommand::SetOption(k, v)] if k == "auto-update" && matches!(v, OptionValue::Bool(true))
        ));
    }

    #[test]
    fn get_option_reads_published_values() {
        use crate::settings::OptionValue;
        let (engine, _rx) = ScriptEngine::new();
        let mut opts = std::collections::HashMap::new();
        opts.insert("font-size".to_string(), OptionValue::Int(13));
        opts.insert("completion-auto".to_string(), OptionValue::Bool(false));
        opts.insert(
            "cursor-style".to_string(),
            OptionValue::Str("bar".to_string()),
        );
        engine.set_options(opts);

        let v = |src: &str| engine.eval(src).unwrap().value;
        assert_eq!(v(r#"(get-option "font-size")"#).as_deref(), Some("13"));
        assert_eq!(
            v(r#"(get-option "completion-auto")"#).as_deref(),
            Some("#false")
        );
        assert_eq!(
            v(r#"(get-option "cursor-style")"#).as_deref(),
            Some("\"bar\"")
        );
        // An unset option reads as #f.
        assert_eq!(v(r#"(get-option "nope")"#).as_deref(), Some("#false"));
    }

    #[test]
    fn bind_key_accepts_string_and_procedure() {
        let (engine, _rx) = ScriptEngine::new();
        // bind-key lives in the bundled core.scm — load the standard library.
        for src in crate::runtime::startup_scripts() {
            engine.eval(src).unwrap();
        }

        // A string command behaves exactly like define-key.
        let r = engine
            .eval(r#"(bind-key "fundamental" "ctrl-x g" "magit-status")"#)
            .unwrap();
        assert!(matches!(
            r.commands.as_slice(),
            [EditorCommand::DefineKey(m, c, cmd)]
                if m == "fundamental" && c == "ctrl-x g" && cmd == "magit-status"
        ));

        // A procedure is stashed and the chord is bound to a run-thunk stub.
        engine
            .eval("(define *fired* #f) (define (mark!) (set! *fired* #t))")
            .unwrap();
        let r = engine
            .eval(r#"(bind-key "fundamental" "ctrl-x h" mark!)"#)
            .unwrap();
        let stub = match r.commands.as_slice() {
            [EditorCommand::DefineKey(_, _, cmd)] => cmd.clone(),
            other => panic!("expected one DefineKey, got {other:?}"),
        };
        assert!(stub.starts_with("run-thunk \"thunk-"), "stub was {stub:?}");

        // Firing the stub (as key dispatch does) runs the procedure.
        engine.eval(&format!("({stub})")).unwrap();
        assert_eq!(
            engine.eval("*fired*").unwrap().value.as_deref(),
            Some("#true")
        );
    }
}

/// Register a Steel function `name` that, when called, enqueues a clone of `cmd`.
fn register_nullary(engine: &mut Engine, queue: &Queue, name: &'static str, cmd: EditorCommand) {
    let q = queue.clone();
    engine.register_fn(name, move || q.lock().unwrap().push(cmd.clone()));
}

/// Register a one-argument Steel command: `name` enqueues the command built by
/// `make` from its argument. Collapses the `{ let q = queue.clone(); ... push }`
/// boilerplate for the many single-payload commands.
fn register_unary<T>(
    engine: &mut Engine,
    queue: &Queue,
    name: &'static str,
    make: impl Fn(T) -> EditorCommand + Send + Sync + 'static,
) where
    T: steel::rvals::FromSteelVal + 'static,
{
    let q = queue.clone();
    engine.register_fn(name, move |arg: T| q.lock().unwrap().push(make(arg)));
}

/// Register a two-argument Steel command (see [`register_unary`]).
fn register_binary<T, U>(
    engine: &mut Engine,
    queue: &Queue,
    name: &'static str,
    make: impl Fn(T, U) -> EditorCommand + Send + Sync + 'static,
) where
    T: steel::rvals::FromSteelVal + 'static,
    U: steel::rvals::FromSteelVal + 'static,
{
    let q = queue.clone();
    engine.register_fn(name, move |a: T, b: U| q.lock().unwrap().push(make(a, b)));
}

/// Register a three-argument Steel command (see [`register_unary`]).
fn register_ternary<T, U, V>(
    engine: &mut Engine,
    queue: &Queue,
    name: &'static str,
    make: impl Fn(T, U, V) -> EditorCommand + Send + Sync + 'static,
) where
    T: steel::rvals::FromSteelVal + 'static,
    U: steel::rvals::FromSteelVal + 'static,
    V: steel::rvals::FromSteelVal + 'static,
{
    let q = queue.clone();
    engine.register_fn(name, move |a: T, b: U, c: V| {
        q.lock().unwrap().push(make(a, b, c))
    });
}

/// Register a nullary Steel *read* bridge: `name` returns a value derived from
/// the editor's published snapshot. Collapses the repeated
/// `{ let shared = point.clone(); engine.register_fn(name, move || …) }`
/// boilerplate for the many single-snapshot readers. `get` runs under the
/// snapshot lock and must produce an owned value (clone out anything borrowed).
fn register_snapshot_read<T: IntoSteelVal + 'static>(
    engine: &mut Engine,
    point: &SharedSnapshot,
    name: &'static str,
    get: impl Fn(&EditorSnapshot) -> T + Send + Sync + 'static,
) {
    let shared = point.clone();
    engine.register_fn(name, move || get(&shared.load()));
}

/// Snap a byte index down to the nearest UTF-8 char boundary in `rope`, clamped
/// to its length. Slicing a `Rope` panics on a non-boundary index, so the
/// text-slicing bridges route caller-supplied offsets through this first — the
/// boundary safety the old `String`-slicing bridges did by hand.
fn snap_byte(rope: &Rope, byte: usize) -> usize {
    let byte = byte.min(rope.len_bytes());
    rope.char_to_byte(rope.byte_to_char(byte))
}

#[cfg(test)]
mod magit_tests {
    use super::*;

    /// Every embedded startup script loads, in real startup order, through one
    /// engine — catches cross-file identifier/ordering bugs and missing bridges.
    #[test]
    fn all_startup_scripts_load() {
        let (engine, _rx) = ScriptEngine::new();
        for src in crate::runtime::startup_scripts() {
            engine.eval(src).expect("a startup script failed to load");
        }
        // Every user-facing entry point is M-x discoverable (registered for the
        // helm-execute-command list). Guards against a lib forgetting to register.
        for cmd in [
            "magit-status",
            "magit",
            "dired",
            "shell",
            "compile",
            "occur",
            "comment-dwim",
            "replace-string",
            "helm-buffers",
            "helm-find-files",
            "helm-magit",
            "projectile-find-file",
            "projectile-grep",
            "projectile-switch-project",
            "sort-lines",
            "goto-line-cmd",
            "licenses",
        ] {
            let q = format!(r#"(if (member "{cmd}" *interactive-commands*) "y" "n")"#);
            assert_eq!(
                engine.eval(&q).unwrap().value.as_deref(),
                Some("\"y\""),
                "{cmd} is not registered for M-x"
            );
        }
    }

    /// `package-install`'s URL→name derivation and quoted-symbol option parsing.
    /// The doc index powers discoverability: built-ins carry a source location,
    /// `index-doc-file` folds a freshly loaded package's documented defines in,
    /// and `helm-apropos`'s live filter matches on doc text — not just names.
    #[test]
    fn doc_index_locates_builtins_and_indexes_packages() {
        let (engine, _rx) = ScriptEngine::new();
        for src in crate::runtime::startup_scripts() {
            engine.eval(src).expect("startup script failed to load");
        }
        let v = |e: &str| engine.eval(e).unwrap().value.unwrap_or_default();

        // A built-in resolves to its source file + a 1-based line.
        assert_eq!(v(r#"(null? (function-source "save-excursion"))"#), "#false");
        assert_eq!(
            v(r#"(ends-with? (car (function-source "save-excursion")) ".scm")"#),
            "#true"
        );
        assert_eq!(
            v(r#"(> (cadr (function-source "save-excursion")) 0)"#),
            "#true"
        );
        // An unknown name has no location.
        assert_eq!(v(r#"(null? (function-source "no-such-fn-xyz"))"#), "#true");

        // A package file on disk: index it, then its commented define is both
        // documented and locatable — exactly what pkg--load does at load time.
        let dir = std::env::temp_dir().join(format!("macros-docidx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("pkg.scm");
        std::fs::write(
            &file,
            ";; Set the editor font size to N points.\n(define (pkg-font-size n) n)\n",
        )
        .unwrap();
        let path = file.to_string_lossy().replace('"', "\\\"");

        // Before indexing: unknown.
        assert_eq!(v(r#"(function-doc "pkg-font-size")"#), "\"\"");

        engine
            .eval(&format!(r#"(index-doc-file "{path}")"#))
            .unwrap();

        // After indexing: doc + source location both present.
        assert_eq!(
            v(r#"(function-doc "pkg-font-size")"#),
            "\"Set the editor font size to N points.\""
        );
        assert_eq!(
            v(&format!(
                r#"(equal? (car (function-source "pkg-font-size")) "{path}")"#
            )),
            "#true"
        );

        // The live picker filters on doc text: searching "font size" surfaces the
        // package fn even though its NAME doesn't contain that phrase. Candidates
        // are `(name summary)` pairs, so map to the bare name before checking.
        assert_eq!(
            v(r#"(if (member "pkg-font-size"
                            (map (lambda (c) (if (list? c) (car c) c))
                                 (helm-apropos-candidates "font size")))
                     #t #f)"#),
            "#true"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Matching is multi-word (order-independent), tolerates a plural query
    /// word, and options are in the index — so the natural "font size" query
    /// finds the `font-size` option even though it is not a `(define …)`.
    #[test]
    fn helm_apropos_matching_is_multiword_and_covers_options() {
        let (engine, _rx) = ScriptEngine::new();
        for src in crate::runtime::startup_scripts() {
            engine.eval(src).expect("startup script failed to load");
        }
        // #t iff `name` is among the candidate names for `query`.
        let finds = |query: &str, name: &str| {
            engine
                .eval(&format!(
                    r#"(if (member "{name}"
                                  (map (lambda (c) (if (list? c) (car c) c))
                                       (helm-apropos-candidates "{query}")))
                          #t #f)"#
                ))
                .unwrap()
                .value
                .unwrap()
        };

        // Options are searchable, and the natural query resolves to the key.
        assert_eq!(
            engine
                .eval(r#"(if (member "font-size" (option-list)) #t #f)"#)
                .unwrap()
                .value
                .unwrap(),
            "#true"
        );
        assert_eq!(finds("font size", "font-size"), "#true");
        // Word order doesn't matter.
        assert_eq!(finds("size font", "font-size"), "#true");
        // A plural query word still finds the singular in the key.
        assert_eq!(finds("font sizes", "font-size"), "#true");
        // A word that appears in neither name nor doc excludes the candidate.
        assert_eq!(finds("font orange", "font-size"), "#false");
    }

    #[test]
    fn package_url_and_option_parsing() {
        let (engine, _rx) = ScriptEngine::new();
        for src in crate::runtime::startup_scripts() {
            engine.eval(src).expect("startup script failed to load");
        }
        let v = |e: &str| engine.eval(e).unwrap().value.unwrap_or_default();
        // The leaf segment, with any trailing slash and ".git" stripped.
        assert_eq!(
            v(r#"(pkg--name-from-url "https://github.com/u/macros-foo.git")"#),
            "\"macros-foo\""
        );
        assert_eq!(
            v(r#"(pkg--name-from-url "https://github.com/u/macros-foo")"#),
            "\"macros-foo\""
        );
        assert_eq!(
            v(r#"(pkg--name-from-url "https://github.com/u/macros-foo/")"#),
            "\"macros-foo\""
        );
        // Option lookup over the quoted-symbol plist.
        assert_eq!(
            v(r#"(pkg--opt (list 'ref "v1" 'entry "x.scm") 'entry)"#),
            "\"x.scm\""
        );
        // A missing option is #f (test via a branch — bool repr aside).
        assert_eq!(
            v(r#"(if (pkg--opt (list 'ref "v1") 'entry) "y" "n")"#),
            "\"n\""
        );
    }

    /// End-to-end: clone a local git repo with `pkg--git`, then `pkg--load` its
    /// entry — the entry's definitions land in the engine and the package is
    /// marked loaded (so `'depends` can see it). Exercises the git shell-out and
    /// the error-isolated load path. Skips cleanly if `git` isn't on PATH.
    #[test]
    fn package_clone_and_load() {
        if std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_err()
        {
            return; // no git in this environment
        }
        let root = std::env::temp_dir().join(format!("macros-pkg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("macros-demo.scm"), "(define demo-loaded 42)\n").unwrap();
        // A repo with one commit (so the clone carries the entry file). Pass the
        // identity inline so it works even where git has no global user config.
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&src)
                .output()
                .unwrap()
        };
        git(&["init", "--quiet"]);
        git(&["-c", "user.email=t@t", "-c", "user.name=t", "add", "."]);
        git(&[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "--quiet",
            "-m",
            "x",
        ]);

        let dest = root.join("packages").join("macros-demo");
        let (engine, _rx) = ScriptEngine::new();
        for s in crate::runtime::startup_scripts() {
            engine.eval(s).unwrap();
        }

        // Clone, then confirm the working tree arrived.
        let clone = format!(
            r#"(pkg--git (list "clone" "--quiet" "{}" "{}") "clone")"#,
            src.display(),
            dest.display()
        );
        engine.eval(&clone).unwrap();
        assert!(
            dest.join("macros-demo.scm").exists(),
            "clone did not produce the entry file"
        );

        // Load the entry; its define becomes a live binding and it's marked loaded.
        let load = format!(
            r#"(pkg--load "macros-demo" "{}" "macros-demo.scm" #f)"#,
            dest.display()
        );
        engine.eval(&load).unwrap();
        assert_eq!(
            engine.eval("demo-loaded").unwrap().value.as_deref(),
            Some("42")
        );
        assert_eq!(
            engine
                .eval(r#"(if (package-loaded? "macros-demo") "y" "n")"#)
                .unwrap()
                .value
                .as_deref(),
            Some("\"y\"")
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The `include` helper (over Steel's `load`) evaluates another file in the
    /// same engine, so its `define-key` lands as a real command.
    #[test]
    fn include_loads_relative_file() {
        let dir = std::env::temp_dir().join(format!("macros-include-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("keys.scm"),
            r#"(define-key "fundamental" "ctrl-z" "undo")"#,
        )
        .unwrap();

        let (engine, _rx) = ScriptEngine::new();
        // The same prelude main.rs injects before init.scm.
        engine.eval(&crate::runtime::include_prelude(&dir)).unwrap();
        let result = engine.eval(r#"(include "keys.scm")"#).unwrap();
        assert!(
            matches!(
                result.commands.as_slice(),
                [EditorCommand::DefineKey(mode, chord, cmd)]
                    if mode == "fundamental" && chord == "ctrl-z" && cmd == "undo"
            ),
            "include should run keys.scm's define-key, got {:?}",
            result.commands
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `call-process` runs a subprocess on the engine thread and returns stdout.
    #[test]
    fn call_process_returns_stdout() {
        let (engine, _rx) = ScriptEngine::new();
        // Compare via a Steel-side trim — the printed repr escapes the trailing
        // newline, so assert on content, not the debug display.
        let out = engine
            .eval(r#"(trim-end (call-process "echo" (list "hello" "world")))"#)
            .unwrap();
        assert_eq!(out.value.as_deref(), Some("\"hello world\""));
    }

    /// `setenv` mutates the process env and `getenv` reads it back — the bridge
    /// config uses to extend PATH for subprocess spawns.
    #[test]
    fn setenv_then_getenv_roundtrips() {
        let (engine, _rx) = ScriptEngine::new();
        let out = engine
            .eval(r#"(begin (setenv "MACROS_TEST_VAR" "abc/123") (getenv "MACROS_TEST_VAR"))"#)
            .unwrap();
        assert_eq!(out.value.as_deref(), Some("\"abc/123\""));
    }

    /// `(render-buffer name mode text faces)` enqueues a `RenderBuffer` with the
    /// `(line face)` pairs parsed into `line_faces`.
    #[test]
    fn render_buffer_parses_faces() {
        let (engine, _rx) = ScriptEngine::new();
        let result = engine
            .eval(
                r#"(render-buffer "*magit*" "magit-status" "a\nb"
                       (list (list 0 "magit-section-heading") (list 1 "magit-file")))"#,
            )
            .unwrap();
        match result.commands.as_slice() {
            [
                EditorCommand::RenderBuffer {
                    name,
                    mode,
                    text,
                    line_faces,
                },
            ] => {
                assert_eq!(name, "*magit*");
                assert_eq!(mode, "magit-status");
                assert_eq!(text, "a\nb");
                assert_eq!(
                    line_faces,
                    &[
                        (0, "magit-section-heading".to_string()),
                        (1, "magit-file".to_string())
                    ]
                );
            }
            other => panic!("expected one RenderBuffer, got {other:?}"),
        }
    }

    /// Overlay / fold / text-property primitives parse to their commands.
    #[test]
    fn overlay_primitives_parse() {
        let (engine, _rx) = ScriptEngine::new();
        match engine
            .eval(r#"(overlay-set-face 3 8 "diff-added")"#)
            .unwrap()
            .commands
            .as_slice()
        {
            [EditorCommand::OverlayFace { start, end, face }] => {
                assert_eq!((*start, *end, face.as_str()), (3, 8, "diff-added"));
            }
            other => panic!("expected OverlayFace, got {other:?}"),
        }
        assert!(matches!(
            engine.eval("(fold-lines 2 5)").unwrap().commands.as_slice(),
            [EditorCommand::FoldLines {
                start_line: 2,
                end_line: 5
            }]
        ));
        assert!(matches!(
            engine.eval("(unfold-all)").unwrap().commands.as_slice(),
            [EditorCommand::UnfoldAll]
        ));
        assert!(matches!(
            engine
                .eval("(set-fold-marker 3 #t)")
                .unwrap()
                .commands
                .as_slice(),
            [EditorCommand::SetFoldMarker {
                line: 3,
                open: true
            }]
        ));
        assert!(matches!(
            engine
                .eval("(clear-fold-markers)")
                .unwrap()
                .commands
                .as_slice(),
            [EditorCommand::ClearFoldMarkers]
        ));
        match engine
            .eval(r#"(put-text-property 0 4 "section" "staged")"#)
            .unwrap()
            .commands
            .as_slice()
        {
            [
                EditorCommand::PutTextProperty {
                    start,
                    end,
                    key,
                    value,
                },
            ] => {
                assert_eq!(
                    (*start, *end, key.as_str(), value.as_str()),
                    (0, 4, "section", "staged")
                );
            }
            other => panic!("expected PutTextProperty, got {other:?}"),
        }
    }

    /// Keymap introspection (help/discoverability) + the M-x execute mechanism.
    #[test]
    fn keymap_introspection_and_mx() {
        let (engine, _rx) = ScriptEngine::new();
        let v = |q: &str| engine.eval(q).unwrap().value.unwrap();
        // command-list exposes the built-ins.
        assert_eq!(
            v(r#"(if (member "forward-char" (command-list)) "yes" "no")"#),
            "\"yes\""
        );
        // define-key populates the shadow keymap → key-binding reads it back.
        engine
            .eval(r#"(define-key "fundamental" "ctrl-z" "undo")"#)
            .unwrap();
        assert_eq!(v(r#"(key-binding "fundamental" "ctrl-z")"#), "\"undo\"");
        assert_eq!(
            v(r#"(key-binding "fundamental" "ctrl-q")"#),
            "\"\"",
            "unbound = empty"
        );
        // A later definition wins (matches the real keymap).
        engine
            .eval(r#"(define-key "fundamental" "ctrl-z" "redo")"#)
            .unwrap();
        assert_eq!(v(r#"(key-binding "fundamental" "ctrl-z")"#), "\"redo\"");
        // eval-string is what M-x's execute-command runs.
        assert_eq!(v(r#"(eval-string "(+ 1 2)")"#), "3");
    }

    /// The editing lib's pure helpers.
    #[test]
    fn editing_lib_helpers() {
        let (engine, _rx) = ScriptEngine::new();
        engine.eval(include_str!("../scheme/core.scm")).unwrap();
        engine.eval(include_str!("../scheme/editing.scm")).unwrap();
        let v = |q: &str| engine.eval(q).unwrap().value.unwrap();
        assert_eq!(v(r#"(car (sort-strings (list "c" "a" "b")))"#), "\"a\"");
        assert_eq!(v(r#"(capitalize-token "hELLO")"#), "\"Hello\"");
        assert_eq!(v(r#"(string-join (list "a" "b" "c") "-")"#), "\"a-b-c\"");
        // sort-lines registered itself for M-x.
        assert_eq!(
            v(r#"(if (member "sort-lines" *interactive-commands*) "yes" "no")"#),
            "\"yes\""
        );
    }

    /// The Org library loads (top-level set-face/define-key/hooks all resolve)
    /// and its pure parser / exporter / table helpers behave.
    #[test]
    fn org_lib_pure_functions() {
        let (engine, _rx) = ScriptEngine::new();
        engine
            .eval(include_str!("../scheme/core.scm"))
            .expect("core.scm");
        // org.scm registers its chevron handler with the folding registry.
        engine
            .eval(include_str!("../scheme/folding.scm"))
            .expect("folding.scm");
        engine
            .eval(include_str!("../scheme/org.scm"))
            .expect("org.scm");
        let v = |q: &str| engine.eval(q).unwrap().value.unwrap();

        // Heading detection + TODO cycle.
        assert_eq!(v(r#"(org--heading-level "** TODO x")"#), "2");
        assert_eq!(v(r#"(if (org--heading-level "plain line") 1 0)"#), "0");
        // C-c C-t rotates the full cycle: nothing → first keyword → … → "".
        assert_eq!(
            v(r#"(org--next-keyword #f (org--cycle-keywords))"#),
            "\"TODO\""
        );
        assert_eq!(
            v(r#"(org--next-keyword "TODO" (org--cycle-keywords))"#),
            "\"NEXT\""
        );
        assert_eq!(
            v(r#"(org--next-keyword "CANCELLED" (org--cycle-keywords))"#),
            "\"\""
        ); // last → clear
        // Custom keywords are user-defined: the built-in ships none, so a word is
        // recognized only after the user adds it to *org-custom-keywords*.
        assert_eq!(v(r#"(length *org-custom-keywords*)"#), "0");
        assert_eq!(v(r#"(if (org--custom-keyword? "BUG") 1 0)"#), "0");
        engine
            .eval(r#"(set! *org-custom-keywords* (list "BUG" "FEATURE"))"#)
            .unwrap();
        // Now recognized for fontification/parsing AND folded into the C-c C-t
        // cycle, after the TODO vocabulary: …→CANCELLED→BUG→FEATURE→"".
        assert_eq!(v(r#"(if (org--custom-keyword? "BUG") 1 0)"#), "1");
        assert_eq!(v(r#"(if (org--custom-keyword? "TODO") 1 0)"#), "0");
        assert_eq!(
            v(r#"(org--leading-keyword "BUG crash on save")"#),
            "\"BUG\""
        );
        assert_eq!(
            v(r#"(nth (car (org-parse "* FEATURE dark mode")) 2)"#),
            "\"FEATURE\""
        );
        assert_eq!(
            v(r#"(org--next-keyword "CANCELLED" (org--cycle-keywords))"#),
            "\"BUG\""
        );
        assert_eq!(
            v(r#"(org--next-keyword "BUG" (org--cycle-keywords))"#),
            "\"FEATURE\""
        );
        assert_eq!(
            v(r#"(org--next-keyword "FEATURE" (org--cycle-keywords))"#),
            "\"\""
        ); // last → clear
        // A custom keyword has no color (inherits the title) until the user sets
        // one, then re-registers the faces.
        assert_eq!(v(r#"(if (org--keyword-color "BUG") 1 0)"#), "0");
        assert_eq!(v(r#"(org--keyword-face-name "BUG")"#), "\"org-kw-BUG\"");
        engine
            .eval(r##"(set! *org-custom-keyword-colors* (list (cons "BUG" "#ff0000")))"##)
            .unwrap();
        engine
            .eval(r#"(org--register-custom-keyword-faces)"#)
            .unwrap();
        assert_eq!(v(r#"(org--keyword-color "BUG")"#), "\"#ff0000\"");
        assert_eq!(v(r#"(if (org--keyword-color "FEATURE") 1 0)"#), "0"); // still uncolored

        // Parser: a heading + a paragraph; heading fields are extracted.
        assert_eq!(v(r#"(length (org-parse "* A\nbody text\n"))"#), "2");
        assert_eq!(
            v(r#"(car (car (org-parse "* TODO Buy milk :home:")))"#),
            "\"heading\""
        );
        assert_eq!(
            v(r#"(nth (car (org-parse "* TODO Buy milk :home:")) 2)"#),
            "\"TODO\""
        );
        assert_eq!(
            v(r#"(nth (car (org-parse "* TODO Buy milk :home:")) 4)"#),
            "\"Buy milk\""
        );
        assert_eq!(
            v(r#"(nth (car (org-parse "* TODO Buy milk :home:")) 5)"#),
            "\"home\""
        );

        // Export smoke: a heading becomes an <h1>.
        assert_eq!(
            v(r#"(if (string-contains? (org--ast->html (org-parse "* Hi")) "<h1>") 1 0)"#),
            "1"
        );

        // Table align pads each column to its widest cell.
        assert_eq!(
            v(r#"(car (org--format-table (list "| a | bb |" "| ccc | d |")))"#),
            "\"| a   | bb |\""
        );

        // Inline-image link detection: only image-typed, undescribed links.
        assert_eq!(
            v(r#"(if (org--image-link-path "[[file:diagram.png]]") 1 0)"#),
            "1"
        );
        assert_eq!(
            v(r#"(if (org--image-link-path "[[notes.org][see]]") 1 0)"#),
            "0"
        );
        assert_eq!(v(r#"(if (org--image-link-path "plain text") 1 0)"#), "0");
        // Absolute paths pass through; relative ones resolve against the buffer dir.
        assert_eq!(
            v(r#"(org--image-link-path "[[/tmp/a.PNG]]")"#),
            "\"/tmp/a.PNG\""
        );

        // LaTeX fragment extraction: inner math of the first $…$.
        assert_eq!(
            v(r#"(org--latex-fragment "before $x^2 + 1$ after")"#),
            "\"x^2 + 1\""
        );
        assert_eq!(v(r#"(if (org--latex-fragment "no math here") 1 0)"#), "0");

        // Interactive-table helpers. Pipe byte positions and cell-at-column.
        assert_eq!(v(r#"(org--pipe-positions "| a | bb |")"#), "(0 4 9)");
        assert_eq!(v(r#"(org--cell-index-at "| a | bb |" 2)"#), "0"); // in 'a'
        assert_eq!(v(r#"(org--cell-index-at "| a | bb |" 6)"#), "1"); // in 'bb'
        assert_eq!(v(r#"(org--cell-index-at "| a | bb |" 0)"#), "0"); // before 1st pipe
        // Column count and padding to a fixed width.
        assert_eq!(v(r#"(org--table-ncols (list "| a | b |" "| c |"))"#), "2");
        assert_eq!(v(r#"(org--cells-pad "| x |" 3)"#), "(\"x\" \"\" \"\")");
        // List swap and row rebuild from cells.
        assert_eq!(
            v(r#"(org--list-swap (list "a" "b" "c") 0 2)"#),
            "(\"c\" \"b\" \"a\")"
        );
        assert_eq!(
            v(r#"(org--row-from-cells (list "a" "bb"))"#),
            "\"| a | bb |\""
        );
        // A column swap reformats and aligns: cols of "a"/"bb" become "bb"/"a".
        assert_eq!(
            v(r#"(car (org--format-table
                        (list (org--row-from-cells
                                (org--list-swap (org--cells-pad "| a | bb |" 2) 0 1)))))"#),
            "\"| bb | a |\"",
        );
        // Empty-row generator: n pipes parses back to n-1 empty cells.
        assert_eq!(v(r"(length (org--table-cells (org--pipes 4)))"), "3");
        // Byte prefix sums lengths + newlines.
        assert_eq!(v(r#"(org--rows-byte-prefix (list "ab" "cde") 1)"#), "3");

        // Three-state fold ranges. Outline: 0:* A 1:body 2:** B 3:body 4:** C 5:* D
        engine
            .eval(r"(define HS (list (list 0 1) (list 2 2) (list 4 2) (list 5 1)))")
            .unwrap();
        // A's subtree is lines 1..4; its direct children are the level-2 B and C.
        assert_eq!(v(r"(org--min-child-level HS 0 4)"), "2");
        assert_eq!(v(r"(org--direct-children HS 0 4)"), "(2 4)");
        // FOLDED hides the whole subtree (1..4).
        assert_eq!(
            v(r"(car (org--fold-ranges-for HS 6 0 (quote folded)))"),
            "(1 . 4)"
        );
        // CHILDREN hides A's own body (1..1) and each child's body; child C (line 4)
        // has none, so that degenerate range is dropped by the reapply guard.
        assert_eq!(
            v(r"(length (org--fold-ranges-for HS 6 0 (quote children)))"),
            "3"
        );
        assert_eq!(
            v(r"(car (org--fold-ranges-for HS 6 0 (quote children)))"),
            "(1 . 1)"
        );
        // A leaf heading (no body) folds to nothing.
        assert_eq!(
            v(r"(length (org--fold-ranges-for HS 6 5 (quote folded)))"),
            "0"
        );

        // --- Layer 7: dates -------------------------------------------------
        assert_eq!(v(r"(org--ymd->days 1970 1 1)"), "0");
        assert_eq!(v(r"(org--days->ymd 0)"), "(1970 1 1)");
        assert_eq!(v(r#"(org--dow-name 0)"#), "\"Thu\"");
        assert_eq!(
            v(r#"(org--days->date-str (org--ymd->days 2026 6 25))"#),
            "\"2026-06-25 Thu\""
        );
        assert_eq!(v(r#"(org--read-date "+1w" 10)"#), "17");
        assert_eq!(v(r#"(org--read-date "today" 42)"#), "42");
        assert_eq!(v(r#"(org--apply-offset "+2d" 0)"#), "2");
        // Repeater shift on a "+" stamp advances from the stamp's own date.
        assert_eq!(
            v(r#"(org--shift-stamp "<2026-06-25 Thu +1d>")"#),
            "\"<2026-06-26 Fri +1d>\""
        );

        // --- Layer 7b: property drawers -------------------------------------
        assert_eq!(
            v(
                r#"(cdr (org--assoc-ci (org--drawer-props (list ":PROPERTIES:" ":ID: abc" ":END:") 0 2) "id"))"#
            ),
            "\"abc\""
        );
        assert_eq!(
            v(r#"(car (org--render-drawer (list (cons "ID" "x"))))"#),
            "\":PROPERTIES:\""
        );

        // --- Layer 7d: clock duration --------------------------------------
        assert_eq!(v(r#"(org--fmt-duration 90)"#), "\"1:30\"");
        assert_eq!(v(r#"(org--stamp-minutes "[1970-01-01 Thu 01:30]")"#), "90");

        // --- Layer 7f: capture templates -----------------------------------
        assert_eq!(
            v(r#"(org--expand-template-final "* TODO %?" "buy milk")"#),
            "\"* TODO buy milk\""
        );

        // --- Layer 7g: tag matching ----------------------------------------
        assert_eq!(v(r#"(if (org--tag-match "a+b" (list "a" "b")) 1 0)"#), "1");
        assert_eq!(v(r#"(if (org--tag-match "a+b" (list "a")) 1 0)"#), "0");
        assert_eq!(v(r#"(if (org--tag-match "a|c" (list "c")) 1 0)"#), "1");
        assert_eq!(v(r#"(if (org--tag-match "-x" (list "a")) 1 0)"#), "1");
        assert_eq!(v(r#"(if (org--tag-match "-x" (list "x")) 1 0)"#), "0");

        // --- Layer 7h/6: markup export -------------------------------------
        assert_eq!(
            v(r#"(if (string-contains? (org--inline->html "+x+") "<del>") 1 0)"#),
            "1"
        );

        // --- Layer 7i: table formula evaluation ----------------------------
        assert_eq!(v(r#"(org--eval-arith "1+2*3")"#), "7");
        assert_eq!(v(r#"(org--eval-arith "(1+2)*3")"#), "9");
        assert_eq!(v(r#"(org--fmt-num (org--eval-arith "10/4"))"#), "\"2.5\"");
        assert_eq!(v(r#"(org--fmt-num (org--eval-arith "2+3"))"#), "\"5\"");
        assert_eq!(v(r#"(org--fmt-num (org--eval-arith "-3+1"))"#), "\"-2\"");
        assert_eq!(
            v(r#"(org--row-set (list "1" "2") 3 "9")"#),
            "(\"1\" \"2\" \"9\")"
        );
        assert_eq!(
            v(r#"(nth (car (org--apply-formula (list (list "1" "2" "")) "$3=$1+$2")) 2)"#),
            "\"3\""
        );

        // --- Layer 7j: export backends -------------------------------------
        assert_eq!(
            v(r##"(if (string-contains? (org--ast->md (org-parse "* Hi")) "# Hi") 1 0)"##),
            "1"
        );
        assert_eq!(v(r#"(org--inline->md "*x*")"#), "\"**x**\"");
        assert_eq!(
            v(
                r#"(if (string-contains? (org--ast->latex (org-parse "* Hi")) "\\section{Hi}") 1 0)"#
            ),
            "1"
        );

        // --- Layer 7k/l: columns + list-item prefix ------------------------
        assert_eq!(
            v(r#"(cdr (car (org--parse-columns "%40ITEM %TODO")))"#),
            "\"ITEM\""
        );
        assert_eq!(
            v(r#"(car (car (org--parse-columns "%40ITEM %TODO")))"#),
            "40"
        );
        assert_eq!(v(r#"(if (org--list-line-prefix "- item") 1 0)"#), "1");
        assert_eq!(v(r#"(if (org--list-line-prefix "plain") 1 0)"#), "0");
    }

    /// org-roam.scm loads on top of core + org and its new pure helpers behave.
    /// (Its references to org.scm functions are all in call position, so loading
    /// it after org.scm resolves them.)
    #[test]
    fn org_roam_extended_helpers() {
        let (engine, _rx) = ScriptEngine::new();
        engine
            .eval(include_str!("../scheme/core.scm"))
            .expect("core.scm");
        engine
            .eval(include_str!("../scheme/folding.scm"))
            .expect("folding.scm");
        engine
            .eval(include_str!("../scheme/org.scm"))
            .expect("org.scm");
        engine
            .eval(include_str!("../scheme/org-roam.scm"))
            .expect("org-roam.scm");
        let v = |q: &str| engine.eval(q).unwrap().value.unwrap();

        // Slug + tag splitting.
        assert_eq!(v(r#"(org-roam--slug "Hello, World!")"#), "\"hello-world\"");
        assert_eq!(v(r#"(org-roam--split-tags ":a:b:")"#), "(\"a\" \"b\")");
        // Alias/ref value parsing: quoted tokens, else whitespace split.
        assert_eq!(
            v(r#"(org-roam--parse-quoted-list "\"alpha one\" \"beta\"")"#),
            "(\"alpha one\" \"beta\")"
        );
        assert_eq!(
            v(r#"(org-roam--parse-quoted-list "alpha beta")"#),
            "(\"alpha\" \"beta\")"
        );
        // file-meta reads :KEY: front matter and #+KEY: keywords.
        assert_eq!(
            v(r#"(org-roam--file-meta ":ROAM_REFS: https://x.test" "ROAM_REFS")"#),
            "\"https://x.test\""
        );
        // Template expansion fills ${title}/${slug}/${id}.
        assert_eq!(
            v(r#"(org-roam--expand-tmpl "${slug}.org" "My Note" "my-note" "ID1")"#),
            "\"my-note.org\""
        );
        // Daily date arithmetic (self-contained calendar).
        assert_eq!(
            v(r#"(org-roam--date-str (+ 1 (org-roam--date-str->days "2026-06-25")))"#),
            "\"2026-06-26\""
        );
        // Link-description rewrite for rename.
        assert_eq!(
            v(r#"(org-roam--rewrite-links "see [[id:ID1][Old]] end" "ID1" "New")"#),
            "\"see [[id:ID1][New]] end\""
        );
    }

    /// folding.scm's indentation model: which lines are fold headers and how far
    /// each block extends. The snapshot-backed line readers are shadowed with a
    /// pure fixture so the algorithm is exercised without a live buffer.
    ///
    /// Fixture (indent per line, -1 = blank):
    ///   0 `def foo():`   header → body lines 1..2
    ///   1     `a`        4
    ///   2     `b`        4
    ///   3     (blank)   -1
    ///   4 `def bar():`   header → body line 5
    ///   5     `c`        4
    ///   6 `x`            0  not a header (nothing deeper follows)
    #[test]
    fn folding_indentation_model() {
        let (engine, _rx) = ScriptEngine::new();
        engine
            .eval(include_str!("../scheme/core.scm"))
            .expect("core.scm");
        // Define the line readers as pure Scheme over a fixture *before* loading
        // folding.scm, so its references bind to these rather than the native
        // snapshot bridges (a live buffer would otherwise be needed).
        engine
            .eval(
                r"(define *fix* (list 0 4 4 -1 0 4 0))
                  (define (line-count) (length *fix*))
                  (define (line-indentation i) (nth *fix* i))",
            )
            .expect("fixture");
        engine
            .eval(include_str!("../scheme/folding.scm"))
            .expect("folding.scm");
        let v = |q: &str| engine.eval(q).unwrap().value.unwrap();
        // Header detection.
        assert_eq!(v("(if (fold--header-indent 0 7) 1 0)"), "1");
        assert_eq!(v("(if (fold--header-indent 4 7) 1 0)"), "1");
        assert_eq!(v("(if (fold--header-indent 1 7) 1 0)"), "0"); // peer line
        assert_eq!(v("(if (fold--header-indent 3 7) 1 0)"), "0"); // blank
        assert_eq!(v("(if (fold--header-indent 6 7) 1 0)"), "0"); // nothing under
        // Block extent (inclusive last body line; trailing blank not absorbed).
        assert_eq!(v("(fold--block-end 0 0 7)"), "2");
        assert_eq!(v("(fold--block-end 4 0 7)"), "5");

        // A toggle emits the right chevron states: folding line 0 hides its body
        // (FoldLines 1..3) and flips marker 0 to closed (open=false), while the
        // untouched header 4 stays open. Toggling back reopens it.
        let fold = engine.eval("(fold-toggle-at-line 0)").unwrap().commands;
        assert!(
            fold.iter().any(|c| matches!(
                c,
                EditorCommand::SetFoldMarker {
                    line: 0,
                    open: false
                }
            )),
            "fold should close marker 0: {fold:?}"
        );
        assert!(fold.iter().any(|c| matches!(
            c,
            EditorCommand::SetFoldMarker {
                line: 4,
                open: true
            }
        )));
        assert!(fold.iter().any(|c| matches!(
            c,
            EditorCommand::FoldLines {
                start_line: 1,
                end_line: 3
            }
        )));
        let unfold = engine.eval("(fold-toggle-at-line 0)").unwrap().commands;
        assert!(
            unfold.iter().any(|c| matches!(
                c,
                EditorCommand::SetFoldMarker {
                    line: 0,
                    open: true
                }
            )),
            "unfold should reopen marker 0: {unfold:?}"
        );
        assert!(
            !unfold
                .iter()
                .any(|c| matches!(c, EditorCommand::FoldLines { .. })),
            "unfold should not re-hide anything: {unfold:?}"
        );
    }

    /// `set-line-image`'s optional width arg: omitted → 0 (default cap); given →
    /// passed through. Exercises the core.scm wrapper over the `-impl` primitive.
    #[test]
    fn set_line_image_optional_width() {
        let (engine, _rx) = ScriptEngine::new();
        engine
            .eval(include_str!("../scheme/core.scm"))
            .expect("core.scm");
        assert!(matches!(
            engine.eval(r#"(set-line-image 1 "/x.png")"#).unwrap().commands.as_slice(),
            [EditorCommand::SetLineImage { line: 1, path, max_width: 0 }] if path == "/x.png"
        ));
        assert!(matches!(
            engine
                .eval(r#"(set-line-image 1 "/x.png" 800)"#)
                .unwrap()
                .commands
                .as_slice(),
            [EditorCommand::SetLineImage {
                line: 1,
                max_width: 800,
                ..
            }]
        ));
    }

    /// The virtual-indent primitives enqueue the right commands.
    #[test]
    fn set_line_indent_primitive() {
        let (engine, _rx) = ScriptEngine::new();
        assert!(matches!(
            engine
                .eval(r"(set-line-indent 3 4)")
                .unwrap()
                .commands
                .as_slice(),
            [EditorCommand::SetLineIndent { line: 3, cols: 4 }]
        ));
        assert!(matches!(
            engine
                .eval(r"(clear-line-indents)")
                .unwrap()
                .commands
                .as_slice(),
            [EditorCommand::ClearLineIndents]
        ));
    }

    /// The per-line scale primitives enqueue the right commands.
    #[test]
    fn set_line_scale_primitive() {
        let (engine, _rx) = ScriptEngine::new();
        assert!(matches!(
            engine
                .eval(r"(set-line-scale 2 1.6)")
                .unwrap()
                .commands
                .as_slice(),
            [EditorCommand::SetLineScale { line: 2, scale }] if (*scale - 1.6).abs() < 1e-4
        ));
        assert!(matches!(
            engine
                .eval(r"(clear-line-scales)")
                .unwrap()
                .commands
                .as_slice(),
            [EditorCommand::ClearLineScales]
        ));
    }

    /// The markdown library loads and its heading detection (which drives the
    /// marker-hiding and heading-scale fontify pass) is correct.
    #[test]
    fn markdown_lib_heading_helpers() {
        let (engine, _rx) = ScriptEngine::new();
        engine
            .eval(include_str!("../scheme/core.scm"))
            .expect("core.scm");
        // markdown.scm registers its chevron handler with the folding registry.
        engine
            .eval(include_str!("../scheme/folding.scm"))
            .expect("folding.scm");
        engine
            .eval(include_str!("../scheme/markdown.scm"))
            .expect("markdown.scm");
        let v = |q: &str| engine.eval(q).unwrap().value.unwrap();

        // ATX heading levels; a `#` with no following space is not a heading.
        assert_eq!(v("(md-heading-level \"## Title\")"), "2");
        assert_eq!(v("(md-heading-level \"#### deep\")"), "4");
        assert_eq!(v("(md-heading-level \"# Top\")"), "1");
        assert_eq!(v("(md-heading-level \"not a heading\")"), "0");
        assert_eq!(v("(md-heading-level \"#nospace\")"), "0");

        // Scale tapers from h1 down, and every heading is larger than body text.
        assert_eq!(
            v("(> (md--heading-scale 1) (md--heading-scale 2))"),
            "#true"
        );
        assert_eq!(v("(> (md--heading-scale 4) 1)"), "#true");

        // The structure signature (which gates re-fontify, fixing the per-key
        // blink) is stable when only title text changes, but differs when a
        // heading's level changes — so editing a title skips the repaint.
        assert_eq!(
            v("(equal? (md--heading-signature (list \"# A\" \"body\")) \
                       (md--heading-signature (list \"# Z\" \"text\")))"),
            "#true"
        );
        assert_eq!(
            v("(equal? (md--heading-signature (list \"# A\")) \
                       (md--heading-signature (list \"## A\")))"),
            "#false"
        );
        // Inserting a (non-heading) line — pressing Enter — leaves the signature
        // unchanged, so no repaint fires and the headers don't blink.
        assert_eq!(
            v("(equal? (md--heading-signature (list \"# A\" \"body\")) \
                       (md--heading-signature (list \"# A\" \"\" \"body\")))"),
            "#true"
        );

        // Section extent + folding. Lines: 0 "# A", 1 body, 2 body, 3 "# B", 4 body.
        // A's body is the half-open [1, 3); folding it must hide BOTH body lines
        // (this guards the off-by-one that used to leave the last body line shown).
        engine
            .eval("(define L (list \"# A\" \"b1\" \"b2\" \"# B\" \"b3\"))")
            .unwrap();
        assert_eq!(v("(md--section-body-end L 0)"), "3");
        assert_eq!(v("(md--section-body-end L 3)"), "5"); // last section runs to EOF
        // Foldable headings are those with a body (both here have one).
        assert_eq!(v("(length (md--foldable-headings L))"), "2");
        assert_eq!(v("(car (md--foldable-headings L))"), "0");
        // Folding section A must hide its whole body [1,3) — not [1,2).
        engine.eval("(md--folded-set (list 0))").unwrap();
        let cmds = engine.eval("(md--reapply-folds L)").unwrap().commands;
        assert!(
            cmds.iter().any(|c| matches!(
                c,
                EditorCommand::FoldLines {
                    start_line: 1,
                    end_line: 3
                }
            )),
            "section A should fold [1,3): {cmds:?}"
        );
    }

    /// The org-roam library loads and its pure helpers + the new id/time
    /// primitives behave.
    #[test]
    fn org_roam_lib_pure_functions() {
        let (engine, _rx) = ScriptEngine::new();
        engine
            .eval(include_str!("../scheme/core.scm"))
            .expect("core.scm");
        engine
            .eval(include_str!("../scheme/org-roam.scm"))
            .expect("org-roam.scm");
        let v = |q: &str| engine.eval(q).unwrap().value.unwrap();

        // The two new Rust primitives.
        assert_eq!(v(r"(string-length (org-id-new))"), "36"); // v4 UUID
        assert_eq!(v(r#"(string-length (current-time "%Y%m%d%H%M%S"))"#), "14");
        assert_eq!(v(r#"(string-length (current-time "%Y-%m-%d"))"#), "10");

        // Slugify: lowercase, collapse non-alphanumerics, trim.
        assert_eq!(v(r#"(org-roam--slug "Hello, World!")"#), "\"hello-world\"");
        assert_eq!(v(r#"(org-roam--slug "  ??? ")"#), "\"untitled\"");

        // filetags parsing and grep-hit parsing.
        assert_eq!(
            v(r#"(string-join (org-roam--split-tags ":work:urgent:") "|")"#),
            "\"work|urgent\""
        );
        assert_eq!(
            v(r#"(nth (org-roam--parse-hit "/n/a.org:12:see [[id:x]]") 1)"#),
            "12"
        );
        assert_eq!(
            v(r#"(nth (org-roam--parse-hit "/n/a.org:12:see [[id:x]]") 0)"#),
            "\"/n/a.org\""
        );

        // Node lookups over a hand-built index (5-element records: …line).
        engine
            .eval(
                r#"(set! *org-roam-nodes* (list (list "ID1" "Alpha" "/n/a.org" '() 0)
                                                  (list "ID2" "Beta" "/n/b.org" '() 7)))"#,
            )
            .unwrap();
        assert_eq!(v(r#"(nth (org-roam--node-by-title "Beta") 0)"#), "\"ID2\"");
        assert_eq!(v(r#"(nth (org-roam--node-by-id "ID1") 1)"#), "\"Alpha\"");
        assert_eq!(
            v(r#"(string-join (org-roam--titles) "|")"#),
            "\"Alpha|Beta\""
        );
        // Subtree node carries its headline line; file-level node is line 0.
        assert_eq!(
            v(r#"(org-roam--node-line (org-roam--node-by-id "ID2"))"#),
            "7"
        );

        // Subtree parsing: headline title cleaned, drawer :ID: found below it.
        assert_eq!(
            v(r#"(org-roam--heading-text "** TODO My Topic :tag:")"#),
            "\"TODO My Topic\""
        );
        assert_eq!(
            v(r#"(org-roam--drawer-id (list ":PROPERTIES:" ":ID:  abc-123" ":END:"))"#),
            "\"abc-123\""
        );
        assert_eq!(
            v(r#"(if (org-roam--drawer-id (list "plain text")) 1 0)"#),
            "0"
        );

        // Link-id extraction (drives graph edges).
        assert_eq!(
            v(r#"(string-join (org-roam--link-ids "see [[id:AA][x]] and [[id:BB]]") "|")"#),
            "\"AA|BB\""
        );
        // DOT label sanitising: embedded quotes become apostrophes.
        assert_eq!(v(r#"(org-roam--dot-escape "a\"b")"#), "\"a'b\"");
    }

    /// Rename porcelain entries ("OLD -> NEW") split into real pathspecs so git
    /// add/reset/checkout don't get the literal arrow string as one path.
    #[test]
    fn magit_rename_pathspecs() {
        let (engine, _rx) = ScriptEngine::new();
        engine.eval(include_str!("../scheme/core.scm")).unwrap();
        engine
            .eval(include_str!("../scheme/transient.scm"))
            .unwrap();
        engine.eval(include_str!("../scheme/magit.scm")).unwrap();
        let v = |e: &str| engine.eval(e).unwrap().value.unwrap_or_default();
        // Rename: both sides, NEW is the on-disk path.
        assert_eq!(
            v(r#"(string-join (magit-pathspecs "a/old.svg -> a/new.svg") "|")"#),
            "\"a/old.svg|a/new.svg\""
        );
        assert_eq!(
            v(r#"(magit-newpath "a/old.svg -> a/new.svg")"#),
            "\"a/new.svg\""
        );
        // Plain path: a single pathspec, unchanged.
        assert_eq!(
            v(r#"(string-join (magit-pathspecs "a/plain.txt") "|")"#),
            "\"a/plain.txt\""
        );
        assert_eq!(v(r#"(magit-newpath "a/plain.txt")"#), "\"a/plain.txt\"");
    }

    /// The `## …` branch header from `git status --branch` parses into
    /// (branch upstream ahead behind) — replacing 3 separate git spawns.
    #[test]
    fn magit_branch_line_parse() {
        let (engine, _rx) = ScriptEngine::new();
        engine.eval(include_str!("../scheme/core.scm")).unwrap();
        engine
            .eval(include_str!("../scheme/transient.scm"))
            .unwrap();
        engine.eval(include_str!("../scheme/magit.scm")).unwrap();
        let v = |e: &str| engine.eval(e).unwrap().value.unwrap_or_default();
        let pipe = |line: &str| format!(r#"(string-join (magit-parse-branch-line "{line}") "|")"#);
        // No upstream.
        assert_eq!(v(&pipe("## main")), "\"main||0|0\"");
        // Upstream, in sync.
        assert_eq!(
            v(&pipe("## main...origin/main")),
            "\"main|origin/main|0|0\""
        );
        // Both ahead and behind.
        assert_eq!(
            v(&pipe("## main...origin/main [ahead 1, behind 2]")),
            "\"main|origin/main|1|2\""
        );
        // One-sided.
        assert_eq!(
            v(&pipe("## main...origin/main [ahead 3]")),
            "\"main|origin/main|3|0\""
        );
        assert_eq!(
            v(&pipe("## main...origin/main [behind 4]")),
            "\"main|origin/main|0|4\""
        );
        // Fresh repo and detached HEAD.
        assert_eq!(v(&pipe("## No commits yet on main")), "\"main||0|0\"");
        assert_eq!(v(&pipe("## HEAD (no branch)")), "\"HEAD (detached)||0|0\"");
    }

    /// The expanded magit command set builds its transient menus.
    #[test]
    fn magit_transients_build() {
        let (engine, _rx) = ScriptEngine::new();
        engine.eval(include_str!("../scheme/core.scm")).unwrap();
        engine
            .eval(include_str!("../scheme/transient.scm"))
            .unwrap();
        engine.eval(include_str!("../scheme/magit.scm")).unwrap();

        match engine
            .eval("(magit-branch-popup)")
            .unwrap()
            .commands
            .as_slice()
        {
            [EditorCommand::Transient { title, columns }] => {
                assert_eq!(title, "Branch");
                let rows: Vec<_> = columns.iter().flat_map(|c| &c.rows).collect();
                // checkout, checkout-remote, create, set-upstream, rename, delete
                assert_eq!(rows.len(), 6);
            }
            other => panic!("expected a Transient, got {other:?}"),
        }
        // The dispatch menu lists the command groups; Push uses the raw `shift-p`
        // chord (prettified to "P" only at display time).
        match engine.eval("(magit-dispatch)").unwrap().commands.as_slice() {
            [EditorCommand::Transient { title, columns }] => {
                assert_eq!(title, "Magit");
                let rows: Vec<_> = columns.iter().flat_map(|c| &c.rows).collect();
                assert!(rows.iter().any(|r| r.key == "shift-p" && r.label == "Push"));
                assert!(rows.iter().any(|r| r.command == "magit-stash-popup"));
            }
            other => panic!("expected a Transient, got {other:?}"),
        }
    }

    /// magit diff line classification + sub-line accent byte offsets.
    #[test]
    fn magit_diff_rendering() {
        let (engine, _rx) = ScriptEngine::new();
        engine.eval(include_str!("../scheme/core.scm")).unwrap();
        engine
            .eval(include_str!("../scheme/transient.scm"))
            .unwrap();
        engine.eval(include_str!("../scheme/magit.scm")).unwrap();

        // Face classification.
        for (line, face) in [
            ("@@ -1 +1 @@", "magit-hunk"),
            ("+added", "magit-added"),
            ("-removed", "magit-removed"),
            (" context", "magit-context"),
        ] {
            assert_eq!(
                engine
                    .eval(&format!("(magit-diff-face {line:?})"))
                    .unwrap()
                    .value
                    .as_deref(),
                Some(&*format!("\"{face}\"")),
            );
        }

        // Accent byte offsets: "Head!" is 5 bytes + newline = 6, so the next
        // line's "+" marker accent is byte [6, 7).
        engine
            .eval("(set! *magit-lines* '()) (set! *magit-n* 0) (set! *magit-byte* 0) (set! *magit-accents* '())")
            .unwrap();
        engine
            .eval(r#"(magit-emit "Head!" "magit-section-heading" #f #f)"#)
            .unwrap();
        engine.eval(r#"(magit-emit-diff-line "+x")"#).unwrap();
        assert_eq!(
            engine
                .eval("(nth (car *magit-accents*) 0)")
                .unwrap()
                .value
                .as_deref(),
            Some("6")
        );
        assert_eq!(
            engine
                .eval("(nth (car *magit-accents*) 1)")
                .unwrap()
                .value
                .as_deref(),
            Some("7")
        );
        assert_eq!(
            engine
                .eval("(nth (car *magit-accents*) 2)")
                .unwrap()
                .value
                .as_deref(),
            Some("\"magit-add-marker\"")
        );

        // utf8-length counts BYTES, not chars (é = 2 bytes) — accent offsets stay
        // correct after multibyte lines.
        assert_eq!(
            engine
                .eval(r#"(utf8-length "café")"#)
                .unwrap()
                .value
                .as_deref(),
            Some("5")
        );
    }

    /// Inline-blame: `git blame --line-porcelain` parses into per-line records,
    /// the age helpers humanize/tint by author-time, and uncommitted lines are
    /// recognized.
    #[test]
    fn magit_blame_porcelain() {
        let (engine, _rx) = ScriptEngine::new();
        engine.eval(include_str!("../scheme/core.scm")).unwrap();
        engine
            .eval(include_str!("../scheme/transient.scm"))
            .unwrap();
        engine.eval(include_str!("../scheme/magit.scm")).unwrap();
        let v = |e: &str| engine.eval(e).unwrap().value.unwrap_or_default();

        // Two commits over three lines (lines 1-2 share a commit; line 3 differs),
        // built as the list of porcelain lines the parser consumes. Each block ends
        // with the tab-prefixed content line.
        engine
            .eval(
                r#"(define blame-sample (list
                     "abc1234500000000000000000000000000000000 1 1 2"
                     "author Alice"
                     "author-time 100"
                     "summary First commit subject"
                     "filename f.txt"
                     "\tline one"
                     "abc1234500000000000000000000000000000000 2 2"
                     "author Alice"
                     "author-time 100"
                     "summary First commit subject"
                     "filename f.txt"
                     "\tline two"
                     "def4567800000000000000000000000000000000 3 3 1"
                     "author Bob"
                     "author-time 200"
                     "summary Second commit"
                     "filename f.txt"
                     "\tline three"))
                   (define blame-recs (magit-blame-parse blame-sample))"#,
            )
            .unwrap();

        // One record per source line, in file order.
        assert_eq!(v("(length blame-recs)"), "3");
        // Record 1: line 1, Alice's commit, summary carried through.
        assert_eq!(v("(magit-blame-rec-line (list-ref blame-recs 0))"), "1");
        assert_eq!(v("(list-ref (list-ref blame-recs 0) 2)"), "\"Alice\"");
        assert_eq!(
            v("(list-ref (list-ref blame-recs 0) 4)"),
            "\"First commit subject\""
        );
        // Lines 1-2 share a SHA; line 3 is a different commit (so the collapse in
        // `magit-blame-annotate` will annotate lines 1 and 3, not line 2).
        assert_eq!(
            v(
                "(equal? (magit-blame-rec-sha (list-ref blame-recs 0)) (magit-blame-rec-sha (list-ref blame-recs 1)))"
            ),
            "#true"
        );
        assert_eq!(
            v(
                "(equal? (magit-blame-rec-sha (list-ref blame-recs 1)) (magit-blame-rec-sha (list-ref blame-recs 2)))"
            ),
            "#false"
        );

        // Header detection: a 40-hex SHA header is recognized; a key line is not.
        assert_eq!(
            v(r#"(magit-blame-hex40? "abc1234500000000000000000000000000000000 1 1 2")"#),
            "#true"
        );
        assert_eq!(v(r#"(magit-blame-hex40? "author Alice")"#), "#false");

        // Ancient epochs (1970) humanize to "years ago" and tint old.
        assert_eq!(
            v(r#"(ends-with? (magit-blame-humanize 100) "years ago")"#),
            "#true"
        );
        assert_eq!(v("(magit-blame-age-face 100)"), "\"magit-blame-old\"");

        // Uncommitted (all-zero SHA) is recognized and rendered plainly.
        assert_eq!(
            v(
                r#"(magit-blame-uncommitted? (list 1 "0000000000000000000000000000000000000000" "Not Committed Yet" 0 ""))"#
            ),
            "#true"
        );
        assert_eq!(
            v(
                r#"(magit-blame-rec-suffix (list 1 "0000000000000000000000000000000000000000" "Not Committed Yet" 0 ""))"#
            ),
            "\"  Not committed yet\""
        );
        // A committed record's suffix carries short-hash, author, and summary.
        assert_eq!(
            v(r#"(string-contains? (magit-blame-rec-suffix (list-ref blame-recs 0)) "abc12345")"#),
            "#true"
        );
        assert_eq!(
            v(
                r#"(string-contains? (magit-blame-rec-suffix (list-ref blame-recs 0)) "First commit subject")"#
            ),
            "#true"
        );
    }

    /// Range-editing + undo-boundary + buffer-local commands parse.
    #[test]
    fn editing_primitives_parse() {
        let (engine, _rx) = ScriptEngine::new();
        assert!(matches!(
            engine
                .eval("(delete-region 2 5)")
                .unwrap()
                .commands
                .as_slice(),
            [EditorCommand::DeleteRegion { start: 2, end: 5 }]
        ));
        assert!(matches!(
            engine
                .eval("(kill-region-between 2 5)")
                .unwrap()
                .commands
                .as_slice(),
            [EditorCommand::KillRegionBetween { start: 2, end: 5 }]
        ));
        assert!(matches!(
            engine.eval(r#"(replace-region 1 4 "hi")"#).unwrap().commands.as_slice(),
            [EditorCommand::ReplaceRegion { start: 1, end: 4, text }] if text == "hi"
        ));
        assert!(matches!(
            engine.eval(r#"(insert-at 7 "x")"#).unwrap().commands.as_slice(),
            [EditorCommand::InsertAt { pos: 7, text }] if text == "x"
        ));
        assert!(matches!(
            engine.eval("(undo-boundary)").unwrap().commands.as_slice(),
            [EditorCommand::UndoBoundary]
        ));
        assert!(matches!(
            engine.eval(r#"(buffer-local-set "k" "v")"#).unwrap().commands.as_slice(),
            [EditorCommand::BufferLocalSet { key, value }] if key == "k" && value == "v"
        ));
    }

    /// Regexp primitives (pure string ops).
    #[test]
    fn regexp_primitives() {
        let (engine, _rx) = ScriptEngine::new();
        let v = |s: &str| engine.eval(s).unwrap().value.unwrap();
        assert_eq!(v(r#"(string-match "b+" "aabbb")"#), "2");
        assert_eq!(v(r#"(string-match "z" "abc")"#), "-1");
        assert_eq!(v(r#"(length (regexp-captures "(a)(b)" "ab"))"#), "3");
        assert_eq!(v(r#"(car (regexp-captures "(a)(b)" "xaby"))"#), "\"ab\"");
        assert_eq!(
            v(r#"(replace-regexp-in-string "a" "X" "banana")"#),
            "\"bXnXnX\""
        );
        // regexp-quote escapes regex metacharacters (".": 1 char → 2).
        assert_eq!(v(r#"(string-length (regexp-quote "a.b"))"#), "4");
    }

    /// Markers track positions across edits via `adjust_markers`.
    #[test]
    fn markers_track_edits() {
        let (engine, _rx) = ScriptEngine::new();
        engine.eval("(define m (make-marker 10))").unwrap();
        assert_eq!(
            engine.eval("(marker-position m)").unwrap().value.as_deref(),
            Some("10")
        );
        // Insert 5 bytes at offset 0 → the marker shifts to 15.
        engine.adjust_markers(0, 0, 5);
        assert_eq!(
            engine.eval("(marker-position m)").unwrap().value.as_deref(),
            Some("15")
        );
        // Delete [2, 8) (6 bytes) → marker (15) shifts back by 6 to 9.
        engine.adjust_markers(2, 8, 0);
        assert_eq!(
            engine.eval("(marker-position m)").unwrap().value.as_deref(),
            Some("9")
        );
        // A marker inside a deleted span collapses to the deletion start.
        engine.eval("(define inside (make-marker 5))").unwrap();
        engine.adjust_markers(3, 7, 0);
        assert_eq!(
            engine
                .eval("(marker-position inside)")
                .unwrap()
                .value
                .as_deref(),
            Some("3")
        );
    }

    /// Minibuffer + process + macro + line-suffix commands parse.
    #[test]
    fn misc_primitives_parse() {
        let (engine, _rx) = ScriptEngine::new();
        // `completing-read` / `read-string` are now core.scm wrappers (optional
        // initial value) over the Rust `*-impl` primitives.
        engine.eval(include_str!("../scheme/core.scm")).unwrap();
        assert!(matches!(
            engine.eval(r#"(completing-read "Pick: " (list "a" "b") "cb")"#).unwrap().commands.as_slice(),
            [EditorCommand::CompletingRead { candidates, callback, .. }]
                if candidates == &["a", "b"] && callback == "cb"
        ));
        assert!(matches!(
            engine.eval(r#"(y-or-n-p "Sure?" "cb")"#).unwrap().commands.as_slice(),
            [EditorCommand::YesOrNo { callback, .. }] if callback == "cb"
        ));
        assert!(matches!(
            engine.eval(r#"(set-line-suffix 3 "// hint" "dim")"#).unwrap().commands.as_slice(),
            [EditorCommand::SetLineSuffix { line: 3, text, face }] if text == "// hint" && face == "dim"
        ));
        // make-process returns an id and enqueues SendProcess/KillProcess by id.
        assert!(matches!(
            engine
                .eval(r"(kill-process 1)")
                .unwrap()
                .commands
                .as_slice(),
            [EditorCommand::KillProcess { id: 1 }]
        ));
        assert!(matches!(
            engine
                .eval("(start-kbd-macro)")
                .unwrap()
                .commands
                .as_slice(),
            [EditorCommand::StartKbdMacro]
        ));
    }

    /// The Scheme editing lib (edit.scm) loads and its pure helpers work.
    #[test]
    fn edit_lib_helpers() {
        let (engine, _rx) = ScriptEngine::new();
        engine.eval(include_str!("../scheme/core.scm")).unwrap();
        engine.eval(include_str!("../scheme/edit.scm")).unwrap();
        // uncomment-line strips the first comment prefix.
        assert_eq!(
            engine
                .eval(r#"(uncomment-line ";; hello" ";; ")"#)
                .unwrap()
                .value
                .as_deref(),
            Some("\"hello\"")
        );
        // Default comment string (no buffer-local set) is "// ".
        assert_eq!(
            engine.eval("(comment-string)").unwrap().value.as_deref(),
            Some("\"// \"")
        );
    }

    /// Async primitives parse to their commands.
    #[test]
    fn async_primitives_parse() {
        let (engine, _rx) = ScriptEngine::new();
        match engine
            .eval(r#"(make-process "grep" (list "-rn" "x" ".") "on-line" "on-done")"#)
            .unwrap()
            .commands
            .as_slice()
        {
            [
                EditorCommand::MakeProcess {
                    program,
                    args,
                    on_output,
                    on_exit,
                    ..
                },
            ] => {
                assert_eq!(program, "grep");
                assert_eq!(args, &["-rn", "x", "."]);
                assert_eq!(on_output, "on-line");
                assert_eq!(on_exit.as_deref(), Some("on-done"));
            }
            other => panic!("expected MakeProcess, got {other:?}"),
        }
        // Empty on-exit string means "no exit callback".
        match engine
            .eval(r#"(make-process "ls" (list) "on-line" "")"#)
            .unwrap()
            .commands
            .as_slice()
        {
            [EditorCommand::MakeProcess { on_exit, .. }] => assert!(on_exit.is_none()),
            other => panic!("expected MakeProcess, got {other:?}"),
        }
        assert!(matches!(
            engine.eval(r#"(run-with-timer 500 "tick")"#).unwrap().commands.as_slice(),
            [EditorCommand::RunWithTimer { ms: 500, callback }] if callback == "tick"
        ));
    }

    /// `(make-terminal name program args)` enqueues a `MakeTerminal` and returns
    /// a fresh id.
    #[test]
    fn make_terminal_parses() {
        let (engine, _rx) = ScriptEngine::new();
        match engine
            .eval(r#"(make-terminal "*terminal*" "/bin/fish" (list "-l"))"#)
            .unwrap()
            .commands
            .as_slice()
        {
            [
                EditorCommand::MakeTerminal {
                    name,
                    program,
                    args,
                    id,
                },
            ] => {
                assert_eq!(name, "*terminal*");
                assert_eq!(program, "/bin/fish");
                assert_eq!(args, &["-l"]);
                assert!(*id > 0);
            }
            other => panic!("expected MakeTerminal, got {other:?}"),
        }
    }

    /// `(transient title rows)` enqueues a `Transient` with parsed rows.
    #[test]
    fn transient_parses_rows() {
        let (engine, _rx) = ScriptEngine::new();
        let result = engine
            .eval(r#"(transient "Commit" (list (list "c" "Commit" "magit-do-commit")))"#)
            .unwrap();
        match result.commands.as_slice() {
            // Flat form → a single untitled column.
            [EditorCommand::Transient { title, columns }] => {
                assert_eq!(title, "Commit");
                assert_eq!(columns.len(), 1);
                assert_eq!(columns[0].title, None);
                let rows = &columns[0].rows;
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].key, "c");
                assert_eq!(rows[0].command, "magit-do-commit");
            }
            other => panic!("expected one Transient, got {other:?}"),
        }
    }

    #[test]
    fn transient_parses_columns() {
        let (engine, _rx) = ScriptEngine::new();
        // Grouped form → titled columns.
        let result = engine
            .eval(
                r#"(transient "Menu"
                     (list (list "Edit" (list "c" "Commit" "do-c") (list "a" "Amend" "do-a"))
                           (list "Sync" (list "p" "Push" "do-p"))))"#,
            )
            .unwrap();
        match result.commands.as_slice() {
            [EditorCommand::Transient { columns, .. }] => {
                assert_eq!(columns.len(), 2);
                assert_eq!(columns[0].title.as_deref(), Some("Edit"));
                assert_eq!(columns[0].rows.len(), 2);
                assert_eq!(columns[1].title.as_deref(), Some("Sync"));
                assert_eq!(columns[1].rows[0].key, "p");
            }
            other => panic!("expected one Transient, got {other:?}"),
        }
    }

    /// The Scheme libs load (no git runs at load — only defines), and the pure
    /// helpers magit relies on (named-let lookup, porcelain parsing, transient
    /// arg toggles) behave.
    #[test]
    fn lsp_config_wrapper_registers_languages_and_formatters() {
        let (engine, _rx) = ScriptEngine::new();
        engine
            .eval(include_str!("../scheme/core.scm"))
            .expect("core.scm");
        engine
            .eval(include_str!("../scheme/lsp.scm"))
            .expect("lsp.scm");

        // plist + extension helpers.
        assert_eq!(
            engine
                .eval("(lsp--plist-get (list 'a 1 'b 2) 'b)")
                .unwrap()
                .value
                .as_deref(),
            Some("2")
        );
        assert_eq!(
            engine
                .eval(r#"(lsp--file-ext "src/main.rs")"#)
                .unwrap()
                .value
                .as_deref(),
            Some("\"rs\"")
        );
        assert_eq!(
            engine
                .eval(r#"(lsp--file-ext "Makefile")"#)
                .unwrap()
                .value
                .as_deref(),
            Some("\"\"")
        );

        // A shell formatter registers against every extension of the language.
        engine
            .eval(
                r#"(lsp-language "rust" 'extensions (list "rs") 'format-on-save (list "rustfmt"))"#,
            )
            .expect("lsp-language");
        assert_eq!(
            engine
                .eval(r#"(car (lsp--alist-get *lsp-formatters* "rs"))"#)
                .unwrap()
                .value
                .as_deref(),
            Some("\"rustfmt\"")
        );

        // 'lsp format-on-save enqueues the Rust toggle command, not a shell spec.
        let cmds = engine
            .eval(r#"(lsp-language "go" 'extensions (list "go") 'format-on-save 'lsp)"#)
            .unwrap()
            .commands;
        assert!(
            cmds.iter()
                .any(|c| matches!(c, EditorCommand::SetLspFormatOnSave(l, true) if l == "go")),
            "'lsp format-on-save should enable the Rust path: {cmds:?}"
        );
        // …and registers no shell formatter for .go.
        assert_eq!(
            engine
                .eval(r#"(lsp--alist-get *lsp-formatters* "go")"#)
                .unwrap()
                .value
                .as_deref(),
            Some("#false")
        );
    }

    #[test]
    fn scheme_libs_load_and_helpers_work() {
        let (engine, _rx) = ScriptEngine::new();
        // Load the full bundled library set in real startup order (sorted by
        // name), exactly as production does. Cherry-picking individual files is
        // brittle: e.g. projectile.scm references persistence-dir (defined in
        // persistence.scm), which in turn pulls in helm.scm — and Steel resolves
        // such free identifiers at load time. Loading everything mirrors startup
        // and keeps this test stable as libs gain cross-dependencies.
        for src in crate::runtime::startup_scripts() {
            engine.eval(src).expect("startup script");
        }

        // core.scm helpers.
        assert_eq!(
            engine
                .eval(r#"(basename "/home/me/proj/")"#)
                .unwrap()
                .value
                .as_deref(),
            Some("\"proj\"")
        );
        assert_eq!(
            engine
                .eval(r#"(path-join "/a/b" "c.rs")"#)
                .unwrap()
                .value
                .as_deref(),
            Some("\"/a/b/c.rs\"")
        );
        assert_eq!(
            engine
                .eval(r#"(alist-ref (list (cons "k" "v")) "k")"#)
                .unwrap()
                .value
                .as_deref(),
            Some("\"v\"")
        );

        // save-excursion macro expands and evaluates (point/goto-char are stubbed
        // by the default snapshot here, but the form must compile and run).
        engine
            .eval(r"(save-excursion (+ 1 2))")
            .expect("save-excursion");

        // The `fuzzy-match` primitive ranks/filters from Scheme (the building
        // block for a Scheme-driven Helm): the non-match drops, tighter first.
        assert_eq!(
            engine
                .eval(r#"(car (fuzzy-match "fbr" (list "unrelated.txt" "foo/bar.rs")))"#)
                .unwrap()
                .value
                .as_deref(),
            Some("\"foo/bar.rs\"")
        );

        // projectile root resolution walks up to a marker on the real FS. This
        // crate's dir contains Cargo.toml, so it resolves to the manifest dir.
        let dir = env!("CARGO_MANIFEST_DIR");
        let root = engine
            .eval(&format!("(project-root \"{dir}/src\")"))
            .unwrap()
            .value
            .unwrap();
        assert_eq!(root, format!("\"{dir}\""));

        // Porcelain entry parsing.
        assert_eq!(
            engine
                .eval(r#"(entry-path " M src/foo.rs")"#)
                .unwrap()
                .value
                .as_deref(),
            Some("\"src/foo.rs\"")
        );
        assert_eq!(
            engine
                .eval(r#"(entry-status "?? new.txt")"#)
                .unwrap()
                .value
                .as_deref(),
            Some("\"??\"")
        );

        // Section-at-point lookup against fixed *magit-sections*
        // (heading path type body-start body-end). Line 4 is inside b.rs's body.
        engine
            .eval(
                r#"(set! *magit-sections*
                     (list (list 0 "a.rs" "staged" 1 1)
                           (list 3 "b.rs" "unstaged" 4 7)))"#,
            )
            .unwrap();
        assert_eq!(
            engine
                .eval("(sec-path (magit-section-at-line 5))")
                .unwrap()
                .value
                .as_deref(),
            Some("\"b.rs\"")
        );
        // The heading line itself also resolves to its section.
        assert_eq!(
            engine
                .eval("(sec-path (magit-section-at-line 0))")
                .unwrap()
                .value
                .as_deref(),
            Some("\"a.rs\"")
        );

        // Transient argument toggles.
        engine
            .eval(r#"(transient-toggle-arg "--force" (lambda () #f))"#)
            .unwrap();
        assert_eq!(
            engine
                .eval(r#"(transient-arg-active? "--force")"#)
                .unwrap()
                .value
                .as_deref(),
            Some("#true")
        );
        engine
            .eval(r#"(transient-toggle-arg "--force" (lambda () #f))"#)
            .unwrap();
        assert_eq!(
            engine
                .eval(r#"(transient-arg-active? "--force")"#)
                .unwrap()
                .value
                .as_deref(),
            Some("#false")
        );
    }

    /// Structural diff view: funcname-context extraction and hunk grouping are
    /// pure reshapings of the line hunks (no git, no AST), the grouped emitter
    /// lays out sub-headings + foldable spans, and group lookup over
    /// *magit-groups* distinguishes a heading (whole-fn staging) from a body
    /// line (fold). The view itself is a live global, line view by default.
    #[test]
    fn structural_diff_grouping() {
        let (engine, _rx) = ScriptEngine::new();
        for src in crate::runtime::startup_scripts() {
            engine.eval(src).expect("startup script failed to load");
        }
        let v = |e: &str| engine.eval(e).unwrap().value.unwrap_or_default();

        // Context = text after the second "@@" on the header, trimmed; the rest
        // of the hunk body is ignored (only the first line is read).
        assert_eq!(
            v(r#"(magit-hunk-context "@@ -10,7 +10,8 @@ fn parse_expr(&mut self) {\n+  x")"#),
            "\"fn parse_expr(&mut self) {\""
        );
        // No funcname (top-of-file change, or a language with no pattern) → ""
        // → displayed as "(top level)".
        assert_eq!(v(r#"(magit-hunk-context "@@ -1,3 +1,4 @@\n+x")"#), "\"\"");
        assert_eq!(v(r#"(magit-group-display "")"#), "\"(top level)\"");

        // Consecutive same-context hunks merge; a new context starts a new group.
        engine
            .eval(
                r#"(define gtest
                     (magit-group-hunks
                       (list "@@ -1,2 +1,2 @@ fn a\n x"
                             "@@ -9,2 +9,2 @@ fn a\n y"
                             "@@ -20,2 +20,2 @@ fn b\n z")))"#,
            )
            .unwrap();
        assert_eq!(v("(length gtest)"), "2"); // two groups
        assert_eq!(v("(car (car gtest))"), "\"fn a\""); // first context
        assert_eq!(v("(length (cdr (car gtest)))"), "2"); // fn a holds 2 hunks
        assert_eq!(v("(car (cadr gtest))"), "\"fn b\""); // second context

        // Group lookup over a fixed *magit-groups*
        // (heading body-start body-end type path context).
        engine
            .eval(r#"(set! *magit-groups* (list (list 5 6 9 "unstaged" "a.rs" "fn a")))"#)
            .unwrap();
        // A body line resolves to its group (for TAB fold)…
        assert_eq!(v("(grp-context (magit-group-at-line 6))"), "\"fn a\"");
        // …but only the heading line is a staging target (s/u/k on a whole fn).
        assert_eq!(v("(grp-context (magit-group-on-heading 5))"), "\"fn a\"");
        assert_eq!(v(r#"(if (magit-group-on-heading 6) "y" "n")"#), "\"n\"");

        // End-to-end render path: drive the grouped emitter over a 3-hunk diff
        // (fn a ×2, fn b ×1) on a reset of the render globals, then inspect what
        // it laid down — group count, the fn-a sub-heading text and its foldable
        // body span, and that every hunk is still registered for per-hunk ops.
        engine
            .eval(
                r#"(begin
                     (set! *magit-n* 0) (set! *magit-byte* 0)
                     (set! *magit-lines* '()) (set! *magit-faces* '())
                     (set! *magit-accents* '())
                     (set! *magit-groups* '()) (set! *magit-hunks* '())
                     (magit-emit-hunks-grouped "hdr"
                       (list "@@ -1,2 +1,2 @@ fn a\n ctx\n+a1"
                             "@@ -9,2 +9,2 @@ fn a\n ctx\n+a2"
                             "@@ -20,2 +20,2 @@ fn b\n ctx\n+b1")
                       "unstaged" "a.rs"))"#,
            )
            .unwrap();
        assert_eq!(v("(length *magit-groups*)"), "2"); // fn a, fn b
        assert_eq!(v("(length *magit-hunks*)"), "3"); // all hunks still per-hunk
        // The fn-a group: heading at line 0, body lines 1..7 (heading + 2×3-line
        // hunks), and the sub-heading text reads "(2 hunks)".
        assert_eq!(v("(grp-context (magit-group-at-line 0))"), "\"fn a\"");
        assert_eq!(v("(grp-body-start (magit-group-at-line 0))"), "1");
        assert_eq!(v("(grp-body-end (magit-group-at-line 0))"), "7");
        assert_eq!(
            v("(list-ref (reverse *magit-lines*) 0)"),
            "\"    fn a  (2 hunks)\""
        );
        // fn b starts where fn a's body ends.
        assert_eq!(v("(grp-context (magit-group-at-line 7))"), "\"fn b\"");

        // The view is a live global (so a toggle takes effect on the next
        // render), line view by default.
        assert_eq!(v("(magit-structural?)"), "#false");
        engine
            .eval(r#"(set! *magit-diff-view* "structural")"#)
            .unwrap();
        assert_eq!(v("(magit-structural?)"), "#true");
    }

    /// Within-line word diff: common prefix/suffix trimming locates the changed
    /// middle of a modified line, and the emit path paints exactly that span on
    /// the word faces (leaving the rest of the line on the dim body face). The
    /// hunk TEXT is never touched — this is overlay-only.
    #[test]
    fn word_diff_highlights_changed_span() {
        let (engine, _rx) = ScriptEngine::new();
        for src in crate::runtime::startup_scripts() {
            engine.eval(src).expect("startup script failed to load");
        }
        let v = |e: &str| engine.eval(e).unwrap().value.unwrap_or_default();

        // Prefix/suffix counts (char-indexed).
        assert_eq!(v(r#"(magit-common-prefix "abc" "abx")"#), "2");
        assert_eq!(v(r#"(magit-common-suffix "abc" "xbc" 3)"#), "2");
        // The changed middle of a vs b, as a (start . end) char range. Here only
        // the "-" vs " " at index 8 differs → a one-char span.
        assert_eq!(
            v(r#"(car (magit-change-span "An Emacs-style" "An Emacs style"))"#),
            "8"
        );
        assert_eq!(
            v(r#"(cdr (magit-change-span "An Emacs-style" "An Emacs style"))"#),
            "9"
        );
        // A wholly-shared side (pure insertion on the other) yields an empty span.
        assert_eq!(v(r#"(car (magit-change-span "foo" "foobar"))"#), "3");
        assert_eq!(v(r#"(cdr (magit-change-span "foo" "foobar"))"#), "3");

        // End-to-end: emit a paired -/+ body on a byte-offset reset, then check
        // the accents. Line 0 "-An Emacs-style, x" (18 bytes) sits at byte 0, so
        // line 1 "+An Emacs style, x" starts at byte 19. The changed char is at
        // content index 8 → byte 9 (removed) / byte 28 (added), one byte wide.
        engine
            .eval(
                r#"(begin
                     (set! *magit-n* 0) (set! *magit-byte* 0)
                     (set! *magit-lines* '()) (set! *magit-accents* '())
                     (set! *magit-faces* '())
                     (magit-emit-hunk-body
                       (list "-An Emacs-style, x" "+An Emacs style, x")))"#,
            )
            .unwrap();
        // The whole modified line sits on the DIM base face (only the change is
        // re-brightened below), so unchanged text recedes.
        assert_eq!(
            v(r#"(if (member (list 0 "magit-removed-dim") *magit-faces*) "y" "n")"#),
            "\"y\""
        );
        assert_eq!(
            v(r#"(if (member (list 1 "magit-added-dim") *magit-faces*) "y" "n")"#),
            "\"y\""
        );
        // The removed "-" and the added " " are each painted on their word face…
        assert_eq!(
            v(r#"(if (member (list 9 10 "magit-word-removed") *magit-accents*) "y" "n")"#),
            "\"y\""
        );
        assert_eq!(
            v(r#"(if (member (list 28 29 "magit-word-added") *magit-accents*) "y" "n")"#),
            "\"y\""
        );

        // Unbalanced blocks (2 removed, 1 added) are NOT paired — no word accent.
        engine
            .eval(
                r#"(begin
                     (set! *magit-n* 0) (set! *magit-byte* 0)
                     (set! *magit-lines* '()) (set! *magit-accents* '())
                     (magit-emit-hunk-body (list "-aaa" "-bbb" "+ccc")))"#,
            )
            .unwrap();
        // "y" = no word-face accent was recorded for the unbalanced block.
        assert_eq!(
            v(
                r#"(if (null? (filter (lambda (a) (or (equal? (nth a 2) "magit-word-removed")
                                                    (equal? (nth a 2) "magit-word-added")))
                                    *magit-accents*)) "y" "n")"#
            ),
            "\"y\""
        );
    }
}

#[cfg(test)]
mod search_e2e {
    use super::*;

    #[test]
    fn walk_and_grep_respect_gitignore_and_config() {
        let dir = std::env::temp_dir().join(format!("macros-search-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        let d = dir.to_str().unwrap();
        std::process::Command::new("git")
            .arg("-C")
            .arg(d)
            .arg("init")
            .arg("-q")
            .output()
            .unwrap();
        std::fs::write(dir.join(".gitignore"), "target/\n").unwrap();
        std::fs::write(dir.join("a.rs"), "let needle = 1;\n").unwrap();
        std::fs::write(dir.join("b.txt"), "plain\n").unwrap();
        std::fs::write(dir.join("sub").join("c.rs"), "fn c() {}\n").unwrap();
        std::fs::write(dir.join("target").join("junk.rs"), "ignored\n").unwrap();
        std::fs::write(dir.join(".secret"), "hidden\n").unwrap();

        let (engine, _rx) = ScriptEngine::new();
        engine
            .eval("(define (any-ends? lst suf) (if (null? lst) #f (if (ends-with? (car lst) suf) #t (any-ends? (cdr lst) suf))))")
            .unwrap();
        let walks = |q: &str| engine.eval(q).unwrap().value.unwrap();

        // Default walk: real files in, gitignored + hidden out.
        let w = format!("(walk-files {d:?})");
        assert_eq!(walks(&format!("(any-ends? {w} \"a.rs\")")), "#true");
        assert_eq!(walks(&format!("(any-ends? {w} \"sub/c.rs\")")), "#true");
        assert_eq!(
            walks(&format!("(any-ends? {w} \"junk.rs\")")),
            "#false",
            "gitignored"
        );
        assert_eq!(
            walks(&format!("(any-ends? {w} \".secret\")")),
            "#false",
            "hidden"
        );

        // Configurable exclude.
        engine.eval(r#"(search-exclude "*.txt")"#).unwrap();
        assert_eq!(
            walks(&format!("(any-ends? {w} \"b.txt\")")),
            "#false",
            "excluded glob"
        );
        // Configurable include (whitelist) — only .rs after reset.
        engine.eval("(search-reset)").unwrap();
        engine.eval(r#"(search-include "*.rs")"#).unwrap();
        assert_eq!(
            walks(&format!("(any-ends? {w} \"b.txt\")")),
            "#false",
            "not whitelisted"
        );
        assert_eq!(walks(&format!("(any-ends? {w} \"a.rs\")")), "#true");
        // Hidden opt-in surfaces .secret.
        engine.eval("(search-reset)").unwrap();
        engine
            .eval(r#"(set-search-option "hidden" "true")"#)
            .unwrap();
        assert_eq!(walks(&format!("(any-ends? {w} \".secret\")")), "#true");

        // grep-files finds the needle and skips ignored dirs.
        engine.eval("(search-reset)").unwrap();
        let g = format!(r#"(grep-files {d:?} "needle")"#);
        assert_eq!(walks(&format!("(length {g})")), "1");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod magit_e2e {
    use super::*;
    #[test]
    fn magit_render_parses_real_diffs() {
        // Build a temp repo with a staged add + an unstaged modification.
        let dir = std::env::temp_dir().join(format!("macros-magit-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let d = dir.to_str().unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(d)
                .args(args)
                .output()
                .unwrap();
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "-qm", "init"]);
        std::fs::write(dir.join("a.txt"), "one\nCHANGED\n").unwrap(); // unstaged mod
        std::fs::write(dir.join("b.txt"), "new\n").unwrap();
        git(&["add", "b.txt"]); // staged add

        let (engine, _rx) = ScriptEngine::new();
        engine.eval(include_str!("../scheme/core.scm")).unwrap();
        engine
            .eval(include_str!("../scheme/transient.scm"))
            .unwrap();
        engine.eval(include_str!("../scheme/magit.scm")).unwrap();
        engine.eval(&format!("(set! *magit-dir* {d:?})")).unwrap();
        engine.eval("(magit-render)").unwrap();

        // Both files appear as sections; the unstaged one has a real diff body.
        assert!(
            engine
                .eval("(length *magit-sections*)")
                .unwrap()
                .value
                .unwrap()
                .parse::<i64>()
                .unwrap()
                >= 2
        );
        // a.txt (unstaged) has body lines (body-end > body-start) and accents.
        let accents = engine
            .eval("(length *magit-accents*)")
            .unwrap()
            .value
            .unwrap();
        assert!(
            accents.parse::<i64>().unwrap() >= 2,
            "expected +/- accents, got {accents}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod magit_hunk_e2e {
    use super::*;
    #[test]
    fn hunk_staging_against_real_git() {
        let dir = std::env::temp_dir().join(format!("macros-hunk-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let d = dir.to_str().unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(d)
                .args(args)
                .output()
                .unwrap();
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(dir.join("f.txt"), "a\nb\nc\nd\n").unwrap();
        git(&["add", "f.txt"]);
        git(&["commit", "-qm", "init"]);
        std::fs::write(dir.join("f.txt"), "a\nCHANGED\nc\nd\n").unwrap(); // one unstaged hunk

        let (engine, _rx) = ScriptEngine::new();
        engine.eval(include_str!("../scheme/core.scm")).unwrap();
        engine
            .eval(include_str!("../scheme/transient.scm"))
            .unwrap();
        engine.eval(include_str!("../scheme/magit.scm")).unwrap();
        engine.eval(&format!("(set! *magit-dir* {d:?})")).unwrap();
        engine.eval("(magit-render)").unwrap();

        // The diff parsed into at least one hunk with a header + @@ text.
        assert!(
            engine
                .eval("(length *magit-hunks*)")
                .unwrap()
                .value
                .unwrap()
                .parse::<i64>()
                .unwrap()
                >= 1
        );
        assert_eq!(
            engine
                .eval(r#"(starts-with? (nth (car *magit-hunks*) 2) "diff --git")"#)
                .unwrap()
                .value
                .as_deref(),
            Some("#true"),
            "hunk carries the file header"
        );

        // Stage the hunk → it moves to the index, nothing left unstaged.
        assert_eq!(
            engine
                .eval("(magit-apply-hunk (car *magit-hunks*) #t #f)")
                .unwrap()
                .value
                .as_deref(),
            Some("#true"),
            "git apply --cached succeeded"
        );
        assert_eq!(
            engine
                .eval(r#"(string-contains? (git (list "diff" "--cached" "--name-only")) "f.txt")"#)
                .unwrap()
                .value
                .as_deref(),
            Some("#true"),
            "the hunk is now staged"
        );
        assert_eq!(
            engine
                .eval(r#"(equal? (trim-end (git (list "diff" "--name-only"))) "")"#)
                .unwrap()
                .value
                .as_deref(),
            Some("#true"),
            "nothing left unstaged"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod helm_lib {
    use super::*;
    #[test]
    fn helm_occur_and_imenu_helpers() {
        let (engine, _rx) = ScriptEngine::new();
        for src in crate::runtime::startup_scripts() {
            engine.eval(src).unwrap();
        }
        let v = |q: &str| engine.eval(q).unwrap().value.unwrap();
        // imenu name extraction across a few languages.
        assert_eq!(v(r#"(imenu-name "(define (foo x) ...)")"#), "\"foo\"");
        assert_eq!(v(r#"(imenu-name "fn bar() {")"#), "\"bar\"");
        assert_eq!(v(r#"(imenu-name "def baz(self):")"#), "\"baz\"");
        assert_eq!(v(r#"(imenu-name "class Qux:")"#), "\"Qux\"");
        assert_eq!(
            v(r#"(if (imenu-name "   x = 1") "hit" "none")"#),
            "\"none\""
        );
        // new commands registered for M-x.
        for c in [
            "helm-buffers-list",
            "helm-mini",
            "list-buffers",
            "helm-recentf",
            "helm-occur",
            "helm-imenu",
        ] {
            assert_eq!(
                engine
                    .eval(&format!(
                        r#"(if (member "{c}" *interactive-commands*) "y" "n")"#
                    ))
                    .unwrap()
                    .value
                    .unwrap(),
                "\"y\"",
                "{c} not registered"
            );
        }
    }
}

#[cfg(test)]
mod diagnostics_lib {
    use super::*;
    #[test]
    fn list_and_navigation_logic() {
        let (engine, _rx) = ScriptEngine::new();
        engine.eval(include_str!("../scheme/core.scm")).unwrap();
        engine
            .eval(include_str!("../scheme/diagnostics.scm"))
            .unwrap();
        // next-error/previous-error are the unified dispatchers in shell.scm; with
        // no compile run, *next-error-source* stays "diagnostics" so they route here.
        engine.eval(include_str!("../scheme/shell.scm")).unwrap();
        // Inject diagnostics through the snapshot — the real primitive path.
        engine.set_snapshot(EditorSnapshot {
            line: 0,
            diagnostics: vec![
                "5\t2\terror\tunbound x".into(),
                "10\t0\twarning\tunused y".into(),
            ],
            ..Default::default()
        });
        // Parsing: leading number is the 0-based source line.
        assert_eq!(
            engine
                .eval(r#"(diag-line-of "5\t2\terror\tx")"#)
                .unwrap()
                .value
                .as_deref(),
            Some("5")
        );
        // next-error from line 0 jumps to the nearest below (5).
        assert!(matches!(
            engine.eval("(next-error)").unwrap().commands.as_slice(),
            [EditorCommand::GotoLine(5)]
        ));
        // previous-error from line 0 has nothing above → a message, no jump.
        assert!(matches!(
            engine.eval("(previous-error)").unwrap().commands.as_slice(),
            [EditorCommand::Message(_)]
        ));
        // The list buffer renders one row per diagnostic (header + blank + 2).
        match engine
            .eval("(list-diagnostics)")
            .unwrap()
            .commands
            .as_slice()
        {
            [EditorCommand::RenderBuffer { name, text, .. }] => {
                assert_eq!(name, "*diagnostics*");
                assert!(
                    text.contains("6:3  error: unbound x"),
                    "1-based display, got:\n{text}"
                );
                assert!(text.contains("11:1  warning: unused y"));
            }
            other => panic!("expected RenderBuffer, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod compile_lib {
    use super::*;

    fn engine() -> (ScriptEngine, UnboundedReceiver<ScriptOutcome>) {
        let (engine, rx) = ScriptEngine::new();
        engine.eval(include_str!("../scheme/core.scm")).unwrap();
        engine
            .eval(include_str!("../scheme/diagnostics.scm"))
            .unwrap();
        engine.eval(include_str!("../scheme/shell.scm")).unwrap();
        (engine, rx)
    }

    #[test]
    fn second_compile_prefills_last_command() {
        let (engine, _rx) = engine();
        // First run records the command.
        let result = engine.eval(r#"(compile-run "make test")"#).unwrap();
        assert!(
            result
                .commands
                .iter()
                .any(|c| matches!(c, EditorCommand::MakeProcess { .. }))
        );
        assert_eq!(
            engine.eval("*compile-last*").unwrap().value.as_deref(),
            Some("\"make test\"")
        );
        // Invoking `compile` again should open the minibuffer pre-filled.
        match engine.eval("(compile)").unwrap().commands.as_slice() {
            [
                EditorCommand::ReadString {
                    initial, callback, ..
                },
            ] => {
                assert_eq!(
                    initial, "make test",
                    "second compile should pre-fill last cmd"
                );
                assert_eq!(callback, "compile-run");
            }
            other => panic!("expected ReadString, got {other:?}"),
        }
    }

    /// The error parser recognizes the common compiler/grep formats and falls
    /// through to #f on plain text.
    #[test]
    fn compile_match_formats() {
        let (engine, _rx) = engine();
        let v = |q: &str| engine.eval(q).unwrap().value.unwrap();
        // rust/cargo `  --> path:line:col`
        assert_eq!(
            v(r#"(nth (compile-match "  --> src/main.rs:10:5") 0)"#),
            "\"src/main.rs\""
        );
        assert_eq!(
            v(r#"(nth (compile-match "  --> src/main.rs:10:5") 1)"#),
            "10"
        );
        // python traceback (no column → 1)
        assert_eq!(
            v(r#"(nth (compile-match "  File \"foo.py\", line 42, in main") 0)"#),
            "\"foo.py\""
        );
        assert_eq!(
            v(r#"(nth (compile-match "  File \"foo.py\", line 42, in main") 2)"#),
            "1"
        );
        // gcc/clang `path:line:col:`
        assert_eq!(
            v(r#"(nth (compile-match "src/x.c:3:9: error: oops") 1)"#),
            "3"
        );
        // grep -n `path:line:` (no column)
        assert_eq!(
            v(r#"(nth (compile-match "README.md:7:matched text") 1)"#),
            "7"
        );
        assert_eq!(
            v(r#"(nth (compile-match "README.md:7:matched text") 2)"#),
            "1"
        );
        // plain prose → no match
        assert_eq!(
            v(r#"(if (compile-match "Compiling macros v0.1.0") "hit" "miss")"#),
            "\"miss\""
        );
    }

    /// Streaming records each error against its *compilation* line, and the final
    /// re-render faces those lines plus the status footer at the right index.
    #[test]
    fn streaming_and_faced_render() {
        let (engine, _rx) = engine();
        engine.eval(r#"(set! *compile-last* "make")"#).unwrap();
        engine.eval(r#"(set! *compile-dir* "/proj")"#).unwrap();
        engine.eval("(set! *compile-out* '())").unwrap();
        engine.eval("(set! *compile-errors* '())").unwrap();
        engine.eval("(set! *compile-line* 2)").unwrap();
        // Feed three output lines; the middle one is an error at *compilation* line 3.
        engine.eval(r#"(compile-output "Compiling")"#).unwrap();
        engine
            .eval(r#"(compile-output "src/x.c:3:9: error: oops")"#)
            .unwrap();
        engine.eval(r#"(compile-output "more output")"#).unwrap();
        // One error recorded, on comp-line 3 (header is lines 0-1, output starts at 2).
        assert_eq!(
            engine
                .eval("(length *compile-errors*)")
                .unwrap()
                .value
                .as_deref(),
            Some("1")
        );
        assert_eq!(
            engine
                .eval("(car (car *compile-errors*))")
                .unwrap()
                .value
                .as_deref(),
            Some("3")
        );
        // Finishing re-renders with the error line + status line faced.
        match engine
            .eval(r#"(compile-done "2")"#)
            .unwrap()
            .commands
            .as_slice()
        {
            [
                EditorCommand::RenderBuffer {
                    name,
                    text,
                    line_faces,
                    ..
                },
                EditorCommand::Message(m),
            ] => {
                assert_eq!(name, "*compilation*");
                assert!(
                    text.starts_with("$ make\n\n"),
                    "header preserved, got:\n{text}"
                );
                assert!(text.contains("Compilation exited abnormally with code 2"));
                // error line (3) is red; status line (last) is also red on failure.
                let last = text.split('\n').count() - 1;
                assert!(
                    line_faces.contains(&(3, "compilation-error".into())),
                    "faces: {line_faces:?}"
                );
                assert!(
                    line_faces.contains(&(last, "compilation-error".into())),
                    "faces: {line_faces:?}"
                );
                assert!(m.contains("1 error"), "summary: {m}");
            }
            other => panic!("expected RenderBuffer + Message, got {other:?}"),
        }
    }

    /// compile-run claims the next-error source, so the unified next-error walks
    /// the compilation errors (jumping to the source in another window).
    #[test]
    fn next_error_routes_to_compile() {
        let (engine, _rx) = engine();
        engine.eval(r#"(set! *compile-dir* "/proj")"#).unwrap();
        engine.eval("(set! *compile-out* '())").unwrap();
        engine.eval("(set! *compile-errors* '())").unwrap();
        engine.eval("(set! *compile-line* 2)").unwrap();
        engine.eval("(set! *compile-idx* -1)").unwrap();
        engine
            .eval(r#"(set! *next-error-source* "compile")"#)
            .unwrap();
        // Two errors; the file must exist for the jump (use this very source file).
        let here = concat!(env!("CARGO_MANIFEST_DIR"), "/src/script.rs");
        engine
            .eval(&format!(r#"(compile-output "{here}:5:1: error: a")"#))
            .unwrap();
        engine
            .eval(&format!(r#"(compile-output "{here}:9:1: error: b")"#))
            .unwrap();
        // First next-error opens the file in another window and jumps to line 5 (4 0-based).
        let cmds = engine.eval("(next-error)").unwrap();
        assert!(
            cmds.commands
                .iter()
                .any(|c| matches!(c, EditorCommand::GotoLine(4))),
            "expected GotoLine(4), got {:?}",
            cmds.commands
        );
        assert!(
            cmds.commands
                .iter()
                .any(|c| matches!(c, EditorCommand::OpenFileOtherWindow(_)))
        );
        // Second advances to the next error (line 9 → 8 0-based).
        let cmds = engine.eval("(next-error)").unwrap();
        assert!(
            cmds.commands
                .iter()
                .any(|c| matches!(c, EditorCommand::GotoLine(8)))
        );
    }

    #[test]
    fn projectile_grep_goto_opens_file_and_jumps_line() {
        // projectile.scm depends on the rest of the bundled library, so load it all.
        let (engine, _rx) = ScriptEngine::new();
        for src in crate::runtime::startup_scripts() {
            engine.eval(src).unwrap();
        }
        // grep-files emits 1-based `path:line:text`; selecting a match must open
        // the file and land on the 0-based line (line 12 -> GotoLine(11)).
        let cmds = engine
            .eval(r#"(projectile-grep-goto "/proj/src/main.rs:12:    let x = 1;")"#)
            .unwrap();
        assert!(
            cmds.commands
                .iter()
                .any(|c| matches!(c, EditorCommand::OpenFile(p) if p == "/proj/src/main.rs")),
            "expected OpenFile(/proj/src/main.rs), got {:?}",
            cmds.commands
        );
        assert!(
            cmds.commands
                .iter()
                .any(|c| matches!(c, EditorCommand::GotoLine(11))),
            "expected GotoLine(11), got {:?}",
            cmds.commands
        );
    }
}

#[cfg(test)]
mod revert_cmd {
    use super::*;
    #[test]
    fn revert_buffer_is_a_builtin_command() {
        let (engine, _rx) = ScriptEngine::new();
        // Nullary builtin → in command-list → M-x discoverable.
        assert_eq!(
            engine
                .eval(r#"(if (member "revert-buffer" (command-list)) "y" "n")"#)
                .unwrap()
                .value
                .as_deref(),
            Some("\"y\"")
        );
        // And it enqueues the RevertBuffer command.
        assert!(matches!(
            engine.eval("(revert-buffer)").unwrap().commands.as_slice(),
            [EditorCommand::RevertBuffer]
        ));
    }
}

#[cfg(test)]
mod display_modes {
    use super::*;
    #[test]
    fn line_number_and_scroll_bar_modes() {
        let (engine, _rx) = ScriptEngine::new();
        let v = |q: &str| engine.eval(q).unwrap().value.unwrap();
        assert_eq!(
            v(r#"(if (member "line-numbers-mode" (command-list)) "y" "n")"#),
            "\"y\""
        );
        assert_eq!(
            v(r#"(if (member "scroll-bar-mode" (command-list)) "y" "n")"#),
            "\"y\""
        );
        assert_eq!(
            v(r#"(if (member "word-wrap-mode" (command-list)) "y" "n")"#),
            "\"y\""
        );
        assert!(matches!(
            engine
                .eval("(line-numbers-mode)")
                .unwrap()
                .commands
                .as_slice(),
            [EditorCommand::ToggleLineNumbers]
        ));
        assert!(matches!(
            engine
                .eval("(scroll-bar-mode)")
                .unwrap()
                .commands
                .as_slice(),
            [EditorCommand::ToggleScrollBar]
        ));
        assert!(matches!(
            engine.eval("(word-wrap-mode)").unwrap().commands.as_slice(),
            [EditorCommand::ToggleWordWrap]
        ));
        assert_eq!(
            v(r#"(if (member "electric-pair-mode" (command-list)) "y" "n")"#),
            "\"y\""
        );
        assert!(matches!(
            engine
                .eval("(electric-pair-mode)")
                .unwrap()
                .commands
                .as_slice(),
            [EditorCommand::ToggleElectricPair]
        ));
    }
}

#[cfg(test)]
mod window_targeting {
    use super::*;
    #[test]
    fn window_reads_and_commands() {
        let (engine, _rx) = ScriptEngine::new();
        engine.set_snapshot(EditorSnapshot {
            windows: vec![0, 2, 5],
            selected_window: 2,
            ..Default::default()
        });
        // read bridges over the snapshot
        assert_eq!(
            engine.eval("(selected-window)").unwrap().value.as_deref(),
            Some("2")
        );
        assert_eq!(
            engine.eval("(car (window-list))").unwrap().value.as_deref(),
            Some("0")
        );
        assert_eq!(
            engine
                .eval("(length (window-list))")
                .unwrap()
                .value
                .as_deref(),
            Some("3")
        );
        // command enqueues
        assert!(matches!(
            engine
                .eval("(select-window 5)")
                .unwrap()
                .commands
                .as_slice(),
            [EditorCommand::SelectWindow(5)]
        ));
        assert!(
            matches!(engine.eval(r#"(open-file-other-window "/tmp/x.rs")"#).unwrap().commands.as_slice(),
            [EditorCommand::OpenFileOtherWindow(p)] if p == "/tmp/x.rs")
        );
    }
}

#[cfg(test)]
mod icons {
    use super::*;
    #[test]
    fn assets_load_and_set_line_icon() {
        // The AssetSource resolves a bundled Lucide icon.
        use gpui::AssetSource;
        let bytes = crate::assets::Assets.load("icons/folder.svg").unwrap();
        assert!(bytes.is_some(), "folder.svg should be bundled");
        assert!(
            crate::assets::Assets
                .load("icons/file-code.svg")
                .unwrap()
                .is_some()
        );
        // The primitive enqueues a SetLineIcon with the parsed args.
        let (engine, _rx) = ScriptEngine::new();
        assert!(matches!(
            engine.eval(r#"(set-line-icon 3 4 "folder")"#).unwrap().commands.as_slice(),
            [EditorCommand::SetLineIcon(3, 4, n)] if n == "folder"
        ));
        assert!(matches!(
            engine
                .eval("(clear-line-icons)")
                .unwrap()
                .commands
                .as_slice(),
            [EditorCommand::ClearLineIcons]
        ));
    }
}

#[cfg(test)]
mod fs_watch_lib {
    use super::*;

    /// The watch primitives and the sidebar auto-refresh path: `watch-directories`
    /// and `render-buffer-quiet` map to their commands, and the treemacs/dired FS
    /// hooks render *quietly* (no focus-stealing switch) while re-declaring the
    /// directory set they want watched.
    #[test]
    fn watch_primitives_and_quiet_refresh() {
        let (engine, _rx) = ScriptEngine::new();
        for src in crate::runtime::startup_scripts() {
            engine.eval(src).unwrap();
        }
        let v = |q: &str| engine.eval(q).unwrap().value.unwrap();

        // The hook handlers exist and are wired to `directory-changed-hook`.
        assert_eq!(v("(if (procedure? treemacs-on-fs-change) \"y\" \"n\")"), "\"y\"");
        assert_eq!(v("(if (procedure? dired-on-fs-change) \"y\" \"n\")"), "\"y\"");

        // `(watch-directories owner dirs)` carries the owner + the full dir list.
        let cmds = engine
            .eval(r#"(watch-directories "treemacs" (list "/a" "/b"))"#)
            .unwrap()
            .commands;
        match cmds.as_slice() {
            [EditorCommand::WatchDirectories(owner, dirs)] => {
                assert_eq!(owner, "treemacs");
                assert_eq!(dirs, &vec!["/a".to_string(), "/b".to_string()]);
            }
            other => panic!("expected one WatchDirectories, got {other:?}"),
        }

        // `render-buffer-quiet` is its own command (never the focus-stealing one).
        let cmds = engine
            .eval(r#"(render-buffer-quiet "*x*" "m" "hi" '())"#)
            .unwrap()
            .commands;
        assert!(matches!(
            cmds.as_slice(),
            [EditorCommand::RenderBufferQuiet { name, .. }] if name == "*x*"
        ));

        // A treemacs auto-refresh renders quietly and re-declares its watch set
        // (root + expanded dirs) for the "treemacs" owner.
        let cmds = engine
            .eval(
                r#"(begin
                     (set! *treemacs-root* ".")
                     (set! *treemacs-expanded* '())
                     (set! *treemacs-quiet* #t)
                     (treemacs-render))"#,
            )
            .unwrap()
            .commands;
        assert!(
            cmds.iter()
                .any(|c| matches!(c, EditorCommand::RenderBufferQuiet { name, .. } if name == "*treemacs*")),
            "quiet treemacs refresh must use render-buffer-quiet"
        );
        assert!(
            cmds.iter().any(|c| matches!(
                c,
                EditorCommand::WatchDirectories(owner, dirs) if owner == "treemacs" && dirs == &vec![".".to_string()]
            )),
            "treemacs render must re-declare its watched dirs"
        );

        // The interactive (non-quiet) path still uses the focus-switching render.
        let cmds = engine
            .eval(r#"(begin (set! *treemacs-quiet* #f) (treemacs-render))"#)
            .unwrap()
            .commands;
        assert!(
            cmds.iter()
                .any(|c| matches!(c, EditorCommand::RenderBuffer { name, .. } if name == "*treemacs*")),
            "interactive treemacs refresh keeps the switching render-buffer"
        );
    }
}

#[cfg(test)]
mod treemacs_lib {
    use super::*;
    #[test]
    fn loads_and_maps_icons() {
        let (engine, _rx) = ScriptEngine::new();
        for src in crate::runtime::startup_scripts() {
            engine.eval(src).unwrap();
        }
        let v = |q: &str| engine.eval(q).unwrap().value.unwrap();
        assert_eq!(v(r#"(file-icon "main.rs")"#), "\"file-code\"");
        assert_eq!(v(r#"(file-icon "Cargo.toml")"#), "\"settings-2\"");
        assert_eq!(v(r#"(file-icon "data.json")"#), "\"braces\"");
        assert_eq!(v(r#"(file-icon "logo.png")"#), "\"image\"");
        assert_eq!(v(r#"(file-icon "README.md")"#), "\"file-text\"");
        assert_eq!(v(r#"(file-icon "notes")"#), "\"file\"");
        assert_eq!(v(r#"(strip-slash "/a/b/")"#), "\"/a/b\"");
        assert_eq!(
            v(r#"(if (member "treemacs" *interactive-commands*) "y" "n")"#),
            "\"y\""
        );
        // Indent rails: two columns ("│ ") per ancestor depth.
        assert_eq!(v(r"(treemacs-rails 0)"), "\"\"");
        assert_eq!(v(r"(treemacs-rails 2)"), "\"│ │ \"");
        // Whole-line faces reuse theme faces so colors track the active theme:
        // dotfiles -> comment (file or dir), other dirs -> keyword accent, plain
        // files take the default foreground (#f).
        assert_eq!(v(r#"(treemacs-face ".git" #t)"#), "\"comment\"");
        assert_eq!(v(r#"(treemacs-face "src" #t)"#), "\"keyword\"");
        assert_eq!(v(r#"(if (treemacs-face "main.rs" #f) "y" "n")"#), "\"n\"");
    }
}

#[cfg(test)]
mod section_lib {
    use super::*;

    /// Build a two-level section tree and assert the rendered text, the
    /// point→section lookup, and the fold-toggle/default-collapsed logic.
    #[test]
    fn sections_render_navigate_and_fold() {
        let (engine, _rx) = ScriptEngine::new();
        for src in crate::runtime::startup_scripts() {
            engine.eval(src).unwrap();
        }
        let v = |q: &str| engine.eval(q).unwrap().value.unwrap();

        engine
            .eval(
                r#"(begin
                     (section-begin)
                     (section-line "Head" "h")
                     (section-open 'files "Files (2)" "h")
                     (section-line "  a.txt")
                     (section-line "  b.txt")
                     (section-close 'files))"#,
            )
            .unwrap();

        // Rendering emits a read-only buffer with the accumulated text (followed
        // by the overlay/fold commands that section-apply-overlays queues).
        let cmds = engine
            .eval(r#"(section-render "*sec*" "sec-mode")"#)
            .unwrap()
            .commands;
        let render = cmds
            .iter()
            .find_map(|c| match c {
                EditorCommand::RenderBuffer {
                    name, mode, text, ..
                } => Some((name, mode, text)),
                _ => None,
            })
            .expect("a RenderBuffer command");
        assert_eq!(render.0, "*sec*");
        assert_eq!(render.1, "sec-mode");
        assert_eq!(render.2, "Head\nFiles (2)\n  a.txt\n  b.txt");

        // Heading is line 1; its body covers lines 2..4. Lookups resolve to the id.
        assert_eq!(v("(section-id (section-at-line 1))"), "files");
        assert_eq!(v("(section-id (section-at-line 3))"), "files");
        assert_eq!(v("(if (section-at-line 0) \"hit\" \"miss\")"), "\"miss\""); // the Head line

        // Default-expanded: nothing folded until toggled.
        assert_eq!(v("(section-folded? 'files)"), "#false");
        engine
            .eval("(set! *section-toggled* (list 'files))")
            .unwrap();
        assert_eq!(v("(section-folded? 'files)"), "#true");

        // With default-collapsed, the meaning of a toggle flips.
        engine.eval("(section-default-collapsed! #t)").unwrap();
        engine.eval("(set! *section-toggled* '())").unwrap();
        assert_eq!(v("(section-folded? 'files)"), "#true");
    }
}

#[cfg(test)]
mod tabulated_lib {
    use super::*;

    /// A tabulated render pads columns and emits a header face; sorting reorders
    /// the body rows.
    #[test]
    fn tabulated_formats_and_sorts() {
        let (engine, _rx) = ScriptEngine::new();
        for src in crate::runtime::startup_scripts() {
            engine.eval(src).unwrap();
        }
        let v = |q: &str| engine.eval(q).unwrap().value.unwrap();

        // Column padding: "ab" in a width-4 column, then a flexible last column.
        assert_eq!(
            v(r#"(tabulated-format (list "ab" "c") (list 4 #f))"#),
            "\"ab   c\""
        );

        let render = engine
            .eval(
                r#"(tabulated-render "*tab*" "tab-mode"
                     (list (list "Name" 6) (list "V" #f))
                     (list (list 'b (list "bbb" "2")) (list 'a (list "aaa" "1")))
                     (lambda (id) (message "x")))"#,
            )
            .unwrap();
        match render.commands.as_slice() {
            [
                EditorCommand::RenderBuffer {
                    text, line_faces, ..
                },
            ] => {
                // Header line + unsorted rows (b before a), columns aligned.
                assert_eq!(text, "Name   V\nbbb    2\naaa    1");
                assert_eq!(line_faces, &[(0, "tabulated-header".to_string())]);
            }
            other => panic!("expected one RenderBuffer, got {other:?}"),
        }

        // The sort helper orders rows by a column's cell (ascending), and the
        // descending flag reverses it. (The interactive `tabulated-sort` command
        // drives this off the active buffer, which a headless engine lacks.)
        assert_eq!(
            v(r#"(map (lambda (r) (symbol->string (car r)))
                   (tabulated-sort-rows
                     (list (list 'b (list "bbb" "2")) (list 'a (list "aaa" "1"))) 0 #f))"#),
            "(\"a\" \"b\")"
        );
        assert_eq!(
            v(r#"(map (lambda (r) (symbol->string (car r)))
                   (tabulated-sort-rows
                     (list (list 'b (list "bbb" "2")) (list 'a (list "aaa" "1"))) 0 #t))"#),
            "(\"b\" \"a\")"
        );
    }
}

#[cfg(test)]
mod capf_lib {
    use super::*;

    /// The bundled dabbrev CAPF is registered, tokenizes buffer words, and
    /// declines on too-short prefixes.
    #[test]
    fn dabbrev_capf_helpers() {
        let (engine, _rx) = ScriptEngine::new();
        let mut registered = false;
        for src in crate::runtime::startup_scripts() {
            let out = engine.eval(src).unwrap();
            registered |= out
                .commands
                .iter()
                .any(|c| matches!(c, EditorCommand::AddCapf(f) if f == "dabbrev-capf"));
        }
        assert!(registered, "completion.scm should register dabbrev-capf");

        let v = |q: &str| engine.eval(q).unwrap().value.unwrap();
        // Word tokenizer splits on non-word chars, keeping _ and -.
        assert_eq!(
            v(r#"(capf-buffer-words "foo bar, foo_baz qux-1")"#),
            "(\"foo\" \"bar\" \"foo_baz\" \"qux-1\")"
        );
        // Dedup preserves first-seen order.
        assert_eq!(
            v(r#"(capf-dedup (list "a" "b" "a" "c" "b"))"#),
            "(\"a\" \"b\" \"c\")"
        );
        // A one-character prefix is declined (too short).
        assert_eq!(v(r#"(dabbrev-capf "f")"#), "()");
    }
}
