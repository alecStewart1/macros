/* The static charts on the Phosphor page, computed at build time with the
   local rasterizer. Data is either mathematical (functions, seeded samples)
   or a dated snapshot of this repository (wc -l, git log). */

import {
  BrailleCanvas,
  mulberry32,
  normalPdf,
  sparkline,
  hbar,
  vbars,
  VIRIDIS,
  type Row,
} from "./braille";

export type XTick = { at: number; text: string; accent?: boolean };

export type PlotData = {
  rows: Row[];
  /** Row index → y label. */
  yTicks: Record<number, string>;
  xTicks: XTick[];
};

/* ── The hero's normal curve ─────────────────────────────────────────── */

export const NORMAL = { cols: 60, rows: 14, lo: -4, hi: 4, ymax: 0.4 };

const nDw = NORMAL.cols * 2;
const nDh = NORMAL.rows * 4;
const nX = (i: number) => NORMAL.lo + ((NORMAL.hi - NORMAL.lo) * i) / (nDw - 1);
const nY = (y: number) => nDh - 1 - (y / NORMAL.ymax) * (nDh - 1);
const NORMAL_CURVE: [number, number][] = Array.from({ length: nDw }, (_, i) => [
  i,
  nY(normalPdf(nX(i))),
]);

/** plot-normal 0 1 'shade (list -k k): the density curve with the area
 *  under it shaded between -k and k, one vertical dot line per column. */
export function normalPlot(k: number): PlotData {
  const c = new BrailleCanvas(NORMAL.cols, NORMAL.rows);
  const base = nDh - 1;
  for (let i = 0; i < nDw; i++) {
    const x = nX(i);
    if (Math.abs(x) <= k + 1e-9) c.line(i, base, i, NORMAL_CURVE[i][1], "shade");
  }
  c.polyline(NORMAL_CURVE, "curve");
  const xTicks: XTick[] = [];
  for (let s = -3; s <= 3; s++) {
    xTicks.push({
      at: (s - NORMAL.lo) / (NORMAL.hi - NORMAL.lo),
      text: s === 0 ? "0" : `${s > 0 ? "" : "−"}${Math.abs(s)}σ`,
    });
  }
  return {
    rows: c.render(),
    yTicks: { 0: "0.4", [Math.floor(NORMAL.rows / 2)]: "0.2", [NORMAL.rows - 1]: "0.0" },
    xTicks,
  };
}

/* ── plot-function: two functions, braille lines ─────────────────────── */

export function functionPlot(): PlotData {
  const cols = 56;
  const rows = 13;
  const c = new BrailleCanvas(cols, rows);
  const lo = 0;
  const hi = 4 * Math.PI;
  const toY = (y: number) => (c.dh - 1) * (1 - (y + 1) / 2);
  const fns: [string, (x: number) => number][] = [
    ["s0", Math.sin],
    ["s1", (x) => Math.exp(-x / 6) * Math.cos(2 * x)],
  ];
  for (const [face, f] of fns) {
    const pts: [number, number][] = [];
    for (let i = 0; i < c.dw; i++) {
      const x = lo + ((hi - lo) * i) / (c.dw - 1);
      pts.push([i, toY(f(x))]);
    }
    c.polyline(pts, face);
  }
  const names = ["0", "π", "2π", "3π", "4π"];
  return {
    rows: c.render(),
    yTicks: { 0: "1.0", 6: "0.0", 12: "-1.0" },
    xTicks: names.map((text, i) => ({ at: i / 4, text })),
  };
}

/* ── plot-scatter: two seeded clusters ───────────────────────────────── */

function gauss(rand: () => number): number {
  // Box–Muller.
  const u = Math.max(rand(), 1e-12);
  const v = rand();
  return Math.sqrt(-2 * Math.log(u)) * Math.cos(2 * Math.PI * v);
}

export function scatterPlot(): PlotData {
  const cols = 30;
  const rows = 9;
  const c = new BrailleCanvas(cols, rows);
  const rand = mulberry32(7);
  const clusters: [string, number, number, number][] = [
    ["s0", 3.2, 6.6, 0.9],
    ["s1", 6.8, 3.2, 1.1],
  ];
  for (const [face, mx, my, sd] of clusters) {
    for (let n = 0; n < 70; n++) {
      const x = mx + gauss(rand) * sd;
      const y = my + gauss(rand) * sd * 0.8;
      c.dot((x / 10) * (c.dw - 1), (c.dh - 1) * (1 - y / 10), face);
    }
  }
  return {
    rows: c.render(),
    yTicks: { 0: "10", 4: "5", 8: "0" },
    xTicks: [
      { at: 0, text: "0" },
      { at: 0.5, text: "5" },
      { at: 1, text: "10" },
    ],
  };
}

