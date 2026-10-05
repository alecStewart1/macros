/* ── textplot.ts ───────────────────────────────────────────────────────────
   A tiny TypeScript port of the ideas in scheme/plot.scm, so the charts on
   this page are real text rendered from data — not screenshots. Everything
   is deterministic (seeded RNG) so server output is stable between builds.

   Braille cells pack 2 × 4 dots; the dot at (x, y) inside a cell sets one bit
   of U+2800 + pattern:
     (0,0)=0x01 (0,1)=0x02 (0,2)=0x04 (0,3)=0x40
     (1,0)=0x08 (1,1)=0x10 (1,2)=0x20 (1,3)=0x80                          */

export const RAMP = [
  "#440154",
  "#46327e",
  "#365c8d",
  "#277f8e",
  "#1fa187",
  "#4ac16d",
  "#a0da39",
  "#fde725",
] as const;

export const INK = "#0b0a10";
export const AXIS = "#5d566e";
export const LABEL = "#a39cb4";

export type Seg = { t: string; fg?: string; bg?: string };
export type Row = Seg[];

const SPARK = "▁▂▃▄▅▆▇█";
const HBAR = "▏▎▍▌▋▊▉█";

/** Append text to a row, merging with the previous run when colors match. */
export function push(row: Row, t: string, fg?: string, bg?: string) {
  const last = row[row.length - 1];
  if (last && last.fg === fg && last.bg === bg) last.t += t;
  else row.push({ t, fg, bg });
}

function concat(a: Row, b: Row): Row {
  const out: Row = a.map((s) => ({ ...s }));
  for (const s of b) push(out, s.t, s.fg, s.bg);
  return out;
}

/* ── Numbers ──────────────────────────────────────────────────────────── */

/** mulberry32: small, fast, seedable. */
export function rng(seed: number) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

export function gauss(r: () => number) {
  const u = Math.max(1e-9, r());
  const v = r();
  return Math.sqrt(-2 * Math.log(u)) * Math.cos(2 * Math.PI * v);
}

/** A smooth-ish bounded random walk, handy for sparklines. */
export function walk(seed: number, n: number, lo = 0, hi = 100) {
  const r = rng(seed);
  const out: number[] = [];
  let v = lo + (hi - lo) * (0.3 + 0.4 * r());
  let vel = 0;
  for (let i = 0; i < n; i++) {
    vel = vel * 0.7 + (r() - 0.5) * (hi - lo) * 0.12;
    v = Math.min(hi, Math.max(lo, v + vel));
    out.push(v);
  }
  return out;
}

export function fmt(v: number): string {
  const a = Math.abs(v);
  if (a >= 1e6) return `${(v / 1e6).toFixed(1)}M`;
  if (a >= 1e3) return `${(v / 1e3).toFixed(1)}k`;
  if (Math.abs(v - Math.round(v)) < 1e-9) return String(Math.round(v));
  return v.toFixed(1);
}

/** Widen (lo, hi) to whole tick steps, like textplot-nice-range. */
export function niceRange(lo: number, hi: number, n = 4): [number, number] {
  const raw = (hi - lo || 1) / n;
  const mag = Math.pow(10, Math.floor(Math.log10(raw)));
  const norm = raw / mag;
  const step =
    (norm <= 1 ? 1 : norm <= 2 ? 2 : norm <= 2.5 ? 2.5 : norm <= 5 ? 5 : 10) * mag;
  return [Math.floor(lo / step) * step, Math.ceil(hi / step) * step];
}

/** Map a 0…1 value onto the bright two-thirds of the ramp (legible on ink). */
export function heat(t: number, from = 2): string {
  const span = 7 - from;
  const i = from + Math.round(Math.min(1, Math.max(0, t)) * span);
  return RAMP[i];
}

/* ── Braille raster ───────────────────────────────────────────────────── */

const BIT = [
  [0x01, 0x02, 0x04, 0x40],
  [0x08, 0x10, 0x20, 0x80],
];

export class BrailleGrid {
  readonly w: number;
  readonly h: number;
  private bits: Uint8Array;
  private color: (string | undefined)[];

  constructor(w: number, h: number) {
    this.w = w;
    this.h = h;
    this.bits = new Uint8Array(w * h);
    this.color = new Array(w * h).fill(undefined);
  }

