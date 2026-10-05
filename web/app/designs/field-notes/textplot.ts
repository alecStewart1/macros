/* A small TypeScript port of the ideas in scheme/plot.scm + src/textplot.rs:
   charts drawn out of characters (braille dots, block elements, box-drawing
   axes), computed at build time. Every chart returns rows of styled segments
   that a <pre> renders monospace-aligned.

   Tones name the plot faces (plot-series-N, plot-shade, plot-axis, …) and are
   mapped to CSS classes by the page, so light/dark themes recolor them. Heat
   cells carry explicit fg/bg colors (the viridis ramp reads in both themes). */

export type Tone =
  | "s0"
  | "s1"
  | "s2"
  | "s3"
  | "shade"
  | "axis"
  | "label"
  | "title"
  | "muted";

export interface Seg {
  t: string;
  tone?: Tone;
  fg?: string;
  bg?: string;
}
export type Row = Seg[];

/* ── row helpers ───────────────────────────────────────────────────────── */

function sameStyle(a: Seg, b: Seg) {
  return a.tone === b.tone && a.fg === b.fg && a.bg === b.bg;
}

/** Append a segment, merging with the previous one when styles match. */
function push(row: Row, seg: Seg) {
  if (!seg.t) return;
  const last = row[row.length - 1];
  if (last && sameStyle(last, seg)) last.t += seg.t;
  else row.push({ ...seg });
}

function rowLen(row: Row) {
  return row.reduce((n, s) => n + [...s.t].length, 0);
}

function padLeft(s: string, n: number) {
  return s.length >= n ? s : " ".repeat(n - s.length) + s;
}
function padRight(s: string, n: number) {
  return s.length >= n ? s : s + " ".repeat(n - s.length);
}

/** Plain-text version of a chart (for aria-labels / copy). */
export function rowsToText(rows: Row[]) {
  return rows.map((r) => r.map((s) => s.t).join("")).join("\n");
}

/* ── deterministic data helpers ────────────────────────────────────────── */

/** mulberry32: a tiny seeded PRNG so illustrative data is stable per build. */
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

export function gaussian(r: () => number) {
  const u = Math.max(r(), 1e-12);
  const v = r();
  return Math.sqrt(-2 * Math.log(u)) * Math.cos(2 * Math.PI * v);
}

/* ── numbers & ticks ───────────────────────────────────────────────────── */

function niceStep(span: number, n: number) {
  const raw = span / Math.max(1, n);
  const mag = Math.pow(10, Math.floor(Math.log10(raw)));
  const f = raw / mag;
  const nice = f <= 1 ? 1 : f <= 2 ? 2 : f <= 2.5 ? 2.5 : f <= 5 ? 5 : 10;
  return nice * mag;
}

/** Widen (lo, hi) to whole tick steps so both ends get a label
 *  (textplot-nice-range). */
export function niceRange(lo: number, hi: number, n: number) {
  if (hi <= lo) hi = lo + 1;
  const step = niceStep(hi - lo, n);
  return {
    lo: Math.floor(lo / step + 1e-9) * step,
    hi: Math.ceil(hi / step - 1e-9) * step,
    step,
  };
}

/** Compact number format, like textplot-format: 950, 12.3k, 3.4M. */
export function fmt(v: number, digits?: number) {
  if (digits !== undefined) return v.toFixed(digits);
  const a = Math.abs(v);
  if (a >= 1e6) return (v / 1e6).toFixed(1).replace(/\.0$/, "") + "M";
  if (a >= 1e4) return (v / 1e3).toFixed(1).replace(/\.0$/, "") + "k";
  if (Number.isInteger(v)) return String(v);
  return v.toFixed(a < 1 ? 2 : 1);
}

function decimalsFor(step: number) {
  if (step >= 1) return 0;
  return Math.min(4, Math.ceil(-Math.log10(step) - 1e-9));
}

/* ── braille canvas ────────────────────────────────────────────────────── */

/* Dot bits within one 2 × 4 cell, indexed [dx][dy]:
     (0,0)=0x01 (0,1)=0x02 (0,2)=0x04 (0,3)=0x40
     (1,0)=0x08 (1,1)=0x10 (1,2)=0x20 (1,3)=0x80 */
