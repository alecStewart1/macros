import type { CSSProperties } from "react";
import Link from "next/link";
import { Footer } from "../components/Footer";
import { SpectrumHero } from "./SpectrumHero";
import { Art, ArtLine, Scheme } from "./Art";
import { EditorShots } from "./EditorShots";
import {
  RAMP,
  bars,
  gauss,
  heatStrip,
  heatmapHalf,
  histogram,
  lines,
  normalCurve,
  rng,
  scatter,
  sparkline,
  walk,
  type Row,
} from "./textplot";
import s from "./spectrum.module.css";

const VERSION = process.env.NEXT_PUBLIC_APP_VERSION || "0.1.0";

/* ── Chart data (seeded, so every build renders the same art) ──────────── */

type Chart = {
  key: string;
  title: string;
  kind: string;
  blurb: string;
  code: string;
  rows: Row[];
  label: string;
  accent: string;
};

function buildCharts(): Chart[] {
  const r = rng(7);

  const cpu = walk(11, 72, 8, 96);
  const mem = walk(29, 72, 30, 70).map((v, i) => v * 0.55 + 18 + i * 0.35);

  const fast: [number, number][] = Array.from({ length: 70 }, () => {
    const x = 1 + r() * 9;
    return [x, 18 + x * 3.2 + gauss(r) * 4];
  });
  const slow: [number, number][] = Array.from({ length: 55 }, () => {
    const x = 1 + r() * 9;
    return [x, 38 + x * 4.6 + gauss(r) * 7];
  });

  const latencies = Array.from({ length: 900 }, () => Math.exp(Math.log(42) + gauss(r) * 0.32));

  const heat: number[][] = Array.from({ length: 12 }, (_, y) =>
    Array.from({ length: 34 }, (_, x) => {
      const a = Math.exp(-((x - 10) ** 2) / 40 - ((y - 4) ** 2) / 10);
      const b = 0.8 * Math.exp(-((x - 25) ** 2) / 30 - ((y - 8) ** 2) / 8);
      return a + b + 0.25 * Math.sin(x * 0.5 + y * 0.7) * 0.4 + r() * 0.08;
    }),
  );

  const spark = [1, 3, 2, 5, 8, 6];

  return [
    {
      key: "spark",
      title: "Sparkline",
      kind: "plot-sparkline",
      blurb: "Eight block heights that fit in a mode line, a dashboard row, or a sentence.",
      code: "(plot-sparkline (list 1 3 2 5 8 6))",
      rows: [
        (() => {
          const row: Row = [{ t: "(1 3 2 5 8 6)  ", fg: "#a39cb4" }];
          return row.concat(sparkline(spark));
        })(),
        [],
        [{ t: "cpu ", fg: "#a39cb4" }, ...sparkline(walk(3, 32, 0, 100))],
        [{ t: "mem ", fg: "#a39cb4" }, ...sparkline(walk(5, 32, 0, 100))],
        [{ t: "net ", fg: "#a39cb4" }, ...sparkline(walk(8, 32, 0, 100))],
        [{ t: "gc  ", fg: "#a39cb4" }, ...sparkline(walk(13, 32, 0, 100))],
      ],
      label: "Sparklines: the list 1 3 2 5 8 6 rendered as six block glyphs, then four 32-sample sparklines for cpu, mem, net and gc.",
      accent: RAMP[7],
    },
    {
      key: "bars",
      title: "Bar chart",
      kind: "plot-bars",
      blurb: "Labeled bars with eighth-block tips, so small differences still show.",
      code: `(plot-bars langs 'width 24)`,
      rows: bars(
        [
          ["rust", 48210],
          ["scheme", 15302],
          ["tsx", 6120],
          ["md", 3904],
          ["toml", 1188],
        ],
        24,
      ),
      label: "Horizontal bar chart of five labeled values: rust 48.2k, scheme 15.3k, tsx 6.1k, md 3.9k, toml 1.2k.",
      accent: RAMP[6],
    },
    {
      key: "lines",
      title: "Line plot",
      kind: "plot-lines",
      blurb: "Braille dots give 2 × 4 sub-cell resolution. Two series, one shared axis.",
      code: `(plot-lines load 'width 36 'height 8)`,
      rows: lines(
        [
          { name: "cpu", data: cpu, color: RAMP[7] },
          { name: "mem", data: mem, color: RAMP[4] },
        ],
        36,
        8,
      ),
      label: "Braille line plot of two series, cpu fluctuating between about 10 and 95 and mem rising steadily, on a 0 to 100 axis.",
      accent: RAMP[7],
    },
    {
      key: "scatter",
      title: "Scatter",
      kind: "plot-scatter",
      blurb: "Points, not lines. Where two series share a cell, the later one wins the color.",
      code: `(plot-scatter runs 'width 36 'height 8)`,
      rows: scatter(
        [
          { name: "fast", pts: fast, color: RAMP[5] },
          { name: "slow", pts: slow, color: RAMP[7] },
        ],
        36,
        8,
      ),
      label: "Braille scatter plot of two point clouds, fast and slow, both trending upward, with slow higher and more spread out.",
      accent: RAMP[5],
    },
    {
      key: "hist",
      title: "Histogram",
      kind: "plot-histogram",
      blurb: "Bin a list of numbers and draw the counts. Axes round out to whole tick steps.",
      code: `(plot-histogram latencies 'bins 12 'height 6)`,
      rows: histogram(latencies, 12, 6),
      label: "Histogram of 900 request latencies in 12 bins, right-skewed with a peak near 40 milliseconds.",
      accent: RAMP[4],
    },
    {
      key: "normal",
      title: "Normal curve",
      kind: "plot-normal",
      blurb: "Plot any function; shade ranges under the curve and get their probability.",
      code: `(plot-normal 0 1 'shade (list (list -3.5 -1.96) (list 1.96 3.5)))`,
      rows: normalCurve(36, 9, 1.96, RAMP[5], RAMP[7]),
      label: "Standard normal bell curve drawn in braille with both tails beyond plus and minus 1.96 sigma shaded, labeled 5.0 percent in the tails.",
      accent: RAMP[7],
    },
    {
      key: "heat",
      title: "Heatmap",
      kind: "plot-heatmap",
      blurb: "Half-block mode packs two data rows into every line with ▀ foreground and background.",
      code: `(plot-heatmap activity 'mode 'half)`,
      rows: heatmapHalf(heat, [
        [0, "00h"],
        [11, "08h"],
        [22, "16h"],
        [33, "24h"],
      ]),
      label: "Half-block heatmap of a 12 by 34 grid in eight viridis levels with two warm hotspots, plus a low-to-high color scale.",
      accent: RAMP[6],
    },
  ];
}

