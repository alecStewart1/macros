use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use gpui::{
    App, Bounds, Context, CursorStyle, EntityInputHandler, ExternalPaths, FocusHandle, Focusable,
    Keystroke, MouseButton, Pixels, Point, RenderImage, SharedString, Task, UTF16Selection, Window,
    actions, div, point, prelude::*, px,
};
use notify_debouncer_mini::notify::RecommendedWatcher;
use notify_debouncer_mini::{DebounceEventResult, Debouncer, new_debouncer};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};
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

mod buffers;
mod commands;
mod completion;
mod editing_extras;
mod evil;
mod helm;
mod input;
mod isearch;
mod kill_ring;
mod lsp_methods;
mod minibuffer;
mod overlays;
mod panes;
mod terminal;
mod undo;

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
    /// First-trust confirmation for the grammar install at the front of
    /// `pending_grammar_installs`. `yes` records trust + installs; either way the
    /// next queued install (if any) is then prompted.
    ConfirmGrammarInstall,
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
    /// Working-tree-vs-HEAD gutter marks by line index (git-gutter / diff-hl),
    /// recomputed by `magit-gutter-refresh`. Mirrors `line_faces`.
    diff_marks: HashMap<usize, DiffMark>,
}

/// A working-tree-vs-HEAD change marker for one buffer line — the git-gutter /
/// diff-hl feature. Computed by [`crate::script::git_diff_marks`] and painted in
/// the line-number gutter's far-left edge.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DiffMark {
    /// New since HEAD — a full-height bar (`magit-gutter-added`).
    Added,
    /// Content changed since HEAD — a full-height bar (`magit-gutter-modified`).
    Modified,
    /// One or more lines were deleted just above this one — a small wedge at the
    /// line's top edge (`magit-gutter-removed`). Deletions own no line of their
    /// own, so the mark rides the line that now follows the removed text.
    Removed,
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

/// One field of a mode-line segment — either inert text or a live value the
/// renderer pulls from current editor state every frame (the threading forbids
/// calling Scheme per frame, so volatile values like the modified marker and the
/// mode stack are resolved here, in Rust, not pushed from Scheme).
#[derive(Clone)]
pub enum MlField {
    /// Inert text (separators, parens, labels Scheme bakes into the format).
    Literal(String),
    /// `**` when the buffer is modified, `--` when saved.
    Modified,
    /// The active buffer's name.
    BufferName,
    /// The active mode names, space-joined.
    Modes,
    /// The evil (modal) state name — "Normal"/"Insert"/…, "" when evil is off.
    EvilMode,
    /// Ready language-server names, space-joined.
    Lsp,
    /// The LSP diagnostic summary ("E:n W:n"), "" when clean.
    Diagnostics,
    /// A named register written from Scheme via `set-mode-line-slot`.
    Slot(String),
}

/// One mode-line segment: a [`MlField`] plus optional styling. `prefix`/`suffix`
/// wrap the resolved value and are emitted ONLY when the value is non-empty, so
/// an absent optional field takes its separators with it. `face` names a theme
/// face for the whole span, or `None` for the default mode-line color.
#[derive(Clone)]
pub struct MlSeg {
    pub field: MlField,
    pub face: Option<SharedString>,
    pub prefix: String,
    pub suffix: String,
}

impl MlSeg {
    fn new(field: MlField, face: Option<&str>, prefix: &str, suffix: &str) -> Self {
        MlSeg {
            field,
            face: face.map(|f| SharedString::from(f.to_string())),
            prefix: prefix.to_string(),
            suffix: suffix.to_string(),
        }
    }
}

/// The built-in mode-line layout, used before `scheme/modeline.scm` installs its
/// own (and whenever no config does). Mirrors that file's default so behavior is
/// identical with or without Scheme: `-- name (modes) [project] git:branch lsp … E:n W:n`.
fn default_mode_line_format() -> Vec<MlSeg> {
    use MlField::*;
    vec![
        MlSeg::new(Modified, None, "", ""),
        MlSeg::new(Literal("  ".into()), None, "", ""),
        MlSeg::new(BufferName, None, "", ""),
        MlSeg::new(Literal("  (".into()), None, "", ""),
        MlSeg::new(Modes, None, "", ""),
        MlSeg::new(Literal(")".into()), None, "", ""),
        MlSeg::new(Slot("project".into()), None, "  [", "]"),
        MlSeg::new(Slot("git".into()), Some("magit-branch"), "  git:", ""),
        MlSeg::new(Lsp, None, "  lsp ", ""),
        MlSeg::new(Diagnostics, None, "  ", ""),
    ]
}