const BRAILLE_BITS = [
  [0x01, 0x02, 0x04, 0x40],
  [0x08, 0x10, 0x20, 0x80],
];

export class BrailleCanvas {
  readonly dw: number;
  readonly dh: number;
  private bits: Uint8Array;
  private tone: (Tone | undefined)[];

  constructor(
    readonly w: number,
    readonly h: number,
  ) {
    this.dw = w * 2;
    this.dh = h * 4;
    this.bits = new Uint8Array(w * h);
    this.tone = new Array(w * h).fill(undefined);
  }

  /** Light one dot. Where several tones meet in a cell, the last one wins
   *  the cell's color (as in textplot-render). */
  dot(x: number, y: number, tone: Tone) {
    x = Math.round(x);
    y = Math.round(y);
    if (x < 0 || y < 0 || x >= this.dw || y >= this.dh) return;
    const i = (y >> 2) * this.w + (x >> 1);
    this.bits[i] |= BRAILLE_BITS[x & 1][y & 3];
    this.tone[i] = tone;
  }

  /** Bresenham line between dot coordinates. */
  line(x0: number, y0: number, x1: number, y1: number, tone: Tone) {
    x0 = Math.round(x0);
    y0 = Math.round(y0);
    x1 = Math.round(x1);
    y1 = Math.round(y1);
    const dx = Math.abs(x1 - x0);
    const dy = -Math.abs(y1 - y0);
    const sx = x0 < x1 ? 1 : -1;
    const sy = y0 < y1 ? 1 : -1;
    let err = dx + dy;
    for (;;) {
      this.dot(x0, y0, tone);
      if (x0 === x1 && y0 === y1) break;
      const e2 = 2 * err;
      if (e2 >= dy) {
        err += dy;
        x0 += sx;
      }
      if (e2 <= dx) {
        err += dx;
        y0 += sy;
      }
    }
  }

  polyline(pts: [number, number][], tone: Tone) {
    for (let i = 1; i < pts.length; i++) {
      this.line(pts[i - 1][0], pts[i - 1][1], pts[i][0], pts[i][1], tone);
    }
    if (pts.length === 1) this.dot(pts[0][0], pts[0][1], tone);
  }

  rows(): Row[] {
    const out: Row[] = [];
    for (let cy = 0; cy < this.h; cy++) {
      const row: Row = [];
      for (let cx = 0; cx < this.w; cx++) {
        const i = cy * this.w + cx;
        const b = this.bits[i];
        if (b === 0) push(row, { t: " " });
        else push(row, { t: String.fromCharCode(0x2800 + b), tone: this.tone[i] });
      }
      out.push(row);
    }
    return out;
  }
}

/* ── axis frame ────────────────────────────────────────────────────────── */

interface FrameOpts {
  ylo: number;
  yhi: number;
  step: number;
  /** Labels under the x axis: [canvas column, text]. Centered on column. */
  xLabels: [number, string][];
  title?: Row;
  legend?: [string, Tone][];
}

