//! Text plotting: rasterize chart drawing operations onto a character-cell
//! grid, for the `textplot-*` Scheme primitives behind `scheme/plot.scm`.
//!
//! A [`Canvas`] is `width × height` character cells, each subdivided into a
//! mode-dependent grid of "dots" (braille 2×4, quadrant 2×2, block 1×8 levels,
//! half-block 1×2). Drawing happens in dot coordinates with the origin at the
//! top-left and y growing downward; Scheme does the data → dot scaling.
//!
//! Rendering yields one [`Row`] per cell row: the row's text plus face spans as
//! BYTE offsets into that row (the unit `overlay-set-faces` takes). Computing
//! them here means Scheme never does char/byte arithmetic on the multibyte
//! glyphs these charts are made of.
//!
//! Colors are face names. A cell has one foreground face — the last op that
//! touched it wins — except in half-block mode, where the top and bottom
//! pixels each keep their own face: the bottom one is emitted as the face
//! name with a `-bg` suffix, which themes define as a *background* color
//! (`set-face-background`).

/// How each character cell is subdivided.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// U+2800 braille patterns: 2 × 4 dots per cell.
    Braille,
    /// `▁▂▃▄▅▆▇█`: one column, 8 fill levels (filled from the lowest dot set
    /// up to the highest — for bars and columns, not free-form shapes).
    Block,
    /// `▘▝▀▖▌▞▛▗▚▐▜▄▙▟█`: 2 × 2 dots per cell.
    Quadrant,
    /// `▀` / `▄` / `█` with independent top/bottom colors: 1 × 2 pixels.
    HalfBlock,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        match s {
            "braille" => Some(Mode::Braille),
            "block" => Some(Mode::Block),
            "quadrant" => Some(Mode::Quadrant),
            "half" | "half-block" | "halfblock" => Some(Mode::HalfBlock),
            _ => None,
        }
    }

    /// Dots per cell, `(columns, rows)`.
    pub fn dots(self) -> (usize, usize) {
        match self {
            Mode::Braille => (2, 4),
            Mode::Block => (1, 8),
            Mode::Quadrant => (2, 2),
            Mode::HalfBlock => (1, 2),
        }
    }
}

/// Braille dot bit for (x, y) within a cell, per the Unicode dot numbering.
const BRAILLE_BITS: [[u8; 2]; 4] = [[0x01, 0x08], [0x02, 0x10], [0x04, 0x20], [0x40, 0x80]];

/// Quadrant glyph by mask: bit 0 top-left, 1 top-right, 2 bottom-left, 3 bottom-right.
const QUADRANTS: [char; 16] = [
    ' ', '▘', '▝', '▀', '▖', '▌', '▞', '▛', '▗', '▚', '▐', '▜', '▄', '▙', '▟', '█',
];

/// Vertical eighths, 1..=8 filled (index 0 = one eighth).
pub const EIGHTHS_UP: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Horizontal eighths, 1..=7 (a full cell is `█`).
const EIGHTHS_RIGHT: [char; 7] = ['▏', '▎', '▍', '▌', '▋', '▊', '▉'];

#[derive(Clone, Default)]
struct Cell {
    /// Dot mask; bit layout depends on the mode (see `glyph`).
    bits: u8,
    /// Foreground face (half-block: the TOP pixel's face).
    face: Option<u16>,
    /// Half-block only: the bottom pixel's face.
    bottom: Option<u16>,
    /// A `text` op's character, which replaces the dot glyph.
    text: Option<char>,
}

/// One rendered row: its text and `(start, end, face)` byte spans within it.
#[derive(Debug, PartialEq)]
pub struct Row {
    pub text: String,
    pub spans: Vec<(usize, usize, String)>,
}

pub struct Canvas {
    mode: Mode,
    width: usize,
    height: usize,
    cells: Vec<Cell>,
    /// Interned face names; cells store indices.
    faces: Vec<String>,
}

