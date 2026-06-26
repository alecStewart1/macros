use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use gpui::{
    App, Bounds, Context, CursorStyle, EntityInputHandler, ExternalPaths, FocusHandle, Focusable,
    KeyDownEvent, Keystroke, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Point, RenderImage, ScrollWheelEvent, SharedString, Task, UTF16Selection, Window, actions, div,
    point, prelude::*, px,
};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};
use regex::Regex;
use unicode_segmentation::UnicodeSegmentation;

use crate::anvil::{AnvilEngine, AnvilOutcome};
use crate::buffer::Buffer;
use crate::element::{EditorElement, LineLayout};
use crate::highlight::{Edit, Highlighter, HlSpan};
use crate::lsp::{
    self, CompletionItem, Diagnostic, LspBackend, LspClient, RawDiagnostic, SteelLsp,
};
use crate::mode::{Keymap, Mode, ModeStack};
use crate::script::{EditorCommand, ScriptEngine, ScriptOutcome};
use crate::terminal::{TermEvent, Terminal};
use crate::theme::Theme;

mod completion;
mod evil;
mod helm;
mod lsp_methods;
mod panes;

// Keys that the OS treats specially are captured as GPUI actions (in `main`)
// rather than through the Scheme keymap, then delegated to the same commands.
actions!(
    editor,
    [
        Backspace,
        Delete,
        Left,
        Right,
        Up,
        Down,
        Home,
        End,
        Newline,
        Tab,
        SelectLeft,
        SelectRight,
        SelectAll,
    ]
);

/// A reversible edit at byte `at`: it replaced `removed` with `inserted`.
/// Undo restores `removed` over the inserted span; redo re-applies `inserted`
/// over the removed span. Storing both texts makes undo and redo symmetric,
/// which is what the undo *tree* (branching history) needs.
#[derive(Clone)]
struct UndoRecord {
    at: usize,
    removed: String,
    inserted: String,
}

/// One state in the undo tree. The root (index 0) is the buffer as loaded;
/// every other node carries the edit group that transforms its parent into it.
/// Branches are preserved: undoing then typing adds a *sibling* child rather
/// than discarding the redo branch (the whole point of an undo tree / vundo).
#[derive(Clone)]
struct UndoNode {
    parent: Option<usize>,
    children: Vec<usize>,
    /// Edits (in application order) that take `parent`'s state to this node's.
    edit: Vec<UndoRecord>,
}

/// A fresh undo tree: a single root node with no edits.
fn new_undo_tree() -> Vec<UndoNode> {
    vec![UndoNode {
        parent: None,
        children: Vec::new(),
        edit: Vec::new(),
    }]
}

/// An action deferred until the user confirms it in a yes/no minibuffer.
#[derive(Clone, Copy)]
enum PendingAction {
    KillActiveBuffer,
    KillBufferAndWindow,
}

/// What a minibuffer read is for, deciding completion candidates and what
/// happens on submit.
#[derive(Clone)]
enum MinibufferPurpose {
    /// `M-x`: run the entered command name.
    ExecuteCommand,
    /// `C-x C-f`: visit the entered file path.
    FindFile,
    /// `C-x b`: switch to the entered buffer name.
    SwitchBuffer,
    /// A yes/no confirmation gating `action`.
    Confirm(PendingAction),
    /// Quit-time save offer: `yes` saves all modified buffers then quits, `no`
    /// quits discarding changes, `C-g` cancels the quit.
    QuitSave,
    /// LSP completion: replace the word at `completion_range` with the choice.
    CompleteAtPoint,
    /// evil `/` — regex search forward.
    SearchForward,
    /// evil `?` — regex search backward.
    SearchBackward,
    /// Emacs `C-s` / `C-r` — incremental search. The query is read in the
    /// minibuffer and matched live on every keystroke (see `isearch_*`).
    Isearch,
    /// evil `:` — an ex command line (e.g. `%s/a/b/g`, `w`, `q`).
    ExCommand,
    /// `M-:` — evaluate the entered Steel expression, echoing the result.
    EvalExpression,
    /// `read-string`: call the named Steel fn with the entered text.
    SchemeCallback(String),
    /// `completing-read`: complete against `completing_candidates`, then call fn.
    SchemeCompleting(String),
    /// Dynamic `completing-read`: candidates come from a Scheme fn re-queried per
    /// keystroke (see `mb_dynamic`); shown as-is (the fn does the filtering).
    SchemeCompletingDynamic(String),
    /// `read-file-name`: file-path completion, then call fn with the path.
    SchemeReadFile(String),
    /// `y-or-n-p`: call fn with "yes" or "no".
    SchemeYesNo(String),
}

/// A buffer's state, snapshotted when it isn't the active buffer. The active
/// buffer's state lives directly on [`Editor`]; switching swaps it with one of
/// these.
struct Document {
    /// Buffer name shown in the mode line (e.g. `*scratch*`, `main.scm`).
    name: String,
    /// File this buffer is visiting, if any.
    path: Option<PathBuf>,
    buffer: Buffer,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    mark_active: bool,
    undo_tree: Vec<UndoNode>,
    undo_current: usize,
    scroll_offset: Pixels,
    modes: ModeStack,
    highlighter: Option<Highlighter>,
    /// Unsaved changes since the last load/save.
    modified: bool,
    /// Whether this buffer's mode hooks have run (Emacs: once per buffer).
    hooks_ran: bool,
    /// When `Some(mode)`, this is a read-only program-generated buffer (the
    /// magit pattern): plain keys are commands resolved against `mode`'s keymap
    /// rather than self-inserting.
    special: Option<String>,
    /// Per-line whole-line face overrides, by line index (special buffers).
    line_faces: HashMap<usize, SharedString>,
    /// Per-line icon (asset name + char column to paint at), for special buffers
    /// like the file tree. Mirrors `line_faces`.
    line_icons: HashMap<usize, (usize, SharedString)>,
    /// Per-line inline image: `line -> image file path`. The decoded bitmap lives
    /// in the editor-wide [`Editor::image_cache`]; this just records which lines
    /// display which image (Org inline images / LaTeX previews).
    line_images: HashMap<usize, (PathBuf, usize)>,
    /// Overlays / text properties / folds for this buffer.
    overlays: Overlays,
    /// Buffer-local variables (Scheme `buffer-local-set`/`-get`).
    locals: HashMap<String, String>,
}

/// A decoded raster image kept in the editor-wide cache, with its intrinsic
/// pixel size so the layout can compute how many rows it needs.
#[derive(Clone)]
struct CachedImage {
    data: Arc<RenderImage>,
    width: u32,
    height: u32,
}

/// Inline images wider than this (logical px) are scaled down to fit; narrower
/// ones display at their natural size. A fixed cap (rather than the pane width)
/// keeps row-height math independent of window size, so folds/scroll/cursor all
/// stay consistent. Content past the pane edge is clipped by the viewport mask.
const INLINE_IMAGE_MAX_WIDTH: f32 = 600.0;

/// An active minibuffer prompt: a single-line input read in the echo area.
struct Minibuffer {
    prompt: String,
    input: String,
    /// Byte offset of point within `input`.
    cursor: usize,
    /// Transient hint shown after the input (e.g. completion feedback).
    message: String,
    purpose: MinibufferPurpose,
}

/// A snapshot of the active minibuffer for rendering, split at the cursor.
pub struct MinibufferView {
    pub prompt: String,
    pub before: String,
    pub after: String,
    pub message: String,
}

/// A text editor over a single [`Buffer`], with a stack of active [`Mode`]s and
/// an embedded [`ScriptEngine`]. This is the GPUI entity that owns all buffer
/// state; the [`EditorElement`] only draws it.
pub struct Editor {
    pub buffer: Buffer,
    pub focus_handle: FocusHandle,
    pub modes: ModeStack,
    pub script: ScriptEngine,
    /// Drains async script outcomes from the engine thread onto the foreground.
    script_task: Option<Task<()>>,
    /// The ANVIL REPL: a second, independent Steel interpreter (see
    /// [`crate::anvil`]). Used by `anvil-eval` to run forms in an isolated,
    /// interruptible image without touching the config engine.
    pub anvil: AnvilEngine,
    /// Drains async ANVIL REPL results onto the foreground.
    anvil_task: Option<Task<()>>,
    /// Global key bindings, shared across all buffers.
    keymap: Keymap,
    /// Echo-area message (the minibuffer line).
    pub status: SharedString,

    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    is_selecting: bool,
    /// Drag-select autoscroll. While the pointer is held near/past the top or
    /// bottom edge during a drag-selection, a self-rescheduling timer scrolls
    /// the view and extends the selection. `autoscroll_dy` is the signed
    /// per-tick velocity in pixels (0 = idle), `autoscroll_pos` the last pointer
    /// position to re-hit-test from, and `autoscrolling` guards against spawning
    /// more than one timer chain at a time.
    autoscrolling: bool,
    autoscroll_dy: f32,
    autoscroll_pos: Point<Pixels>,
    /// Scrollback of echo-area messages (Emacs `*Messages*`), capped. Every
    /// `echo` appends here so transient status can be reviewed later.
    messages: Vec<String>,
    /// Captured stderr from external language servers (Emacs-style `*lsp-log*`),
    /// capped — the only window into why a server failed/crashed.
    lsp_log: Vec<String>,
    /// The diagnostic message currently echoed because the pointer is hovering
    /// it — so a later move off the diagnostic clears only our own echo.
    hover_message: Option<SharedString>,
    /// Whether the mark is active, so plain motion extends the region (Emacs
    /// transient-mark behaviour).
    mark_active: bool,
    /// Unsaved changes in the active buffer since its last load/save.
    modified: bool,
    /// Whether the active buffer's mode hooks have run yet.
    hooks_ran: bool,
    /// electric-pair-mode: auto-insert a closing delimiter after a typed opener.
    electric_pair: bool,
    /// Active yasnippet session: (field marker id, placeholder byte length) in
    /// tab order, plus the current field index. Empty when no snippet is active.
    snippet_fields: Vec<(u64, usize)>,
    snippet_index: usize,
    /// avy jump hints: (byte offset, label) painted over the buffer until a jump
    /// is made (or cleared). Empty when avy isn't active.
    avy_hints: Vec<(usize, SharedString)>,
    /// Secondary cursors (multiple-cursors), stored as marker ids so they ride
    /// along on edits via the marker store. Edits (typed text, newline, delete)
    /// are replicated to each. Empty when only the primary cursor is active.
    extra_cursors: Vec<u64>,
    /// Mode name -> Steel functions to run when that mode is enabled.
    hooks: HashMap<String, Vec<String>>,
    /// Point's buffer line at the last `post-command-hook` fire, so the hook runs
    /// once per cursor-line change rather than on every intra-command move.
    last_hook_line: Option<usize>,
    /// Scheme completion-at-point functions, tried in order before LSP.
    capf_functions: Vec<String>,

    kill_ring: Vec<String>,
    kill_index: usize,
    last_yank: Option<Range<usize>>,
    last_was_yank: bool,
    undo_tree: Vec<UndoNode>,
    undo_current: usize,
    /// While true, edits append to the current undo group (so a multi-edit Scheme
    /// command undoes as one unit); otherwise each edit is its own group.
    undo_grouping: bool,
    /// Whether the current group has received an edit yet (lazily created).
    undo_group_started: bool,

    /// Keys accumulated in an in-progress prefix sequence (e.g. after `C-x`).
    pending_keys: Vec<String>,
    /// Emacs universal-argument (`C-u` / `M-<digit>`) reading state. While
    /// `uarg_reading`, digit keys accumulate into `uarg_digits` and repeated
    /// `C-u` raises `uarg_cu` (giving the 4/16/64 default). When a non-argument
    /// key follows, the computed count lands in `prefix_arg` for the next
    /// command to consume as a repeat count.
    uarg_reading: bool,
    uarg_digits: Option<i64>,
    uarg_cu: u32,
    prefix_arg: Option<i64>,
    /// The active minibuffer prompt, if a read is in progress.
    minibuffer: Option<Minibuffer>,
    /// Candidates for an active `completing-read` minibuffer.
    completing_candidates: Vec<String>,
    /// Shared minibuffer input history (most recent last), recalled with Up/Down
    /// or `M-p`/`M-n` while a read is open (Emacs minibuffer history).
    minibuffer_history: Vec<String>,
    /// Position while browsing history: `None` = editing a fresh input; `Some(i)`
    /// = showing `history[len-1-i]`. `minibuffer_stash` holds the fresh input so
    /// returning past the newest entry restores it.
    minibuffer_history_pos: Option<usize>,
    minibuffer_stash: String,
    /// For a dynamic `completing-read` (candidates from a Scheme fn): the fn name
    /// and the latest query id, re-dispatched per keystroke. `None` otherwise.
    mb_dynamic: Option<(String, u64)>,

    scroll_offset: Pixels,
    /// Horizontal scroll of the selected pane, in pixels. Always 0 when word-wrap
    /// is on; otherwise it shifts long lines left so off-screen text is reachable.
    h_scroll_offset: Pixels,
    lines: Vec<LineLayout>,
    last_bounds: Option<Bounds<Pixels>>,
    line_height: Pixels,
    viewport_height: Pixels,
    /// Text-area width of the selected pane (past the gutter), for h-scroll clamp.
    viewport_width: Pixels,
    /// One monospace character's advance, reported by the renderer — used to
    /// follow the cursor horizontally when word-wrap is off.
    char_width: Pixels,
    /// Mirror of the `word-wrap` setting, cached from the renderer so the
    /// (cx-less) scroll helpers can branch on it.
    word_wrap: bool,

    /// Tree-sitter highlighter; keeps the persistent parse tree. Spans are
    /// queried on demand per-frame, not cached.
    highlighter: Option<Highlighter>,

    /// Identity of the active buffer.
    active_name: String,
    active_path: Option<PathBuf>,
    /// Process cwd, resolved once at startup (the editor never `chdir`s). Used as
    /// the fallback `default-directory` for fileless buffers so `publish_point`
    /// doesn't make a `current_dir` syscall on every snapshot publish.
    process_cwd: String,
    /// Other open buffers (snapshots), most-recently-used last.
    inactive: Vec<Document>,

    // --- LSP (one server per language, many at once) ---
    /// Configured servers: LSP language id -> launch command (from `set-lsp`).
    lsp_commands: HashMap<String, String>,
    /// File extension -> server key / config language (from `set-extension`).
    extensions: HashMap<String, String>,
    /// File extension -> wire `languageId` sent in `didOpen` (from
    /// `set-language-id`). Distinct from `extensions`: several extensions can
    /// share one server (e.g. `ts`/`tsx` → the `typescript` server) yet must
    /// announce different ids (`typescript` vs `typescriptreact`) so the server
    /// parses JSX. Falls back to the server key when unset.
    language_ids: HashMap<String, String>,
    /// Running servers, keyed by language id.
    servers: HashMap<String, LspServer>,
    /// Latest diagnostics per URI, in LSP coordinates (any server).
    diagnostics: HashMap<String, Vec<RawDiagnostic>>,
    /// Jump targets `(uri, line, character)` for the rows of the current
    /// `*lsp-xref*` / `*lsp-symbols*` list buffer (RET visits row N's target).
    xref_targets: Vec<(String, usize, usize)>,
    /// Location ring for jump-back (`xref-pop-marker`, Emacs `pop-tag-mark`):
    /// `(path, byte offset)` pushed before each definition/reference jump.
    xref_marker_stack: Vec<(Option<PathBuf>, usize)>,
    /// Code actions backing the rows of the current `*lsp-actions*` buffer.
    code_actions: Vec<lsp::CodeAction>,
    /// Languages whose buffers are LSP-formatted on save (set via the Scheme
    /// `lsp-language` config / `(set-lsp-format-on-save …)`).
    lsp_format_on_save: HashSet<String>,
    /// True while a save is waiting on an LSP format response (so the format's
    /// applied edits trigger the actual write, and we don't re-run hooks).
    formatting_for_save: bool,
    /// Candidates + replacement range for the active completion minibuffer.
    completion_items: Vec<String>,
    completion_range: Option<Range<usize>>,
    /// The in-buffer completion popup (corfu-style), when active.
    completion_popup: Option<CompletionPopup>,
    /// An auto-triggered completion request is in flight (suppresses duplicate
    /// requests and the "no completions" echo while typing).
    completion_auto_pending: bool,
    /// The active Helm selection session, if any.
    helm: Option<HelmSession>,
    /// Monotonic id for dynamic-candidate queries (stale results are dropped).
    helm_query_counter: u64,
    /// While the TAB action menu is open: the candidate it acts on + the
    /// `(label, fn)` actions to choose among.
    helm_action_context: Option<(String, Vec<(String, String)>)>,
    /// Modal-editing (evil) state.
    evil: EvilState,
    /// The last `/`/`?` or `C-s` regex, for `n`/`N` repeat and match highlight.
    search_query: Option<String>,
    /// Direction of the last evil `/` (true) or `?` (false) search, so `n`
    /// repeats it in the same direction and `N` in the opposite (vim semantics).
    evil_search_forward: bool,
    /// Whether the last evil yank/delete was line-wise (`yy`/`dd`) rather than
    /// character-wise (`yw`/visual). Decides how `p`/`P` paste.
    evil_yank_linewise: bool,
    /// Incremental-search (`C-s`) transient state, valid only while an `Isearch`
    /// minibuffer is open. `forward` is the current direction (toggled by C-r/C-s),
    /// `origin` is where point was when the search began (restored on C-g), `from`
    /// is the offset the current attempt searches from, and `match` is the range
    /// of the current hit (the base for the next repeat).
    isearch_forward: bool,
    isearch_origin: usize,
    isearch_from: usize,
    isearch_match: Option<Range<usize>>,
    /// The last completed `C-s` string, so an empty `C-s` recalls it (Emacs).
    last_isearch: Option<String>,
    /// `C-l` (recenter-top-bottom) cycle position: 0 = middle, 1 = top, 2 = bottom.
    /// Advances on each consecutive press; resets when point has moved since.
    recenter_cycle: usize,
    /// Point at the last `C-l`, to detect a fresh (non-consecutive) invocation.
    recenter_point: usize,
    /// When the active buffer is a special read-only buffer, its major-mode name
    /// (mirrors the active [`Document::special`]); plain keys become commands.
    special: Option<String>,
    /// Per-line whole-line face overrides for the active buffer, by line index.
    line_faces: HashMap<usize, SharedString>,
    /// Per-line icon (asset name + char column to paint at), for special buffers
    /// like the file tree. Mirrors `line_faces`.
    line_icons: HashMap<usize, (usize, SharedString)>,
    /// Per-line inline image (`line -> path`) for the active buffer; the decoded
    /// bitmap lives in `image_cache`. Mirrors `line_icons`.
    line_images: HashMap<usize, (PathBuf, usize)>,
    /// Decoded inline images, shared across buffers and keyed by file path so a
    /// re-scan (toggle) reuses the bitmap instead of re-decoding. Not swapped on
    /// buffer switch — it's a process-wide bitmap cache.
    image_cache: HashMap<PathBuf, CachedImage>,
    /// Overlays / text properties / folds for the active buffer.
    overlays: Overlays,
    /// Buffer-local variables for the active buffer.
    locals: HashMap<String, String>,
    /// The open transient popup, if any (the magit / `transient.el` pattern).
    transient: Option<TransientState>,
    /// A mode-line project indicator set from Scheme (projectile-style).
    project_indicator: Option<SharedString>,

    // --- Windows (split panes) ---
    /// All window panes (the selected one is the live view onto the buffer).
    panes: Vec<Pane>,
    /// The split layout tree over `panes`.
    layout: WindowLayout,
    /// The selected pane's id — where editing and the cursor live.
    selected_pane: usize,
    /// Monotonic id source for new panes.
    next_pane_id: usize,
    /// Each pane's last painted screen bounds, for click-to-focus routing.
    pane_bounds: Vec<(usize, Bounds<Pixels>)>,
    /// The completion popup's last painted geometry (selected pane only), for
    /// mapping a click to a candidate row. `None` when no popup is showing.
    completion_popup_hit: Option<PopupHit>,
    /// Fold chevron click regions from the last paint (selected pane only), for
    /// mapping a gutter click to the line whose fold should toggle.
    fold_hitboxes: Vec<FoldHitbox>,
    /// While dragging a split divider: the split node's path + axis.
    resizing: Option<ResizeDrag>,
    /// Live async child processes, by id (for stdin/kill).
    processes: HashMap<u64, ProcessHandle>,
    /// Live embedded terminals, by id. A `term-mode` buffer's name maps to one
    /// of these via [`Terminal::buffer_name`].
    terminals: HashMap<u64, Terminal>,
    /// Sender cloned into each terminal so its background event loop can wake the
    /// editor (redraw / title / exit). Drained by `terminal_task`.
    term_events_tx: futures::channel::mpsc::UnboundedSender<(u64, TermEvent)>,
    /// Keeps the terminal-event drain task alive.
    terminal_task: Option<Task<()>>,
    /// Full-sync LSP coalescing: an edit to a full-document-sync server records
    /// the active `(language, uri)` here instead of serializing and shipping the
    /// whole document on every keystroke. Flushed on a short debounce (so live
    /// diagnostics still update), before any outbound request, and before the
    /// active buffer is swapped out.
    lsp_fullsync_dirty: Option<(String, String)>,
    /// Wakes the debounce task when a full-sync edit marks the document dirty.
    lsp_flush_tx: futures::channel::mpsc::UnboundedSender<()>,
    /// Last painted geometry of each terminal pane, for mapping mouse positions
    /// to grid cells (set by `TerminalElement` each frame).
    terminal_geom: HashMap<u64, TermGeom>,
    /// The terminal id whose selection a drag is currently extending, if any.
    terminal_drag: Option<u64>,
    /// The keyboard macro being recorded (`C-x (`), if any.
    macro_recording: Option<Vec<MacroEvent>>,
    /// The last completed keyboard macro, for replay (`C-x e`).
    last_macro: Vec<MacroEvent>,
    /// Cursor blink phase: whether the cursor is currently shown. Always true
    /// when `cursor-blink` is off. Toggled by a self-rescheduling timer and
    /// reset to true on every keystroke (so the cursor stays solid while typing).
    cursor_blink_visible: bool,
    /// Bumped on each keystroke to invalidate in-flight blink timers, so the
    /// blink chain restarts from "shown" after activity (and old chains die).
    blink_token: u64,
    /// Whether the OS window is active (focused). The cursor only blinks while
    /// active; when the window loses focus the blink stops and the cursor holds
    /// steady (shown).
    window_active: bool,
    /// Subscription to window activate/deactivate, set up lazily on first render
    /// (kept alive by storing it). Drives `window_active`.
    blink_observer: Option<gpui::Subscription>,
}

/// An open transient popup: a title and its `(key, label, command)` rows.
struct TransientState {
    title: String,
    columns: Vec<crate::script::TransientColumn>,
}

/// One rendered transient row: the key to press and what it does.
pub struct TransientRow {
    pub key: String,
    pub label: String,
    /// The bare command name this row runs, for click-to-invoke.
    pub command: String,
}

/// A rendered transient column: an optional header and its rows.
pub struct TransientColumnView {
    pub title: Option<String>,
    pub rows: Vec<TransientRow>,
}

/// A render snapshot of the transient popup (one or more side-by-side columns).
pub struct TransientView {
    pub title: String,
    pub columns: Vec<TransientColumnView>,
}

/// A message from an async [`make_process`](Editor::make_process) child: one
/// stdout line, or the process finishing (the editor then `wait()`s for the code).
enum ProcMsg {
    Line(String),
    Exit,
}

/// A live child process kept so Scheme can send to its stdin or kill it.
struct ProcessHandle {
    child: Child,
    stdin: Option<ChildStdin>,
}

/// A terminal pane's last painted geometry, for mapping a mouse position to a
/// grid cell. `origin` is the pane's top-left; `cell_w`/`cell_h` are one cell.
#[derive(Clone, Copy)]
struct TermGeom {
    origin: Point<Pixels>,
    cell_w: Pixels,
    cell_h: Pixels,
    cols: usize,
    rows: usize,
}

/// One step of a keyboard macro: a resolved command, or self-inserted text.
#[derive(Clone)]
enum MacroEvent {
    Command(String),
    Text(String),
}

/// Overlays / text properties on a buffer (the Emacs overlay + text-property
/// model). Sub-line face ranges layer over tree-sitter highlighting; folded line
/// ranges are hidden from display; text properties carry arbitrary metadata
/// Scheme can read back. All are byte/line-range based and reset only when the
/// buffer itself is replaced.
#[derive(Default)]
struct Overlays {
    /// `(byte_range, face)` — overrides syntax highlighting in that range.
    faces: Vec<(Range<usize>, SharedString)>,
    /// Byte ranges hidden from display, without changing the buffer text.
    hidden: Vec<Range<usize>>,
    /// Folded line ranges (`start..end`, end-exclusive) — hidden from display.
    folds: Vec<Range<usize>>,
    /// `(byte_range, key, value)` text properties (Emacs `put-text-property`).
    props: Vec<(Range<usize>, String, String)>,
    /// `line -> (suffix_text, face)` virtual text appended after a line (inlay
    /// hints, blame, inline diagnostics).
    suffixes: HashMap<usize, (SharedString, SharedString)>,
    /// `line -> columns` of virtual left indentation (display-only), the Org
    /// `org-indent-mode` look. The buffer text is untouched.
    indents: HashMap<usize, usize>,
    /// `line -> font-size multiplier` for whole-line scaling (markdown/org
    /// headings render larger). A scaled line reserves extra display rows the
    /// same way an inline image does, so cursor/scroll math stays correct.
    scales: HashMap<usize, f32>,
    /// `line -> is_open` fold chevrons painted in the gutter. Scheme declares a
    /// marker on each foldable header line (`set-fold-marker`); the value is the
    /// glyph state (open = expanded). A click on the chevron is hit-tested and
    /// routed to Scheme's `fold-toggle-at-line`, which mutates `folds` to match.
    fold_markers: HashMap<usize, bool>,
}

/// Re-key a line-indexed overlay map after `line_delta` lines were inserted
/// (positive) or removed (negative) starting just after `start_line`. Entries on
/// or before `start_line` keep their line; later ones shift; any that would land
/// at/before `start_line` (deleted out) are dropped.
fn shift_line_map<V>(map: &mut HashMap<usize, V>, start_line: usize, line_delta: isize) {
    if line_delta == 0 {
        return;
    }
    // Re-key only the entries past the edit, in place. Process in an order that
    // never overwrites an entry we haven't moved yet: highest-first when shifting
    // up, lowest-first when shifting down. Entries shifted to at/before
    // `start_line` (deleted out) are dropped.
    let mut movers: Vec<usize> = map.keys().copied().filter(|&l| l > start_line).collect();
    if line_delta > 0 {
        movers.sort_unstable_by(|a, b| b.cmp(a));
    } else {
        movers.sort_unstable();
    }
    for line in movers {
        let Some(v) = map.remove(&line) else { continue };
        let shifted = line as isize + line_delta;
        if shifted > start_line as isize {
            map.insert(shifted as usize, v);
        }
    }
}

/// Merge runs of consecutive [`EditorCommand::Insert`] into a single insert.
/// Each applied insert triggers a tree-sitter reparse and an LSP incremental
/// change, so a script inserting one character at a time otherwise pays that
/// cost per character. Only strictly-adjacent inserts merge — any other command
/// is a barrier — and the merge is byte-for-byte equivalent to applying them in
/// sequence (each insert lands at point, which the previous one advanced).
fn coalesce_inserts(commands: Vec<EditorCommand>) -> Vec<EditorCommand> {
    let mut out: Vec<EditorCommand> = Vec::with_capacity(commands.len());
    for command in commands {
        match (out.last_mut(), command) {
            (Some(EditorCommand::Insert(prev)), EditorCommand::Insert(next)) => {
                prev.push_str(&next);
            }
            (_, command) => out.push(command),
        }
    }
    out
}

/// One window pane: a view onto a buffer with its own point and scroll. The
/// selected pane's buffer is the live active buffer; other panes render from
/// their buffer's inactive [`Document`]. This is the Emacs model — buffers are a
/// shared pool, windows are views — adapted to Macros' single-active-buffer
/// editor: only the selected pane is "live" (editable); switching panes
/// (`other-window`) makes that pane's buffer active.
/// An in-progress divider drag: which split (by path from the layout root) and
/// its axis (`vertical` = the divider is horizontal / stacked split).
#[derive(Clone)]
struct ResizeDrag {
    path: Vec<bool>,
    vertical: bool,
}

struct Pane {
    id: usize,
    buffer: String,
    /// Saved point/scroll, used while this pane isn't the selected one.
    point: usize,
    scroll: Pixels,
}

/// The split layout: a binary tree whose leaves are pane ids. `ratio` is the
/// fraction of the split's extent given to child `a` (the other gets `1 - ratio`);
/// resize commands nudge it.
enum WindowLayout {
    Leaf(usize),
    Split {
        /// `true` = stacked (split below), `false` = side by side (split right).
        vertical: bool,
        /// Fraction `[0.1, 0.9]` of the area given to `a`.
        ratio: f32,
        a: Box<WindowLayout>,
        b: Box<WindowLayout>,
    },
}

/// Whether `layout`'s subtree contains pane `id`.
fn subtree_contains(layout: &WindowLayout, id: usize) -> bool {
    let mut leaves = Vec::new();
    layout_leaves(layout, &mut leaves);
    leaves.contains(&id)
}

/// Nudge the ratio of the deepest `vertical`-matching split on the path to
/// `pane`, enlarging the side that contains it by `delta`. Returns whether a
/// matching split was found.
fn layout_resize(layout: &mut WindowLayout, pane: usize, vertical: bool, delta: f32) -> bool {
    let WindowLayout::Split {
        vertical: v,
        ratio,
        a,
        b,
    } = layout
    else {
        return false;
    };
    let in_a = subtree_contains(a, pane);
    let child = if in_a { a } else { b };
    // Prefer a deeper matching split; otherwise adjust this one.
    if layout_resize(child, pane, vertical, delta) {
        return true;
    }
    if *v == vertical {
        let signed = if in_a { delta } else { -delta };
        *ratio = (*ratio + signed).clamp(0.1, 0.9);
        true
    } else {
        false
    }
}

/// Parse a yasnippet template into its plain text plus its tab-stop fields as
/// `(field_number, byte_offset_in_text, placeholder_len)`. Supports `$1`/`$2`/…,
/// `$0` (final stop), and `${1:default}` (placeholder text). Fields are returned
/// in tab order: ascending number, with `$0` last. An unrecognized `$` is kept
/// literally.
fn parse_snippet(template: &str) -> (String, Vec<(u32, usize, usize)>) {
    let mut text = String::new();
    let mut fields: Vec<(u32, usize, usize)> = Vec::new();
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            text.push(c);
            continue;
        }
        match chars.peek() {
            Some('{') => {
                chars.next();
                let mut num = String::new();
                while let Some(d) = chars.peek().filter(|d| d.is_ascii_digit()) {
                    num.push(*d);
                    chars.next();
                }
                let mut default = String::new();
                if chars.peek() == Some(&':') {
                    chars.next();
                    while let Some(&d) = chars.peek() {
                        chars.next();
                        if d == '}' {
                            break;
                        }
                        default.push(d);
                    }
                } else if chars.peek() == Some(&'}') {
                    chars.next();
                }
                if let Ok(n) = num.parse::<u32>() {
                    fields.push((n, text.len(), default.len()));
                    text.push_str(&default);
                }
            }
            Some(d) if d.is_ascii_digit() => {
                let mut num = String::new();
                while let Some(d) = chars.peek().filter(|d| d.is_ascii_digit()) {
                    num.push(*d);
                    chars.next();
                }
                if let Ok(n) = num.parse::<u32>() {
                    fields.push((n, text.len(), 0));
                }
            }
            _ => text.push('$'),
        }
    }
    fields.sort_by_key(|&(n, _, _)| if n == 0 { u32::MAX } else { n });
    (text, fields)
}

/// The closing partner for an opening delimiter, if `c` opens a pair.
fn open_delim_partner(c: char) -> Option<&'static str> {
    match c {
        '(' => Some(")"),
        '[' => Some("]"),
        '{' => Some("}"),
        '"' => Some("\""),
        '\'' => Some("'"),
        '`' => Some("`"),
        _ => None,
    }
}

/// Whether `c` is a closing delimiter electric-pair may "type over".
fn is_close_delim(c: char) -> bool {
    matches!(c, ')' | ']' | '}' | '"' | '\'' | '`')
}

/// Byte range [start,end) spanning lines [s,e) of `buf` (end-exclusive line
/// indices; `e` past the last line clamps to end-of-buffer). The byte start of
/// line `e` is just past line `e-1`'s break, so the range includes that break.
fn line_span(buf: &Buffer, s: usize, e: usize) -> (usize, usize) {
    let lc = buf.line_count();
    let sb = if s >= lc {
        buf.len()
    } else {
        buf.line(s).0.start
    };
    let eb = if e >= lc {
        buf.len()
    } else {
        buf.line(e).0.start
    };
    (sb, eb)
}

/// The subtree at `path` (a sequence of `false`=a / `true`=b descents from the
/// root), or `None` if the path doesn't lead to a node.
fn node_at<'a>(layout: &'a WindowLayout, path: &[bool]) -> Option<&'a WindowLayout> {
    match path.split_first() {
        None => Some(layout),
        Some((&go_b, rest)) => match layout {
            WindowLayout::Split { a, b, .. } => node_at(if go_b { b } else { a }, rest),
            WindowLayout::Leaf(_) => None,
        },
    }
}

/// Set the ratio of the Split node at `path` (clamped). Returns whether it hit a
/// Split there.
fn set_ratio_at(layout: &mut WindowLayout, path: &[bool], new_ratio: f32) -> bool {
    match path.split_first() {
        None => match layout {
            WindowLayout::Split { ratio, .. } => {
                *ratio = new_ratio.clamp(0.1, 0.9);
                true
            }
            WindowLayout::Leaf(_) => false,
        },
        Some((&go_b, rest)) => match layout {
            WindowLayout::Split { a, b, .. } => {
                set_ratio_at(if go_b { b } else { a }, rest, new_ratio)
            }
            WindowLayout::Leaf(_) => false,
        },
    }
}

/// Bounding box of a set of rects, or `None` if empty.
fn union_bounds(rects: &[Bounds<Pixels>]) -> Option<Bounds<Pixels>> {
    let first = *rects.first()?;
    let mut left = first.left();
    let mut top = first.top();
    let mut right = first.right();
    let mut bottom = first.bottom();
    for r in &rects[1..] {
        left = left.min(r.left());
        top = top.min(r.top());
        right = right.max(r.right());
        bottom = bottom.max(r.bottom());
    }
    Some(Bounds::from_corners(point(left, top), point(right, bottom)))
}

/// Convert parsed [`HelmSourceSpec`]s into runtime [`HelmSource`]s.
fn helm_sources_from_specs(specs: Vec<crate::script::HelmSourceSpec>) -> Vec<HelmSource> {
    specs
        .into_iter()
        .map(|s| {
            let (candidates, annotations, dynamic, raw) = match s.candidates {
                crate::script::HelmCandidates::Static(pairs) => {
                    let mut annotations = HashMap::new();
                    let candidates = pairs
                        .into_iter()
                        .map(|(c, ann)| {
                            if let Some(ann) = ann {
                                annotations.insert(c.clone(), ann);
                            }
                            c
                        })
                        .collect();
                    (candidates, annotations, None, false)
                }
                crate::script::HelmCandidates::Dynamic(f) => {
                    (Vec::new(), HashMap::new(), Some(f), false)
                }
                crate::script::HelmCandidates::DynamicRaw(f) => {
                    (Vec::new(), HashMap::new(), Some(f), true)
                }
            };
            HelmSource {
                name: s.name,
                candidates,
                annotations,
                dynamic,
                raw,
                action: HelmAction::Scheme(s.actions),
                preview: s.preview,
            }
        })
        .collect()
}

