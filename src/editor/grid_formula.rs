//! Formulas in grid buffers: wiring the [`crate::formula`] engine into
//! [`GridDoc`] and the editor (see `docs/plans/formula-engine.md`).
//!
//! A formula cell's stored text is [`MARK`]`=…`; its *result* lives in the
//! [`Engine`]. [`GridDoc::cell`] shows results and [`GridDoc::source`] the
//! formula text, so display code and editing code each read what they need.
//! Every workbook edit goes through `GridDoc::model_op`, which hands the
//! model's change log to the engine and — for structural edits — rewrites
//! the references of every formula the edit moved, in the same undo step.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::Context;

use super::grid::{GridDoc, SHEET};
use super::*;
use crate::formula::ast::Expr;
use crate::formula::engine::{
    Engine, SheetValues, Source, display_text, is_formula, store_formula,
};
use crate::formula::refs::{self, Axis};
use crate::formula::value::Value;
use crate::grid::model::{Change, Workbook};

/// How long one recalculation slice may run before yielding a frame.
const SLICE: Duration = Duration::from_millis(8);

/// The engine's view of a workbook sheet.
pub(super) struct WbSource<'a> {
    pub wb: &'a Workbook,
    /// The workbook's other sheets, by lower-case name.
    pub others: &'a HashMap<String, SheetValues>,
}

impl Source for WbSource<'_> {
    fn text(&self, row: usize, col: usize) -> &str {
        self.wb.cell(SHEET, row, col)
    }
    fn extent(&self) -> (usize, usize) {
        (self.wb.used_rows(SHEET), self.wb.used_cols(SHEET))
    }
    fn for_each_in_column(
        &self,
        col: usize,
        rows: std::ops::Range<usize>,
        f: &mut dyn FnMut(usize, &str),
    ) {
        self.wb.for_each_in_column(SHEET, col, rows, |r, t| f(r, t));
    }
    fn sheet_name(&self) -> &str {
        &self.wb.sheet(SHEET).name
    }
    fn other_sheet(&self, name: &str) -> Option<&SheetValues> {
        self.others.get(&name.to_lowercase())
    }
}

/// What a user-entered cell text is stored as: `=…` (more than just `=`)
/// becomes a formula, in Excel's canonical form when it parses (`=sum(a1)`
/// → `=SUM(A1)`); `'=…` is the literal text `=…` (Excel's escape); anything
/// else is stored as typed.
pub(super) fn stored_input(text: &str) -> String {
    if text.len() > 1 && text.starts_with('=') {
        let parsed = crate::formula::parse::parse(text).or_else(|err| {
            // Excel closes parentheses left open (`=SUM(B4,B5` → `…)`).
            let open = unclosed_parens(text);
            if open == 0 {
                return Err(err);
            }
            crate::formula::parse::parse(&format!("{text}{}", ")".repeat(open)))
        });
        match parsed {
            Ok(e) => store_formula(&crate::formula::print::print(&e)),
            Err(_) => store_formula(text),
        }
    } else if let Some(rest) = text.strip_prefix('\'')
        && rest.starts_with('=')
    {
        rest.to_string()
    } else {
        text.to_string()
    }
}

/// How many `(` in formula `text` are still open (ignoring any inside
/// quoted text). 0 when they balance or there are extra `)`.
fn unclosed_parens(text: &str) -> usize {
    let mut depth: isize = 0;
    let mut in_str = false;
    for c in text.chars() {
        match c {
            '"' => in_str = !in_str,
            '(' if !in_str => depth += 1,
            ')' if !in_str => depth -= 1,
            _ => {}
        }
    }
    depth.max(0) as usize
}

/// Where row/column `i` lands after a change, or `None` if it was deleted.
fn map_index(i: usize, change: &Change, axis: Axis) -> Option<usize> {
    match (change, axis) {
        (Change::RowsInserted { at, n }, Axis::Rows)
        | (Change::ColsInserted { at, n }, Axis::Cols) => Some(if i >= *at { i + n } else { i }),
        (Change::RowsDeleted { at, n }, Axis::Rows)
        | (Change::ColsDeleted { at, n }, Axis::Cols) => {
            if i >= at + n {
                Some(i - n)
            } else if i >= *at {
                None
            } else {
                Some(i)
            }
        }
        (Change::Sorted { start, perm } | Change::RowsMoved { start, perm }, Axis::Rows) => {
            if i < *start || i >= start + perm.len() {
                return Some(i);
            }
            // perm[k] = old row of new row start+k.
            perm.iter().position(|&old| old == i).map(|k| start + k)
        }
        (Change::ColMoved { from, to }, Axis::Cols) => Some(if i == *from {
            *to
        } else if from < to && i > *from && i <= *to {
            i - 1
        } else if from > to && i >= *to && i < *from {
            i + 1
        } else {
            i
        }),
        _ => Some(i),
    }
}