impl Canvas {
    pub fn new(width: usize, height: usize, mode: Mode) -> Canvas {
        Canvas {
            mode,
            width,
            height,
            cells: vec![Cell::default(); width * height],
            faces: Vec::new(),
        }
    }

    /// The drawable area in dots, `(width, height)`.
    pub fn dot_size(&self) -> (usize, usize) {
        let (dx, dy) = self.mode.dots();
        (self.width * dx, self.height * dy)
    }

    fn intern(&mut self, face: Option<&str>) -> Option<u16> {
        let face = face.filter(|f| !f.is_empty())?;
        if let Some(i) = self.faces.iter().position(|f| f == face) {
            return Some(i as u16);
        }
        self.faces.push(face.to_string());
        Some((self.faces.len() - 1) as u16)
    }

    /// Set one dot. Out-of-bounds dots are clipped silently.
    pub fn set(&mut self, x: i64, y: i64, face: Option<&str>) {
        let (w, h) = self.dot_size();
        if x < 0 || y < 0 || x as usize >= w || y as usize >= h {
            return;
        }
        let (x, y) = (x as usize, y as usize);
        let (dx, dy) = self.mode.dots();
        let (cx, cy) = (x / dx, y / dy);
        let (sx, sy) = (x % dx, y % dy);
        let face = self.intern(face);
        let mode = self.mode;
        let cell = &mut self.cells[cy * self.width + cx];
        match mode {
            Mode::Braille => cell.bits |= BRAILLE_BITS[sy][sx],
            Mode::Quadrant => cell.bits |= 1 << (sy * 2 + sx),
            // Bit k = the k-th eighth counted from the BOTTOM of the cell.
            Mode::Block => cell.bits |= 1 << (dy - 1 - sy),
            Mode::HalfBlock => {
                cell.bits |= 1 << sy;
                if sy == 1 {
                    cell.bottom = face;
                    return;
                }
            }
        }
        cell.face = face;
    }

    /// A straight line between two dots (Bresenham), inclusive.
    pub fn line(&mut self, x0: i64, y0: i64, x1: i64, y1: i64, face: Option<&str>) {
        let (mut x, mut y) = (x0, y0);
        let dx = (x1 - x0).abs();
        let dy = -(y1 - y0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;
        // Bound the walk so absurd coordinates can't spin forever.
        let limit = (dx - dy) as usize + 1;
        for _ in 0..=limit.min(1 << 20) {
            self.set(x, y, face);
            if x == x1 && y == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                err += dx;
                y += sy;
            }
        }
    }

    /// A filled rectangle between two corner dots, inclusive. Clipped to the
    /// canvas before filling, so a huge rect costs at most the canvas size.
    pub fn rect(&mut self, x0: i64, y0: i64, x1: i64, y1: i64, face: Option<&str>) {
        let (w, h) = self.dot_size();
        let clamp = |v: i64, max: usize| v.clamp(0, max as i64 - 1);
        if w == 0 || h == 0 {
            return;
        }
        let (xa, xb) = (x0.min(x1), x0.max(x1));
        let (ya, yb) = (y0.min(y1), y0.max(y1));
        if xb < 0 || yb < 0 || xa >= w as i64 || ya >= h as i64 {
            return;
        }
        for y in clamp(ya, h)..=clamp(yb, h) {
            for x in clamp(xa, w)..=clamp(xb, w) {
                self.set(x, y, face);
            }
        }
    }

    /// Write `text` starting at CELL (col, row); characters replace the dot
    /// glyphs under them. Clipped at the right edge.
    pub fn text(&mut self, col: i64, row: i64, text: &str, face: Option<&str>) {
        if row < 0 || row as usize >= self.height {
            return;
        }
        let face = self.intern(face);
        for (i, ch) in text.chars().enumerate() {
            let c = col + i as i64;
            if c < 0 {
                continue;
            }
            if c as usize >= self.width {
                break;
            }
            let cell = &mut self.cells[row as usize * self.width + c as usize];
            cell.text = Some(ch);
            cell.face = face;
            cell.bottom = None;
        }
    }