/// Flatten Helm sources to `(source_index, candidate)` matching `pattern`:
/// nucleo fuzzy ranking (best-first within each source) when `fuzzy`, else
/// case-insensitive substring; an empty pattern keeps everything in order.
fn helm_filter(sources: &[HelmSource], pattern: &str, fuzzy: bool) -> Vec<(usize, String)> {
    let empty = pattern.is_empty();
    // Fuzzy machinery, built once and only when a non-raw source will need it.
    let mut fuzzy_ctx = (!empty && fuzzy).then(|| {
        (
            Matcher::new(Config::DEFAULT),
            Pattern::parse(pattern, CaseMatching::Smart, Normalization::Smart),
        )
    });
    let needle = (!empty && !fuzzy).then(|| pattern.to_lowercase());
    let mut out = Vec::new();
    for (i, source) in sources.iter().enumerate() {
        // An empty pattern keeps everything; a `raw` source self-filtered in
        // Scheme, so it too is shown verbatim, in the order the fn returned.
        if empty || source.raw {
            out.extend(source.candidates.iter().map(|c| (i, c.clone())));
        } else if let Some((matcher, parsed)) = fuzzy_ctx.as_mut() {
            for (candidate, _score) in parsed.match_list(source.candidates.iter(), matcher) {
                out.push((i, candidate.clone()));
            }
        } else if let Some(needle) = &needle {
            for candidate in &source.candidates {
                if candidate.to_lowercase().contains(needle) {
                    out.push((i, candidate.clone()));
                }
            }
        }
    }
    out
}

/// Emacs-style file prompt shortcuts for Helm file sources. When a file picker
/// is pre-filled with the current directory, typing `/` after that trailing `/`
/// produces `//`; the second slash shadows everything before it and jumps to
/// the filesystem root.
fn helm_file_shadow_pattern(sources: &[HelmSource], pattern: &str) -> Option<String> {
    let is_file_source = sources
        .iter()
        .any(|s| s.dynamic.as_deref() == Some("find-file-candidates"));
    if !is_file_source {
        return None;
    }
    let (_, tail) = pattern.rsplit_once("//")?;
    Some(format!("/{tail}"))
}

/// For Helm file pickers, `RET` on text that is not an exact listed candidate
/// should visit that typed path. This is how `find-file` creates a file-visiting
/// empty buffer for a new file; directories still use candidate completion.
fn helm_file_accept_target(sources: &[HelmSource], pattern: &str) -> Option<(HelmAction, String)> {
    let pattern = pattern.trim();
    if pattern.is_empty() || pattern.ends_with('/') {
        return None;
    }
    let source = sources
        .iter()
        .find(|s| s.dynamic.as_deref() == Some("find-file-candidates"))?;
    if source.candidates.iter().any(|c| c == pattern) {
        return None;
    }
    if Path::new(&expand_path(pattern)).is_dir() {
        return None;
    }
    Some((source.action.clone(), pattern.to_string()))
}

/// Cycle a selection index by one step with wraparound over `count` items.
/// `forward` advances, otherwise it steps back. Returns 0 if `count` is 0.
fn wrap_index(selected: usize, count: usize, forward: bool) -> usize {
    if count == 0 {
        return 0;
    }
    let step = if forward { 1 } else { count - 1 };
    (selected + step) % count
}

/// In-order list of pane ids (left-to-right / top-to-bottom) — the `other-window`
/// cycle order.
fn layout_leaves(layout: &WindowLayout, out: &mut Vec<usize>) {
    match layout {
        WindowLayout::Leaf(id) => out.push(*id),
        WindowLayout::Split { a, b, .. } => {
            layout_leaves(a, out);
            layout_leaves(b, out);
        }
    }
}

/// Replace the leaf for `target` with a split of `target` (ratio `split_ratio`)
/// and `new_id`.
fn layout_split(
    layout: WindowLayout,
    target: usize,
    vertical: bool,
    new_id: usize,
    split_ratio: f32,
) -> WindowLayout {
    match layout {
        WindowLayout::Leaf(id) if id == target => WindowLayout::Split {
            vertical,
            ratio: split_ratio,
            a: Box::new(WindowLayout::Leaf(id)),
            b: Box::new(WindowLayout::Leaf(new_id)),
        },
        WindowLayout::Leaf(id) => WindowLayout::Leaf(id),
        WindowLayout::Split {
            vertical: v,
            ratio,
            a,
            b,
        } => WindowLayout::Split {
            vertical: v,
            ratio,
            a: Box::new(layout_split(*a, target, vertical, new_id, split_ratio)),
            b: Box::new(layout_split(*b, target, vertical, new_id, split_ratio)),
        },
    }
}

/// Remove the leaf `id`, collapsing its parent split to the sibling. `None` if
/// `id` was the whole layout.
fn layout_remove(layout: WindowLayout, id: usize) -> Option<WindowLayout> {
    match layout {
        WindowLayout::Leaf(x) if x == id => None,
        WindowLayout::Leaf(x) => Some(WindowLayout::Leaf(x)),
        WindowLayout::Split {
            vertical,
            ratio,
            a,
            b,
        } => match (layout_remove(*a, id), layout_remove(*b, id)) {
            (Some(a), Some(b)) => Some(WindowLayout::Split {
                vertical,
                ratio,
                a: Box::new(a),
                b: Box::new(b),
            }),
            (Some(only), None) | (None, Some(only)) => Some(only),
            (None, None) => None,
        },
    }
}

/// State for the floating completion popup: the full candidate set, where the
/// completed word starts, and which (filtered) row is selected. The visible set
/// is `items` filtered by the live prefix `buffer[anchor..point]`.
struct CompletionPopup {
    items: Vec<CompletionItem>,
    anchor: usize,
    selected: usize,
}

/// One rendered completion row: the label plus the optional signature/kind the
/// element shows according to the Scheme `completion-show-*` options.
pub struct CompletionRow {
    pub label: String,
    pub detail: Option<String>,
    pub kind: Option<String>,
}

/// A render snapshot of the completion popup: the visible rows, which one is
/// selected, and the selected item's documentation (resolved lazily).
pub struct CompletionView {
    pub items: Vec<CompletionRow>,
    pub selected: usize,
    pub doc: Option<String>,
}

/// The painted completion popup's clickable geometry, reported back from the
/// element each frame so `on_mouse_down` can hit-test a click to a row.
#[derive(Clone, Copy)]
pub struct PopupHit {
    /// The rows region in window coordinates (excludes the doc side-panel).
    pub bounds: Bounds<Pixels>,
    /// Height of one row.
    pub row_height: Pixels,
    /// Filtered-item index of the first visible row.
    pub start: usize,
    /// Number of visible rows.
    pub rows: usize,
}

/// A painted fold chevron's clickable region, reported by the element each frame
/// so `on_mouse_down` can route a gutter click to `(fold-toggle-at-line line)`.
#[derive(Clone, Copy)]
pub struct FoldHitbox {
    /// The chevron's bounds in window coordinates.
    pub bounds: Bounds<Pixels>,
    /// The buffer line the chevron sits on.
    pub line: usize,
}

/// What running a Helm candidate does.
#[derive(Clone)]
enum HelmAction {
    /// `(label, function-name)` pairs — the first is the default (RET); the rest
    /// are reachable via the TAB action menu. The function is called with the
    /// chosen candidate string.
    Scheme(Vec<(String, String)>),
    /// Treat the candidate itself as a command name and run it (helm-M-x).
    ExecuteCommand,
    /// An action-menu session: the candidate is an action label, resolved
    /// against `helm_action_context`.
    ActionMenu,
    /// Jump to one of a set of LSP locations. The vec is parallel to the
    /// source's `candidates`; the accepted candidate's index selects the target.
    GotoLocation(Vec<lsp::Location>),
    /// Apply one of a set of LSP code actions. The vec is parallel to the
    /// source's `candidates`; the accepted candidate's index selects the action.
    ApplyCodeAction(Vec<lsp::CodeAction>),
}

/// One group of candidates in a Helm session.
struct HelmSource {
    name: String,
    /// Current candidates — for a dynamic source, the latest query result.
    candidates: Vec<String>,
    /// Optional marginalia-style annotation per candidate (candidate → text),
    /// shown dimmed to the right of the row. Candidates absent from the map have
    /// no annotation.
    annotations: HashMap<String, String>,
    /// Name of a Scheme fn that (re)computes candidates from the pattern; `None`
    /// for static sources.
    dynamic: Option<String>,
    /// When set, the source's candidates are shown verbatim — Rust does no
    /// pattern filtering (the dynamic fn already did, e.g. via `fuzzy-match`).
    raw: bool,
    action: HelmAction,
    /// Name of a Scheme fn that returns the preview (as lines) for a candidate;
    /// `None` = this source has no preview pane.
    preview: Option<String>,
}

/// An active Helm selection session (a generic completing-read UI): a prompt, a
/// live filter `pattern`, the sources, the selected match index, and the
/// `query_id` that tags in-flight dynamic-candidate requests.
struct HelmSession {
    prompt: String,
    pattern: String,
    sources: Vec<HelmSource>,
    selected: usize,
    query_id: u64,
    /// Fuzzy (nucleo) vs substring matching, captured from settings at open.
    fuzzy: bool,
    /// Candidates marked for multi-select (`C-SPC`). When non-empty, accepting
    /// runs the action once per marked candidate instead of the selected one.
    marked: Vec<String>,
    /// Preview text for the current selection (from the source's preview fn),
    /// shown in a side pane. Empty when the selected source has no preview.
    preview: String,
    /// Query id of the in-flight preview request, so a stale result is dropped.
    preview_query: u64,
}

/// One rendered line of the Helm panel: a source header or a candidate.
pub struct HelmRow {
    pub text: String,
    /// Marginalia-style annotation shown dimmed to the right (candidates only).
    pub annotation: Option<String>,
    pub header: bool,
    pub selected: bool,
    pub marked: bool,
    /// Index into the flattened match list, for click-to-select (candidate rows
    /// only; headers carry `None`).
    pub index: Option<usize>,
}

/// A render snapshot of the Helm panel.
pub struct HelmView {
    pub prompt: String,
    pub pattern: String,
    pub rows: Vec<HelmRow>,
    /// Preview text for the current selection, shown in a side pane (`None` when
    /// the selected source has no preview fn).
    pub preview: Option<String>,
}

/// Evil (modal editing) state. `Normal`/`Visual` intercept plain keys as
/// commands; `Insert` is ordinary self-inserting editing; `Disabled` is plain
/// Emacs behaviour.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EvilState {
    Disabled,
    Normal,
    Insert,
    Visual,
}

/// A single running language server and the per-server state we track for it.
struct LspServer {
    client: LspBackend,
    /// True once the `initialize` response has arrived.
    ready: bool,
    /// Server's preferred sync kind: 1 = full, 2 = incremental.
    sync_kind: u8,
    /// URIs we've sent `didOpen` for to this server.
    open: HashSet<String>,
    versions: HashMap<String, i32>,
    /// Pending completion request: (id, word range being completed).
    pending_completion: Option<(i64, Range<usize>)>,
    /// In-flight non-completion requests (definition, references, rename, …),
    /// keyed by request id so the response can be routed to the right handler.
    pending: HashMap<i64, PendingRequest>,
    /// `semanticTokens` legend (token-type names by id) from `initialize`.
    semantic_legend: Vec<String>,
    /// Whether the server supports `prepareRename` (gates the pre-check).
    prepare_rename: bool,
    /// Cached semantic tokens per URI: `(resultId, packed data)`, for deltas.
    semantic_state: HashMap<String, (Option<String>, Vec<i64>)>,
    /// The reader task forwarding this server's messages; dropped on removal.
    _task: Task<()>,
}

/// What a pending LSP request was for — picked up in `on_lsp_message` to route
/// the response to the right handler.
#[derive(Clone, Debug)]
enum PendingRequest {
    /// A Scheme-initiated request: deliver the raw result JSON to the named
    /// Steel callback. This is the generic bridge powering `(lsp-request …)`.
    Script(String),
    /// Go to a location: definition / declaration / type-definition /
    /// implementation. The label is shown if the result is a list.
    Goto(&'static str),
    /// A list of references / document highlights → the `*lsp-xref*` buffer.
    References,
    /// Document symbols → the `*lsp-symbols*` buffer.
    DocumentSymbols,
    /// Workspace symbols → the `*lsp-symbols*` buffer.
    WorkspaceSymbols,
    /// Hover → echo area (minibuffer).
    Hover,
    /// Signature help → echo.
    SignatureHelp,
    /// Rename → apply the resulting workspace edit.
    Rename,
    /// Formatting → apply the resulting text edits to the active buffer.
    Format,
    /// Code actions → the `*lsp-actions*` buffer.
    CodeAction,
    /// `prepareCallHierarchy` done → send the incoming/outgoing-calls request
    /// (true = incoming). The prepared item is echoed back verbatim.
    CallHierarchyPrepare(bool),
    /// `incomingCalls`/`outgoingCalls` result → the `*lsp-xref*` buffer.
    CallHierarchyCalls(bool),
    /// A semantic-tokens result (full / range / delta) → decode + deliver
    /// records to the named Steel callback.
    SemanticTokens {
        callback: String,
        kind: SemanticKind,
    },
    /// `prepareRename` result → open the rename prompt (or reject the position).
    PrepareRename,
    /// A `completionItem/resolve` result for the item with this label → fill in
    /// its documentation / auto-import edits.
    ResolveCompletion(String),
    /// `textDocument/codeLens` result → resolve + render the lenses.
    CodeLens,
    /// A `codeLens/resolve` result for the lens at this (line, character).
    ResolveCodeLens(usize, usize),
}

/// Which semantic-tokens request a [`PendingRequest::SemanticTokens`] is for.
#[derive(Clone, Copy, Debug)]
enum SemanticKind {
    /// `semanticTokens/full` — caches the resultId + data for later deltas.
    Full,
    /// `semanticTokens/range` — the viewport window; one-shot, no caching.
    Range,
    /// `semanticTokens/full/delta` — merge edits into the cached data.
    Delta,
}

impl Editor {
    pub fn new(cx: &mut Context<Self>) -> Self {
        // Keymaps are populated from Scheme at startup; the modes just need to
        // exist. `steel-mode` (the major mode) declares the highlighting
        // language and stacks on the always-present `fundamental` base.
        let mut modes = ModeStack::default();
        modes.push(Mode::new("fundamental"));
        modes.push(Mode::new("steel").with_language("scheme"));

        // The active highlighter follows the active mode's language.
        let highlighter = modes.active_language().and_then(Highlighter::for_language);

        let welcome = ";; Macros — the Emacs you know, with a modern engine\n\
                       ;; Movement: C-f/C-b/C-n/C-p, C-a/C-e, M-f/M-b\n\
                       ;; Kill/yank: C-k, C-w, M-w, C-y; mark: C-Space; undo: C-/\n\
                       ;; steel-mode: C-c C-e eval-last-sexp, C-c C-b eval-buffer\n\n";

        let (script, mut script_outcomes) = ScriptEngine::new();
        let (anvil, mut anvil_outcomes) = AnvilEngine::new();
        let (term_events_tx, mut term_events_rx) =
            futures::channel::mpsc::unbounded::<(u64, TermEvent)>();
        let (lsp_flush_tx, mut lsp_flush_rx) = futures::channel::mpsc::unbounded::<()>();

        let mut editor = Self {
            buffer: Buffer::new(welcome),
            focus_handle: cx.focus_handle(),
            modes,
            script,
            script_task: None,
            anvil,
            anvil_task: None,
            keymap: Keymap::default(),
            status: "Macros ready".into(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            is_selecting: false,
            messages: Vec::new(),
            lsp_log: Vec::new(),
            hover_message: None,
            mark_active: false,
            modified: false,
            hooks_ran: false,
            electric_pair: false,
            snippet_fields: Vec::new(),
            snippet_index: 0,
            avy_hints: Vec::new(),
            extra_cursors: Vec::new(),
            hooks: HashMap::new(),
            last_hook_line: None,
            capf_functions: Vec::new(),
            kill_ring: Vec::new(),
            kill_index: 0,
            last_yank: None,
            last_was_yank: false,
            undo_tree: new_undo_tree(),
            undo_current: 0,
            undo_grouping: false,
            undo_group_started: false,
            pending_keys: Vec::new(),
            minibuffer: None,
            completing_candidates: Vec::new(),
            minibuffer_history: Vec::new(),
            minibuffer_history_pos: None,
            minibuffer_stash: String::new(),
            mb_dynamic: None,
            uarg_reading: false,
            uarg_digits: None,
            uarg_cu: 0,
            prefix_arg: None,
            autoscrolling: false,
            autoscroll_dy: 0.0,
            autoscroll_pos: point(px(0.), px(0.)),
            scroll_offset: px(0.),
            h_scroll_offset: px(0.),
            viewport_width: px(0.),
            char_width: px(8.),
            word_wrap: true,
            lines: Vec::new(),
            last_bounds: None,
            line_height: px(0.),
            viewport_height: px(0.),
            highlighter,
            active_name: "*scratch*".to_string(),
            active_path: None,
            process_cwd: std::env::current_dir()
                .map(|d| d.to_string_lossy().into_owned())
                .unwrap_or_default(),
            inactive: Vec::new(),
            lsp_commands: HashMap::new(),
            extensions: HashMap::new(),
            language_ids: HashMap::new(),
            servers: HashMap::new(),
            diagnostics: HashMap::new(),
            xref_targets: Vec::new(),
            xref_marker_stack: Vec::new(),
            code_actions: Vec::new(),
            lsp_format_on_save: HashSet::new(),
            formatting_for_save: false,
            completion_items: Vec::new(),
            completion_range: None,
            completion_popup: None,
            completion_auto_pending: false,
            helm: None,
            helm_query_counter: 0,
            helm_action_context: None,
            evil: EvilState::Disabled,
            search_query: None,
            evil_search_forward: true,
            evil_yank_linewise: false,
            isearch_forward: true,
            isearch_origin: 0,
            isearch_from: 0,
            isearch_match: None,
            last_isearch: None,
            recenter_cycle: 0,
            recenter_point: usize::MAX,
            special: None,
            line_faces: HashMap::new(),
            line_icons: HashMap::new(),
            line_images: HashMap::new(),
            image_cache: HashMap::new(),
            overlays: Overlays::default(),
            locals: HashMap::new(),
            transient: None,
            project_indicator: None,
            panes: vec![Pane {
                id: 0,
                buffer: "*scratch*".to_string(),
                point: 0,
                scroll: px(0.),
            }],
            layout: WindowLayout::Leaf(0),
            selected_pane: 0,
            next_pane_id: 1,
            pane_bounds: Vec::new(),
            completion_popup_hit: None,
            fold_hitboxes: Vec::new(),
            resizing: None,
            processes: HashMap::new(),
            terminals: HashMap::new(),
            term_events_tx,
            lsp_fullsync_dirty: None,
            lsp_flush_tx,
            terminal_task: None,
            terminal_geom: HashMap::new(),
            terminal_drag: None,
            macro_recording: None,
            last_macro: Vec::new(),
            cursor_blink_visible: true,
            blink_token: 0,
            window_active: true,
            blink_observer: None,
        };
        // Drain async script outcomes from the engine thread onto the foreground.
        editor.script_task = Some(cx.spawn(async move |this, cx| {
            while let Some(outcome) = script_outcomes.next().await {
                if this
                    .update(cx, |editor, cx| editor.apply_script_outcome(outcome, cx))
                    .is_err()
                {
                    break; // editor dropped
                }
            }
        }));
        // Drain ANVIL REPL results from its engine thread onto the foreground.
        editor.anvil_task = Some(cx.spawn(async move |this, cx| {
            while let Some(outcome) = anvil_outcomes.next().await {
                if this
                    .update(cx, |editor, cx| editor.apply_anvil_outcome(outcome, cx))
                    .is_err()
                {
                    break; // editor dropped
                }
            }
        }));
        // Coalesce full-sync LSP did_change: when a burst of edits wakes us, wait
        // for typing to settle, then send the whole document once.
        cx.spawn(async move |this, cx| {
            while lsp_flush_rx.next().await.is_some() {
                cx.background_executor()
                    .timer(Duration::from_millis(150))
                    .await;
                // Swallow signals that arrived during the debounce window — the
                // single flush below already covers them.
                while lsp_flush_rx.try_recv().is_ok() {}
                if this
                    .update(cx, |editor, _| editor.flush_lsp_fullsync())
                    .is_err()
                {
                    break; // editor dropped
                }
            }
        })
        .detach();
        // Drain terminal events (redraw / title / exit) from the alacritty event
        // loops onto the foreground.
        editor.terminal_task = Some(cx.spawn(async move |this, cx| {
            while let Some((id, event)) = term_events_rx.next().await {
                if this
                    .update(cx, |editor, cx| editor.on_terminal_event(id, event, cx))
                    .is_err()
                {
                    break; // editor dropped
                }
            }
        }));
        editor.parse_full();
        editor.sync_buffers();
        // Start the cursor-blink loop now, so the cursor blinks from the first
        // frame (not only after the first keystroke). No-op when blinking is off.
        editor.schedule_blink(cx);
        editor
    }

    /// Apply what a dispatched script sent back: commands + echo, or refreshed
    /// dynamic Helm candidates.
    fn apply_script_outcome(&mut self, outcome: ScriptOutcome, cx: &mut Context<Self>) {
        match outcome {
            ScriptOutcome::Commands { commands, message } => {
                // All edits from one Scheme command batch undo as one unit.
                self.begin_undo_group();
                // Merge runs of adjacent `Insert`s so a script that inserts
                // character-by-character pays one tree-sitter reparse + LSP
                // did_change instead of one per character. Safe only when no
                // `after-change-functions` hook would observe the per-edit
                // granularity; undo is unaffected (the batch is one group).
                let commands = if self.has_active_hook("after-change-functions") {
                    commands
                } else {
                    coalesce_inserts(commands)
                };
                for command in commands {
                    self.apply_command(command, cx);
                }
                self.end_undo_group();
                if let Some(message) = message {
                    self.echo(message, cx);
                }
            }
            ScriptOutcome::Candidates {
                source,
                query_id,
                candidates,
            } => {
                // `usize::MAX` is the minibuffer's dynamic `completing-read`;
                // real source indices belong to Helm.
                if source == usize::MAX {
                    // The minibuffer has no annotation column — keep just the
                    // candidate text.
                    let names = candidates.into_iter().map(|(c, _)| c).collect();
                    self.mb_set_dynamic_candidates(query_id, names, cx);
                } else if source == usize::MAX - 1 {
                    // Helm preview lines (candidate text only).
                    let lines = candidates.into_iter().map(|(c, _)| c).collect();
                    self.helm_set_preview(query_id, lines, cx);
                } else {
                    self.helm_set_candidates(source, query_id, candidates, cx);
                }
            }
            ScriptOutcome::Capf { start, end, items } => {
                self.apply_capf_result(start, end, items, cx);
            }
        }
    }

    /// Deliver a finished ANVIL REPL evaluation to its Scheme callback. The
    /// result is rendered as a single string (errors keep the `error: …` prefix
    /// the config engine uses), and handed to the named callback in the *main*
    /// engine, whose `(append-to-buffer …)` / `(message …)` then flows back
    /// through the normal outcome drain — exactly like a subprocess callback.
    fn apply_anvil_outcome(&mut self, outcome: AnvilOutcome, cx: &mut Context<Self>) {
        match outcome {
            AnvilOutcome::Result {
                callback,
                value,
                error,
                output,
            } => {
                // Captured `(display …)` output prints first, then the value (or
                // error), matching a REPL transcript.
                let mut payload = String::new();
                if let Some(out) = output.filter(|o| !o.is_empty()) {
                    payload.push_str(out.trim_end_matches('\n'));
                    payload.push('\n');
                }
                match error {
                    Some(error) => payload.push_str(&format!("error: {error}")),
                    None => payload.push_str(&value.unwrap_or_default()),
                }
                self.script.dispatch_action(&callback, Some(&payload));
            }
            // Image-backed completion candidates → the normal completion popup.
            // Empty means no live binding matches; show "No completions" rather
            // than falling through to LSP (the REPL has no language server).
            AnvilOutcome::Completions { start, end, items } => {
                let len = self.buffer.len();
                let range = start.min(len)..end.min(len);
                let items = items
                    .into_iter()
                    .map(|label| CompletionItem {
                        insert_text: label.clone(),
                        label,
                        detail: None,
                        kind: None,
                        documentation: None,
                        additional_edits: Vec::new(),
                        data: serde_json::Value::Null,
                        resolved: true,
                    })
                    .collect();
                self.show_completion(items, range, cx);
            }
        }
    }

    // --- Accessors used by the element ------------------------------------

    pub fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    pub fn set_layout(
        &mut self,
        lines: Vec<LineLayout>,
        bounds: Bounds<Pixels>,
        line_height: Pixels,
        char_width: Pixels,
        word_wrap: bool,
    ) {
        self.lines = lines;
        self.last_bounds = Some(bounds);
        self.line_height = line_height;
        self.viewport_height = bounds.size.height;
        self.viewport_width = bounds.size.width;
        self.char_width = char_width;
        self.word_wrap = word_wrap;
    }

    /// Record (or clear) the completion popup's painted geometry, so a click can
    /// be hit-tested to a candidate row in `on_mouse_down`.
    pub fn report_completion_popup(&mut self, hit: Option<PopupHit>) {
        self.completion_popup_hit = hit;
    }

    /// Record the fold chevrons' painted regions (selected pane), so a click in
    /// the gutter can be hit-tested to a fold line in `on_mouse_down`.
    pub fn report_fold_hitboxes(&mut self, hits: Vec<FoldHitbox>) {
        self.fold_hitboxes = hits;
    }

    /// Horizontal scroll of the selected pane (0 when word-wrap is on).
    pub fn h_scroll(&self) -> Pixels {
        self.h_scroll_offset
    }

    fn point(&self) -> usize {
        self.cursor_offset()
    }

    // --- Cursor / selection movement --------------------------------------

    /// Move point to `offset`, extending the region if the mark is active.
    fn goto(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.mark_active {
            self.select_to(offset, cx);
        } else {
            self.move_to(offset, cx);
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        self.reveal_point();
        self.ensure_cursor_visible();
        if let Some(message) = self.diagnostic_at_point() {
            self.status = message.into();
        }
        self.run_post_command_hook();
        cx.notify();
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.selection_reversed {
            self.selected_range.start = offset;
        } else {
            self.selected_range.end = offset;
        }
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        self.ensure_cursor_visible();
        self.run_post_command_hook();
        cx.notify();
    }

    /// Fire `post-command-hook` when point's line changed since the last fire —
    /// the signal current-line decorations (e.g. inline blame) re-place on. The
    /// hook dispatches asynchronously (no reentrancy into cursor movement), and
    /// `run_hook` short-circuits when nothing is registered, so this is free
    /// unless a Scheme feature opts in.
    fn run_post_command_hook(&mut self) {
        if !self.has_active_hook("post-command-hook") {
            return;
        }
        let line = self.buffer.line_index_at_byte(self.point());
        if self.last_hook_line == Some(line) {
            return;
        }
        self.last_hook_line = Some(line);
        self.run_hook("post-command-hook");
    }

    /// Target offset `delta` lines away, preserving the character column.
    /// Computed from the buffer so it works off-screen. Folded (invisible) lines
    /// are skipped, so moving over a collapsed section lands on the next visible
    /// line rather than inside the fold (where the cursor wouldn't be painted).
    fn line_offset(&self, delta: isize) -> usize {
        let cursor = self.point();
        let line = self.buffer.line_index_at_byte(cursor);
        let target = if self.overlays.folds.is_empty() {
            (line as isize + delta).clamp(0, self.buffer.line_count() as isize - 1) as usize
        } else {
            self.visible_line_offset(line, delta)
        };
        if target == line {
            return cursor;
        }
        let line_start_char = self.buffer.byte_to_char(self.buffer.line_range(line).start);
        let goal = self.buffer.byte_to_char(cursor) - line_start_char;
        let tr = self.buffer.line_range(target);
        let ts = self.buffer.byte_to_char(tr.start);
        let tlen = self.buffer.byte_to_char(tr.end) - ts;
        self.buffer.char_to_byte(ts + goal.min(tlen))
    }

    /// The line `delta` *visible* lines from `line`, skipping folded lines and
    /// clamping to the buffer. If there's no visible line that far in the given
    /// direction, returns the last reachable visible line (so the cursor stops
    /// at the section edge instead of vanishing into a fold).
    fn visible_line_offset(&self, line: usize, delta: isize) -> usize {
        let last = self.buffer.line_count().saturating_sub(1) as isize;
        let step: isize = if delta >= 0 { 1 } else { -1 };
        let mut remaining = delta.unsigned_abs();
        let mut cur = line as isize;
        while remaining > 0 {
            let mut next = cur + step;
            while next >= 0 && next <= last && self.is_line_invisible(next as usize) {
                next += step;
            }
            if next < 0 || next > last {
                break; // no further visible line this way
            }
            cur = next;
            remaining -= 1;
        }
        cur as usize
    }

    /// Reveal (unfold) any fold hiding the line that now holds point, so a jump
    /// that lands inside a collapsed region — search, goto-line, xref, an LSP
    /// jump — makes the destination and the cursor visible. Vertical line motion
    /// skips folds instead of revealing them (see `line_offset`), so ordinary
    /// up/down navigation never triggers this. No-op when nothing is folded.
    fn reveal_point(&mut self) {
        if self.overlays.folds.is_empty() {
            return;
        }
        let line = self.buffer.line_index_at_byte(self.point());
        if self.is_line_invisible(line) {
            self.overlays.folds.retain(|r| !r.contains(&line));
        }
    }

    fn ensure_cursor_visible(&mut self) {
        let lh = f32::from(self.line_height);
        let vh = f32::from(self.viewport_height);
        if lh <= 0.0 || vh <= 0.0 {
            return;
        }
        // Position in display rows (folds collapsed) so scrolling stays correct
        // when lines are hidden.
        let buffer_line = self.buffer.line_index_at_byte(self.point());
        let line = self.display_row_of_line(buffer_line) as f32;
        let top = line * lh;
        let bottom = top + lh;
        let mut scroll = f32::from(self.scroll_offset);
        if top < scroll {
            scroll = top;
        } else if bottom > scroll + vh {
            scroll = bottom - vh;
        }
        // The cursor's row in display-row units (1 line == 1 line-height). Under
        // word-wrap a line can occupy several visual rows, so this *underestimates*
        // the cursor's true pixel position — `reconcile_cursor_scroll` corrects the
        // remainder after paint, using the real wrapped geometry. The clamp upper
        // bound therefore must not pull `scroll` back below a value that a prior
        // post-paint correction set (the buffer-line content height is itself an
        // underestimate when lines wrap), or the cursor would jump off-screen for a
        // frame on every edit.
        let content = self.display_line_count() as f32 * lh;
        let max = (content - vh).max(0.0).max(f32::from(self.scroll_offset));
        self.scroll_offset = px(scroll.clamp(0.0, max));
        self.ensure_cursor_visible_horizontal();
    }

    /// Correct the vertical scroll against the cursor's *real* painted position.
    ///
    /// [`ensure_cursor_visible`] works in buffer-line units, which underestimates
    /// the cursor's pixel position whenever lines above it (or its own line)
    /// soft-wrap into multiple visual rows. The renderer knows the exact geometry,
    /// so after each paint it reports the cursor's top, relative to the viewport,
    /// and we nudge the scroll by just enough to bring it fully into view. This is
    /// the authoritative scroll-follow; converges in one extra frame.
    pub fn reconcile_cursor_scroll(&mut self, cursor_top: Option<Pixels>, cx: &mut Context<Self>) {
        let Some(cursor_top) = cursor_top else {
            return;
        };
        let lh = f32::from(self.line_height);
        let vh = f32::from(self.viewport_height);
        if lh <= 0.0 || vh <= 0.0 {
            return;
        }
        let top = f32::from(cursor_top);
        let bottom = top + lh;
        let scroll = f32::from(self.scroll_offset);
        // Only nudge when the cursor falls outside the viewport. Going past the
        // buffer-line content height is fine here (it's an underestimate under
        // wrapping) — the lower clamp of 0 is the only hard bound.
        let target = if bottom > vh {
            scroll + (bottom - vh)
        } else if top < 0.0 {
            scroll + top
        } else {
            scroll
        };
        let target = target.max(0.0);
        if (target - scroll).abs() > 0.5 {
            self.scroll_offset = px(target);
            cx.notify();
        }
    }

    /// Keep the cursor within the visible columns when word-wrap is off, by
    /// scrolling horizontally. Approximates the cursor's x as (byte-column ×
    /// char-width) — exact for ASCII, close enough for scroll-follow otherwise.
    /// With word-wrap on, the buffer never scrolls horizontally.
    fn ensure_cursor_visible_horizontal(&mut self) {
        if self.word_wrap {
            self.h_scroll_offset = px(0.);
            return;
        }
        let cw = f32::from(self.char_width);
        let vw = f32::from(self.viewport_width);
        if cw <= 0.0 || vw <= 0.0 {
            return;
        }
        let line_start = self
            .buffer
            .line_range(self.buffer.line_index_at_byte(self.point()))
            .start;
        let cursor_x = (self.point() - line_start) as f32 * cw;
        let margin = cw; // keep a character's breathing room at the edge
        let mut h = f32::from(self.h_scroll_offset);
        if cursor_x < h + margin {
            h = (cursor_x - margin).max(0.0);
        } else if cursor_x > h + vw - margin {
            h = cursor_x - vw + margin;
        }
        self.h_scroll_offset = px(h.max(0.0));
    }

    fn offset_for_position(&self, position: Point<Pixels>, line_height: Pixels) -> usize {
        let Some(bounds) = self.last_bounds else {
            return 0;
        };
        // Map a click to a byte offset within a (possibly wrapped) line. `top` is
        // the line's top relative to the element; `wrapped` knows how to resolve a
        // local (x, y) — including which visual row y falls on — to a byte index.
        let hit = |line: &LineLayout| {
            let local = point(
                position.x - bounds.left() + self.h_scroll_offset - line.indent,
                position.y - bounds.top() - line.top,
            );
            let idx = match line.wrapped.closest_index_for_position(local, line_height) {
                Ok(i) | Err(i) => i,
            };
            // `idx` is a display index; map it back past any hidden ranges.
            line.range.start
                + line
                    .from_display(idx)
                    .min(line.range.end - line.range.start)
        };
        for line in &self.lines {
            let bottom = bounds.top() + line.top + line.height;
            if position.y < bottom {
                return hit(line);
            }
        }
        self.lines.last().map_or(0, hit)
    }

    /// Byte offset at the start of the line a click at vertical position `y`
    /// falls on in the selected pane — computed straight from the pane's painted
    /// bounds and scroll, *not* `self.lines`. Unlike [`offset_for_position`], this
    /// stays correct on the very frame a pane's buffer is swapped in (which clears
    /// `self.lines`), so a click that also focuses a special pane lands on the
    /// right line. Column is the line start; special-buffer commands act per-line.
    fn line_start_for_click_y(&self, y: Pixels, line_height: Pixels) -> Option<usize> {
        let top = self
            .pane_bounds
            .iter()
            .find(|(id, _)| *id == self.selected_pane)
            .map(|(_, b)| b.top())?;
        let rel = f32::from(y - top) + f32::from(self.scroll_offset);
        let row = (rel.max(0.0) / f32::from(line_height).max(1.0)) as usize;
        let last = self.buffer.line_count().saturating_sub(1);
        Some(self.buffer.line_range(row.min(last)).start)
    }

    /// Index of the cached visible line containing `offset`, if it's on screen.
    fn line_index_at(&self, offset: usize) -> Option<usize> {
        self.lines
            .iter()
            .position(|l| offset >= l.range.start && offset <= l.range.end)
    }

    // --- Editing primitives (all edits funnel through `edit` for undo) -----

    /// Shift overlays to track an edit that replaced `[start, old_end)` with
    /// `new_len` bytes (`line_delta` net newlines, the edit beginning on
    /// `start_line`). Byte-range overlays (faces, hidden, props) move with the
    /// text; whole-line overlays (indents, suffixes) shift by the line delta when
    /// lines were added/removed. Without this they'd point at stale positions
    /// until the debounced re-fontify, making content below an edit flicker.
    fn adjust_overlays(
        &mut self,
        start: usize,
        old_end: usize,
        new_len: usize,
        line_delta: isize,
        start_line: usize,
    ) {
        let delta = new_len as isize - (old_end - start) as isize;
        // A byte offset, mapped past the edit: before it is fixed, inside it
        // collapses to its start, after it shifts by the size delta.
        let shift = |pos: usize| -> usize {
            if pos <= start {
                pos
            } else if pos >= old_end {
                (pos as isize + delta).max(start as isize) as usize
            } else {
                start
            }
        };
        if delta != 0 {
            self.overlays.faces.retain_mut(|(r, _)| {
                *r = shift(r.start)..shift(r.end);
                r.end > r.start
            });
            self.overlays.hidden.retain_mut(|r| {
                *r = shift(r.start)..shift(r.end);
                r.end > r.start
            });
            self.overlays.props.retain_mut(|(r, _, _)| {
                *r = shift(r.start)..shift(r.end);
                r.end > r.start
            });
        }
        if line_delta != 0 {
            shift_line_map(&mut self.overlays.indents, start_line, line_delta);
            shift_line_map(&mut self.overlays.scales, start_line, line_delta);
            shift_line_map(&mut self.overlays.suffixes, start_line, line_delta);
            shift_line_map(&mut self.overlays.fold_markers, start_line, line_delta);
        }
    }

    /// Apply a buffer edit and record its inverse for undo. Returns the byte
    /// offset just past the inserted text. Highlighting is updated
    /// **incrementally**: the edit's positions are captured before the change,
    /// then tree-sitter reuses the unchanged subtrees on reparse.
    fn edit(&mut self, range: Range<usize>, new_text: &str) -> usize {
        let (range_start, range_end) = (range.start, range.end);
        let removed = self.buffer.slice_to_string(range.clone());
        let removed_newlines = removed.matches('\n').count();
        let record = UndoRecord {
            at: range.start,
            removed,
            inserted: new_text.to_string(),
        };
        // Append to the open group when grouping (one Scheme command = one undo);
        // otherwise start a NEW node branching off the current one — undoing then
        // editing keeps the old branch as a sibling (undo tree, not a stack).
        if self.undo_grouping && self.undo_group_started {
            self.undo_tree[self.undo_current].edit.push(record);
        } else {
            let new_idx = self.undo_tree.len();
            let parent = self.undo_current;
            self.undo_tree.push(UndoNode {
                parent: Some(parent),
                children: Vec::new(),
                edit: vec![record],
            });
            self.undo_tree[parent].children.push(new_idx);
            self.undo_current = new_idx;
            self.undo_group_started = self.undo_grouping;
        }

        // Capture pre-edit byte/point positions while the old text still exists.
        let positions = self.highlighter.is_some().then(|| {
            (
                range.start,
                range.end,
                self.buffer.point_at(range.start),
                self.buffer.point_at(range.end),
            )
        });

        // Capture pre-edit LSP positions for incremental sync (before the change).
        let lsp_change = self.active_language().map(|language| {
            let start = self.buffer.byte_to_position(range.start);
            let end = self.buffer.byte_to_position(range.end);
            (language, start, end)
        });

        let cursor = self.buffer.replace(range, new_text);
        self.modified = true;

        if let Some((start_byte, old_end_byte, start_point, old_end_point)) = positions {
            let new_end_byte = start_byte + new_text.len();
            let new_end_point = self.buffer.point_at(new_end_byte);
            let highlighter = self.highlighter.as_mut().unwrap();
            highlighter.apply_edit(&Edit {
                start_byte,
                old_end_byte,
                new_end_byte,
                start_point,
                old_end_point,
                new_end_point,
            });
            highlighter.reparse(self.buffer.rope());
        }
        if let Some((language, start, end)) = lsp_change {
            self.lsp_did_change(&language, start, end, new_text);
        }
        // Move any markers tracking positions in this buffer.
        self.script
            .adjust_markers(range_start, range_end, new_text.len());
        // Shift byte-range overlays the same way so they stay aligned until the
        // debounced re-fontify runs — otherwise hidden ranges / faces below the
        // edit point at stale bytes for a frame and visibly flicker (e.g. an Org
        // heading's hidden stars momentarily reappear and its indent jumps).
        let line_delta = new_text.matches('\n').count() as isize - removed_newlines as isize;
        let start_line = self.buffer.byte_to_position(range_start).0;
        self.adjust_overlays(
            range_start,
            range_end,
            new_text.len(),
            line_delta,
            start_line,
        );
        // Emacs `after-change-functions`. Guarded, so it costs nothing unless a
        // hook is registered (then it opts into a dispatch per edit).
        self.run_hook("after-change-functions");
        cursor
    }

    /// Reparse the whole buffer from scratch. Used on initial load and after
    /// undo, where edit deltas aren't tracked.
    fn parse_full(&mut self) {
        if let Some(highlighter) = self.highlighter.as_mut() {
            highlighter.set_text(self.buffer.rope());
        }
    }

    /// Insert `text`, replacing the active region if any.
    fn insert_text(&mut self, text: &str, cx: &mut Context<Self>) {
        let cursor = self.edit(self.selected_range.clone(), text);
        self.collapse_to(cursor);
        cx.notify();
    }

    /// electric-pair-mode handling for a single typed delimiter `ch` over
    /// `range`. Returns true if it fully handled the insertion (so the caller
    /// should not also self-insert): wraps an active region, types over an
    /// auto-inserted closer, or inserts an opener's partner and sits between.
    fn electric_pair_insert(
        &mut self,
        range: &Range<usize>,
        ch: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        let c = ch.chars().next().unwrap();
        // Active region: wrap it in the delimiter pair.
        if !range.is_empty() {
            let Some(close) = open_delim_partner(c) else {
                return false;
            };
            let inner = self.buffer.slice_to_string(range.clone());
            let cursor = self.edit(range.clone(), &format!("{ch}{inner}{close}"));
            self.collapse_to(cursor);
            self.ensure_cursor_visible();
            cx.notify();
            return true;
        }
        let at = range.start;
        // Type over an auto-inserted closer rather than doubling it.
        if is_close_delim(c) {
            let nxt = self.buffer.next_grapheme(at);
            if self.buffer.slice_to_string(at..nxt) == ch {
                self.collapse_to(nxt);
                self.ensure_cursor_visible();
                cx.notify();
                return true;
            }
        }
        // After an opener, insert its partner and place point between the two —
        // but not right before a word char (e.g. an apostrophe in "don't").
        if let Some(close) = open_delim_partner(c) {
            let nxt = self.buffer.next_grapheme(at);
            let next_is_word = self
                .buffer
                .slice_to_string(at..nxt)
                .chars()
                .next()
                .is_some_and(|n| n.is_alphanumeric() || n == '_');
            if !next_is_word {
                let cursor = self.edit(at..at, &format!("{ch}{close}"));
                self.collapse_to(cursor - close.len());
                self.ensure_cursor_visible();
                cx.notify();
                return true;
            }
        }
        false
    }

    /// Delete `range`, collapsing point to its start.
    fn delete_range(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        if range.is_empty() {
            return;
        }
        let cursor = self.edit(range, "");
        self.collapse_to(cursor);
        cx.notify();
    }

    /// Like `delete_range` but saves the deleted text to the kill ring.
    fn kill_range(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        if range.is_empty() {
            return;
        }
        let killed = self.buffer.slice_to_string(range.clone());
        self.set_clipboard(&killed, cx); // C-k/C-w/M-d also feed the OS clipboard
        self.kill_ring.push(killed);
        self.kill_index = self.kill_ring.len() - 1;
        self.delete_range(range, cx);
    }

    fn collapse_to(&mut self, offset: usize) {
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        self.marked_range = None;
        self.mark_active = false;
    }

    fn region(&self) -> Option<Range<usize>> {
        let r = self.visual_region();
        (!r.is_empty()).then_some(r)
    }

    /// The byte range an operation acts on and the renderer highlights. In evil
    /// Visual state this is *inclusive* of the grapheme under the block cursor
    /// (vim semantics), so the emacs-exclusive `selected_range` is extended one
    /// grapheme to the right — but never past the end of the line, so a charwise
    /// visual delete at EOL doesn't swallow the newline and join lines.
    fn visual_region(&self) -> Range<usize> {
        let r = self.selected_range.clone();
        if !matches!(self.evil, EvilState::Visual) {
            return r;
        }
        let at_eol = self.buffer.line_range_at(r.end).end == r.end;
        let end = if at_eol {
            r.end
        } else {
            self.buffer.next_grapheme(r.end)
        };
        r.start..end.max(r.start)
    }

    // --- Command implementations ------------------------------------------

    fn delete_char_forward(&mut self, cx: &mut Context<Self>) {
        let range = self.region().unwrap_or_else(|| {
            let p = self.point();
            p..self.buffer.next_grapheme(p)
        });
        self.delete_range(range, cx);
    }

    fn delete_char_backward(&mut self, cx: &mut Context<Self>) {
        if self.has_extra_cursors() && self.region().is_none() {
            self.multi_delete_back(cx);
            return;
        }
        let range = self.region().unwrap_or_else(|| {
            let p = self.point();
            self.buffer.prev_grapheme(p)..p
        });
        self.delete_range(range, cx);
    }

    fn kill_line(&mut self, cx: &mut Context<Self>) {
        let p = self.point();
        let line = self.buffer.line_range_at(p);
        let range = if p < line.end {
            p..line.end
        } else if p < self.buffer.len() {
            p..self.buffer.next_grapheme(p) // the line break
        } else {
            return;
        };
        self.kill_range(range, cx);
    }

    fn kill_word(&mut self, cx: &mut Context<Self>) {
        let p = self.point();
        self.kill_range(p..self.buffer.forward_word(p), cx);
    }

    fn backward_kill_word(&mut self, cx: &mut Context<Self>) {
        let p = self.point();
        self.kill_range(self.buffer.backward_word(p)..p, cx);
    }

    fn kill_region(&mut self, cx: &mut Context<Self>) {
        match self.region() {
            Some(range) => self.kill_range(range, cx),
            None => self.echo("No region selected", cx),
        }
    }

    fn kill_ring_save(&mut self, cx: &mut Context<Self>) {
        if self.terminal_copy(cx) {
            return;
        }
        match self.region() {
            Some(range) => {
                let text = self.buffer.slice_to_string(range);
                self.set_clipboard(&text, cx); // sync the system clipboard
                self.kill_ring.push(text);
                self.kill_index = self.kill_ring.len() - 1;
                self.mark_active = false;
                self.echo("Saved to kill ring", cx);
            }
            None => self.echo("No region selected", cx),
        }
    }

    // --- System clipboard (macOS cmd-c/x/v; bindable as clipboard-*) ----------

    /// Write `text` to the OS clipboard (kept in sync with the kill ring so the
    /// Emacs and platform copy/paste worlds interoperate).
    fn set_clipboard(&self, text: &str, cx: &mut Context<Self>) {
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.to_string()));
    }