impl GridDoc {
    /// What this grid holds that CSV can't store — formulas, hand-set
    /// formatting (bold, colors, number formats) — or `None`.
    pub(super) fn csv_would_lose(&self) -> Option<&'static str> {
        match (self.formulas.formula_count() > 0, !self.formats.is_empty()) {
            (true, true) => Some("formulas and formatting"),
            (true, false) => Some("formulas"),
            (false, true) => Some("formatting"),
            (false, false) => None,
        }
    }

    /// A CSV grid holding formulas or formatting can't be saved as CSV: it
    /// saves as `.xlsx` instead (see the plan's §7).
    pub(super) fn csv_blocked(&self) -> bool {
        !self.is_xlsx() && self.csv_would_lose().is_some()
    }

    /// Whether this grid saves as `.xlsx` (it visits an `.xlsx` file).
    pub(super) fn is_xlsx(&self) -> bool {
        self.ext.as_deref() == Some("xlsx")
    }

    /// What a cell draws: its formula under `grid-show-formulas`, else what
    /// it shows.
    pub(super) fn shown_text(&self, r: usize, c: usize) -> &str {
        if self.view.show_formulas && self.is_formula_at(r, c) {
            display_text(self.wb.cell(SHEET, r, c))
        } else {
            self.cell(r, c)
        }
    }

    /// A cell shown under number format `code`: the text, and whether the
    /// value reads as a number or date (so it aligns right). `None` when the
    /// format doesn't apply — an error value, or a formula shown as source.
    pub(super) fn number_formatted(
        &self,
        r: usize,
        c: usize,
        code: crate::grid::cellfmt::NumCode,
    ) -> Option<(String, bool)> {
        if self.view.show_formulas && self.is_formula_at(r, c) {
            return None;
        }
        let text = self.cell(r, c);
        if code.is_text() {
            return Some((text.to_string(), false));
        }
        let value = self
            .formula_value(r, c)
            .cloned()
            .unwrap_or_else(|| Value::text(text));
        let numeric = match &value {
            Value::Number(_) => true,
            Value::Text(s) => {
                crate::formula::value::parse_number(s.trim()).is_some()
                    || crate::formula::date::parse_datetime(s, None, false).is_some()
            }
            _ => false,
        };
        let shown = crate::formula::textfmt::format_value(&value, &code.code(), false).ok()?;
        Some((shown, numeric))
    }

    /// The text to edit for a cell — what typing it recreates the cell
    /// ([`crate::formula::engine::input_text`]): a formula's source
    /// (`=SUM(A1:A3)`), literal text starting with `=` as `'=…`, anything
    /// else as stored.
    pub(super) fn source(&self, r: usize, c: usize) -> std::borrow::Cow<'_, str> {
        crate::formula::engine::input_text(self.wb.cell(SHEET, r, c))
    }

    /// Whether `(r, c)` holds a formula.
    pub(super) fn is_formula_at(&self, r: usize, c: usize) -> bool {
        is_formula(self.wb.cell(SHEET, r, c))
    }

    /// The computed value of a formula cell.
    pub(super) fn formula_value(&self, r: usize, c: usize) -> Option<&Value> {
        self.formulas.result(r, c)
    }

    /// Every formula's position and parsed form, before a structural edit.
    pub(super) fn formula_snapshot(&self) -> Option<Vec<((usize, usize), Expr)>> {
        (self.formulas.formula_count() > 0).then(|| {
            self.formulas
                .positions()
                .filter_map(|(r, c)| Some(((r, c), self.formulas.expr(r, c)?.clone())))
                .collect()
        })
    }

    /// After a structural edit: rewrite every formula the edit affected, in
    /// place, as a cell edit (so it undoes with the structural one).
    pub(super) fn rewrite_after(&mut self, changes: &[Change], snap: Vec<((usize, usize), Expr)>) {
        let mut items: Vec<((usize, usize), Expr, bool)> =
            snap.into_iter().map(|(p, e)| (p, e, false)).collect();
        // References to other sheets are left alone: their cells didn't move.
        let sheet = self.wb.sheet(SHEET).name.clone();
        for change in changes {
            match change {
                Change::Cells(_) | Change::Reshaped => continue,
                Change::RowsInserted { at, n } => {
                    for it in &mut items {
                        it.2 |= refs::insert(&mut it.1, &sheet, Axis::Rows, *at, *n);
                    }
                }
                Change::ColsInserted { at, n } => {
                    for it in &mut items {
                        it.2 |= refs::insert(&mut it.1, &sheet, Axis::Cols, *at, *n);
                    }
                }
                Change::RowsDeleted { at, n } => {
                    for it in &mut items {
                        it.2 |= refs::delete(&mut it.1, &sheet, Axis::Rows, *at, *n);
                    }
                }
                Change::ColsDeleted { at, n } => {
                    for it in &mut items {
                        it.2 |= refs::delete(&mut it.1, &sheet, Axis::Cols, *at, *n);
                    }
                }
                Change::Sorted { .. } => {
                    // A sort moves formulas like a copy: their relative
                    // references follow them (Excel's sort behaviour).
                    for it in &mut items {
                        if let Some(new) = map_index(it.0.0, change, Axis::Rows)
                            && new != it.0.0
                        {
                            it.2 |= refs::translate(&mut it.1, new as isize - it.0.0 as isize, 0);
                        }
                    }
                }
                Change::RowsMoved { .. } => {
                    let map = |i: usize| map_index(i, change, Axis::Rows).unwrap_or(i);
                    for it in &mut items {
                        it.2 |= refs::remap(&mut it.1, &sheet, Axis::Rows, &map);
                    }
                }
                Change::ColMoved { .. } => {
                    let map = |i: usize| map_index(i, change, Axis::Cols).unwrap_or(i);
                    for it in &mut items {
                        it.2 |= refs::remap(&mut it.1, &sheet, Axis::Cols, &map);
                    }
                }
            }
            // Positions move with the edit; deleted formulas are gone.
            items.retain_mut(|it| {
                match (
                    map_index(it.0.0, change, Axis::Rows),
                    map_index(it.0.1, change, Axis::Cols),
                ) {
                    (Some(r), Some(c)) => {
                        it.0 = (r, c);
                        true
                    }
                    _ => false,
                }
            });
        }
        let writes: Vec<(usize, usize, String)> = items
            .into_iter()
            .filter(|it| it.2)
            .map(|((r, c), e, _)| (r, c, store_formula(&crate::formula::print::print(&e))))
            .collect();
        if !writes.is_empty() {
            self.wb.set_cells(SHEET, &writes);
        }
    }

    /// Carry hand-set formatting, and column and row sizes and hidden
    /// flags, along with the cells a structural edit moved; those of deleted
    /// cells go with them.
    pub(super) fn remap_layout(&mut self, changes: &[Change]) {
        for change in changes {
            if matches!(change, Change::Cells(_) | Change::Reshaped) {
                continue;
            }
            let rows = |r| map_index(r, change, Axis::Rows);
            let cols = |c| map_index(c, change, Axis::Cols);
            self.formats.remap(rows, cols);
            self.view.rows.remap(rows);
            self.view.cols.remap(cols);
            self.xlsx_text = std::mem::take(&mut self.xlsx_text)
                .into_iter()
                .filter_map(|((r, c), t)| Some(((rows(r)?, cols(c)?), t)))
                .collect();
        }
    }

    /// Hand the model's change log to the engine and run a recalculation
    /// slice. Returns whether work remains (the editor schedules more).
    pub(super) fn absorb_changes(&mut self) -> bool {
        let changes = self.wb.take_changes();
        if !changes.is_empty() {
            let structural = changes.iter().any(|c| !matches!(c, Change::Cells(_)));
            let src = WbSource {
                wb: &self.wb,
                others: &self.other_sheets,
            };
            if structural {
                // Positions shifted: re-index from the cells (only once a
                // formula has ever existed here — plain CSVs skip the scan).
                if self.had_formulas {
                    self.formulas.rebuild(&src);
                }
            } else {
                let cells: Vec<(usize, usize)> = changes
                    .into_iter()
                    .flat_map(|c| match c {
                        Change::Cells(v) => v,
                        _ => Vec::new(),
                    })
                    .collect();
                self.formulas.cells_changed(&src, &cells);
            }
            self.had_formulas |= self.formulas.formula_count() > 0;
        }
        self.recalc_slice()
    }

    /// Re-index every formula from scratch (a load or reload).
    pub(super) fn rebuild_formulas(&mut self) {
        self.wb.take_changes();
        let src = WbSource {
            wb: &self.wb,
            others: &self.other_sheets,
        };
        self.formulas = Engine::new();
        self.formulas.rebuild(&src);
        self.had_formulas = self.formulas.formula_count() > 0;
        self.recalc_slice();
    }

    /// Run recalculation for up to one slice. Returns whether work remains.
    pub(super) fn recalc_slice(&mut self) -> bool {
        if !self.formulas.busy() {
            return false;
        }
        let src = WbSource {
            wb: &self.wb,
            others: &self.other_sheets,
        };
        !self.formulas.recalc(&src, Some(Instant::now() + SLICE))
    }
}

