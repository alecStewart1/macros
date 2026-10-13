//! [`TerminalElement`]: the GPUI element that draws an embedded terminal.

use super::*;

/// The `(row, first_col, last_col)` cell spans (inclusive) a terminal
/// selection covers within the visible `rows` x `cols` viewport. Rows are
/// viewport-relative and may be negative (scrolled above) or past the bottom:
/// those are clipped, and a selection lying entirely outside the viewport yields
/// nothing (rather than a phantom band clamped onto row 0). Block selections
/// use the same columns on every row; a simple selection runs start..eol on its
/// first row, full width in between, and 0..end on its last — with a clipped
/// end row taking the full width it would have had.
fn terminal_selection_spans(
    sel: &crate::terminal::TermSelection,
    rows: usize,
    cols: usize,
) -> Vec<(usize, usize, usize)> {
    let mut spans = Vec::new();
    if rows == 0 || cols == 0 || sel.end_row < 0 || sel.start_row >= rows as i32 {
        return spans;
    }
    let first = sel.start_row.max(0) as usize;
    let last = (sel.end_row as usize).min(rows - 1);
    for r in first..=last {
        let (c0, c1) = if sel.is_block {
            (sel.start_col, sel.end_col)
        } else {
            let c0 = if sel.start_row == r as i32 {
                sel.start_col
            } else {
                0
            };
            let c1 = if sel.end_row == r as i32 {
                sel.end_col
            } else {
                cols - 1
            };
            (c0, c1)
        };
        let c1 = c1.min(cols - 1);
        if c1 < c0 {
            continue;
        }
        spans.push((r, c0, c1));
    }
    spans
}

/// The custom GPUI element that draws an embedded terminal grid. Like
/// [`EditorElement`] it shapes and paints text itself, but over a fixed
/// `cols × rows` cell grid read from the alacritty backend (via a plain-data
/// [`TermSnapshot`](crate::terminal::TermSnapshot)) rather than a rope.
pub struct TerminalElement {
    pub editor: Entity<Editor>,
    pub pane: usize,
    /// The backing terminal id.
    pub id: u64,
}

/// What `TerminalElement::prepaint` hands to `paint`: background fills, shaped
/// glyph rows, and the cursor quad.
pub struct TerminalPrepaint {
    background: PaintQuad,
    cell_backgrounds: Vec<PaintQuad>,
    /// Selection highlight quads, painted over cell backgrounds, under glyphs.
    selection: Vec<PaintQuad>,
    rows: Vec<(ShapedLine, Point<Pixels>)>,
    cursor: Option<PaintQuad>,
}