  get dw() {
    return this.w * 2;
  }
  get dh() {
    return this.h * 4;
  }

  /** Set one dot. The cell takes the color of the last dot drawn into it. */
  dot(x: number, y: number, c: string) {
    const xi = Math.round(x);
    const yi = Math.round(y);
    if (xi < 0 || yi < 0 || xi >= this.dw || yi >= this.dh) return;
    const i = (yi >> 2) * this.w + (xi >> 1);
    this.bits[i] |= BIT[xi & 1][yi & 3];
    this.color[i] = c;
  }

  /** Bresenham line in dot space. */
  line(x0: number, y0: number, x1: number, y1: number, c: string) {
    let x = Math.round(x0);
    let y = Math.round(y0);
    const xe = Math.round(x1);
    const ye = Math.round(y1);
    const dx = Math.abs(xe - x);
    const sx = x < xe ? 1 : -1;
    const dy = -Math.abs(ye - y);
    const sy = y < ye ? 1 : -1;
    let err = dx + dy;
    for (;;) {
      this.dot(x, y, c);
      if (x === xe && y === ye) break;
      const e2 = 2 * err;
      if (e2 >= dy) {
        err += dy;
        x += sx;
      }
      if (e2 <= dx) {
        err += dx;
        y += sy;
      }
    }
  }

  rows(): Row[] {
    const out: Row[] = [];
    for (let r = 0; r < this.h; r++) {
      const row: Row = [];
      for (let c = 0; c < this.w; c++) {
        const i = r * this.w + c;
        const b = this.bits[i];
        if (b) push(row, String.fromCharCode(0x2800 + b), this.color[i]);
        else push(row, " ");
      }
      out.push(row);
    }
    return out;
  }
}

/* ── Frames ───────────────────────────────────────────────────────────── */

/** Left value axis (top / middle / bottom labels) plus a bottom rule. */
function yFrame(body: Row[], lo: number, hi: number, width: number): Row[] {
  const h = body.length;
  // middle tick goes on the row nearest its value, like textplot-ticks
  const midV = (lo + hi) / 2;
  const mid = Math.round(((hi - midV) / (hi - lo)) * (h - 1));
  const labels = body.map((_, r) =>
    r === 0 ? fmt(hi) : r === h - 1 ? fmt(lo) : r === mid ? fmt(midV) : "",
  );
  const lw = Math.max(...labels.map((l) => l.length));
  const out = body.map((row, r) => {
    const head: Row = [];
    push(head, labels[r].padStart(lw) + " ", LABEL);
    push(head, labels[r] ? "┤" : "│", AXIS);
    return concat(head, row);
  });
  const rule: Row = [];
  push(rule, " ".repeat(lw + 1) + "└" + "─".repeat(width), AXIS);
  out.push(rule);
  return out;
}

/** Place short labels at character columns on a blank line. */
function labelLine(width: number, items: [number, string, string?][], indent = 0): Row {
  const chars: { ch: string; fg?: string }[] = Array.from({ length: width }, () => ({ ch: " " }));
  for (const [col, text, fg] of items) {
    let start = Math.round(col - text.length / 2);
    start = Math.max(0, Math.min(width - text.length, start));
    for (let i = 0; i < text.length; i++) chars[start + i] = { ch: text[i], fg: fg ?? LABEL };
  }
  const row: Row = [];
  if (indent) push(row, " ".repeat(indent));
  for (const c of chars) push(row, c.ch, c.ch === " " ? undefined : c.fg);
  return row;
}

/* ── Charts ───────────────────────────────────────────────────────────── */

/** ▁▃▂▅█▆ — each glyph colored by its own height. */
export function sparkline(values: number[], colorFrom = 2): Row {
  const lo = Math.min(...values);
  const hi = Math.max(...values);
  const row: Row = [];
  for (const v of values) {
    const t = hi === lo ? 0.5 : (v - lo) / (hi - lo);
    const i = Math.round(t * 7);
    push(row, SPARK[i], heat(t, colorFrom));
  }
  return row;
}