    /// Copy the region to the system clipboard (and the kill ring).
    fn clipboard_copy(&mut self, cx: &mut Context<Self>) {
        if self.terminal_copy(cx) {
            return;
        }
        match self.region() {
            Some(range) => {
                let text = self.buffer.slice_to_string(range);
                self.set_clipboard(&text, cx);
                self.kill_ring.push(text);
                self.kill_index = self.kill_ring.len() - 1;
                self.mark_active = false;
                self.echo("Copied", cx);
                cx.notify();
            }
            None => self.echo("No region selected", cx),
        }
    }

    /// Cut the region to the system clipboard (and the kill ring).
    fn clipboard_cut(&mut self, cx: &mut Context<Self>) {
        // A terminal's grid is read-only; "cut" just copies the selection.
        if self.terminal_copy(cx) {
            return;
        }
        match self.region() {
            Some(range) => {
                let text = self.buffer.slice_to_string(range.clone());
                self.set_clipboard(&text, cx);
                self.kill_range(range, cx); // deletes + pushes to the kill ring
                self.echo("Cut", cx);
            }
            None => self.echo("No region selected", cx),
        }
    }

    /// Paste the system clipboard at point (replacing the region), also pushing
    /// it onto the kill ring so `C-y` / `yank-pop` see it.
    fn clipboard_paste(&mut self, cx: &mut Context<Self>) {
        if self.terminal_paste(cx) {
            return;
        }
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            self.echo("Clipboard is empty", cx);
            return;
        };
        if text.is_empty() {
            self.echo("Clipboard is empty", cx);
            return;
        }
        let start = self.selected_range.start;
        let cursor = self.edit(self.selected_range.clone(), &text);
        self.collapse_to(cursor);
        self.last_yank = Some(start..cursor);
        self.kill_ring.push(text);
        self.kill_index = self.kill_ring.len() - 1;
        cx.notify();
    }

    fn yank(&mut self, cx: &mut Context<Self>) {
        let Some(text) = self.kill_ring.last().cloned() else {
            self.echo("Kill ring is empty", cx);
            return;
        };
        let start = self.selected_range.start;
        let cursor = self.edit(self.selected_range.clone(), &text);
        self.collapse_to(cursor);
        self.last_yank = Some(start..cursor);
        self.kill_index = self.kill_ring.len() - 1;
        cx.notify();
    }

    fn yank_pop(&mut self, cx: &mut Context<Self>) {
        if !self.last_was_yank {
            self.echo("Previous command was not a yank", cx);
            return;
        }
        let (Some(range), false) = (self.last_yank.clone(), self.kill_ring.is_empty()) else {
            return;
        };
        self.kill_index = (self.kill_index + self.kill_ring.len() - 1) % self.kill_ring.len();
        let text = self.kill_ring[self.kill_index].clone();
        let cursor = self.edit(range.clone(), &text);
        self.collapse_to(cursor);
        self.last_yank = Some(range.start..cursor);
        cx.notify();
    }

    fn transpose_chars(&mut self, cx: &mut Context<Self>) {
        let p = self.point();
        // Need a character on each side; at buffer end, transpose the two before.
        let (left, mid, right) = if p < self.buffer.len() && p > 0 {
            (
                self.buffer.prev_grapheme(p),
                p,
                self.buffer.next_grapheme(p),
            )
        } else if p == self.buffer.len() && p > 0 {
            let mid = self.buffer.prev_grapheme(p);
            if mid == 0 {
                return;
            }
            (self.buffer.prev_grapheme(mid), mid, p)
        } else {
            return;
        };
        let a = self.buffer.slice_to_string(left..mid);
        let b = self.buffer.slice_to_string(mid..right);
        let swapped = format!("{b}{a}");
        self.edit(left..right, &swapped);
        self.collapse_to(right);
        cx.notify();
    }

    fn newline(&mut self, cx: &mut Context<Self>) {
        if self.has_extra_cursors() {
            self.multi_insert("\n", cx);
            return;
        }
        self.insert_text("\n", cx);
    }

    fn open_line(&mut self, cx: &mut Context<Self>) {
        let start = self.selected_range.start;
        self.edit(self.selected_range.clone(), "\n");
        self.move_to(start, cx);
    }

    fn indent_for_tab(&mut self, cx: &mut Context<Self>) {
        let width = cx.global::<crate::settings::Settings>().tab_width();
        self.insert_text(&" ".repeat(width), cx);
    }

    fn set_mark(&mut self, cx: &mut Context<Self>) {
        let p = self.point();
        self.selected_range = p..p;
        self.selection_reversed = false;
        self.mark_active = true;
        self.echo("Mark set", cx);
    }