impl IntoElement for TerminalElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = TerminalPrepaint;

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
        // Terminal default colors: dedicated `terminal-*` faces, else the editor's
        // foreground/background.
        let default_fg = theme.color("terminal-foreground", theme.foreground());
        let default_bg = theme.color("terminal-background", theme.background());
        let cursor_color = theme.color("cursor", rgb(0x528bff).into());
        let selection_color = theme.color(
            "terminal-selection",
            theme.color("region", rgba(0x3a5fcd55).into()),
        );
        let dfg = hsla_to_rgb_u32(default_fg);
        let dbg = hsla_to_rgb_u32(default_bg);

        let settings = cx.global::<crate::settings::Settings>();
        let cursor_width = px(settings.cursor_width());

        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();
        let term_font = font(settings.font_family().to_string());
        let mk_run = |len: usize, color: Hsla| plain_run(len, term_font.clone(), color);
        // One monospace cell advance.
        let em = window
            .text_system()
            .shape_line(
                SharedString::from("0"),
                font_size,
                &[mk_run(1, default_fg)],
                None,
            )
            .x_for_index(1);

        // Cell grid that fits the viewport, and re-fit the backend to it.
        let cols = if em > px(0.) {
            (bounds.size.width / em) as usize
        } else {
            0
        };
        let rows = if line_height > px(0.) {
            (bounds.size.height / line_height) as usize
        } else {
            0
        };
        let id = self.id;
        self.editor.update(cx, |editor, _| {
            editor.terminal_resize_fit(
                id,
                cols.max(1),
                rows.max(1),
                f32::from(em).max(1.0) as u16,
                f32::from(line_height).max(1.0) as u16,
            );
        });

        let snapshot = self.editor.read(cx).terminal_snapshot(id, dfg, dbg);
        let background = fill(bounds, default_bg);
        let mut cell_backgrounds = Vec::new();
        let mut selection: Vec<PaintQuad> = Vec::new();
        let mut shaped_rows = Vec::new();
        let mut cursor = None;

        if let Some(snapshot) = snapshot {
            // Reassemble the sparse cell list into dense rows so column positions
            // line up (gaps are spaces over the default background).
            let cols = snapshot.cols;
            let rows = snapshot.rows;
            // Single flat grid (len rows*cols), indexed `r*cols + c`, instead of a
            // per-row Vec to avoid the per-row allocation each frame.
            let mut grid: Vec<Option<&crate::terminal::TermCell>> = vec![None; rows * cols];
            for cell in &snapshot.cells {
                if cell.row < rows && cell.col < cols {
                    grid[cell.row * cols + cell.col] = Some(cell);
                }
            }

            for (r, row_cells) in grid.chunks(cols).enumerate() {
                let y = bounds.top() + line_height * r as f32;
                // Background fills first.
                for (c, cell) in row_cells.iter().enumerate() {
                    if let Some(cell) = cell
                        && let Some(bg) = cell.bg
                    {
                        let x = bounds.left() + em * c as f32;
                        let w = if cell.wide { em * 2. } else { em };
                        cell_backgrounds.push(fill(
                            Bounds::new(point(x, y), size(w, line_height)),
                            Hsla::from(rgb(bg)),
                        ));
                    }
                }
                // Build the row as grid-anchored segments of styled runs. A
                // wide (CJK/emoji) glyph covers two columns but its shaped
                // advance is whatever the (often fallback) font says, so the
                // segment ends after it and the next one restarts exactly at
                // column `c + 2` — later text stays on the `em * col` grid. The
                // wide glyph's spacer column is skipped (it must not become an
                // extra space).
                let mut text = String::with_capacity(cols);
                let mut runs: Vec<TextRun> = Vec::with_capacity(cols);
                let mut seg_col = 0usize;
                let mut skip_spacer = false;
                for (c, cell) in row_cells.iter().enumerate() {
                    if skip_spacer {
                        skip_spacer = false;
                        seg_col = c + 1;
                        continue;
                    }
                    let (ch, color, bold, italic, underline) = match cell {
                        Some(c) => {
                            let ch = if c.ch == '\0' { ' ' } else { c.ch };
                            (ch, Hsla::from(rgb(c.fg)), c.bold, c.italic, c.underline)
                        }
                        None => (' ', default_fg, false, false, false),
                    };
                    let wide = cell.is_some_and(|c| c.wide);
                    text.push(ch);
                    let mut f = term_font.clone();
                    if bold {
                        f.weight = FontWeight::BOLD;
                    }
                    if italic {
                        f.style = FontStyle::Italic;
                    }
                    runs.push(TextRun {
                        len: ch.len_utf8(),
                        font: f,
                        color,
                        background_color: None,
                        underline: underline.then(|| UnderlineStyle {
                            thickness: px(1.),
                            color: Some(color),
                            wavy: false,
                        }),
                        strikethrough: None,
                    });
                    if wide {
                        let shaped = window.text_system().shape_line(
                            SharedString::from(std::mem::take(&mut text)),
                            font_size,
                            &std::mem::take(&mut runs),
                            None,
                        );
                        shaped_rows.push((shaped, point(bounds.left() + em * seg_col as f32, y)));
                        skip_spacer = true;
                    }
                }
                if !text.is_empty() {
                    let shaped = window.text_system().shape_line(
                        SharedString::from(text),
                        font_size,
                        &runs,
                        None,
                    );
                    shaped_rows.push((shaped, point(bounds.left() + em * seg_col as f32, y)));
                }
            }

            // Selection highlight: one quad per visible row the range covers.
            if let Some(sel) = &snapshot.selection {
                for (r, c0, c1) in terminal_selection_spans(sel, rows, cols) {
                    let x0 = bounds.left() + em * c0 as f32;
                    let x1 = bounds.left() + em * (c1 + 1) as f32;
                    let y = bounds.top() + line_height * r as f32;
                    selection.push(fill(
                        Bounds::from_corners(point(x0, y), point(x1, y + line_height)),
                        selection_color,
                    ));
                }
            }

            if let Some(cur) = snapshot.cursor {
                let x = bounds.left() + em * cur.col as f32;
                let y = bounds.top() + line_height * cur.row as f32;
                let (w, mut color) = if cur.block {
                    (em, cursor_color)
                } else {
                    (cursor_width, cursor_color)
                };
                // A block cursor is translucent so the glyph under it stays legible.
                if cur.block {
                    color.a *= BLOCK_CURSOR_ALPHA;
                }
                cursor = Some(fill(Bounds::new(point(x, y), size(w, line_height)), color));
            }
        }

        // Report the painted geometry so the editor can map mouse positions to
        // grid cells for selection.
        self.editor.update(cx, |editor, _| {
            editor.set_terminal_geometry(
                id,
                point(bounds.left(), bounds.top()),
                em,
                line_height,
                cols.max(1),
                rows.max(1),
            );
        });

        TerminalPrepaint {
            background,
            cell_backgrounds,
            selection,
            rows: shaped_rows,
            cursor,
        }
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

        // Claim the IME/typed-text input handler for the selected terminal pane,
        // just like `EditorElement`. Live terminal keys are grabbed in
        // `on_key_down` (which sends PTY bytes and stops propagation before the
        // IME), so this only matters when an overlay is open over the terminal —
        // e.g. helm/minibuffer, which let plain text fall through to the IME.
        // Without a handler here that text would be dropped, so typing to filter
        // M-x wouldn't work after the shell exits and the terminal is still shown.
        if self.editor.read(cx).pane_is_selected(self.pane) {
            window.handle_input(
                &focus_handle,
                ElementInputHandler::new(bounds, self.editor.clone()),
                cx,
            );
        }

        let line_height = window.line_height();
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            window.paint_quad(prepaint.background.clone());
            for quad in prepaint.cell_backgrounds.drain(..) {
                window.paint_quad(quad);
            }
            for quad in prepaint.selection.drain(..) {
                window.paint_quad(quad);
            }
            if focus_handle.is_focused(window)
                && let Some(cursor) = prepaint.cursor.take()
            {
                window.paint_quad(cursor);
            }
            for (line, origin) in prepaint.rows.drain(..) {
                line.paint(origin, line_height, window, cx).ok();
            }
        });
        // Report bounds so click-to-focus routes to this pane like any other.
        let pane = self.pane;
        self.editor.update(cx, |editor, _| {
            editor.report_pane_bounds(pane, bounds);
        });
    }
}

