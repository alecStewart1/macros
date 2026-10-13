//! `GridElement`: paints one pane showing a grid buffer — formula bar,
//! column/row headers, gridlines, cell text, selection, the cursor cell, and
//! the inline cell editor. Only cells on screen are touched, so the cost of a
//! frame is independent of the file's size.
//!
//! GPUI's line-layout cache already reuses a shaped line whose text, font and
//! size match one from the previous frame, so repeated values (a `status`
//! column, scrolling a few rows) don't re-shape; the element just keeps every
//! string it shapes short (truncated to what the cell can show).

use gpui::{
    App, Bounds, ContentMask, CursorStyle, Element, ElementId, ElementInputHandler, Entity, Font,
    FontWeight, GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId, LayoutId,
    PaintQuad, Pixels, Point, ShapedLine, SharedString, Style, TextRun, Window, fill, font, point,
    prelude::*, px, relative, rgb, rgba, size,
};

use super::Editor;
use super::grid::{BAR_HEIGHT, GridDoc, GridDrag, GridGeom, NAME_BOX_WIDTH, SHEET};
use crate::grid::cellfmt::{self, CellFmt};
use crate::grid::view::{self, CellRange, col_name};
use crate::theme::Theme;

/// Horizontal padding inside a cell, unzoomed px.
const CELL_PAD: f32 = 3.0;
/// Thickness of the cursor-cell / cell-editor outline, screen px. It is drawn
/// over the cell's gridlines (not inside the cell), so it reads as a crisp
/// frame without eating into the text.
const CURSOR_BORDER: f32 = 1.0;

pub struct GridElement {
    pub editor: Entity<Editor>,
    pub pane: usize,
}

/// A shaped string and where to draw it, clipped to `clip`.
struct Text {
    line: ShapedLine,
    origin: Point<Pixels>,
    clip: Bounds<Pixels>,
    line_height: Pixels,
}

pub struct GridPrepaint {
    /// Fills drawn under everything in the cells area (selection, blocks).
    under: Vec<PaintQuad>,
    /// Gridlines.
    lines: Vec<PaintQuad>,
    /// Cell text, clipped per cell (scrolled rows).
    cells: Vec<Text>,
    /// Frozen rows: their fills and text, drawn over the scrolled rows.
    frozen_under: Vec<PaintQuad>,
    frozen_cells: Vec<Text>,
    /// Headers, formula bar, cursor, editor, guides: drawn last.
    over: Vec<PaintQuad>,
    over_text: Vec<Text>,
    /// Cursor / editor / range outlines: painted after the headers, so an
    /// outline on row 1 or column A keeps its top/left edge (which sits on
    /// the header separator line).
    outlines: Vec<PaintQuad>,
    /// Where outlines may draw: the cells area plus the 1px line above and
    /// left of it, so they never spill over the header labels.
    outline_bounds: Bounds<Pixels>,
    cells_bounds: Bounds<Pixels>,
    scroll_bounds: Bounds<Pixels>,
    hitboxes: Vec<(Hitbox, CursorStyle)>,
    /// The function-name dropdown while typing a formula (the editor's
    /// LSP completion popup, reused).
    popup: Option<crate::element::PopupLayout>,
    background: PaintQuad,
}

impl IntoElement for GridElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

/// Theme colors the grid uses, resolved once per frame.
struct Palette {
    /// Colours for the references of a formula being edited, in order
    /// (each reference and the cells it covers share one), as Excel does.
    refs: Vec<Hsla>,
    /// Outline of `grid-trace-precedents` / `-dependents`.
    trace: Hsla,
    bg: Hsla,
    fg: Hsla,
    dim: Hsla,
    line: Hsla,
    header_bg: Hsla,
    header_active: Hsla,
    /// Column letters / row numbers: quiet, so the data reads first.
    header_label: Hsla,
    /// The active row/column's label: a step up from `header_label`.
    header_label_active: Hsla,
    selection: Hsla,
    cursor: Hsla,
    block: Hsla,
    warn: Hsla,
}

impl Palette {
    fn new(theme: &Theme) -> Self {
        let bg = theme.background();
        let fg = theme.foreground();
        let dim = theme.color("dim", Hsla { a: 0.55, ..fg });
        let mut line = theme.color("grid-line", fg);
        if theme.background_of("grid-line").is_none() {
            line.a = 0.14;
        }
        // Headers sit on the plain background unless a theme sets the
        // `grid-header` face: they're reference marks, not content.
        let mut header_bg = theme.color("grid-header", fg);
        if theme.background_of("grid-header").is_none() {
            header_bg.a = 0.0;
        }
        let selection = theme.color("region", rgba(0x3a5fcd55).into());
        let cursor = theme.color("cursor", rgb(0x528bff).into());
        let header_active = Hsla { a: 0.07, ..fg };
        let refs: Vec<Hsla> = [0x4f8bf5, 0xe0533c, 0x9b59d0, 0x2fa36b, 0xd4882a, 0xd14b9b]
            .iter()
            .enumerate()
            .map(|(i, &fallback)| theme.color(&format!("plot-series-{i}"), rgb(fallback).into()))
            .collect();
        let trace = theme.color("warning", rgb(0xe5c07b).into());
        Self {
            refs,
            trace,
            bg,
            fg,
            dim,
            line,
            header_bg,
            header_active,
            header_label: Hsla { a: 0.30, ..fg },
            header_label_active: Hsla { a: 0.70, ..fg },
            selection,
            cursor,
            block: Hsla { a: 0.45, ..fg },
            warn: theme.color("warning", rgb(0xe5c07b).into()),
        }
    }
}

/// Default colors of the hand-formatting palette ([`cellfmt::COLORS`]):
/// mid tones that read on light and dark backgrounds. A theme overrides one
/// with the face `grid-<name>` (its background, if set, is the fill).
const PALETTE_DEFAULTS: [&str; 6] = [
    "#e5484d", "#f76b15", "#d6a400", "#30a46c", "#3e8ef7", "#8e4ec6",
];