/** Wrap canvas rows in a y axis (┤ at tick rows) and an x axis (└───). */
function frame(canvas: Row[], o: FrameOpts): Row[] {
  const h = canvas.length;
  const w = rowLen(canvas[0] ?? []);
  const dec = decimalsFor(o.step);
  const ticks = new Map<number, string>();
  const n = Math.round((o.yhi - o.ylo) / o.step);
  for (let k = 0; k <= n; k++) {
    const v = o.ylo + k * o.step;
    const doty = (1 - (v - o.ylo) / (o.yhi - o.ylo)) * (h * 4 - 1);
    const r = Math.min(h - 1, Math.floor(doty / 4));
    if (!ticks.has(r)) ticks.set(r, v.toFixed(dec));
  }
  const lw = Math.max(...[...ticks.values()].map((s) => s.length));
  const out: Row[] = [];
  const indent = lw + 2; // label + space + axis char

  if (o.title) {
    const tl = rowLen(o.title);
    const row: Row = [];
    push(row, { t: " ".repeat(indent + Math.max(0, Math.floor((w - tl) / 2))) });
    for (const s of o.title) push(row, s);
    out.push(row);
  }

  canvas.forEach((c, r) => {
    const row: Row = [];
    const lab = ticks.get(r);
    push(row, { t: padLeft(lab ?? "", lw) + " ", tone: "label" });
    push(row, { t: lab !== undefined ? "┤" : "│", tone: "axis" });
    for (const s of c) push(row, s);
    out.push(row);
  });

  const axis: Row = [];
  push(axis, { t: " ".repeat(lw + 1) });
  push(axis, { t: "└" + "─".repeat(w), tone: "axis" });
  out.push(axis);

  // x labels, centered on their column, dropped when they would collide.
  const chars: string[] = Array(indent + w + 8).fill(" ");
  let lastEnd = -1;
  for (const [col, text] of o.xLabels) {
    let start = indent + col - Math.floor(text.length / 2);
    start = Math.max(indent - 1, Math.min(start, indent + w - text.length + 2));
    if (start <= lastEnd) continue;
    for (let i = 0; i < text.length; i++) chars[start + i] = text[i];
    lastEnd = start + text.length;
  }
  const xl: Row = [];
  push(xl, { t: chars.join("").replace(/\s+$/, ""), tone: "label" });
  out.push(xl);

  if (o.legend) {
    const lg: Row = [];
    push(lg, { t: " ".repeat(indent) });
    o.legend.forEach(([name, tone], i) => {
      if (i) push(lg, { t: "   " });
      push(lg, { t: "━━", tone });
      push(lg, { t: " " + name, tone: "label" });
    });
    out.push(lg);
  }
  return out;
}

/* ── chart: normal distribution with shaded area ───────────────────────── */

/** erf via Abramowitz & Stegun 7.1.26 — the same approximation plot.scm uses. */
function erf(x: number) {
  const s = x < 0 ? -1 : 1;
  x = Math.abs(x);
  const t = 1 / (1 + 0.3275911 * x);
  const poly =
    t *
    (0.254829592 +
      t * (-0.284496736 + t * (1.421413741 + t * (-1.453152027 + t * 1.061405429))));
  return s * (1 - poly * Math.exp(-x * x));
}
export function normalCdf(x: number, mu: number, sigma: number) {
  return 0.5 * (1 + erf((x - mu) / (sigma * Math.SQRT2)));
}
function normalPdf(x: number, mu: number, sigma: number) {
  const z = (x - mu) / sigma;
  return Math.exp(-0.5 * z * z) / (sigma * Math.sqrt(2 * Math.PI));
}

/** (plot-normal mu sigma 'shade (list a b)) */
export function plotNormal(
  mu: number,
  sigma: number,
  shade: [number, number],
  width = 60,
  height = 13,
) {
  const c = new BrailleCanvas(width, height);
  const lo = mu - 4 * sigma;
  const hi = mu + 4 * sigma;
  const yr = niceRange(0, normalPdf(mu, mu, sigma), Math.max(2, Math.floor(height / 3)));
  const xs = Array.from({ length: c.dw }, (_, i) => lo + ((hi - lo) * i) / (c.dw - 1));
  const ydot = (y: number) => (c.dh - 1) * (1 - (y - yr.lo) / (yr.hi - yr.lo));
  const xdot = (x: number) => ((x - lo) / (hi - lo)) * (c.dw - 1);
  const base = c.dh - 1;

  // Shade first, curve over it — the later op wins each cell's color.
  xs.forEach((x, i) => {
    if (x >= shade[0] && x <= shade[1]) c.line(i, base, i, ydot(normalPdf(x, mu, sigma)), "shade");
  });
  c.polyline(
    xs.map((x, i) => [i, ydot(normalPdf(x, mu, sigma))] as [number, number]),
    "s0",
  );

  const p = normalCdf(shade[1], mu, sigma) - normalCdf(shade[0], mu, sigma);
  const pct = (Math.round(p * 1000) / 10).toFixed(1) + "%";
  const ticks: [number, string][] = [];
  for (let k = -4; k <= 4; k++) {
    const x = mu + k * sigma;
    ticks.push([Math.floor(xdot(x) / 2), fmt(x)]);
  }
  return {
    pct,
    rows: frame(c.rows(), {
      ylo: yr.lo,
      yhi: yr.hi,
      step: yr.step,
      xLabels: ticks,
      title: [
        { t: `N(μ=${fmt(mu)}, σ=${fmt(sigma)})`, tone: "title" },
        { t: "   shaded: ", tone: "label" },
        { t: pct, tone: "title" },
      ],
    }),
  };
}