    fn exchange_point_and_mark(&mut self, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.echo("No mark set", cx);
            return;
        }
        self.selection_reversed = !self.selection_reversed;
        self.mark_active = true;
        self.ensure_cursor_visible();
        cx.notify();
    }

    fn mark_whole_buffer(&mut self, cx: &mut Context<Self>) {
        self.selected_range = 0..self.buffer.len();
        self.selection_reversed = false;
        self.mark_active = true;
        self.ensure_cursor_visible();
        cx.notify();
    }

    fn undo(&mut self, cx: &mut Context<Self>) {
        let node = self.undo_current;
        let Some(parent) = self.undo_tree[node].parent else {
            self.echo("No further undo information", cx);
            return;
        };
        // Invert the current node's edit group (last edit undone first):
        // restore `removed` over the inserted span.
        let group = self.undo_tree[node].edit.clone();
        let mut cursor = 0;
        for record in group.into_iter().rev() {
            let span = record.at..record.at + record.inserted.len();
            self.buffer.replace(span, &record.removed);
            cursor = record.at + record.removed.len();
        }
        self.undo_current = parent;
        self.undo_group_started = false; // next edit branches a new node
        self.modified = true;
        self.parse_full();
        self.collapse_to(cursor);
        self.ensure_cursor_visible();
        let branches = self.undo_tree[parent].children.len();
        if branches > 1 {
            self.echo(format!("Undo (branch point: {branches} redo branches)"), cx);
        } else {
            self.echo("Undo", cx);
        }
    }

    /// Redo: re-apply the most recently created child branch of the current node
    /// (undo-tree semantics — the last branch you came from / created).
    fn redo(&mut self, cx: &mut Context<Self>) {
        let Some(&child) = self.undo_tree[self.undo_current].children.last() else {
            self.echo("No further redo information", cx);
            return;
        };
        // Re-apply the child's edit group forward: `inserted` over the removed span.
        let group = self.undo_tree[child].edit.clone();
        let mut cursor = 0;
        for record in &group {
            let span = record.at..record.at + record.removed.len();
            self.buffer.replace(span, &record.inserted);
            cursor = record.at + record.inserted.len();
        }
        self.undo_current = child;
        self.undo_group_started = false;
        self.modified = true;
        self.parse_full();
        self.collapse_to(cursor);
        self.ensure_cursor_visible();
        self.echo("Redo", cx);
    }

    /// A textual dump of the undo tree for the vundo visualizer: one
    /// "idx\tparent\tcurrent?\tdepth\tsize" line per node (depth = distance from
    /// root, size = edits in the node's group).
    fn undo_tree_dump(&self) -> String {
        self.undo_tree
            .iter()
            .enumerate()
            .map(|(i, n)| {
                let mut depth = 0;
                let mut p = n.parent;
                while let Some(parent) = p {
                    depth += 1;
                    p = self.undo_tree[parent].parent;
                }
                let parent = n.parent.map_or(-1, |x| x as isize);
                let current = if i == self.undo_current { 1 } else { 0 };
                format!("{i}\t{parent}\t{current}\t{depth}\t{}", n.edit.len())
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Group all edits made by one Scheme command batch into a single undo unit.
    fn begin_undo_group(&mut self) {
        self.undo_grouping = true;
        self.undo_group_started = false;
    }

    fn end_undo_group(&mut self) {
        self.undo_grouping = false;
        self.undo_group_started = false;
    }

    /// `(undo-boundary)`: close the current group so the next edit starts a new
    /// one even within the same command batch.
    fn undo_boundary(&mut self) {
        self.undo_group_started = false;
    }

    // --- Programmatic range editing ---------------------------------------

    fn delete_region(&mut self, start: usize, end: usize, cx: &mut Context<Self>) {
        let len = self.buffer.len();
        let (start, end) = (start.min(len), end.min(len));
        if start < end {
            let cursor = self.edit(start..end, "");
            self.collapse_to(cursor);
            cx.notify();
        }
    }

    fn replace_region(&mut self, start: usize, end: usize, text: &str, cx: &mut Context<Self>) {
        let len = self.buffer.len();
        let (start, end) = (start.min(len), end.min(len));
        let cursor = self.edit(start..end, text);
        self.collapse_to(cursor);
        cx.notify();
    }

    fn insert_at(&mut self, pos: usize, text: &str, cx: &mut Context<Self>) {
        let pos = pos.min(self.buffer.len());
        let cursor = self.edit(pos..pos, text);
        self.collapse_to(cursor);
        cx.notify();
    }

    fn scroll_page(&mut self, dir: isize, cx: &mut Context<Self>) {
        let lh = f32::from(self.line_height);
        let page = if lh <= 0.0 {
            1
        } else {
            ((f32::from(self.viewport_height) / lh).floor() as isize - 2).max(1)
        };
        self.move_to(self.line_offset(dir * page), cx);
    }

    /// Scroll half a page (evil `C-d` / `C-u`): `dir` +1 down, -1 up.
    fn scroll_half(&mut self, dir: isize, cx: &mut Context<Self>) {
        let lh = f32::from(self.line_height);
        let half = if lh <= 0.0 {
            1
        } else {
            ((f32::from(self.viewport_height) / lh).floor() as isize / 2).max(1)
        };
        self.move_to(self.line_offset(dir * half), cx);
    }

    /// `C-l` — Emacs `recenter-top-bottom`. Successive presses cycle the line at
    /// point through the middle, top, then bottom of the window; the cycle resets
    /// whenever point has moved since the last press. (On a buffer that already
    /// fits the window there's nothing to scroll, just as in Emacs.)
    fn recenter(&mut self, cx: &mut Context<Self>) {
        let lh = f32::from(self.line_height);
        let vh = f32::from(self.viewport_height);
        if lh <= 0.0 || vh <= 0.0 {
            return;
        }
        let point = self.point();
        if point == self.recenter_point {
            self.recenter_cycle = (self.recenter_cycle + 1) % 3;
        } else {
            self.recenter_cycle = 0;
        }
        self.recenter_point = point;

        let line = self.buffer.line_index_at_byte(point) as f32;
        let top = line * lh; // point's line flush with the window top
        let desired = match self.recenter_cycle {
            0 => top - vh / 2.0 + lh / 2.0, // middle
            1 => top,                       // top
            _ => top - vh + lh,             // bottom
        };
        let content = self.display_line_count() as f32 * lh;
        let max = (content - vh).max(0.0);
        self.scroll_offset = px(desired.clamp(0.0, max));
        cx.notify();
    }

    fn keyboard_quit(&mut self, cx: &mut Context<Self>) {
        if self.minibuffer.is_some() {
            self.mb_abort(cx);
            return;
        }
        let p = self.point();
        self.collapse_to(p);
        self.pending_keys.clear();
        self.avy_hints.clear(); // cancel any pending avy jump
        if self.has_extra_cursors() {
            self.mc_clear(cx); // C-g collapses back to a single cursor
        }
        self.echo("Quit", cx);
    }

    /// Public entry point for logging a status message to the echo area and the
    /// `*Messages*` buffer. Used by the launch-time auto-update flow, which runs
    /// from the workspace and has no direct access to the private `echo`.
    pub fn message(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.echo(message, cx);
    }

    fn echo(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.status = message.into();
        let line = self.status.to_string();
        push_capped(&mut self.messages, line.clone(), 1000);
        // Append live to `*Messages*` if it's open (active or any pane/inactive),
        // so the buffer accumulates like Emacs's `*Messages*`.
        self.append_to_log_buffer("*Messages*", &line);
        cx.notify();
    }

    /// Flip a boolean `Settings` option (stored as `"true"`/`"false"`) given its
    /// `current` value, then echo `"<label> enabled/disabled"`. Used by the
    /// `toggle-*` commands so the set-then-report dance lives in one place.
    fn toggle_setting(
        &mut self,
        key: impl Into<String>,
        current: bool,
        label: &str,
        cx: &mut Context<Self>,
    ) {
        let key = key.into();
        cx.update_global::<crate::settings::Settings, _>(|s, _| {
            s.set(key, crate::settings::OptionValue::Bool(!current));
        });
        self.script
            .set_options(cx.global::<crate::settings::Settings>().snapshot());
        self.echo(
            format!("{label} {}", if current { "disabled" } else { "enabled" }),
            cx,
        );
    }

    /// Append a line to a log buffer, creating it (as an inactive editable
    /// buffer) the first time so it shows up in the buffer list. Never switches
    /// to it or steals focus.
    fn append_to_log_buffer(&mut self, name: &str, line: &str) {
        let entry = format!("{line}\n");
        if self.active_name == name {
            let end = self.buffer.len();
            self.buffer.replace(end..end, &entry);
            self.lines.clear();
        } else if let Some(doc) = self.inactive.iter_mut().find(|d| d.name == name) {
            let end = doc.buffer.len();
            doc.buffer.replace(end..end, &entry);
            // Bound growth (only while not being viewed): keep the last N lines.
            let lines = doc.buffer.line_count();
            if lines > 5000 {
                let cut = doc.buffer.line_range(lines - 5000).start;
                doc.buffer.replace(0..cut, "");
            }
        } else {
            self.inactive
                .push(make_log_document(name.to_string(), &entry));
            self.sync_buffers(); // now visible in `(buffer-names)` / helm-buffers
        }
    }

    /// `C-h e`: switch to the editable `*Messages*` buffer (creating it from the
    /// scrollback ring if it doesn't exist yet). New messages append live.
    fn list_messages(&mut self, cx: &mut Context<Self>) {
        if self.active_name == "*Messages*" {
            return;
        }
        if let Some(index) = self.inactive.iter().position(|d| d.name == "*Messages*") {
            self.activate(index, cx);
        } else {
            let text = if self.messages.is_empty() {
                String::new()
            } else {
                format!("{}\n", self.messages.join("\n"))
            };
            let current = self.snapshot_active();
            self.inactive.push(current);
            self.install_active(make_log_document("*Messages*".to_string(), &text));
            self.move_to(self.buffer.len(), cx);
        }
    }

    /// `C-h l`: switch to the editable `*lsp-log*` buffer (creating it from the
    /// captured stderr if needed). New server output appends live.
    fn list_lsp_log(&mut self, cx: &mut Context<Self>) {
        if self.active_name == "*lsp-log*" {
            return;
        }
        if let Some(index) = self.inactive.iter().position(|d| d.name == "*lsp-log*") {
            self.activate(index, cx);
        } else {
            let text = if self.lsp_log.is_empty() {
                String::new()
            } else {
                format!("{}\n", self.lsp_log.join("\n"))
            };
            let current = self.snapshot_active();
            self.inactive.push(current);
            self.install_active(make_log_document("*lsp-log*".to_string(), &text));
            self.move_to(self.buffer.len(), cx);
        }
    }

    /// `M-x check-for-updates`: force a check against macros.dev, then echo the
    /// outcome. If a newer release exists, install it (macOS: download, verify,
    /// and swap in place; elsewhere: open the downloads page). All network/IO
    /// runs on the background executor; only echoes touch the editor. Unlike the
    /// launch-time check this never gates on the once-a-day interval.
    fn check_for_updates(&mut self, cx: &mut Context<Self>) {
        self.echo("Checking for updates…", cx);
        let task = cx
            .background_executor()
            .spawn(async { crate::update::check() });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |editor, cx| match result {
                Ok(None) => editor.echo(
                    format!(
                        "Macros is up to date (v{}).",
                        crate::update::current_version()
                    ),
                    cx,
                ),
                Ok(Some(available)) => editor.start_update_install(available, cx),
                Err(err) => editor.echo(format!("Update check failed: {err}"), cx),
            })
            .ok();
        })
        .detach();
    }

    /// Download + install an already-resolved [`crate::update::UpdateAvailable`],
    /// echoing progress. Driven by [`check_for_updates`].
    fn start_update_install(
        &mut self,
        available: crate::update::UpdateAvailable,
        cx: &mut Context<Self>,
    ) {
        self.echo(
            format!("Update v{} available — downloading…", available.version),
            cx,
        );
        let task = cx
            .background_executor()
            .spawn(async move { crate::update::install(&available, |_| {}) });
        cx.spawn(async move |this, cx| {
            let outcome = task.await;
            this.update(cx, |editor, cx| match outcome {
                Ok(crate::update::InstallOutcome::Staged) => {
                    let _ = crate::update::relaunch();
                    editor.echo("Update installed — restart Macros to finish.", cx);
                }
                Ok(crate::update::InstallOutcome::OpenedDownloadPage) => {
                    editor.echo("Opened the download page in your browser.", cx)
                }
                Err(err) => editor.echo(format!("Update failed: {err}"), cx),
            })
            .ok();
        })
        .detach();
    }

    /// Pop `*lsp-log*` into a split below the current pane (focus stays on the
    /// file), unless it's already shown. Called when an external server starts.
    fn open_lsp_log_split(&mut self, cx: &mut Context<Self>) {
        if self.panes.iter().any(|p| p.buffer == "*lsp-log*") {
            return;
        }
        let origin = self.selected_pane;
        let new_id = self.next_pane_id;
        self.split_window_ratio(true, 0.75, cx); // file keeps 75% on top, log below
        self.select_pane(new_id, cx);
        self.list_lsp_log(cx); // render into the new (selected) pane
        self.select_pane(origin, cx); // hand focus back to the file
    }

    // --- Minibuffer -------------------------------------------------------

    /// A render snapshot of the active minibuffer, split at the cursor.
    pub fn minibuffer_view(&self) -> Option<MinibufferView> {
        self.minibuffer.as_ref().map(|mb| {
            let (before, after) = mb.input.split_at(mb.cursor);
            MinibufferView {
                prompt: mb.prompt.clone(),
                before: before.to_string(),
                after: after.to_string(),
                message: mb.message.clone(),
            }
        })
    }

    fn open_minibuffer(
        &mut self,
        prompt: &str,
        purpose: MinibufferPurpose,
        initial: &str,
        cx: &mut Context<Self>,
    ) {
        self.minibuffer = Some(Minibuffer {
            prompt: prompt.to_string(),
            input: initial.to_string(),
            cursor: initial.len(),
            message: String::new(),
            purpose,
        });
        self.minibuffer_history_pos = None;
        self.minibuffer_stash = String::new();
        self.mb_dynamic = None; // set afterward by a dynamic completing-read
        cx.notify();
    }

    /// For a dynamic `completing-read`, re-dispatch the candidate fn with the
    /// current input on the engine thread (results arrive via `helm_set_candidates`'
    /// sibling routing in the `Candidates` outcome). No-op otherwise.
    fn mb_dynamic_requery(&mut self) {
        let Some((function, _)) = self.mb_dynamic.clone() else {
            return;
        };
        let Some(input) = self.minibuffer.as_ref().map(|mb| mb.input.clone()) else {
            return;
        };
        self.helm_query_counter += 1;
        let id = self.helm_query_counter;
        self.mb_dynamic = Some((function.clone(), id));
        // Sentinel source id `usize::MAX` distinguishes minibuffer queries from
        // Helm's (which use real source indices) in the `Candidates` outcome.
        self.script
            .dispatch_candidates(usize::MAX, id, &function, &input);
    }

    /// Install dynamic `completing-read` candidates from the engine, if they're
    /// still current (the query id matches the latest dispatch).
    fn mb_set_dynamic_candidates(
        &mut self,
        query_id: u64,
        candidates: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        if self.mb_dynamic.as_ref().map(|(_, id)| *id) == Some(query_id) {
            self.completing_candidates = candidates;
            cx.notify();
        }
    }

    /// Record a submitted minibuffer input in the shared history (newest last,
    /// skipping blanks and consecutive duplicates).
    fn push_minibuffer_history(&mut self, input: &str) {
        if input.is_empty() || self.minibuffer_history.last().map(String::as_str) == Some(input) {
            return;
        }
        self.minibuffer_history.push(input.to_string());
    }

    /// Browse minibuffer history: `delta < 0` recalls older entries, `delta > 0`
    /// newer ones; stepping past the newest restores the in-progress input.
    fn mb_history(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.minibuffer.is_none() || self.minibuffer_history.is_empty() {
            return;
        }
        let len = self.minibuffer_history.len();
        let pos = self.minibuffer_history_pos;
        let next = if delta < 0 {
            // Older. Entering history stashes the current (fresh) input.
            match pos {
                None => {
                    if let Some(mb) = self.minibuffer.as_ref() {
                        self.minibuffer_stash = mb.input.clone();
                    }
                    Some(0)
                }
                Some(i) => Some((i + 1).min(len - 1)),
            }
        } else {
            // Newer.
            match pos {
                None => return,
                Some(0) => None,
                Some(i) => Some(i - 1),
            }
        };
        let text = match next {
            Some(i) => self.minibuffer_history[len - 1 - i].clone(),
            None => std::mem::take(&mut self.minibuffer_stash),
        };
        self.minibuffer_history_pos = next;
        if let Some(mb) = self.minibuffer.as_mut() {
            mb.cursor = text.len();
            mb.input = text;
        }
        cx.notify();
    }

    // --- Buffers / files --------------------------------------------------

    /// Names of all open buffers (active first).
    fn buffer_names(&self) -> Vec<String> {
        let mut names = vec![self.active_name.clone()];
        names.extend(self.inactive.iter().map(|d| d.name.clone()));
        names
    }

    /// Publish the buffer list to Scheme (for `(buffer-names)`).
    fn sync_buffers(&self) {
        self.script.set_buffer_names(self.buffer_names());
    }

    pub fn buffer_name(&self) -> &str {
        &self.active_name
    }

    /// Snapshot the active buffer's state into a [`Document`], emptying the
    /// active fields (the caller installs another document next).
    fn snapshot_active(&mut self) -> Document {
        // Flush any pending full-sync change while this buffer is still active —
        // after the swap below `self.buffer` is a different document.
        self.flush_lsp_fullsync();
        Document {
            name: std::mem::take(&mut self.active_name),
            path: self.active_path.take(),
            buffer: std::mem::take(&mut self.buffer),
            selected_range: std::mem::replace(&mut self.selected_range, 0..0),
            selection_reversed: std::mem::take(&mut self.selection_reversed),
            marked_range: self.marked_range.take(),
            mark_active: std::mem::take(&mut self.mark_active),
            undo_tree: std::mem::replace(&mut self.undo_tree, new_undo_tree()),
            undo_current: std::mem::take(&mut self.undo_current),
            scroll_offset: self.scroll_offset,
            modes: std::mem::take(&mut self.modes),
            highlighter: self.highlighter.take(),
            modified: self.modified,
            hooks_ran: self.hooks_ran,
            special: self.special.take(),
            line_faces: std::mem::take(&mut self.line_faces),
            line_icons: std::mem::take(&mut self.line_icons),
            line_images: std::mem::take(&mut self.line_images),
            overlays: std::mem::take(&mut self.overlays),
            locals: std::mem::take(&mut self.locals),
        }
    }

    fn install_active(&mut self, doc: Document) {
        // Destructured (not field-by-field on `doc`) so adding a buffer-local
        // field to `Document` is a compile error here until it's restored too —
        // otherwise a new field would silently leak across buffer switches.
        let Document {
            name,
            path,
            buffer,
            selected_range,
            selection_reversed,
            marked_range,
            mark_active,
            undo_tree,
            undo_current,
            scroll_offset,
            modes,
            highlighter,
            modified,
            hooks_ran,
            special,
            line_faces,
            line_icons,
            line_images,
            overlays,
            locals,
        } = doc;
        self.active_name = name;
        self.active_path = path;
        self.buffer = buffer;
        self.selected_range = selected_range;
        self.selection_reversed = selection_reversed;
        self.marked_range = marked_range;
        self.mark_active = mark_active;
        self.undo_tree = undo_tree;
        self.undo_current = undo_current;
        self.scroll_offset = scroll_offset;
        self.modes = modes;
        self.highlighter = highlighter;
        self.modified = modified;
        self.hooks_ran = hooks_ran;
        self.special = special;
        self.line_faces = line_faces;
        self.line_icons = line_icons;
        self.line_images = line_images;
        self.overlays = overlays;
        self.locals = locals;
        // Markers are positions in the previous buffer; drop them on a switch.
        self.script.clear_markers();
        // Secondary cursors / snippet fields are marker-backed, so they're gone
        // with the markers — forget their (now dangling) ids and any avy hints.
        self.extra_cursors.clear();
        self.snippet_fields.clear();
        self.avy_hints.clear();
        // The selected window now shows this buffer (Emacs: switching the active
        // buffer changes what the selected window displays).
        let active = self.active_name.clone();
        let selected = self.selected_pane;
        if let Some(pane) = self.panes.iter_mut().find(|p| p.id == selected) {
            pane.buffer = active;
        }
        // Invalidate the layout cache; it's recomputed on the next paint.
        self.lines.clear();
        self.last_bounds = None;
        // Any buffer switch/create/kill flows through here — keep Scheme's view
        // of the buffer list current.
        self.sync_buffers();
    }

    /// Make the buffer `name` active, snapshotting the current one.
    fn activate(&mut self, index: usize, cx: &mut Context<Self>) {
        let target = self.inactive.remove(index);
        let current = self.snapshot_active();
        self.inactive.push(current);
        self.install_active(target);
        self.parse_full();
        self.ensure_cursor_visible();
        self.run_mode_hooks_once(cx);
        cx.notify();
    }

    /// `C-x <right>`: cycle forward through the buffer ring (the active buffer
    /// goes to the back, the front buffer becomes active).
    fn next_buffer(&mut self, cx: &mut Context<Self>) {
        if self.inactive.is_empty() {
            self.echo("No other buffer", cx);
            return;
        }
        let target = self.inactive.remove(0);
        self.cycle_to(target, true, cx);
    }

    /// `C-x <left>`: cycle backward (the active buffer goes to the front, the
    /// back buffer becomes active) — the exact inverse of `next-buffer`.
    fn previous_buffer(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.inactive.pop() else {
            self.echo("No other buffer", cx);
            return;
        };
        self.cycle_to(target, false, cx);
    }

    fn cycle_to(&mut self, target: Document, current_to_back: bool, cx: &mut Context<Self>) {
        let current = self.snapshot_active();
        if current_to_back {
            self.inactive.push(current);
        } else {
            self.inactive.insert(0, current);
        }
        self.install_active(target);
        self.parse_full();
        self.ensure_cursor_visible();
        self.run_mode_hooks_once(cx);
        let name = self.active_name.clone();
        self.echo(name, cx);
    }

    /// `C-x C-f`: visit a file, reusing its buffer if already open.
    fn find_file(&mut self, input: &str, cx: &mut Context<Self>) {
        let path = PathBuf::from(expand_path(input.trim()));
        if path.as_os_str().is_empty() {
            return;
        }
        if self.active_path.as_deref() == Some(path.as_path()) {
            self.echo(format!("Already in {}", self.active_name), cx);
            return;
        }
        if let Some(index) = self
            .inactive
            .iter()
            .position(|d| d.path.as_deref() == Some(path.as_path()))
        {
            self.activate(index, cx);
            self.echo(format!("Switched to {}", self.active_name), cx);
            return;
        }

        let name = path.file_name().map_or_else(
            || input.trim().to_string(),
            |f| f.to_string_lossy().into_owned(),
        );

        // Read off the UI thread so opening a large file never freezes the
        // editor; install the buffer back on the foreground when it's ready.
        let read_path = path.clone();
        let read = cx
            .background_executor()
            .spawn(async move { std::fs::read_to_string(&read_path).unwrap_or_default() });
        cx.spawn(async move |this, cx| {
            let contents = read.await;
            this.update(cx, |editor, cx| editor.open_file(name, path, contents, cx))
                .ok();
        })
        .detach();
    }

    /// Install a freshly-read file as the active buffer (foreground half of the
    /// async `find_file`).
    fn open_file(&mut self, name: String, path: PathBuf, contents: String, cx: &mut Context<Self>) {
        let doc = make_document(name.clone(), Some(path), &contents);
        let current = self.snapshot_active();
        self.inactive.push(current);
        self.install_active(doc);
        self.parse_full();
        self.move_to(0, cx);
        self.run_mode_hooks_once(cx);
        self.echo(format!("Visiting {name}"), cx);
        self.run_hook("find-file-hook"); // Emacs find-file-hook
    }

    /// Open files dropped onto the window from the OS (Finder, a terminal, etc.).
    /// Each dropped file is visited in its own buffer; the last one ends up
    /// active. Directories are skipped — `find_file` only reads regular files.
    fn on_drop_files(
        &mut self,
        paths: &ExternalPaths,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut skipped = 0;
        for path in paths.paths() {
            if path.is_dir() {
                skipped += 1;
                continue;
            }
            self.find_file(&path.to_string_lossy(), cx);
        }
        if skipped > 0 {
            self.echo(
                format!("Skipped {skipped} folder(s) — can only open files"),
                cx,
            );
        }
    }

    /// `:w <file>` — write the buffer's text to a given path (Vim save-as). The
    /// buffer keeps its own file; this just drops a copy.
    fn write_buffer_to(&mut self, path: &str, cx: &mut Context<Self>) {
        let target = expand_path(path.trim());
        match std::fs::write(&target, self.buffer.full_text()) {
            Ok(()) => self.echo(format!("Wrote {target}"), cx),
            Err(err) => self.echo(format!("Save failed: {err}"), cx),
        }
    }

    /// Open `path` in a window *other* than the selected one — the sidebar
    /// pattern (treemacs/dired): `RET` on a file keeps the tree and shows the
    /// file in the main editor. Uses the first other pane, or splits if the
    /// selected pane is the only one. Focus moves to the file's window.
    fn open_file_other_window(&mut self, path: String, cx: &mut Context<Self>) {
        let mut leaves = Vec::new();
        layout_leaves(&self.layout, &mut leaves);
        let other = leaves.iter().copied().find(|&id| id != self.selected_pane);
        let target = match other {
            Some(id) => id,
            None => {
                let new_id = self.next_pane_id; // split assigns this id to the new pane
                // Narrow split: the sidebar (the current pane) keeps ~22%.
                self.split_window_ratio(false, 0.22, cx);
                new_id
            }
        };
        self.select_pane(target, cx);
        self.find_file(&path, cx);
    }

    /// `:e!` / `M-x revert-buffer`: re-read the visited file from disk,
    /// discarding unsaved changes. Point is kept (clamped to the new length).
    fn revert_buffer(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.active_path.clone() else {
            self.echo("Buffer is not visiting a file", cx);
            return;
        };
        match std::fs::read_to_string(&path) {
            Ok(contents) => {
                let point = self.point().min(contents.len());
                let len = self.buffer.len();
                self.edit(0..len, &contents);
                self.parse_full();
                self.move_to(point.min(self.buffer.len()), cx);
                self.modified = false;
                self.echo(format!("Reverted {}", self.active_name), cx);
            }
            Err(err) => self.echo(format!("Revert failed: {err}"), cx),
        }
    }

    /// Byte ranges of the active search query's matches *within `[start, end)`*
    /// (the visible region), in absolute buffer offsets. Empty when no search is
    /// active or the regex doesn't compile. Scoped to the viewport so the cost
    /// is O(visible text), not O(document), and stays correct-by-construction
    /// (recomputed from the live buffer each frame — no cache to go stale).
    /// Navigation (`n`/`N`) searches the whole buffer separately in `do_search`.
    pub fn search_match_ranges_in(&self, start: usize, end: usize) -> Vec<Range<usize>> {
        let Some(query) = self.search_query.as_ref().filter(|q| !q.is_empty()) else {
            return Vec::new();
        };
        let Ok(regex) = Regex::new(query) else {
            return Vec::new();
        };
        let end = end.min(self.buffer.len());
        let start = start.min(end);
        // `start`/`end` are line boundaries (char-safe); slice only the visible
        // bytes, then map match offsets back to absolute positions.
        let slice = self.buffer.slice_to_string(start..end);
        regex
            .find_iter(&slice)
            .filter(|m| m.end() > m.start()) // skip zero-width matches
            .map(|m| (start + m.start())..(start + m.end()))
            .collect()
    }

    // --- Incremental search (Emacs `C-s` / `C-r`) -------------------------

    /// Start an incremental search. Reads a query in the minibuffer and matches
    /// it live (see `isearch_update`); `C-s`/`C-r` repeat, `RET` finishes leaving
    /// point on the match, and `C-g` aborts back to where the search began.
    fn isearch_start(&mut self, forward: bool, cx: &mut Context<Self>) {
        self.isearch_forward = forward;
        self.isearch_origin = self.point();
        self.isearch_from = self.point();
        self.isearch_match = None;
        self.search_query = None;
        let prompt = if forward {
            "I-search: "
        } else {
            "I-search backward: "
        };
        self.open_minibuffer(prompt, MinibufferPurpose::Isearch, "", cx);
    }

    /// Re-run the search for the current minibuffer string from `isearch_from`.
    /// Called on every edit of the search string, so matching is incremental.
    fn isearch_update(&mut self, cx: &mut Context<Self>) {
        let Some(query) = self.minibuffer.as_ref().map(|mb| mb.input.clone()) else {
            return;
        };
        let (forward, from) = (self.isearch_forward, self.isearch_from);
        self.isearch_run(&query, forward, from, cx);
    }

    /// `C-s` / `C-r` while searching: repeat in `forward`'s direction. An empty
    /// search string first recalls the previous one (Emacs `C-s C-s`); otherwise
    /// the search advances past the current match.
    fn isearch_repeat(&mut self, forward: bool, cx: &mut Context<Self>) {
        self.isearch_forward = forward;
        let empty = self
            .minibuffer
            .as_ref()
            .is_some_and(|mb| mb.input.is_empty());
        if empty {
            if let Some(prev) = self.last_isearch.clone()
                && let Some(mb) = self.minibuffer.as_mut()
            {
                mb.input = prev;
                mb.cursor = mb.input.len();
            }
        } else if let Some(m) = self.isearch_match.clone() {
            // Step off the current match so the next hit is a different one.
            self.isearch_from = if forward {
                m.start + 1
            } else {
                m.start.saturating_sub(1)
            };
        }
        let query = self
            .minibuffer
            .as_ref()
            .map(|mb| mb.input.clone())
            .unwrap_or_default();
        let from = self.isearch_from;
        self.isearch_run(&query, forward, from, cx);
    }

    /// Find the next match of `query` from `from` in the given direction, wrapping
    /// around the buffer; move point onto it (its end going forward, its start
    /// going backward) and record the match. Updates the minibuffer hint on
    /// failure or an invalid regex. An empty query just clears the highlight.
    fn isearch_run(&mut self, query: &str, forward: bool, from: usize, cx: &mut Context<Self>) {
        if query.is_empty() {
            self.search_query = None;
            self.isearch_match = None;
            if let Some(mb) = self.minibuffer.as_mut() {
                mb.message.clear();
            }
            cx.notify();
            return;
        }
        self.search_query = Some(query.to_string());
        let regex = match Regex::new(query) {
            Ok(r) => r,
            Err(_) => {
                if let Some(mb) = self.minibuffer.as_mut() {
                    mb.message = " [invalid regex]".into();
                }
                cx.notify();
                return;
            }
        };
        let text = self.buffer.full_text();
        let matches: Vec<Range<usize>> = regex
            .find_iter(&text)
            .filter(|m| m.end() > m.start())
            .map(|m| m.start()..m.end())
            .collect();
        let found = if forward {
            matches
                .iter()
                .find(|m| m.start >= from)
                .or_else(|| matches.first())
                .cloned()
        } else {
            matches
                .iter()
                .rev()
                .find(|m| m.start <= from)
                .or_else(|| matches.last())
                .cloned()
        };
        match found {
            Some(m) => {
                let target = if forward { m.end } else { m.start };
                self.isearch_match = Some(m);
                if let Some(mb) = self.minibuffer.as_mut() {
                    mb.message.clear();
                }
                self.move_to(target, cx);
            }
            None => {
                self.isearch_match = None;
                if let Some(mb) = self.minibuffer.as_mut() {
                    mb.message = " [failing]".into();
                }
                cx.notify();
            }
        }
    }

    /// `C-x b`: switch to an already-open buffer by name.
    fn switch_to_buffer(&mut self, name: &str, cx: &mut Context<Self>) {
        let name = name.trim();
        if name == self.active_name {
            cx.notify();
            return;
        }
        match self.inactive.iter().position(|d| d.name == name) {
            Some(index) => self.activate(index, cx),
            None => self.echo(format!("No buffer named {name}"), cx),
        }
    }

    /// `C-x C-s`: write the active buffer back to its file.
    fn save_buffer(&mut self, cx: &mut Context<Self>) {
        if self.active_path.is_none() {
            self.echo("Buffer is not visiting a file", cx);
            return;
        }
        if self.formatting_for_save {
            // Re-entry after an LSP format-on-save completed: just write now.
            self.formatting_for_save = false;
        } else {
            // First entry: run synchronous before-save hooks (e.g. a shell
            // formatter), then — if this language formats via LSP on save —
            // kick off the (async) format and defer the write to its response.
            self.run_hook_blocking("before-save-hook", cx);
            if self.start_lsp_format_on_save(cx) {
                return;
            }
        }
        self.write_active_to_disk(cx);
    }

    /// Save every modified file-visiting buffer (Emacs `save-some-buffers`).
    /// Inactive buffers are written synchronously here; the active one goes
    /// through the normal (async, hook-running) save path.
    fn save_some_buffers(&mut self, cx: &mut Context<Self>) {
        let mut saved = 0;
        for doc in &mut self.inactive {
            if doc.modified
                && let Some(path) = doc.path.clone()
                && std::fs::write(&path, doc.buffer.full_text()).is_ok()
            {
                doc.modified = false;
                saved += 1;
            }
        }
        if self.active_path.is_some() && self.modified {
            self.write_active_to_disk(cx);
            saved += 1;
        }
        self.echo(
            match saved {
                0 => "No unsaved buffers".to_string(),
                1 => "Saved 1 buffer".to_string(),
                n => format!("Saved {n} buffers"),
            },
            cx,
        );
    }

    /// Write the active buffer to its file off the UI thread. Optimistically
    /// clears `modified`; restores it (and reports) if the write fails.
    fn write_active_to_disk(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.active_path.clone() else {
            return;
        };
        let text = self.buffer.full_text();
        let name = self.active_name.clone();
        self.modified = false;
        let write = cx
            .background_executor()
            .spawn(async move { std::fs::write(&path, text) });
        cx.spawn(async move |this, cx| {
            let result = write.await;
            this.update(cx, |editor, cx| match result {
                Ok(()) => {
                    editor.echo(format!("Wrote {name}"), cx);
                    editor.run_hook("after-save-hook"); // Emacs after-save-hook
                }
                Err(err) => {
                    editor.modified = true;
                    editor.echo(format!("Save failed: {err}"), cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// If the active buffer's language formats via LSP on save and a server is
    /// ready, trigger the format (the response's edits re-enter `save_buffer` to
    /// write). Returns whether a format was started.
    fn start_lsp_format_on_save(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(language) = self.active_language() else {
            return false;
        };
        if !self.lsp_format_on_save.contains(&language) {
            return false;
        }
        let Some(uri) = self.active_uri() else {
            return false;
        };
        let ready = self
            .servers
            .get(&language)
            .is_some_and(|s| s.ready && s.open.contains(&uri));
        if !ready {
            return false;
        }
        self.formatting_for_save = true;
        self.lsp_format(cx);
        true
    }

    /// Run a hook's functions synchronously (blocking on the engine thread),
    /// applying each one's enqueued commands immediately. Used for
    /// `before-save-hook`, where a formatter must modify the buffer *before* the
    /// write. Async work a hook starts (e.g. an LSP request) still completes
    /// later — only the synchronous part is guaranteed done here.
    fn run_hook_blocking(&mut self, hook: &str, cx: &mut Context<Self>) {
        let Some(functions) = self.hooks.get(hook).filter(|f| !f.is_empty()).cloned() else {
            return;
        };
        for function in functions {
            self.publish_point();
            for command in self.script.call_action(&function, None) {
                self.apply_command(command, cx);
            }
        }
    }

    /// The active buffer's directory as a minibuffer-ready prefix (trailing `/`),
    /// for file-name prompts. Falls back to the process cwd.
    fn default_directory_input(&self) -> String {
        let dir = self
            .active_path
            .as_ref()
            .and_then(|p| p.parent())
            .map(Path::to_path_buf)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        let mut initial = dir.to_string_lossy().into_owned();
        if !initial.ends_with('/') {
            initial.push('/');
        }
        initial
    }

    fn find_file_prompt(&mut self, cx: &mut Context<Self>) {
        let initial = self.default_directory_input();
        self.open_minibuffer("Find file: ", MinibufferPurpose::FindFile, &initial, cx);
    }

    fn mb_insert(&mut self, text: &str, cx: &mut Context<Self>) {
        if let Some(mb) = self.minibuffer.as_mut() {
            mb.input.insert_str(mb.cursor, text);
            mb.cursor += text.len();
            mb.message.clear();
            cx.notify();
        }
        if self.is_isearch() {
            self.isearch_update(cx);
        }
        self.mb_dynamic_requery();
    }

    fn mb_backspace(&mut self, cx: &mut Context<Self>) {
        if let Some(mb) = self.minibuffer.as_mut()
            && mb.cursor > 0
        {
            let prev = prev_grapheme(&mb.input, mb.cursor);
            mb.input.replace_range(prev..mb.cursor, "");
            mb.cursor = prev;
            mb.message.clear();
            cx.notify();
        }
        if self.is_isearch() {
            // Shorten the string and re-match from the original start so deleting
            // a char backs up to the earlier (shorter) match.
            self.isearch_from = self.isearch_origin;
            self.isearch_update(cx);
        }
        self.mb_dynamic_requery();
    }

    /// Whether an incremental-search minibuffer is currently open.
    fn is_isearch(&self) -> bool {
        matches!(
            self.minibuffer.as_ref().map(|mb| &mb.purpose),
            Some(MinibufferPurpose::Isearch)
        )
    }

    fn mb_delete(&mut self, cx: &mut Context<Self>) {
        if let Some(mb) = self.minibuffer.as_mut() {
            let next = next_grapheme(&mb.input, mb.cursor);
            if next > mb.cursor {
                mb.input.replace_range(mb.cursor..next, "");
                mb.message.clear();
                cx.notify();
            }
        }
        self.mb_dynamic_requery();
    }

    fn mb_move(&mut self, to: usize, cx: &mut Context<Self>) {
        if let Some(mb) = self.minibuffer.as_mut() {
            mb.cursor = to.min(mb.input.len());
            cx.notify();
        }
    }

    fn mb_left(&mut self, cx: &mut Context<Self>) {
        if let Some(mb) = self.minibuffer.as_ref() {
            let to = prev_grapheme(&mb.input, mb.cursor);
            self.mb_move(to, cx);
        }
    }

    fn mb_right(&mut self, cx: &mut Context<Self>) {
        if let Some(mb) = self.minibuffer.as_ref() {
            let to = next_grapheme(&mb.input, mb.cursor);
            self.mb_move(to, cx);
        }
    }

    fn mb_abort(&mut self, cx: &mut Context<Self>) {
        // Aborting an incremental search returns point to where it began and
        // drops the match highlight (Emacs C-g out of isearch).
        let isearch = self.is_isearch();
        self.minibuffer = None;
        if isearch {
            self.search_query = None;
            self.isearch_match = None;
            self.move_to(self.isearch_origin, cx);
        }
        self.echo("Quit", cx);
    }

    /// Completion candidates for the active minibuffer's purpose and input.
    fn mb_candidates(&self, input: &str) -> Vec<String> {
        match self.minibuffer.as_ref().map(|mb| mb.purpose.clone()) {
            Some(MinibufferPurpose::ExecuteCommand) => self
                .script
                .command_names()
                .iter()
                .filter(|c| c.starts_with(input))
                .cloned()
                .collect(),
            Some(MinibufferPurpose::SwitchBuffer) => self
                .buffer_names()
                .into_iter()
                .filter(|n| n.starts_with(input))
                .collect(),
            Some(MinibufferPurpose::FindFile) => file_completions(input),
            Some(MinibufferPurpose::Confirm(_)) | Some(MinibufferPurpose::QuitSave) => {
                ["yes", "no"]
                    .iter()
                    .map(|s| s.to_string())
                    .filter(|c| c.starts_with(input))
                    .collect()
            }
            Some(MinibufferPurpose::CompleteAtPoint) => self
                .completion_items
                .iter()
                .filter(|c| c.starts_with(input))
                .cloned()
                .collect(),
            Some(MinibufferPurpose::SchemeCompleting(_)) => self
                .completing_candidates
                .iter()
                .filter(|c| c.starts_with(input))
                .cloned()
                .collect(),
            // Dynamic: the Scheme fn already filtered for the pattern — show as-is.
            Some(MinibufferPurpose::SchemeCompletingDynamic(_)) => {
                self.completing_candidates.clone()
            }
            Some(MinibufferPurpose::SchemeReadFile(_)) => file_completions(input),
            Some(MinibufferPurpose::SchemeYesNo(_)) => ["yes", "no"]
                .iter()
                .map(|s| s.to_string())
                .filter(|c| c.starts_with(input))
                .collect(),
            // Free-form reads (regex search, ex command, read-string) have no
            // completions.
            Some(
                MinibufferPurpose::SearchForward
                | MinibufferPurpose::SearchBackward
                | MinibufferPurpose::Isearch
                | MinibufferPurpose::ExCommand
                | MinibufferPurpose::EvalExpression
                | MinibufferPurpose::SchemeCallback(_),
            ) => Vec::new(),
            None => Vec::new(),
        }
    }

    /// `TAB`: complete the input against the candidate set for this read.
    fn mb_complete(&mut self, cx: &mut Context<Self>) {
        let Some((input, purpose)) = self
            .minibuffer
            .as_ref()
            .map(|mb| (mb.input.clone(), mb.purpose.clone()))
        else {
            return;
        };
        // The eval read holds a whole expression, so TAB completes the trailing
        // Scheme symbol token (e.g. `(set-o` → `(set-option`) rather than the
        // whole string — a dedicated, splice-in-place path.
        if matches!(purpose, MinibufferPurpose::EvalExpression) {
            self.mb_complete_symbol(&input, cx);
            return;
        }
        let matches = self.mb_candidates(&input);

        let mb = self.minibuffer.as_mut().unwrap();
        if matches.is_empty() {
            mb.message = "[No match]".into();
        } else if matches.len() == 1 {
            mb.input = matches[0].clone();
            mb.cursor = mb.input.len();
            mb.message = "[Sole completion]".into();
        } else {
            let prefix = longest_common_prefix(&matches);
            if prefix.len() > mb.input.len() {
                mb.input = prefix;
                mb.cursor = mb.input.len();
            }
            // Show the candidates so the user can see what to disambiguate.
            mb.message = format_candidates(&matches);
        }
        cx.notify();
    }

    /// All Scheme symbols worth completing in the eval minibuffer: the
    /// Rust-registered builtins and interactive commands, plus every top-level
    /// define in the bundled Scheme library. Sorted and de-duplicated.
    fn scheme_symbol_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.script.global_names().to_vec();
        names.extend(crate::runtime::defined_names());
        names.sort();
        names.dedup();
        names
    }

    /// `TAB` in the eval minibuffer: complete the trailing symbol token against
    /// [`Self::scheme_symbol_names`], splicing the result in place of just that
    /// token so the rest of the expression (open parens, earlier args) is kept.
    fn mb_complete_symbol(&mut self, input: &str, cx: &mut Context<Self>) {
        let (start, token) = scheme_symbol_token(input);
        let candidates: Vec<String> = if token.is_empty() {
            Vec::new()
        } else {
            self.scheme_symbol_names()
                .into_iter()
                .filter(|s| s.starts_with(token))
                .collect()
        };
        let head = input[..start].to_string();
        let token_len = token.len();
        let mb = self.minibuffer.as_mut().unwrap();
        if candidates.is_empty() {
            mb.message = "[No match]".into();
        } else if candidates.len() == 1 {
            mb.input = format!("{head}{}", candidates[0]);
            mb.cursor = mb.input.len();
            mb.message = "[Sole completion]".into();
        } else {
            let lcp = longest_common_prefix(&candidates);
            if lcp.len() > token_len {
                mb.input = format!("{head}{lcp}");
                mb.cursor = mb.input.len();
            }
            // Show bare symbol names so the user can see what to disambiguate.
            mb.message = format_candidates(&candidates);
        }
        cx.notify();
    }

    /// `RET`: finish the read and act on the input.
    fn mb_submit(&mut self, cx: &mut Context<Self>) {
        let Some(mb) = self.minibuffer.take() else {
            return;
        };
        cx.notify();
        let input = mb.input.trim().to_string();
        // Record text-entry reads in the shared history (not yes/no prompts).
        if !matches!(
            mb.purpose,
            MinibufferPurpose::Confirm(_)
                | MinibufferPurpose::QuitSave
                | MinibufferPurpose::SchemeYesNo(_)
        ) {
            self.push_minibuffer_history(&input);
        }
        match mb.purpose {
            MinibufferPurpose::ExecuteCommand => {
                if input.is_empty() {
                    self.echo("", cx);
                } else if self.script.command_names().iter().any(|c| c == &input) {
                    self.run_script(&format!("({input})"), cx);
                } else {
                    self.echo(format!("[No match: {input}]"), cx);
                }
            }
            MinibufferPurpose::FindFile => {
                if !input.is_empty() {
                    self.find_file(&input, cx);
                }
            }
            MinibufferPurpose::SwitchBuffer => {
                if !input.is_empty() {
                    self.switch_to_buffer(&input, cx);
                }
            }
            MinibufferPurpose::Confirm(action) => {
                if matches!(input.as_str(), "yes" | "y") {
                    self.perform(action, cx);
                } else {
                    self.echo("Cancelled", cx);
                }
            }
            MinibufferPurpose::QuitSave => match input.as_str() {
                "yes" | "y" => {
                    self.save_all_modified(cx);
                    cx.quit();
                }
                "no" | "n" => cx.quit(),
                _ => self.echo("Cancelled", cx),
            },
            MinibufferPurpose::CompleteAtPoint => {
                if let Some(range) = self.completion_range.take()
                    && !input.is_empty()
                {
                    let cursor = self.edit(range, &input);
                    self.collapse_to(cursor);
                    cx.notify();
                }
            }
            MinibufferPurpose::SearchForward => {
                if !input.is_empty() {
                    self.search_query = Some(input);
                }
                self.evil_search_forward = true;
                self.do_search(true, cx);
            }
            MinibufferPurpose::SearchBackward => {
                if !input.is_empty() {
                    self.search_query = Some(input);
                }
                self.evil_search_forward = false;
                self.do_search(false, cx);
            }
            MinibufferPurpose::Isearch => {
                // RET ends the search, leaving point at the current match. Remember
                // the string so a later empty C-s recalls it; keep `search_query`
                // set so the matches stay highlighted (Emacs lazy-highlight).
                if !input.is_empty() {
                    self.last_isearch = Some(input);
                } else if let Some(prev) = self.last_isearch.clone() {
                    // RET on an empty isearch with a prior string: adopt it.
                    self.search_query = Some(prev);
                }
                self.isearch_match = None;
            }
            MinibufferPurpose::ExCommand => self.run_ex_command(&input, cx),
            MinibufferPurpose::EvalExpression => {
                if !input.is_empty() {
                    self.eval_interactive(&input, cx); // echoes `=> value`
                }
            }
            MinibufferPurpose::SchemeCallback(callback)
            | MinibufferPurpose::SchemeCompleting(callback)
            | MinibufferPurpose::SchemeCompletingDynamic(callback) => {
                // Pass the entered text to the Scheme callback.
                self.publish_point();
                self.script.dispatch_action(&callback, Some(&input));
            }
            MinibufferPurpose::SchemeReadFile(callback) => {
                self.publish_point();
                self.script
                    .dispatch_action(&callback, Some(&expand_path(&input)));
            }
            MinibufferPurpose::SchemeYesNo(callback) => {
                let answer = if matches!(input.as_str(), "yes" | "y") {
                    "yes"
                } else {
                    "no"
                };
                self.publish_point();
                self.script.dispatch_action(&callback, Some(answer));
            }
        }
    }

    fn open_confirm(&mut self, prompt: &str, action: PendingAction, cx: &mut Context<Self>) {
        let prompt = format!("{prompt} (yes or no) ");
        self.open_minibuffer(&prompt, MinibufferPurpose::Confirm(action), "", cx);
    }

    fn perform(&mut self, action: PendingAction, cx: &mut Context<Self>) {
        match action {
            PendingAction::KillActiveBuffer => self.kill_active_buffer(cx),
            PendingAction::KillBufferAndWindow => {
                self.kill_active_buffer(cx);
                self.delete_window(cx);
            }
        }
    }

    // --- kill-buffer / quit ----------------------------------------------

    fn any_modified(&self) -> bool {
        self.modified || self.inactive.iter().any(|d| d.modified)
    }

    pub fn is_modified(&self) -> bool {
        self.modified
    }

    /// Handle an OS window-close request (the title-bar close button) and
    /// `C-x C-c`. If any buffer has unsaved changes, offer to save them first:
    /// `yes` saves all modified file buffers then quits, `no` quits discarding,
    /// `C-g` cancels. With nothing unsaved, quit straight away. Always returns
    /// `false` so the bare window-close is cancelled — quitting (`cx.quit()`)
    /// tears the whole app down rather than leaving a windowless process on
    /// macOS.
    pub fn confirm_quit(&mut self, cx: &mut Context<Self>) -> bool {
        if self.any_modified() {
            self.open_minibuffer(
                "Save modified buffers before quitting? (yes/no, C-g to cancel) ",
                MinibufferPurpose::QuitSave,
                "",
                cx,
            );
        } else {
            cx.quit();
        }
        false
    }

    /// Write every modified, file-visiting buffer (active + inactive) to disk
    /// synchronously. Used on quit, so writes are blocking (no UI to keep
    /// responsive) and skip the async format-on-save path. Buffers not visiting
    /// a file (e.g. `*scratch*`) can't be saved and are left untouched.
    fn save_all_modified(&mut self, cx: &mut Context<Self>) {
        if self.modified
            && let Some(path) = self.active_path.clone()
        {
            match std::fs::write(&path, self.buffer.full_text()) {
                Ok(()) => self.modified = false,
                Err(err) => self.echo(format!("Save failed: {err}"), cx),
            }
        }
        for doc in &mut self.inactive {
            if doc.modified
                && let Some(path) = doc.path.clone()
                && std::fs::write(&path, doc.buffer.full_text()).is_ok()
            {
                doc.modified = false;
            }
        }
    }

    /// `C-x k`: kill the active buffer (confirming first if it has unsaved
    /// changes), then show the most-recent other buffer (or a fresh scratch).
    fn kill_buffer(&mut self, cx: &mut Context<Self>) {
        if self.modified {
            self.open_confirm(
                "Buffer modified; kill anyway?",
                PendingAction::KillActiveBuffer,
                cx,
            );
        } else {
            self.kill_active_buffer(cx);
        }
    }

    /// Kill the active buffer *and* close its window (Emacs `kill-buffer-and-
    /// window`), confirming first if the buffer has unsaved changes. With a sole
    /// window, the buffer is killed and `delete_window` just declines.
    fn kill_buffer_and_window(&mut self, cx: &mut Context<Self>) {
        if self.modified {
            self.open_confirm(
                "Buffer modified; kill anyway?",
                PendingAction::KillBufferAndWindow,
                cx,
            );
        } else {
            self.kill_active_buffer(cx);
            self.delete_window(cx);
        }
    }

    fn kill_active_buffer(&mut self, cx: &mut Context<Self>) {
        let killed = self.active_name.clone();
        let replacement = self
            .inactive
            .pop()
            .unwrap_or_else(|| make_document("*scratch*".to_string(), None, ""));
        self.install_active(replacement);
        self.drop_terminal_for_buffer(&killed);
        self.parse_full();
        self.ensure_cursor_visible();
        self.echo(format!("Killed {killed}"), cx);
    }

    /// Shut down and forget any embedded terminal backing buffer `name` (its
    /// `Drop` closes the PTY and ends the event-loop thread).
    fn drop_terminal_for_buffer(&mut self, name: &str) {
        if let Some(id) = self
            .terminals
            .values()
            .find(|t| t.buffer_name == name)
            .map(|t| t.id)
        {
            self.terminals.remove(&id);
            self.terminal_geom.remove(&id);
            if self.terminal_drag == Some(id) {
                self.terminal_drag = None;
            }
        }
    }

    /// Kill a buffer by name: the active one (via the normal path), or just drop
    /// an inactive [`Document`] (ibuffer/dired). Last buffer stays as scratch.
    fn kill_buffer_named(&mut self, name: &str, cx: &mut Context<Self>) {
        if name == self.active_name {
            self.kill_active_buffer(cx);
        } else if let Some(idx) = self.inactive.iter().position(|d| d.name == name) {
            self.inactive.remove(idx);
            self.drop_terminal_for_buffer(name);
            self.sync_buffers();
            self.echo(format!("Killed {name}"), cx);
        }
    }

    /// `C-x C-c` / `cmd-q`: quit, offering to save first if any buffer is
    /// unsaved (same save/discard/cancel prompt as the title-bar close button).
    fn quit(&mut self, cx: &mut Context<Self>) {
        self.confirm_quit(cx);
    }

    // --- Special (read-only, program-generated) buffers & transients ------

    /// Render `text` into the read-only buffer `name` (major mode `mode`),
    /// switching to it. If it's already the active buffer this refreshes in
    /// place, preserving the point line (magit's revert-and-keep-position).
    fn render_buffer(
        &mut self,
        name: String,
        mode: String,
        text: String,
        line_faces: Vec<(usize, String)>,
        cx: &mut Context<Self>,
    ) {
        let faces: HashMap<usize, SharedString> = line_faces
            .into_iter()
            .map(|(line, face)| (line, SharedString::from(face)))
            .collect();
        // Icons are attached after render via `set-line-icon`; a (re)render of
        // this buffer starts with a clean slate.
        if self.active_name == name {
            self.line_icons.clear();
        }

        if self.active_name == name {
            // Refresh in place, keeping point on the same line.
            let line = self.buffer.line_index_at_byte(self.point());
            self.buffer = Buffer::new(&text);
            self.line_faces = faces;
            self.lines.clear();
            let last = self.buffer.line_count().saturating_sub(1);
            let offset = self.buffer.line_range(line.min(last)).start;
            self.move_to(offset, cx);
        } else {
            let current = self.snapshot_active();
            // Replace any stale buffer of the same name, then install fresh.
            self.inactive.retain(|d| d.name != name);
            self.inactive.push(current);
            self.install_active(make_special_document(name, &mode, &text));
            self.line_faces = faces;
            self.move_to(0, cx);
        }
        self.modified = false;
        cx.notify();
    }

    /// Append `text` to the end of buffer `name` (creating it if needed) and
    /// switch to it — an output-buffer primitive (Emacs `append-to-buffer`).
    /// Unlike `render-buffer`, the target is a normal editable buffer.
    fn append_to_buffer(&mut self, name: String, text: String, cx: &mut Context<Self>) {
        if self.active_name != name {
            if let Some(idx) = self.inactive.iter().position(|d| d.name == name) {
                self.activate(idx, cx);
            } else {
                let current = self.snapshot_active();
                self.inactive.push(current);
                self.install_active(make_document(name, None, ""));
            }
        }
        let end = self.buffer.len();
        self.buffer.replace(end..end, &text);
        self.lines.clear();
        let new_end = self.buffer.len();
        self.move_to(new_end, cx);
        self.parse_full();
        cx.notify();
    }

    /// Borrow the rope buffer named `name` — the active buffer or any inactive
    /// pane buffer. Used by the cross-buffer ediff primitives.
    fn buffer_named(&self, name: &str) -> Option<&Buffer> {
        if self.active_name == name {
            Some(&self.buffer)
        } else {
            self.inactive
                .iter()
                .find(|d| d.name == name)
                .map(|d| &d.buffer)
        }
    }

    /// Diff buffers `a` and `b` and hand each non-equal hunk to Steel `callback`.
    fn diff_buffers(&self, a: &str, b: &str, callback: &str) {
        let records = match (self.buffer_named(a), self.buffer_named(b)) {
            (Some(abuf), Some(bbuf)) => {
                let (ta, tb) = (abuf.full_text(), bbuf.full_text());
                let diff = similar::TextDiff::from_lines(&ta, &tb);
                diff.ops()
                    .iter()
                    .filter_map(|op| {
                        let (tag, ar, br) = op.as_tag_tuple();
                        let name = match tag {
                            similar::DiffTag::Equal => return None,
                            similar::DiffTag::Delete => "delete",
                            similar::DiffTag::Insert => "insert",
                            similar::DiffTag::Replace => "replace",
                        };
                        let (abs, abe) = line_span(abuf, ar.start, ar.end);
                        let (bbs, bbe) = line_span(bbuf, br.start, br.end);
                        // Hunk text, newlines escaped to U+001F so each hunk
                        // stays a single tab-delimited record for Scheme.
                        let atext = abuf.slice_to_string(abs..abe).replace('\n', "\u{1f}");
                        let btext = bbuf.slice_to_string(bbs..bbe).replace('\n', "\u{1f}");
                        Some(format!(
                            "{name}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                            ar.start, ar.end, br.start, br.end, abs, abe, bbs, bbe, atext, btext
                        ))
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            _ => String::new(),
        };
        self.script.dispatch_action(callback, Some(&records));
    }

    /// Replace lines [ds,de) of buffer `dst` with lines [ss,se) of buffer `src`.
    #[allow(clippy::too_many_arguments)]
    fn buffer_copy_lines(
        &mut self,
        src: &str,
        ss: usize,
        se: usize,
        dst: &str,
        ds: usize,
        de: usize,
        cx: &mut Context<Self>,
    ) {
        let text = match self.buffer_named(src) {
            Some(buf) => {
                let (s, e) = line_span(buf, ss, se);
                buf.slice_to_string(s..e)
            }
            None => return,
        };
        if self.active_name == dst {
            let (s, e) = line_span(&self.buffer, ds, de);
            self.replace_region(s, e, &text, cx);
        } else if let Some(idx) = self.inactive.iter().position(|d| d.name == dst) {
            let len = {
                let doc = &mut self.inactive[idx];
                let (s, e) = line_span(&doc.buffer, ds, de);
                doc.buffer.replace(s..e, &text);
                doc.modified = true;
                if let Some(h) = doc.highlighter.as_mut() {
                    h.set_text(doc.buffer.rope());
                }
                let len = doc.buffer.len();
                if doc.selected_range.start > len {
                    doc.selected_range.start = len;
                }
                if doc.selected_range.end > len {
                    doc.selected_range.end = len;
                }
                len
            };
            // Keep any pane viewing this buffer in bounds after the edit.
            for p in &mut self.panes {
                if p.buffer == dst && p.point > len {
                    p.point = len;
                }
            }
            cx.notify();
        }
    }

    /// Paint `face` over lines [sline,eline) of buffer `name` (active or not).
    fn buffer_highlight_lines(
        &mut self,
        name: &str,
        sline: usize,
        eline: usize,
        face: String,
        cx: &mut Context<Self>,
    ) {
        if self.active_name == name {
            let (s, e) = line_span(&self.buffer, sline, eline);
            if s < e {
                self.overlays.faces.push((s..e, SharedString::from(face)));
                cx.notify();
            }
        } else if let Some(doc) = self.inactive.iter_mut().find(|d| d.name == name) {
            let (s, e) = line_span(&doc.buffer, sline, eline);
            if s < e {
                doc.overlays.faces.push((s..e, SharedString::from(face)));
                cx.notify();
            }
        }
    }

    /// Drop all face overlays from buffer `name` (active or inactive).
    fn buffer_clear_highlights(&mut self, name: &str, cx: &mut Context<Self>) {
        if self.active_name == name {
            if !self.overlays.faces.is_empty() {
                self.overlays.faces.clear();
                cx.notify();
            }
        } else if let Some(doc) = self.inactive.iter_mut().find(|d| d.name == name)
            && !doc.overlays.faces.is_empty()
        {
            doc.overlays.faces.clear();
            cx.notify();
        }
    }

    // --- yasnippet session ------------------------------------------------

    /// Expand `template` at point: insert its plain text, create a marker per
    /// tab-stop field, and jump to the first field (selecting its placeholder so
    /// typing overwrites it). Field markers ride along on edits via the marker
    /// store, so navigation stays correct as the user types.
    fn snippet_expand(&mut self, template: String, cx: &mut Context<Self>) {
        self.snippet_clear();
        let (text, fields) = parse_snippet(&template);
        let origin = self.selected_range.start.min(self.buffer.len());
        let cursor = self.edit(origin..origin, &text);
        self.snippet_fields = fields
            .iter()
            .map(|&(_, off, len)| (self.script.add_marker(origin + off), len))
            .collect();
        self.snippet_index = 0;
        if self.snippet_fields.is_empty() {
            self.collapse_to(cursor);
        } else {
            self.snippet_goto_field(0);
        }
        self.ensure_cursor_visible();
        cx.notify();
    }

    /// Place point at field `idx`, selecting its placeholder when it has one.
    fn snippet_goto_field(&mut self, idx: usize) {
        let Some(&(mid, len)) = self.snippet_fields.get(idx) else {
            return;
        };
        let Some(pos) = self.script.marker_pos(mid) else {
            return;
        };
        let pos = pos.min(self.buffer.len());
        if len > 0 {
            self.selected_range = pos..(pos + len).min(self.buffer.len());
            self.selection_reversed = false;
            self.marked_range = None;
            self.mark_active = true;
        } else {
            self.collapse_to(pos);
        }
        self.snippet_index = idx;
    }

    /// Move to the next (`dir` +1) or previous (`dir` -1) field; stepping past
    /// the last field ends the session.
    fn snippet_step_field(&mut self, dir: isize, cx: &mut Context<Self>) {
        if self.snippet_fields.is_empty() {
            return;
        }
        let next = self.snippet_index as isize + dir;
        if next < 0 {
            return;
        }
        if next as usize >= self.snippet_fields.len() {
            self.snippet_clear();
            self.echo("snippet: done", cx);
            return;
        }
        self.snippet_goto_field(next as usize);
        self.ensure_cursor_visible();
        cx.notify();
    }

    /// End the snippet session, dropping its field markers.
    fn snippet_clear(&mut self) {
        let ids: Vec<u64> = self.snippet_fields.iter().map(|&(id, _)| id).collect();
        for id in ids {
            self.script.remove_marker(id);
        }
        self.snippet_fields.clear();
        self.snippet_index = 0;
    }

    // --- avy jump hints ---------------------------------------------------

    /// The active avy hints (byte offset + label), read by the renderer.
    pub fn avy_hints(&self) -> &[(usize, SharedString)] {
        &self.avy_hints
    }

    /// Set avy hints from "byte\tlabel" records (one per line).
    fn avy_show_hints(&mut self, records: &str, cx: &mut Context<Self>) {
        self.avy_hints = records
            .lines()
            .filter_map(|line| {
                let (b, label) = line.split_once('\t')?;
                Some((
                    b.trim().parse::<usize>().ok()?,
                    SharedString::from(label.to_string()),
                ))
            })
            .collect();
        cx.notify();
    }

    fn avy_clear_hints(&mut self, cx: &mut Context<Self>) {
        if !self.avy_hints.is_empty() {
            self.avy_hints.clear();
            cx.notify();
        }
    }

    // --- multiple cursors -------------------------------------------------

    /// Byte positions of the secondary cursors (resolved from their markers).
    pub fn extra_cursor_positions(&self) -> Vec<usize> {
        self.extra_cursors
            .iter()
            .filter_map(|&id| self.script.marker_pos(id))
            .collect()
    }

    fn has_extra_cursors(&self) -> bool {
        !self.extra_cursors.is_empty()
    }

    /// All cursor byte positions (primary + extras), unique and ascending.
    fn all_cursor_positions(&self) -> Vec<usize> {
        let mut v: Vec<usize> = self.extra_cursor_positions();
        v.push(self.selected_range.start);
        v.sort_unstable();
        v.dedup();
        v
    }

    /// Add a secondary cursor at byte `pos` (ignored if it coincides with an
    /// existing cursor). Returns whether one was added.
    fn mc_add_cursor_at(&mut self, pos: usize) -> bool {
        let pos = pos.min(self.buffer.len());
        if pos == self.selected_range.start || self.extra_cursor_positions().contains(&pos) {
            return false;
        }
        let id = self.script.add_marker(pos);
        self.extra_cursors.push(id);
        true
    }

    /// Drop all secondary cursors.
    fn mc_clear(&mut self, cx: &mut Context<Self>) {
        for id in std::mem::take(&mut self.extra_cursors) {
            self.script.remove_marker(id);
        }
        cx.notify();
    }

    /// Replicate an insertion of `text` at every cursor (primary + extras),
    /// keeping all of them after their inserted text. One undo group.
    fn multi_insert(&mut self, text: &str, cx: &mut Context<Self>) {
        let primary = self.selected_range.start;
        let pm = self.script.add_marker(primary);
        let positions = self.all_cursor_positions();
        self.begin_undo_group();
        for &pos in positions.iter().rev() {
            self.edit(pos..pos, text);
        }
        self.end_undo_group();
        let np = self.script.marker_pos(pm).unwrap_or(primary);
        self.script.remove_marker(pm);
        self.collapse_to(np);
        self.parse_full();
        self.ensure_cursor_visible();
        cx.notify();
    }

    /// Replicate a backspace (delete the grapheme before each cursor).
    fn multi_delete_back(&mut self, cx: &mut Context<Self>) {
        let primary = self.selected_range.start;
        let pm = self.script.add_marker(primary);
        let positions = self.all_cursor_positions();
        self.begin_undo_group();
        for &pos in positions.iter().rev() {
            let prev = self.buffer.prev_grapheme(pos);
            if prev < pos {
                self.edit(prev..pos, "");
            }
        }
        self.end_undo_group();
        let np = self.script.marker_pos(pm).unwrap_or(primary);
        self.script.remove_marker(pm);
        self.collapse_to(np);
        self.parse_full();
        self.ensure_cursor_visible();
        cx.notify();
    }

    /// Add a cursor on the line above/below the lowest/highest cursor, at the
    /// primary cursor's column (`dir` -1 = above, +1 = below).
    fn mc_add_line(&mut self, dir: isize, cx: &mut Context<Self>) {
        let primary = self.selected_range.start;
        let line = self.buffer.line_index_at_byte(primary);
        let col = primary - self.buffer.line_range(line).start;
        let target = line as isize + dir;
        if target < 0 || target as usize >= self.buffer.line_count() {
            self.echo("multiple-cursors: no line there", cx);
            return;
        }
        let lr = self.buffer.line_range(target as usize);
        let pos = (lr.start + col).min(lr.end);
        if self.mc_add_cursor_at(pos) {
            cx.notify();
        }
    }

    /// Add a cursor at the next occurrence of the word at point (mc/mark-next).
    fn mc_mark_next(&mut self, cx: &mut Context<Self>) {
        let primary = self.selected_range.start;
        // The word under the primary cursor.
        let start = self.buffer.backward_word(self.buffer.forward_word(primary));
        let end = self.buffer.forward_word(start);
        let word = self.buffer.slice_to_string(start..end);
        if word.trim().is_empty() {
            self.echo("multiple-cursors: no word at point", cx);
            return;
        }
        // Search after the furthest existing cursor, wrapping once.
        let from = self
            .all_cursor_positions()
            .into_iter()
            .max()
            .unwrap_or(primary);
        let text = self.buffer.full_text();
        let found = text[from..]
            .find(&word)
            .map(|i| from + i)
            .or_else(|| text.find(&word));
        match found {
            Some(at) if self.mc_add_cursor_at(at) => {
                self.collapse_to(at + word.len());
                self.ensure_cursor_visible();
                cx.notify();
            }
            _ => self.echo("multiple-cursors: no more matches", cx),
        }
    }

    /// Whether `hook` has at least one Scheme function registered — lets edit
    /// coalescing avoid changing observable behaviour for hooks that watch every
    /// individual change (e.g. `after-change-functions`).
    fn has_active_hook(&self, hook: &str) -> bool {
        self.hooks.get(hook).is_some_and(|fns| !fns.is_empty())
    }

    /// Run every Scheme function registered on `hook` (Emacs lifecycle hooks like
    /// `after-save-hook`, `find-file-hook`, `after-change-functions`). No-op —
    /// and no thread hop — when nothing is registered.
    fn run_hook(&mut self, hook: &str) {
        let Some(functions) = self.hooks.get(hook) else {
            return;
        };
        if functions.is_empty() {
            return;
        }
        let functions = functions.clone();
        self.publish_point();
        for function in functions {
            self.script.dispatch_action(&function, None);
        }
    }

    /// Publish the active buffer name, point line, and directory so
    /// `(buffer-line)` / `(current-buffer)` / `(buffer-directory)` see the buffer
    /// as of command invocation. The directory is the visited file's parent
    /// (Emacs's buffer-local `default-directory`), or the process cwd when the
    /// buffer visits no file — what magit resolves the repo from.
    fn publish_point(&self) {
        let point = self.point();
        let directory = self
            .active_path
            .as_deref()
            .and_then(|p| p.parent())
            .map_or_else(
                || self.process_cwd.clone(),
                |d| d.to_string_lossy().into_owned(),
            );
        let region = self.region().map(|r| (r.start, r.end));
        let props = self
            .overlays
            .props
            .iter()
            .map(|(r, k, v)| (r.start, r.end, k.clone(), v.clone()))
            .collect();
        let locals = self
            .locals
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        // Diagnostics as flat records "line\tcol\tseverity\tmessage" (message
        // tabs/newlines flattened so each diagnostic stays one record).
        let diagnostics = self
            .active_diagnostics()
            .into_iter()
            .map(|d| {
                let (line, col) = self.buffer.byte_to_position(d.range.start);
                let sev = match d.severity {
                    1 => "error",
                    2 => "warning",
                    3 => "info",
                    _ => "hint",
                };
                let msg = d.message.replace(['\t', '\n'], " ");
                format!("{line}\t{col}\t{sev}\t{msg}")
            })
            .collect();
        self.script.set_snapshot(crate::script::EditorSnapshot {
            name: self.active_name.clone(),
            line: self.buffer.line_index_at_byte(point),
            directory,
            point,
            // O(1) copy-on-write clone of the rope; read bridges materialize
            // only the slice they need, off the UI thread.
            text: self.buffer.rope().clone(),
            region,
            props,
            locals,
            diagnostics,
            windows: {
                let mut leaves = Vec::new();
                layout_leaves(&self.layout, &mut leaves);
                leaves
            },
            selected_window: self.selected_pane,
            lsp_ready: self.active_lsp_ready(),
            visible_lines: self.visible_line_range(),
        });
    }

    /// Approximate first/last buffer line currently on screen, from the scroll
    /// offset and viewport height. An approximation under word-wrap/folds, but
    /// fine for scoping avy's jump hints to the visible region.
    fn visible_line_range(&self) -> (usize, usize) {
        let lh = f32::from(self.line_height).max(1.0);
        let first = (f32::from(self.scroll_offset) / lh).floor().max(0.0) as usize;
        let rows = (f32::from(self.viewport_height) / lh).ceil() as usize + 1;
        let last = (first + rows).min(self.buffer.line_count());
        (first, last)
    }

    /// Whether the active buffer has a ready, opened LSP server (for the Scheme
    /// `(lsp-server-ready?)` guard).
    fn active_lsp_ready(&self) -> bool {
        let (Some(language), Some(uri)) = (self.active_language(), self.active_uri()) else {
            return false;
        };
        self.servers
            .get(&language)
            .is_some_and(|s| s.ready && s.open.contains(&uri))
    }

    /// The mode-line project indicator set from Scheme, if any.
    pub fn project_indicator(&self) -> Option<&str> {
        self.project_indicator.as_ref().map(SharedString::as_ref)
    }

    // --- Overlays / text properties / folds (active buffer) ---------------

    /// Sub-line face ranges, for the element to layer over syntax highlighting.
    pub fn range_faces(&self) -> &[(Range<usize>, SharedString)] {
        &self.overlays.faces
    }

    /// Hidden ranges (`overlay-hide`) intersecting buffer line `line`, expressed
    /// as `(start, end)` byte offsets relative to the line start, sorted and
    /// merged. The renderer splices these out so the bytes collapse from display.
    pub fn line_holes(&self, line: usize) -> Vec<(usize, usize)> {
        if self.overlays.hidden.is_empty() {
            return Vec::new();
        }
        let lr = self.buffer.line_range(line);
        let mut holes: Vec<(usize, usize)> = self
            .overlays
            .hidden
            .iter()
            .filter_map(|r| {
                let s = r.start.max(lr.start);
                let e = r.end.min(lr.end);
                (s < e).then(|| (s - lr.start, e - lr.start))
            })
            .collect();
        if holes.len() > 1 {
            holes.sort_unstable();
            let mut merged: Vec<(usize, usize)> = Vec::with_capacity(holes.len());
            for (s, e) in holes {
                match merged.last_mut() {
                    Some(last) if s <= last.1 => last.1 = last.1.max(e),
                    _ => merged.push((s, e)),
                }
            }
            holes = merged;
        }
        holes
    }

    /// Whether any line is folded (the element's fast path skips fold handling).
    pub fn has_folds(&self) -> bool {
        !self.overlays.folds.is_empty()
    }

    /// Whether buffer line `line` is hidden by a fold.
    pub fn is_line_invisible(&self, line: usize) -> bool {
        self.overlays.folds.iter().any(|r| r.contains(&line))
    }

    /// Whether any gutter fold chevron is set (element's fast path skips them).
    pub fn has_fold_markers(&self) -> bool {
        !self.overlays.fold_markers.is_empty()
    }

    /// The fold chevron state for buffer `line`, if one is set: `Some(true)` for
    /// an open (expanded) chevron, `Some(false)` for a collapsed one.
    pub fn fold_marker_at(&self, line: usize) -> Option<bool> {
        self.overlays.fold_markers.get(&line).copied()
    }

    /// Number of visible (non-folded) lines — the element's screen-row count.
    pub fn display_line_count(&self) -> usize {
        let text = if self.overlays.folds.is_empty() {
            self.buffer.line_count()
        } else {
            (0..self.buffer.line_count())
                .filter(|i| !self.is_line_invisible(*i))
                .count()
        };
        text + self.image_rows_total() + self.scale_rows_total()
    }

    /// Screen row a buffer line maps to once folds are collapsed and inline
    /// images on earlier lines have reserved their rows.
    pub fn display_row_of_line(&self, line: usize) -> usize {
        let text = if self.overlays.folds.is_empty() {
            line
        } else {
            (0..line).filter(|i| !self.is_line_invisible(*i)).count()
        };
        text + self.image_rows_before(line) + self.scale_rows_before(line)
    }

    /// Virtual left indentation (in columns) for buffer `line`, 0 if none.
    /// Display-only — the Org `org-indent-mode` look.
    pub fn line_indent(&self, line: usize) -> usize {
        self.overlays.indents.get(&line).copied().unwrap_or(0)
    }

    /// Whether the active buffer is an Org buffer (set by `org-mode`). The
    /// renderer drops font ligatures here so prose markup like `**` / `***`
    /// doesn't ligate into a single shifted glyph.
    pub fn is_org(&self) -> bool {
        self.locals.get("org-active").is_some_and(|v| v == "t")
    }

    // --- Inline images (Org / LaTeX preview) ------------------------------
    /// Whether the active buffer displays any inline image.
    pub fn has_line_images(&self) -> bool {
        !self.line_images.is_empty()
    }

    /// Decoded image + its displayed (width, height) in px for `line`, or `None`
    /// if the line has no image / it hasn't decoded yet. The width is clamped to
    /// the line's own override (set via `set-line-image`'s optional width arg),
    /// or [`INLINE_IMAGE_MAX_WIDTH`] when the override is 0; height keeps aspect.
    fn image_display(&self, line: usize) -> Option<(Arc<RenderImage>, f32, f32)> {
        let (path, override_w) = self.line_images.get(&line)?;
        let img = self.image_cache.get(path)?;
        if img.width == 0 {
            return None;
        }
        let cap = if *override_w > 0 {
            *override_w as f32
        } else {
            INLINE_IMAGE_MAX_WIDTH
        };
        let (w, h) = (img.width as f32, img.height as f32);
        let disp_w = w.min(cap);
        Some((img.data.clone(), disp_w, h * (disp_w / w)))
    }

    /// How many display rows the image on `line` reserves below it (0 if the
    /// line has no image, the image hasn't decoded yet, or the line is folded
    /// away). Width-independent of the window, so folds/scroll stay consistent.
    pub fn image_rows_at(&self, line: usize) -> usize {
        if self.is_line_invisible(line) {
            return 0;
        }
        let lh = f32::from(self.line_height);
        match self.image_display(line) {
            Some((_, _, disp_h)) if lh > 0.0 => (disp_h / lh).ceil().max(1.0) as usize,
            _ => 0,
        }
    }

    /// The decoded image to paint below `line`, with its displayed pixel size.
    pub fn line_image_at(&self, line: usize) -> Option<(Arc<RenderImage>, f32, f32)> {
        self.image_display(line)
    }

    fn image_rows_total(&self) -> usize {
        if self.line_images.is_empty() {
            return 0;
        }
        self.line_images
            .keys()
            .map(|&l| self.image_rows_at(l))
            .sum()
    }

    /// Whether any line is scaled (e.g. headings rendered larger).
    pub fn has_line_scales(&self) -> bool {
        !self.overlays.scales.is_empty()
    }

    /// The font-size multiplier for `line` (1.0 = unscaled).
    pub fn line_scale(&self, line: usize) -> f32 {
        self.overlays.scales.get(&line).copied().unwrap_or(1.0)
    }

    /// Extra display rows a scaled line reserves beyond its own — so a heading
    /// taller than one row claims that space, exactly like an inline image. A
    /// scale of S occupies `ceil(S)` rows total, hence `ceil(S) - 1` extra.
    pub fn scale_rows_at(&self, line: usize) -> usize {
        if self.is_line_invisible(line) {
            return 0;
        }
        let s = self.line_scale(line);
        if s > 1.0 {
            (s.ceil() as usize).saturating_sub(1)
        } else {
            0
        }
    }

    fn scale_rows_total(&self) -> usize {
        self.overlays
            .scales
            .keys()
            .map(|&l| self.scale_rows_at(l))
            .sum()
    }

    fn scale_rows_before(&self, line: usize) -> usize {
        self.overlays
            .scales
            .keys()
            .filter(|&&l| l < line)
            .map(|&l| self.scale_rows_at(l))
            .sum()
    }

    fn image_rows_before(&self, line: usize) -> usize {
        if self.line_images.is_empty() {
            return 0;
        }
        self.line_images
            .keys()
            .filter(|&&l| l < line)
            .map(|&l| self.image_rows_at(l))
            .sum()
    }

    /// Record an inline image on `line` and ensure its bitmap is decoded (async,
    /// off the UI thread). Re-uses the cache so a re-scan never re-decodes.
    fn set_line_image(
        &mut self,
        line: usize,
        path: String,
        max_width: usize,
        cx: &mut Context<Self>,
    ) {
        let path = PathBuf::from(expand_path(&path));
        self.line_images.insert(line, (path.clone(), max_width));
        if self.image_cache.contains_key(&path) {
            self.ensure_cursor_visible();
            cx.notify();
            return;
        }
        // Decode off the UI thread, then install on the foreground and repaint.
        let decode_path = path.clone();
        let decode = cx
            .background_executor()
            .spawn(async move { load_inline_image(&decode_path) });
        cx.spawn(async move |this, cx| {
            let decoded = decode.await;
            this.update(cx, |editor, cx| {
                if let Some(img) = decoded {
                    editor.image_cache.insert(path, img);
                    editor.ensure_cursor_visible();
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn overlay_set_face(&mut self, start: usize, end: usize, face: String, cx: &mut Context<Self>) {
        let len = self.buffer.len();
        let (start, end) = (start.min(len), end.min(len));
        if start < end {
            self.overlays
                .faces
                .push((start..end, SharedString::from(face)));
            cx.notify();
        }
    }

    fn overlay_hide(&mut self, start: usize, end: usize, cx: &mut Context<Self>) {
        let len = self.buffer.len();
        let (start, end) = (start.min(len), end.min(len));
        if start < end {
            self.overlays.hidden.push(start..end);
            cx.notify();
        }
    }

    fn overlay_fold_lines(&mut self, start_line: usize, end_line: usize, cx: &mut Context<Self>) {
        let last = self.buffer.line_count();
        let (start, end) = (start_line.min(last), end_line.min(last));
        if start < end {
            self.overlays.folds.push(start..end);
            self.ensure_cursor_visible();
            cx.notify();
        }
    }

    fn overlay_unfold_all(&mut self, cx: &mut Context<Self>) {
        if !self.overlays.folds.is_empty() {
            self.overlays.folds.clear();
            self.ensure_cursor_visible();
            cx.notify();
        }
    }

    fn overlay_set_fold_marker(&mut self, line: usize, open: bool, cx: &mut Context<Self>) {
        self.overlays.fold_markers.insert(line, open);
        cx.notify();
    }

    fn overlay_clear_fold_markers(&mut self, cx: &mut Context<Self>) {
        if !self.overlays.fold_markers.is_empty() {
            self.overlays.fold_markers.clear();
            cx.notify();
        }
    }

    fn overlay_clear(&mut self, cx: &mut Context<Self>) {
        self.overlays = Overlays::default();
        self.ensure_cursor_visible();
        cx.notify();
    }

    fn put_text_property(&mut self, start: usize, end: usize, key: String, value: String) {
        let len = self.buffer.len();
        let (start, end) = (start.min(len), end.min(len));
        if start < end {
            self.overlays.props.push((start..end, key, value));
        }
    }

    // --- Async (subprocess streaming, timers) -----------------------------

    /// Spawn `program` asynchronously (Emacs `make-process`): a worker thread
    /// reads its stdout line by line and forwards each line to the foreground,
    /// which calls the Steel `on_output` fn with it; `on_exit` runs when the
    /// process finishes. The UI never blocks, and the callbacks' commands flow
    /// through the normal outcome drain — so `(append-to-buffer …)` etc. work.
    fn make_process(
        &mut self,
        id: u64,
        program: String,
        args: Vec<String>,
        on_output: String,
        on_exit: Option<String>,
        cx: &mut Context<Self>,
    ) {
        // Spawn here (not on the worker) so we can keep the child for stdin/kill.
        let mut child = match Command::new(&program)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(e) => {
                self.echo(format!("make-process: {e}"), cx);
                return;
            }
        };
        let stdout = child.stdout.take();
        let stdin = child.stdin.take();
        self.processes.insert(id, ProcessHandle { child, stdin });

        let (tx, mut rx) = futures::channel::mpsc::unbounded::<ProcMsg>();
        std::thread::spawn(move || {
            if let Some(out) = stdout {
                for line in BufReader::new(out).lines() {
                    let Ok(l) = line else { break };
                    if tx.unbounded_send(ProcMsg::Line(l)).is_err() {
                        break;
                    }
                }
            }
            // The editor wait()s for the exit code on Exit.
            let _ = tx.unbounded_send(ProcMsg::Exit);
        });
        cx.spawn(async move |this, cx| {
            while let Some(msg) = rx.next().await {
                let alive = this
                    .update(cx, |editor, _| match &msg {
                        ProcMsg::Line(line) => {
                            editor.script.dispatch_action(&on_output, Some(line))
                        }
                        ProcMsg::Exit => {
                            let code = editor
                                .processes
                                .remove(&id)
                                .and_then(|mut h| h.child.wait().ok())
                                .and_then(|status| status.code())
                                .unwrap_or(-1);
                            if let Some(cb) = &on_exit {
                                editor.script.dispatch_action(cb, Some(&code.to_string()));
                            }
                        }
                    })
                    .is_ok();
                if !alive || matches!(msg, ProcMsg::Exit) {
                    break;
                }
            }
        })
        .detach();
    }

    /// Write `text` to a live process's stdin (Emacs `process-send-string`).
    fn send_to_process(&mut self, id: u64, text: &str, cx: &mut Context<Self>) {
        if let Some(handle) = self.processes.get_mut(&id)
            && let Some(stdin) = handle.stdin.as_mut()
            && let Err(e) = stdin.write_all(text.as_bytes())
        {
            self.echo(format!("process-send-string: {e}"), cx);
        }
    }

    /// Kill a live process; its reader thread then reports exit (Emacs `kill-process`).
    fn kill_process(&mut self, id: u64) {
        if let Some(handle) = self.processes.get_mut(&id) {
            let _ = handle.child.kill();
        }
    }

    // --- Embedded terminal ------------------------------------------------

    /// Spawn a terminal (`make-terminal`): start the PTY/shell and show it in a
    /// read-only `term-mode` buffer named `name`. If a terminal buffer of that
    /// name already exists, just switch to it (reuse the running shell).
    fn make_terminal(
        &mut self,
        id: u64,
        name: String,
        program: String,
        args: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        if self.terminals.values().any(|t| t.buffer_name == name) {
            self.switch_to_buffer(&name, cx);
            return;
        }

        // Initial grid size from the current cell metrics; the element re-fits it
        // on the first frame, so rough numbers are fine.
        let (cols, rows) = self.terminal_fit_size();
        let cell_w = (f32::from(self.char_width).max(1.0)) as u16;
        let cell_h = (f32::from(self.line_height).max(1.0)) as u16;
        let program = if program.trim().is_empty() {
            None
        } else {
            Some((program, args))
        };
        let cwd = self
            .active_path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf);

        match Terminal::new(
            id,
            name.clone(),
            cols,
            rows,
            cell_w,
            cell_h,
            program,
            cwd,
            self.term_events_tx.clone(),
        ) {
            Ok(terminal) => {
                self.terminals.insert(id, terminal);
                // A read-only special buffer drives input routing and the mode
                // line; its rope text is unused (the grid is rendered directly).
                self.render_buffer(name, "term".to_string(), String::new(), Vec::new(), cx);
            }
            Err(e) => self.echo(format!("make-terminal: {e}"), cx),
        }
    }

    /// A starting `(columns, rows)` for a new terminal from the live viewport,
    /// defaulting to 80×24 before the first frame has measured anything.
    fn terminal_fit_size(&self) -> (usize, usize) {
        let cw = f32::from(self.char_width);
        let lh = f32::from(self.line_height);
        let cols = if cw > 0.0 {
            (f32::from(self.viewport_width) / cw) as usize
        } else {
            0
        };
        let rows = if lh > 0.0 {
            (f32::from(self.viewport_height) / lh) as usize
        } else {
            0
        };
        // Before the first frame the viewport is unmeasured; fall back to 80×24.
        (
            if cols == 0 { 80 } else { cols },
            if rows == 0 { 24 } else { rows },
        )
    }

    /// The terminal shown in pane `pane`, if its buffer is a terminal buffer.
    pub fn pane_terminal_id(&self, pane: usize) -> Option<u64> {
        let name = self.pane(pane).map(|p| p.buffer.as_str())?;
        self.terminals
            .values()
            .find(|t| t.buffer_name == name)
            .map(|t| t.id)
    }

    /// The terminal in the selected pane (the live one), if any. Includes a
    /// terminal whose shell has exited — its scrollback is still readable, so
    /// copy/scroll keep working on the leftover buffer.
    fn active_terminal_id(&self) -> Option<u64> {
        self.terminals
            .values()
            .find(|t| t.buffer_name == self.active_name)
            .map(|t| t.id)
    }

    /// The active terminal only while its shell is still running. Input paths
    /// (key grab, action keys, paste) use this so a dead terminal releases the
    /// keyboard and normal editor dispatch (M-x, C-x k, …) resumes.
    fn active_running_terminal_id(&self) -> Option<u64> {
        self.active_terminal_id()
            .filter(|tid| !self.terminals.get(tid).is_some_and(|t| t.exited))
    }

    /// Send already-encoded key bytes to terminal `id`, snapping the view back to
    /// the live region first (typing scrolls out of scrollback).
    fn terminal_input(&mut self, id: u64, bytes: Vec<u8>, cx: &mut Context<Self>) {
        if let Some(t) = self.terminals.get(&id) {
            t.selection_clear(); // typing dismisses any mouse selection
            t.scroll_to_bottom();
            t.input(bytes);
            cx.notify();
        }
    }

    /// If a terminal is active, encode the named OS-special key and send it to
    /// the PTY, returning `true`. This is the *only* dispatch path for keys bound
    /// to GPUI actions (Backspace/Enter/Tab/arrows/…): gpui stops propagation
    /// after running an action, so `on_key_down` never sees them — the `act_*`
    /// handlers call this instead. Plain text and Ctrl/Alt chords still go through
    /// `on_key_down`'s own terminal branch.
    fn terminal_action_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let Some(tid) = self.active_running_terminal_id() else {
            return false;
        };
        let bytes: Vec<u8> = match key {
            "enter" => vec![b'\r'],
            "tab" => vec![b'\t'],
            "backtab" => b"\x1b[Z".to_vec(),
            "backspace" => vec![0x7f],
            "delete" => b"\x1b[3~".to_vec(),
            "up" => b"\x1b[A".to_vec(),
            "down" => b"\x1b[B".to_vec(),
            "right" => b"\x1b[C".to_vec(),
            "left" => b"\x1b[D".to_vec(),
            "home" => b"\x1b[H".to_vec(),
            "end" => b"\x1b[F".to_vec(),
            _ => return false,
        };
        self.terminal_input(tid, bytes, cx);
        true
    }

    /// Scroll terminal `id`'s scrollback by `delta` lines (positive = older).
    fn terminal_scroll(&mut self, id: u64, delta: i32, cx: &mut Context<Self>) {
        if let Some(t) = self.terminals.get(&id) {
            t.scroll(delta);
            cx.notify();
        }
    }

    /// Record a terminal pane's painted geometry (called by its element each
    /// frame) so mouse positions can be mapped to grid cells.
    pub fn set_terminal_geometry(
        &mut self,
        id: u64,
        origin: Point<Pixels>,
        cell_w: Pixels,
        cell_h: Pixels,
        cols: usize,
        rows: usize,
    ) {
        self.terminal_geom.insert(
            id,
            TermGeom {
                origin,
                cell_w,
                cell_h,
                cols,
                rows,
            },
        );
    }

    /// Map a window position to a `(row, col, right_half)` cell in terminal `id`,
    /// clamped to the grid. `None` if the geometry isn't known yet.
    fn terminal_cell_at(&self, id: u64, pos: Point<Pixels>) -> Option<(usize, usize, bool)> {
        let g = self.terminal_geom.get(&id)?;
        let cw = f32::from(g.cell_w).max(1.0);
        let ch = f32::from(g.cell_h).max(1.0);
        let relx = (f32::from(pos.x) - f32::from(g.origin.x)) / cw;
        let rely = (f32::from(pos.y) - f32::from(g.origin.y)) / ch;
        let col = (relx.floor().max(0.0) as usize).min(g.cols.saturating_sub(1));
        let row = (rely.floor().max(0.0) as usize).min(g.rows.saturating_sub(1));
        let right = (relx - relx.floor()) >= 0.5;
        Some((row, col, right))
    }

    /// Begin a mouse text selection in the terminal under `pos` (if any). Returns
    /// whether a terminal selection was started.
    fn terminal_selection_begin(&mut self, pos: Point<Pixels>, cx: &mut Context<Self>) -> bool {
        let Some(pane) = self.pane_at_position(pos) else {
            return false;
        };
        let Some(tid) = self.pane_terminal_id(pane) else {
            return false;
        };
        if pane != self.selected_pane {
            self.select_pane(pane, cx);
        }
        if let Some((row, col, right)) = self.terminal_cell_at(tid, pos)
            && let Some(t) = self.terminals.get(&tid)
        {
            t.selection_start(row, col, right);
            self.terminal_drag = Some(tid);
            cx.notify();
        }
        true
    }

    /// Extend the active terminal selection to `pos` while dragging.
    fn terminal_selection_drag(&mut self, pos: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(tid) = self.terminal_drag else {
            return;
        };
        if let Some((row, col, right)) = self.terminal_cell_at(tid, pos)
            && let Some(t) = self.terminals.get(&tid)
        {
            t.selection_update(row, col, right);
            cx.notify();
        }
    }

    /// Copy the active terminal's selection to the system clipboard. Returns
    /// whether a terminal was active (so the caller skips the buffer copy path).
    fn terminal_copy(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(tid) = self.active_terminal_id() else {
            return false;
        };
        match self.terminals.get(&tid).and_then(|t| t.selection_text()) {
            Some(text) => {
                self.set_clipboard(&text, cx);
                if let Some(t) = self.terminals.get(&tid) {
                    t.selection_clear();
                }
                self.echo("Copied", cx);
                cx.notify();
            }
            None => self.echo("No selection", cx),
        }
        true
    }

    /// Paste the system clipboard into the active terminal. Returns whether a
    /// terminal was active.
    fn terminal_paste(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(tid) = self.active_running_terminal_id() else {
            return false;
        };
        if let Some(text) = cx.read_from_clipboard().and_then(|i| i.text())
            && !text.is_empty()
        {
            self.terminal_input(tid, text.into_bytes(), cx);
        }
        true
    }

    /// Re-fit terminal `id` to a measured cell grid (called by the element each
    /// frame); a no-op when the size is unchanged.
    pub fn terminal_resize_fit(
        &mut self,
        id: u64,
        cols: usize,
        rows: usize,
        cell_w: u16,
        cell_h: u16,
    ) {
        if let Some(t) = self.terminals.get_mut(&id) {
            t.resize(cols, rows, cell_w, cell_h);
        }
    }

    /// A render snapshot of terminal `id`'s visible grid, with theme defaults for
    /// default-colored cells.
    pub fn terminal_snapshot(
        &self,
        id: u64,
        default_fg: u32,
        default_bg: u32,
    ) -> Option<crate::terminal::TermSnapshot> {
        self.terminals
            .get(&id)
            .map(|t| t.snapshot(default_fg, default_bg))
    }

    /// Apply a terminal event from its background event loop.
    fn on_terminal_event(&mut self, id: u64, event: TermEvent, cx: &mut Context<Self>) {
        match event {
            TermEvent::Wakeup => {}
            TermEvent::Title(title) => {
                if let Some(t) = self.terminals.get_mut(&id) {
                    t.title = title;
                }
            }
            TermEvent::Exit => {
                if let Some(t) = self.terminals.get_mut(&id) {
                    t.exited = true;
                    let name = t.buffer_name.clone();
                    self.echo(format!("Terminal {name} exited"), cx);
                }
            }
        }
        cx.notify();
    }

    /// Whether the cursor should currently be painted: always, unless blinking
    /// is on and we're in the hidden half of the blink cycle.
    pub fn cursor_blink_visible(&self) -> bool {
        self.cursor_blink_visible
    }

    /// Mark cursor activity (a keystroke): show the cursor and restart the blink
    /// cycle from "shown", so it never blinks off mid-typing. Bumping the token
    /// kills any in-flight blink timer from the previous cycle.
    fn reset_blink(&mut self, cx: &mut Context<Self>) {
        if !self.cursor_blink_visible {
            self.cursor_blink_visible = true;
            cx.notify();
        }
        self.blink_token = self.blink_token.wrapping_add(1);
        self.schedule_blink(cx);
    }

    /// React to the OS window gaining/losing focus. While inactive the cursor
    /// stops blinking and holds steady (shown); on reactivation it resumes.
    fn set_window_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if self.window_active == active {
            return;
        }
        self.window_active = active;
        if active {
            self.reset_blink(cx); // shown + restart the blink chain
        } else {
            // Kill the in-flight chain and leave the cursor steady.
            self.blink_token = self.blink_token.wrapping_add(1);
            if !self.cursor_blink_visible {
                self.cursor_blink_visible = true;
            }
            cx.notify();
        }
    }

    /// Schedule the next blink toggle, if `cursor-blink` is enabled. Each tick
    /// re-checks the setting and the token (so a disabled cursor or a fresh
    /// keystroke stops the chain), toggles visibility, and reschedules itself.
    fn schedule_blink(&mut self, cx: &mut Context<Self>) {
        let settings = cx.global::<crate::settings::Settings>();
        if !settings.cursor_blink() {
            return;
        }
        let interval = Duration::from_millis(settings.cursor_blink_interval());
        let token = self.blink_token;
        let timer = cx.background_executor().timer(interval);
        cx.spawn(async move |this, cx| {
            timer.await;
            this.update(cx, |editor, cx| {
                if editor.blink_token != token {
                    return; // superseded by a keystroke (newer chain owns blinking)
                }
                // Stop (leaving the cursor shown) when blinking is disabled or the
                // window is inactive; the chain restarts on enable / reactivation.
                if !cx.global::<crate::settings::Settings>().cursor_blink() || !editor.window_active
                {
                    if !editor.cursor_blink_visible {
                        editor.cursor_blink_visible = true;
                        cx.notify();
                    }
                    return;
                }
                editor.cursor_blink_visible = !editor.cursor_blink_visible;
                cx.notify();
                editor.schedule_blink(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Call Steel fn `callback` after `ms` milliseconds (Emacs `run-with-timer`).
    fn run_with_timer(&mut self, ms: u64, callback: String, cx: &mut Context<Self>) {
        let timer = cx.background_executor().timer(Duration::from_millis(ms));
        cx.spawn(async move |this, cx| {
            timer.await;
            this.update(cx, |editor, _| {
                editor.script.dispatch_action(&callback, None)
            })
            .ok();
        })
        .detach();
    }

    fn open_transient(
        &mut self,
        title: String,
        columns: Vec<crate::script::TransientColumn>,
        cx: &mut Context<Self>,
    ) {
        self.transient = Some(TransientState { title, columns });
        cx.notify();
    }

    fn transient_quit(&mut self, cx: &mut Context<Self>) {
        if self.transient.take().is_some() {
            cx.notify();
        }
    }

    /// The command bound to `chord` in the open transient, if any (any column).
    fn transient_command(&self, chord: &str) -> Option<String> {
        self.transient
            .as_ref()?
            .columns
            .iter()
            .flat_map(|c| c.rows.iter())
            .find(|r| r.key == chord)
            .map(|r| r.command.clone())
    }

    /// A mouse click on a transient row: close the popup and run its command,
    /// mirroring the keyboard dispatch in `on_key_down`.
    pub fn transient_click(&mut self, command: &str, cx: &mut Context<Self>) {
        if self.transient.take().is_none() {
            return;
        }
        cx.notify();
        // Transient rows store a bare command name (e.g. `magit-do-commit`); wrap
        // it in parens so it's *called*, matching the keyboard path.
        self.run_script(&format!("({command})"), cx);
    }

    /// A render snapshot of the open transient popup.
    pub fn transient_view(&self) -> Option<TransientView> {
        self.transient.as_ref().map(|t| TransientView {
            title: t.title.clone(),
            columns: t
                .columns
                .iter()
                .map(|c| TransientColumnView {
                    title: c.title.clone(),
                    rows: c
                        .rows
                        .iter()
                        .map(|r| TransientRow {
                            key: pretty_key(&r.key),
                            label: r.label.clone(),
                            command: r.command.clone(),
                        })
                        .collect(),
                })
                .collect(),
        })
    }

    // --- LSP --------------------------------------------------------------

    /// The URI identifying the active buffer for LSP. File buffers use a
    /// `file://` URI; unsaved buffers (e.g. `*scratch*`) get a stable
    /// `untitled:` URI so they can still be served (the built-in Steel LSP just
    /// uses it as a key).
    fn active_uri(&self) -> Option<String> {
        match self.active_path.as_deref() {
            Some(path) => Some(lsp::path_to_uri(path)),
            None => Some(format!("untitled:{}", self.active_name)),
        }
    }

    /// The LSP language id for the active buffer: the Scheme-configured mapping
    /// for its file extension, or — when there's no file — the active mode's
    /// language (so `*scratch*` in steel-mode resolves to `scheme`).
    fn active_language(&self) -> Option<String> {
        if let Some(language) = self
            .active_path
            .as_ref()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .and_then(|ext| self.extensions.get(ext))
        {
            // Editor config / extension Scheme uses a distinct `macros-scheme`
            // server (seeded with the editor API); plain `.scm` stays on the
            // vanilla `scheme` server. The split is by path, so it can't live in
            // the extension map — apply it here.
            if language == "scheme"
                && self
                    .active_path
                    .as_deref()
                    .is_some_and(crate::runtime::is_macros_scheme_path)
            {
                return Some("macros-scheme".to_string());
            }
            return Some(language.clone());
        }
        self.modes.active_language().map(str::to_string)
    }

    /// The wire `languageId` for the active buffer's `didOpen`. Resolved from the
    /// file extension via the `language_ids` map (`set-language-id`), falling
    /// back to the server key (`active_language`) when no override is set. This
    /// is what lets a `.tsx` file open as `typescriptreact` on the shared
    /// `typescript` server, so JSX isn't flagged as syntax errors.
    fn active_language_id(&self, server_key: &str) -> String {
        self.active_path
            .as_ref()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .and_then(|ext| self.language_ids.get(ext))
            .cloned()
            .unwrap_or_else(|| server_key.to_string())
    }

    /// Run the active buffer's mode hooks once (Emacs `run-mode-hooks`). For each
    /// active mode, every Scheme function registered with `add-hook` is called
    /// (deduplicated, so a function on both `fundamental` and `steel` runs once).
    /// This is where steel-mode's `(add-hook "steel" "enable-lsp")` brings up the
    /// LSP — entirely Scheme-driven, with the default set in `steel-mode.scm`.
    pub fn run_mode_hooks_once(&mut self, cx: &mut Context<Self>) {
        if self.hooks_ran {
            return;
        }
        self.hooks_ran = true;
        let mut seen = std::collections::HashSet::new();
        let mut to_run = Vec::new();
        for mode in self.modes.names() {
            if let Some(functions) = self.hooks.get(mode) {
                for function in functions {
                    if seen.insert(function.clone()) {
                        to_run.push(function.clone());
                    }
                }
            }
        }
        for function in to_run {
            // Dispatch to the engine thread; results apply via the drain task.
            self.script.dispatch_action(&function, None);
        }
        let _ = cx;
    }

    /// Diagnostics for the active buffer, mapped to byte offsets.
    pub fn active_diagnostics(&self) -> Vec<Diagnostic> {
        let Some(uri) = self.active_uri() else {
            return Vec::new();
        };
        let Some(raw) = self.diagnostics.get(&uri) else {
            return Vec::new();
        };
        raw.iter()
            .map(|d| {
                let start = self.buffer.position_to_byte(d.start.0, d.start.1);
                let end = self.buffer.position_to_byte(d.end.0, d.end.1).max(start);
                Diagnostic {
                    range: start..end,
                    severity: d.severity,
                    message: d.message.clone(),
                }
            })
            .collect()
    }

    /// `E:n W:n` summary for the mode line, or `None` if clean.
    pub fn diagnostic_summary(&self) -> Option<String> {
        let raw = self
            .active_uri()
            .and_then(|uri| self.diagnostics.get(&uri))?;
        if raw.is_empty() {
            return None;
        }
        let errors = raw.iter().filter(|d| d.severity == 1).count();
        let warnings = raw.iter().filter(|d| d.severity == 2).count();
        Some(format!("E:{errors} W:{warnings}"))
    }

    /// The diagnostic message at point, if any (shown in the echo area).
    fn diagnostic_at_offset(&self, offset: usize) -> Option<String> {
        self.active_diagnostics()
            .into_iter()
            .find(|d| d.range.contains(&offset) || d.range.start == offset)
            .map(|d| d.message)
    }

    fn diagnostic_at_point(&self) -> Option<String> {
        self.diagnostic_at_offset(self.point())
    }

    /// `completion-at-point`: try the Scheme CAPFs first (on the engine thread,
    /// results arrive via `apply_capf_result`); if none are registered, go
    /// straight to LSP.
    fn completion_at_point(&mut self, cx: &mut Context<Self>) {
        // The ANVIL REPL completes against its *live image* (its actual
        // bindings, including everything you've defined this session), not the
        // main engine's CAPFs or LSP — SLIME-style symbol completion. Uses a
        // Lisp-symbol boundary so `string->number` completes as one token.
        if self.modes.names().last().copied() == Some("anvil-repl-mode") {
            let point = self.point();
            let start = self.buffer.backward_symbol(point);
            let prefix = self.buffer.slice_to_string(start..point);
            self.anvil.complete(&prefix, start, point);
            return;
        }
        let capf_enabled = cx.global::<crate::settings::Settings>().completion_capf();
        if capf_enabled && !self.capf_functions.is_empty() {
            let point = self.point();
            let start = self.buffer.backward_word(point);
            let prefix = self.buffer.slice_to_string(start..point);
            self.publish_point();
            self.script
                .dispatch_capf(self.capf_functions.clone(), &prefix, start, point);
            return;
        }
        self.completion_at_point_lsp(cx);
    }

    /// Request completions from the active buffer's language server.
    fn completion_at_point_lsp(&mut self, cx: &mut Context<Self>) {
        let (Some(language), Some(uri)) = (self.active_language(), self.active_uri()) else {
            self.echo("No language server for this buffer", cx);
            return;
        };
        let point = self.point();
        let (line, character) = self.buffer.byte_to_position(point);
        let word_range = self.buffer.backward_word(point)..point;
        let sent = if let Some(server) = self.servers.get_mut(&language) {
            if server.ready && server.open.contains(&uri) {
                let id = server.client.completion(&uri, line, character);
                server.pending_completion = Some((id, word_range));
                true
            } else {
                false
            }
        } else {
            false
        };
        if !sent {
            self.echo("No language server for this buffer", cx);
        }
    }

    /// Open the completion popup from Scheme CAPF results. An empty item list
    /// means no CAPF produced anything, so fall back to LSP (so a buffer with a
    /// language server still completes when its CAPFs decline).
    fn apply_capf_result(
        &mut self,
        start: usize,
        end: usize,
        items: Vec<(String, Option<String>)>,
        cx: &mut Context<Self>,
    ) {
        if items.is_empty() {
            self.completion_at_point_lsp(cx);
            return;
        }
        let len = self.buffer.len();
        let range = start.min(len)..end.min(len);
        let items = items
            .into_iter()
            .map(|(label, annotation)| CompletionItem {
                insert_text: label.clone(),
                label,
                detail: annotation,
                kind: None,
                documentation: None,
                additional_edits: Vec::new(),
                data: serde_json::Value::Null,
                resolved: true,
            })
            .collect();
        self.show_completion(items, range, cx);
    }

    fn show_completion(
        &mut self,
        items: Vec<CompletionItem>,
        word_range: Range<usize>,
        cx: &mut Context<Self>,
    ) {
        let was_auto = std::mem::take(&mut self.completion_auto_pending);
        if items.is_empty() {
            if !was_auto {
                self.echo("No completions", cx); // stay quiet for auto-triggers
            }
            return;
        }
        // Presentation is Scheme-configurable: an in-buffer popup (default) or
        // the minibuffer.
        let style = cx
            .global::<crate::settings::Settings>()
            .completion_style()
            .to_string();
        if style == "minibuffer" {
            let prefix = self.buffer.slice_to_string(word_range.clone());
            self.completion_items = items.iter().map(|i| i.label.clone()).collect();
            self.completion_range = Some(word_range);
            self.open_minibuffer(
                "Complete: ",
                MinibufferPurpose::CompleteAtPoint,
                &prefix,
                cx,
            );
        } else {
            self.completion_popup = Some(CompletionPopup {
                items,
                anchor: word_range.start,
                selected: 0,
            });
            self.completion_resolve_selected();
            cx.notify();
        }
    }

    // --- Command dispatch -------------------------------------------------

    /// Apply a command, honoring a pending universal-argument (Emacs `C-u N`):
    /// a repeatable command runs `N` times (as one undo group for edits); others
    /// run once and the argument is discarded. Every command — from a key, a
    /// script, or an OS-special action — flows through here.
    fn apply_command(&mut self, command: EditorCommand, cx: &mut Context<Self>) {
        match self.prefix_arg.take() {
            Some(n) if n > 1 && is_repeatable(&command) => {
                self.status = SharedString::default();
                self.begin_undo_group();
                for _ in 0..n {
                    self.apply_command_once(command.clone(), cx);
                }
                self.end_undo_group();
            }
            _ => self.apply_command_once(command, cx),
        }
    }

    /// Apply a single command exactly once.
    fn apply_command_once(&mut self, command: EditorCommand, cx: &mut Context<Self>) {
        use EditorCommand::*;
        let yank_like = matches!(command, Yank | YankPop);
        match command {
            ForwardChar => {
                let o = self.buffer.next_grapheme(self.point());
                self.goto(o, cx);
            }
            BackwardChar => {
                let o = self.buffer.prev_grapheme(self.point());
                self.goto(o, cx);
            }
            NextLine => {
                let o = self.line_offset(1);
                self.goto(o, cx);
            }
            PreviousLine => {
                let o = self.line_offset(-1);
                self.goto(o, cx);
            }
            ForwardWord => {
                let o = self.buffer.forward_word(self.point());
                self.goto(o, cx);
            }
            ForwardWordStart => {
                let o = self.buffer.forward_word_start(self.point());
                self.goto(o, cx);
            }
            BackwardWord => {
                let o = self.buffer.backward_word(self.point());
                self.goto(o, cx);
            }
            BeginningOfLine => {
                let o = self.buffer.line_range_at(self.point()).start;
                self.goto(o, cx);
            }
            EndOfLine => {
                let o = self.buffer.line_range_at(self.point()).end;
                self.goto(o, cx);
            }
            BeginningOfBuffer => self.goto(0, cx),
            EndOfBuffer => {
                let o = self.buffer.len();
                self.goto(o, cx);
            }
            ExtendBackwardChar => {
                let o = self.buffer.prev_grapheme(self.point());
                self.select_to(o, cx);
            }
            ExtendForwardChar => {
                let o = self.buffer.next_grapheme(self.point());
                self.select_to(o, cx);
            }
            SetMark => self.set_mark(cx),
            ExchangePointAndMark => self.exchange_point_and_mark(cx),
            MarkWholeBuffer => self.mark_whole_buffer(cx),
            Insert(text) => self.insert_text(&text, cx),
            Newline => self.newline(cx),
            OpenLine => self.open_line(cx),
            IndentForTab => self.indent_for_tab(cx),
            DeleteChar => self.delete_char_forward(cx),
            DeleteBackwardChar => self.delete_char_backward(cx),
            TransposeChars => self.transpose_chars(cx),
            KillLine => self.kill_line(cx),
            KillWord => self.kill_word(cx),
            BackwardKillWord => self.backward_kill_word(cx),
            KillRegion => self.kill_region(cx),
            KillRingSave => self.kill_ring_save(cx),
            ClipboardCopy => self.clipboard_copy(cx),
            ClipboardCut => self.clipboard_cut(cx),
            ClipboardPaste => self.clipboard_paste(cx),
            ClipboardSet(text) => self.set_clipboard(&text, cx),
            Yank => self.yank(cx),
            YankPop => self.yank_pop(cx),
            Undo => self.undo(cx),
            Redo => self.redo(cx),
            UndoTreeDump(callback) => {
                let dump = self.undo_tree_dump();
                self.script.dispatch_action(&callback, Some(&dump));
            }
            ScrollUp => self.scroll_page(1, cx),
            ScrollDown => self.scroll_page(-1, cx),
            Recenter => self.recenter(cx),
            KeyboardQuit => self.keyboard_quit(cx),
            ExecuteExtendedCommand => {
                self.open_minibuffer("M-x ", MinibufferPurpose::ExecuteCommand, "", cx);
            }
            ListMessages => self.list_messages(cx),
            ListLspLog => self.list_lsp_log(cx),
            CheckForUpdates => self.check_for_updates(cx),
            FindFile => self.find_file_prompt(cx),
            OpenFile(path) => self.find_file(&path, cx),
            SwitchBuffer => {
                self.open_minibuffer(
                    "Switch to buffer: ",
                    MinibufferPurpose::SwitchBuffer,
                    "",
                    cx,
                );
            }
            SwitchToBufferNamed(name) => self.switch_to_buffer(&name, cx),
            KillBufferNamed(name) => self.kill_buffer_named(&name, cx),
            SetMajorMode(mode) => {
                let mut modes = ModeStack::default();
                modes.push(Mode::new("fundamental"));
                modes.push(Mode::new(mode));
                self.modes = modes;
                cx.notify();
            }
            NextBuffer => self.next_buffer(cx),
            PreviousBuffer => self.previous_buffer(cx),
            SaveBuffer => self.save_buffer(cx),
            SaveSomeBuffers => self.save_some_buffers(cx),
            RevertBuffer => self.revert_buffer(cx),
            SelectWindow(id) => self.select_pane(id, cx),
            OpenFileOtherWindow(path) => self.open_file_other_window(path, cx),
            SetLineIcon(line, col, name) => {
                self.line_icons
                    .insert(line, (col, SharedString::from(name)));
                cx.notify();
            }
            ClearLineIcons => {
                self.line_icons.clear();
                cx.notify();
            }
            SetLineImage {
                line,
                path,
                max_width,
            } => self.set_line_image(line, path, max_width, cx),
            ClearLineImages => {
                if !self.line_images.is_empty() {
                    self.line_images.clear();
                    self.ensure_cursor_visible();
                    cx.notify();
                }
            }
            SetLineIndent { line, cols } => {
                if cols == 0 {
                    self.overlays.indents.remove(&line);
                } else {
                    self.overlays.indents.insert(line, cols);
                }
                cx.notify();
            }
            ClearLineIndents => {
                if !self.overlays.indents.is_empty() {
                    self.overlays.indents.clear();
                    cx.notify();
                }
            }
            SetLineScale { line, scale } => {
                if scale <= 1.0 {
                    self.overlays.scales.remove(&line);
                } else {
                    self.overlays.scales.insert(line, scale);
                }
                cx.notify();
            }
            ClearLineScales => {
                if !self.overlays.scales.is_empty() {
                    self.overlays.scales.clear();
                    cx.notify();
                }
            }
            ToggleLineNumbers => {
                // Toggle for the current buffer's major mode, so it mirrors
                // Emacs's per-mode `display-line-numbers-mode`.
                let major = self.modes.names().last().map(|s| s.to_string());
                let key = match &major {
                    Some(m) => format!("line-numbers:{m}"),
                    None => "line-numbers".to_string(),
                };
                let on = cx
                    .global::<crate::settings::Settings>()
                    .line_numbers_for(major.as_deref());
                self.toggle_setting(key, on, "Line numbers", cx);
            }
            ToggleScrollBar => {
                let on = cx.global::<crate::settings::Settings>().scroll_bar();
                self.toggle_setting("scroll-bar", on, "Scroll bar", cx);
            }
            ToggleWordWrap => {
                // Toggle for the current buffer's major mode, mirroring Emacs's
                // per-mode `visual-line-mode`.
                let major = self.modes.names().last().map(|s| s.to_string());
                let key = match &major {
                    Some(m) => format!("word-wrap:{m}"),
                    None => "word-wrap".to_string(),
                };
                let on = cx
                    .global::<crate::settings::Settings>()
                    .word_wrap_for(major.as_deref());
                // Leaving word-wrap resets any horizontal scroll to a clean state.
                self.h_scroll_offset = px(0.);
                self.toggle_setting(key, on, "Word wrap", cx);
            }
            ToggleElectricPair => {
                self.electric_pair = !self.electric_pair;
                self.echo(
                    format!(
                        "electric-pair-mode {}",
                        if self.electric_pair {
                            "enabled"
                        } else {
                            "disabled"
                        }
                    ),
                    cx,
                );
            }
            KillBuffer => self.kill_buffer(cx),
            KillBufferAndWindow => self.kill_buffer_and_window(cx),
            Quit => self.quit(cx),
            SetLsp(language, command) => {
                self.lsp_commands.insert(language, command);
            }
            SetExtension(ext, language) => {
                self.extensions.insert(ext, language);
            }
            SetLanguageId(ext, language_id) => {
                self.language_ids.insert(ext, language_id);
            }
            SetOption(key, value) => {
                cx.update_global::<crate::settings::Settings, _>(|settings, _| {
                    settings.set(key, value);
                });
                self.script
                    .set_options(cx.global::<crate::settings::Settings>().snapshot());
                cx.notify();
            }
            LspDefinition => self.lsp_goto("textDocument/definition", "definition", cx),
            LspDeclaration => self.lsp_goto("textDocument/declaration", "declaration", cx),
            LspTypeDefinition => {
                self.lsp_goto("textDocument/typeDefinition", "type definition", cx)
            }
            LspImplementation => self.lsp_goto("textDocument/implementation", "implementation", cx),
            LspReferences => self.lsp_references(cx),
            LspHover => self.lsp_hover(cx),
            LspSignatureHelp => self.lsp_signature_help(cx),
            LspDocumentSymbols => self.lsp_document_symbols(cx),
            LspCodeAction => self.lsp_code_action(cx),
            LspFormatBuffer => self.lsp_format(cx),
            LspCallHierarchyIncoming => self.lsp_call_hierarchy(true, cx),
            LspCallHierarchyOutgoing => self.lsp_call_hierarchy(false, cx),
            LspPrepareRename => self.lsp_prepare_rename(cx),
            LspCodeLens => self.lsp_code_lens(cx),
            LspXrefVisit => self.lsp_xref_visit(cx),
            LspApplyCodeAction => self.lsp_apply_code_action(cx),
            LspRename(name) => self.lsp_rename(name, cx),
            LspWorkspaceSymbol(query) => self.lsp_workspace_symbol(query, cx),
            LspRequest {
                method,
                kind,
                params,
                callback,
            } => self.lsp_script_request(method, kind, params, callback, cx),
            LspGoto {
                uri,
                line,
                character,
            } => self.goto_file_position(uri, line, character, cx),
            LspApplyWorkspaceEdit(json) => self.lsp_apply_workspace_edit_json(json, cx),
            LspApplyTextEdits(json) => self.lsp_apply_text_edits_json(json, cx),
            XrefPushMarker => self.xref_push_marker(),
            XrefPopMarker => self.xref_pop_marker(cx),
            SetLspFormatOnSave(lang, on) => {
                if on {
                    self.lsp_format_on_save.insert(lang);
                } else {
                    self.lsp_format_on_save.remove(&lang);
                }
            }
            LspSemanticTokens(callback) => self.lsp_semantic_tokens(callback, cx),
            LspSemanticTokensRange(callback) => self.lsp_semantic_tokens_range(callback, cx),
            LspSemanticTokensDelta(callback) => self.lsp_semantic_tokens_delta(callback, cx),
            TreeSitterQuery { query, callback } => self.tree_sitter_query(query, callback),
            TreeSitterNodeAt { byte, callback } => self.tree_sitter_node_at(byte, callback),
            CompletionAtPoint => self.completion_at_point(cx),
            CompletionNext => self.completion_next(cx),
            CompletionPrevious => self.completion_previous(cx),
            CompletionAccept => self.completion_accept(cx),
            CompletionAbort => self.completion_abort(cx),
            Helm(specs) => {
                let sources = helm_sources_from_specs(specs);
                self.helm_open("Helm: ".to_string(), sources, String::new(), cx);
            }
            HelmFrom(initial, specs) => {
                let sources = helm_sources_from_specs(specs);
                self.helm_open("Helm: ".to_string(), sources, initial, cx);
            }
            HelmMx => self.helm_mx(cx),
            HelmNext => self.helm_next(cx),
            HelmPrevious => self.helm_previous(cx),
            HelmAccept => self.helm_accept(cx),
            HelmAbort => self.helm_abort(cx),
            HelmActions => self.helm_action_menu(cx),
            HelmToggleMark => self.helm_toggle_mark(cx),
            EvilMode => self.evil_enable(cx),
            EvilNormalState => self.evil_normal_state(cx),
            EvilInsertState => self.evil_insert_state(cx),
            EvilVisualState => self.evil_visual_state(cx),
            EvilDisable => self.evil_disable(cx),
            EvilScrollDown => self.scroll_half(1, cx),
            EvilScrollUp => self.scroll_half(-1, cx),
            EvilSearchForward => self.evil_search_forward(cx),
            EvilSearchBackward => self.evil_search_backward(cx),
            EvilSearchNext => self.evil_search_next(cx),
            EvilSearchPrevious => self.evil_search_previous(cx),
            EvilPasteAfter => self.evil_paste(true, cx),
            EvilPasteBefore => self.evil_paste(false, cx),
            EvilSetLinewise => self.evil_yank_linewise = true,
            EvilSetCharwise => self.evil_yank_linewise = false,
            EvilExCommand => {
                self.open_minibuffer(":", MinibufferPurpose::ExCommand, "", cx);
            }
            IsearchForward => self.isearch_start(true, cx),
            IsearchBackward => self.isearch_start(false, cx),
            EnableLsp => self.maybe_start_lsp(cx),
            AddHook(mode, function) => {
                let list = self.hooks.entry(mode).or_default();
                if !list.contains(&function) {
                    list.push(function);
                }
            }
            RemoveHook(mode, function) => {
                if let Some(list) = self.hooks.get_mut(&mode) {
                    list.retain(|f| f != &function);
                }
            }
            AddCapf(function) => {
                if !self.capf_functions.contains(&function) {
                    self.capf_functions.push(function);
                }
            }
            EvalExpression => {
                self.open_minibuffer("Eval: ", MinibufferPurpose::EvalExpression, "", cx);
            }
            EvalBuffer => {
                let source = self.buffer.full_text();
                self.eval_interactive(&source, cx);
            }
            EvalLastSexp => {
                let point = self.point();
                let text = self.buffer.slice_to_string(0..point);
                match last_sexp(&text) {
                    Some(range) => {
                        let source = text[range].to_string();
                        self.eval_interactive(&source, cx);
                    }
                    None => self.echo("Nothing to evaluate", cx),
                }
            }
            Message(text) => self.echo(text, cx),
            DefineKey(mode, sequence, command) => {
                self.keymap.define(mode, sequence, format!("({command})"));
            }
            SetFace(face, color) => match crate::theme::parse_color(&color) {
                Some(c) => {
                    cx.update_global::<Theme, ()>(|theme, _| theme.set(face, c));
                    cx.notify();
                }
                None => self.echo(format!("Invalid color: {color}"), cx),
            },
            LoadTheme(name) => match crate::runtime::theme_script(&name) {
                Some(source) => {
                    // Reset to an empty palette, then apply the theme's set-face
                    // calls so switching themes never leaves stale faces.
                    cx.update_global::<Theme, ()>(|theme, _| *theme = Theme::default());
                    self.run_script(source, cx);
                }
                None => self.echo(format!("No theme named {name}"), cx),
            },
            RenderBuffer {
                name,
                mode,
                text,
                line_faces,
            } => self.render_buffer(name, mode, text, line_faces, cx),
            GotoLine(n) => {
                let last = self.buffer.line_count().saturating_sub(1);
                let offset = self.buffer.line_range(n.min(last)).start;
                self.move_to(offset, cx);
            }
            GotoChar(n) => {
                let offset = n.min(self.buffer.len());
                self.move_to(offset, cx);
            }
            ReadString {
                prompt,
                initial,
                callback,
            } => {
                self.open_minibuffer(
                    &prompt,
                    MinibufferPurpose::SchemeCallback(callback),
                    &initial,
                    cx,
                );
            }
            AppendToBuffer { name, text } => self.append_to_buffer(name, text, cx),
            DiffBuffers { a, b, callback } => self.diff_buffers(&a, &b, &callback),
            BufferCopyLines {
                src,
                src_start,
                src_end,
                dst,
                dst_start,
                dst_end,
            } => self.buffer_copy_lines(&src, src_start, src_end, &dst, dst_start, dst_end, cx),
            BufferHighlightLines {
                name,
                start,
                end,
                face,
            } => self.buffer_highlight_lines(&name, start, end, face, cx),
            BufferClearHighlights { name } => self.buffer_clear_highlights(&name, cx),
            SnippetExpand(template) => self.snippet_expand(template, cx),
            SnippetNextField => self.snippet_step_field(1, cx),
            SnippetPrevField => self.snippet_step_field(-1, cx),
            AvyShowHints(records) => self.avy_show_hints(&records, cx),
            AvyClearHints => self.avy_clear_hints(cx),
            McAddBelow => self.mc_add_line(1, cx),
            McAddAbove => self.mc_add_line(-1, cx),
            McMarkNext => self.mc_mark_next(cx),
            McClear => self.mc_clear(cx),
            SetModelineProject(name) => {
                self.project_indicator = if name.is_empty() {
                    None
                } else {
                    Some(name.into())
                };
                cx.notify();
            }
            Transient { title, columns } => self.open_transient(title, columns, cx),
            TransientQuit => self.transient_quit(cx),
            SplitWindowBelow => self.split_window(true, cx),
            SplitWindowRight => self.split_window(false, cx),
            SplitWindowRightRatio(ratio) => self.split_window_ratio(false, ratio as f32, cx),
            OtherWindow => self.other_window(cx),
            DeleteWindow => self.delete_window(cx),
            DeleteOtherWindows => self.delete_other_windows(cx),
            EnlargeWindow => self.resize_window(true, 0.05, cx),
            ShrinkWindow => self.resize_window(true, -0.05, cx),
            EnlargeWindowHorizontally => self.resize_window(false, 0.05, cx),
            ShrinkWindowHorizontally => self.resize_window(false, -0.05, cx),
            StartKbdMacro => self.start_kbd_macro(cx),
            EndKbdMacro => self.end_kbd_macro(cx),
            CallLastKbdMacro => self.call_last_kbd_macro(cx),
            OverlayFace { start, end, face } => self.overlay_set_face(start, end, face, cx),
            OverlayHide { start, end } => self.overlay_hide(start, end, cx),
            FoldLines {
                start_line,
                end_line,
            } => self.overlay_fold_lines(start_line, end_line, cx),
            UnfoldAll => self.overlay_unfold_all(cx),
            SetFoldMarker { line, open } => self.overlay_set_fold_marker(line, open, cx),
            ClearFoldMarkers => self.overlay_clear_fold_markers(cx),
            ClearOverlays => self.overlay_clear(cx),
            ClearOverlayFaces => {
                // Faces only — NOT hides. Several features (rainbow-delimiters,
                // LSP highlight) repaint their faces on every edit via this; if
                // it also cleared hides it would wipe another feature's
                // concealment (e.g. markdown/org heading markers). Clear hides
                // with `clear-overlay-hides`.
                self.overlays.faces.clear();
                cx.notify();
            }
            ClearOverlayHides => {
                self.overlays.hidden.clear();
                cx.notify();
            }
            ClearLineSuffixes => {
                self.overlays.suffixes.clear();
                cx.notify();
            }
            PutTextProperty {
                start,
                end,
                key,
                value,
            } => {
                self.put_text_property(start, end, key, value);
            }
            SetLineSuffix { line, text, face } => {
                self.overlays
                    .suffixes
                    .insert(line, (text.into(), face.into()));
                cx.notify();
            }
            MakeProcess {
                id,
                program,
                args,
                on_output,
                on_exit,
            } => {
                self.make_process(id, program, args, on_output, on_exit, cx);
            }
            SendProcess { id, text } => self.send_to_process(id, &text, cx),
            KillProcess { id } => self.kill_process(id),
            AnvilEval { source, on_result } => self.anvil.eval(&source, &on_result),
            AnvilInterrupt => self.anvil.interrupt(),
            AnvilReset => self.anvil.reset(),
            AnvilEvalDefun => {
                let text = self.buffer.full_text();
                match top_level_form_at(&text, self.point()) {
                    Some(r) => self.anvil.eval(text[r].trim(), "anvil-on-result-echo"),
                    None => self.echo("anvil: no top-level form at point", cx),
                }
            }
            AnvilLoadFile => match &self.active_path {
                Some(path) => {
                    // `{:?}` renders a quoted, escaped Scheme string literal.
                    let form = format!("(load {:?})", path.to_string_lossy());
                    self.anvil.eval(&form, "anvil-on-result-echo");
                }
                None => self.echo("anvil: buffer is not visiting a file", cx),
            },
            AnvilDoc => {
                let text = self.buffer.full_text();
                match symbol_at(&text, self.point()) {
                    Some(sym) => self
                        .anvil
                        .query(crate::anvil::AnvilQuery::Doc(sym), "anvil-on-describe"),
                    None => self.echo("anvil: no symbol at point", cx),
                }
            }
            AnvilMacroexpand => {
                let text = self.buffer.full_text();
                let form = match self.region() {
                    Some(r) => text.get(r).map(str::to_string),
                    None => top_level_form_at(&text, self.point()).map(|r| text[r].to_string()),
                };
                match form.filter(|f| !f.trim().is_empty()) {
                    Some(form) => self.anvil.query(
                        crate::anvil::AnvilQuery::Macroexpand(form),
                        "anvil-on-describe",
                    ),
                    None => self.echo("anvil: no form to expand", cx),
                }
            }
            AnvilApropos(pattern) => self.anvil.query(
                crate::anvil::AnvilQuery::Apropos(pattern),
                "anvil-on-describe",
            ),
            MakeTerminal {
                id,
                name,
                program,
                args,
            } => {
                self.make_terminal(id, name, program, args, cx);
            }
            RunWithTimer { ms, callback } => self.run_with_timer(ms, callback, cx),
            DeleteRegion { start, end } => self.delete_region(start, end, cx),
            KillRegionBetween { start, end } => self.kill_range(start.min(end)..start.max(end), cx),
            ReplaceRegion { start, end, text } => self.replace_region(start, end, &text, cx),
            InsertAt { pos, text } => self.insert_at(pos, &text, cx),
            SetRegion { start, end } => {
                let len = self.buffer.len();
                self.mark_active = true;
                self.selected_range = start.min(len)..end.min(len);
                self.selection_reversed = false;
                self.ensure_cursor_visible();
                cx.notify();
            }
            UndoBoundary => self.undo_boundary(),
            BufferLocalSet { key, value } => {
                self.locals.insert(key, value);
            }
            CompletingRead {
                prompt,
                candidates,
                dynamic,
                initial,
                callback,
            } => {
                self.completing_candidates = candidates;
                match dynamic {
                    Some(function) => {
                        self.open_minibuffer(
                            &prompt,
                            MinibufferPurpose::SchemeCompletingDynamic(callback),
                            &initial,
                            cx,
                        );
                        // Seed the candidate list for the initial input.
                        self.helm_query_counter += 1;
                        let id = self.helm_query_counter;
                        self.mb_dynamic = Some((function.clone(), id));
                        self.script
                            .dispatch_candidates(usize::MAX, id, &function, &initial);
                    }
                    None => self.open_minibuffer(
                        &prompt,
                        MinibufferPurpose::SchemeCompleting(callback),
                        &initial,
                        cx,
                    ),
                }
            }
            ReadFileName { prompt, callback } => {
                let initial = self.default_directory_input();
                self.open_minibuffer(
                    &prompt,
                    MinibufferPurpose::SchemeReadFile(callback),
                    &initial,
                    cx,
                );
            }
            YesOrNo { prompt, callback } => {
                let prompt = format!("{prompt} (yes or no) ");
                self.open_minibuffer(&prompt, MinibufferPurpose::SchemeYesNo(callback), "", cx);
            }
        }
        self.last_was_yank = yank_like;
    }

    /// Run a tree-sitter query over the active buffer's live parse tree and hand
    /// the captures to Steel `callback` as `"capture\tstart\tend\tkind"` records
    /// (one per line). The tree lives here on the UI thread; only the serialized
    /// result crosses to the engine thread. No grammar / uncompilable query →
    /// empty string (the callback decides what that means).
    fn tree_sitter_query(&mut self, query: String, callback: String) {
        let records = self
            .highlighter
            .as_ref()
            .and_then(|h| h.query_captures(self.buffer.rope(), &query))
            .map(|caps| {
                caps.iter()
                    .map(|(name, start, end, kind)| format!("{name}\t{start}\t{end}\t{kind}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        self.script.dispatch_action(&callback, Some(&records));
    }

    /// Hand the node chain covering `byte` (innermost first) to Steel `callback`
    /// as `"kind\tstart\tend"` records — lets a script ask "what am I inside?".
    fn tree_sitter_node_at(&mut self, byte: usize, callback: String) {
        let records = self
            .highlighter
            .as_ref()
            .map(|h| {
                h.node_at(byte)
                    .iter()
                    .map(|(kind, start, end)| format!("{kind}\t{start}\t{end}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        self.script.dispatch_action(&callback, Some(&records));
    }

    /// Run a keybinding/interactive Steel expression. A bare `(builtin)` applies
    /// synchronously (instant motion, etc.); anything else (user functions,
    /// argument calls) is dispatched to the engine thread so a slow script can't
    /// freeze the UI — its commands arrive via `apply_script_outcome`.
    fn run_script(&mut self, source: &str, cx: &mut Context<Self>) {
        if let Some(commands) = self.script.builtins_for(source) {
            // One builtin: apply directly (motions/edits manage their own undo).
            // A sequence: group it so the compound binding undoes as one unit.
            if commands.len() == 1 {
                self.apply_command(commands.into_iter().next().unwrap(), cx);
            } else {
                self.begin_undo_group();
                for command in commands {
                    self.apply_command(command, cx);
                }
                self.end_undo_group();
            }
        } else {
            // Publish point so a command reading `(buffer-line)` /
            // `(current-buffer)` sees it as of this invocation.
            self.publish_point();
            self.script.dispatch(source, false);
        }
    }

    /// Run a key-resolved command and, if recording a keyboard macro, append it
    /// (skipping the macro-control commands themselves).
    fn dispatch_recorded(&mut self, expr: &str, cx: &mut Context<Self>) {
        if let Some(events) = self.macro_recording.as_mut()
            && !expr.contains("kbd-macro")
        {
            events.push(MacroEvent::Command(expr.to_string()));
        }
        self.run_script(expr, cx);
    }

    // --- Keyboard macros --------------------------------------------------

    fn start_kbd_macro(&mut self, cx: &mut Context<Self>) {
        self.macro_recording = Some(Vec::new());
        self.echo("Recording keyboard macro…", cx);
    }

    fn end_kbd_macro(&mut self, cx: &mut Context<Self>) {
        if let Some(rec) = self.macro_recording.take() {
            self.last_macro = rec;
            self.echo("Keyboard macro defined", cx);
        }
    }

    fn call_last_kbd_macro(&mut self, cx: &mut Context<Self>) {
        if self.last_macro.is_empty() {
            self.echo("No keyboard macro defined", cx);
            return;
        }
        for event in self.last_macro.clone() {
            match event {
                MacroEvent::Command(expr) => self.run_script(&expr, cx),
                MacroEvent::Text(text) => self.insert_text(&text, cx),
            }
        }
    }

    /// Evaluate Steel synchronously and apply its commands in order. Used at
    /// startup, where config (keymaps, options, themes) must take effect before
    /// the editor is driven. Blocks the caller — fine before the UI is live.
    pub fn run_script_blocking(&mut self, source: &str, cx: &mut Context<Self>) {
        match self.script.eval(source) {
            Ok(result) => {
                for command in result.commands {
                    self.apply_command(command, cx);
                }
            }
            Err(err) => self.echo(format!("error: {err}"), cx),
        }
    }

    /// Evaluate Steel source interactively (steel-mode): dispatch to the engine
    /// thread and echo the result (handled in `apply_script_outcome`).
    fn eval_interactive(&mut self, source: &str, _cx: &mut Context<Self>) {
        self.script.dispatch(source, true);
    }

    // --- Key handling -----------------------------------------------------

    /// One ASCII digit `0`–`9` if `key` is exactly that, else `None`.
    fn ascii_digit(key: &str) -> Option<i64> {
        let mut chars = key.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) if c.is_ascii_digit() => Some((c as u8 - b'0') as i64),
            _ => None,
        }
    }

    /// Handle a keystroke while reading (or about to start) a universal-argument.
    /// Returns `true` if the key was consumed as part of the argument (the caller
    /// stops here); `false` if the key should proceed to normal dispatch — in
    /// which case `self.prefix_arg` has been set so the command repeats.
    fn handle_prefix_arg(&mut self, ks: &Keystroke, cx: &mut Context<Self>) -> bool {
        let only_ctrl = ks.modifiers.control && !ks.modifiers.alt && !ks.modifiers.platform;
        let only_alt = ks.modifiers.alt && !ks.modifiers.control && !ks.modifiers.platform;
        let plain = !ks.modifiers.control && !ks.modifiers.alt && !ks.modifiers.platform;
        let digit = Self::ascii_digit(&ks.key);

        // `C-u`: begin reading, or multiply the running default by 4 (4, 16, …).
        if only_ctrl && ks.key == "u" {
            self.uarg_reading = true;
            self.uarg_digits = None;
            self.uarg_cu = self.uarg_cu.saturating_add(1);
            self.echo_prefix_arg(cx);
            return true;
        }

        if self.uarg_reading {
            // A digit (plain, or `M-<digit>`) extends the explicit number.
            if (plain || only_alt)
                && let Some(d) = digit
            {
                let acc = self
                    .uarg_digits
                    .unwrap_or(0)
                    .saturating_mul(10)
                    .saturating_add(d);
                self.uarg_digits = Some(acc);
                self.echo_prefix_arg(cx);
                return true;
            }
            // Any other key ends the argument: stash the count and fall through
            // so this key runs as the command.
            self.prefix_arg = Some(prefix_arg_value(self.uarg_digits, self.uarg_cu));
            self.reset_uarg();
            return false;
        }

        // Not yet reading: `M-<digit>` starts a digit-argument (Emacs).
        if only_alt && let Some(d) = digit {
            self.uarg_reading = true;
            self.uarg_digits = Some(d);
            self.uarg_cu = 0;
            self.echo_prefix_arg(cx);
            return true;
        }

        false
    }

    fn reset_uarg(&mut self) {
        self.uarg_reading = false;
        self.uarg_digits = None;
        self.uarg_cu = 0;
    }

    /// Echo the argument being built in the status line, Emacs-style.
    fn echo_prefix_arg(&mut self, cx: &mut Context<Self>) {
        let v = prefix_arg_value(self.uarg_digits, self.uarg_cu);
        self.status = format!("C-u {v}").into();
        cx.notify();
    }

    /// Mode-driven dispatch for `Ctrl-`/`Alt-` chords and prefix sequences.
    /// Plain printable keys are left to the IME (self-insert); OS-special keys
    /// (arrows, Backspace, …) are handled by GPUI actions in `main`.
    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        if ks.key.is_empty() {
            return; // modifier-only press
        }

        // Any keystroke is cursor activity: keep the cursor solid and restart the
        // blink cycle (no-op timer when `cursor-blink` is off).
        self.reset_blink(cx);

        // A transient popup is modal: C-g / Escape close it; a row's key runs its
        // command (the popup closes first); everything else is swallowed.
        if self.transient.is_some() {
            if (ks.modifiers.control && ks.key == "g") || ks.key == "escape" {
                self.transient_quit(cx);
            } else if let Some(command) = self.transient_command(&chord_string(ks)) {
                self.transient = None;
                cx.notify();
                // Transient rows store a bare command name (e.g. `magit-do-commit`);
                // wrap it in parens so it's *called*, not just evaluated to the
                // closure — the same convention `DefineKey` uses for keymaps.
                self.run_script(&format!("({command})"), cx);
            }
            cx.stop_propagation();
            return;
        }

        // Helm is modal: C-g aborts, chords resolve against the Scheme `helm`
        // keymap, any other Ctrl/Alt chord is swallowed, and plain text falls
        // through to the IME (→ `helm_input`).
        if self.helm.is_some() {
            if ks.modifiers.control && ks.key == "g" {
                self.helm_abort(cx);
                cx.stop_propagation();
                return;
            }
            let chord = chord_string(ks);
            if let Some(expr) = self.keymap.resolve_mode("helm", &chord).map(str::to_string) {
                self.run_script(&expr, cx);
                cx.stop_propagation();
                return;
            }
            if ks.modifiers.control || ks.modifiers.alt {
                cx.stop_propagation();
            }
            return;
        }

        // While the minibuffer is open, only C-g (abort) is handled here; other
        // chords are swallowed so they can't reach the main buffer. Plain and
        // OS-special keys fall through to the IME / GPUI actions, which route to
        // the minibuffer.
        if self.minibuffer.is_some() {
            if ks.modifiers.control && ks.key == "g" {
                self.mb_abort(cx);
                cx.stop_propagation();
            } else if self.is_isearch() && ks.modifiers.control && ks.key == "s" {
                // C-s within isearch: repeat forward (Emacs).
                self.isearch_repeat(true, cx);
                cx.stop_propagation();
            } else if self.is_isearch() && ks.modifiers.control && ks.key == "r" {
                // C-r within isearch: repeat backward.
                self.isearch_repeat(false, cx);
                cx.stop_propagation();
            } else if ks.modifiers.alt && ks.key == "p" {
                self.mb_history(-1, cx); // M-p: older history
                cx.stop_propagation();
            } else if ks.modifiers.alt && ks.key == "n" {
                self.mb_history(1, cx); // M-n: newer history
                cx.stop_propagation();
            } else if ks.modifiers.control || ks.modifiers.alt {
                cx.stop_propagation();
            }
            return;
        }

        // While the completion popup is open, `Ctrl-`/`Alt-` chords resolve
        // against the Scheme-defined `completion` keymap first (C-n/C-p/C-g, …).
        // An unbound chord dismisses the popup and is then handled normally; plain
        // text keys fall through to self-insert, which re-filters the popup.
        if self.completion_popup.is_some() && (ks.modifiers.control || ks.modifiers.alt) {
            let chord = chord_string(ks);
            if let Some(expr) = self
                .keymap
                .resolve_mode("completion", &chord)
                .map(str::to_string)
            {
                self.pending_keys.clear();
                self.run_script(&expr, cx);
                cx.stop_propagation();
                return;
            }
            self.completion_popup = None; // dismiss; fall through to normal dispatch
        }

        // An embedded terminal grabs the keyboard: keys are translated to PTY
        // bytes rather than editor commands. `C-x` stays the editor prefix (so
        // window/buffer management still works), as does any in-progress prefix
        // sequence — everything else (including C-c, arrows, C-r…) goes to the shell.
        // Once the shell has exited the terminal is dead: it releases the keyboard
        // so normal editor dispatch (M-x, C-x k, …) resumes in the leftover buffer.
        if let Some(tid) = self.active_running_terminal_id() {
            // `C-x` is the editor prefix, an in-progress sequence continues, and
            // platform (cmd) chords fall through to the editor — so cmd-c/cmd-v
            // (terminal copy/paste) and cmd-q still work.
            let editor_prefix = !self.pending_keys.is_empty()
                || (ks.modifiers.control && ks.key == "x")
                || ks.modifiers.platform;
            if !editor_prefix {
                if let Some(bytes) = keystroke_to_bytes(ks) {
                    self.terminal_input(tid, bytes, cx);
                }
                cx.stop_propagation();
                return;
            }
        }

        // Evil Normal/Visual: every plain key is a command, resolved against the
        // evil keymap (with prefix support for `g g`, leader `space …`). Ctrl/Alt
        // chords fall through to the normal Emacs dispatch, so e.g. C-x C-s still
        // works in normal mode.
        if self.evil_intercepts()
            && !ks.modifiers.control
            && !ks.modifiers.alt
            && !ks.modifiers.platform
        {
            self.pending_keys.push(chord_string(ks));
            let sequence = self.pending_keys.join(" ");
            let state = self.evil_keymap();
            // The active major mode (top of the stack) scopes bindings, so an
            // `evil-normal:rust` keymap can shadow the global `evil-normal`.
            let major: Option<String> = self.modes.names().last().map(|s| s.to_string());
            let resolved = self
                .keymap
                .resolve_evil(state, major.as_deref(), &sequence)
                .map(str::to_string);
            if let Some(expr) = resolved {
                self.pending_keys.clear();
                self.status = SharedString::default();
                self.dispatch_recorded(&expr, cx);
            } else if self
                .keymap
                .is_prefix_evil(state, major.as_deref(), &sequence)
            {
                self.status = format!("{sequence}-").into();
                cx.notify();
            } else {
                self.pending_keys.clear();
                cx.notify();
            }
            cx.stop_propagation();
            return;
        }

        // Evil Insert: a chord bound in `evil-insert` (e.g. Escape → normal) wins;
        // otherwise fall through (plain keys self-insert, C-x etc. still work).
        if self.evil == EvilState::Insert
            && self.pending_keys.is_empty()
            && let Some(expr) = self
                .keymap
                .resolve_mode("evil-insert", &chord_string(ks))
                .map(str::to_string)
        {
            self.run_script(&expr, cx);
            cx.stop_propagation();
            return;
        }

        // Special read-only buffers (magit-style): every plain key is a command
        // resolved against the buffer's keymap — never self-insert. Ctrl/Alt
        // chords fall through to the normal dispatch (so C-x C-c, C-g, and any
        // ctrl-bound mode keys still work). Mirrors Evil Normal.
        if self.special.is_some()
            && !ks.modifiers.control
            && !ks.modifiers.alt
            && !ks.modifiers.platform
        {
            self.pending_keys.push(chord_string(ks));
            let sequence = self.pending_keys.join(" ");
            if let Some(expr) = self
                .keymap
                .resolve(&self.modes, &sequence)
                .map(str::to_string)
            {
                self.pending_keys.clear();
                self.status = SharedString::default();
                self.dispatch_recorded(&expr, cx);
            } else if self.keymap.is_prefix(&self.modes, &sequence) {
                self.status = format!("{sequence}-").into();
                cx.notify();
            } else {
                self.pending_keys.clear();
                cx.notify();
            }
            cx.stop_propagation();
            return;
        }

        // Emacs universal-argument: `C-u` and `M-<digit>` begin a numeric prefix
        // that the next command consumes as a repeat count. While the argument
        // is being read, digits (and further `C-u`) are swallowed here; any
        // other key ends the argument and falls through to run as the command.
        if self.handle_prefix_arg(ks, cx) {
            cx.stop_propagation();
            return;
        }

        let is_text = ks.key_char.is_some()
            && !ks.modifiers.control
            && !ks.modifiers.alt
            && !ks.modifiers.platform;

        // Platform (cmd) chords: dispatch when bound (e.g. cmd-c/x/v), otherwise
        // fall through to the OS WITHOUT consuming the event, so system shortcuts
        // (cmd-q, cmd-w, cmd-m, cmd-h, …) still work. No multi-key cmd sequences.
        if self.pending_keys.is_empty() && ks.modifiers.platform {
            let chord = chord_string(ks);
            if let Some(expr) = self.keymap.resolve(&self.modes, &chord).map(str::to_string) {
                self.dispatch_recorded(&expr, cx);
                cx.stop_propagation();
            }
            return;
        }

        if self.pending_keys.is_empty() {
            if is_text {
                return; // self-insert via the IME
            }
            if !(ks.modifiers.control || ks.modifiers.alt) {
                return; // OS-special key; handled as a GPUI action
            }
        }

        self.pending_keys.push(chord_string(ks));
        let sequence = self.pending_keys.join(" ");

        if let Some(expr) = self
            .keymap
            .resolve(&self.modes, &sequence)
            .map(str::to_string)
        {
            self.pending_keys.clear();
            self.status = SharedString::default(); // clear any prefix echo
            self.dispatch_recorded(&expr, cx);
        } else if self.keymap.is_prefix(&self.modes, &sequence) {
            self.status = format!("{sequence}-").into();
            cx.notify();
        } else {
            self.status = format!("{sequence} is undefined").into();
            self.pending_keys.clear();
            cx.notify();
        }
        cx.stop_propagation();
    }

    // --- Mouse ------------------------------------------------------------

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A click inside the completion popup picks that candidate (rather than
        // dismissing the popup and moving the cursor).
        if let Some(hit) = self.completion_popup_hit
            && hit.bounds.contains(&event.position)
        {
            let row = ((event.position.y - hit.bounds.origin.y) / hit.row_height) as usize;
            if row < hit.rows {
                self.completion_click(hit.start + row, cx);
                return;
            }
        }
        self.completion_popup = None;
        self.completion_popup_hit = None;
        // A click on a gutter fold chevron toggles that line's fold. Policy lives
        // in Scheme: we just route the line to `fold-toggle-at-line`. Checked
        // before cursor positioning so the click doesn't also move point.
        if let Some(line) = self
            .fold_hitboxes
            .iter()
            .find(|h| h.bounds.contains(&event.position))
            .map(|h| h.line)
        {
            self.run_script(&format!("(fold-toggle-at-line {line})"), cx);
            return;
        }
        // A click in a terminal pane starts a grid text selection (and focuses it).
        if self.terminal_selection_begin(event.position, cx) {
            return;
        }
        // A click in another pane focuses it (Emacs: mouse selects the window);
        // for a normal buffer the cursor is positioned by a *subsequent* click.
        // Special buffers (treemacs/dired/…) act on the first click, so fall
        // through to position the cursor and run their `mouse-1` binding below.
        if let Some(pane) = self.pane_at_position(event.position)
            && pane != self.selected_pane
        {
            self.select_pane(pane, cx);
            if self.special.is_none() {
                return;
            }
        }
        self.is_selecting = true;
        self.mark_active = false;
        // In a special (read-only) buffer, a plain left click acts like a key:
        // position the cursor on the clicked line, then resolve a `mouse-1`
        // binding against the mode keymap and run it (treemacs-visit, etc.). The
        // line is mapped from pane geometry rather than `self.lines`, so a click
        // that *also* focuses the pane (cursor was in another buffer) still lands
        // on the right line this frame instead of needing a second click.
        if self.special.is_some() && !event.modifiers.shift {
            // Normally map the click through the painted line layout (handles
            // wrapped lines exactly). On the frame a click *also* focuses this
            // pane, `self.lines` was just cleared and isn't repainted yet, so fall
            // back to mapping straight from pane geometry.
            let offset = if self.lines.is_empty() {
                // `self.line_height` is the real height from the last paint;
                // `window.line_height()` in an event handler is the unstyled
                // default and would scale every row off (clicks land one line up).
                self.line_start_for_click_y(event.position.y, self.line_height)
            } else {
                Some(self.offset_for_position(event.position, window.line_height()))
            };
            if let Some(offset) = offset {
                self.move_to(offset, cx);
            }
            if let Some(expr) = self
                .keymap
                .resolve(&self.modes, "mouse-1")
                .map(str::to_string)
            {
                self.dispatch_recorded(&expr, cx);
            }
            return;
        }
        let offset = self.offset_for_position(event.position, window.line_height());
        if event.modifiers.shift {
            self.select_to(offset, cx);
        } else {
            self.move_to(offset, cx);
        }
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Dragging a split divider: set the split's ratio from the pointer's
        // position within the split's bounds (union of the panes under it).
        if let Some(drag) = self.resizing.clone() {
            let mut leaves = Vec::new();
            if let Some(node) = node_at(&self.layout, &drag.path) {
                layout_leaves(node, &mut leaves);
            }
            let rects: Vec<Bounds<Pixels>> = leaves
                .iter()
                .filter_map(|id| {
                    self.pane_bounds
                        .iter()
                        .find(|(p, _)| p == id)
                        .map(|(_, b)| *b)
                })
                .collect();
            if let Some(area) = union_bounds(&rects) {
                let pos = event.position;
                let ratio = if drag.vertical {
                    (f32::from(pos.y) - f32::from(area.top()))
                        / f32::from(area.size.height).max(1.0)
                } else {
                    (f32::from(pos.x) - f32::from(area.left()))
                        / f32::from(area.size.width).max(1.0)
                };
                if set_ratio_at(&mut self.layout, &drag.path, ratio) {
                    cx.notify();
                }
            }
            return;
        }
        if self.terminal_drag.is_some() {
            self.terminal_selection_drag(event.position, cx);
            return;
        }
        if self.is_selecting {
            let offset = self.offset_for_position(event.position, window.line_height());
            self.select_to(offset, cx);
            // If the pointer is near/past the top or bottom edge, keep scrolling
            // (and extending the selection) until it comes back inside or the
            // drag ends — driven by a self-rescheduling timer so it continues
            // even while the pointer is held still.
            self.autoscroll_pos = event.position;
            self.autoscroll_dy = self.drag_autoscroll_velocity(event.position);
            if self.autoscroll_dy != 0.0 && !self.autoscrolling {
                self.autoscrolling = true;
                self.schedule_drag_autoscroll(cx);
            }
            return;
        }
        // Hover: echo the diagnostic under the pointer (only over the selected
        // pane, so an adjacent pane's text doesn't show the wrong message).
        let over_selected = self
            .pane_at_position(event.position)
            .is_none_or(|p| p == self.selected_pane);
        let message = if over_selected {
            let offset = self.offset_for_position(event.position, window.line_height());
            self.diagnostic_at_offset(offset)
        } else {
            None
        };
        match message {
            Some(m) => {
                // Only re-render when the hovered message actually changes.
                let changed = self
                    .hover_message
                    .as_ref()
                    .is_none_or(|s| s.as_ref() != m.as_str());
                if changed {
                    self.status = m.clone().into();
                    self.hover_message = Some(m.into());
                    cx.notify();
                }
            }
            None => {
                if let Some(prev) = self.hover_message.take() {
                    if self.status == prev {
                        self.status = SharedString::default();
                    }
                    cx.notify();
                }
            }
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
        // The autoscroll timer chain checks `is_selecting` and stops itself.
        self.autoscroll_dy = 0.0;
        self.resizing = None;
        // End any terminal drag; the selection stays so it can be copied.
        self.terminal_drag = None;
    }

    /// Per-tick vertical autoscroll velocity (signed pixels) for a drag-select,
    /// given the pointer position. Zero when the pointer is comfortably inside
    /// the text area; otherwise it ramps up the further past the edge the
    /// pointer is, so flicking well below the view scrolls faster than hugging
    /// the edge. Negative scrolls up, positive scrolls down.
    fn drag_autoscroll_velocity(&self, pos: Point<Pixels>) -> f32 {
        let Some(bounds) = self.last_bounds else {
            return 0.0;
        };
        let lh = f32::from(self.line_height).max(1.0);
        // Start scrolling once the pointer is within a line of the edge (so you
        // don't have to push fully past the window), ramping over ~4 lines.
        let margin = lh;
        let ramp = lh * 4.0;
        let min_speed = lh * 0.4;
        let max_speed = lh * 2.5;
        let speed = |over: f32| (min_speed + (over / ramp) * max_speed).min(max_speed);
        let y = f32::from(pos.y);
        let top = f32::from(bounds.top());
        let bottom = f32::from(bounds.bottom());
        if y > bottom - margin {
            speed(y - (bottom - margin))
        } else if y < top + margin {
            -speed((top + margin) - y)
        } else {
            0.0
        }
    }

    /// Schedule the next drag-autoscroll tick. Each tick re-checks that a drag
    /// is still active and the pointer is still off the edge; otherwise it stops
    /// the chain. Mirrors [`schedule_blink`]'s self-rescheduling timer.
    fn schedule_drag_autoscroll(&mut self, cx: &mut Context<Self>) {
        let timer = cx.background_executor().timer(Duration::from_millis(16));
        cx.spawn(async move |this, cx| {
            timer.await;
            this.update(cx, |editor, cx| {
                if !editor.is_selecting || editor.autoscroll_dy == 0.0 {
                    editor.autoscrolling = false;
                    return;
                }
                if editor.apply_drag_autoscroll(cx) {
                    editor.schedule_drag_autoscroll(cx);
                } else {
                    // Hit the top/bottom of the buffer — nothing left to reveal.
                    editor.autoscrolling = false;
                }
            })
            .ok();
        })
        .detach();
    }

    /// Apply one autoscroll step: scroll by the current velocity, then re-extend
    /// the selection to the (now newly revealed) line under the last pointer
    /// position. Returns `false` when the view can't scroll any further in that
    /// direction, so the caller can stop the timer chain.
    fn apply_drag_autoscroll(&mut self, cx: &mut Context<Self>) -> bool {
        let lh = f32::from(self.line_height).max(1.0);
        let vh = f32::from(self.viewport_height);
        let content = self.display_line_count() as f32 * lh;
        let max = (content - vh).max(0.0);
        let cur = f32::from(self.scroll_offset);
        let next = (cur + self.autoscroll_dy).clamp(0.0, max);
        if (next - cur).abs() < 0.01 {
            return false;
        }
        self.scroll_offset = px(next);
        // The pointer sits past the edge, so this hit-tests to the first/last
        // visible line; after the scroll + repaint that line has moved, so the
        // selection grows by roughly the scrolled distance each tick.
        let offset = self.offset_for_position(self.autoscroll_pos, px(lh));
        self.select_to(offset, cx);
        cx.notify();
        true
    }

    fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let line_height = window.line_height();
        let pixel_delta = event.delta.pixel_delta(line_height);
        // Over a terminal pane, the wheel scrolls the terminal's scrollback.
        if let Some(pane) = self.pane_at_position(event.position)
            && let Some(tid) = self.pane_terminal_id(pane)
        {
            let lines = (f32::from(pixel_delta.y) / f32::from(line_height)).round() as i32;
            if lines != 0 {
                self.terminal_scroll(tid, lines, cx);
            }
            return;
        }
        let lh = f32::from(line_height);
        let vh = f32::from(self.viewport_height);
        let content = self.buffer.line_count() as f32 * lh;
        let max = (content - vh).max(0.0);
        // A downward wheel delta is negative, which scrolls content up.
        let scroll = (f32::from(self.scroll_offset) - f32::from(pixel_delta.y)).clamp(0.0, max);
        self.scroll_offset = px(scroll);
        // Horizontal scrolling only when word-wrap is off (otherwise lines never
        // extend past the right edge). The clamp upper bound is generous — the
        // true line width isn't known here — and h-scroll is re-clamped to the
        // widest visible line during paint.
        if !self.word_wrap {
            let h = (f32::from(self.h_scroll_offset) - f32::from(pixel_delta.x)).max(0.0);
            self.h_scroll_offset = px(h);
        }
        cx.notify();
    }

    // --- GPUI action handlers (OS-special keys -> commands) ----------------

    fn act_backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        if self.transient.is_some() {
            // transient is modal; ignore editing keys
        } else if self.helm.is_some() {
            self.helm_backspace(cx);
        } else if self.minibuffer.is_some() {
            self.mb_backspace(cx);
        } else if self.terminal_action_key("backspace", cx) {
            // terminal: sent to the shell (DEL)
        } else if self.special_key("backspace", cx) {
            // read-only buffer: consumed (ran any binding)
        } else if self.evil_intercepts() {
            self.apply_command(EditorCommand::BackwardChar, cx); // vim: move left
        } else {
            self.apply_command(EditorCommand::DeleteBackwardChar, cx);
            if self.completion_popup.is_some() {
                self.popup_refilter(cx);
            }
        }
    }
    fn act_delete(&mut self, _: &Delete, _: &mut Window, cx: &mut Context<Self>) {
        if self.transient.is_some() {
            // transient is modal; ignore editing keys
        } else if self.minibuffer.is_some() {
            self.mb_delete(cx);
        } else if self.terminal_action_key("delete", cx) {
            // terminal: sent to the shell
        } else if self.special_key("delete", cx) {
            // read-only buffer: consumed
        } else {
            self.apply_command(EditorCommand::DeleteChar, cx);
        }
    }
    fn act_left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        self.completion_popup = None; // horizontal motion dismisses the popup
        if self.minibuffer.is_some() {
            self.mb_left(cx);
        } else if self.terminal_action_key("left", cx) {
            // terminal: arrow sent to the shell
        } else {
            self.apply_command(EditorCommand::BackwardChar, cx);
        }
    }
    fn act_right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        self.completion_popup = None; // horizontal motion dismisses the popup
        if self.minibuffer.is_some() {
            self.mb_right(cx);
        } else if self.terminal_action_key("right", cx) {
            // terminal: arrow sent to the shell
        } else {
            self.apply_command(EditorCommand::ForwardChar, cx);
        }
    }
    fn act_up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        if self.helm.is_some() {
            self.helm_previous(cx);
            return;
        }
        if self.popup_special_key("up", EditorCommand::CompletionPrevious, cx) {
            return;
        }
        if self.minibuffer.is_some() {
            self.mb_history(-1, cx); // older
        } else if self.terminal_action_key("up", cx) {
            // terminal: arrow sent to the shell (history)
        } else {
            self.apply_command(EditorCommand::PreviousLine, cx);
        }
    }
    fn act_down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        if self.helm.is_some() {
            self.helm_next(cx);
            return;
        }
        if self.popup_special_key("down", EditorCommand::CompletionNext, cx) {
            return;
        }
        if self.minibuffer.is_some() {
            self.mb_history(1, cx); // newer
        } else if self.terminal_action_key("down", cx) {
            // terminal: arrow sent to the shell (history)
        } else {
            self.apply_command(EditorCommand::NextLine, cx);
        }
    }
    fn act_home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        if self.minibuffer.is_some() {
            self.mb_move(0, cx);
        } else if self.terminal_action_key("home", cx) {
            // terminal: sent to the shell
        } else {
            self.apply_command(EditorCommand::BeginningOfLine, cx);
        }
    }
    fn act_end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(len) = self.minibuffer.as_ref().map(|mb| mb.input.len()) {
            self.mb_move(len, cx);
        } else if self.terminal_action_key("end", cx) {
            // terminal: sent to the shell
        } else {
            self.apply_command(EditorCommand::EndOfLine, cx);
        }
    }
    fn act_newline(&mut self, _: &Newline, _: &mut Window, cx: &mut Context<Self>) {
        if self.transient.is_some() {
            return; // transient is modal
        }
        if self.helm.is_some() {
            self.helm_accept(cx);
            return;
        }
        if self.popup_special_key("enter", EditorCommand::CompletionAccept, cx) {
            return;
        }
        if self.minibuffer.is_some() {
            self.mb_submit(cx);
        } else if self.terminal_action_key("enter", cx) {
            // terminal: RET sent to the shell (carriage return)
        } else if self.special_key("enter", cx) {
            // read-only buffer: RET runs its binding (e.g. visit/expand)
        } else if !self.evil_intercepts() && !self.mode_key("enter", cx) {
            // a mode may rebind RET (comint send-input); else insert a newline
            self.apply_command(EditorCommand::Newline, cx);
        }
    }
    fn act_tab(&mut self, _: &Tab, _: &mut Window, cx: &mut Context<Self>) {
        if self.transient.is_some() {
            return; // transient is modal
        }
        if self.helm.is_some() {
            self.helm_complete(cx); // TAB → complete to selection / descend folders
            return;
        }
        if self.popup_special_key("tab", EditorCommand::CompletionAccept, cx) {
            return;
        }
        if self.minibuffer.is_some() {
            self.mb_complete(cx);
        } else if self.terminal_action_key("tab", cx) {
            // terminal: TAB sent to the shell (completion)
        } else if self.special_key("tab", cx) {
            // read-only buffer: TAB runs its binding (e.g. toggle section)
        } else if !self.evil_intercepts() && !self.mode_key("tab", cx) {
            self.apply_command(EditorCommand::IndentForTab, cx);
        }
    }
    fn act_select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        if self.minibuffer.is_none() {
            self.apply_command(EditorCommand::ExtendBackwardChar, cx);
        }
    }
    fn act_select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        if self.minibuffer.is_none() {
            self.apply_command(EditorCommand::ExtendForwardChar, cx);
        }
    }
    fn act_select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        if self.minibuffer.is_none() {
            self.apply_command(EditorCommand::MarkWholeBuffer, cx);
        }
    }
}

/// The grapheme boundary immediately before `cursor` in `s`.
fn prev_grapheme(s: &str, cursor: usize) -> usize {
    s[..cursor]
        .grapheme_indices(true)
        .next_back()
        .map_or(0, |(i, _)| i)
}

/// The grapheme boundary immediately after `cursor` in `s`.
fn next_grapheme(s: &str, cursor: usize) -> usize {
    s[cursor..]
        .graphemes(true)
        .next()
        .map_or(cursor, |g| cursor + g.len())
}

/// Translate Vim-style backreferences (`\1`..`\9`, `\0`) in a substitution
/// replacement to the regex crate's `${N}` form. Other `\x` escapes pass through.
fn translate_backrefs(replacement: &str) -> String {
    let mut out = String::with_capacity(replacement.len());
    let mut chars = replacement.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some(d) if d.is_ascii_digit() => {
                    out.push_str("${");
                    out.push(*d);
                    out.push('}');
                    chars.next();
                }
                Some(_) | None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Expand a leading `~/` to the home directory.
fn expand_path(input: &str) -> String {
    if let Some(rest) = input.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return format!("{home}/{rest}");
    }
    input.to_string()
}

/// Decode an image file into a GPUI [`RenderImage`] (RGBA → BGRA, the byte order
/// the renderer's atlas expects). Returns `None` if the file is missing or not a
/// decodable image. Runs on a background thread (it can be slow for big files).
fn load_inline_image(path: &Path) -> Option<CachedImage> {
    let bytes = std::fs::read(path).ok()?;
    let decoded = image::load_from_memory(&bytes).ok()?;
    let mut rgba = decoded.into_rgba8();
    let (width, height) = rgba.dimensions();
    // GPUI's sprite atlas samples BGRA; swap the red/blue channels in place.
    for px in rgba.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    let frame = image::Frame::new(rgba);
    let data = Arc::new(RenderImage::new(vec![frame]));
    Some(CachedImage {
        data,
        width,
        height,
    })
}

/// The highlighting language for a file, by extension.
fn language_for_path(path: &Path) -> Option<&'static str> {
    let ext = path.extension().and_then(|e| e.to_str())?;
    let language = match ext {
        "scm" | "ss" | "sld" | "sls" | "rkt" | "steel" => "scheme",
        "rs" => "rust",
        "py" | "pyi" => "python",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "ts" => "typescript",
        "tsx" => "tsx",
        "go" => "go",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
        "java" => "java",
        "rb" => "ruby",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "html" | "htm" => "html",
        "css" => "css",
        "sh" | "bash" | "zsh" => "bash",
        "md" | "markdown" => "markdown",
        "lua" => "lua",
        "zig" => "zig",
        "r" | "R" => "r",
        "ml" | "mli" => "ocaml",
        "hs" => "haskell",
        "tex" | "sty" | "cls" => "latex",
        "typ" => "typst",
        "mmd" | "mermaid" => "mermaid",
        _ => return None,
    };
    Some(language)
}

/// Build a fresh document for `contents`, choosing modes/highlighter by file type.
/// Append `item` to a capped ring buffer, dropping the oldest entries on
/// overflow (used by the `*Messages*` and `*lsp-log*` rings).
fn push_capped(ring: &mut Vec<String>, item: String, cap: usize) {
    ring.push(item);
    if ring.len() > cap {
        ring.drain(0..ring.len() - cap);
    }
}

/// A blank document with the shared default editing state (empty selection, a
/// fresh undo tree, no overlays/locals/faces). The `make_*` constructors layer
/// the buffer contents, mode stack, and any special/hooks flags on top via
/// struct-update syntax, so the ~15 invariant fields live in one place.
fn blank_document(
    name: String,
    path: Option<PathBuf>,
    contents: &str,
    modes: ModeStack,
) -> Document {
    Document {
        name,
        path,
        buffer: Buffer::new(contents),
        selected_range: 0..0,
        selection_reversed: false,
        marked_range: None,
        mark_active: false,
        undo_tree: new_undo_tree(),
        undo_current: 0,
        scroll_offset: px(0.),
        modes,
        highlighter: None,
        modified: false,
        hooks_ran: false,
        special: None,
        line_faces: HashMap::new(),
        line_icons: HashMap::new(),
        line_images: HashMap::new(),
        overlays: Overlays::default(),
        locals: HashMap::new(),
    }
}

fn make_document(name: String, path: Option<PathBuf>, contents: &str) -> Document {
    let language = path.as_deref().and_then(language_for_path);
    let mut modes = ModeStack::default();
    modes.push(Mode::new("fundamental"));
    let highlighter = match language {
        // Scheme keeps the `steel` major mode (keymaps, hooks, built-in LSP).
        // Config / extension Scheme (under the config dir, or `*.macros.scm`)
        // instead gets the editor-aware `macros-steel` mode and `macros-scheme`
        // server — completion/hover/diagnostics over the editor API.
        Some("scheme") => {
            if path
                .as_deref()
                .is_some_and(crate::runtime::is_macros_scheme_path)
            {
                modes.push(Mode::new("macros-steel").with_language("macros-scheme"));
            } else {
                modes.push(Mode::new("steel").with_language("scheme"));
            }
            Highlighter::scheme()
        }
        // Other languages: a major mode named after the language, with its
        // bundled tree-sitter grammar (if any) for highlighting.
        Some(lang) => {
            modes.push(Mode::new(lang).with_language(lang));
            Highlighter::for_language(lang)
        }
        None => None,
    };
    Document {
        highlighter,
        ..blank_document(name, path, contents, modes)
    }
}

/// A normal (editable) buffer for logs like `*Messages*` — `special: None` so
/// it can be edited/copied, `messages-mode` (no language) so no LSP/highlighter
/// runs on it, `hooks_ran` so it never tries to start a server.
fn make_log_document(name: String, contents: &str) -> Document {
    let mut modes = ModeStack::default();
    modes.push(Mode::new("fundamental"));
    modes.push(Mode::new("messages-mode"));
    Document {
        hooks_ran: true,
        ..blank_document(name, None, contents, modes)
    }
}

/// A read-only program-generated buffer (the magit pattern): no file, no
/// highlighter, major mode `mode` stacked on `fundamental` so its keymap
/// resolves. Hooks are marked run (these buffers don't take mode hooks).
fn make_special_document(name: String, mode: &str, contents: &str) -> Document {
    let mut modes = ModeStack::default();
    modes.push(Mode::new("fundamental"));
    modes.push(Mode::new(mode));
    Document {
        hooks_ran: true,
        special: Some(mode.to_string()),
        ..blank_document(name, None, contents, modes)
    }
}

/// Filesystem completions for a (possibly partial) path: entries in the
/// directory part whose names start with the filename part, as full paths.
fn file_completions(input: &str) -> Vec<String> {
    let expanded = expand_path(input);
    let (dir, prefix) = if expanded.ends_with('/') {
        (PathBuf::from(&expanded), String::new())
    } else {
        let path = Path::new(&expanded);
        let dir = path
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        let prefix = path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        (dir, prefix)
    };
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(&prefix) {
                let mut full = dir.join(&name).to_string_lossy().into_owned();
                if entry.path().is_dir() {
                    full.push('/');
                }
                out.push(full);
            }
        }
    }
    out.sort();
    out
}

/// Render completion candidates for the echo area, capped so a long list
/// doesn't overflow the line.
fn format_candidates(candidates: &[String]) -> String {
    const MAX_SHOWN: usize = 12;
    let shown = candidates.len().min(MAX_SHOWN);
    let mut out = format!(
        "{} matches: {}",
        candidates.len(),
        candidates[..shown].join("  ")
    );
    if candidates.len() > shown {
        out.push_str("  …");
    }
    out
}

/// The trailing Scheme symbol token of `s`, as a `(start, token)` pair where
/// `token == &s[start..]`. The token runs back to the previous delimiter —
/// whitespace or one of `()[]{}'`\``,";` — so completing it leaves the rest of
/// the expression (open parens, earlier args) untouched.
fn scheme_symbol_token(s: &str) -> (usize, &str) {
    let is_delim = |c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '(' | ')' | '[' | ']' | '{' | '}' | '\'' | '`' | ',' | '"' | ';'
            )
    };
    let start = s
        .char_indices()
        .rev()
        .find(|(_, c)| is_delim(*c))
        .map_or(0, |(i, c)| i + c.len_utf8());
    (start, &s[start..])
}

/// Longest common byte prefix of `items`, trimmed to a char boundary.
fn longest_common_prefix(items: &[String]) -> String {
    let Some(first) = items.first() else {
        return String::new();
    };
    let mut end = first.len();
    for item in &items[1..] {
        let common = first
            .bytes()
            .zip(item.bytes())
            .take_while(|(a, b)| a == b)
            .count();
        end = end.min(common);
    }
    while !first.is_char_boundary(end) {
        end -= 1;
    }
    first[..end].to_string()
}

/// The byte range of the last complete s-expression at the end of `text` (a
/// balanced `(...)` form, or a trailing atom). Used by `eval-last-sexp`. Strings
/// containing parens aren't accounted for — fine for a first cut.
fn last_sexp(text: &str) -> Option<Range<usize>> {
    let bytes = text.as_bytes();
    let mut end = text.len();
    while end > 0 && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    if end == 0 {
        return None;
    }

    if bytes[end - 1] == b')' {
        let mut depth = 0;
        let mut i = end;
        while i > 0 {
            i -= 1;
            match bytes[i] {
                b')' => depth += 1,
                b'(' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i..end);
                    }
                }
                _ => {}
            }
        }
        None
    } else {
        let mut start = end;
        while start > 0 {
            let c = bytes[start - 1];
            if c.is_ascii_whitespace() || c == b'(' || c == b')' {
                break;
            }
            start -= 1;
        }
        Some(start..end)
    }
}

/// The top-level parenthesized form containing `point` — the unit
/// `anvil-eval-defun` sends. Falls back to the last complete top-level form that
/// ends at or before `point`. Counts parens only (like [`last_sexp`]); good
/// enough for well-formed Lisp.
fn top_level_form_at(text: &str, point: usize) -> Option<Range<usize>> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut open = None;
    let mut last_before = None;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'(' => {
                if depth == 0 {
                    open = Some(i);
                }
                depth += 1;
            }
            b')' if depth > 0 => {
                depth -= 1;
                if depth == 0
                    && let Some(start) = open.take()
                {
                    let range = start..i + 1;
                    if start <= point && point <= i + 1 {
                        return Some(range);
                    }
                    if i < point {
                        last_before = Some(range);
                    } else {
                        // First form ends after point and nothing enclosed it —
                        // take the last complete form before point.
                        return last_before;
                    }
                }
            }
            _ => {}
        }
    }
    last_before
}