fn palette_color(theme: &Theme, i: u8) -> Hsla {
    let fallback = PALETTE_DEFAULTS
        .get(i as usize)
        .and_then(|hex| crate::theme::parse_color(hex))
        .unwrap_or_default();
    theme.color(&format!("grid-{}", cellfmt::color_name(i)), fallback)
}

/// A fill: the face's background when the theme sets one, else the color
/// as a wash light enough to read text over.
fn palette_fill(theme: &Theme, i: u8) -> Hsla {
    theme
        .background_of(&format!("grid-{}", cellfmt::color_name(i)))
        .unwrap_or_else(|| Hsla {
            a: 0.4,
            ..palette_color(theme, i)
        })
}

fn run(len: usize, font: &Font, color: Hsla) -> TextRun {
    TextRun {
        len,
        font: font.clone(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    }
}

/// The first `max_chars` chars of `s`'s first line, with a `↵` marking any
/// further lines (cells keep a uniform row height; the bar shows it all).
fn display_text(s: &str, max_chars: usize) -> String {
    let mut out = String::new();
    let mut more_lines = false;
    for (i, line) in s.split('\n').enumerate() {
        if i > 0 {
            more_lines = true;
            break;
        }
        out.extend(line.trim_end_matches('\r').chars().take(max_chars));
    }
    if more_lines {
        out.push_str(" ↵");
    }
    out
}

impl Element for GridElement {
    type RequestLayoutState = ();
    type PrepaintState = Option<GridPrepaint>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let theme = cx.global::<Theme>();
        let pal = Palette::new(theme);
        let settings = cx.global::<crate::settings::Settings>();
        let base_size = settings.font_size();
        let family = settings.font_family().to_string();
        let popup_rows = settings.completion_max_rows();
        let popup_detail = settings.completion_show_detail();
        let popup_kind = settings.completion_show_kind();

        let editor = self.editor.read(cx);
        let g = editor.pane_grid(self.pane)?;
        let drag = editor.grid_ui.drag;
        let zoom = g.view.zoom.max(0.1);
        let font_size = px((base_size * zoom).max(3.0));
        let chrome_size = px(base_size.min(14.0) * 0.92);
        let regular = font(family.clone());
        let bold = Font {
            weight: FontWeight::BOLD,
            ..regular.clone()
        };
        // Without a bold face installed (the bundled Lilex is regular only),
        // bold resolves to the regular face: hand-set bold is then drawn
        // twice, a fraction of a pixel apart.
        let fake_bold = {
            let ts = window.text_system();
            ts.resolve_font(&bold) == ts.resolve_font(&regular)
        };
        let char_w = f32::from(
            window
                .text_system()
                .shape_line("0".into(), font_size, &[run(1, &regular, pal.fg)], None)
                .x_for_index(1),
        )
        .max(1.0);
        let chrome_char_w = f32::from(
            window
                .text_system()
                .shape_line("0".into(), chrome_size, &[run(1, &regular, pal.fg)], None)
                .x_for_index(1),
        )
        .max(1.0);

        let v = &g.view;
        let (used_rows, used_cols) = g.used();
        let bar_h = if v.bar_visible { BAR_HEIGHT } else { 0.0 };
        let header_h = (view::DEFAULT_ROW_HEIGHT as f32 * zoom).max(12.0);
        // Row header: wide enough for the largest row number on screen.
        let approx_last_row = v.top_row() + v.page_rows() + 2;
        let digits = approx_last_row.to_string().len().max(2) as f32;
        let header_w = (digits * char_w + 12.0).max(40.0 * zoom);

        let left = f32::from(bounds.left());
        let top = f32::from(bounds.top());
        let width = f32::from(bounds.size.width);
        let height = f32::from(bounds.size.height);
        let ox = left + header_w;
        let oy = top + bar_h + header_h;
        let cw = (width - header_w).max(0.0);
        let ch = (height - bar_h - header_h).max(0.0);
        let cells_bounds = Bounds::new(point(px(ox), px(oy)), size(px(cw), px(ch)));
        let frozen_h = (v.frozen_height() * zoom as f64) as f32;
        let scroll_bounds = Bounds::new(
            point(px(ox), px(oy + frozen_h.min(ch))),
            size(px(cw), px((ch - frozen_h).max(0.0))),
        );

        // Visible columns: (index, x, width) in screen px.
        let mut cols: Vec<(usize, f32, f32)> = Vec::new();
        let mut c = v.col_at(0.0);
        loop {
            let x = v.col_x(c) as f32;
            if x >= cw || c >= view::MAX_INDEX {
                break;
            }
            let w = v.col_px(c) as f32;
            if w > 0.0 {
                cols.push((c, ox + x, w));
            }
            c = v.cols.next_visible(c + 1);
        }
        // Visible rows: frozen first, then the scrolled ones.
        let mut rows: Vec<(usize, f32, f32, bool)> = Vec::new();
        for r in 0..v.frozen_rows {
            let y = v.row_y(r) as f32;
            if y >= ch {
                break; // the rest of the frozen band is off screen
            }
            let h = v.row_px(r) as f32;
            if h > 0.0 {
                rows.push((r, oy + y, h, true));
            }
        }
        let mut r = v.top_row();
        loop {
            let y = v.row_y(r) as f32;
            if y >= ch || r >= view::MAX_INDEX {
                break;
            }
            let h = v.row_px(r) as f32;
            if h > 0.0 {
                rows.push((r, oy + y, h, false));
            }
            // Skips a filter's hidden rows in one jump, however many.
            r = v.rows.next_visible(r + 1);
        }

        let mut p = GridPrepaint {
            under: Vec::new(),
            lines: Vec::new(),
            cells: Vec::new(),
            frozen_under: Vec::new(),
            frozen_cells: Vec::new(),
            over: Vec::new(),
            over_text: Vec::new(),
            outlines: Vec::new(),
            outline_bounds: Bounds::new(
                point(px(ox - CURSOR_BORDER), px(oy - CURSOR_BORDER)),
                size(px(cw + CURSOR_BORDER), px(ch + CURSOR_BORDER)),
            ),
            cells_bounds,
            scroll_bounds,
            hitboxes: Vec::new(),
            popup: None,
            background: fill(bounds, pal.bg),
        };

        let sel = g.selection();
        let has_range = !sel.is_single();
        let editing = v.edit.as_ref();
        let blocks = zoom < view::BLOCK_ZOOM;
        let cell_lh = px(header_h.max(1.0));
        let truncate = editor.grid_ui.opts.truncate_indicator.as_str();
        let truncate_len = truncate.chars().count();
        // Zebra stripes: the face's background, or a faint wash of the text
        // color when the theme gives the face none.
        let stripe = g.style.stripe.as_deref().map(|f| {
            theme
                .background_of(f)
                .unwrap_or(Hsla { a: 0.035, ..pal.fg })
        });
        let header_face = g.style.header_face.as_deref();

        // --- cells ---------------------------------------------------------
        for &(r, y, h, frozen) in &rows {
            let (under, texts) = if frozen {
                (&mut p.frozen_under, &mut p.frozen_cells)
            } else {
                (&mut p.under, &mut p.cells)
            };
            if frozen {
                under.push(fill(
                    Bounds::new(point(px(ox), px(y)), size(px(cw), px(h))),
                    pal.bg,
                ));
            }
            // The header row reads as a header.
            let header_row = r == 0 && g.wb.header(SHEET);
            let row_font = if header_row { &bold } else { &regular };
            if let Some(bg) = stripe
                && !header_row
                && v.rows.rank(r) % 2 == 1
            {
                under.push(fill(
                    Bounds::new(point(px(ox), px(y)), size(px(cw), px(h - 1.0))),
                    bg,
                ));
            }
            let mut skip_until = 0usize; // columns already covered by overflow
            for (i, &(c, x, w)) in cols.iter().enumerate() {
                // Bold and colors set by hand; a fill shows on any cell,
                // empty or not, and under the selection's tint.
                let fmt = if g.formats.touches_col(c) {
                    g.formats.get(r, c)
                } else {
                    CellFmt::default()
                };
                if let Some(i) = fmt.fill {
                    under.push(fill(
                        Bounds::new(point(px(x), px(y)), size(px(w - 1.0), px(h - 1.0))),
                        palette_fill(theme, i),
                    ));
                }
                if has_range && sel.contains(r, c) {
                    under.push(fill(
                        Bounds::new(point(px(x), px(y)), size(px(w), px(h))),
                        pal.selection,
                    ));
                }
                if i < skip_until || r >= used_rows || c >= used_cols {
                    continue;
                }
                if editing.is_some_and(|e| e.row == r && e.col == c) {
                    continue; // the editor draws this cell
                }
                let value = g.shown_text(r, c);
                // Scheme-set styling (faces, rules, number formats); the
                // header row is left plain.
                let styled = !header_row && g.style.touches(c);
                let face = if styled {
                    g.style.face(r, c, value, |k| g.cell(r, k))
                } else if header_row {
                    header_face
                } else {
                    None
                };
                if let Some(bg) = face.and_then(|f| theme.background_of(f)) {
                    under.push(fill(
                        Bounds::new(point(px(x), px(y)), size(px(w - 1.0), px(h - 1.0))),
                        bg,
                    ));
                }
                if value.is_empty() {
                    continue;
                }
                // A number format set on the cell wins over a column's.
                let by_cell = fmt.num.and_then(|code| g.number_formatted(r, c, code));
                let formatted = match &by_cell {
                    Some((text, _)) => Some(text.clone()),
                    None if styled => g.style.display(c, value),
                    None => None,
                };
                let value = formatted.as_deref().unwrap_or(value);
                if blocks {
                    under.push(fill(
                        Bounds::new(
                            point(px(x + 1.0), px(y + 1.0)),
                            size(px(w - 2.0), px(h - 2.0)),
                        ),
                        pal.block,
                    ));
                    continue;
                }
                let pad = CELL_PAD * zoom;
                // A formula aligns by its result: numbers right, as Excel
                // does, even in a text column.
                let result = g.formula_value(r, c);
                let right = match (&by_cell, result) {
                    (Some((_, numeric)), _) => *numeric,
                    (None, Some(v)) => matches!(v, crate::formula::value::Value::Number(_)),
                    (None, None) => g.right_aligned(c),
                } && !header_row;
                let center =
                    !header_row && g.style.align(c) == Some(crate::grid::style::Align::Center);
                // Left-aligned text spills into empty cells to its right, as
                // in Excel; right-aligned numbers and centered text never do.
                let mut avail = w - 2.0 * pad;
                let mut clip_w = w;
                if !right && !center {
                    let mut j = i + 1;
                    while (avail / char_w) < value.chars().count() as f32
                        && j < cols.len()
                        // Into empty cells only — a formula cell isn't empty
                        // even when its result is "".
                        && (cols[j].0 >= used_cols
                            || (g.cell(r, cols[j].0).is_empty() && !g.is_formula_at(r, cols[j].0)))
                        && !editing.is_some_and(|e| e.row == r && e.col == cols[j].0)
                        && !(has_range && sel.contains(r, cols[j].0))
                    {
                        avail += cols[j].2;
                        clip_w += cols[j].2;
                        j += 1;
                    }
                    skip_until = j;
                }
                let max_chars = ((avail / char_w).ceil().max(1.0) as usize).saturating_add(1);
                let mut shown = display_text(value, max_chars);
                let fits = (avail / char_w).floor().max(1.0) as usize;
                if !right && truncate_len > 0 && shown.chars().count() > fits {
                    // Cut off: end with the indicator, inside the cell.
                    let keep = fits.saturating_sub(truncate_len);
                    shown = shown.chars().take(keep).collect::<String>() + truncate;
                }
                let mut color = face.map_or(pal.fg, |f| theme.color(f, pal.fg));
                if face.is_none() && matches!(result, Some(crate::formula::value::Value::Error(_)))
                {
                    color = theme.color("error", pal.warn);
                }
                if let Some(i) = fmt.text {
                    color = palette_color(theme, i);
                }
                let cell_font = if fmt.bold { &bold } else { row_font };
                // A number too wide shrinks or turns to ###; text (right-
                // aligned by its column) is clipped like any text.
                let numeric = match result {
                    Some(v) => matches!(v, crate::formula::value::Value::Number(_)),
                    None => crate::grid::infer::cell_number(&shown).is_some(),
                };
                if right && numeric && shown.chars().count() as f32 * char_w > avail {
                    let n = (avail / char_w).floor().max(1.0) as usize;
                    // Like Excel's General format: drop decimal places until
                    // it fits; only a number whose whole part can't fit shows
                    // as ###, never cut off.
                    match fit_number(&shown, n) {
                        Some(fitted) => shown = fitted,
                        None => {
                            shown = "#".repeat(n);
                            color = pal.dim;
                        }
                    }
                }
                let line = window.text_system().shape_line(
                    SharedString::from(shown.clone()),
                    font_size,
                    &[run(shown.len(), cell_font, color)],
                    None,
                );
                let tw = f32::from(line.width);
                let tx = if right {
                    x + w - pad - tw
                } else if center {
                    x + ((w - tw) / 2.0).max(pad)
                } else {
                    x + pad
                };
                let ty = y + (h - header_h) / 2.0;
                let clip = Bounds::new(point(px(x), px(y)), size(px(clip_w), px(h)));
                if fmt.bold && fake_bold {
                    texts.push(Text {
                        line: line.clone(),
                        origin: point(px(tx + (0.6 * zoom).max(0.4)), px(ty)),
                        clip,
                        line_height: cell_lh,
                    });
                }
                texts.push(Text {
                    line,
                    origin: point(px(tx), px(ty)),
                    clip,
                    line_height: cell_lh,
                });
            }
        }

        // --- gridlines -----------------------------------------------------
        if zoom >= 0.25 {
            for &(_, x, w) in &cols {
                p.lines.push(fill(
                    Bounds::new(point(px(x + w - 1.0), px(oy)), size(px(1.0), px(ch))),
                    pal.line,
                ));
            }
            for &(_, y, h, _) in &rows {
                p.lines.push(fill(
                    Bounds::new(point(px(ox), px(y + h - 1.0)), size(px(cw), px(1.0))),
                    pal.line,
                ));
            }
        }
        if frozen_h > 0.0 && frozen_h < ch {
            // A stronger line under the frozen band.
            p.over.push(fill(
                Bounds::new(
                    point(px(ox), px(oy + frozen_h - 1.0)),
                    size(px(cw), px(2.0)),
                ),
                Hsla { a: 0.35, ..pal.fg },
            ));
        }

        // --- cursor cell & inline editor -----------------------------------
        let (cr, cc) = v.cursor;
        let cell_rect = |r: usize, c: usize| -> Option<(f32, f32, f32, f32)> {
            let x = ox + v.col_x(c) as f32;
            let y = oy + v.row_y(r) as f32;
            let w = v.col_px(c) as f32;
            let h = v.row_px(r) as f32;
            let visible_y = if r < v.frozen_rows {
                true
            } else {
                y + h > oy + frozen_h
            };
            (x + w > ox && x < ox + cw && y + h > oy && y < oy + ch && visible_y)
                .then_some((x, y, w, h))
        };
        // A formula being edited: each reference gets a colour, and the
        // cells it covers an outline in that colour.
        let ref_spans = editing.map_or_else(Vec::new, |e| formula_refs(&e.text, &pal));
        let mut outline = |a: CellRange, color: Hsla| {
            let x0 = ox + v.col_x(a.c0) as f32;
            let y0 = oy + v.row_y(a.r0) as f32;
            let x1 = ox + (v.col_x(a.c1) + v.col_px(a.c1)) as f32;
            let y1 = oy + (v.row_y(a.r1) + v.row_px(a.r1)) as f32;
            if x1 > x0 && y1 > y0 {
                push_border(
                    &mut p.outlines,
                    x0 - 1.0,
                    y0 - 1.0,
                    x1 - x0 + 1.0,
                    y1 - y0 + 1.0,
                    1.5,
                    color,
                );
            }
        };
        for rs in &ref_spans {
            // Whole rows/columns stop at the data.
            let mut a = rs.area;
            a.r1 = a.r1.min(used_rows.max(a.r0 + 1) - 1);
            a.c1 = a.c1.min(used_cols.max(a.c0 + 1) - 1);
            outline(a, rs.color);
        }
        for &a in &v.trace {
            outline(a, pal.trace);
        }
        if let Some(e) = editing {
            if let Some((x, y, w, h)) = cell_rect(e.row, e.col) {
                let shown = e.text.replace('\n', "↵");
                let caret_disp = e.caret + 2 * e.text[..e.caret].matches('\n').count();
                let runs = coloured_runs(&shown, &ref_spans, &regular, pal.fg);
                let line = window.text_system().shape_line(
                    SharedString::from(shown.clone()),
                    font_size,
                    &runs,
                    None,
                );
                let pad = CELL_PAD * zoom;
                // The editor grows to the right to fit its text, like Excel.
                let ew = (f32::from(line.width) + 2.0 * pad + char_w)
                    .max(w)
                    .min(ox + cw - x);
                p.over.push(fill(
                    Bounds::new(point(px(x), px(y)), size(px(ew), px(h))),
                    pal.bg,
                ));
                push_border(
                    &mut p.outlines,
                    x - 1.0,
                    y - 1.0,
                    ew + 1.0,
                    h + 1.0,
                    CURSOR_BORDER,
                    pal.cursor,
                );
                let caret_x = x + pad + f32::from(line.x_for_index(caret_disp));
                if !e.in_bar {
                    p.over.push(fill(
                        Bounds::new(point(px(caret_x), px(y + 2.0)), size(px(1.5), px(h - 4.0))),
                        pal.cursor,
                    ));
                }
                // The function-name dropdown hangs below the caret.
                if let Some(view) = Editor::grid_completion_view(g) {
                    // Rows at the base font's height whatever the zoom (its
                    // text isn't zoomed), the list starting under the cell.
                    let row_h = (base_size * 1.4).round().max(4.0);
                    p.popup = Some(crate::element::build_popup(
                        point(px(caret_x), px(y + h - row_h)),
                        view,
                        popup_rows,
                        popup_detail,
                        popup_kind,
                        &regular,
                        px((base_size).max(3.0)),
                        px(row_h),
                        bounds,
                        theme,
                        window,
                    ));
                }
                p.over_text.push(Text {
                    line,
                    origin: point(px(x + pad), px(y + (h - header_h) / 2.0)),
                    clip: Bounds::new(point(px(x), px(y)), size(px(ew), px(h))),
                    line_height: cell_lh,
                });
            }
        } else if !has_range && let Some((x, y, w, h)) = cell_rect(cr, cc) {
            // A multi-cell selection shows only its range outline; the
            // cursor cell gets its own frame only when it's the selection.
            push_border(
                &mut p.outlines,
                x - 1.0,
                y - 1.0,
                w + 1.0,
                h + 1.0,
                CURSOR_BORDER,
                pal.cursor,
            );
        }
        if has_range {
            // Outline the whole selected range (clamped to the view).
            let x0 = (ox + v.col_x(sel.c0) as f32).max(ox - 2.0);
            // A range below the frozen rows, scrolled up under them, stops
            // at the frozen band's edge rather than drawing across it.
            let top_limit = if sel.r0 >= v.frozen_rows {
                oy + frozen_h + 1.0
            } else {
                oy - 2.0
            };
            let y0 = (oy + v.row_y(sel.r0) as f32).max(top_limit);
            let x1 = (ox + (v.col_x(sel.c1) + v.col_px(sel.c1)) as f32).min(ox + cw + 2.0);
            let y1 = (oy + (v.row_y(sel.r1) + v.row_px(sel.r1)) as f32).min(oy + ch + 2.0);
            if x1 > x0 && y1 > y0 {
                push_border(
                    &mut p.outlines,
                    x0 - 1.0,
                    y0 - 1.0,
                    x1 - x0 + 1.0,
                    y1 - y0 + 1.0,
                    CURSOR_BORDER,
                    pal.cursor,
                );
            }
        }

        // --- headers -------------------------------------------------------
        let hy = top + bar_h;
        p.over.push(fill(
            Bounds::new(point(px(left), px(hy)), size(px(width), px(header_h))),
            pal.bg,
        ));
        p.over.push(fill(
            Bounds::new(point(px(left), px(hy)), size(px(width), px(header_h))),
            pal.header_bg,
        ));
        p.over.push(fill(
            Bounds::new(point(px(left), px(oy)), size(px(header_w), px(ch))),
            pal.bg,
        ));
        p.over.push(fill(
            Bounds::new(point(px(left), px(oy)), size(px(header_w), px(ch))),
            pal.header_bg,
        ));
        let header_font_size = px((base_size * zoom * 0.8).max(3.0));
        let active_cols = if has_range {
            (sel.c0, sel.c1)
        } else {
            (cc, cc)
        };
        let active_rows = if has_range {
            (sel.r0, sel.r1)
        } else {
            (cr, cr)
        };
        for &(c, x, w) in &cols {
            if (active_cols.0..=active_cols.1).contains(&c) {
                p.over.push(fill(
                    Bounds::new(point(px(x), px(hy)), size(px(w), px(header_h))),
                    pal.header_active,
                ));
            }
            if !blocks || c % 5 == 0 {
                let name = col_name(c);
                let color = if (active_cols.0..=active_cols.1).contains(&c) {
                    pal.header_label_active
                } else {
                    pal.header_label
                };
                let line = window.text_system().shape_line(
                    SharedString::from(name.clone()),
                    header_font_size,
                    &[run(name.len(), &regular, color)],
                    None,
                );
                let tx = x + (w - f32::from(line.width)) / 2.0;
                p.over_text.push(Text {
                    line,
                    origin: point(px(tx), px(hy)),
                    clip: Bounds::new(point(px(x), px(hy)), size(px(w), px(header_h))),
                    line_height: px(header_h),
                });
            }
            // A grab band on the border: resize cursor.
            let hb = window.insert_hitbox(
                Bounds::new(point(px(x + w - 3.0), px(hy)), size(px(6.0), px(header_h))),
                HitboxBehavior::Normal,
            );
            p.hitboxes.push((hb, CursorStyle::ResizeLeftRight));
        }
        for &(r, y, h, _) in &rows {
            if (active_rows.0..=active_rows.1).contains(&r) {
                p.over.push(fill(
                    Bounds::new(point(px(left), px(y)), size(px(header_w), px(h))),
                    pal.header_active,
                ));
            }
            if !blocks || r % 10 == 0 {
                let label = (r + 1).to_string();
                let problem = g.wb.problems(SHEET).binary_search(&r).is_ok();
                let line = window.text_system().shape_line(
                    SharedString::from(label.clone()),
                    header_font_size,
                    &[run(
                        label.len(),
                        &regular,
                        if problem {
                            pal.warn
                        } else if (active_rows.0..=active_rows.1).contains(&r) {
                            pal.header_label_active
                        } else {
                            pal.header_label
                        },
                    )],
                    None,
                );
                let tx = left + header_w - 6.0 - f32::from(line.width);
                p.over_text.push(Text {
                    line,
                    origin: point(px(tx), px(y + (h - header_h) / 2.0)),
                    clip: Bounds::new(point(px(left), px(y)), size(px(header_w), px(h))),
                    line_height: px(header_h),
                });
            }
            let hb = window.insert_hitbox(
                Bounds::new(
                    point(px(left), px(y + h - 3.0)),
                    size(px(header_w), px(6.0)),
                ),
                HitboxBehavior::Normal,
            );
            p.hitboxes.push((hb, CursorStyle::ResizeUpDown));
        }
        // Header separators: the same hairline as the gridlines.
        p.over.push(fill(
            Bounds::new(point(px(left), px(oy - 1.0)), size(px(width), px(1.0))),
            pal.line,
        ));
        p.over.push(fill(
            Bounds::new(
                point(px(ox - 1.0), px(hy)),
                size(px(1.0), px(height - bar_h)),
            ),
            pal.line,
        ));

        // --- scroll position indicators ----------------------------------------
        let content_h = (v.rows.start(used_rows.max(v.cursor.0 + 1)) - v.frozen_height())
            .max(v.scroll_y + (ch - frozen_h) as f64 / zoom as f64)
            * zoom as f64;
        if content_h > (ch - frozen_h) as f64 + 1.0 {
            let view_h = (ch - frozen_h) as f64;
            let thumb = (view_h / content_h * view_h).max(18.0) as f32;
            let pos = (v.scroll_y * zoom as f64 / content_h * view_h) as f32;
            p.over.push(fill(
                Bounds::new(
                    point(
                        px(ox + cw - 6.0),
                        px(oy + frozen_h + pos.min((view_h as f32 - thumb).max(0.0))),
                    ),
                    size(px(4.0), px(thumb)),
                ),
                Hsla { a: 0.25, ..pal.fg },
            ));
        }
        let content_w = v
            .cols
            .start(used_cols.max(v.cursor.1 + 1))
            .max(v.scroll_x + cw as f64 / zoom as f64)
            * zoom as f64;
        if content_w > cw as f64 + 1.0 {
            let thumb = (cw as f64 / content_w * cw as f64).max(18.0) as f32;
            let pos = (v.scroll_x * zoom as f64 / content_w * cw as f64) as f32;
            p.over.push(fill(
                Bounds::new(
                    point(px(ox + pos.min((cw - thumb).max(0.0))), px(oy + ch - 6.0)),
                    size(px(thumb), px(4.0)),
                ),
                Hsla { a: 0.25, ..pal.fg },
            ));
        }

        // --- resize guide ------------------------------------------------------
        if let Some(GridDrag::Resize {
            pane,
            axis,
            start,
            size: sz,
            ..
        }) = drag
            && pane == self.pane
        {
            let is_col = axis == crate::formula::refs::Axis::Cols;
            let at = start + (sz as f32) * zoom;
            let guide = if is_col {
                Bounds::new(
                    point(px(ox + at - 1.0), px(hy)),
                    size(px(2.0), px(height - bar_h)),
                )
            } else {
                Bounds::new(point(px(left), px(oy + at - 1.0)), size(px(width), px(2.0)))
            };
            p.over.push(fill(guide, pal.cursor));
            let whole = window.insert_hitbox(bounds, HitboxBehavior::Normal);
            let style = if is_col {
                CursorStyle::ResizeLeftRight
            } else {
                CursorStyle::ResizeUpDown
            };
            p.hitboxes.push((whole, style));
        }

        // --- formula bar ---------------------------------------------------------
        let mut bar_text = (0.0, 0.0);
        if bar_h > 0.0 {
            bar_text = self.formula_bar(
                g,
                &pal,
                &regular,
                chrome_size,
                chrome_char_w,
                bounds,
                &mut p,
                window,
            );
        }

        let geom = GridGeom {
            origin: bounds.origin,
            width,
            height,
            bar_h,
            header_w,
            header_h,
            char_w,
            bar_text_x: bar_text.0,
            bar_shift: bar_text.1,
            bar_char_w: chrome_char_w,
        };
        let pane = self.pane;
        let hit = p.popup.as_ref().map(|pp| pp.hit);
        self.editor.update(cx, |editor, _| {
            editor.set_grid_geometry(pane, geom);
            editor.grid_ui.completion_hit = hit;
        });
        Some(p)
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.editor.read(cx).focus_handle.clone();
        // Claim the typed-text input handler for the selected grid pane, so
        // IME composition (dead keys, CJK) reaches the cell editor.
        if self.editor.read(cx).pane_is_selected(self.pane) {
            window.handle_input(
                &focus_handle,
                ElementInputHandler::new(bounds, self.editor.clone()),
                cx,
            );
        }
        let Some(p) = prepaint.as_mut() else {
            return;
        };
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            window.paint_quad(p.background.clone());
            window.with_content_mask(
                Some(ContentMask {
                    bounds: p.scroll_bounds,
                }),
                |window| {
                    for q in p.under.drain(..) {
                        window.paint_quad(q);
                    }
                    for t in p.cells.drain(..) {
                        paint_text(t, window, cx);
                    }
                },
            );
            window.with_content_mask(
                Some(ContentMask {
                    bounds: p.cells_bounds,
                }),
                |window| {
                    for q in p.frozen_under.drain(..) {
                        window.paint_quad(q);
                    }
                    for t in p.frozen_cells.drain(..) {
                        paint_text(t, window, cx);
                    }
                    for q in p.lines.drain(..) {
                        window.paint_quad(q);
                    }
                },
            );
            for q in p.over.drain(..) {
                window.paint_quad(q);
            }
            for t in p.over_text.drain(..) {
                paint_text(t, window, cx);
            }
            window.with_content_mask(
                Some(ContentMask {
                    bounds: p.outline_bounds,
                }),
                |window| {
                    for q in p.outlines.drain(..) {
                        window.paint_quad(q);
                    }
                },
            );
        });
        if let Some(popup) = p.popup.take() {
            // Over everything, unclipped by the cells area.
            for quad in popup.quads {
                window.paint_quad(quad);
            }
            for (line, origin) in popup.labels {
                line.paint(origin, popup.hit.row_height, window, cx).ok();
            }
        }
        for (hitbox, style) in &p.hitboxes {
            window.set_cursor_style(*style, hitbox);
        }
        let pane = self.pane;
        self.editor.update(cx, |editor, _| {
            editor.report_pane_bounds(pane, bounds);
        });
    }
}