/* ── Features ──────────────────────────────────────────────────────────── */

type Feature = { title: string; body: string; href?: string; accent: "spark" | "heat" };

const FEATURES: Feature[] = [
  {
    title: "Steel, all the way down",
    body: "Commands, keymaps, modes, the mode line — every layer is Scheme you can read, redefine, and re-eval while the editor runs.",
    href: "/docs/scripting",
    accent: "heat",
  },
  {
    title: "Modal, your way",
    body: "Vim-style evil or selection-first helix, with counts, operators, text objects and dot-repeat. Or stay in Emacs keys.",
    href: "/docs/modal-editing",
    accent: "spark",
  },
  {
    title: "Magit",
    body: "Stage hunks, commit, branch, and read diffs in a buffer, powered by gitoxide underneath.",
    href: "/docs/git",
    accent: "heat",
  },
  {
    title: "Org mode",
    body: "Headings, TODOs, agenda, capture, tables, and babel. A real org-mode, written in Scheme.",
    href: "/docs/org-mode",
    accent: "spark",
  },
  {
    title: "Language servers",
    body: "Definitions, references, hover, rename, code actions, and diagnostics in real buffers.",
    href: "/docs/lsp",
    accent: "heat",
  },
  {
    title: "Debugger",
    body: "Breakpoints in the gutter, stepping, and a variables panel over the Debug Adapter Protocol.",
    accent: "spark",
  },
  {
    title: "Embedded terminal",
    body: "A real VT/ANSI terminal, alacritty-backed, running your login shell inside a buffer.",
    href: "/docs/terminal",
    accent: "heat",
  },
  {
    title: "Completion",
    body: "A corfu-style in-buffer popup with pluggable completion-at-point functions and an LSP fallback.",
    href: "/docs/completion",
    accent: "spark",
  },
  {
    title: "Text charts",
    body: "Sparklines, bars, braille plots, histograms, and heatmaps — just colored text, so they work in any buffer.",
    href: "/docs/plotting",
    accent: "heat",
  },
];