/** Horizontal bars with eighth-block tips. */
export function bars(items: [string, number][], width: number): Row[] {
  const max = Math.max(...items.map(([, v]) => v));
  const lw = Math.max(...items.map(([l]) => l.length));
  const colors = [RAMP[7], RAMP[6], RAMP[5], RAMP[4], RAMP[3]];
  return items.map(([label, v], i) => {
    const row: Row = [];
    push(row, label.padStart(lw) + " ", LABEL);
    const eighths = Math.round((v / max) * width * 8);
    const full = Math.floor(eighths / 8);
    const rem = eighths % 8;
    let bar = "█".repeat(full);
    if (rem) bar += HBAR[rem - 1];
    push(row, bar, colors[i % colors.length]);
    push(row, " ".repeat(Math.max(0, width - bar.length)) + " ");
    push(row, fmt(v), LABEL);
    return row;
  });
}

/** Braille line plot of one or more series (x = index). */
export function lines(
  series: { name: string; data: number[]; color: string }[],
  width: number,
  height: number,
): Row[] {
  const all = series.flatMap((s) => s.data);
  const [lo, hi] = niceRange(Math.min(...all), Math.max(...all));
  const g = new BrailleGrid(width, height);
  const sy = (v: number) => g.dh - 1 - ((v - lo) / (hi - lo)) * (g.dh - 1);
  for (const s of series) {
    const n = s.data.length;
    let px = 0;
    let py = sy(s.data[0]);
    for (let i = 1; i < n; i++) {
      const x = (i * (g.dw - 1)) / (n - 1);
      const y = sy(s.data[i]);
      g.line(px, py, x, y, s.color);
      px = x;
      py = y;
    }
  }
  const out = yFrame(g.rows(), lo, hi, width);
  const legend: Row = [];
  push(legend, " ".repeat(5));
  for (const s of series) {
    push(legend, "■ ", s.color);
    push(legend, s.name + "   ", LABEL);
  }
  out.push(legend);
  return out;
}

/** Braille scatter of (x, y) points per series. */
export function scatter(
  series: { name: string; pts: [number, number][]; color: string }[],
  width: number,
  height: number,
): Row[] {
  const xs = series.flatMap((s) => s.pts.map((p) => p[0]));
  const ys = series.flatMap((s) => s.pts.map((p) => p[1]));
  const [xlo, xhi] = [Math.min(...xs), Math.max(...xs)];
  const [lo, hi] = niceRange(Math.min(...ys), Math.max(...ys));
  const g = new BrailleGrid(width, height);
  for (const s of series)
    for (const [x, y] of s.pts)
      g.dot(
        ((x - xlo) / (xhi - xlo)) * (g.dw - 1),
        g.dh - 1 - ((y - lo) / (hi - lo)) * (g.dh - 1),
        s.color,
      );
  const out = yFrame(g.rows(), lo, hi, width);
  const legend: Row = [];
  push(legend, " ".repeat(5));
  for (const s of series) {
    push(legend, "■ ", s.color);
    push(legend, s.name + "   ", LABEL);
  }
  out.push(legend);
  return out;
}

/** Column histogram drawn with eighth blocks, 2 glyphs per bin + a gap. */
export function histogram(values: number[], bins: number, height: number): Row[] {
  // trim the extreme 0.5% on each side so one outlier can't flatten the bins
  const sorted = [...values].sort((a, b) => a - b);
  const lo = sorted[Math.floor(sorted.length * 0.005)];
  const hi = sorted[Math.ceil(sorted.length * 0.995) - 1];
  values = values.filter((v) => v >= lo && v <= hi);
  const counts = new Array(bins).fill(0);
  for (const v of values) counts[Math.min(bins - 1, Math.floor(((v - lo) / (hi - lo)) * bins))]++;
  const [, cmax] = niceRange(0, Math.max(...counts), 5);
  const body: Row[] = [];
  for (let r = 0; r < height; r++) {
    const row: Row = [];
    counts.forEach((c, b) => {
      const level = (c / cmax) * height * 8;
      const fill = Math.max(0, Math.min(8, Math.round(level - (height - 1 - r) * 8)));
      const glyph = fill ? SPARK[fill - 1] : " ";
      push(row, glyph + glyph, fill ? heat(b / (bins - 1)) : undefined);
      push(row, " ");
    });
    body.push(row);
  }
  const width = bins * 3;
  const out = yFrame(body, 0, cmax, width);
  const lw = fmt(cmax).length + 2;
  out.push(
    labelLine(
      width,
      [
        [1, `${Math.round(lo)}ms`],
        [width / 2, `${Math.round((lo + hi) / 2)}`],
        [width - 2, `${Math.round(hi)}ms`],
      ],
      lw,
    ),
  );
  return out;
}

