//! The grid's command vocabulary: every key binding in `scheme/grid.scm`
//! names one of these. They are registered as nullary builtins (see
//! `nullary_commands` in `src/script/registry.rs`), so a keypress applies
//! synchronously without a trip through the Scheme engine thread.

/// One grid command. Applied by `Editor::grid_command` against the active
/// grid buffer; a no-op (with a message) anywhere else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridCmd {
    // Motion
    Left,
    Right,
    Up,
    Down,
    NextCell,
    PreviousCell,
    EdgeLeft,
    EdgeRight,
    EdgeUp,
    EdgeDown,
    RowStart,
    RowFirstNonEmpty,
    RowEnd,
    GotoFirstRow,
    GotoLastRow,
    GotoColumn,
    GotoFirstCell,
    GotoLastCell,
    PageDown,
    PageUp,
    HalfPageDown,
    HalfPageUp,
    // Selection
    ExtendLeft,
    ExtendRight,
    ExtendUp,
    ExtendDown,
    SelectCells,
    SelectRows,
    SelectColumns,
    SelectAll,
    SelectCancel,
    // Editing
    EditStart,
    EditEnd,
    EditReplace,
    EditPopup,
    Clear,
    Cut,
    Yank,
    Paste,
    DeleteRow,
    DeleteColumn,
    InsertRowBelow,
    InsertRowAbove,
    InsertColumnRight,
    InsertColumnLeft,
    SortAscending,
    SortDescending,
    Undo,
    Redo,
    Repeat,
    // View
    WidenColumn,
    NarrowColumn,
    AutofitColumn,
    AutofitColumnFull,
    TallerRow,
    ShorterRow,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    Goto,
    SearchForward,
    SearchNext,
    SearchPrevious,
    ToggleHeader,
    /// Freeze the rows above the cursor (row 1 at the top), or unfreeze.
    ToggleFreeze,
    FormulaBar,
    ViewAsText,
    NextProblem,
    /// `M-x grid-mode`: reopen the current text buffer as a grid.
    GridMode,
    /// `grid-recalculate` (F9): recalculate every formula.
    Recalculate,
    /// Toggle cells between results and formula text (Excel's Ctrl+`).
    ShowFormulas,
    /// Outline the cells the cursor's formula reads.
    TracePrecedents,
    /// Outline the formulas that read the cursor cell.
    TraceDependents,
    /// `M-x grid-toggle-view`: switch this buffer between grid and raw text.
    ToggleView,
    /// In a `ge` popup (`*cell C7*`): write the text back to the cell.
    CellPopupCommit,
    /// In a `ge` popup: discard it.
    CellPopupAbort,
    // Emacs-style additions
    /// `C-x C-x`: swap the cursor and the selection's other corner.
    ExchangeAnchor,
    /// `C-l`: scroll the cursor row to the middle, top, then bottom.
    Recenter,
    SearchBackward,
    /// Jump to a column by its header name, with completion.
    JumpToColumn,
    /// `C-k`: kill the cells from the cursor to the end of the row.
    KillToRowEnd,
    Upcase,
    Downcase,
    Capitalize,
    /// `S-RET`: copy the cell down one row, continuing a series (org-table's
    /// copy-down); over a multi-row selection, fill it down.
    CopyDown,
    /// `M-y` after a paste: replace it with the previous kill.
    YankPop,
    /// Complete the cell's value from the column's choices or its values.
    CompleteCell,
    MoveRowUp,
    MoveRowDown,
    MoveColumnLeft,
    MoveColumnRight,
    HideColumn,
    ShowColumns,
    /// `C-x n n`: show only the selected rows.
    NarrowToSelection,
    /// Show only rows whose cursor-column value passes a typed test.
    Filter,
    /// Show only rows whose cursor-column value equals the cursor cell's.
    FilterByValue,
    /// `C-x n w`: drop narrowing and filters.
    Widen,
    CopyAsOrg,
    CopyAsMarkdown,
    CopyAsJson,
    /// In a text buffer: open the org/Markdown table at point as a grid.
    OrgTableToGrid,
    // Formatting by hand (`src/grid/cellfmt.rs`)
    /// Make the selected cells bold, or not bold when the cursor's is.
    ToggleBold,
    /// Give the selected cells the text color after the cursor cell's.
    CycleTextColor,
    /// Give the selected cells the fill color after the cursor cell's.
    CycleFillColor,
    /// Remove bold, colors and number formats from the selected cells.
    ClearFormat,
    /// Number formats (display only — the stored values are untouched).
    FormatGeneral,
    FormatNumber,
    FormatCurrency,
    FormatPercent,
    FormatDate,
    FormatText,
    /// One decimal place more / fewer in the selection's number format.
    IncreaseDecimals,
    DecreaseDecimals,
}