/* ── Page ──────────────────────────────────────────────────────────────── */

export default function SpectrumHome() {
  const charts = buildCharts();
  const ctaStrip = heatStrip(2026, 48);

  return (
    <div className={s.root}>
      {/* This candidate is dark-only; keep overscroll from flashing paper. */}
      <style>{`html,body{background:#0b0a10;color-scheme:dark}`}</style>

      <SpectrumHero />

      <main>
        {/* ── Intro ────────────────────────────────────────────────── */}
        <section className={s.intro}>
          <div className={s.wrap}>
            <div className={s.introGrid}>
              <p className={s.lede}>
                Macros takes what made Emacs worth learning — an editor written in its own extension
                language, customizable to the core — and rebuilds it on{" "}
                <strong>Steel</strong>, a fast embedded Scheme, with a GPU renderer underneath.
                Magit, evil, helix, org-mode, LSP, a debugger, and a terminal ship in the box.
              </p>
              <div className={s.introSide}>
                <div className={s.ctaRow}>
                  <Link href="/download" className={s.btnPrimary}>
                    Download Macros
                  </Link>
                  <Link href="/docs/getting-started" className={s.btnGhost}>
                    Get started
                  </Link>
                </div>
                <p className={s.introNote}>
                  v{VERSION} · macOS · Windows · Linux · free &amp; open source
                </p>
              </div>
            </div>

            <ul className={s.facts}>
              <li>
                <span className={s.factKey}>engine</span>
                <span className={s.factVal}>Rust + gpui, GPU-rendered</span>
              </li>
              <li>
                <span className={s.factKey}>language</span>
                <span className={s.factVal}>Steel Scheme, hot-reloadable</span>
              </li>
              <li>
                <span className={s.factKey}>modes</span>
                <span className={s.factVal}>emacs · evil · helix</span>
              </li>
            </ul>
          </div>
        </section>

        {/* ── The editor itself ────────────────────────────────────── */}
        <section className={s.shots} aria-labelledby="shots-title">
          <div className={s.wrap}>
            <p className={s.eyebrow}>A whole editor · not a plugin host</p>
            <h2 id="shots-title" className={s.h2}>
              Code, git, notes. One live image.
            </h2>
            <p className={s.sectionLede}>
              Steel Scheme you can evaluate in place, a Magit-style git porcelain, Org mode, and
              language servers for everything else. Every buffer below runs on the same Scheme you
              write your config in, so any of it can be rewritten while it runs.
            </p>
            <EditorShots />
          </div>
        </section>

        {/* ── Chart gallery ────────────────────────────────────────── */}
        <section className={s.gallery} aria-labelledby="gallery-title">
          <div className={s.wrap}>
            <p className={s.eyebrow}>New in Macros · scheme/plot.scm</p>
            <h2 id="gallery-title" className={s.h2}>
              Every buffer is a canvas.
            </h2>
            <p className={s.sectionLede}>
              Charts drawn from braille dots and block glyphs, colored with faces. Because a chart is
              just text, it works anywhere text does: a dashboard row, an Org file, the REPL, or a
              buffer of its own. Everything below is rendered from data on this page, the same way.
            </p>
          </div>

          <div className={s.band} role="region" aria-label="Chart gallery, scrolls horizontally" tabIndex={0}>
            {charts.map((c) => (
              <article
                key={c.key}
                className={s.card}
                style={{ "--glow": c.accent } as CSSProperties}
              >
                <header className={s.cardHead}>
                  <h3 className={s.cardTitle}>{c.title}</h3>
                  <span className={s.cardKind}>{c.kind}</span>
                </header>
                <div className={s.cardArt}>
                  <Art rows={c.rows} label={c.label} />
                </div>
                <p className={s.cardBlurb}>{c.blurb}</p>
                <Scheme code={c.code} className={s.cardCode} />
              </article>
            ))}
            <Link href="/docs/plotting" className={s.cardMore}>
              <span className={s.cardMoreBig}>M-x plot-demo</span>
              <span>Every chart type, live in the editor. Read the text charts guide →</span>
            </Link>
          </div>
        </section>

        {/* ── Feature grid ─────────────────────────────────────────── */}
        <section className={s.features} aria-labelledby="features-title">
          <div className={s.wrap}>
            <p className={s.eyebrow}>If you&apos;ve used Emacs</p>
            <h2 id="features-title" className={s.h2}>
              Batteries included.
              <br />
              <span className={s.h2Dim}>Every one of them is Scheme.</span>
            </h2>
            <div className={s.featureGrid}>
              {FEATURES.map((f, i) => {
                const accent: Row =
                  f.accent === "heat" ? heatStrip(100 + i * 17, 28) : sparkline(walk(200 + i * 13, 28));
                const inner = (
                  <>
                    <ArtLine
                      row={accent}
                      decorative
                      className={s.featureAccent}
                    />
                    <h3 className={s.featureTitle}>{f.title}</h3>
                    <p className={s.featureBody}>{f.body}</p>
                    {f.href ? <span className={s.featureMore} aria-hidden="true">Docs →</span> : null}
                  </>
                );
                return f.href ? (
                  <Link key={f.title} href={f.href} className={s.feature}>
                    {inner}
                  </Link>
                ) : (
                  <div key={f.title} className={s.feature}>
                    {inner}
                  </div>
                );
              })}
            </div>
          </div>
        </section>

        {/* ── In the editor ────────────────────────────────────────── */}
        <section className={s.demo} aria-labelledby="demo-title">
          <div className={`${s.wrap} ${s.demoGrid}`}>
            <div>
              <p className={s.eyebrow}>Scheme in, chart out</p>
              <h2 id="demo-title" className={s.h2}>
                Six lines to a live report.
              </h2>
              <p className={s.sectionLede}>
                Define a command, hand <code className={s.inlineCode}>plot-histogram</code> a list of
                numbers, and show it in its own buffer. Bind it to a key, run it from{" "}
                <kbd className={s.kbd}>M-x</kbd>, or drop the same plot into an Org file with{" "}
                <code className={s.inlineCode}>plot-insert</code>.
              </p>
              <pre className={s.codeBlock}>
                <Scheme
                  code={`(define-command (latency-report)
  (plot-show
    (plot-histogram (read-latencies "access.log")
                    'bins 12 'height 6
                    'title "latency (ms)")
    "*latency*"))`}
                />
              </pre>
              <p className={s.footnote}>
                <code className={s.inlineCode}>read-latencies</code> is yours to write: any list of
                numbers will do.{" "}
                <Link href="/docs/plotting" className={s.textLink}>
                  Text charts reference
                </Link>
              </p>
            </div>

            <figure className={s.window}>
              <div className={s.windowBar} aria-hidden="true">
                <span className={s.dots}>
                  <i />
                  <i />
                  <i />
                </span>
                <span className={s.windowTitle}>*latency*</span>
              </div>
              <div className={s.windowBody}>
                <Art
                  rows={[
                    [{ t: "latency (ms)", fg: "#f2eff7" }],
                    [],
                    ...charts.find((c) => c.key === "hist")!.rows,
                  ]}
                  label="The *latency* buffer showing the histogram titled latency (ms)."
                />
              </div>
              <figcaption className={s.modeline}>
                <span className={s.modeState}>NORMAL</span>
                <span>*latency*</span>
                <span className={s.modeSpark}>
                  <ArtLine row={sparkline(walk(41, 14, 0, 100))} decorative />
                </span>
                <span className={s.modeRight}>plot-mode</span>
              </figcaption>
            </figure>
          </div>
        </section>

        {/* ── Closing CTA ──────────────────────────────────────────── */}
        <section className={s.closing} aria-labelledby="closing-title">
          <div className={s.wrap}>
            <ArtLine row={ctaStrip} decorative className={s.closingStrip} />
            <h2 id="closing-title" className={s.closingTitle}>
              Make it yours,
              <br />
              in every color.
            </h2>
            <div className={s.ctaRow}>
              <Link href="/download" className={s.btnPrimary}>
                Download for macOS, Windows, Linux
              </Link>
              <Link href="/docs" className={s.btnGhost}>
                Read the docs
              </Link>
            </div>
          </div>
        </section>
      </main>

      <Footer />
    </div>
  );
}