impl GridElement {
    #[allow(clippy::too_many_arguments)]
    fn formula_bar(
        &self,
        g: &GridDoc,
        pal: &Palette,
        regular: &Font,
        font_size: Pixels,
        char_w: f32,
        bounds: Bounds<Pixels>,
        p: &mut GridPrepaint,
        window: &mut Window,
    ) -> (f32, f32) {
        let left = f32::from(bounds.left());
        let top = f32::from(bounds.top());
        let width = f32::from(bounds.size.width);
        let h = BAR_HEIGHT;
        p.over.push(fill(
            Bounds::new(point(px(left), px(top)), size(px(width), px(h))),
            pal.bg,
        ));
        p.over.push(fill(
            Bounds::new(point(px(left), px(top + h - 1.0)), size(px(width), px(1.0))),
            pal.line,
        ));
        p.over.push(fill(
            Bounds::new(
                point(px(left + NAME_BOX_WIDTH), px(top + 4.0)),
                size(px(1.0), px(h - 8.0)),
            ),
            pal.line,
        ));
        let (name, content) = g.bar_text();
        let lh = px(h);
        let shape = |window: &mut Window, s: &str, color: Hsla| {
            window.text_system().shape_line(
                SharedString::from(s.to_string()),
                font_size,
                &[run(s.len(), regular, color)],
                None,
            )
        };
        let name_line = shape(window, &name, pal.fg);
        p.over_text.push(Text {
            line: name_line,
            origin: point(px(left + 8.0), px(top)),
            clip: Bounds::new(
                point(px(left), px(top)),
                size(px(NAME_BOX_WIDTH - 4.0), px(h)),
            ),
            line_height: lh,
        });
        let fx_x = left + NAME_BOX_WIDTH + 8.0;
        let fx = shape(window, "fx", pal.dim);
        p.over_text.push(Text {
            line: fx,
            origin: point(px(fx_x), px(top)),
            clip: Bounds::new(point(px(fx_x), px(top)), size(px(24.0), px(h))),
            line_height: lh,
        });
        let text_x = left + NAME_BOX_WIDTH + 34.0;
        let avail = (width - (text_x - left) - 8.0).max(0.0);
        // Loading progress takes the right end of the bar.
        let mut avail_text = avail;
        if let Some(l) = &g.loading {
            let pct = (l.progress.load(std::sync::atomic::Ordering::Relaxed) as f64
                / l.total.max(1) as f64
                * 100.0)
                .min(99.0);
            let label = format!("Loading… {pct:.0}%");
            let line = shape(window, &label, pal.warn);
            let lw = f32::from(line.width);
            avail_text = (avail - lw - 16.0).max(0.0);
            let lx = left + width - 8.0 - lw;
            p.over_text.push(Text {
                line,
                origin: point(px(lx), px(top)),
                clip: Bounds::new(point(px(lx), px(top)), size(px(lw + 4.0), px(h))),
                line_height: lh,
            });
        }
        let editing = g.view.edit.as_ref();
        let max_chars = (avail_text / char_w) as usize + 2;
        let (shown, caret) = match editing {
            Some(e) => {
                let shown = e.text.replace('\n', "↵");
                let caret = e.caret + 2 * e.text[..e.caret].matches('\n').count();
                (shown, Some(caret))
            }
            None => {
                let mut s: String = content.replace('\n', "↵").chars().take(max_chars).collect();
                if content.chars().count() > max_chars {
                    s.pop();
                    s.push('…');
                }
                (s, None)
            }
        };
        let line = match editing {
            Some(e) => {
                let spans = formula_refs(&e.text, pal);
                window.text_system().shape_line(
                    SharedString::from(shown.clone()),
                    font_size,
                    &coloured_runs(&shown, &spans, regular, pal.fg),
                    None,
                )
            }
            None => shape(window, &shown, pal.fg),
        };
        // Keep the caret in view when the edited text is wider than the bar.
        let caret_x = caret.map(|c| f32::from(line.x_for_index(c)));
        let shift = caret_x.map_or(0.0, |x| (x - avail_text + char_w).max(0.0));
        if let (Some(x), Some(e)) = (caret_x, editing)
            && e.in_bar
        {
            p.over.push(fill(
                Bounds::new(
                    point(px(text_x + x - shift), px(top + 4.0)),
                    size(px(1.5), px(h - 8.0)),
                ),
                pal.cursor,
            ));
        }
        if let Some(e) = editing
            && !e.in_bar
        {
            // A subtle marker that the cell is being edited.
            p.over.push(fill(
                Bounds::new(
                    point(px(text_x - 6.0), px(top + 6.0)),
                    size(px(2.0), px(h - 12.0)),
                ),
                pal.cursor,
            ));
        }
        p.over_text.push(Text {
            line,
            origin: point(px(text_x - shift), px(top)),
            clip: Bounds::new(point(px(text_x), px(top)), size(px(avail_text), px(h))),
            line_height: lh,
        });
        // Where its text starts (from the pane's left) and how far long text
        // is scrolled, so a click lands on the character under it.
        (text_x - left, shift)
    }
}

