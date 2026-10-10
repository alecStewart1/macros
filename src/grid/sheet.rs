//! One sheet's cells: column-major, chunked storage.
//!
//! - **Column-major:** sorting, type inference, auto-fit and dictionary
//!   encoding all walk one column, so each column is its own run of
//!   [`CellRef`]s. Drawing reads ~1000 cells a frame, where layout is moot.
//! - **Chunked:** a column is a list of ~[`CHUNK_ROWS`]-row chunks, so
//!   inserting or deleting a row shifts one chunk per column rather than the
//!   whole column. Row operations always apply to every column, so all columns
//!   (and the per-row [`RowMeta`]) share one set of chunk boundaries, indexed
//!   by `starts` — a row lookup is one binary search, done once per row even
//!   when reading many columns.
//! - **Dense inside the used area, nothing outside:** every column has a cell
//!   for every row (Empty where a row was ragged), and reading beyond the used
//!   area just yields `""`.
//!
//! Every structural change is computed from the shared chunk lengths only, then
//! applied identically to each column and to the metadata — that is what keeps
//! the boundaries shared.

use std::ops::{ControlFlow, Range};
use std::sync::Arc;

use super::cell::CellRef;
use super::infer::ColType;
use super::intern::StringPool;

/// Target rows per chunk.
pub const CHUNK_ROWS: usize = 4096;
/// A chunk that grows past this is split.
pub const MAX_CHUNK_ROWS: usize = 2 * CHUNK_ROWS;
/// A chunk that shrinks below this (after a delete) is merged with neighbours.
pub const MIN_CHUNK_ROWS: usize = CHUNK_ROWS / 4;

pub(super) type Chunks<T> = Vec<Vec<T>>;

/// `col`'s cells regrouped into chunks of `lens` (Empty-padded, or cut, to
/// their total).
fn rechunk(col: Chunks<CellRef>, lens: &[usize]) -> Chunks<CellRef> {
    if col.iter().map(Vec::len).eq(lens.iter().copied()) {
        return col;
    }
    let mut cells = col.into_iter().flatten();
    lens.iter()
        .map(|&n| {
            (0..n)
                .map(|_| cells.next().unwrap_or(CellRef::EMPTY))
                .collect()
        })
        .collect()
}

/// How a record ended in the source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Terminator {
    /// No terminator: the last record of a file without a final newline, or a
    /// row that didn't come from the file (the writer picks the dialect's).
    #[default]
    None,
    Lf,
    CrLf,
    Cr,
}

impl Terminator {
    pub fn as_str(self) -> &'static str {
        match self {
            Terminator::None => "",
            Terminator::Lf => "\n",
            Terminator::CrLf => "\r\n",
            Terminator::Cr => "\r",
        }
    }

    /// `self`, or `fallback` when there is no terminator.
    pub fn or(self, fallback: Terminator) -> Terminator {
        if self == Terminator::None {
            fallback
        } else {
            self
        }
    }
}

/// Where a row came from in the source, for byte-exact saves.
///
/// A row is *clean* when it was loaded from the file and hasn't been edited
/// since: the writer then copies `source[src_start..][..src_len]` verbatim,
/// keeping its original quoting and spacing, followed by its own terminator.
/// Sorting moves rows without dirtying them — their bytes are still intact.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RowMeta {
    /// Byte offset of the record in the sheet source.
    pub src_start: u64,
    /// Record length, excluding the terminator.
    pub src_len: u32,
    pub term: Terminator,
    flags: u8,
}

impl RowMeta {
    const ORIGIN: u8 = 1;
    const DIRTY: u8 = 2;

    /// A row read from `source[start..start + len]`. A record too long for
    /// `src_len` (4 GiB) is born dirty, so it is re-serialized from its cells.
    pub(super) fn original(start: usize, len: usize, term: Terminator) -> RowMeta {
        match u32::try_from(len) {
            Ok(src_len) => RowMeta {
                src_start: start as u64,
                src_len,
                term,
                flags: Self::ORIGIN,
            },
            Err(_) => RowMeta {
                term,
                flags: Self::DIRTY,
                ..RowMeta::default()
            },
        }
    }

