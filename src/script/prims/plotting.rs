//! Text plotting (src/textplot.rs, driven by scheme/plot.scm): charts
//! rendered to text plus faces. Pure functions: no editor state, no command
//! queue.

use steel::rvals::SteelVal;

use super::*;

pub(in crate::script) fn register(reg: &mut Registrar, _ctx: &EngineShared) {
    reg.section(Section::Plotting);
    // --- Text plotting (src/textplot.rs, driven by scheme/plot.scm) --------
    // Pure functions: no editor state, no command queue.
    {
        // `(textplot-render width height mode ops)` → one `(text spans)` per
        // cell row, spans being `(start end face)` BYTE offsets into the row.
        // `mode` ∈ braille|block|quadrant|half; `ops` is a list of
        //   ("point" x y face) ("line" x0 y0 x1 y1 face)
        //   ("rect" x0 y0 x1 y1 face) ("polyline" face (x0 y0 x1 y1 …))
        //   ("text" col row string face)
        // in dot coordinates (origin top-left, y down) — `text` in cells.
        // A face of #f draws uncolored. Malformed ops are skipped.
        reg.register_fn(
            "textplot-render",
            |width: usize, height: usize, mode: SteelVal, ops: SteelVal| -> SteelVal {
                let mode = steel_string(&mode)
                    .and_then(|m| m.parse::<crate::textplot::Mode>().ok())
                    .unwrap_or(crate::textplot::Mode::Braille);
                // Cap the grid so a bad call can't allocate gigabytes.
                let mut canvas =
                    crate::textplot::Canvas::new(width.min(2000), height.min(2000), mode);
                for op in steel_list(&ops).unwrap_or_default() {
                    textplot_op(&mut canvas, &op);
                }
                SteelVal::ListV(
                    canvas
                        .render()
                        .into_iter()
                        .map(|row| {
                            let spans = row
                                .spans
                                .into_iter()
                                .map(|(s, e, f)| {
                                    SteelVal::ListV(
                                        vec![
                                            SteelVal::IntV(s as isize),
                                            SteelVal::IntV(e as isize),
                                            SteelVal::StringV(f.into()),
                                        ]
                                        .into(),
                                    )
                                })
                                .collect();
                            SteelVal::ListV(
                                vec![SteelVal::StringV(row.text.into()), SteelVal::ListV(spans)]
                                    .into(),
                            )
                        })
                        .collect(),
                )
            },
        );
        // `(textplot-sparkline values lo hi)` → one `▁…█` glyph per value.
        reg.register_fn(
            "textplot-sparkline",
            |values: SteelVal, lo: SteelVal, hi: SteelVal| -> String {
                let values: Vec<f64> = steel_list(&values)
                    .unwrap_or_default()
                    .iter()
                    .map(|v| steel_f64(v).unwrap_or(f64::NAN))
                    .collect();
                crate::textplot::sparkline(
                    &values,
                    steel_f64(&lo).unwrap_or(0.0),
                    steel_f64(&hi).unwrap_or(0.0),
                )
            },
        );
        // `(textplot-hbar value max width)` → bar glyphs at ⅛-cell resolution.
        reg.register_fn(
            "textplot-hbar",
            |value: SteelVal, max: SteelVal, width: usize| -> String {
                crate::textplot::hbar(
                    steel_f64(&value).unwrap_or(0.0),
                    steel_f64(&max).unwrap_or(0.0),
                    width.min(10_000),
                )
            },
        );
        // `(textplot-ticks lo hi n)` → `((value label) …)`, ~n nice ticks.
        reg.register_fn(
            "textplot-ticks",
            |lo: SteelVal, hi: SteelVal, n: usize| -> SteelVal {
                let (Some(lo), Some(hi)) = (steel_f64(&lo), steel_f64(&hi)) else {
                    return SteelVal::ListV(Vec::<SteelVal>::new().into());
                };
                SteelVal::ListV(
                    crate::textplot::ticks(lo, hi, n.clamp(1, 100))
                        .into_iter()
                        .map(|(v, label)| {
                            SteelVal::ListV(
                                vec![SteelVal::NumV(v), SteelVal::StringV(label.into())].into(),
                            )
                        })
                        .collect(),
                )
            },
        );
        // `(textplot-nice-range lo hi n)` → `(lo hi)` widened to the tick step.
        reg.register_fn(
            "textplot-nice-range",
            |lo: SteelVal, hi: SteelVal, n: usize| -> SteelVal {
                let lo = steel_f64(&lo).unwrap_or(0.0);
                let hi = steel_f64(&hi).unwrap_or(0.0);
                let (a, b) = crate::textplot::nice_range(lo, hi, n.clamp(1, 100));
                SteelVal::ListV(vec![SteelVal::NumV(a), SteelVal::NumV(b)].into())
            },
        );
        // `(textplot-format v)` → compact label: 950, 12.3k, 3.4M, 0.25.
        reg.register_fn("textplot-format", |v: SteelVal| -> String {
            steel_f64(&v)
                .map(crate::textplot::format_compact)
                .unwrap_or_default()
        });
    }
}