/// `text` (a number as shown) rounded to fewer decimal places so it fits in
/// `width` characters, or `None` when even its whole part doesn't fit (or it
/// isn't a plain decimal number).
fn fit_number(text: &str, width: usize) -> Option<String> {
    let (_, frac) = text.split_once('.')?;
    if !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: f64 = text.parse().ok()?;
    (0..frac.len()).rev().find_map(|d| {
        // Half away from zero on the decimal value, as Excel rounds
        // (`format!` alone rounds half to even), and never "-0".
        let r = crate::formula::decimal::round(n, d as i32, crate::formula::decimal::Mode::Half);
        let r = if r == 0.0 { 0.0 } else { r };
        let s = format!("{r:.d$}");
        (s.chars().count() <= width).then_some(s)
    })
}

/// A coloured reference: its byte span in the text, the rectangle it
/// covers, and its colour.
/// A reference in a formula being edited.
struct RefSpan {
    /// Its byte span in the text.
    span: std::ops::Range<usize>,
    /// The cells it covers.
    area: CellRange,
    color: Hsla,
}

/// The references in edited formula `text`, each with its byte span, the
/// rectangle it covers, and its colour. Empty for anything but a formula
/// (and for multi-line text, whose display shifts byte offsets).
fn formula_refs(text: &str, pal: &Palette) -> Vec<RefSpan> {
    if !text.starts_with('=') || text.contains('\n') {
        return Vec::new();
    }
    crate::formula::parse::ref_spans(text)
        .into_iter()
        .filter(|(_, r)| r.sheet.is_none())
        .enumerate()
        .map(|(i, (span, r))| RefSpan {
            span,
            area: CellRange::from_bounds(r.area.bounds()),
            color: pal.refs[i % pal.refs.len()],
        })
        .collect()
}

