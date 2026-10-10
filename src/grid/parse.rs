//! Loading: bytes → one-sheet [`Workbook`].
//!
//! The scanner ([`next_record`]) is hand-rolled on `memchr` rather than built
//! on `csv-core`, because the model needs things csv-core's API doesn't give:
//! the exact source byte range of every field (so a cell can be a zero-copy
//! Span) and of every record and its terminator (so a clean row is saved as
//! its original bytes), and blank lines as rows (csv-core skips them, which
//! would lose them on save). Its quoting semantics follow csv-core's, and the
//! tests check the two agree field-for-field on generated input.
//!
//! Malformed input never fails a load: an unclosed quote runs to the end of
//! the file, a ragged row is padded with Empty, and both are listed in
//! [`Sheet::problems`].

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use memchr::{memchr, memchr2, memchr3};
use rustc_hash::FxHashSet;

use super::cell::{CellKind, CellRef};
use super::dialect::{self, Dialect, Encoding, Escape};
use super::intern::{StringPool, hash_str};
use super::model::{Format, Workbook};
use super::sheet::{CHUNK_ROWS, Chunks, RowMeta, Sheet, Terminator};

/// Rows sampled before deciding which columns are low-cardinality.
pub const CARDINALITY_SAMPLE_ROWS: usize = 10_000;
/// A column with fewer distinct values than this share of the sampled rows is
/// interned wholesale (1/20 = 5%).
const CARDINALITY_DIVISOR: usize = 20;
/// First window [`parse_prefix`] decodes; it grows ×4 until enough rows fit.
const PREFIX_WINDOW: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    /// The `cancel` flag was raised (the buffer was closed while loading).
    Cancelled,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::Cancelled => write!(f, "load cancelled"),
        }
    }
}

impl std::error::Error for ParseError {}

/// The bytes that matter to the scanner.
#[derive(Clone, Copy, Debug)]
pub struct Syntax {
    pub delimiter: u8,
    pub quote: u8,
    /// The escape byte inside quoted fields (`\`), if not just `""`.
    pub escape: Option<u8>,
}

impl Syntax {
    pub fn new(delimiter: u8, quote: u8, escape: Escape) -> Syntax {
        Syntax {
            delimiter,
            quote,
            escape: match escape {
                Escape::Doubled => None,
                Escape::Backslash => Some(b'\\'),
            },
        }
    }

    pub fn of(d: &Dialect) -> Syntax {
        Syntax::new(d.delimiter, d.quote, d.escape)
    }
}

/// One field's source range. When `escaped` is false the value is exactly
/// `src[start..end]` (an unquoted field, or a quoted one's inner bytes with
/// nothing to undo). When true, `src[start..end]` is the raw field from its
/// opening quote through any text after the closing one; [`unescape`] gives
/// the value.
#[derive(Clone, Copy, Debug)]
pub struct FieldSpan {
    pub start: usize,
    pub end: usize,
    pub escaped: bool,
}

/// One record's source range. `src[start..content_end]` is the record,
/// `src[content_end..next]` its terminator.
#[derive(Clone, Copy, Debug)]
pub struct RecordSpan {
    pub start: usize,
    pub content_end: usize,
    pub next: usize,
    pub term: Terminator,
    /// A quoted field ran to the end of the input without closing.
    pub unclosed: bool,
}

fn terminator_at(src: &[u8], p: usize) -> (Terminator, usize) {
    match src[p] {
        b'\n' => (Terminator::Lf, p + 1),
        _ if src.get(p + 1) == Some(&b'\n') => (Terminator::CrLf, p + 2),
        _ => (Terminator::Cr, p + 1),
    }
}