/// Steel name → command, for builtin registration.
pub const GRID_COMMANDS: &[(&str, GridCmd)] = {
    use GridCmd::*;
    &[
        ("grid-left", Left),
        ("grid-right", Right),
        ("grid-up", Up),
        ("grid-down", Down),
        ("grid-next-cell", NextCell),
        ("grid-previous-cell", PreviousCell),
        ("grid-edge-left", EdgeLeft),
        ("grid-edge-right", EdgeRight),
        ("grid-edge-up", EdgeUp),
        ("grid-edge-down", EdgeDown),
        ("grid-row-start", RowStart),
        ("grid-row-first-nonempty", RowFirstNonEmpty),
        ("grid-row-end", RowEnd),
        ("grid-goto-first-row", GotoFirstRow),
        ("grid-goto-last-row", GotoLastRow),
        ("grid-goto-column", GotoColumn),
        ("grid-goto-first-cell", GotoFirstCell),
        ("grid-goto-last-cell", GotoLastCell),
        ("grid-page-down", PageDown),
        ("grid-page-up", PageUp),
        ("grid-half-page-down", HalfPageDown),
        ("grid-half-page-up", HalfPageUp),
        ("grid-extend-left", ExtendLeft),
        ("grid-extend-right", ExtendRight),
        ("grid-extend-up", ExtendUp),
        ("grid-extend-down", ExtendDown),
        ("grid-select-cells", SelectCells),
        ("grid-select-rows", SelectRows),
        ("grid-select-columns", SelectColumns),
        ("grid-select-all", SelectAll),
        ("grid-select-cancel", SelectCancel),
        ("grid-edit-start", EditStart),
        ("grid-edit-end", EditEnd),
        ("grid-edit-replace", EditReplace),
        ("grid-edit-popup", EditPopup),
        ("grid-clear", Clear),
        ("grid-cut", Cut),
        ("grid-yank", Yank),
        ("grid-paste", Paste),
        ("grid-delete-row", DeleteRow),
        ("grid-delete-column", DeleteColumn),
        ("grid-insert-row-below", InsertRowBelow),
        ("grid-insert-row-above", InsertRowAbove),
        ("grid-insert-column-right", InsertColumnRight),
        ("grid-insert-column-left", InsertColumnLeft),
        ("grid-sort-ascending", SortAscending),
        ("grid-sort-descending", SortDescending),
        ("grid-undo", Undo),
        ("grid-redo", Redo),
        ("grid-repeat", Repeat),
        ("grid-widen-column", WidenColumn),
        ("grid-narrow-column", NarrowColumn),
        ("grid-autofit-column", AutofitColumn),
        ("grid-autofit-column-full", AutofitColumnFull),
        ("grid-taller-row", TallerRow),
        ("grid-shorter-row", ShorterRow),
        ("grid-zoom-in", ZoomIn),
        ("grid-zoom-out", ZoomOut),
        ("grid-zoom-reset", ZoomReset),
        ("grid-goto", Goto),
        ("grid-search-forward", SearchForward),
        ("grid-search-next", SearchNext),
        ("grid-search-previous", SearchPrevious),
        ("grid-toggle-header", ToggleHeader),
        ("grid-toggle-freeze", ToggleFreeze),
        ("grid-formula-bar", FormulaBar),
        ("grid-view-as-text", ViewAsText),
        ("grid-next-problem", NextProblem),
        ("grid-mode", GridMode),
        ("grid-recalculate", Recalculate),
        ("grid-show-formulas", ShowFormulas),
        ("grid-trace-precedents", TracePrecedents),
        ("grid-trace-dependents", TraceDependents),
        ("grid-toggle-view", ToggleView),
        ("grid-cell-edit-commit", CellPopupCommit),
        ("grid-cell-edit-abort", CellPopupAbort),
        ("grid-exchange-point-and-mark", ExchangeAnchor),
        ("grid-recenter", Recenter),
        ("grid-search-backward", SearchBackward),
        ("grid-jump-to-column", JumpToColumn),
        ("grid-kill-to-row-end", KillToRowEnd),
        ("grid-upcase", Upcase),
        ("grid-downcase", Downcase),
        ("grid-capitalize", Capitalize),
        ("grid-copy-down", CopyDown),
        ("grid-yank-pop", YankPop),
        ("grid-complete-cell", CompleteCell),
        ("grid-move-row-up", MoveRowUp),
        ("grid-move-row-down", MoveRowDown),
        ("grid-move-column-left", MoveColumnLeft),
        ("grid-move-column-right", MoveColumnRight),
        ("grid-hide-column", HideColumn),
        ("grid-show-columns", ShowColumns),
        ("grid-narrow-to-selection", NarrowToSelection),
        ("grid-filter", Filter),
        ("grid-filter-by-value", FilterByValue),
        ("grid-widen", Widen),
        ("grid-copy-as-org", CopyAsOrg),
        ("grid-copy-as-markdown", CopyAsMarkdown),
        ("grid-copy-as-json", CopyAsJson),
        ("org-table-to-grid", OrgTableToGrid),
        ("grid-toggle-bold", ToggleBold),
        ("grid-cycle-text-color", CycleTextColor),
        ("grid-cycle-fill-color", CycleFillColor),
        ("grid-clear-format", ClearFormat),
        ("grid-format-general", FormatGeneral),
        ("grid-format-number", FormatNumber),
        ("grid-format-currency", FormatCurrency),
        ("grid-format-percent", FormatPercent),
        ("grid-format-date", FormatDate),
        ("grid-format-text", FormatText),
        ("grid-increase-decimals", IncreaseDecimals),
        ("grid-decrease-decimals", DecreaseDecimals),
    ]
};