/// Text runs colouring each reference span; the rest in `default`.
fn coloured_runs(text: &str, spans: &[RefSpan], font: &Font, default: Hsla) -> Vec<TextRun> {
    let mut runs = Vec::new();
    let mut at = 0;
    for RefSpan { span, color, .. } in spans {
        if span.start < at || span.end > text.len() {
            continue;
        }
        if span.start > at {
            runs.push(run(span.start - at, font, default));
        }
        runs.push(run(span.end - span.start, font, *color));
        at = span.end;
    }
    if at < text.len() || runs.is_empty() {
        runs.push(run(text.len() - at, font, default));
    }
    runs
}

/// Four quads forming a rectangle outline `t` px thick.
fn push_border(out: &mut Vec<PaintQuad>, x: f32, y: f32, w: f32, h: f32, t: f32, color: Hsla) {
    out.push(fill(
        Bounds::new(point(px(x), px(y)), size(px(w), px(t))),
        color,
    ));
    out.push(fill(
        Bounds::new(point(px(x), px(y + h - t)), size(px(w), px(t))),
        color,
    ));
    out.push(fill(
        Bounds::new(point(px(x), px(y)), size(px(t), px(h))),
        color,
    ));
    out.push(fill(
        Bounds::new(point(px(x + w - t), px(y)), size(px(t), px(h))),
        color,
    ));
}