/// Scan the record starting at `pos`, filling `fields`. `None` at the end of
/// input. A blank line is a record with no fields. Records tile the input:
/// each starts where the previous one's terminator ended.
pub fn next_record(
    src: &[u8],
    pos: usize,
    syn: &Syntax,
    fields: &mut Vec<FieldSpan>,
) -> Option<RecordSpan> {
    let len = src.len();
    if pos >= len {
        return None;
    }
    fields.clear();
    let record = |content_end: usize, unclosed: bool| {
        let (term, next) = if content_end >= len {
            (Terminator::None, len)
        } else {
            terminator_at(src, content_end)
        };
        RecordSpan {
            start: pos,
            content_end,
            next,
            term,
            unclosed,
        }
    };
    if matches!(src[pos], b'\n' | b'\r') {
        return Some(record(pos, false));
    }
    let field_end =
        |from: usize| memchr3(syn.delimiter, b'\n', b'\r', &src[from..]).map_or(len, |i| from + i);
    let mut p = pos;
    loop {
        if p < len && src[p] == syn.quote {
            let open = p;
            let mut q = p + 1;
            let mut escaped = false;
            let close = loop {
                let hit = match syn.escape {
                    Some(e) => memchr2(syn.quote, e, &src[q..]),
                    None => memchr(syn.quote, &src[q..]),
                };
                let Some(i) = hit else { break None };
                let k = q + i;
                if src[k] != syn.quote {
                    // Escape byte: the next byte is literal.
                    if k + 1 >= len {
                        break None;
                    }
                    escaped = true;
                    q = k + 2;
                } else if src.get(k + 1) == Some(&syn.quote) {
                    escaped = true;
                    q = k + 2;
                } else {
                    break Some(k);
                }
            };
            let Some(close) = close else {
                fields.push(FieldSpan {
                    start: open,
                    end: len,
                    escaped: true,
                });
                return Some(record(len, true));
            };
            // Text after the closing quote (`"ab"c`) is kept, as csv-core does.
            let end = field_end(close + 1);
            if escaped || end > close + 1 {
                fields.push(FieldSpan {
                    start: open,
                    end,
                    escaped: true,
                });
            } else {
                fields.push(FieldSpan {
                    start: open + 1,
                    end: close,
                    escaped: false,
                });
            }
            p = end;
        } else {
            let end = field_end(p);
            fields.push(FieldSpan {
                start: p,
                end,
                escaped: false,
            });
            p = end;
        }
        if p >= len || src[p] != syn.delimiter {
            return Some(record(p, false));
        }
        p += 1;
    }
}

/// The value of an escaped field (see [`FieldSpan`]); `raw` starts at the
/// opening quote. Undoes `""` and escape bytes; anything after the closing
/// quote is appended as is. Always valid UTF-8: only ASCII bytes are dropped.
pub fn unescape(raw: &str, syn: &Syntax, out: &mut String) {
    out.clear();
    let b = raw.as_bytes();
    let (mut i, mut run) = (1, 1);
    while i < b.len() {
        let c = b[i];
        if c != syn.quote && Some(c) == syn.escape {
            out.push_str(&raw[run..i]);
            match raw[i + 1..].chars().next() {
                Some(ch) => {
                    out.push(ch);
                    i += 1 + ch.len_utf8();
                }
                None => i += 1,
            }
            run = i;
        } else if c == syn.quote {
            out.push_str(&raw[run..i]);
            if b.get(i + 1) == Some(&syn.quote) {
                out.push(syn.quote as char);
                i += 2;
                run = i;
            } else {
                out.push_str(&raw[i + 1..]);
                return;
            }
        } else {
            i += 1;
        }
    }
    out.push_str(&raw[run..]);
}

/// Bounds for [`parse_sheet`].
pub(super) struct Limits {
    pub max_rows: Option<usize>,
    /// The input is a cut-off window: drop a final record that has no
    /// terminator (it may continue past the cut).
    pub drop_unterminated_tail: bool,
}

/// The cell for a parsed value: Empty, Inline, a Span into the source when
/// `span_at` says the value is verbatim there (and the column isn't being
/// interned), else Interned.
fn store(value: &str, span_at: Option<usize>, intern_long: bool, pool: &mut StringPool) -> CellRef {
    if value.is_empty() {
        return CellRef::EMPTY;
    }
    if let Some(cell) = CellRef::inline(value) {
        return cell;
    }
    if !intern_long && let Some(cell) = span_at.and_then(|at| CellRef::span(at, value.len())) {
        return cell;
    }
    CellRef::interned(pool.intern(value))
}

/// Distinct-value sampling for the low-cardinality optimisation: in columns
/// like `country` or `status`, interning every long cell collapses them to a
/// few dozen pool strings, so sort and filter compare ids instead of bytes.
struct Cardinality {
    /// FxHash of each distinct non-empty value per column, while sampling.
    seen: Vec<FxHashSet<u64>>,
    /// Columns decided low-cardinality: intern every long cell.
    intern: Vec<bool>,
    sampling: bool,
}

