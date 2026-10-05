/* A tiny text-plot rasterizer, mirroring the shape of Macros' own
   `textplot-render` primitive: draw dots on a (cols*2) x (rows*4) grid, get
   back one row of colored segments per text line. Each character cell packs
   a 2x4 block of dots into a single braille glyph, U+2800 + bit pattern. */

export type Seg = { text: string; face: string };
export type Row = Seg[];

/** Braille dot bits, indexed [y][x] within a cell. */
const BITS: number[][] = [
  [0x01, 0x08],
  [0x02, 0x10],
  [0x04, 0x20],
  [0x40, 0x80],
];

/** U+2800, the blank braille pattern. We pad with it instead of spaces so
 *  every character in a plot row comes from the same (braille) font and
 *  columns can never drift. */
export const BLANK = "⠀";

export class BrailleCanvas {
  readonly cols: number;
  readonly rows: number;
  readonly dw: number;
  readonly dh: number;
  private bits: Uint8Array;
  private faces: string[];

  constructor(cols: number, rows: number) {
    this.cols = cols;
    this.rows = rows;
    this.dw = cols * 2;
    this.dh = rows * 4;
    this.bits = new Uint8Array(cols * rows);
    this.faces = new Array<string>(cols * rows).fill("");
  }

  /** Set one dot. Top-left origin, y grows downward. The face drawn last in
   *  a cell wins its color, as in the editor. */
  dot(x: number, y: number, face: string): void {
    const xi = Math.round(x);
    const yi = Math.round(y);
    if (xi < 0 || yi < 0 || xi >= this.dw || yi >= this.dh) return;
    const i = (yi >> 2) * this.cols + (xi >> 1);
    this.bits[i] |= BITS[yi & 3][xi & 1];
    this.faces[i] = face;
  }

  /** Bresenham line between two dot coordinates. */
  line(x0: number, y0: number, x1: number, y1: number, face: string): void {
    let ax = Math.round(x0);
    let ay = Math.round(y0);
    const bx = Math.round(x1);
    const by = Math.round(y1);
    const dx = Math.abs(bx - ax);
    const dy = -Math.abs(by - ay);
    const sx = ax < bx ? 1 : -1;
    const sy = ay < by ? 1 : -1;
    let err = dx + dy;
    for (;;) {
      this.dot(ax, ay, face);
      if (ax === bx && ay === by) break;
      const e2 = 2 * err;
      if (e2 >= dy) {
        err += dy;
        ax += sx;
      }
      if (e2 <= dx) {
        err += dx;
        ay += sy;
      }
    }
  }

  polyline(pts: [number, number][], face: string): void {
    for (let i = 1; i < pts.length; i++) {
      this.line(pts[i - 1][0], pts[i - 1][1], pts[i][0], pts[i][1], face);
    }
  }

  /** One row of segments per text line; adjacent cells sharing a face are
   *  merged into one segment. Empty cells get face "". */
  render(): Row[] {
    const out: Row[] = [];
    for (let r = 0; r < this.rows; r++) {
      const row: Row = [];
      for (let c = 0; c < this.cols; c++) {
        const i = r * this.cols + c;
        const b = this.bits[i];
        const ch = String.fromCharCode(0x2800 + b);
        const face = b ? this.faces[i] : "";
        const last = row[row.length - 1];
        if (last && last.face === face) last.text += ch;
        else row.push({ text: ch, face });
      }
      out.push(row);
    }
    return out;
  }
}

/* ── Numeric helpers ─────────────────────────────────────────────────── */

export const normalPdf = (x: number): number =>
  Math.exp(-0.5 * x * x) / Math.sqrt(2 * Math.PI);

/** erf via Abramowitz & Stegun 7.1.26 — the same approximation plot.scm uses. */
export function erf(x: number): number {
  const s = x < 0 ? -1 : 1;
  const a = Math.abs(x);
  const t = 1 / (1 + 0.3275911 * a);
  const poly =
    t *
    (0.254829592 +
      t * (-0.284496736 + t * (1.421413741 + t * (-1.453152027 + t * 1.061405429))));
  return s * (1 - poly * Math.exp(-a * a));
}

/** P(-k < Z < k) for a standard normal Z. */
export const centralMass = (k: number): number => erf(k / Math.SQRT2);

/** "68.3%" — one decimal place, like plot.scm's plot--percent. */
export const percent = (p: number): string =>
  `${(Math.round(Math.max(0, Math.min(1, p)) * 1000) / 10).toFixed(1)}%`;

/** Deterministic PRNG so server and client render identical "random" data. */
export function mulberry32(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/* ── Glyph charts ────────────────────────────────────────────────────── */

const SPARK = "▁▂▃▄▅▆▇█";

/** ▁▃▂▅█▆ — the glyph string `plot-sparkline-string` returns. */
export function sparkline(values: number[], lo?: number, hi?: number): string {
  const min = lo ?? Math.min(...values);
  const max = hi ?? Math.max(...values);
  const span = max - min || 1;
  return values
    .map((v) => SPARK[Math.max(0, Math.min(7, Math.round(((v - min) / span) * 7)))])
    .join("");
}

const EIGHTHS = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];

/** A horizontal bar `width` cells wide at most, in eighth-cell steps
 *  (`textplot-hbar`). */
export function hbar(value: number, max: number, width: number): string {
  const eighths = Math.round((value / max) * width * 8);
  const full = Math.floor(eighths / 8);
  return "█".repeat(full) + EIGHTHS[eighths % 8];
}

/** Vertical bars (a histogram) `height` rows tall, using ▁…█ for the
 *  fractional top cell. Returns rows top to bottom. */
export function vbars(counts: number[], height: number): string[] {
  const max = Math.max(...counts) || 1;
  const rows: string[] = [];
  for (let r = height - 1; r >= 0; r--) {
    let line = "";
    for (const c of counts) {
      const h = (c / max) * height * 8; // in eighths
      const inCell = Math.max(0, Math.min(8, Math.round(h - r * 8)));
      line += inCell === 0 ? " " : SPARK[inCell - 1];
    }
    rows.push(line);
  }
  return rows;
}

/** The viridis ramp, in the same eight steps as Macros' plot-heat-0…7. */
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