    /// Loaded from the file and unedited: written back as its raw bytes.
    pub fn is_clean(&self) -> bool {
        self.flags & (Self::ORIGIN | Self::DIRTY) == Self::ORIGIN
    }

    pub fn is_dirty(&self) -> bool {
        self.flags & Self::DIRTY != 0
    }

    /// The record's byte range in the source, if it came from the file.
    pub fn src_range(&self) -> Option<Range<usize>> {
        (self.flags & Self::ORIGIN != 0).then(|| {
            let start = self.src_start as usize;
            start..start + self.src_len as usize
        })
    }

    pub(super) fn set_dirty(&mut self, dirty: bool) {
        if dirty {
            self.flags |= Self::DIRTY;
        } else {
            self.flags &= !Self::DIRTY;
        }
    }
}

/// One sheet (tab) of a workbook. A CSV workbook has exactly one.
#[derive(Clone)]
pub struct Sheet {
    #[allow(dead_code)] // model API kept for multi-sheet / `.xlsx`
    pub name: String,
    /// The text the Span cells point into: the file as loaded, BOM stripped
    /// and transcoded to UTF-8. Never modified (edits are interned), so a
    /// clean row's bytes are always still here.
    pub(super) source: Arc<str>,
    /// `starts[i]` is the first row of chunk `i`; the last entry is the row
    /// count. Rebuilt from the chunk lengths after each structural change.
    pub(super) starts: Vec<usize>,
    pub(super) columns: Vec<Chunks<CellRef>>,
    pub(super) meta: Chunks<RowMeta>,
    pub(super) header: bool,
    pub(super) col_types: Vec<ColType>,
    /// Rows (load-time indices) with an unclosed quote or a field count that
    /// differs from the first record's.
    pub(super) problems: Vec<usize>,
    /// Dirty rows are padded to at least this many fields: the widest record
    /// loaded, shifted by column inserts and deletes. Keeps an edited row of a
    /// file with trailing empty columns from losing them.
    pub(super) min_width: usize,
}

/// `total` rows split into the fewest chunks of at most [`CHUNK_ROWS`], with
/// sizes as even as possible.
fn even_sizes(total: usize) -> Vec<usize> {
    if total == 0 {
        return Vec::new();
    }
    let k = total.div_ceil(CHUNK_ROWS);
    let (base, extra) = (total / k, total % k);
    (0..k).map(|i| base + usize::from(i < extra)).collect()
}

/// Re-split the chunks in `range` into chunks of `sizes` (same total).
fn relayout<T>(chunks: &mut Chunks<T>, range: Range<usize>, sizes: &[usize]) {
    let start = range.start;
    let mut flat = chunks.drain(range).flatten();
    let pieces: Vec<Vec<T>> = sizes
        .iter()
        .map(|&s| {
            let mut v = Vec::with_capacity(s);
            v.extend(flat.by_ref().take(s));
            v
        })
        .collect();
    drop(flat);
    chunks.splice(start..start, pieces);
}

fn insert_into<T>(
    chunks: &mut Chunks<T>,
    ci: usize,
    off: usize,
    items: impl IntoIterator<Item = T>,
) {
    if chunks.is_empty() {
        chunks.push(Vec::new());
    }
    chunks[ci].splice(off..off, items);
}

fn drain_rows<T>(
    chunks: &mut Chunks<T>,
    (c0, o0): (usize, usize),
    (c1, o1): (usize, usize),
    n: usize,
) -> Vec<T> {
    let mut out = Vec::with_capacity(n);
    for (ci, chunk) in chunks.iter_mut().enumerate().take(c1 + 1).skip(c0) {
        let lo = if ci == c0 { o0 } else { 0 };
        let hi = if ci == c1 { o1 + 1 } else { chunk.len() };
        out.extend(chunk.drain(lo..hi));
    }
    out
}