/// The symbol (identifier) under `point`, for `anvil-doc`. A run of non-delimiter
/// characters around point; `None` if point sits on whitespace or a delimiter.
fn symbol_at(text: &str, point: usize) -> Option<String> {
    let bytes = text.as_bytes();
    let is_sym = |c: u8| {
        !c.is_ascii_whitespace()
            && !matches!(
                c,
                b'(' | b')' | b'[' | b']' | b'{' | b'}' | b'"' | b';' | b'\'' | b'`' | b','
            )
    };
    let p = point.min(text.len());
    let mut start = p;
    while start > 0 && is_sym(bytes[start - 1]) {
        start -= 1;
    }
    let mut end = p;
    while end < bytes.len() && is_sym(bytes[end]) {
        end += 1;
    }
    (start < end).then(|| text[start..end].to_string())
}

/// Prettify a chord for display in a transient menu, Emacs-style: `shift-p` →
/// `P`, `ctrl-x` → `C-x`, `alt-a` → `M-a`. Matching still uses the raw chord.
fn pretty_key(key: &str) -> String {
    if let Some(rest) = key.strip_prefix("shift-")
        && rest.chars().count() == 1
    {
        return rest.to_uppercase();
    }
    key.replace("ctrl-", "C-")
        .replace("alt-", "M-")
        .replace("shift-", "S-")
}