impl GridCmd {
    /// Whether this command changes cell data or formatting (so `.` repeats
    /// it, and it is refused while the file is still loading).
    pub fn edits(self) -> bool {
        use GridCmd::*;
        matches!(
            self,
            Clear
                | Cut
                | Paste
                | DeleteRow
                | DeleteColumn
                | InsertRowBelow
                | InsertRowAbove
                | InsertColumnRight
                | InsertColumnLeft
                | SortAscending
                | SortDescending
                | KillToRowEnd
                | Upcase
                | Downcase
                | Capitalize
                | CopyDown
                | YankPop
                | MoveRowUp
                | MoveRowDown
                | MoveColumnLeft
                | MoveColumnRight
                | ToggleBold
                | CycleTextColor
                | CycleFillColor
                | ClearFormat
                | FormatGeneral
                | FormatNumber
                | FormatCurrency
                | FormatPercent
                | FormatDate
                | FormatText
                | IncreaseDecimals
                | DecreaseDecimals
        )
    }

    /// Whether `C-u` is a flag for this command (changes what it does)
    /// rather than a repeat count: `C-u C-d` clears whole rows, `C-u C-c C-s`
    /// asks how to sort, `C-u S-RET` copies without incrementing.
    pub fn prefix_is_flag(self) -> bool {
        use GridCmd::*;
        matches!(self, Clear | SortAscending | SortDescending | CopyDown)
    }
}