    /// The glyph for one cell plus its (foreground, background) face indices.
    fn glyph(&self, cell: &Cell) -> (char, Option<u16>, Option<u16>) {
        if let Some(ch) = cell.text {
            return (ch, cell.face, None);
        }
        if cell.bits == 0 {
            return (' ', None, None);
        }
        match self.mode {
            Mode::Braille => (
                char::from_u32(0x2800 + cell.bits as u32).unwrap_or(' '),
                cell.face,
                None,
            ),
            Mode::Quadrant => (QUADRANTS[(cell.bits & 0xF) as usize], cell.face, None),
            Mode::Block => {
                // Filled up to the highest set eighth.
                let level = 8 - cell.bits.leading_zeros() as usize;
                (EIGHTHS_UP[level - 1], cell.face, None)
            }
            Mode::HalfBlock => match (cell.bits & 1 != 0, cell.bits & 2 != 0) {
                (true, true) if cell.face == cell.bottom => ('█', cell.face, None),
                (true, true) => ('▀', cell.face, cell.bottom),
                (true, false) => ('▀', cell.face, None),
                _ => ('▄', cell.bottom, None),
            },
        }
    }

    /// Render every row. Adjacent cells sharing a face merge into one span;
    /// background faces come out as `<face>-bg`.
    pub fn render(&self) -> Vec<Row> {
        (0..self.height)
            .map(|r| {
                let mut text = String::with_capacity(self.width * 3);
                let mut fg: Vec<(usize, usize, u16)> = Vec::new();
                let mut bg: Vec<(usize, usize, u16)> = Vec::new();
                for c in 0..self.width {
                    let (ch, f, b) = self.glyph(&self.cells[r * self.width + c]);
                    let start = text.len();
                    text.push(ch);
                    let end = text.len();
                    // A blank cell carries no color, so it breaks a run.
                    if ch != ' ' {
                        push_span(&mut fg, start, end, f);
                    }
                    push_span(&mut bg, start, end, b);
                }
                let mut spans: Vec<(usize, usize, String)> = fg
                    .into_iter()
                    .map(|(s, e, f)| (s, e, self.faces[f as usize].clone()))
                    .collect();
                spans.extend(
                    bg.into_iter()
                        .map(|(s, e, f)| (s, e, format!("{}-bg", self.faces[f as usize]))),
                );
                Row { text, spans }
            })
            .collect()
    }
}

/// Extend the last span when it ends here with the same face; else start one.
fn push_span(spans: &mut Vec<(usize, usize, u16)>, start: usize, end: usize, face: Option<u16>) {
    let Some(face) = face else { return };
    match spans.last_mut() {
        Some(last) if last.1 == start && last.2 == face => last.1 = end,
        _ => spans.push((start, end, face)),
    }
}

/// One `▁…█` glyph per value, scaled between `lo` and `hi`. Non-finite values
/// render as a space; a flat range renders mid-height.
pub fn sparkline(values: &[f64], lo: f64, hi: f64) -> String {
    values
        .iter()
        .map(|&v| {
            if !v.is_finite() {
                return ' ';
            }
            if lo.is_nan() || hi.is_nan() || hi <= lo {
                return EIGHTHS_UP[3];
            }
            let t = ((v - lo) / (hi - lo)).clamp(0.0, 1.0);
            EIGHTHS_UP[(t * 7.0).round() as usize]
        })
        .collect()
}

/// A horizontal bar `width` cells long at full scale, with ⅛-cell resolution.
/// Returns just the bar glyphs (no padding).
pub fn hbar(value: f64, max: f64, width: usize) -> String {
    if max.is_nan() || max <= 0.0 || !value.is_finite() || value <= 0.0 {
        return String::new();
    }
    let eighths = ((value / max).min(1.0) * width as f64 * 8.0).round() as usize;
    let mut s = "█".repeat(eighths / 8);
    if !eighths.is_multiple_of(8) {
        s.push(EIGHTHS_RIGHT[eighths % 8 - 1]);
    }
    s
}

