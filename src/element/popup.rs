//! The completion popup [`EditorElement`] (and the grid view) draw below the
//! cursor: candidate rows plus the doc side-panel.

use super::*;

/// Inner padding of the completion popup (and its doc side-panel).
const POPUP_PAD: Pixels = px(8.);
/// Column (in characters) at which the completion doc side-panel word-wraps.
const DOC_WRAP_COLS: usize = 64;
/// Maximum rows shown in the completion doc side-panel.
const DOC_MAX_ROWS: usize = 16;

/// A laid-out completion popup: background/border/selection quads plus the
/// shaped candidate lines and where to paint each.
pub(crate) struct PopupLayout {
    pub(crate) quads: Vec<PaintQuad>,
    pub(crate) labels: Vec<(ShapedLine, Point<Pixels>)>,
    pub(crate) hit: crate::editor::PopupHit,
}

/// Lay out the completion popup below `anchor` (the cursor cell's top-left):
/// shape each visible candidate, size the box to the widest, and produce the
/// background/border/selection quads plus positioned label lines.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_popup(
    anchor: Point<Pixels>,
    view: CompletionView,
    max_rows: usize,
    show_detail: bool,
    show_kind: bool,
    font: &Font,
    font_size: Pixels,
    line_height: Pixels,
    editor_bounds: Bounds<Pixels>,
    theme: &Theme,
    window: &mut Window,
) -> PopupLayout {
    let total = view.items.len();
    // Scroll the window so the selected row stays visible.
    let start = if view.selected >= max_rows {
        view.selected + 1 - max_rows
    } else {
        0
    };
    let end = (start + max_rows).min(total);
    let visible = &view.items[start..end];

    let fg = theme.color("completion-fg", theme.foreground());
    let bg = theme.color(
        "completion-bg",
        theme.color("mode-line-bg", theme.background()),
    );
    let selected_bg = theme.color(
        "completion-selected-bg",
        theme.color("region", theme.foreground()),
    );
    let selected_fg = theme.color("completion-selected-fg", fg);
    let border = theme.color("completion-border", theme.color("dim", theme.foreground()));
    let dim = theme.color("dim", fg);
    let kind_color = theme.color("completion-kind", theme.color("type", dim));

    let run = |len: usize, color: Hsla| plain_run(len, font.clone(), color);

    let mut shaped = Vec::new();
    let mut max_width = px(0.);
    for (i, row) in visible.iter().enumerate() {
        // Each row is `label  detail  kind`, with the label in the foreground and
        // the (optional, Scheme-gated) signature and kind dimmed.
        let label_color = if start + i == view.selected {
            selected_fg
        } else {
            fg
        };
        let mut text = row.label.clone();
        let mut runs = vec![run(row.label.len(), label_color)];
        if show_detail && let Some(detail) = &row.detail {
            let segment = format!("  {detail}");
            runs.push(run(segment.len(), dim));
            text.push_str(&segment);
        }
        if show_kind && let Some(kind) = &row.kind {
            let segment = format!("  {kind}");
            runs.push(run(segment.len(), kind_color));
            text.push_str(&segment);
        }
        let length = text.len();
        // `shape_line` panics (aborting the editor) on an embedded newline. Items
        // are sanitized at parse time, but guard here too so no future text source
        // can crash the popup. Same byte length, so `runs` stay valid.
        debug_assert!(!text.contains('\n'), "completion row text has a newline");
        let text: String = text
            .chars()
            .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
            .collect();
        let line =
            window
                .text_system()
                .shape_line(SharedString::from(text), font_size, &runs, None);
        let width = line.x_for_index(length);
        if width > max_width {
            max_width = width;
        }
        shaped.push(line);
    }

    let padding = POPUP_PAD;
    let width = max_width + padding + padding;
    let rows = shaped.len();
    let height = line_height * rows as f32;
    // Default below the cursor line. Flip ABOVE it when the box would spill past
    // the pane's bottom edge (off the window) AND there's more room above the
    // cursor than below — so it stays on-screen instead of being clipped. When
    // flipped, the box's bottom rests just above the cursor line (`anchor.y`).
    let below_top = anchor.y + line_height;
    let space_below = editor_bounds.bottom() - below_top;
    let space_above = anchor.y - editor_bounds.top();
    let top = if height > space_below && space_above > space_below {
        anchor.y - height
    } else {
        below_top
    };
    // Keep the box within the editor horizontally.
    let max_left = editor_bounds.right() - width;
    let mut left = anchor.x;
    if left > max_left {
        left = max_left;
    }
    if left < editor_bounds.left() {
        left = editor_bounds.left();
    }

    let mut quads = Vec::new();
    // Border, then the background inset by 1px.
    quads.push(fill(
        Bounds::from_corners(
            point(left - px(1.), top - px(1.)),
            point(left + width + px(1.), top + height + px(1.)),
        ),
        border,
    ));
    quads.push(fill(
        Bounds::from_corners(point(left, top), point(left + width, top + height)),
        bg,
    ));
    // Selected-row highlight.
    let selected_row = view.selected - start;
    let sy = top + line_height * selected_row as f32;
    quads.push(fill(
        Bounds::from_corners(point(left, sy), point(left + width, sy + line_height)),
        selected_bg,
    ));

    let hit = crate::editor::PopupHit {
        bounds: Bounds::from_corners(point(left, top), point(left + width, top + height)),
        row_height: line_height,
        start,
        rows,
    };

    let mut labels: Vec<(ShapedLine, Point<Pixels>)> = shaped
        .into_iter()
        .enumerate()
        .map(|(i, line)| (line, point(left + padding, top + line_height * i as f32)))
        .collect();

    // Documentation side-panel for the selected item (resolved lazily). Sits to
    // the right of the popup, or below it when there isn't room.
    if let Some(doc) = view.doc.as_ref() {
        let doc_fg = theme.color("completion-doc-fg", fg);
        let doc_bg = theme.color("completion-doc-bg", bg);
        let mut doc_lines: Vec<String> = Vec::new();
        for raw in doc.lines() {
            if raw.trim().is_empty() {
                doc_lines.push(String::new());
            } else {
                let mut cur = String::new();
                let mut cur_cols = 0usize;
                for word in raw.split_whitespace() {
                    // Wrap by character columns, not UTF-8 bytes, so non-ASCII
                    // docs don't wrap early.
                    let word_cols = word.chars().count();
                    if cur.is_empty() {
                        cur = word.to_string();
                        cur_cols = word_cols;
                    } else if cur_cols + 1 + word_cols <= DOC_WRAP_COLS {
                        cur.push(' ');
                        cur.push_str(word);
                        cur_cols += 1 + word_cols;
                    } else {
                        doc_lines.push(std::mem::take(&mut cur));
                        cur = word.to_string();
                        cur_cols = word_cols;
                    }
                }
                if !cur.is_empty() {
                    doc_lines.push(cur);
                }
            }
            if doc_lines.len() >= DOC_MAX_ROWS {
                break;
            }
        }
        doc_lines.truncate(DOC_MAX_ROWS);
        if !doc_lines.is_empty() {
            let mut shaped_doc = Vec::new();
            let mut doc_w = px(0.);
            for dl in &doc_lines {
                let line = window.text_system().shape_line(
                    SharedString::from(dl.clone()),
                    font_size,
                    &[run(dl.len(), doc_fg)],
                    None,
                );
                let w = line.x_for_index(dl.len());
                if w > doc_w {
                    doc_w = w;
                }
                shaped_doc.push(line);
            }
            let doc_width = doc_w + padding + padding;
            let doc_height = line_height * doc_lines.len() as f32;
            let gap = px(6.);
            let (dleft, dtop) = if left + width + gap + doc_width <= editor_bounds.right() {
                (left + width + gap, top)
            } else {
                (left, top + height + gap)
            };
            quads.push(fill(
                Bounds::from_corners(
                    point(dleft - px(1.), dtop - px(1.)),
                    point(dleft + doc_width + px(1.), dtop + doc_height + px(1.)),
                ),
                border,
            ));
            quads.push(fill(
                Bounds::from_corners(
                    point(dleft, dtop),
                    point(dleft + doc_width, dtop + doc_height),
                ),
                doc_bg,
            ));
            for (i, line) in shaped_doc.into_iter().enumerate() {
                labels.push((line, point(dleft + padding, dtop + line_height * i as f32)));
            }
        }
    }

    PopupLayout { quads, labels, hit }
}