impl Cardinality {
    /// Decide after the sample, converting the decided columns' Spans so far.
    fn decide(&mut self, columns: &mut [Chunks<CellRef>], source: &str, pool: &mut StringPool) {
        self.sampling = false;
        let seen = std::mem::take(&mut self.seen);
        for (c, set) in seen.iter().enumerate() {
            let low = set.len() * CARDINALITY_DIVISOR < CARDINALITY_SAMPLE_ROWS;
            self.intern[c] = low;
            if low {
                for cell in columns[c].iter_mut().flatten() {
                    if let CellKind::Span { offset, len } = cell.kind() {
                        *cell = CellRef::interned(pool.intern(&source[offset..offset + len]));
                    }
                }
            }
        }
    }
}

/// Parse `source` (UTF-8, BOM stripped) into a sheet. Checks `cancel` and
/// publishes bytes consumed to `progress` once per chunk of rows.
pub(super) fn parse_sheet(
    source: Arc<str>,
    dialect: &Dialect,
    pool: &mut StringPool,
    limits: &Limits,
    cancel: Option<&AtomicBool>,
    progress: Option<&AtomicUsize>,
) -> Result<Sheet, ParseError> {
    let src = source.as_bytes();
    let syn = Syntax::of(dialect);
    let mut fields = Vec::new();
    let mut scratch = String::new();
    let mut columns: Vec<Chunks<CellRef>> = Vec::new();
    let mut meta: Chunks<RowMeta> = Vec::new();
    let mut problems = Vec::new();
    let mut expected_fields: Option<usize> = None;
    let mut card = Cardinality {
        seen: Vec::new(),
        intern: Vec::new(),
        sampling: true,
    };
    let mut rows = 0usize;
    let mut pos = 0usize;

    while let Some(rec) = next_record(src, pos, &syn, &mut fields) {
        if limits.max_rows.is_some_and(|m| rows >= m)
            || (limits.drop_unterminated_tail && rec.term == Terminator::None)
        {
            break;
        }
        // Open a new chunk (for every column) when the current one is full.
        if meta.last().is_none_or(|c| c.len() == CHUNK_ROWS) {
            if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                return Err(ParseError::Cancelled);
            }
            if let Some(p) = progress {
                p.store(pos, Ordering::Relaxed);
            }
            meta.push(Vec::with_capacity(CHUNK_ROWS));
            for col in &mut columns {
                col.push(Vec::with_capacity(CHUNK_ROWS));
            }
        }
        // A wider record adds columns, Empty for every earlier row.
        while columns.len() < fields.len() {
            columns.push(
                meta.iter()
                    .map(|m| {
                        let mut v = Vec::with_capacity(m.capacity());
                        v.resize(m.len(), CellRef::EMPTY);
                        v
                    })
                    .collect(),
            );
            card.intern.push(false);
            if card.sampling {
                card.seen.push(FxHashSet::default());
            }
        }

        if rec.unclosed {
            problems.push(rows);
        } else if !fields.is_empty() {
            match expected_fields {
                None => expected_fields = Some(fields.len()),
                Some(n) if n != fields.len() => problems.push(rows),
                Some(_) => {}
            }
        }

        for (c, f) in fields.iter().enumerate() {
            let (value, span_at) = if f.escaped {
                unescape(&source[f.start..f.end], &syn, &mut scratch);
                (scratch.as_str(), None)
            } else {
                (&source[f.start..f.end], Some(f.start))
            };
            if card.sampling && !value.is_empty() {
                card.seen[c].insert(hash_str(value));
            }
            let cell = store(value, span_at, card.intern[c], pool);
            columns[c]
                .last_mut()
                .expect("chunk opened above")
                .push(cell);
        }
        for col in &mut columns[fields.len()..] {
            col.last_mut()
                .expect("chunk opened above")
                .push(CellRef::EMPTY);
        }
        meta.last_mut()
            .expect("chunk opened above")
            .push(RowMeta::original(
                rec.start,
                rec.content_end - rec.start,
                rec.term,
            ));

        rows += 1;
        pos = rec.next;
        if card.sampling && rows == CARDINALITY_SAMPLE_ROWS {
            card.decide(&mut columns, &source, pool);
        }
    }
    if let Some(p) = progress {
        p.store(pos, Ordering::Relaxed);
    }
    // The last chunk was allocated for a full CHUNK_ROWS.
    for chunk in columns.iter_mut().filter_map(|c| c.last_mut()) {
        chunk.shrink_to_fit();
    }
    if let Some(chunk) = meta.last_mut() {
        chunk.shrink_to_fit();
    }
    Ok(Sheet::from_parts("Sheet1", source, columns, meta, problems))
}