/// Editor-wide grid settings (`grid-default-column-width`, …).
#[derive(Debug, Clone)]
pub enum GridOption {
    /// Width (100% px) of columns without their own width, for grids opened
    /// from now on.
    DefaultColumnWidth(f64),
    /// Widest an auto-fit (`=`, double-click a border) makes a column.
    MaxAutofitWidth(f64),
    /// Whether row 1 is a header: `None` sniffs it from the data.
    AutoHeader(Option<bool>),
    /// Drawn at the end of text cut off by its cell ("" draws nothing).
    TruncateIndicator(String),
    /// Seconds a Scheme function called from a formula may run.
    FunctionTimeout(f64),
}

/// A grid operation with arguments, queued by the Scheme API (`grid-set-cell`,
/// `grid-format-column`, …). Data edits from one Scheme command batch undo as
/// one step. Column/row arguments are 0-based; `None` marks an argument
/// Scheme passed that isn't a valid index, reported instead of applied.
#[derive(Debug, Clone)]
pub enum GridOp {
    /// `(row, col, text)` triples, applied as one edit.
    SetCells(Vec<(Option<usize>, Option<usize>, String)>),
    InsertRows {
        at: Option<usize>,
        n: usize,
    },
    DeleteRows {
        at: Option<usize>,
        n: usize,
    },
    InsertCols {
        at: Option<usize>,
        n: usize,
    },
    DeleteCols {
        at: Option<usize>,
        n: usize,
    },
    Sort {
        col: Option<usize>,
        descending: bool,
        kind: crate::grid::model::SortKind,
    },
    MoveTo {
        row: Option<usize>,
        col: Option<usize>,
    },
    Select {
        r0: Option<usize>,
        c0: Option<usize>,
        r1: Option<usize>,
        c1: Option<usize>,
    },
    ColumnWidth {
        col: Option<usize>,
        px: f64,
    },
    RowHeight {
        row: Option<usize>,
        px: f64,
    },
    Freeze(usize),
    SetHeader(bool),
    /// Zoom in percent (10–400).
    Zoom(f64),
    /// Call Scheme fn `callback` with the cells of `range` (`"A1:C10"`) as a
    /// list of rows, each a list of strings.
    ReadRange {
        range: String,
        callback: String,
    },
    ColumnFace {
        col: Option<usize>,
        face: String,
    },
    FormatColumn {
        col: Option<usize>,
        format: String,
    },
    /// `col: None` applies to every column. A `row` rule colors the whole
    /// row when `col`'s value passes.
    HighlightRule {
        col: Option<usize>,
        op: String,
        value: String,
        face: String,
        row: bool,
    },
    ClearStyles,
    CellFace {
        row: Option<usize>,
        col: Option<usize>,
        face: String,
    },
    ClearCellFaces,
    /// Hand-set formatting of one cell (`grid-set-cell-bold`, …): each
    /// `Some` field is changed; a color of `Some(None)` removes it.
    CellFormat {
        row: Option<usize>,
        col: Option<usize>,
        bold: Option<bool>,
        text: Option<Option<u8>>,
        fill: Option<Option<u8>>,
        /// A number format code or preset name (`"General"` removes it).
        num: Option<String>,
        /// The color name given, when it isn't one of the palette's.
        bad_color: Option<String>,
    },
    ClearCellFormats,
    /// Show the workbook's sheet with this name (`grid-open-sheet`).
    OpenSheet(String),
    /// Give the selection (or the cursor cell) this number format: a code
    /// or a preset name (`grid-set-number-format`).
    NumberFormat(String),
    /// Zebra-stripe rows with this face's background (`None` turns it off).
    Stripe(Option<String>),
    HeaderFace(String),
    Align {
        col: Option<usize>,
        align: String,
    },
    HideColumn {
        col: Option<usize>,
        hidden: bool,
    },
    /// Show only rows whose `col` value passes `(op value)`; filters stack.
    Filter {
        col: Option<usize>,
        op: String,
        value: String,
    },
    /// Values offered by `grid-complete-cell` for `col` (empty: none).
    Choices {
        col: Option<usize>,
        values: Vec<String>,
    },
}