/// Pack a [`Hsla`] into a `0xRRGGBB` value for the terminal color resolver.
fn hsla_to_rgb_u32(color: Hsla) -> u32 {
    let rgba = gpui::Rgba::from(color);
    let to_u8 = |v: f32| (v.clamp(0., 1.) * 255.0).round() as u32;
    (to_u8(rgba.r) << 16) | (to_u8(rgba.g) << 8) | to_u8(rgba.b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_selection_outside_viewport_paints_nothing() {
        use crate::terminal::TermSelection;
        let sel = |start_row, start_col, end_row, end_col| TermSelection {
            start_row,
            start_col,
            end_row,
            end_col,
            is_block: false,
        };
        // Entirely above the viewport: no phantom band on row 0.
        assert!(terminal_selection_spans(&sel(-5, 3, -2, 4), 10, 80).is_empty());
        // Entirely below.
        assert!(terminal_selection_spans(&sel(12, 0, 14, 4), 10, 80).is_empty());
        // Starting above: row 0 is a full-width continuation up to the end.
        assert_eq!(
            terminal_selection_spans(&sel(-3, 5, 1, 4), 10, 80),
            vec![(0, 0, 79), (1, 0, 4)]
        );
        // Running past the bottom: the last visible row is full width.
        assert_eq!(
            terminal_selection_spans(&sel(8, 2, 20, 4), 10, 80),
            vec![(8, 2, 79), (9, 0, 79)]
        );
    }
}