fn workbook(dialect: Dialect, pool: StringPool, mut sheet: Sheet, complete: bool) -> Workbook {
    sheet.header = dialect.header;
    let mut wb = Workbook::from_parts(Format::Csv(dialect), pool, vec![sheet], complete);
    wb.infer_types(0);
    wb
}

/// [`parse`] on this thread, with no cancelling or progress to report.
pub fn parse_now(bytes: Vec<u8>, ext_hint: Option<&str>) -> Result<Workbook, ParseError> {
    parse(
        bytes,
        ext_hint,
        &AtomicBool::new(false),
        &AtomicUsize::new(0),
    )
}

/// Load a whole file as a one-sheet workbook. Only cancellation fails;
/// malformed data still loads (see the module doc). `ext_hint` is the file
/// extension, which biases delimiter detection (`tsv` → tab, `psv` → pipe).
/// `progress` receives bytes consumed (of the transcoded text).
pub fn parse(
    bytes: Vec<u8>,
    ext_hint: Option<&str>,
    cancel: &AtomicBool,
    progress: &AtomicUsize,
) -> Result<Workbook, ParseError> {
    let (encoding, bom) = dialect::detect_encoding(&bytes);
    let (source, encoding, bom) = dialect::decode(bytes, encoding, bom);
    // An empty file (the new-file flow) gets conventional newline-terminated
    // records once something is typed.
    let ends_with_newline = source.is_empty() || source.ends_with(['\n', '\r']);
    let dialect = dialect::sniff(&source, encoding, bom, ends_with_newline, ext_hint);
    let mut pool = StringPool::new();
    let limits = Limits {
        max_rows: None,
        drop_unterminated_tail: false,
    };
    let sheet = parse_sheet(
        source,
        &dialect,
        &mut pool,
        &limits,
        Some(cancel),
        Some(progress),
    )?;
    // "Ends with a newline" means the last *record* had a terminator: a file
    // ending inside an unclosed quote ends in a newline that belongs to the
    // field, and appending another would change the file.
    let mut dialect = dialect;
    if let Some(last) = sheet.rows().checked_sub(1).and_then(|r| sheet.row_meta(r)) {
        dialect.trailing_newline = last.term != Terminator::None;
    }
    Ok(workbook(dialect, pool, sheet, true))
}

/// The first `max_rows` records only, for an instant first paint while
/// [`parse`] runs. Decodes just a window at the front of the file (growing it
/// until enough whole records fit), so it costs the same on any file size.
/// The result is marked incomplete ([`Workbook::is_complete`]) and must not be
/// saved.
pub fn parse_prefix(bytes: &[u8], ext_hint: Option<&str>, max_rows: usize) -> Workbook {
    let (encoding, bom) = dialect::detect_encoding(bytes);
    let body = &bytes[dialect::bom_len(encoding, bom)..];
    let ends_with_newline = body.is_empty()
        || match encoding {
            Encoding::Utf16Le => body.ends_with(b"\n\0") || body.ends_with(b"\r\0"),
            Encoding::Utf16Be => body.ends_with(b"\0\n") || body.ends_with(b"\0\r"),
            _ => body.ends_with(b"\n") || body.ends_with(b"\r"),
        };
    let mut window = PREFIX_WINDOW;
    loop {
        let mut cut = window.min(body.len());
        if matches!(encoding, Encoding::Utf16Le | Encoding::Utf16Be) {
            cut &= !1;
        }
        let truncated = cut < body.len();
        let source = dialect::decode_slice(&body[..cut], encoding);
        let dialect = dialect::sniff(&source, encoding, bom, ends_with_newline, ext_hint);
        let mut pool = StringPool::new();
        let limits = Limits {
            max_rows: Some(max_rows),
            drop_unterminated_tail: truncated,
        };
        let sheet = parse_sheet(source, &dialect, &mut pool, &limits, None, None)
            .expect("no cancel flag, so parsing can't fail");
        if !truncated || sheet.rows() >= max_rows {
            let complete = !truncated && sheet.rows() < max_rows;
            return workbook(dialect, pool, sheet, complete);
        }
        window = window.saturating_mul(4);
    }
}