fn paint_text(t: Text, window: &mut Window, cx: &mut App) {
    window.with_content_mask(Some(ContentMask { bounds: t.clip }), |window| {
        t.line.paint(t.origin, t.line_height, window, cx).ok();
    });
}

#[cfg(test)]
mod tests {
    use super::fit_number;

    #[test]
    fn numbers_lose_decimals_before_hashes() {
        assert_eq!(fit_number("18958.56", 7).as_deref(), Some("18958.6"));
        assert_eq!(fit_number("18958.56", 5).as_deref(), Some("18959"));
        assert_eq!(fit_number("18958.56", 4), None);
        assert_eq!(fit_number("123456", 3), None);
    }

    #[test]
    fn regression_fit_number_rounding() {
        // Excel rounds half away from zero: 18958.5 in a 5-char cell is 18959.
        eprintln!("18958.5 -> {:?}", fit_number("18958.5", 5));
        eprintln!("2.5 -> {:?}", fit_number("2.5", 1));
        eprintln!("-0.4 -> {:?}", fit_number("-0.4", 2));
        eprintln!("0.125 -> {:?}", fit_number("0.125", 4));
        assert_eq!(fit_number("18958.5", 5).as_deref(), Some("18959"));
        assert_eq!(fit_number("-0.4", 2).as_deref(), Some("0"));
    }
}