/// Apply one `textplot-render` drawing op to `canvas`. Coordinates may be ints
/// or floats (rounded); a face of `#f` (or any non-string) draws uncolored.
/// Malformed ops are skipped rather than failing the whole render.
fn textplot_op(canvas: &mut crate::textplot::Canvas, op: &SteelVal) {
    let Some(items) = steel_list(op) else { return };
    let Some(kind) = items.first().and_then(steel_string) else {
        return;
    };
    // NaN/±inf coordinates make the op a no-op (NaN would otherwise cast to
    // 0 and draw at the origin). Finite but huge values saturate, and the
    // canvas clips them.
    let n = |i: usize| {
        items
            .get(i)
            .and_then(steel_f64)
            .filter(|v| v.is_finite())
            .map(|v| v.round() as i64)
    };
    let face = |i: usize| items.get(i).and_then(steel_string);
    match kind.as_str() {
        "point" => {
            if let (Some(x), Some(y)) = (n(1), n(2)) {
                canvas.set(x, y, face(3).as_deref());
            }
        }
        "line" | "rect" => {
            if let (Some(x0), Some(y0), Some(x1), Some(y1)) = (n(1), n(2), n(3), n(4)) {
                let f = face(5);
                if kind == "line" {
                    canvas.line(x0, y0, x1, y1, f.as_deref());
                } else {
                    canvas.rect(x0, y0, x1, y1, f.as_deref());
                }
            }
        }
        "polyline" => {
            let f = face(1);
            let coords: Vec<Option<f64>> = items
                .get(2)
                .and_then(steel_list)
                .unwrap_or_default()
                .iter()
                .map(steel_f64)
                .collect();
            // A non-finite (or non-numeric) point breaks the line into runs,
            // the way a gap in the data should look.
            let points: Vec<Option<(i64, i64)>> = coords
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&[x, y]| match (x, y) {
                    (Some(x), Some(y)) if x.is_finite() && y.is_finite() => {
                        Some((x.round() as i64, y.round() as i64))
                    }
                    _ => None,
                })
                .collect();
            for run in points.split(Option::is_none) {
                let run: Vec<(i64, i64)> = run.iter().flatten().copied().collect();
                if let [(x, y)] = run.as_slice() {
                    canvas.set(*x, *y, f.as_deref());
                }
                for pair in run.windows(2) {
                    let ((x0, y0), (x1, y1)) = (pair[0], pair[1]);
                    canvas.line(x0, y0, x1, y1, f.as_deref());
                }
            }
        }
        "text" => {
            if let (Some(col), Some(row), Some(text)) =
                (n(1), n(2), items.get(3).and_then(steel_string))
            {
                canvas.text(col, row, &text, face(4).as_deref());
            }
        }
        _ => {}
    }
}