#[cfg(test)]
mod tests {
    use super::super::testgen::{GenOpts, Rng, gen_csv};
    use super::*;

    fn load(s: &str) -> Workbook {
        parse(
            s.as_bytes().to_vec(),
            None,
            &AtomicBool::new(false),
            &AtomicUsize::new(0),
        )
        .unwrap()
    }

    fn row(wb: &Workbook, r: usize) -> Vec<&str> {
        (0..wb.used_cols(0)).map(|c| wb.cell(0, r, c)).collect()
    }

    #[test]
    fn grid_parse_basic_quoting() {
        let wb = load("a,\"b,c\",\"say \"\"hi\"\"\"\n\"multi\nline\",x,\"\"\n");
        assert_eq!(wb.used_rows(0), 2);
        assert_eq!(row(&wb, 0), ["a", "b,c", "say \"hi\""]);
        assert_eq!(row(&wb, 1), ["multi\nline", "x", ""]);
        assert!(wb.sheet(0).problems().is_empty());
    }

    #[test]
    fn grid_parse_blank_lines_ragged_and_unclosed() {
        let wb = load("a,b,c\n\n1,2\n3,4,5,6\n\"open,x\ny\n");
        assert_eq!(wb.used_rows(0), 5);
        assert_eq!(wb.used_cols(0), 4);
        assert_eq!(row(&wb, 1), ["", "", "", ""]);
        assert_eq!(row(&wb, 2), ["1", "2", "", ""]);
        assert_eq!(row(&wb, 3), ["3", "4", "5", "6"]);
        assert_eq!(wb.cell(0, 4, 0), "open,x\ny\n");
        // Ragged rows 2 and 3, unclosed quote at row 4; the blank line is fine.
        assert_eq!(wb.sheet(0).problems(), &[2, 3, 4]);
    }

    #[test]
    fn grid_parse_quote_edge_cases_match_csv_core() {
        // A quote inside an unquoted field is literal; text after a closing
        // quote is appended.
        let wb = load("ab\"c,\"xy\"z,\"\"\"\"\n");
        assert_eq!(row(&wb, 0), ["ab\"c", "xyz", "\""]);
    }

    #[test]
    fn grid_parse_backslash_escape() {
        let wb = load("id,text\n1,\"say \\\"hi\\\"\"\n2,\"back\\\\slash\"\n");
        let Format::Csv(d) = &wb.format;
        assert_eq!(d.escape, Escape::Backslash);
        assert_eq!(wb.cell(0, 1, 1), "say \"hi\"");
        assert_eq!(wb.cell(0, 2, 1), "back\\slash");
    }

    #[test]
    fn grid_parse_cell_storage_kinds() {
        let src = "short,a much longer value,\"quoted long value\",\"esc \"\"long\"\" value\"\n";
        let wb = load(src);
        let sheet = wb.sheet(0);
        let kind = |c| sheet.cell_ref(0, c).unwrap().kind();
        assert_eq!(kind(0), CellKind::Inline);
        assert!(matches!(kind(1), CellKind::Span { offset: 6, len: 19 }));
        assert!(matches!(kind(2), CellKind::Span { offset: 27, .. }));
        assert!(matches!(kind(3), CellKind::Interned(_)));
        assert_eq!(wb.cell(0, 0, 2), "quoted long value");
        assert_eq!(wb.cell(0, 0, 3), "esc \"long\" value");
    }

    #[test]
    fn grid_parse_low_cardinality_columns_are_interned() {
        let statuses = ["pending review", "approved by admin", "rejected outright"];
        let mut s = String::from("id,status,note\n");
        for i in 0..20_000 {
            s.push_str(&format!("{i},{},unique note number {i}\n", statuses[i % 3]));
        }
        let wb = load(&s);
        let sheet = wb.sheet(0);
        let mut ids = FxHashSet::default();
        let mut spans = 0;
        sheet.try_for_each_in_column(1, 1..sheet.rows(), |_, c| {
            match c.kind() {
                CellKind::Interned(id) => {
                    ids.insert(id);
                }
                _ => spans += 1,
            }
            std::ops::ControlFlow::Continue(())
        });
        assert_eq!(
            spans, 0,
            "every status cell is interned, before and after the sample"
        );
        assert_eq!(ids.len(), 3);
        // The high-cardinality column stays as zero-copy spans.
        assert!(matches!(
            sheet.cell_ref(15_000, 2).unwrap().kind(),
            CellKind::Span { .. }
        ));
        // Only the 3 statuses (plus the header) went into the pool.
        assert!(wb.pool().len() <= 4, "pool has {}", wb.pool().len());
    }