/// Audit probe: wraps a `GridElement` and records what its prepaint
/// produced, before paint drains it.
#[cfg(test)]
pub(crate) mod probe {
    use super::*;
    use std::{cell::RefCell, rc::Rc};

    #[derive(Default, Debug, Clone)]
    pub struct Seen {
        /// (text, origin x, origin y, clip x, clip w, frozen?)
        pub cells: Vec<(String, f32, f32, f32, f32, bool)>,
        pub over_text: Vec<String>,
        /// outline quads: (x, y, w, h)
        pub outlines: Vec<(f32, f32, f32, f32)>,
        pub popup_row_height: Option<f32>,
        pub cells_top: f32,
        pub frozen_bottom: f32,
    }

    pub struct Probe {
        pub inner: GridElement,
        pub seen: Rc<RefCell<Seen>>,
    }

    impl IntoElement for Probe {
        type Element = Self;
        fn into_element(self) -> Self {
            self
        }
    }

    impl Element for Probe {
        type RequestLayoutState = ();
        type PrepaintState = Option<GridPrepaint>;
        fn id(&self) -> Option<ElementId> {
            None
        }
        fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
            None
        }
        fn request_layout(
            &mut self,
            id: Option<&GlobalElementId>,
            iid: Option<&InspectorElementId>,
            window: &mut Window,
            cx: &mut App,
        ) -> (LayoutId, ()) {
            self.inner.request_layout(id, iid, window, cx)
        }
        fn prepaint(
            &mut self,
            id: Option<&GlobalElementId>,
            iid: Option<&InspectorElementId>,
            bounds: Bounds<Pixels>,
            rl: &mut (),
            window: &mut Window,
            cx: &mut App,
        ) -> Option<GridPrepaint> {
            let p = self.inner.prepaint(id, iid, bounds, rl, window, cx);
            if let Some(p) = &p {
                let mut s = self.seen.borrow_mut();
                let rec = |t: &Text, frozen: bool| {
                    (
                        t.line.text.to_string(),
                        f32::from(t.origin.x),
                        f32::from(t.origin.y),
                        f32::from(t.clip.origin.x),
                        f32::from(t.clip.size.width),
                        frozen,
                    )
                };
                s.cells = p.cells.iter().map(|t| rec(t, false)).collect();
                s.cells.extend(p.frozen_cells.iter().map(|t| rec(t, true)));
                s.over_text = p
                    .over_text
                    .iter()
                    .map(|t| t.line.text.to_string())
                    .collect();
                s.outlines = p
                    .outlines
                    .iter()
                    .map(|q| {
                        (
                            f32::from(q.bounds.origin.x),
                            f32::from(q.bounds.origin.y),
                            f32::from(q.bounds.size.width),
                            f32::from(q.bounds.size.height),
                        )
                    })
                    .collect();
                s.popup_row_height = p.popup.as_ref().map(|pp| f32::from(pp.hit.row_height));
                s.cells_top = f32::from(p.cells_bounds.origin.y);
                s.frozen_bottom = f32::from(p.scroll_bounds.origin.y);
            }
            p
        }
        fn paint(
            &mut self,
            id: Option<&GlobalElementId>,
            iid: Option<&InspectorElementId>,
            bounds: Bounds<Pixels>,
            rl: &mut (),
            pp: &mut Option<GridPrepaint>,
            window: &mut Window,
            cx: &mut App,
        ) {
            self.inner.paint(id, iid, bounds, rl, pp, window, cx)
        }
    }
}