/// "Nice" axis ticks covering `[lo, hi]`, about `n` of them, on a 1/2/5 × 10ᵏ
/// step. Each is `(value, label)`, labels sharing one decimal count so a
/// column of them lines up.
pub fn ticks(lo: f64, hi: f64, n: usize) -> Vec<(f64, String)> {
    if !lo.is_finite() || !hi.is_finite() {
        return Vec::new();
    }
    let (lo, hi) = if hi > lo {
        (lo, hi)
    } else {
        (lo - 0.5, lo + 0.5)
    };
    let step = nice_step((hi - lo) / n.max(1) as f64);
    let decimals = (-step.log10().floor()).max(0.0) as usize;
    let first = (lo / step).ceil() as i64;
    let last = (hi / step).floor() as i64;
    (first..=last)
        .take(1000)
        .map(|k| {
            let v = k as f64 * step;
            (v, format_number(v, decimals))
        })
        .collect()
}

fn nice_step(raw: f64) -> f64 {
    let mag = 10f64.powf(raw.log10().floor());
    let frac = raw / mag;
    let nice = if frac <= 1.0 {
        1.0
    } else if frac <= 2.0 {
        2.0
    } else if frac <= 5.0 {
        5.0
    } else {
        10.0
    };
    nice * mag
}

/// Fixed-decimal formatting with no `-0`.
pub fn format_number(v: f64, decimals: usize) -> String {
    let s = format!("{v:.decimals$}");
    if s.trim_start_matches('-')
        .chars()
        .all(|c| c == '0' || c == '.')
    {
        s.trim_start_matches('-').to_string()
    } else {
        s
    }
}