    #[test]
    fn grid_parse_cancel_and_progress() {
        let s = "a,b\n".repeat(10_000);
        let cancel = AtomicBool::new(true);
        let progress = AtomicUsize::new(0);
        assert_eq!(
            parse(s.clone().into_bytes(), None, &cancel, &progress).err(),
            Some(ParseError::Cancelled)
        );
        cancel.store(false, Ordering::Relaxed);
        parse(s.clone().into_bytes(), None, &cancel, &progress).unwrap();
        assert_eq!(progress.load(Ordering::Relaxed), s.len());
    }

    #[test]
    fn grid_parse_prefix() {
        let mut s = String::from("name,n\n");
        for i in 0..100_000 {
            s.push_str(&format!("\"row\nnumber {i}\",{i}\n"));
        }
        let wb = parse_prefix(s.as_bytes(), None, 200);
        assert_eq!(wb.used_rows(0), 200);
        assert!(!wb.is_complete());
        assert!(wb.header(0));
        assert_eq!(wb.cell(0, 199, 0), "row\nnumber 198");
        // A small file is parsed whole.
        let small = parse_prefix(b"a,b\n1,2", None, 200);
        assert!(small.is_complete());
        assert_eq!(small.used_rows(0), 2);
        assert_eq!(small.cell(0, 1, 1), "2");
    }

    /// csv-core as an oracle: on well-formed generated input (no blank lines,
    /// which csv-core skips), both parsers agree on every field.
    #[test]
    fn grid_parse_agrees_with_csv_core() {
        let mut rng = Rng::new(0x5eed_c5c0);
        for case in 0..300 {
            let opts = GenOpts {
                delimiter: b",\t;|"[case % 4],
                blank_lines: false,
                ragged: true,
                lone_cr: false,
                bom: false,
            };
            let input = gen_csv(&mut rng, &opts);
            let expected = csv_core_records(input.as_bytes(), opts.delimiter);
            let mut dialect = Dialect {
                delimiter: opts.delimiter,
                ..Dialect::default()
            };
            dialect.header = false;
            let mut pool = StringPool::new();
            let limits = Limits {
                max_rows: None,
                drop_unterminated_tail: false,
            };
            let sheet = parse_sheet(
                Arc::from(input.as_str()),
                &dialect,
                &mut pool,
                &limits,
                None,
                None,
            )
            .unwrap();
            assert_eq!(sheet.rows(), expected.len(), "case {case}: {input:?}");
            for (r, rec) in expected.iter().enumerate() {
                for c in 0..sheet.cols().max(rec.len()) {
                    let want = rec.get(c).map_or("", String::as_str);
                    assert_eq!(
                        sheet.cell(&pool, r, c),
                        want,
                        "case {case} row {r} col {c}: {input:?}"
                    );
                }
            }
        }
    }

    fn csv_core_records(input: &[u8], delimiter: u8) -> Vec<Vec<String>> {
        use csv_core::{ReadRecordResult, ReaderBuilder};
        let mut rdr = ReaderBuilder::new().delimiter(delimiter).build();
        let mut out = vec![0u8; input.len() + 16];
        let mut ends = vec![0usize; input.len() + 16];
        let (mut inp, mut opos, mut epos) = (input, 0, 0);
        let mut records = Vec::new();
        loop {
            // csv-core keeps its own position within a record across calls,
            // so `ends` are already relative to the record's start.
            let (res, nin, nout, nend) = rdr.read_record(inp, &mut out[opos..], &mut ends[epos..]);
            inp = &inp[nin..];
            opos += nout;
            epos += nend;
            match res {
                ReadRecordResult::InputEmpty => {}
                ReadRecordResult::Record => {
                    let mut start = 0;
                    let rec = ends[..epos]
                        .iter()
                        .map(|&e| {
                            let s = String::from_utf8(out[start..e].to_vec()).unwrap();
                            start = e;
                            s
                        })
                        .collect();
                    records.push(rec);
                    (opos, epos) = (0, 0);
                }
                ReadRecordResult::End => return records,
                other => panic!("csv-core buffer too small: {other:?}"),
            }
        }
    }
}