/// Render a [`Keystroke`] as a chord string like `"ctrl-g"` or `"alt-shift-,"`.
/// Order is fixed (control, alt, shift, platform) so keymap entries can match.
fn chord_string(ks: &Keystroke) -> String {
    let mut s = String::new();
    if ks.modifiers.control {
        s.push_str("ctrl-");
    }
    if ks.modifiers.alt {
        s.push_str("alt-");
    }
    if ks.modifiers.shift {
        s.push_str("shift-");
    }
    if ks.modifiers.platform {
        s.push_str("cmd-");
    }
    s.push_str(&ks.key);
    s
}

/// The numeric value of a universal-argument from its parts: an explicit typed
/// number wins; otherwise each `C-u` multiplies the default by 4 (`C-u` = 4,
/// `C-u C-u` = 16, …); with neither, the count is 1.
fn prefix_arg_value(digits: Option<i64>, cu: u32) -> i64 {
    match digits {
        Some(n) => n,
        None if cu > 0 => 4i64.saturating_pow(cu),
        None => 1,
    }
}

/// Whether a command should repeat under a universal-argument (Emacs `C-u N`).
/// Limited to motions and simple edits, where "do it N times" is the meaning;
/// modal/meta commands (`M-x`, eval, quit, toggles, …) run once regardless.
fn is_repeatable(command: &EditorCommand) -> bool {
    use EditorCommand::*;
    matches!(
        command,
        ForwardChar
            | BackwardChar
            | NextLine
            | PreviousLine
            | ForwardWord
            | BackwardWord
            | ScrollUp
            | ScrollDown
            | EvilScrollUp
            | EvilScrollDown
            | DeleteChar
            | DeleteBackwardChar
            | KillWord
            | BackwardKillWord
            | KillLine
            | Newline
            | OpenLine
            | TransposeChars
            | Yank
            | Undo
            | Redo
    )
}