/// Compact human formatting for labels: `950`, `1.2k`, `3.4M`, `0.25`.
pub fn format_compact(v: f64) -> String {
    if !v.is_finite() {
        return String::new();
    }
    let a = v.abs();
    let (n, suffix) = if a >= 1e12 {
        (v / 1e12, "T")
    } else if a >= 1e9 {
        (v / 1e9, "G")
    } else if a >= 1e6 {
        (v / 1e6, "M")
    } else if a >= 1e4 {
        (v / 1e3, "k")
    } else {
        (v, "")
    };
    let decimals = if suffix.is_empty() {
        if n.fract() == 0.0 {
            0
        } else if a >= 100.0 {
            1
        } else {
            2
        }
    } else if n.abs() >= 100.0 {
        0
    } else {
        1
    };
    let s = format_number(n, decimals);
    // Trim trailing zeros after a decimal point ("2.50" → "2.5", "3.0" → "3").
    let s = if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    };
    format!("{s}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(c: &Canvas) -> Vec<String> {
        c.render().into_iter().map(|r| r.text).collect()
    }

    #[test]
    fn braille_dots_map_to_unicode_bits() {
        let mut c = Canvas::new(2, 1, Mode::Braille);
        // Left column of cell 0 fully set → dots 1,2,3,7.
        for y in 0..4 {
            c.set(0, y, Some("a"));
        }
        c.set(3, 3, Some("a")); // cell 1, right column, bottom → dot 8
        assert_eq!(texts(&c), vec!["⡇⢀"]);
    }

    #[test]
    fn line_and_clipping() {
        let mut c = Canvas::new(2, 1, Mode::Braille);
        c.line(0, 0, 3, 3, Some("s"));
        c.line(-10, 0, -5, 0, Some("s")); // fully off-canvas: no effect
        assert_eq!(texts(&c), vec!["⠑⢄"]);
    }

    #[test]
    fn quadrant_and_block_glyphs() {
        let mut q = Canvas::new(1, 1, Mode::Quadrant);
        q.set(0, 0, None);
        q.set(1, 1, None);
        assert_eq!(texts(&q), vec!["▚"]);

        // A column filled from the bottom three eighths up.
        let mut b = Canvas::new(2, 2, Mode::Block);
        b.rect(0, 13, 0, 15, Some("bar")); // bottom cell, 3/8
        b.rect(1, 4, 1, 15, Some("bar")); // full bottom cell + top half of top
        assert_eq!(texts(&b), vec![" ▄", "▃█"]);
    }

    #[test]
    fn half_block_keeps_two_faces_per_cell() {
        let mut c = Canvas::new(3, 1, Mode::HalfBlock);
        c.set(0, 0, Some("hot"));
        c.set(0, 1, Some("cold"));
        c.set(1, 0, Some("hot"));
        c.set(1, 1, Some("hot"));
        c.set(2, 1, Some("cold"));
        let row = &c.render()[0];
        assert_eq!(row.text, "▀█▄");
        assert_eq!(
            row.spans,
            vec![
                (0, 6, "hot".to_string()),
                (6, 9, "cold".to_string()),
                (0, 3, "cold-bg".to_string()),
            ]
        );
    }

    #[test]
    fn spans_are_byte_offsets_and_merge() {
        let mut c = Canvas::new(4, 1, Mode::Braille);
        c.set(0, 0, Some("a"));
        c.set(2, 0, Some("a"));
        c.set(6, 0, Some("b"));
        c.text(3, 0, "x", Some("label"));
        let row = &c.render()[0];
        assert_eq!(row.text, "⠁⠁ x");
        // Braille glyphs are 3 bytes; the blank cell splits the runs.
        assert_eq!(
            row.spans,
            vec![(0, 6, "a".to_string()), (7, 8, "label".to_string())]
        );
    }

    #[test]
    fn last_face_wins_in_a_shared_cell() {
        let mut c = Canvas::new(1, 1, Mode::Braille);
        c.set(0, 0, Some("a"));
        c.set(1, 0, Some("b"));
        assert_eq!(c.render()[0].spans, vec![(0, 3, "b".to_string())]);
    }

    #[test]
    fn sparkline_and_hbar() {
        assert_eq!(
            sparkline(&[0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0], 0.0, 7.0),
            "▁▂▃▄▅▆▇█"
        );
        assert_eq!(sparkline(&[5.0, f64::NAN, 5.0], 5.0, 5.0), "▄ ▄");
        assert_eq!(hbar(5.0, 10.0, 4), "██");
        // 1/8 of a 3-cell bar = 3 of its 24 eighths.
        assert_eq!(hbar(1.0, 8.0, 3), "▍");
        assert_eq!(hbar(7.0, 8.0, 1), "▉"); // 7/8 of one cell
        assert_eq!(hbar(0.0, 10.0, 4), "");
        assert_eq!(hbar(20.0, 10.0, 2), "██"); // clamped at full scale
    }

    #[test]
    fn nice_ticks() {
        let t: Vec<String> = ticks(0.0, 10.0, 5).into_iter().map(|(_, l)| l).collect();
        assert_eq!(t, vec!["0", "2", "4", "6", "8", "10"]);
        let t: Vec<String> = ticks(0.0, 1.0, 5).into_iter().map(|(_, l)| l).collect();
        assert_eq!(t, vec!["0.0", "0.2", "0.4", "0.6", "0.8", "1.0"]);
        let t: Vec<String> = ticks(-3.0, 3.0, 3).into_iter().map(|(_, l)| l).collect();
        assert_eq!(t, vec!["-2", "0", "2"]);
        // Flat range still yields a tick.
        assert!(!ticks(4.0, 4.0, 4).is_empty());
    }

    #[test]
    fn compact_numbers() {
        assert_eq!(format_compact(950.0), "950");
        assert_eq!(format_compact(12_345.0), "12.3k");
        assert_eq!(format_compact(3_400_000.0), "3.4M");
        assert_eq!(format_compact(0.25), "0.25");
        assert_eq!(format_compact(2.5), "2.5");
        assert_eq!(format_compact(-0.0), "0");
    }
}