/* ── plot-histogram: 2000 standard-normal samples ────────────────────── */

export function histogram(): { rows: string[]; bins: number } {
  const rand = mulberry32(42);
  const bins = 30;
  const lo = -3.5;
  const hi = 3.5;
  const counts = new Array<number>(bins).fill(0);
  for (let n = 0; n < 2000; n++) {
    const z = gauss(rand);
    if (z < lo || z >= hi) continue;
    counts[Math.floor(((z - lo) / (hi - lo)) * bins)]++;
  }
  return { rows: vbars(counts, 7), bins };
}

/* ── Repository snapshot (wc -l / git log, October 2026) ─────────────── */

/** Lines per file, largest 24 files, by language. */
const RUST_FILES = [10649, 5631, 4189, 3108, 2728, 2259, 1945, 1614, 1547, 1534, 1508, 1450, 1413, 1299, 1138, 1115, 1017, 858, 837, 826, 812, 772, 764, 751];
const SCHEME_FILES = [2971, 2245, 2189, 756, 700, 683, 632, 629, 576, 547, 491, 466, 457, 447, 411, 340, 299, 294, 264, 256, 255, 235, 229, 224];
const DOC_FILES = [329, 317, 193, 191, 172, 166, 165, 148, 144, 138, 136, 132, 116, 109, 86, 82, 76, 69, 69, 66, 61, 59, 59, 56];
/** Commits per active day, oldest first. */
const COMMITS_PER_DAY = [16, 26, 16, 7, 19, 6, 5, 3, 1, 4, 1, 6, 2, 4, 3, 1, 2, 3, 1, 3, 1, 4, 1, 1, 1, 1, 1, 1, 2, 2, 41, 3, 4];

const sq = (v: number[]) => v.map(Math.sqrt);

export const STATS: { value: string; unit: string; spark: string; note: string }[] = [
  { value: "60.6k", unit: "lines of Rust", spark: sparkline(sq(RUST_FILES), 0), note: "56 files · engine, renderer, primitives" },
  { value: "20.2k", unit: "lines of Scheme", spark: sparkline(sq(SCHEME_FILES), 0), note: "61 files · the editor you actually use" },
  { value: "192", unit: "commits", spark: sparkline(sq(COMMITS_PER_DAY), 0), note: "per active day, oldest → newest" },
  { value: "31", unit: "docs pages", spark: sparkline(DOC_FILES, 0), note: "lines per page, longest first" },
];

export const BARS: { label: string; value: number; bar: string; face: string }[] = (() => {
  const items: [string, number, string][] = [
    ["rust", 60615, "s0"],
    ["scheme", 20178, "s1"],
    ["docs", 3419, "s2"],
  ];
  return items.map(([label, value, face]) => ({ label, value, face, bar: hbar(value, 60615, 26) || "▏" }));
})();

/** Commits by weekday (rows, Mon–Sun) and hour (columns, 00–23). */
const COMMIT_HOURS: number[][] = [
  [0,0,0,0,0,0,0,2,0,1,0,2,0,1,0,0,5,6,0,0,9,2,2,1],
  [0,0,0,0,0,0,0,1,0,0,0,0,1,0,1,3,0,1,1,1,2,2,0,0],
  [0,0,0,0,0,0,0,0,1,1,0,0,0,0,3,0,0,0,0,1,0,2,3,1],
  [0,0,0,0,0,0,1,1,0,2,1,0,0,1,3,2,1,7,2,0,4,33,9,0],
  [0,0,0,0,0,0,1,2,2,1,0,2,3,1,3,2,2,2,4,1,1,0,4,0],
  [0,0,0,0,0,0,0,0,0,2,4,5,1,4,0,5,3,1,0,0,0,0,0,0],
  [0,0,0,0,0,0,1,1,0,1,2,3,0,1,1,0,0,0,2,1,0,0,0,0],
];

export const HEAT_ROWS = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

/** Heatmap cells as viridis colors (log scale, so one late night doesn't
 *  wash out the rest). */
export const HEATMAP: string[][] = (() => {
  const max = Math.log1p(Math.max(...COMMIT_HOURS.flat()));
  return COMMIT_HOURS.map((row) =>
    row.map((v) => VIRIDIS[Math.min(7, Math.round((Math.log1p(v) / max) * 7))]),
  );
})();