/* ── chart: braille line plot ──────────────────────────────────────────── */

export function plotLines(
  series: { name: string; data: number[]; tone: Tone }[],
  width = 44,
  height = 9,
  title?: string,
) {
  const c = new BrailleCanvas(width, height);
  const all = series.flatMap((s) => s.data);
  const yr = niceRange(Math.min(...all), Math.max(...all), Math.max(2, Math.floor(height / 3)));
  for (const s of series) {
    const n = s.data.length;
    c.polyline(
      s.data.map(
        (v, i) =>
          [
            (i / (n - 1)) * (c.dw - 1),
            (c.dh - 1) * (1 - (v - yr.lo) / (yr.hi - yr.lo)),
          ] as [number, number],
      ),
      s.tone,
    );
  }
  const n = series[0]?.data.length ?? 1;
  return frame(c.rows(), {
    ylo: yr.lo,
    yhi: yr.hi,
    step: yr.step,
    xLabels: [
      [0, "0"],
      [width - 1, String(n - 1)],
    ],
    title: title ? [{ t: title, tone: "title" }] : undefined,
    legend: series.map((s) => [s.name, s.tone] as [string, Tone]),
  });
}

/* ── chart: scatter ────────────────────────────────────────────────────── */

export function plotScatter(
  points: [number, number][],
  tone: Tone,
  width = 30,
  height = 9,
  title?: string,
) {
  const c = new BrailleCanvas(width, height);
  const xsr = niceRange(Math.min(...points.map((p) => p[0])), Math.max(...points.map((p) => p[0])), 3);
  const yr = niceRange(Math.min(...points.map((p) => p[1])), Math.max(...points.map((p) => p[1])), 3);
  for (const [x, y] of points) {
    c.dot(
      ((x - xsr.lo) / (xsr.hi - xsr.lo)) * (c.dw - 1),
      (c.dh - 1) * (1 - (y - yr.lo) / (yr.hi - yr.lo)),
      tone,
    );
  }
  return frame(c.rows(), {
    ylo: yr.lo,
    yhi: yr.hi,
    step: yr.step,
    xLabels: [
      [0, fmt(xsr.lo)],
      [width - 1, fmt(xsr.hi)],
    ],
    title: title ? [{ t: title, tone: "title" }] : undefined,
  });
}

/* ── chart: histogram in block elements ────────────────────────────────── */

const VBLOCKS = [" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];

export function plotHistogram(
  values: number[],
  bins: number,
  height: number,
  binWidth: number,
  tone: Tone,
  title?: string,
) {
  const lo = Math.min(...values);
  const hi = Math.max(...values);
  const counts = new Array(bins).fill(0);
  for (const v of values) {
    counts[Math.min(bins - 1, Math.floor(((v - lo) / (hi - lo)) * bins))]++;
  }
  const yr = niceRange(0, Math.max(...counts), Math.max(2, Math.floor(height / 3)));
  const canvas: Row[] = [];
  for (let r = 0; r < height; r++) {
    const row: Row = [];
    for (const n of counts) {
      // How many eighths of this cell (counting from the top row) are filled.
      const eighths = Math.round(((n - yr.lo) / (yr.hi - yr.lo)) * height * 8);
      const fromBottom = height - 1 - r;
      const e = Math.max(0, Math.min(8, eighths - fromBottom * 8));
      push(row, { t: VBLOCKS[e].repeat(binWidth - 1), tone: e ? tone : undefined });
      push(row, { t: " " });
    }
    canvas.push(row);
  }
  const w = bins * binWidth;
  return frame(canvas, {
    ylo: yr.lo,
    yhi: yr.hi,
    step: yr.step,
    xLabels: [
      [0, fmt(Math.round(lo))],
      [Math.floor(w / 2), fmt(Math.round((lo + hi) / 2))],
      [w - 2, fmt(Math.round(hi))],
    ],
    title: title ? [{ t: title, tone: "title" }] : undefined,
  });
}

/* ── chart: horizontal bars with eighth-block resolution ───────────────── */

const HBLOCKS = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];