/// Parse the `set-mode-line-format` payload into renderable segments. Each token
/// is either a bare string (a literal with the default face) or a tab-joined
/// `kind\tpayload\tface\tprefix\tsuffix` produced by modeline.scm's `ml-*`
/// helpers. An unknown `field` name renders as a visible `?name` marker so a
/// typo in config is obvious rather than silent.
fn parse_mode_line_format(tokens: Vec<String>) -> Vec<MlSeg> {
    tokens
        .into_iter()
        .map(|token| {
            if !token.contains('\t') {
                return MlSeg::new(MlField::Literal(token), None, "", "");
            }
            let mut parts = token.splitn(5, '\t');
            let kind = parts.next().unwrap_or("");
            let payload = parts.next().unwrap_or("");
            let face = parts.next().unwrap_or("");
            let prefix = parts.next().unwrap_or("");
            let suffix = parts.next().unwrap_or("");
            let field = match kind {
                "field" => match payload {
                    "modified" => MlField::Modified,
                    "buffer-name" => MlField::BufferName,
                    "modes" => MlField::Modes,
                    "evil-mode" => MlField::EvilMode,
                    "lsp" => MlField::Lsp,
                    "diagnostics" => MlField::Diagnostics,
                    other => MlField::Literal(format!("?{other}")),
                },
                "slot" => MlField::Slot(payload.to_string()),
                _ => MlField::Literal(payload.to_string()),
            };
            MlSeg::new(field, (!face.is_empty()).then_some(face), prefix, suffix)
        })
        .collect()
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
    /// Anchor marker for each secondary selection, paired by index with
    /// [`Self::extra_cursors`] (the head). For a bare multiple-cursor the anchor
    /// sits on the head (empty selection, no highlight); Helix's selection-first
    /// model sets them apart so each secondary selection paints its own span.
    extra_anchors: Vec<u64>,
    /// When true the modal layer resolves plain keys against the `helix-*`
    /// keymaps instead of the `evil-*` ones (Helix's selection-first bindings,
    /// installed by scheme/helix.scm). The Normal/Insert state machine itself is
    /// shared with evil; only the keymap names change.
    modal_helix: bool,
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
    /// Set whenever point moves and the view should follow it (`ensure_cursor_visible`),
    /// consumed by the post-paint `reconcile_cursor_scroll`. Gating the reconcile on
    /// this flag means it only corrects the scroll right after a cursor move — it does
    /// NOT fight a passive repaint, so a mouse-wheel scroll away from the cursor sticks
    /// instead of being yanked back to the (unmoved) cursor.
    cursor_follow_pending: bool,
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

    /// Grammar installs from untrusted repos awaiting first-trust confirmation
    /// (see `install_grammar`). A queue, not a single slot, so several
    /// `install-language` calls in one `init.scm` each get prompted in turn
    /// rather than clobbering one another. The front is the one being prompted.
    pending_grammar_installs: VecDeque<crate::grammar::InstallSpec>,

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
    /// Vim `r` replace-pending: armed by `evil-replace-char`, it makes the very
    /// next printable keystroke overwrite the character under the cursor instead
    /// of being dispatched as a command. Cleared by that keystroke or `Escape`.
    evil_replace_pending: bool,
    /// True until startup config (bundled scripts + `init.scm`) has been applied.
    /// While set, plain keystrokes are swallowed so they can't self-insert before
    /// `(evil-mode)` runs — the window can be revealed by the load watchdog before
    /// config finishes, and a stray `j`/`k` in that gap must not land as text.
    config_pending: bool,
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
    /// Working-tree-vs-HEAD gutter marks for the active buffer, by line index
    /// (git-gutter / diff-hl). Mirrors `line_faces`; recomputed on save / open.
    diff_marks: HashMap<usize, DiffMark>,
    /// The open transient popup, if any (the magit / `transient.el` pattern).
    transient: Option<TransientState>,
    /// Named mode-line registers written from Scheme (`set-mode-line-slot`),
    /// e.g. "project" → "macros", "git" → "main". A `Slot` segment in
    /// `mode_line_format` looks up its value here; absent/empty = renders nothing.
    mode_line_slots: HashMap<String, SharedString>,
    /// The ordered mode-line segments, the Emacs `mode-line-format` analogue.
    /// Defined in Scheme (`scheme/modeline.scm`, via `set-mode-line-format`);
    /// seeded with [`default_mode_line_format`] so the bar works pre-Scheme.
    mode_line_format: Vec<MlSeg>,

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
    /// Cancel flags for in-flight HTTP streams, by id. `http-stream-cancel` flips
    /// the flag; the stream's reader thread checks it between lines and stops.
    http_streams: HashMap<u64, std::sync::Arc<std::sync::atomic::AtomicBool>>,
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

    // --- Filesystem watching (auto-revert + dired-sidebar/dired live refresh) ---
    /// The debounced OS watcher. Its background thread forwards each coalesced
    /// batch onto `fs_task`; dropping it stops all watches. `None` if the platform
    /// watcher failed to initialise (the feature then silently no-ops).
    fs_debouncer: Option<Debouncer<RecommendedWatcher>>,
    /// Keeps the FS-event drain task alive.
    fs_task: Option<Task<()>>,
    /// Paths currently registered with the watcher (visiting files + watched
    /// dirs), so `reconcile_watches` adds/removes only the delta.
    fs_watched: HashSet<PathBuf>,
    /// Directories to watch on behalf of each Scheme owner (`dired-sidebar`, `dired`).
    /// Replace-per-owner via `(watch-directories owner dirs)`.
    fs_watch_owners: HashMap<String, Vec<PathBuf>>,
    /// Flattened union of `fs_watch_owners`' values, for O(1) "is this changed
    /// path inside a watched dir?" membership tests in `on_fs_events`.
    fs_watch_dirs: HashSet<PathBuf>,
    /// Recursive watches (whole subtree), keyed by Scheme owner — for magit
    /// watching a repo root. Reconciled with `RecursiveMode::Recursive`.
    fs_watch_tree_owners: HashMap<String, Vec<PathBuf>>,
    /// Flattened recursive roots, for descendant matching in `on_fs_events`.
    fs_watch_trees: HashSet<PathBuf>,
    /// Paths currently watched recursively (delta tracking, mirrors `fs_watched`).
    fs_watched_recursive: HashSet<PathBuf>,
    /// While `render-buffer` refreshes a special buffer that's shown in a
    /// *non-selected* pane (the dired-sidebar/dired view during an auto-refresh), the
    /// index of that inactive [`Document`] — so the icon/overlay commands in the
    /// same batch target it instead of the active buffer, and focus never moves.
    /// Reset to `None` after each applied command batch.
    render_redirect: Option<usize>,
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
    /// Vim line-wise Visual (`V`): the selection is always rounded out to whole
    /// lines (see [`Editor::visual_region`]).
    VisualLine,
    /// Vim block-wise Visual (`C-v`): a rectangular selection between the two
    /// corners `selected_range.start`/`.end` (see [`Editor::visual_block_rect`]).
    VisualBlock,
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

        // *scratch* defaults to Org mode (see org.scm), so the greeting uses Org
        // comment syntax (`#`) rather than Steel's `;;`.
        let welcome = "# Macros — the Emacs you know, with a modern engine\n\
                       # This buffer is for notes you don't want to save, in Org mode.\n\
                       # Movement: C-f/C-b/C-n/C-p, C-a/C-e, M-f/M-b\n\
                       # Kill/yank: C-k, C-w, M-w, C-y; mark: C-Space; undo: C-/\n\n";

        let (script, mut script_outcomes) = ScriptEngine::new();
        let (anvil, mut anvil_outcomes) = AnvilEngine::new();
        let (term_events_tx, mut term_events_rx) =
            futures::channel::mpsc::unbounded::<(u64, TermEvent)>();
        let (lsp_flush_tx, mut lsp_flush_rx) = futures::channel::mpsc::unbounded::<()>();

        // Filesystem watcher: the debouncer runs its own thread and hands us a
        // coalesced batch of changed paths, which we forward (deduped) onto the
        // foreground drain task below. Watches are added lazily by
        // `reconcile_watches`; if the platform watcher won't start we keep `None`
        // and the auto-revert / live-refresh features simply no-op.
        let (fs_tx, mut fs_rx) = futures::channel::mpsc::unbounded::<Vec<PathBuf>>();
        let fs_debouncer = new_debouncer(
            Duration::from_millis(400),
            move |res: DebounceEventResult| {
                if let Ok(events) = res {
                    let paths: Vec<PathBuf> = events.into_iter().map(|e| e.path).collect();
                    if !paths.is_empty() {
                        let _ = fs_tx.unbounded_send(paths);
                    }
                }
            },
        )
        .ok();

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
            extra_anchors: Vec::new(),
            modal_helix: false,
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
            cursor_follow_pending: false,
            h_scroll_offset: px(0.),
            viewport_width: px(0.),
            char_width: px(8.),
            word_wrap: true,
            lines: Vec::new(),
            last_bounds: None,
            line_height: px(0.),
            viewport_height: px(0.),
            highlighter,
            pending_grammar_installs: VecDeque::new(),
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
            evil_replace_pending: false,
            config_pending: true,
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
            diff_marks: HashMap::new(),
            transient: None,
            mode_line_slots: HashMap::new(),
            mode_line_format: default_mode_line_format(),
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
            http_streams: HashMap::new(),
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
            fs_debouncer,
            fs_task: None,
            fs_watched: HashSet::new(),
            fs_watch_owners: HashMap::new(),
            fs_watch_tree_owners: HashMap::new(),
            fs_watch_trees: HashSet::new(),
            fs_watched_recursive: HashSet::new(),
            fs_watch_dirs: HashSet::new(),
            render_redirect: None,
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
        // Drain debounced filesystem-change batches onto the foreground: revert
        // unmodified buffers whose file changed, and fire `directory-changed-hook`
        // for watched dirs (dired-sidebar / dired refresh).
        editor.fs_task = Some(cx.spawn(async move |this, cx| {
            while let Some(paths) = fs_rx.next().await {
                if this
                    .update(cx, |editor, cx| editor.on_fs_events(paths, cx))
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
                // `render-buffer` may have redirected icon/overlay writes to a
                // sidebar buffer for this batch; that scope ends here.
                self.render_redirect = None;
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
        // Point moved (or a command asked to reveal it): let the post-paint
        // `reconcile_cursor_scroll` correct the wrapped-geometry estimate this frame.
        self.cursor_follow_pending = true;
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
        // Only follow the cursor on the frame after it moved. On a passive repaint
        // (mouse-wheel scroll, fontify, resize) the flag is clear, so a scroll away
        // from the cursor sticks instead of being yanked back to it.
        if !std::mem::take(&mut self.cursor_follow_pending) {
            return;
        }
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

    fn offset_for_position(&self, position: Point<Pixels>) -> usize {
        let Some(bounds) = self.last_bounds else {
            return 0;
        };
        // The *real* line height from the last paint. NOT `window.line_height()`:
        // inside a mouse-event handler that returns the unstyled default, which
        // differs from the editor's font-driven height and would mis-compute the
        // wrapped-row index (`position.y / line_height`) — so a click on a soft-
        // wrapped line lands on the wrong visual row (cursor "jumps" elsewhere).
        let line_height = self.line_height;
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
        match self.evil {
            EvilState::Visual => {
                let at_eol = self.buffer.line_range_at(r.end).end == r.end;
                let end = if at_eol {
                    r.end
                } else {
                    self.buffer.next_grapheme(r.end)
                };
                r.start..end.max(r.start)
            }
            // Line-wise Visual (`V`): round the raw selection out to whole lines —
            // from the start of the first line through the start of the line
            // *after* the last, so the trailing newline is included and a
            // line-wise `d`/`y` takes the whole line. At EOF (no trailing
            // newline) stop at the buffer end.
            EvilState::VisualLine => {
                let (lo, hi) = (r.start.min(r.end), r.start.max(r.end));
                let start = self.buffer.line_range_at(lo).start;
                let last = self.buffer.line_range_at(hi);
                let end = if last.end < self.buffer.len() {
                    self.buffer.next_grapheme(last.end)
                } else {
                    last.end
                };
                start..end
            }
            _ => r,
        }
    }

    /// The rectangle of a block-wise Visual selection (`C-v`) as
    /// `(top_row, bottom_row, left_col, right_col)` in 0-based line indices and
    /// character columns — the bounding box of the two corners
    /// `selected_range.start`/`.end`. `None` unless in [`EvilState::VisualBlock`].
    /// `right_col` is the cursor's own column (inclusive, vim semantics).
    fn visual_block_rect(&self) -> Option<(usize, usize, usize, usize)> {
        if !matches!(self.evil, EvilState::VisualBlock) {
            return None;
        }
        let row_col = |off: usize| -> (usize, usize) {
            let row = self.buffer.line_index_at_byte(off);
            let line_start = self.buffer.line_range(row).start;
            let col = self.buffer.byte_to_char(off) - self.buffer.byte_to_char(line_start);
            (row, col)
        };
        let (ra, ca) = row_col(self.selected_range.start);
        let (rb, cb) = row_col(self.selected_range.end);
        Some((ra.min(rb), ra.max(rb), ca.min(cb), ca.max(cb)))
    }

    /// Block-selection rectangle for pane `id`, for the renderer (selected pane
    /// only; `None` otherwise or when not in block-wise Visual).
    pub fn pane_visual_block(&self, id: usize) -> Option<(usize, usize, usize, usize)> {
        (id == self.selected_pane)
            .then(|| self.visual_block_rect())
            .flatten()
    }

    // --- Command implementations ------------------------------------------

    fn delete_char_forward(&mut self, cx: &mut Context<Self>) {
        let range = self.region().unwrap_or_else(|| {
            let p = self.point();
            p..self.buffer.next_grapheme(p)
        });
        self.delete_range(range, cx);
    }

    /// Vim `r<char>`: overwrite the single grapheme under the cursor with `ch`,
    /// then keep the cursor on the replaced character (Normal mode unchanged).
    /// A no-op — vim's "beep" — when point sits on the line break or past the
    /// end of the buffer, since there is no character there to replace.
    fn evil_replace_char(&mut self, ch: &str, cx: &mut Context<Self>) {
        let p = self.point();
        let line = self.buffer.line_range_at(p);
        if p >= line.end {
            return;
        }
        let end = self.buffer.next_grapheme(p);
        self.replace_region(p, end, ch, cx);
        self.move_to(p, cx);
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

    fn newline(&mut self, cx: &mut Context<Self>) {
        if self.has_extra_cursors() {
            // Multi-cursor RET stays a plain split; per-cursor reindent would
            // need each cursor's own structural context.
            self.multi_insert("\n", cx);
            return;
        }
        if !cx.global::<crate::settings::Settings>().electric_indent() {
            self.insert_text("\n", cx);
            return;
        }
        let p = self.selected_range.start;

        // Lisp: dedicated paren-aligning indenter (column alignment, which the
        // step-based tree-sitter model can't express).
        if self
            .active_language()
            .as_deref()
            .is_some_and(is_lisp_language)
        {
            let tab = cx.global::<crate::settings::Settings>().tab_width();
            let text = self.buffer.slice_to_string(self.lisp_defun_start(p)..p);
            let indent = " ".repeat(lisp_indent_for(&text, tab));
            self.insert_text(&format!("\n{indent}"), cx);
            return;
        }

        // Tree-sitter structural indent, when the grammar carries an indent query.
        let tab = cx.global::<crate::settings::Settings>().tab_width();
        let target_row = self.buffer.byte_to_position(p).0 + 1;
        let anchor = self.prev_nonspace(p);
        if let Some(level) = self.tree_indent(anchor, p, target_row, Some(p)) {
            // Cursor sitting just before a closing bracket: open a body line and
            // push the closer down to its own (dedented) line.
            let next = self.buffer.slice_to_string(p..self.buffer.next_grapheme(p));
            if matches!(next.as_str(), "}" | ")" | "]")
                && let Some(body) = self.tree_indent(anchor, p, target_row, None)
                && body > level
            {
                let first = " ".repeat(body * tab);
                let second = " ".repeat(level * tab);
                self.edit(self.selected_range.clone(), &format!("\n{first}\n{second}"));
                self.collapse_to(p + 1 + first.len());
                self.ensure_cursor_visible();
                cx.notify();
                return;
            }
            self.insert_text(&format!("\n{}", " ".repeat(level * tab)), cx);
            return;
        }

        // Fallback (no indent query): copy the current line's leading whitespace
        // up to point, so splitting inside the indentation never adds extra.
        let line = self.buffer.line_range_at(p);
        let indent: String = self
            .buffer
            .slice_to_string(line.start..p)
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        self.insert_text(&format!("\n{indent}"), cx);
    }

    /// The structural indent depth (in `tab_width` steps) for a line, via the
    /// active buffer's tree-sitter indent query. `anchor` is the byte whose
    /// syntax the line follows (previous non-blank for a newline, the line's own
    /// first non-blank for reindent); `ref_byte` is the line's logical start.
    /// `None` when there is no highlighter or no indent query for the language.
    /// See [`crate::highlight::Highlighter::indent_level`].
    fn tree_indent(
        &self,
        anchor: usize,
        ref_byte: usize,
        target_row: usize,
        outdent_byte: Option<usize>,
    ) -> Option<usize> {
        self.highlighter.as_ref()?.indent_level(
            self.buffer.rope(),
            anchor,
            ref_byte,
            target_row,
            outdent_byte,
        )
    }

    /// Byte ranges to highlight for matching-delimiter display (Emacs
    /// `show-paren-mode`): the bracket at point (block-cursor style) or just
    /// before it, together with its match — each one grapheme wide. Empty when
    /// point isn't on a bracket, or the bracket has no match. Computed fresh per
    /// frame by the painter, so it always tracks the cursor.
    pub(super) fn show_paren_ranges(&self) -> Vec<Range<usize>> {
        let p = self.point();
        let at = |b: usize, e: usize| self.buffer.slice_to_string(b..e);
        // Prefer the bracket the cursor sits on (the char "after" point); else
        // the closer the cursor just moved past.
        let delim = if p < self.buffer.len()
            && matches!(
                at(p, self.buffer.next_grapheme(p)).as_str(),
                "(" | "[" | "{"
            ) {
            Some(p)
        } else {
            let prev = self.buffer.prev_grapheme(p);
            (prev < p && matches!(at(prev, p).as_str(), ")" | "]" | "}")).then_some(prev)
        };
        let Some(d) = delim else {
            return Vec::new();
        };
        match self.bracket_match(d) {
            Some(m) => vec![
                d..self.buffer.next_grapheme(d),
                m..self.buffer.next_grapheme(m),
            ],
            None => Vec::new(),
        }
    }

    /// The matching delimiter for the bracket at `byte` — structurally via the
    /// parse tree when the buffer has one, else a balanced text scan (plain-text
    /// buffers, or a grammar where the tree lookup misses).
    fn bracket_match(&self, byte: usize) -> Option<usize> {
        if let Some(m) = self
            .highlighter
            .as_ref()
            .and_then(|h| h.bracket_match(byte))
        {
            return Some(m);
        }
        self.scan_bracket_match(byte)
    }

    /// Balanced bracket scan over buffer text (counting only the delimiter's own
    /// kind), bounded so a keypress can't scan an enormous buffer. Not string- or
    /// comment-aware — that's what the tree-sitter path in [`Self::bracket_match`]
    /// is for; this is the no-grammar fallback.
    fn scan_bracket_match(&self, byte: usize) -> Option<usize> {
        let rope = self.buffer.rope();
        if byte >= rope.len_bytes() {
            return None;
        }
        let total = rope.len_chars();
        let mut ci = rope.byte_to_char(byte);
        let (open, close, forward) = match rope.char(ci) {
            '(' => ('(', ')', true),
            '[' => ('[', ']', true),
            '{' => ('{', '}', true),
            ')' => ('(', ')', false),
            ']' => ('[', ']', false),
            '}' => ('{', '}', false),
            _ => return None,
        };
        const LIMIT: usize = 200_000;
        let mut depth = 0i32;
        for _ in 0..LIMIT {
            let c = rope.char(ci);
            if c == open {
                depth += 1;
            } else if c == close {
                depth -= 1;
            }
            if depth == 0 {
                return Some(rope.char_to_byte(ci));
            }
            if forward {
                ci += 1;
                if ci >= total {
                    return None;
                }
            } else if ci == 0 {
                return None;
            } else {
                ci -= 1;
            }
        }
        None
    }

    /// Byte where the nearest non-whitespace grapheme before `from` begins, or 0
    /// if only whitespace precedes it. Anchors a fresh line's indentation to the
    /// previous line's syntax (e.g. a compound-statement header) rather than the
    /// blank position just past the newline.
    fn prev_nonspace(&self, from: usize) -> usize {
        let mut a = from;
        while a > 0 {
            let prev = self.buffer.prev_grapheme(a);
            if self
                .buffer
                .slice_to_string(prev..a)
                .chars()
                .all(char::is_whitespace)
            {
                a = prev;
            } else {
                return prev;
            }
        }
        0
    }

    /// Byte offset of the nearest enclosing top-level form at or before `pos` —
    /// the start of the last line whose first character is `(`. Falls back to
    /// the buffer start. This bounds the Lisp indent scan to one defun, the
    /// same heuristic Emacs `beginning-of-defun` uses for Lisp.
    fn lisp_defun_start(&self, pos: usize) -> usize {
        let mut i = self.buffer.line_index_at_byte(pos);
        loop {
            let range = self.buffer.line_range(i);
            if self.buffer.slice_to_string(range.clone()).starts_with('(') {
                return range.start;
            }
            if i == 0 {
                return 0;
            }
            i -= 1;
        }
    }

    /// Replace the current line's leading whitespace with `target` spaces,
    /// keeping point in the same place relative to the line's text (Emacs TAB
    /// reindent). A no-op move when the line is already correctly space-indented.
    fn reindent_current_line(&mut self, target: usize, cx: &mut Context<Self>) {
        let p = self.point();
        let line = self.buffer.line_range_at(p);
        let line_text = self.buffer.slice_to_string(line.clone());
        let cur: String = line_text
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        let cur_bytes = cur.len();
        let indent_end = line.start + cur_bytes;
        if target == cur.chars().count() && cur.bytes().all(|b| b == b' ') {
            if p < indent_end {
                self.move_to(indent_end, cx);
            }
            return;
        }
        self.edit(line.start..indent_end, &" ".repeat(target));
        let new_indent_end = line.start + target;
        let cursor = if p <= indent_end {
            new_indent_end
        } else {
            (p + target).saturating_sub(cur_bytes)
        };
        self.collapse_to(cursor);
        cx.notify();
    }

    /// Reindent the current line to its Lisp indentation (Emacs TAB in a Lisp
    /// buffer).
    fn lisp_reindent_line(&mut self, cx: &mut Context<Self>) {
        let tab = cx.global::<crate::settings::Settings>().tab_width();
        let line = self.buffer.line_range_at(self.point());
        let target = lisp_indent_for(
            &self
                .buffer
                .slice_to_string(self.lisp_defun_start(line.start)..line.start),
            tab,
        );
        self.reindent_current_line(target, cx);
    }

    fn open_line(&mut self, cx: &mut Context<Self>) {
        let start = self.selected_range.start;
        self.edit(self.selected_range.clone(), "\n");
        self.move_to(start, cx);
    }

    fn indent_for_tab(&mut self, cx: &mut Context<Self>) {
        let (electric, tab) = {
            let s = cx.global::<crate::settings::Settings>();
            (s.electric_indent(), s.tab_width())
        };
        if electric && !self.has_extra_cursors() && self.region().is_none() {
            // Lisp: paren-aligning reindent.
            if self
                .active_language()
                .as_deref()
                .is_some_and(is_lisp_language)
            {
                self.lisp_reindent_line(cx);
                return;
            }
            // Tree-sitter structural reindent of the current line.
            let line = self.buffer.line_range_at(self.point());
            let line_text = self.buffer.slice_to_string(line.clone());
            let lead_bytes: usize = line_text
                .chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .map(char::len_utf8)
                .sum();
            let ref_byte = if lead_bytes < line_text.len() {
                line.start + lead_bytes
            } else {
                line.start
            };
            let target_row = self.buffer.byte_to_position(line.start).0;
            // Reindent anchors on the line's own content (no scan back), so a
            // line that belongs outside an enclosing block isn't pulled into it.
            if let Some(level) = self.tree_indent(ref_byte, ref_byte, target_row, Some(ref_byte)) {
                self.reindent_current_line(level * tab, cx);
                return;
            }
        }
        self.insert_text(&" ".repeat(tab), cx);
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

    /// Recompute the active buffer's git-gutter (diff-hl) marks: its working-tree
    /// text (the live rope, so marks track unsaved edits) diffed against HEAD.
    /// Cleared when the gutter is disabled or the buffer isn't visiting a file.
    /// Only repaints when the marks actually changed, so it's cheap to call from
    /// frequent hooks (save / find-file). Driven by `magit-gutter-refresh`.
    fn refresh_git_gutter(&mut self, cx: &mut Context<Self>) {
        let enabled = cx.global::<crate::settings::Settings>().git_gutter();
        let new_marks: HashMap<usize, DiffMark> = if enabled {
            match self.active_path.clone() {
                Some(path) => {
                    let text = self.buffer.full_text();
                    crate::script::git_diff_marks(&path, &text)
                        .into_iter()
                        .collect()
                }
                None => HashMap::new(),
            }
        } else {
            HashMap::new()
        };
        if new_marks != self.diff_marks {
            self.diff_marks = new_marks;
            cx.notify();
        }
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

    /// Download, compile, and load a user tree-sitter grammar (M-x
    /// install-language → `(grammar-install …)`). The git clone + C compile run
    /// on the background executor so the UI stays live; only the result echo and
    /// the re-highlight touch the editor. On success the grammar is in the
    /// process-global registry, so any later file open of a matching extension
    /// highlights automatically (see `language_for_path`); here we additionally
    /// re-highlight the *current* buffer if it already visits such a file.
    /// Entry point for a grammar install. Installing runs arbitrary native code
    /// (the compiled parser + scanner are `dlopen`ed), so the first install from
    /// a given repo asks for confirmation in the minibuffer; approved sources are
    /// remembered (see [`crate::grammar::is_trusted`]) so a config-driven install
    /// on every launch doesn't re-prompt. Trusted → straight to the work.
    fn install_grammar(&mut self, spec: crate::grammar::InstallSpec, cx: &mut Context<Self>) {
        if crate::grammar::is_trusted(&spec.repo) {
            self.run_grammar_install(spec, cx);
            return;
        }
        // Untrusted source: queue it and, if nothing is currently being prompted,
        // start the prompt. Queuing (rather than a single slot) means several
        // untrusted `install-language` calls in one `init.scm` are each prompted
        // in turn instead of all but the last being silently dropped.
        let was_idle = self.pending_grammar_installs.is_empty();
        self.pending_grammar_installs.push_back(spec);
        if was_idle {
            self.prompt_next_grammar_install(cx);
        }
    }

    /// Open the first-trust confirmation for the install at the front of the
    /// queue. The answer is handled by `MinibufferPurpose::ConfirmGrammarInstall`,
    /// which pops the front and chains to the next queued install.
    fn prompt_next_grammar_install(&mut self, cx: &mut Context<Self>) {
        let Some(spec) = self.pending_grammar_installs.front() else {
            return;
        };
        let prompt = format!(
            "Install grammar {} from {} (downloads and runs native code)? (yes or no) ",
            spec.id, spec.repo
        );
        self.open_minibuffer(&prompt, MinibufferPurpose::ConfirmGrammarInstall, "", cx);
    }

    /// Resolve the front grammar-install confirmation with the user's answer,
    /// then prompt the next queued one. Called from the minibuffer submit path.
    pub(super) fn resolve_grammar_install_confirm(
        &mut self,
        approved: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(spec) = self.pending_grammar_installs.pop_front() {
            if approved {
                crate::grammar::trust(&spec.repo);
                self.run_grammar_install(spec, cx);
            } else {
                self.echo(format!("Skipped grammar {}", spec.id), cx);
            }
        }
        if !self.pending_grammar_installs.is_empty() {
            self.prompt_next_grammar_install(cx);
        }
    }

    /// Do the actual download + compile + load on the background executor, then
    /// re-highlight the current buffer and echo the result. Reached only after
    /// the source is trusted (directly, or via the confirm prompt).
    fn run_grammar_install(&mut self, spec: crate::grammar::InstallSpec, cx: &mut Context<Self>) {
        self.echo(
            format!("Installing grammar {} (clone + compile)…", spec.id),
            cx,
        );
        let task = cx
            .background_executor()
            .spawn(async move { crate::grammar::install(spec) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |editor, cx| match result {
                Ok(lang) => {
                    // Colorize the current buffer now if it visits a file the new
                    // grammar claims — otherwise it highlights on next open.
                    let matches = editor.active_path.as_deref().and_then(language_for_path)
                        == Some(lang.as_str());
                    if let Some(h) = matches.then(|| Highlighter::for_language(&lang)).flatten() {
                        editor.highlighter = Some(h);
                        editor.parse_full();
                    }
                    editor.echo(format!("Installed grammar {lang}."), cx);
                }
                Err(err) => editor.echo(format!("Grammar install failed: {err}"), cx),
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
        let selections = self.all_selections();
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
            path: self
                .active_path
                .as_deref()
                .map_or_else(String::new, |p| p.to_string_lossy().into_owned()),
            point,
            // O(1) copy-on-write clone of the rope; read bridges materialize
            // only the slice they need, off the UI thread.
            text: self.buffer.rope().clone(),
            region,
            selections,
            props,
            locals,
            diagnostics,
            windows: {
                let mut leaves = Vec::new();
                layout_leaves(&self.layout, &mut leaves);
                leaves
            },
            window_buffers: {
                let mut leaves = Vec::new();
                layout_leaves(&self.layout, &mut leaves);
                leaves
                    .into_iter()
                    .map(|id| {
                        // The selected pane shows the live active buffer; others
                        // show whatever their `pane.buffer` records.
                        let name = if id == self.selected_pane {
                            self.active_name.clone()
                        } else {
                            self.panes
                                .iter()
                                .find(|p| p.id == id)
                                .map_or_else(|| self.active_name.clone(), |p| p.buffer.clone())
                        };
                        (id, name)
                    })
                    .collect()
            },
            selected_window: self.selected_pane,
            lsp_ready: self.active_lsp_ready(),
            visible_lines: self.visible_line_range(),
            cursor_screen_row: self.cursor_screen_row(),
            evil_enabled: self.evil != EvilState::Disabled,
            helix_enabled: self.modal_helix,
        });
    }

    /// Publish a snapshot built from the inactive buffer at `idx` (its text, name,
    /// path, and buffer-locals) so a `call_action` run sees that buffer rather than
    /// the active one. Only the fields the Org fontifier reads are filled — point,
    /// region, windows, and the rest stay at their defaults, since `org--fontify`
    /// never reads them. Restore with [`publish_point`](Self::publish_point).
    fn publish_inactive_snapshot(&self, idx: usize) {
        let doc = &self.inactive[idx];
        self.script.set_snapshot(crate::script::EditorSnapshot {
            name: doc.name.clone(),
            path: doc
                .path
                .as_deref()
                .map_or_else(String::new, |p| p.to_string_lossy().into_owned()),
            // O(1) copy-on-write clone of the rope, as in `publish_point`.
            text: doc.buffer.rope().clone(),
            locals: doc
                .locals
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            ..Default::default()
        });
    }

    /// The cursor's row within the viewport, in display rows (folds collapsed) —
    /// the distance from the first visible row to the cursor's row. Paired with
    /// [`scroll_cursor_to_row`](Self::scroll_cursor_to_row) to keep a buffer
    /// visually stable across an in-place re-render.
    fn cursor_screen_row(&self) -> usize {
        let lh = f32::from(self.line_height).max(1.0);
        let first = (f32::from(self.scroll_offset) / lh).floor().max(0.0) as usize;
        let cursor = self.display_row_of_line(self.buffer.line_index_at_byte(self.point()));
        cursor.saturating_sub(first)
    }

    /// Scroll so the cursor's row sits `row` rows below the top of the viewport
    /// (clamped to the scrollable range), undoing the jump a re-render + cursor
    /// move would otherwise cause.
    fn scroll_cursor_to_row(&mut self, row: usize, cx: &mut Context<Self>) {
        let lh = f32::from(self.line_height);
        let vh = f32::from(self.viewport_height);
        if lh <= 0.0 {
            return;
        }
        let cursor = self.display_row_of_line(self.buffer.line_index_at_byte(self.point()));
        let first = cursor.saturating_sub(row);
        let max = (self.display_line_count() as f32 * lh - vh).max(0.0);
        self.scroll_offset = px((first as f32 * lh).clamp(0.0, max));
        cx.notify();
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

    /// Names of the LSP servers that are running and ready, for the mode-line
    /// `lsp …` indicator. Keyed by language id; sorted for a stable order since
    /// `servers` is a `HashMap`.
    pub fn active_lsp_servers(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .servers
            .iter()
            .filter(|(_, server)| server.ready)
            .map(|(key, _)| key.clone())
            .collect();
        names.sort();
        names
    }

    /// Resolve the mode-line format into rendered `(text, face)` spans, in
    /// order, dropping any segment whose value is empty (so optional fields and
    /// their separators vanish when there's nothing to show). The live fields
    /// (`modified`, `modes`, …) read current editor state, so the bar never goes
    /// stale between the sparse Scheme hooks; `face` is `None` for the default
    /// mode-line color and a face name for the renderer to resolve otherwise.
    pub fn mode_line_segments(&self) -> Vec<(SharedString, Option<SharedString>)> {
        self.mode_line_format
            .iter()
            .filter_map(|seg| {
                let value = match &seg.field {
                    MlField::Literal(s) => s.clone(),
                    MlField::Modified => if self.is_modified() { "**" } else { "--" }.to_string(),
                    MlField::BufferName => self.buffer_name().to_string(),
                    MlField::Modes => self.modes.names().join(" "),
                    MlField::EvilMode => self.evil_mode_name().unwrap_or("").to_string(),
                    MlField::Lsp => self.active_lsp_servers().join(" "),
                    MlField::Diagnostics => self.diagnostic_summary().unwrap_or_default(),
                    MlField::Slot(key) => self
                        .mode_line_slots
                        .get(key)
                        .map(|s| s.to_string())
                        .unwrap_or_default(),
                };
                if value.is_empty() {
                    return None;
                }
                let text = format!("{}{value}{}", seg.prefix, seg.suffix);
                Some((text.into(), seg.face.clone()))
            })
            .collect()
    }

    /// Write (or, on empty, clear) a named mode-line register.
    fn set_mode_line_slot(&mut self, key: String, value: String) {
        if value.is_empty() {
            self.mode_line_slots.remove(&key);
        } else {
            self.mode_line_slots.insert(key, value.into());
        }
    }

    // --- Overlays / text properties / folds (active buffer) ---------------

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
        // An explicitly-entered `macros-steel` mode (`M-x macros-steel-mode`) forces
        // the editor-aware `macros-scheme` server even for a plain `.scm` file whose
        // path wouldn't otherwise match — the escape hatch's whole purpose. Without
        // this the extension map below would resolve `.scm` to `scheme` and the
        // mode's declared language would be silently ignored.
        if self.modes.active_language() == Some("macros-scheme") {
            return Some("macros-scheme".to_string());
        }
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

    /// Mark startup config as fully applied, so plain keystrokes stop being
    /// swallowed (see `config_pending`). Called once the bundled scripts and the
    /// user's `init.scm` have been evaluated — including when that path errors,
    /// so a broken config can never wedge the keyboard.
    pub fn mark_config_loaded(&mut self, cx: &mut Context<Self>) {
        if self.config_pending {
            self.config_pending = false;
            cx.notify();
        }
    }

    /// Evaluate Steel synchronously and apply its commands in order. Used at
    /// startup, where config (keymaps, options, themes) must take effect before
    /// the editor is driven. Blocks the caller — fine before the UI is live.
    pub fn run_script_blocking(&mut self, source: &str, cx: &mut Context<Self>) {
        // Apply the commands accumulated before any error too (Emacs `init.el`
        // semantics) — a typo late in `init.scm` must not discard the
        // `(evil-mode)` / keymaps that ran before it.
        let (commands, error) = self.script.eval_collecting(source);
        for command in commands {
            self.apply_command(command, cx);
        }
        if let Some(err) = error {
            self.echo(format!("error: {err}"), cx);
        }
    }

    /// Evaluate Steel source interactively (steel-mode): dispatch to the engine
    /// thread and echo the result (handled in `apply_script_outcome`).
    fn eval_interactive(&mut self, source: &str, _cx: &mut Context<Self>) {
        self.script.dispatch(source, true);
    }

    // --- Key handling -----------------------------------------------------
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
        "scm" | "ss" | "sld" | "sls" | "steel" => "scheme",
        "rkt" => "racket",
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
        "scala" | "sc" | "sbt" => "scala",
        "clj" | "cljs" | "cljc" | "edn" => "clojure",
        "sql" => "sql",
        "ex" | "exs" => "elixir",
        "tex" | "sty" | "cls" => "latex",
        "typ" => "typst",
        "mmd" | "mermaid" => "mermaid",
        // Not bundled: a user may have installed a grammar claiming this
        // extension at runtime (M-x install-language). Check that registry so
        // opening the file highlights without a restart.
        _ => return crate::grammar::language_for_extension(ext),
    };
    Some(language)
}

/// Languages that get Lisp-style (paren-aware) auto-indentation rather than the
/// generic copy-previous-line-indent behaviour.
fn is_lisp_language(lang: &str) -> bool {
    matches!(
        lang,
        "scheme"
            | "macros-scheme"
            | "racket"
            | "lisp"
            | "commonlisp"
            | "common-lisp"
            | "clojure"
            | "clojurescript"
            | "elisp"
            | "emacs-lisp"
            | "fennel"
            | "janet"
    )
}

/// Whether a form headed by `head` indents its body by two columns (special
/// forms / macros: `define`, `let`, `when`, …) rather than aligning
/// continuation lines under the first argument (ordinary function calls).
fn lisp_form_indents_body(head: &str) -> bool {
    if head.starts_with("define") || head.starts_with("with-") {
        return true;
    }
    matches!(
        head,
        "lambda"
            | "λ"
            | "named-lambda"
            | "case-lambda"
            | "let"
            | "let*"
            | "letrec"
            | "letrec*"
            | "let-values"
            | "let*-values"
            | "let-syntax"
            | "letrec-syntax"
            | "parameterize"
            | "fluid-let"
            | "when"
            | "unless"
            | "while"
            | "until"
            | "cond"
            | "case"
            | "match"
            | "match-let"
            | "match-let*"
            | "begin"
            | "do"
            | "dynamic-wind"
            | "guard"
            | "syntax-rules"
            | "syntax-case"
            | "for"
            | "delay"
            | "unwind-protect"
            | "module"
            | "library"
    )
}

/// Target indentation column for a line whose preceding text — from the start
/// of the enclosing top-level form (a line beginning at column 0) up to the
/// insertion point — is `text`. A pragmatic Lisp indenter: continuation lines
/// align under the enclosing form's first argument, while the body of a known
/// special form indents `tab_width` columns past its opening paren (the same
/// `tab-width` option the tree-sitter languages use). Strings, line and block
/// comments, and char literals are skipped so their parens don't count.
fn lisp_indent_for(text: &str, tab_width: usize) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    // Open delimiters still unclosed at the end: (column, char-index just past
    // it, delimiter char).
    let mut stack: Vec<(usize, usize, char)> = Vec::new();
    let mut i = 0usize;
    let mut col = 0usize;
    while i < n {
        match chars[i] {
            '\n' => {
                col = 0;
                i += 1;
            }
            ';' => {
                while i < n && chars[i] != '\n' {
                    i += 1; // leave the '\n' to reset col on the next pass
                }
            }
            '"' => {
                i += 1;
                col += 1;
                while i < n {
                    match chars[i] {
                        '\\' => {
                            i += 1;
                            col += 1;
                            if i < n {
                                if chars[i] == '\n' {
                                    col = 0;
                                } else {
                                    col += 1;
                                }
                                i += 1;
                            }
                        }
                        '"' => {
                            i += 1;
                            col += 1;
                            break;
                        }
                        '\n' => {
                            col = 0;
                            i += 1;
                        }
                        _ => {
                            col += 1;
                            i += 1;
                        }
                    }
                }
            }
            '#' if i + 1 < n && chars[i + 1] == '\\' => {
                // Char literal `#\x` — the `#`, the `\`, and the literal char
                // are skipped (trailing letters of a named char like `#\space`
                // are harmless symbol chars that hold no parens).
                i += 2;
                col += 2;
                if i < n {
                    if chars[i] == '\n' {
                        col = 0;
                    } else {
                        col += 1;
                    }
                    i += 1;
                }
            }
            '#' if i + 1 < n && chars[i + 1] == '|' => {
                // Nestable block comment `#| ... |#`.
                i += 2;
                col += 2;
                let mut depth = 1usize;
                while i < n && depth > 0 {
                    if chars[i] == '#' && i + 1 < n && chars[i + 1] == '|' {
                        depth += 1;
                        i += 2;
                        col += 2;
                    } else if chars[i] == '|' && i + 1 < n && chars[i + 1] == '#' {
                        depth -= 1;
                        i += 2;
                        col += 2;
                    } else if chars[i] == '\n' {
                        col = 0;
                        i += 1;
                    } else {
                        col += 1;
                        i += 1;
                    }
                }
            }
            '(' | '[' | '{' => {
                stack.push((col, i + 1, chars[i]));
                i += 1;
                col += 1;
            }
            ')' | ']' | '}' => {
                stack.pop();
                i += 1;
                col += 1;
            }
            _ => {
                i += 1;
                col += 1;
            }
        }
    }

    let Some(&(open_col, after, open)) = stack.last() else {
        return 0; // top level
    };

    // Walk from just past the opener to the head token / first argument,
    // tracking columns. The char at `after` sits at column open_col + 1.
    let mut j = after;
    let mut c = open_col + 1;
    while j < n && (chars[j] == ' ' || chars[j] == '\t') {
        j += 1;
        c += 1;
    }
    // Nothing but a comment after the opener on its line → indent one past it.
    if lisp_line_end(&chars, j) {
        return open_col + 1;
    }
    // Data lists / vectors, or a list used as the head, align under the first
    // element rather than applying special-form rules.
    if open != '(' || matches!(chars[j], '(' | '[' | '{') {
        return c;
    }
    let head_start = j;
    while j < n
        && !chars[j].is_whitespace()
        && !matches!(chars[j], '(' | ')' | '[' | ']' | '{' | '}')
    {
        j += 1;
        c += 1;
    }
    if lisp_form_indents_body(&chars[head_start..j].iter().collect::<String>()) {
        return open_col + tab_width;
    }
    // Function call: align under the first argument when it shares the head's
    // line; otherwise one past the opener.
    while j < n && (chars[j] == ' ' || chars[j] == '\t') {
        j += 1;
        c += 1;
    }
    if lisp_line_end(&chars, j) {
        return open_col + 1;
    }
    c
}

/// Whether position `j` in `chars` is the end of meaningful content on its line:
/// end of input, a newline, or the start of a line (`;`) or block (`#|`)
/// comment. Used to decide there's no argument left to align under.
fn lisp_line_end(chars: &[char], j: usize) -> bool {
    j >= chars.len()
        || chars[j] == '\n'
        || chars[j] == ';'
        || (chars[j] == '#' && j + 1 < chars.len() && chars[j + 1] == '|')
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
        diff_marks: HashMap::new(),
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

/// Prettify a whole space-separated chord sequence (e.g. `"ctrl-x ctrl-t"` →
/// `"C-x C-t"`) for the pending-chord echo. Each chord is run through
/// [`pretty_key`] so the per-chord shift→uppercase rule still applies.
fn pretty_sequence(seq: &str) -> String {
    seq.split(' ').map(pretty_key).collect::<Vec<_>>().join(" ")
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
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let range = self.buffer.range_from_utf16(&range_utf16);
        let row = self.line_index_at(range.start)?;
        let line = self.lines.get(row)?;
        // Real painted height (see `offset_for_position`): `window.line_height()`
        // is the unstyled default here and would put the caret rect on the wrong
        // wrapped row (`position_for_index` lays rows out at `ix * line_height`).
        let lh = self.line_height;
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
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let offset = self.offset_for_position(point);
        Some(self.buffer.offset_to_utf16(offset))
    }
}

/// Byte offset within `line` of character column `col`, clamped to the line's
/// end (so a column past a short line lands at its end). Used by block-wise
/// Visual to map a rectangle's columns to per-line byte ranges.
fn char_col_byte(line: &str, col: usize) -> usize {
    line.char_indices()
        .nth(col)
        .map(|(b, _)| b)
        .unwrap_or(line.len())
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
        Buffer, EditorCommand, MlField, WindowLayout, char_col_byte, default_mode_line_format,
        file_completions, is_lisp_language, is_repeatable, language_for_path, last_sexp,
        layout_leaves, layout_remove, layout_resize, layout_split, line_span, lisp_indent_for,
        node_at, parse_mode_line_format, prefix_arg_value, push_capped, scheme_symbol_token,
        set_ratio_at, shift_line_map, symbol_at, top_level_form_at, translate_backrefs,
        union_bounds,
    };

    #[test]
    fn mode_line_format_parses_literals_fields_slots_and_styling() {
        // A bare string with no tab is an inert literal, default face.
        let segs = parse_mode_line_format(vec!["  (".into()]);
        assert_eq!(segs.len(), 1);
        assert!(matches!(&segs[0].field, MlField::Literal(s) if s == "  ("));
        assert!(segs[0].face.is_none());

        // A tab-encoded field with face/prefix/suffix.
        let segs = parse_mode_line_format(vec!["field\tlsp\tdim\t  lsp \t!".into()]);
        assert!(matches!(segs[0].field, MlField::Lsp));
        assert_eq!(segs[0].face.as_ref().map(|f| f.as_ref()), Some("dim"));
        assert_eq!(segs[0].prefix, "  lsp ");
        assert_eq!(segs[0].suffix, "!");

        // Empty face stays None; a slot carries its key.
        let segs = parse_mode_line_format(vec!["slot\tgit\t\t  git:\t".into()]);
        assert!(matches!(&segs[0].field, MlField::Slot(k) if k == "git"));
        assert!(segs[0].face.is_none());
        assert_eq!(segs[0].prefix, "  git:");

        // An unknown field name surfaces as a visible marker, not silence.
        let segs = parse_mode_line_format(vec!["field\tbogus\t\t\t".into()]);
        assert!(matches!(&segs[0].field, MlField::Literal(s) if s == "?bogus"));
    }

    #[test]
    fn default_mode_line_format_is_the_known_layout() {
        let segs = default_mode_line_format();
        // Leads with the modified marker and ends with the diagnostics field.
        assert!(matches!(segs.first().unwrap().field, MlField::Modified));
        assert!(matches!(segs.last().unwrap().field, MlField::Diagnostics));
        // The git slot is colored with the magit branch face.
        let git = segs
            .iter()
            .find(|s| matches!(&s.field, MlField::Slot(k) if k == "git"))
            .expect("git slot present");
        assert_eq!(git.face.as_ref().map(|f| f.as_ref()), Some("magit-branch"));
        assert_eq!(git.prefix, "  git:");
    }

    #[test]
    fn char_col_byte_maps_columns_and_clamps() {
        // ASCII: column == byte offset.
        assert_eq!(char_col_byte("hello", 0), 0);
        assert_eq!(char_col_byte("hello", 3), 3);
        // Past the end clamps to the line length (a block column off a short row).
        assert_eq!(char_col_byte("hi", 5), 2);
        assert_eq!(char_col_byte("", 0), 0);
        // Multi-byte chars: column 2 is the 'é' (2 bytes) + 1 ascii = byte 3.
        let s = "aéb"; // a=1, é=2, b=1 bytes
        assert_eq!(char_col_byte(s, 0), 0);
        assert_eq!(char_col_byte(s, 1), 1); // start of 'é'
        assert_eq!(char_col_byte(s, 2), 3); // start of 'b'
        assert_eq!(char_col_byte(s, 3), 4); // end
    }
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
    fn pretty_sequence_prettifies_each_chord() {
        use super::pretty_sequence;
        assert_eq!(pretty_sequence("ctrl-x ctrl-t"), "C-x C-t");
        assert_eq!(pretty_sequence("alt-g n"), "M-g n");
        assert_eq!(pretty_sequence("ctrl-c shift-p"), "C-c P");
        assert_eq!(pretty_sequence("ctrl-x"), "C-x");
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
        assert_eq!(language_for_path(Path::new("a.rkt")), Some("racket"));
        assert_eq!(language_for_path(Path::new("paper.tex")), Some("latex"));
        assert_eq!(language_for_path(Path::new("doc.typ")), Some("typst"));
        assert_eq!(language_for_path(Path::new("flow.mmd")), Some("mermaid"));
        assert_eq!(language_for_path(Path::new("query.sql")), Some("sql"));
        assert_eq!(language_for_path(Path::new("app.ex")), Some("elixir"));
        assert_eq!(language_for_path(Path::new("mix.exs")), Some("elixir"));
        assert_eq!(language_for_path(Path::new("notes.txt")), None);
        assert_eq!(language_for_path(Path::new("Makefile")), None);
    }

    #[test]
    fn lisp_indent_aligns_and_bodies() {
        // Body offset tracks `tab-width`; alignment cases are width-independent.
        // Here we pass the classic 2-column width.
        let ind = |s| lisp_indent_for(s, 2);
        // Top level: no enclosing form.
        assert_eq!(ind(""), 0);
        assert_eq!(ind("(define x 1)\n"), 0);
        // Function call: continuation aligns under the first argument.
        assert_eq!(ind("(foo bar\n"), 5);
        // Operator alone on its line → one past the opener.
        assert_eq!(ind("(foo\n"), 1);
        // Special form indents its body `tab-width` columns past the opener.
        assert_eq!(ind("(define (f x)\n"), 2);
        assert_eq!(ind("(let ((x 1))\n"), 2);
        assert_eq!(ind("(when ready\n"), 2);
        // Nesting: indent follows the innermost open form, offset by its column.
        assert_eq!(ind("(define (f x)\n  (when y\n"), 4);
        // A list used as the head aligns under it (let-binding lists).
        assert_eq!(ind("(let ((a 1)\n"), 6);
    }

    #[test]
    fn lisp_indent_body_follows_tab_width() {
        // The body offset is `tab-width`, configurable from init.scm. Alignment
        // under the first argument stays column-based regardless of the width.
        assert_eq!(lisp_indent_for("(define (f x)\n", 4), 4);
        assert_eq!(lisp_indent_for("(when ready\n", 8), 8);
        assert_eq!(lisp_indent_for("(define (f x)\n    (when y\n", 4), 8);
        // Function-call alignment is unaffected by the width setting.
        assert_eq!(lisp_indent_for("(foo bar\n", 4), 5);
        assert_eq!(lisp_indent_for("(foo bar\n", 2), 5);
    }

    #[test]
    fn lisp_indent_skips_strings_and_comments() {
        // None of these hit the body branch, so the width is irrelevant.
        let ind = |s| lisp_indent_for(s, 4);
        // A paren inside a string is not an open form.
        assert_eq!(ind("(foo \"(\"\n"), 5);
        // A paren inside a line comment is ignored.
        assert_eq!(ind("(foo ; (\n"), 1);
        // Char literal `#\(` is not an opener; aligns under the first arg `c`.
        assert_eq!(ind("(char=? c #\\(\n"), 8);
        // Block comment hides its parens.
        assert_eq!(ind("(foo #| ( |#\n"), 1);
    }

    #[test]
    fn lisp_language_detection() {
        assert!(is_lisp_language("scheme"));
        assert!(is_lisp_language("macros-scheme"));
        assert!(is_lisp_language("clojure"));
        assert!(!is_lisp_language("rust"));
        assert!(!is_lisp_language("python"));
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