export function normalPdf(x: number, mu = 0, sigma = 1) {
  const z = (x - mu) / sigma;
  return Math.exp(-0.5 * z * z) / (sigma * Math.sqrt(2 * Math.PI));
}

/** Abramowitz–Stegun erf, as plot.scm does. */
function erf(x: number) {
  const s = Math.sign(x);
  const a = Math.abs(x);
  const t = 1 / (1 + 0.3275911 * a);
  const y =
    1 -
    ((((1.061405429 * t - 1.453152027) * t + 1.421413741) * t - 0.284496736) * t +
      0.254829592) *
      t *
      Math.exp(-a * a);
  return s * y;
}

export function normalCdf(x: number, mu = 0, sigma = 1) {
  return 0.5 * (1 + erf((x - mu) / (sigma * Math.SQRT2)));
}

/** N(0,1) with both tails beyond ±z shaded solid. */
export function normalCurve(
  width: number,
  height: number,
  z: number,
  curve: string,
  shade: string,
  span = 3.5,
): Row[] {
  const g = new BrailleGrid(width, height);
  const ymax = normalPdf(0) * 1.04;
  const xAt = (dx: number) => -span + (2 * span * dx) / (g.dw - 1);
  const yAt = (x: number) => g.dh - 1 - (normalPdf(x) / ymax) * (g.dh - 1);
  for (let dx = 0; dx < g.dw; dx++) {
    const x = xAt(dx);
    if (Math.abs(x) < z) continue;
    for (let yy = Math.ceil(yAt(x)); yy < g.dh; yy++) g.dot(dx, yy, shade);
  }
  let py = yAt(xAt(0));
  for (let dx = 1; dx < g.dw; dx++) {
    const y = yAt(xAt(dx));
    g.line(dx - 1, py, dx, y, curve);
    py = y;
  }
  const out = g.rows();
  const rule: Row = [];
  push(rule, "─".repeat(width), AXIS);
  out.push(rule);
  const col = (x: number) => ((x + span) / (2 * span)) * (width - 1);
  out.push(
    labelLine(width, [
      [col(-z), `-${z}σ`, shade],
      [col(0), "0"],
      [col(z), `+${z}σ`, shade],
    ]),
  );
  const tails = 2 * (1 - normalCdf(z));
  const cap: Row = [];
  const txt = `${(tails * 100).toFixed(1)}% in the tails`;
  push(cap, " ".repeat(Math.max(0, width - txt.length - 2)));
  push(cap, "■ ", shade);
  push(cap, txt, LABEL);
  out.push(cap);
  return out;
}

/** Half-block heatmap: each line packs two data rows via ▀ fg/bg. */
export function heatmapHalf(matrix: number[][], colLabels?: [number, string][]): Row[] {
  const flat = matrix.flat();
  const lo = Math.min(...flat);
  const hi = Math.max(...flat);
  const lvl = (v: number) => RAMP[Math.min(7, Math.floor(((v - lo) / (hi - lo)) * 8))];
  const out: Row[] = [];
  for (let r = 0; r + 1 < matrix.length; r += 2) {
    const row: Row = [];
    for (let c = 0; c < matrix[r].length; c++) push(row, "▀", lvl(matrix[r][c]), lvl(matrix[r + 1][c]));
    out.push(row);
  }
  const width = matrix[0].length;
  if (colLabels) out.push(labelLine(width, colLabels));
  const scale: Row = [];
  for (const c of RAMP) push(scale, "█", c);
  push(scale, "  low → high", LABEL);
  out.push(scale);
  return out;
}

/** A row of half-block cells from two stacked value rows (feature accents). */
export function heatStrip(seed: number, n: number): Row {
  const a = walk(seed, n, 0, 1);
  const b = walk(seed + 101, n, 0, 1);
  const row: Row = [];
  const lvl = (v: number) => RAMP[Math.min(7, Math.floor(v * 8))];
  for (let i = 0; i < n; i++) push(row, "▀", lvl(a[i]), lvl(b[i]));
  return row;
}

export function rowsText(rows: Row[]): string {
  return rows.map((r) => r.map((s) => s.t).join("")).join("\n");
}