export function hbar(value: number, max: number, width: number) {
  const eighths = Math.round((value / max) * width * 8);
  return "█".repeat(Math.floor(eighths / 8)) + HBLOCKS[eighths % 8];
}

export function plotBars(
  items: { label: string; value: number; tone?: Tone }[],
  width: number,
) {
  const max = Math.max(...items.map((i) => i.value));
  const lw = Math.max(...items.map((i) => i.label.length));
  return items.map((it) => {
    const row: Row = [];
    const bar = hbar(it.value, max, width);
    push(row, { t: padRight(it.label, lw) + " ", tone: "label" });
    push(row, { t: "│", tone: "axis" });
    push(row, { t: bar, tone: it.tone ?? "s0" });
    push(row, { t: " ".repeat(width - [...bar].length + 1) });
    push(row, { t: fmt(it.value), tone: "label" });
    return row;
  });
}

/* ── sparkline ─────────────────────────────────────────────────────────── */

const SPARKS = "▁▂▃▄▅▆▇█";

export function sparkline(values: number[], lo?: number, hi?: number) {
  const l = lo ?? Math.min(...values);
  const h = hi ?? Math.max(...values);
  return values
    .map((v) => SPARKS[Math.max(0, Math.min(7, Math.round(((v - l) / (h - l || 1)) * 7)))])
    .join("");
}

/* ── heatmap: half blocks, two data rows per text line ─────────────────── */

/** The viridis ramp the macros themes use for plot-heat-0 … plot-heat-7. */
export const VIRIDIS = [
  "#440154",
  "#46327e",
  "#365c8d",
  "#277f8e",
  "#1fa187",
  "#4ac16d",
  "#a0da39",
  "#fde725",
];

/** `▀` with the upper row's color as foreground and the lower row's as
 *  background — (plot-heatmap m 'mode 'half). */
export function plotHeatHalf(
  grid: number[][],
  rowLabel: (pairIndex: number) => string,
  cellWidth: number,
) {
  const flat = grid.flat();
  const lo = Math.min(...flat);
  const hi = Math.max(...flat);
  const level = (v: number) =>
    VIRIDIS[Math.max(0, Math.min(7, Math.floor(((v - lo) / (hi - lo || 1)) * 8)))];
  const lines = Math.ceil(grid.length / 2);
  const labels = Array.from({ length: lines }, (_, k) => rowLabel(k));
  const lw = Math.max(...labels.map((l) => l.length));
  const out: Row[] = [];
  for (let k = 0; k < lines; k++) {
    const top = grid[2 * k];
    const bot = grid[2 * k + 1];
    const row: Row = [];
    push(row, { t: padLeft(labels[k], lw) + " ", tone: "label" });
    top.forEach((v, c) => {
      push(row, {
        t: "▀".repeat(cellWidth),
        fg: level(v),
        bg: bot ? level(bot[c]) : undefined,
      });
    });
    out.push(row);
  }
  return { rows: out, labelWidth: lw + 1, lo, hi };
}

/** A one-line color key for the heat ramp. */
export function heatScale(lo: string, hi: string, indent: number): Row {
  const row: Row = [];
  push(row, { t: " ".repeat(indent) + lo + " ", tone: "label" });
  for (const c of VIRIDIS) push(row, { t: "██", fg: c });
  push(row, { t: " " + hi, tone: "label" });
  return row;
}

/** Pad every row on the left by n spaces. */
export function indentRows(rows: Row[], n: number): Row[] {
  return rows.map((r) => [{ t: " ".repeat(n) }, ...r]);
}

/** A row of x-axis labels under a heatmap at given character columns. */
export function labelRow(width: number, labels: [number, string][], indent: number): Row {
  const chars: string[] = Array(indent + width + 4).fill(" ");
  for (const [col, text] of labels) {
    for (let i = 0; i < text.length; i++) chars[indent + col + i] = text[i];
  }
  return [{ t: chars.join("").replace(/\s+$/, ""), tone: "label" }];
}