/// Rows `start..` reordered so new row `start + i` is old row `perm[i]`.
fn permute<T: Copy>(chunks: &mut Chunks<T>, start: usize, perm: &[usize]) {
    let flat: Vec<T> = chunks.iter().flatten().copied().collect();
    let mut src = flat[..start]
        .iter()
        .copied()
        .chain(perm.iter().map(|&p| flat[p]));
    for slot in chunks.iter_mut().flatten() {
        *slot = src.next().expect("permutation covers every row");
    }
}

/// One row, positioned once so reading its cells costs no further searches.
#[derive(Clone, Copy)]
pub struct RowView<'a> {
    sheet: &'a Sheet,
    pool: &'a StringPool,
    ci: usize,
    off: usize,
}

impl<'a> RowView<'a> {
    pub fn meta(&self) -> &'a RowMeta {
        &self.sheet.meta[self.ci][self.off]
    }

    /// The cell reference at `col`, or `None` past the sheet's columns.
    pub fn cell_ref(&self, col: usize) -> Option<&'a CellRef> {
        self.sheet.columns.get(col).map(|c| &c[self.ci][self.off])
    }

    /// The text at `col` (`""` past the sheet's columns).
    pub fn cell(&self, col: usize) -> &'a str {
        self.cell_ref(col)
            .map_or("", |c| c.get(&self.sheet.source, self.pool))
    }

    /// Whether every cell in the row is empty.
    #[allow(dead_code)] // model API kept for multi-sheet / `.xlsx`
    pub fn is_empty(&self) -> bool {
        self.sheet
            .columns
            .iter()
            .all(|c| c[self.ci][self.off].is_empty())
    }
}

impl Sheet {
    /// An empty sheet over `source` (which its Span cells will point into).
    pub fn new(name: impl Into<String>, source: Arc<str>) -> Sheet {
        Sheet {
            name: name.into(),
            source,
            starts: vec![0],
            columns: Vec::new(),
            meta: Vec::new(),
            header: false,
            col_types: Vec::new(),
            problems: Vec::new(),
            min_width: 0,
        }
    }