/// Encode a [`Keystroke`] as the bytes a PTY expects, for the embedded terminal.
/// Returns `None` for keystrokes that produce nothing (e.g. an unhandled control
/// chord), which are then swallowed. Meta (`alt`) is sent as an ESC prefix.
fn keystroke_to_bytes(ks: &Keystroke) -> Option<Vec<u8>> {
    let m = &ks.modifiers;
    let key = ks.key.as_str();

    // Named / navigation keys → control bytes or CSI sequences.
    let named: Option<Vec<u8>> = match key {
        "enter" | "return" => Some(vec![b'\r']),
        "escape" => Some(vec![0x1b]),
        "backspace" => Some(vec![0x7f]),
        "delete" => Some(b"\x1b[3~".to_vec()),
        "tab" => Some(if m.shift {
            b"\x1b[Z".to_vec()
        } else {
            vec![b'\t']
        }),
        "up" => Some(b"\x1b[A".to_vec()),
        "down" => Some(b"\x1b[B".to_vec()),
        "right" => Some(b"\x1b[C".to_vec()),
        "left" => Some(b"\x1b[D".to_vec()),
        "home" => Some(b"\x1b[H".to_vec()),
        "end" => Some(b"\x1b[F".to_vec()),
        "pageup" => Some(b"\x1b[5~".to_vec()),
        "pagedown" => Some(b"\x1b[6~".to_vec()),
        _ => None,
    };
    if let Some(named) = named {
        return Some(named);
    }

    // Control chords → C0 control bytes (C-a = 0x01, … C-/ = 0x1f).
    if m.control {
        let byte = match key {
            "space" | "@" => Some(0x00u8),
            k if k.len() == 1 => match k.as_bytes()[0] {
                c @ b'a'..=b'z' => Some(c - b'a' + 1),
                b'[' => Some(0x1b),
                b'\\' => Some(0x1c),
                b']' => Some(0x1d),
                b'^' => Some(0x1e),
                b'_' | b'/' => Some(0x1f),
                _ => None,
            },
            _ => None,
        };
        return byte.map(|b| if m.alt { vec![0x1b, b] } else { vec![b] });
    }

    // Printable text (shifted char from the IME, else a single-char key name).
    let text = ks
        .key_char
        .clone()
        .filter(|t| !t.is_empty())
        .or_else(|| (key.chars().count() == 1).then(|| key.to_string()))?;
    let mut bytes = text.into_bytes();
    if m.alt {
        let mut v = vec![0x1b];
        v.append(&mut bytes);
        return Some(v);
    }
    Some(bytes)
}