impl Editor {
    /// After a grid command: absorb edits into the formula engine, and if a
    /// large recalculation didn't finish in one slice, keep it going on later
    /// frames (stale results draw until then).
    pub(super) fn grid_formulas_after(&mut self, cx: &mut Context<Self>) {
        if !self.grid_fn_names.is_empty()
            && let Some(g) = self.active.grid.as_deref_mut()
        {
            // A grid opened since the functions were defined learns them here.
            g.formulas
                .set_externals(self.grid_fn_names.keys().cloned().collect());
        }
        let busy = self
            .active
            .grid
            .as_deref_mut()
            .is_some_and(|g| g.absorb_changes());
        if busy && !self.grid_recalc_scheduled {
            self.grid_recalc_scheduled = true;
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(1))
                        .await;
                    let more = this
                        .update(cx, |editor, cx| {
                            // Every grid with work left gets a slice (one
                            // loop for all: two big workbooks opened together
                            // both finish).
                            let mut more = false;
                            let mut done = Vec::new();
                            let docs = std::iter::once(&mut editor.active)
                                .map(|d| (None, d))
                                .chain(
                                    editor
                                        .inactive
                                        .iter_mut()
                                        .enumerate()
                                        .map(|(i, d)| (Some(i), d)),
                                );
                            for (slot, doc) in docs {
                                let Some(g) = doc.grid.as_deref_mut() else {
                                    continue;
                                };
                                if !g.formulas.busy() {
                                    continue;
                                }
                                if g.recalc_slice() {
                                    more = true;
                                } else {
                                    done.push(slot);
                                }
                            }
                            editor.grid_dispatch_calls();
                            cx.notify();
                            // Their results are final: the workbooks' other
                            // open sheets can read them now.
                            if !done.is_empty() {
                                editor.grid_sync_sheets_from(done, cx);
                            }
                            if !more {
                                editor.grid_recalc_scheduled = false;
                            }
                            more
                        })
                        .unwrap_or(false);
                    if !more {
                        break;
                    }
                }
            })
            .detach();
        }
        self.grid_dispatch_calls();
    }

    /// Save the active grid as an `.xlsx` file (the required Save As for a
    /// CSV holding formulas). The buffer then visits the new file; the CSV
    /// it came from is left untouched on disk.
    pub(super) fn grid_save_as_xlsx(&mut self, input: &str, cx: &mut Context<Self>) {
        let mut target = PathBuf::from(expand_path(input));
        if target
            .extension()
            .is_none_or(|e| !e.eq_ignore_ascii_case("xlsx"))
        {
            target.set_extension("xlsx");
        }
        if target.exists() && self.active.path.as_deref() != Some(target.as_path()) {
            self.echo(
                format!("{} already exists — choose another name", target.display()),
                cx,
            );
            return;
        }
        self.grid_commit_pending(cx);
        let Some(g) = self.active.grid.as_deref_mut() else {
            return;
        };
        let bytes = g.xlsx_bytes();
        if bytes.is_empty() {
            self.echo("Saving .xlsx isn't available yet in this build", cx);
            return;
        }
        if let Err(err) = crate::save::write_now(&target, &bytes) {
            self.echo(format!("Save failed: {err}"), cx);
            return;
        }
        let Some(g) = self.active.grid.as_deref_mut() else {
            return;
        };
        g.ext = Some("xlsx".into());
        g.saved_externally();
        let old_name = self.active.name.clone();
        let file = target
            .file_name()
            .map_or_else(|| old_name.clone(), |f| f.to_string_lossy().into_owned());
        let new_name = self.unique_buffer_name(&file, &target);
        self.active.name = new_name.clone();
        self.active.path = Some(target.clone());
        self.active.modified = false;
        for pane in &mut self.panes {
            if pane.buffer == old_name {
                pane.buffer = new_name.clone();
            }
        }
        self.reconcile_watches(cx);
        self.echo(
            format!("Wrote {new_name} — this buffer now visits it ({old_name} is unchanged)"),
            cx,
        );
        self.grid_after(cx);
    }

    // ---- Scheme functions -------------------------------------------------

    /// `define-grid-function` / `define-grid-helper` ran: share it with the
    /// grid-function interpreter (started on first use) and, for a function,
    /// make it callable from formulas.
    pub(super) fn grid_register_function(
        &mut self,
        name: String,
        source: String,
        helper: bool,
        cx: &mut Context<Self>,
    ) {
        self.ensure_grid_fns(cx);
        if let Some(fns) = &self.grid_fns {
            fns.define(source);
        }
        if !helper {
            let upper = name.to_uppercase();
            // A built-in's name always calls the built-in: say so, rather
            // than define a function no formula can reach.
            if crate::formula::func::lookup(&upper).is_some()
                || crate::formula::func::special(&upper).is_some()
            {
                self.echo(
                    format!("{name}: a built-in function has that name — formulas keep calling the built-in; choose another name"),
                    cx,
                );
                return;
            }
            self.grid_fn_names.insert(upper, name);
        }
        // A (re)definition — of a function, or of a helper one calls —
        // changes what calls return: recompute everything that might call
        // one.
        let names: HashSet<String> = self.grid_fn_names.keys().cloned().collect();
        for doc in std::iter::once(&mut self.active).chain(self.inactive.iter_mut()) {
            if let Some(g) = doc.grid.as_deref_mut() {
                g.formulas.functions_redefined(names.clone());
            }
        }
        if self.active.grid.is_some() {
            self.grid_after(cx);
        }
    }

    fn ensure_grid_fns(&mut self, cx: &mut Context<Self>) {
        if self.grid_fns.is_some() {
            return;
        }
        let (fns, mut outcomes) = crate::grid_fn::GridFnEngine::new();
        fns.set_timeout(self.grid_fn_timeout);
        self.grid_fns = Some(fns);
        self.grid_fn_task = Some(cx.spawn(async move |this, cx| {
            use futures::StreamExt;
            while let Some(outcome) = outcomes.next().await {
                if this
                    .update(cx, |editor, cx| editor.apply_grid_fn_outcome(outcome, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    pub(super) fn grid_set_function_timeout(&mut self, secs: u64) {
        self.grid_fn_timeout = secs;
        if let Some(fns) = &self.grid_fns {
            fns.set_timeout(secs);
        }
    }

    /// Send every grid's queued Scheme calls to the interpreter.
    pub(super) fn grid_dispatch_calls(&mut self) {
        let Some(fns) = &self.grid_fns else {
            return;
        };
        for doc in std::iter::once(&mut self.active).chain(self.inactive.iter_mut()) {
            let Some(g) = doc.grid.as_deref_mut() else {
                continue;
            };
            for call in g.formulas.take_requests() {
                self.grid_fn_next_id += 1;
                let id = self.grid_fn_next_id;
                let symbol = self
                    .grid_fn_names
                    .get(&call.name)
                    .cloned()
                    .unwrap_or_else(|| call.name.clone());
                fns.call(id, &symbol, &call.args);
                // By the grid's epoch, not the buffer's name: Save As renames
                // the buffer while the call runs.
                self.grid_fn_calls.insert(id, (g.epoch, call));
            }
        }
    }

    fn apply_grid_fn_outcome(
        &mut self,
        outcome: crate::grid_fn::GridFnOutcome,
        cx: &mut Context<Self>,
    ) {
        use crate::formula::value::ErrorKind;
        use crate::grid_fn::GridFnOutcome;
        match outcome {
            GridFnOutcome::DefineFailed { error } => {
                self.echo(format!("grid function definition failed: {error}"), cx);
            }
            GridFnOutcome::Called {
                id,
                result,
                timed_out,
            } => {
                let Some((epoch, call)) = self.grid_fn_calls.remove(&id) else {
                    return;
                };
                let value = match result {
                    Ok(v) => v,
                    Err(_) if timed_out => {
                        let secs = self.grid_fn_timeout;
                        self.echo(format!("{} timed out after {secs} s", call.name), cx);
                        Value::Error(ErrorKind::Calc)
                    }
                    Err(e) => {
                        self.echo(format!("{}: {e}", call.name), cx);
                        Value::Error(ErrorKind::Value)
                    }
                };
                let of_epoch =
                    |d: &&mut super::Document| d.grid.as_deref().is_some_and(|g| g.epoch == epoch);
                let active = of_epoch(&&mut self.active);
                let doc = std::iter::once(&mut self.active)
                    .chain(self.inactive.iter_mut())
                    .find(of_epoch);
                if let Some(g) = doc.and_then(|d| d.grid.as_deref_mut()) {
                    g.formulas.deliver(call, value);
                    if !active {
                        while g.recalc_slice() {}
                    }
                }
                if active {
                    self.grid_after(cx);
                } else {
                    self.grid_dispatch_calls();
                    cx.notify();
                }
            }
        }
    }

    /// Outline what the cursor's formula reads (`precedents`) or the
    /// formulas reading the cursor cell; cleared by the next command.
    pub(super) fn grid_trace(&mut self, precedents: bool, cx: &mut Context<Self>) {
        let Some(g) = self.active.grid.as_deref_mut() else {
            return;
        };
        let (r, c) = g.view.cursor;
        let (rows, cols) = g.used();
        let rects: Vec<(usize, usize, usize, usize)> = if precedents {
            let Some(e) = g.formulas.expr(r, c) else {
                self.echo("Not a formula", cx);
                return;
            };
            let mut refs = Vec::new();
            e.refs(&mut refs);
            refs.iter()
                .filter(|rf| rf.sheet.is_none())
                .map(|rf| {
                    let (r0, c0, r1, c1) = rf.area.bounds();
                    (
                        r0,
                        c0,
                        r1.min(rows.max(r0 + 1) - 1),
                        c1.min(cols.max(c0 + 1) - 1),
                    )
                })
                .collect()
        } else {
            g.formulas
                .dependents(r, c)
                .into_iter()
                .map(|(r, c)| (r, c, r, c))
                .collect()
        };
        let n = rects.len();
        g.view.trace = rects;
        let what = if precedents { "reads" } else { "is read by" };
        self.echo(
            if n == 0 {
                format!("{} {what} nothing", crate::grid::view::cell_name(r, c))
            } else {
                format!(
                    "{} {what} {n} {} (outlined)",
                    crate::grid::view::cell_name(r, c),
                    if precedents {
                        "reference(s)"
                    } else {
                        "formula(s)"
                    }
                )
            },
            cx,
        );
        self.grid_after(cx);
    }

    // ---- typing a formula ---------------------------------------------------

    /// Point mode: steer the reference being pointed at by `(dr, dc)`
    /// (`extend` keeps the anchor, making a range). Returns whether the edit
    /// was in point mode (otherwise the key does its usual thing).
    pub(super) fn grid_point(
        &mut self,
        dr: isize,
        dc: isize,
        extend: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(g) = self.active.grid.as_deref_mut() else {
            return false;
        };
        let Some(e) = g.view.edit.as_ref() else {
            return false;
        };
        if !e.point_eligible() {
            return false;
        }
        let (anchor, at) = match &e.point {
            Some(p) if p.span.end == e.caret => (p.anchor, p.at),
            _ => ((e.row, e.col), (e.row, e.col)),
        };
        // Step over hidden columns and filtered rows, as the cursor does.
        let step = |i: usize, d: isize, f: &dyn Fn(usize, bool) -> usize| {
            (0..d.unsigned_abs()).fold(i, |i, _| f(i, d > 0))
        };
        let at = (
            step(at.0, dr, &|r, down| g.row_step(r, down)),
            step(at.1, dc, &|c, right| g.col_step(c, right)),
        );
        let Some(e) = g.view.edit.as_mut() else {
            return false;
        };
        let anchor = if extend && e.point.is_some() {
            anchor
        } else {
            at
        };
        e.set_point(anchor, at);
        self.grid_after(cx);
        true
    }

    /// Function names a formula can call: built-ins, the evaluator's forms,
    /// and Scheme functions.
    fn formula_function_names(&self) -> Vec<String> {
        let mut names: Vec<String> = crate::formula::func::all()
            .map(|f| f.name.to_string())
            .chain(
                crate::formula::func::SPECIAL
                    .iter()
                    .map(|s| s.0.to_string()),
            )
            .chain(self.grid_fn_names.keys().cloned())
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// Function names starting with the word before the caret, if the edit
    /// is a formula with such a word.
    fn completion_candidates(&self) -> Option<(usize, Vec<String>)> {
        let e = self.active.grid.as_ref()?.view.edit.as_ref()?;
        if !e.text.starts_with('=') {
            return None;
        }
        let (start, word) = crate::formula::parse::word_before(&e.text, e.caret)?;
        // Already a complete call name followed by `(`: nothing to complete.
        if e.text[e.caret..].starts_with('(') {
            return None;
        }
        let upper = word.to_ascii_uppercase();
        let names: Vec<String> = self
            .formula_function_names()
            .into_iter()
            .filter(|n| n.starts_with(&upper))
            .collect();
        (!names.is_empty()).then_some((start, names))
    }

    /// Open, update, or close the function-name dropdown to match the
    /// edit (called after every command).
    pub(super) fn grid_refresh_completion(&mut self) {
        let found = self.completion_candidates();
        let Some(e) = self
            .active
            .grid
            .as_deref_mut()
            .and_then(|g| g.view.edit.as_mut())
        else {
            return;
        };
        if e.completion_dismissed
            .as_ref()
            .is_some_and(|t| *t != e.text)
        {
            e.completion_dismissed = None;
        }
        e.completion = match found {
            Some((start, names)) if e.completion_dismissed.is_none() => {
                // Keep the highlighted name across keystrokes when it still
                // matches.
                let selected = e
                    .completion
                    .as_ref()
                    .and_then(|old| names.iter().position(|n| *n == old.names[old.selected]))
                    .unwrap_or(0);
                Some(crate::grid::view::FormulaCompletion {
                    start,
                    names,
                    selected,
                })
            }
            _ => None,
        };
    }

    /// Whether the dropdown is showing.
    pub(super) fn grid_completion_open(&self) -> bool {
        self.active
            .grid
            .as_ref()
            .and_then(|g| g.view.edit.as_ref())
            .is_some_and(|e| e.completion.is_some())
    }

    /// Move the dropdown's highlight by `delta` (wrapping).
    pub(super) fn grid_completion_move(&mut self, delta: isize, cx: &mut Context<Self>) {
        if let Some(c) = self
            .active
            .grid
            .as_deref_mut()
            .and_then(|g| g.view.edit.as_mut())
            .and_then(|e| e.completion.as_mut())
        {
            let n = c.names.len() as isize;
            c.selected = (c.selected as isize + delta).rem_euclid(n.max(1)) as usize;
        }
        cx.notify();
    }

    /// Close the dropdown until the text changes (Escape).
    pub(super) fn grid_completion_dismiss(&mut self, cx: &mut Context<Self>) {
        if let Some(e) = self
            .active
            .grid
            .as_deref_mut()
            .and_then(|g| g.view.edit.as_mut())
        {
            e.completion = None;
            e.completion_dismissed = Some(e.text.clone());
        }
        cx.notify();
    }

    /// Accept the dropdown's highlighted name (or row `index`): replace the
    /// partial name with `NAME(`. Returns whether the dropdown was open.
    pub(super) fn grid_complete_function(
        &mut self,
        index: Option<usize>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(e) = self
            .active
            .grid
            .as_deref_mut()
            .and_then(|g| g.view.edit.as_mut())
        else {
            return false;
        };
        let Some(c) = e.completion.take() else {
            return false;
        };
        let name = &c.names[index.unwrap_or(c.selected).min(c.names.len() - 1)];
        e.text.replace_range(c.start..e.caret, &format!("{name}("));
        e.caret = c.start + name.len() + 1;
        e.point = None;
        self.grid_after(cx);
        true
    }

    /// What the dropdown shows: each name with its argument list, the
    /// highlighted one's full signature in the side panel.
    pub(super) fn grid_completion_view(g: &GridDoc) -> Option<super::CompletionView> {
        let c = g.view.edit.as_ref()?.completion.as_ref()?;
        let sig = |n: &str| crate::formula::func::sigs::signature(n);
        let items = c
            .names
            .iter()
            .map(|n| super::CompletionRow {
                label: n.clone(),
                detail: sig(n).map(|s| format!("({s})")),
                kind: Some(if sig(n).is_some() { "fn" } else { "scheme" }.to_string()),
            })
            .collect();
        let selected = &c.names[c.selected];
        let doc = Some(match sig(selected) {
            Some(s) => format!("{selected}({s})"),
            None => format!("{selected}(…) — a Scheme function (define-grid-function)"),
        });
        Some(super::CompletionView {
            items,
            selected: c.selected,
            doc,
        })
    }

    /// The echo-area hint while typing a formula: the signature of the call
    /// the caret is in (function names show in the dropdown instead).
    pub(super) fn grid_formula_hint(&self) -> Option<String> {
        let g = self.active.grid.as_ref()?;
        let Some(e) = g.view.edit.as_ref() else {
            // On a formula that doesn't parse: say why.
            let (r, c) = g.view.cursor;
            return g
                .formulas
                .parse_error(r, c)
                .map(|err| format!("#ERROR! in this formula: {err}"));
        };
        if !e.text.starts_with('=') {
            return None;
        }
        if e.completion.is_some() {
            return None; // the dropdown says it
        }
        let (name, arg) = crate::formula::parse::call_context(&e.text, e.caret)?;
        crate::formula::func::sigs::hint(&name, arg).or_else(|| {
            self.grid_fn_names
                .contains_key(&name)
                .then(|| format!("{name}(…)   (a Scheme function)"))
        })
    }

    /// `M-x grid-recalculate` (F9): recalculate every formula.
    pub(super) fn grid_recalculate(&mut self, cx: &mut Context<Self>) {
        if let Some(g) = self.active.grid.as_deref_mut() {
            g.formulas.mark_all();
            g.formulas.forget_calls();
        }
        self.grid_formulas_after(cx);
        cx.notify();
    }
}

/// Formula `source` (`=…`) moved by `(dr, dc)`, as a copy/fill moves it:
/// unanchored references follow. Text that isn't a parseable formula is
/// returned unchanged.
pub(super) fn translated(source: &str, dr: isize, dc: isize) -> String {
    if !source.starts_with('=') {
        return source.to_string();
    }
    match crate::formula::parse::parse(source) {
        Ok(mut e) => {
            refs::translate(&mut e, dr, dc);
            crate::formula::print::print(&e)
        }
        Err(_) => source.to_string(),
    }
}

/// Formula cells of a grid copy, so pasting the same clipboard text back
/// into a grid pastes formulas (translated), not their copied results.
#[derive(Clone, Debug)]
pub(super) struct FormulaClip {
    /// The clipboard text the copy produced (a paste matches on it).
    pub text: String,
    /// `(block row, block col, source row, source col, formula text)`.
    pub cells: Vec<(usize, usize, usize, usize, String)>,
}

impl GridDoc {
    /// The formula cells of the copied block (rows as `visible_block` takes
    /// them: shown rows only).
    pub(super) fn copied_formulas(
        &self,
        sel: &crate::grid::view::CellRange,
    ) -> Vec<(usize, usize, usize, usize, String)> {
        let (rows, cols) = self.used();
        let mut out = Vec::new();
        let shown = (sel.r0..=sel.r1).filter(|&r| r >= rows || self.row_shown(r));
        for (bi, r) in shown.enumerate() {
            for c in sel.c0..=sel.c1.min(cols.saturating_sub(1)) {
                // Formulas, and text starting with `=` (as `'=…`, so it
                // pastes back as text, not a formula).
                if r < rows
                    && self
                        .wb
                        .cell(SHEET, r, c)
                        .starts_with(['=', crate::formula::engine::MARK])
                {
                    out.push((bi, c - sel.c0, r, c, self.source(r, c).into_owned()));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_conversion() {
        assert_eq!(stored_input("=sum(a1:a3)"), store_formula("=SUM(A1:A3)"));
        assert_eq!(
            stored_input("=SUM("),
            store_formula("=SUM("),
            "kept even unparsed"
        );
        assert_eq!(stored_input("="), "=");
        assert_eq!(stored_input("'=1+1"), "=1+1");
        assert_eq!(stored_input("'quoted"), "'quoted");
        // Open parentheses close themselves on commit, as in Excel.
        assert_eq!(stored_input("=sum(B4,B5"), store_formula("=SUM(B4,B5)"));
        assert_eq!(
            stored_input("=IF(A1>0,SUM(B1:B3"),
            store_formula("=IF(A1>0,SUM(B1:B3))")
        );
        assert_eq!(stored_input("=SUM(\"(\""), store_formula("=SUM(\"(\")"));
        assert_eq!(stored_input("=SUM(1,"), store_formula("=SUM(1,)"));
        assert_eq!(stored_input("plain"), "plain");
    }

    #[test]
    fn index_mapping() {
        let ins = Change::RowsInserted { at: 2, n: 3 };
        assert_eq!(map_index(1, &ins, Axis::Rows), Some(1));
        assert_eq!(map_index(2, &ins, Axis::Rows), Some(5));
        assert_eq!(map_index(2, &ins, Axis::Cols), Some(2));
        let del = Change::ColsDeleted { at: 1, n: 2 };
        assert_eq!(map_index(1, &del, Axis::Cols), None);
        assert_eq!(map_index(4, &del, Axis::Cols), Some(2));
        let sort = Change::Sorted {
            start: 1,
            perm: vec![3, 1, 2],
        };
        assert_eq!(map_index(3, &sort, Axis::Rows), Some(1));
        assert_eq!(map_index(1, &sort, Axis::Rows), Some(2));
        assert_eq!(map_index(0, &sort, Axis::Rows), Some(0));
        let mv = Change::ColMoved { from: 0, to: 2 };
        assert_eq!(map_index(0, &mv, Axis::Cols), Some(2));
        assert_eq!(map_index(2, &mv, Axis::Cols), Some(1));
    }

    #[test]
    fn row_move_keeps_range_total() {
        // Rows 2 and 3 (0-based 1, 2) swap, as M-down on row 2 does.
        let change = Change::RowsMoved {
            start: 1,
            perm: vec![2, 1],
        };
        let mut e = crate::formula::parse::parse("=SUM(A1:A3)").unwrap();
        let map = |i: usize| map_index(i, &change, Axis::Rows).unwrap_or(i);
        refs::remap(&mut e, "Sheet1", Axis::Rows, &map);
        assert_eq!(crate::formula::print::print(&e), "=SUM(A1:A3)");
    }

    #[test]
    fn grouped_structural_edits_rewrite_correctly() {
        let csv: String = (1..=7).map(|i| format!("{i}\n")).collect();
        let wb = crate::grid::parse::parse_now(csv.into_bytes(), Some("csv")).unwrap();
        let mut g = GridDoc::new(wb, Some("csv".into()), 0, &Default::default());
        let edits: Vec<(usize, usize, String)> = (0..7)
            .map(|r| (r, 1, stored_input(&format!("=A{}*{}", r + 1, r + 1))))
            .collect();
        g.model_op(|wb| wb.set_cells(SHEET, &edits));
        while g.absorb_changes() {}
        // The filtered delete-row path: several deletes, bottom-up, one group.
        g.grouped(|g| {
            for k in [4usize, 2] {
                g.model_op(|wb| wb.delete_rows(SHEET, k, 1));
                let mut p: Vec<_> = g.formulas.positions().collect();
                p.sort();
                eprintln!(
                    "after delete {k}: engine positions {p:?}; cells {:?}",
                    (0..7)
                        .map(|r| g.source(r, 1).to_string())
                        .collect::<Vec<_>>()
                );
            }
        });
        while g.absorb_changes() {}
        let got: Vec<String> = (0..5).map(|r| g.source(r, 1).to_string()).collect();
        let all: Vec<(String, String)> = (0..8)
            .map(|r| (g.source(r, 0).to_string(), g.source(r, 1).to_string()))
            .collect();
        // Original rows 0, 1, 3, 5, 6 survive.
        assert_eq!(
            got,
            ["=A1*1", "=A2*2", "=A3*4", "=A4*6", "=A5*7"],
            "{all:?}"
        );
        assert_eq!(g.wb.used_rows(SHEET), 5, "{all:?}");
    }
}