    /// Assemble a sheet from parsed chunks. Every column must share `meta`'s
    /// chunk lengths.
    pub(super) fn from_parts(
        name: impl Into<String>,
        source: Arc<str>,
        columns: Vec<Chunks<CellRef>>,
        meta: Chunks<RowMeta>,
        problems: Vec<usize>,
    ) -> Sheet {
        debug_assert!(columns.iter().all(|c| {
            c.len() == meta.len() && c.iter().zip(&meta).all(|(a, b)| a.len() == b.len())
        }));
        let width = columns.len();
        let mut sheet = Sheet {
            col_types: vec![ColType::Text; width],
            columns,
            meta,
            problems,
            min_width: width,
            ..Sheet::new(name, source)
        };
        sheet.rebuild_starts();
        sheet
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    /// Used rows (rows that have storage).
    pub fn rows(&self) -> usize {
        *self.starts.last().unwrap_or(&0)
    }

    /// Used columns.
    pub fn cols(&self) -> usize {
        self.columns.len()
    }

    pub fn header(&self) -> bool {
        self.header
    }

    pub fn problems(&self) -> &[usize] {
        &self.problems
    }

    pub fn col_type(&self, col: usize) -> ColType {
        self.col_types.get(col).copied().unwrap_or_default()
    }

    fn rebuild_starts(&mut self) {
        self.starts.clear();
        self.starts.push(0);
        let mut total = 0;
        for chunk in &self.meta {
            total += chunk.len();
            self.starts.push(total);
        }
    }

    /// `(chunk, offset)` of `row`, which must be in range.
    fn locate(&self, row: usize) -> (usize, usize) {
        debug_assert!(row < self.rows());
        let ci = self.starts.partition_point(|&s| s <= row) - 1;
        (ci, row - self.starts[ci])
    }

    pub fn cell_ref(&self, row: usize, col: usize) -> Option<&CellRef> {
        if row >= self.rows() {
            return None;
        }
        let column = self.columns.get(col)?;
        let (ci, off) = self.locate(row);
        Some(&column[ci][off])
    }

    /// The text at `(row, col)`; `""` anywhere outside the used area.
    pub fn cell<'a>(&'a self, pool: &'a StringPool, row: usize, col: usize) -> &'a str {
        self.cell_ref(row, col)
            .map_or("", |c| c.get(&self.source, pool))
    }

    pub fn row_meta(&self, row: usize) -> Option<&RowMeta> {
        (row < self.rows()).then(|| {
            let (ci, off) = self.locate(row);
            &self.meta[ci][off]
        })
    }

    pub(super) fn cell_mut(&mut self, row: usize, col: usize) -> &mut CellRef {
        let (ci, off) = self.locate(row);
        &mut self.columns[col][ci][off]
    }

    pub(super) fn meta_mut(&mut self, row: usize) -> &mut RowMeta {
        let (ci, off) = self.locate(row);
        &mut self.meta[ci][off]
    }

    /// Visit `rows` in order, each positioned once (no per-cell searches).
    pub fn for_each_row<'a>(
        &'a self,
        pool: &'a StringPool,
        rows: Range<usize>,
        mut f: impl FnMut(usize, RowView<'a>),
    ) {
        let end = rows.end.min(self.rows());
        if rows.start >= end {
            return;
        }
        let (mut ci, mut off) = self.locate(rows.start);
        let mut row = rows.start;
        while row < end {
            let len = self.meta[ci].len();
            while off < len && row < end {
                f(
                    row,
                    RowView {
                        sheet: self,
                        pool,
                        ci,
                        off,
                    },
                );
                off += 1;
                row += 1;
            }
            ci += 1;
            off = 0;
        }
    }

    /// Visit the cells of column `col` over `rows`, in order, until `f`
    /// breaks. A column past the sheet's width has no cells to visit.
    pub fn try_for_each_in_column<'a>(
        &'a self,
        col: usize,
        rows: Range<usize>,
        mut f: impl FnMut(usize, &'a CellRef) -> ControlFlow<()>,
    ) {
        let end = rows.end.min(self.rows());
        let Some(column) = self.columns.get(col) else {
            return;
        };
        if rows.start >= end {
            return;
        }
        let (ci0, mut off) = self.locate(rows.start);
        let mut row = rows.start;
        for chunk in &column[ci0..] {
            for cell in &chunk[off..] {
                if row >= end || f(row, cell).is_break() {
                    return;
                }
                row += 1;
            }
            off = 0;
        }
    }

    /// The last row the writer must emit: the last that is clean (an original
    /// record, kept even when blank) or holds any value. Empty rows after it —
    /// visited, edited and cleared, or never filled — are not part of the file.
    pub fn last_kept_row(&self) -> Option<usize> {
        for ci in (0..self.meta.len()).rev() {
            for off in (0..self.meta[ci].len()).rev() {
                if self.meta[ci][off].is_clean()
                    || self.columns.iter().any(|c| !c[ci][off].is_empty())
                {
                    return Some(self.starts[ci] + off);
                }
            }
        }
        None
    }

    /// Fields a re-serialized row is padded to: [`Self::min_width`] or, if a
    /// value sits further right, up to and including that column.
    pub fn write_width(&self) -> usize {
        for col in (self.min_width..self.columns.len()).rev() {
            if self.columns[col].iter().flatten().any(|c| !c.is_empty()) {
                return col + 1;
            }
        }
        self.min_width.min(self.columns.len())
    }

    // ---- structural changes -------------------------------------------------

    /// An all-Empty column matching the current chunk layout.
    pub(super) fn empty_column(&self) -> Chunks<CellRef> {
        self.meta
            .iter()
            .map(|m| vec![CellRef::EMPTY; m.len()])
            .collect()
    }

    /// Insert `metas.len()` rows before `at` (≤ rows). `cells`, when given,
    /// holds each column's values for the new rows; otherwise they are Empty.
    pub(super) fn insert_rows(
        &mut self,
        at: usize,
        cells: Option<Vec<Vec<CellRef>>>,
        metas: Vec<RowMeta>,
    ) {
        let n = metas.len();
        if n == 0 {
            return;
        }
        assert!(at <= self.rows(), "insert past the end of the sheet");
        let (ci, off) = if self.meta.is_empty() {
            (0, 0)
        } else if at == self.rows() {
            let last = self.meta.len() - 1;
            (last, self.meta[last].len())
        } else {
            self.locate(at)
        };
        match cells {
            Some(cells) => {
                debug_assert_eq!(cells.len(), self.columns.len());
                for (col, values) in self.columns.iter_mut().zip(cells) {
                    debug_assert_eq!(values.len(), n);
                    insert_into(col, ci, off, values);
                }
            }
            None => {
                for col in &mut self.columns {
                    insert_into(col, ci, off, std::iter::repeat_n(CellRef::EMPTY, n));
                }
            }
        }
        insert_into(&mut self.meta, ci, off, metas);
        let len = self.meta[ci].len();
        if len > MAX_CHUNK_ROWS {
            let sizes = even_sizes(len);
            for col in &mut self.columns {
                relayout(col, ci..ci + 1, &sizes);
            }
            relayout(&mut self.meta, ci..ci + 1, &sizes);
        }
        self.rebuild_starts();
    }

    /// Remove rows `at..at + n` (in range), returning each column's removed
    /// cells and the removed metadata, ready for [`Self::insert_rows`].
    pub(super) fn remove_rows(&mut self, at: usize, n: usize) -> (Vec<Vec<CellRef>>, Vec<RowMeta>) {
        if n == 0 {
            return (vec![Vec::new(); self.columns.len()], Vec::new());
        }
        assert!(at + n <= self.rows(), "remove past the end of the sheet");
        let first = self.locate(at);
        let last = self.locate(at + n - 1);
        let cells = self
            .columns
            .iter_mut()
            .map(|col| drain_rows(col, first, last, n))
            .collect();
        let metas = drain_rows(&mut self.meta, first, last, n);
        // Merge what got small with its neighbours (and drop emptied chunks).
        let (c0, c1) = (first.0, last.0);
        if (c0..=c1).any(|ci| self.meta[ci].len() < MIN_CHUNK_ROWS) {
            let region = c0.saturating_sub(1)..(c1 + 2).min(self.meta.len());
            let total = self.meta[region.clone()].iter().map(Vec::len).sum();
            let sizes = even_sizes(total);
            for col in &mut self.columns {
                relayout(col, region.clone(), &sizes);
            }
            relayout(&mut self.meta, region, &sizes);
        }
        self.rebuild_starts();
        (cells, metas)
    }

    /// Grow to `rows` rows with new (non-original) empty rows.
    pub(super) fn grow_rows(&mut self, rows: usize) {
        if rows > self.rows() {
            let n = rows - self.rows();
            self.insert_rows(self.rows(), None, vec![RowMeta::default(); n]);
        }
    }

    /// Drop rows from `rows` on.
    pub(super) fn truncate_rows(&mut self, rows: usize) {
        if rows < self.rows() {
            self.remove_rows(rows, self.rows() - rows);
        }
    }

    /// Grow to `cols` columns of Empty cells.
    pub(super) fn grow_cols(&mut self, cols: usize) {
        while self.columns.len() < cols {
            self.columns.push(self.empty_column());
            self.col_types.push(ColType::Text);
        }
    }

    pub(super) fn truncate_cols(&mut self, cols: usize) {
        self.columns.truncate(cols);
        self.col_types.truncate(cols);
    }

    /// Insert whole columns before `at` (≤ cols). Each is regrouped to the
    /// current chunk layout: a column removed earlier and put back by undo
    /// was chunked as the rows were then, and row edits since (undone, but
    /// splitting or merging chunks on the way) may have changed that.
    pub(super) fn insert_cols(
        &mut self,
        at: usize,
        cols: Vec<Chunks<CellRef>>,
        types: Vec<ColType>,
    ) {
        debug_assert_eq!(cols.len(), types.len());
        let lens: Vec<usize> = self.meta.iter().map(Vec::len).collect();
        let cols = cols.into_iter().map(|c| rechunk(c, &lens));
        self.columns.splice(at..at, cols);
        self.col_types.splice(at..at, types);
    }

    pub(super) fn remove_cols(
        &mut self,
        at: usize,
        n: usize,
    ) -> (Vec<Chunks<CellRef>>, Vec<ColType>) {
        let cols = self.columns.drain(at..at + n).collect();
        let types = self.col_types.drain(at..at + n).collect();
        (cols, types)
    }

    /// Reorder rows `start..` so new row `start + i` is old row `perm[i]`.
    /// Reorder rows `start..start + perm.len()` so new row `start + i` is old
    /// row `perm[i]` (every `perm` entry lies in that range).
    pub(super) fn permute_rows(&mut self, start: usize, perm: &[usize]) {
        debug_assert!(start + perm.len() <= self.rows());
        if start + perm.len() == self.rows() {
            for col in &mut self.columns {
                permute(col, start, perm);
            }
            permute(&mut self.meta, start, perm);
            return;
        }
        // A window (moving a row or two): touch only the rows in it.
        let at: Vec<(usize, usize)> = (start..start + perm.len())
            .map(|r| self.locate(r))
            .collect();
        let from: Vec<(usize, usize)> = perm.iter().map(|&p| self.locate(p)).collect();
        fn shuffle<T: Copy>(
            chunks: &mut Chunks<T>,
            at: &[(usize, usize)],
            from: &[(usize, usize)],
        ) {
            let vals: Vec<T> = from.iter().map(|&(ci, off)| chunks[ci][off]).collect();
            for (&(ci, off), v) in at.iter().zip(vals) {
                chunks[ci][off] = v;
            }
        }
        for col in &mut self.columns {
            shuffle(col, &at, &from);
        }
        shuffle(&mut self.meta, &at, &from);
    }

    /// Every row's dirty flag, as a bitset (for undoing a column operation).
    pub(super) fn dirty_bits(&self) -> Vec<u64> {
        let mut bits = vec![0u64; self.rows().div_ceil(64)];
        for (i, m) in self.meta.iter().flatten().enumerate() {
            if m.is_dirty() {
                bits[i / 64] |= 1 << (i % 64);
            }
        }
        bits
    }

    pub(super) fn restore_dirty_bits(&mut self, bits: &[u64]) {
        for (i, m) in self.meta.iter_mut().flatten().enumerate() {
            m.set_dirty(bits[i / 64] & (1 << (i % 64)) != 0);
        }
    }

    pub(super) fn mark_all_dirty(&mut self) {
        for m in self.meta.iter_mut().flatten() {
            m.set_dirty(true);
        }
    }

    /// Visit every cell mutably (for pool compaction).
    pub(super) fn for_each_cell_mut(&mut self, mut f: impl FnMut(&mut CellRef)) {
        self.columns.iter_mut().flatten().flatten().for_each(&mut f);
    }

    /// Check the shared-boundary invariant (tests and debug assertions).
    #[cfg(test)]
    pub(super) fn check_layout(&self) {
        assert_eq!(self.starts.len(), self.meta.len() + 1);
        for col in &self.columns {
            assert_eq!(col.len(), self.meta.len());
            for (a, b) in col.iter().zip(&self.meta) {
                assert_eq!(a.len(), b.len());
            }
        }
        for chunk in &self.meta {
            assert!(!chunk.is_empty(), "empty chunk left behind");
            assert!(chunk.len() <= MAX_CHUNK_ROWS);
        }
        assert_eq!(self.col_types.len(), self.columns.len());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbered(rows: usize, cols: usize, pool: &mut StringPool) -> Sheet {
        let mut sheet = Sheet::new("t", Arc::from(""));
        sheet.grow_cols(cols);
        sheet.grow_rows(rows);
        for r in 0..rows {
            for c in 0..cols {
                *sheet.cell_mut(r, c) = CellRef::from_value(&format!("{r}:{c}"), pool);
            }
        }
        sheet
    }

    fn check_numbered(sheet: &Sheet, pool: &StringPool, rows: &[usize]) {
        assert_eq!(sheet.rows(), rows.len());
        for (i, &r) in rows.iter().enumerate() {
            assert_eq!(sheet.cell(pool, i, 1), format!("{r}:1"), "row {i}");
        }
    }

    #[test]
    fn grid_even_sizes() {
        assert!(even_sizes(0).is_empty());
        assert_eq!(even_sizes(10), vec![10]);
        assert_eq!(even_sizes(4097), vec![2049, 2048]);
        assert!(even_sizes(1_000_000).iter().all(|&s| s <= CHUNK_ROWS));
    }

    #[test]
    fn grid_sheet_insert_remove_keeps_layout() {
        let mut pool = StringPool::new();
        let mut sheet = numbered(20_000, 3, &mut pool);
        sheet.check_layout();
        let mut model: Vec<usize> = (0..20_000).collect();

        // Big insert in the middle splits the chunk.
        sheet.insert_rows(5000, None, vec![RowMeta::default(); 9000]);
        sheet.check_layout();
        assert_eq!(sheet.cell(&pool, 5000, 1), "");
        assert_eq!(sheet.cell(&pool, 14_000, 1), "5000:1");

        // Remove them again, plus a span crossing chunk boundaries.
        let (cells, metas) = sheet.remove_rows(5000, 9000);
        assert_eq!(metas.len(), 9000);
        assert!(cells.iter().all(|c| c.iter().all(|x| x.is_empty())));
        sheet.check_layout();
        check_numbered(&sheet, &pool, &model);

        let (cells, metas) = sheet.remove_rows(3000, 7000);
        model.drain(3000..10_000);
        sheet.check_layout();
        check_numbered(&sheet, &pool, &model);

        // Re-inserting the removed data restores the original.
        sheet.insert_rows(3000, Some(cells), metas);
        sheet.check_layout();
        check_numbered(&sheet, &pool, &(0..20_000).collect::<Vec<_>>());

        // Remove everything.
        sheet.remove_rows(0, 20_000);
        sheet.check_layout();
        assert_eq!(sheet.rows(), 0);
        assert_eq!(sheet.cell(&pool, 0, 0), "");
    }

    #[test]
    fn grid_sheet_iteration_and_permute() {
        let mut pool = StringPool::new();
        let mut sheet = numbered(10_000, 2, &mut pool);
        let mut seen = Vec::new();
        sheet.try_for_each_in_column(1, 4090..4100, |r, c| {
            seen.push((r, c.get(&sheet.source, &pool).to_string()));
            ControlFlow::Continue(())
        });
        assert_eq!(seen.len(), 10);
        assert_eq!(seen[0], (4090, "4090:1".to_string()));
        let mut rows = 0;
        sheet.for_each_row(&pool, 8190..20_000, |r, view| {
            assert_eq!(view.cell(0), format!("{r}:0"));
            assert_eq!(view.cell(9), "");
            rows += 1;
        });
        assert_eq!(rows, 10_000 - 8190);

        // Reverse everything after row 1.
        let perm: Vec<usize> = (1..10_000).rev().collect();
        sheet.permute_rows(1, &perm);
        let mut expect = vec![0];
        expect.extend((1..10_000).rev());
        check_numbered(&sheet, &pool, &expect);
    }
}