impl Focusable for Editor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Editor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Observe window focus once, so the cursor only blinks while the window
        // is active. Seed the current state here; the observer handles later
        // activate/deactivate changes.
        if self.blink_observer.is_none() {
            self.window_active = window.is_window_active();
            self.blink_observer =
                Some(cx.observe_window_activation(window, |editor, window, cx| {
                    editor.set_window_active(window.is_window_active(), cx);
                }));
        }
        let theme = cx.global::<Theme>();
        let background = theme.background();
        let foreground = theme.foreground();
        // Font size / line height are Scheme-configurable via `(set-option ...)`;
        // the custom element inherits them from `window.text_style()`.
        let settings = cx.global::<crate::settings::Settings>();
        let font_size = settings.font_size();
        let line_height = settings.line_height();
        let font_family = settings.font_family().to_string();
        div()
            .key_context("Editor")
            .track_focus(&self.focus_handle)
            .cursor(CursorStyle::IBeam)
            .size_full()
            .p(px(8.))
            .bg(background)
            .text_color(foreground)
            .font_family(font_family)
            .text_size(px(font_size))
            .line_height(px(line_height))
            .on_action(cx.listener(Self::act_backspace))
            .on_action(cx.listener(Self::act_delete))
            .on_action(cx.listener(Self::act_left))
            .on_action(cx.listener(Self::act_right))
            .on_action(cx.listener(Self::act_up))
            .on_action(cx.listener(Self::act_down))
            .on_action(cx.listener(Self::act_home))
            .on_action(cx.listener(Self::act_end))
            .on_action(cx.listener(Self::act_newline))
            .on_action(cx.listener(Self::act_tab))
            .on_action(cx.listener(Self::act_select_left))
            .on_action(cx.listener(Self::act_select_right))
            .on_action(cx.listener(Self::act_select_all))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            // `on_mouse_up` only fires when the release lands inside the editor's
            // hitbox (gpui gates it on `hitbox.is_hovered`). A drag that ends
            // past the text edge, over the mode line/another pane, or outside the
            // window would otherwise never clear `is_selecting`, so highlighting
            // would keep following the pointer after release. `on_mouse_up_out`
            // catches exactly those releases (macOS keeps delivering the up to
            // the capturing window even outside its bounds).
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            // Open files dragged onto the window from the OS. gpui turns an
            // external file drop into an `ExternalPaths` drag, so a plain
            // `on_drop` of that type fires once the user releases over us.
            .on_drop::<ExternalPaths>(cx.listener(Self::on_drop_files))
            .child(self.render_layout(&self.layout, &[], cx))
    }
}

impl EntityInputHandler for Editor {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = clamp_range(
            self.buffer.range_from_utf16(&range_utf16),
            self.buffer.len(),
        );
        actual_range.replace(self.buffer.range_to_utf16(&range));
        Some(self.buffer.slice_to_string(range))
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.buffer.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.buffer.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.helm.is_some() {
            self.helm_input(new_text, cx);
            return;
        }
        if self.minibuffer.is_some() {
            self.mb_insert(new_text, cx);
            return;
        }
        if self.evil_intercepts() {
            return; // Normal/Visual: plain keys are commands, not text
        }
        if self.transient.is_some() || self.special.is_some() {
            return; // modal transient / read-only special buffer: never insert
        }
        if !self.pending_keys.is_empty() {
            return; // mid key-sequence; ignore stray text the platform may emit
        }
        // universal-argument (Emacs `C-u N` then a literal char): insert it N
        // times. Repeat up front so a recorded keyboard macro replays the full
        // text and electric-pair / completion see the final string.
        let repeated;
        let new_text = match self.prefix_arg.take() {
            Some(n) if n > 1 => {
                self.status = SharedString::default();
                repeated = new_text.repeat(n as usize);
                repeated.as_str()
            }
            _ => new_text,
        };
        // While avy hints are showing, the next printable key picks a hint: hand
        // it to Scheme's `avy-pick` and consume it (never self-insert).
        if !self.avy_hints.is_empty() {
            self.avy_hints.clear();
            cx.notify();
            self.script.dispatch_action("avy-pick", Some(new_text));
            return;
        }
        // Record self-inserted text into an in-progress keyboard macro.
        if let Some(rec) = self.macro_recording.as_mut() {
            rec.push(MacroEvent::Text(new_text.to_string()));
        }

        let range = range_utf16
            .as_ref()
            .map(|r| self.buffer.range_from_utf16(r))
            .or_else(|| self.marked_range.clone())
            .unwrap_or_else(|| self.selected_range.clone());

        // multiple-cursors: replicate the typed text at every cursor.
        if self.has_extra_cursors() && range.is_empty() {
            self.multi_insert(new_text, cx);
            return;
        }

        // electric-pair-mode: handle a single typed delimiter specially.
        if self.electric_pair
            && new_text.chars().count() == 1
            && self.electric_pair_insert(&range, new_text, cx)
        {
            return;
        }

        let cursor = self.edit(range, new_text);
        self.collapse_to(cursor);
        self.ensure_cursor_visible();
        if self.completion_popup.is_some() {
            self.popup_refilter(cx);
        } else {
            self.maybe_auto_complete(new_text, cx);
        }
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.minibuffer.is_some() {
            // IME composition isn't tracked in the minibuffer; treat as a
            // direct insert of the committed text.
            self.mb_insert(new_text, cx);
            return;
        }
        let range = range_utf16
            .as_ref()
            .map(|r| self.buffer.range_from_utf16(r))
            .or_else(|| self.marked_range.clone())
            .unwrap_or_else(|| self.selected_range.clone());

        let start = range.start;
        self.edit(range, new_text);
        self.marked_range = (!new_text.is_empty()).then(|| start..start + new_text.len());
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|r| self.buffer.range_from_utf16(r))
            .map_or_else(
                || {
                    let cursor = start + new_text.len();
                    cursor..cursor
                },
                |r| start + r.start..start + r.end,
            );
        self.mark_active = false;
        self.ensure_cursor_visible();
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let range = self.buffer.range_from_utf16(&range_utf16);
        let row = self.line_index_at(range.start)?;
        let line = self.lines.get(row)?;
        let lh = window.line_height();
        let pos = line
            .wrapped
            .position_for_index(line.to_display(range.start - line.range.start), lh)
            .unwrap_or_default();
        let x = element_bounds.left() + pos.x - self.h_scroll_offset + line.indent;
        let top = element_bounds.top() + line.top + pos.y;
        Some(Bounds::from_corners(point(x, top), point(x, top + lh)))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let offset = self.offset_for_position(point, window.line_height());
        Some(self.buffer.offset_to_utf16(offset))
    }
}

/// Clamp a byte range to `[0, len]` and keep it well-ordered.
fn clamp_range(range: Range<usize>, len: usize) -> Range<usize> {
    let start = range.start.min(len);
    let end = range.end.min(len).max(start);
    start..end
}

#[cfg(test)]
mod tests {
    use super::{
        Buffer, EditorCommand, WindowLayout, file_completions, is_repeatable, language_for_path,
        last_sexp, layout_leaves, layout_remove, layout_resize, layout_split, line_span, node_at,
        prefix_arg_value, push_capped, scheme_symbol_token, set_ratio_at, shift_line_map,
        symbol_at, top_level_form_at, translate_backrefs, union_bounds,
    };
    use std::collections::HashMap;

    #[test]
    fn universal_argument_value() {
        // C-u alone defaults to 4; stacking C-u multiplies by 4 each time.
        assert_eq!(prefix_arg_value(None, 0), 1); // no argument
        assert_eq!(prefix_arg_value(None, 1), 4); // C-u
        assert_eq!(prefix_arg_value(None, 2), 16); // C-u C-u
        assert_eq!(prefix_arg_value(None, 3), 64); // C-u C-u C-u
        // An explicit typed number wins over the C-u default (C-u 10 == 10).
        assert_eq!(prefix_arg_value(Some(10), 1), 10);
        assert_eq!(prefix_arg_value(Some(0), 0), 0);
    }

    #[test]
    fn prefix_arg_repeats_motions_and_edits_not_meta() {
        use EditorCommand::*;
        assert!(is_repeatable(&NextLine));
        assert!(is_repeatable(&ForwardChar));
        assert!(is_repeatable(&KillLine));
        assert!(is_repeatable(&Yank));
        // Meta/modal commands must run once regardless of the argument.
        assert!(!is_repeatable(&ExecuteExtendedCommand));
        assert!(!is_repeatable(&Quit));
        assert!(!is_repeatable(&SaveBuffer));
    }

    #[test]
    fn scheme_symbol_token_extracts_trailing_symbol() {
        // Bare symbol: whole string is the token.
        assert_eq!(scheme_symbol_token("set-o"), (0, "set-o"));
        // After an open paren: the paren is the delimiter, token starts after it.
        assert_eq!(scheme_symbol_token("(set-o"), (1, "set-o"));
        // Nested call with earlier args: only the trailing token completes.
        assert_eq!(scheme_symbol_token("(foo (bar ba"), (10, "ba"));
        // Just after a delimiter (paren or space) → empty token, nothing to complete.
        assert_eq!(scheme_symbol_token("(foo "), (5, ""));
        assert_eq!(scheme_symbol_token("("), (1, ""));
        // A string literal opens a fresh delimiter boundary.
        assert_eq!(scheme_symbol_token("(load-theme \"mac"), (13, "mac"));
    }

    #[test]
    fn shift_line_map_tracks_inserted_and_deleted_lines() {
        let base: HashMap<usize, usize> = [(0, 10), (3, 20), (5, 30)].into_iter().collect();
        // Insert 2 lines after line 3: keys <=3 stay, keys >3 shift by +2.
        let mut ins = base.clone();
        shift_line_map(&mut ins, 3, 2);
        assert_eq!(ins.get(&0), Some(&10));
        assert_eq!(ins.get(&3), Some(&20));
        assert_eq!(ins.get(&7), Some(&30)); // 5 -> 7
        assert_eq!(ins.get(&5), None);
        // Delete 2 lines after line 0: key 3 -> 1, key 5 -> 3.
        let mut del = base.clone();
        shift_line_map(&mut del, 0, -2);
        assert_eq!(del.get(&0), Some(&10));
        assert_eq!(del.get(&1), Some(&20));
        assert_eq!(del.get(&3), Some(&30));
        // A key that would collapse into the deleted region is dropped.
        let mut collapse = base.clone();
        shift_line_map(&mut collapse, 0, -4);
        assert_eq!(collapse.get(&0), Some(&10)); // 0 stays
        assert!(!collapse.values().any(|&v| v == 20)); // 3 -> -1 -> dropped
    }

    #[test]
    fn parse_snippet_extracts_fields_in_tab_order() {
        use super::parse_snippet;
        let (text, fields) = parse_snippet("fn ${1:name}($2) {\n    $0\n}");
        assert_eq!(text, "fn name() {\n    \n}");
        // $1 at "name" offset (len 4), $2 right after "(", $0 last.
        assert_eq!(fields, vec![(1, 3, 4), (2, 8, 0), (0, 16, 0)]);
        // A bare "$" with no field is literal.
        let (t2, f2) = parse_snippet("cost is $5 today");
        // "$5" parses as field 5 (no text), leaving the rest literal.
        assert_eq!(t2, "cost is  today");
        assert_eq!(f2, vec![(5, 8, 0)]);
    }

    #[test]
    fn line_span_maps_line_ranges_to_bytes() {
        // "a\nbb\nccc\n" -> lines: "a"(0..1) "bb"(2..4) "ccc"(5..8) ""(9)
        let buf = Buffer::new("a\nbb\nccc\n");
        assert_eq!(line_span(&buf, 0, 1), (0, 2)); // line 0 incl. its '\n'
        assert_eq!(line_span(&buf, 1, 2), (2, 5)); // line 1 incl. its '\n'
        assert_eq!(line_span(&buf, 0, 3), (0, 9)); // lines 0..3 = up to last '\n'
        assert_eq!(line_span(&buf, 1, 1), (2, 2)); // empty range (insert point)
        // End past the last line clamps to end-of-buffer.
        assert_eq!(line_span(&buf, 2, 99), (5, 9));
    }

    #[test]
    fn push_capped_drops_oldest_over_cap() {
        let mut ring = Vec::new();
        for i in 0..5 {
            push_capped(&mut ring, format!("m{i}"), 3);
        }
        // Keeps the newest `cap`, oldest dropped, order preserved.
        assert_eq!(
            ring,
            vec!["m2".to_string(), "m3".to_string(), "m4".to_string()]
        );
    }
    use gpui::{Bounds, point, px};
    use std::path::Path;

    #[test]
    fn transient_keys_prettified() {
        use super::pretty_key;
        assert_eq!(pretty_key("shift-p"), "P");
        assert_eq!(pretty_key("shift-x"), "X");
        assert_eq!(pretty_key("ctrl-x"), "C-x");
        assert_eq!(pretty_key("alt-a"), "M-a");
        assert_eq!(pretty_key("c"), "c");
        assert_eq!(pretty_key("tab"), "tab");
    }

    #[test]
    fn helm_fuzzy_ranks_and_filters() {
        use super::{HelmAction, HelmSource, helm_filter};
        let sources = vec![HelmSource {
            name: "Files".into(),
            candidates: vec![
                "foo/bar.rs".into(),
                "fxxxbxxxr.rs".into(),
                "unrelated.txt".into(),
            ],
            annotations: std::collections::HashMap::new(),
            dynamic: None,
            raw: false,
            action: HelmAction::ExecuteCommand,
            preview: None,
        }];

        // Empty pattern: everything, in original order.
        let all = helm_filter(&sources, "", true);
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].1, "foo/bar.rs");

        // Fuzzy "fbr": subsequence matches only; tighter match ranks first, and
        // the non-matching candidate is dropped.
        let fuzzy = helm_filter(&sources, "fbr", true);
        assert!(
            fuzzy.iter().all(|(_, c)| c != "unrelated.txt"),
            "non-match filtered"
        );
        assert_eq!(fuzzy[0].1, "foo/bar.rs", "the tighter match ranks first");

        // Substring mode: "bar" is a literal substring of only one candidate.
        let subs = helm_filter(&sources, "bar", false);
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].1, "foo/bar.rs");
    }

    #[test]
    fn helm_filter_shows_raw_sources_verbatim() {
        use super::{HelmAction, HelmSource, helm_filter};
        // A `raw` (self-filtered) source ignores the pattern entirely: its
        // candidates pass through unchanged and in order — the Scheme fn that
        // produced them is the sole filter authority.
        let sources = vec![HelmSource {
            name: "Scheme-filtered".into(),
            candidates: vec!["zebra".into(), "apple".into()],
            annotations: std::collections::HashMap::new(),
            dynamic: Some("my-filter".into()),
            raw: true,
            action: HelmAction::ExecuteCommand,
            preview: None,
        }];
        let out = helm_filter(&sources, "nomatch", true);
        assert_eq!(
            out.iter().map(|(_, c)| c.as_str()).collect::<Vec<_>>(),
            ["zebra", "apple"],
        );
    }

    #[test]
    fn helm_file_shadow_pattern_jumps_to_root_for_file_sources() {
        use super::{HelmAction, HelmSource, helm_file_shadow_pattern};
        let sources = vec![HelmSource {
            name: "Files".into(),
            candidates: Vec::new(),
            annotations: std::collections::HashMap::new(),
            dynamic: Some("find-file-candidates".into()),
            raw: false,
            action: HelmAction::ExecuteCommand,
            preview: None,
        }];

        assert_eq!(
            helm_file_shadow_pattern(&sources, "/Users/me/project//"),
            Some("/".into())
        );
        assert_eq!(
            helm_file_shadow_pattern(&sources, "/Users/me/project//etc"),
            Some("/etc".into())
        );
        assert_eq!(
            helm_file_shadow_pattern(&sources, "/Users//me//project"),
            Some("/project".into())
        );
        assert_eq!(
            helm_file_shadow_pattern(&sources, "/Users/me/project/"),
            None
        );
    }

    #[test]
    fn helm_file_shadow_pattern_ignores_generic_helm_sources() {
        use super::{HelmAction, HelmSource, helm_file_shadow_pattern};
        let sources = vec![HelmSource {
            name: "Commands".into(),
            candidates: Vec::new(),
            annotations: std::collections::HashMap::new(),
            dynamic: None,
            raw: false,
            action: HelmAction::ExecuteCommand,
            preview: None,
        }];

        assert_eq!(helm_file_shadow_pattern(&sources, "foo//bar"), None);
    }

    #[test]
    fn helm_file_accept_target_uses_typed_new_file_path() {
        use super::{HelmAction, HelmSource, helm_file_accept_target};
        let sources = vec![HelmSource {
            name: "Files".into(),
            candidates: vec!["/tmp/existing.rs".into()],
            annotations: std::collections::HashMap::new(),
            dynamic: Some("find-file-candidates".into()),
            raw: false,
            action: HelmAction::Scheme(vec![("open".into(), "open-file".into())]),
            preview: None,
        }];

        let Some((action, target)) = helm_file_accept_target(&sources, "/tmp/new-file.rs") else {
            panic!("new file path should be accepted");
        };
        assert_eq!(target, "/tmp/new-file.rs");
        assert!(matches!(action, HelmAction::Scheme(actions) if actions[0].1 == "open-file"));
    }

    #[test]
    fn helm_file_accept_target_keeps_existing_candidates_and_dirs_normal() {
        use super::{HelmAction, HelmSource, helm_file_accept_target};
        let sources = vec![HelmSource {
            name: "Files".into(),
            candidates: vec!["/tmp/existing.rs".into()],
            annotations: std::collections::HashMap::new(),
            dynamic: Some("find-file-candidates".into()),
            raw: false,
            action: HelmAction::Scheme(vec![("open".into(), "open-file".into())]),
            preview: None,
        }];

        assert_eq!(
            helm_file_accept_target(&sources, "/tmp/existing.rs").map(|(_, t)| t),
            None
        );
        assert_eq!(
            helm_file_accept_target(&sources, "/tmp/").map(|(_, t)| t),
            None
        );
    }

    #[test]
    fn window_layout_resize_nudges_nearest_matching_split() {
        // [0 | 1] horizontal split (ratio 0.5).
        let mut l = layout_split(WindowLayout::Leaf(0), 0, false, 1, 0.5);
        // Enlarge pane 0 horizontally: it's in `a`, so ratio grows.
        assert!(layout_resize(&mut l, 0, false, 0.1));
        if let WindowLayout::Split { ratio, .. } = &l {
            assert!((*ratio - 0.6).abs() < 1e-6, "ratio grew toward pane 0");
        } else {
            panic!("expected split");
        }
        // Enlarging pane 1 (in `b`) shrinks the ratio.
        assert!(layout_resize(&mut l, 1, false, 0.1));
        if let WindowLayout::Split { ratio, .. } = &l {
            assert!((*ratio - 0.5).abs() < 1e-6);
        }
        // A vertical resize finds no vertical split here.
        assert!(!layout_resize(&mut l, 0, true, 0.1));
    }

    #[test]
    fn window_layout_split_leaves_and_remove() {
        // One pane.
        let l = WindowLayout::Leaf(0);
        let mut leaves = Vec::new();
        layout_leaves(&l, &mut leaves);
        assert_eq!(leaves, vec![0]);

        // Split 0 below into [0, 1], then split 1 right into [0, [1, 2]].
        let l = layout_split(l, 0, true, 1, 0.5);
        let l = layout_split(l, 1, false, 2, 0.5);
        let mut leaves = Vec::new();
        layout_leaves(&l, &mut leaves);
        assert_eq!(
            leaves,
            vec![0, 1, 2],
            "in-order traversal is the cycle order"
        );

        // Remove pane 1 — its split collapses to sibling 2.
        let l = layout_remove(l, 1).expect("not the whole layout");
        let mut leaves = Vec::new();
        layout_leaves(&l, &mut leaves);
        assert_eq!(leaves, vec![0, 2]);

        // Removing the last leaf yields None.
        let single = WindowLayout::Leaf(7);
        assert!(layout_remove(single, 7).is_none());
    }

    #[test]
    fn layout_split_ratio_and_path_helpers() {
        // A narrow split: the existing pane keeps 0.22.
        let l = layout_split(WindowLayout::Leaf(0), 0, false, 1, 0.22);
        if let WindowLayout::Split { ratio, .. } = &l {
            assert!((*ratio - 0.22).abs() < 1e-6);
        } else {
            panic!("expected split");
        }
        // Nest: [0, [1, 2]] (split 1 below). The inner split is at path [true].
        let mut l = layout_split(l, 1, true, 2, 0.5);
        // node_at: root is a split; [true] is the inner split; [false] is leaf 0.
        assert!(matches!(node_at(&l, &[]), Some(WindowLayout::Split { .. })));
        assert!(matches!(node_at(&l, &[false]), Some(WindowLayout::Leaf(0))));
        assert!(matches!(
            node_at(&l, &[true]),
            Some(WindowLayout::Split { .. })
        ));
        // set_ratio_at the inner split; clamps to [0.1, 0.9].
        assert!(set_ratio_at(&mut l, &[true], 2.0));
        if let Some(WindowLayout::Split { ratio, .. }) = node_at(&l, &[true]) {
            assert!((*ratio - 0.9).abs() < 1e-6, "clamped to 0.9");
        } else {
            panic!("expected inner split");
        }
        // A path into a leaf isn't a split.
        assert!(!set_ratio_at(&mut l, &[false], 0.5));
    }

    #[test]
    fn union_bounds_covers_all_rects() {
        let a = Bounds::from_corners(point(px(0.), px(0.)), point(px(10.), px(10.)));
        let b = Bounds::from_corners(point(px(20.), px(5.)), point(px(30.), px(40.)));
        let u = union_bounds(&[a, b]).unwrap();
        assert_eq!(u.left(), px(0.));
        assert_eq!(u.top(), px(0.));
        assert_eq!(u.right(), px(30.));
        assert_eq!(u.bottom(), px(40.));
        assert!(union_bounds(&[]).is_none());
    }

    #[test]
    fn backref_translation_for_substitute() {
        assert_eq!(translate_backrefs(r"\1-\2"), "${1}-${2}");
        assert_eq!(translate_backrefs("plain"), "plain");
        // Non-digit escapes pass through unchanged.
        assert_eq!(translate_backrefs(r"a\tb"), r"a\tb");
    }

    #[test]
    fn language_by_extension() {
        assert_eq!(language_for_path(Path::new("main.scm")), Some("scheme"));
        assert_eq!(language_for_path(Path::new("a.rkt")), Some("scheme"));
        assert_eq!(language_for_path(Path::new("paper.tex")), Some("latex"));
        assert_eq!(language_for_path(Path::new("doc.typ")), Some("typst"));
        assert_eq!(language_for_path(Path::new("flow.mmd")), Some("mermaid"));
        assert_eq!(language_for_path(Path::new("notes.txt")), None);
        assert_eq!(language_for_path(Path::new("Makefile")), None);
    }

    #[test]
    fn file_completions_list_directory_entries() {
        // Run from the crate root; complete the bundled font directory.
        let matches = file_completions("assets/fonts/Lilex");
        assert!(
            matches.iter().any(|m| m.ends_with("Lilex-Regular.ttf")),
            "expected Lilex-Regular.ttf in {matches:?}"
        );
        // Every candidate extends the typed prefix.
        assert!(matches.iter().all(|m| m.starts_with("assets/fonts/Lilex")));
    }

    #[test]
    fn last_sexp_finds_trailing_form() {
        // Balanced form before point.
        assert_eq!(
            last_sexp("(foo 1)\n(bar 2)").map(|r| &"(foo 1)\n(bar 2)"[r]),
            Some("(bar 2)")
        );
        // Trailing whitespace is skipped.
        assert_eq!(last_sexp("(a)\n  ").map(|r| &"(a)\n  "[r]), Some("(a)"));
        // A bare atom.
        assert_eq!(last_sexp("foo bar").map(|r| &"foo bar"[r]), Some("bar"));
        // Nested parens balance correctly.
        assert_eq!(
            last_sexp("(+ (* 2 3) 4)").map(|r| &"(+ (* 2 3) 4)"[r]),
            Some("(+ (* 2 3) 4)")
        );
        assert_eq!(last_sexp("   "), None);
    }

    #[test]
    fn top_level_form_at_finds_enclosing_defun() {
        let src = "(define a 1)\n(define (f x)\n  (+ x 1))\n(define b 2)";
        // Point inside the middle multi-line defun returns the whole defun.
        let inside = src.find("+ x").unwrap();
        assert_eq!(
            top_level_form_at(src, inside).map(|r| &src[r]),
            Some("(define (f x)\n  (+ x 1))")
        );
        // Point on the first form.
        assert_eq!(
            top_level_form_at(src, 2).map(|r| &src[r]),
            Some("(define a 1)")
        );
        // Point in the gap after a form falls back to the last form before it.
        let gap = src.find("\n(define b").unwrap();
        assert_eq!(
            top_level_form_at(src, gap).map(|r| &src[r]),
            Some("(define (f x)\n  (+ x 1))")
        );
    }

    #[test]
    fn symbol_at_reads_identifier_under_point() {
        let src = "(foo-bar! baz)";
        assert_eq!(symbol_at(src, 3).as_deref(), Some("foo-bar!")); // inside foo-bar!
        assert_eq!(symbol_at(src, 11).as_deref(), Some("baz"));
        assert_eq!(symbol_at(src, 0), None); // on '('
        // Point right after a symbol still picks it up (point at end of word).
        assert_eq!(symbol_at("(a b)", 2).as_deref(), Some("a"));
        // A space flanked by non-symbol chars yields nothing.
        assert_eq!(symbol_at("a  b", 2), None);
    }
}
