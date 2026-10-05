import type { Metadata } from "next";
import localFont from "next/font/local";
import Link from "next/link";
import type { ReactNode } from "react";
import latest from "../../../public/latest.json";
import { NormalHero } from "./NormalHero";
import { BraillePlot, faceClass } from "./Plot";
import {
  BARS,
  HEATMAP,
  HEAT_ROWS,
  STATS,
  functionPlot,
  histogram,
  scatterPlot,
} from "./charts";
import { VIRIDIS } from "./braille";
import s from "./phosphor.module.css";

// The editor's chart font: braille and block glyphs drawn on the same 0.6em
// grid as the monospace labels. System fallbacks draw braille wider, which
// pushes plots out of their panes.
const plotFont = localFont({
  src: "./MacrosPlot-Regular.ttf",
  // Chart glyphs only (blocks, ■, braille). The font also carries a blank
  // "m" (gpui won't load a font without one); limiting the range keeps every
  // letter on the text font instead.
  declarations: [{ prop: "unicode-range", value: "U+2580-25A0, U+2800-28FF" }],
  variable: "--font-plot",
  display: "swap",
});

export const metadata: Metadata = {
  title: "Phosphor — homepage design candidate",
  description:
    "A design candidate for the Macros homepage: the page as a live Macros buffer, with charts drawn from braille text.",
  robots: { index: false, follow: false },
};

const REPO = "https://codeberg.org/macros-app/macros";
const VERSION: string = latest.version;

export default function PhosphorHome() {
  return (
    <div className={`${s.root} ${plotFont.variable}`}>
      <a className={s.skip} href="#main">
        Skip to content
      </a>
      <TabLine />
      <main id="main">
        <Hero />
        <Dashboard />
        <Packages />
        <InitScm />
        <Closing />
      </main>
      <ModeLineFooter />
    </div>
  );
}

/* ── Chrome ──────────────────────────────────────────────────────────── */

function Kbd({ children }: { children: ReactNode }) {
  return <kbd className={s.kbd}>{children}</kbd>;
}

function TabLine() {
  return (
    <header className={s.tabline}>
      <div className={s.tablineInner}>
        <Link href="/" className={s.brand} aria-label="Macros home">
          <span className={s.brandParen}>(</span>macros<span className={s.brandParen}>)</span>
        </Link>
        <nav aria-label="Primary" className={s.tabs}>
          <Link href="/docs" className={s.tab}>
            docs
          </Link>
          <Link href="/docs/plotting" className={s.tab}>
            plotting
          </Link>
          <a href={REPO} className={`${s.tab} ${s.hideSm}`} target="_blank" rel="noreferrer">
            codeberg
          </a>
          <Link href="/download" className={`${s.tab} ${s.tabActive}`}>
            download
          </Link>
        </nav>
      </div>
    </header>
  );
}

/** A window: buffer contents with an Emacs mode-line along the bottom. */
function Pane({
  buffer,
  mode,
  pos = "All",
  flag = "-:---",
  minor,
  children,
  className,
  bodyClass,
}: {
  buffer: string;
  mode: string;
  pos?: string;
  flag?: string;
  minor?: string;
  children: ReactNode;
  className?: string;
  bodyClass?: string;
}) {
  return (
    <div className={`${s.pane} ${className ?? ""}`}>
      <div className={`${s.paneBody} ${bodyClass ?? ""}`}>{children}</div>
      <div className={s.modeline}>
        <span className={s.mlFlag}>{flag}</span>
        <span className={s.mlBuf}>{buffer}</span>
        <span className={s.mlPos}>{pos}</span>
        <span className={s.mlMode}>
          ({mode}
          {minor ? <span className={s.mlMinor}> {minor}</span> : null})
        </span>
      </div>
    </div>
  );
}

function SectionHead({
  id,
  comment,
  title,
  children,
}: {
  id: string;
  comment: string;
  title: ReactNode;
  children?: ReactNode;
}) {
  return (
    <div className={s.sectionHead}>
      <p className={s.comment}>;; {comment}</p>
      <h2 id={id} className={s.h2}>
        {title}
      </h2>
      {children ? <div className={s.lede}>{children}</div> : null}
    </div>
  );
}

/* ── Hero ────────────────────────────────────────────────────────────── */

function Hero() {
  return (
    <section className={s.hero} aria-labelledby="hero-title">
      <div className={s.heroCopy}>
        <p className={s.comment}>
          ;; -*- mode: macros -*- &nbsp;v{VERSION}
        </p>
        <h1 id="hero-title" className={s.h1}>
          The editor
          <br />
          you program.
          <span className={s.cursor} aria-hidden="true" />
        </h1>
        <p className={s.heroTag}>Even the charts are text.</p>
        <p className={s.heroBody}>
          Macros is an Emacs-style editor with a GPU renderer written in Rust and
          everything else written in <strong>Steel Scheme</strong>. Magit, org-mode,
          evil and helix, LSP, a debugger and a real terminal ship in the box — and
          every line of them is Scheme you can read and change.
        </p>
        <div className={s.ctas}>
          <Link href="/download" className={s.btnPrimary}>
            Download Macros
          </Link>
          <Link href="/docs/getting-started" className={s.btnGhost}>
            Read the docs
          </Link>
        </div>
        <p className={s.hint}>
          macOS · Windows · Linux &nbsp;—&nbsp; free &amp; open source (MIT)
        </p>
      </div>

      <Pane
        buffer="*plot-normal*"
        mode="Plot"
        minor="Braille"
        className={s.heroPane}
        bodyClass={s.heroPaneBody}
      >
        <NormalHero />
      </Pane>
    </section>
  );
}

/* ── Dashboard: charts as text ───────────────────────────────────────── */

function Dashboard() {
  const fn = functionPlot();
  const sc = scatterPlot();
  const hist = histogram();

  return (
    <section className={s.section} aria-labelledby="charts-title">
      <SectionHead id="charts-title" comment="M-x plot-demo" title="Charts are just text.">
        <p>
          <code className={s.inlineCode}>plot.scm</code> draws sparklines, bars, braille
          line and scatter plots, histograms and heatmaps out of characters, colored with
          faces. A chart drops into a dashboard row, an Org file or a REPL — anywhere text
          goes. Every chart on this page is rendered the same way: 2×4 dots to a glyph.
        </p>
        <p>
          <Link href="/docs/plotting" className={s.textLink}>
            Text charts in the docs →
          </Link>
        </p>
      </SectionHead>

      <Pane buffer="*macros-repo*" mode="Dashboard" pos="Top" flag="%%---" minor="auto-revert">
        <h3 className={s.paneHeading}>
          <span className={s.dim}>▾</span> Repository{" "}
          <span className={s.dim}>· snapshot from wc -l and git log</span>
        </h3>
        <ul className={s.stats}>
          {STATS.map((st) => (
            <li key={st.unit} className={s.stat}>
              <div className={s.statTop}>
                <span className={s.statValue}>{st.value}</span>
                <span
                  className={s.spark}
                  role="img"
                  aria-label={`Sparkline: ${st.note}`}
                >
                  {st.spark}
                </span>
              </div>
              <div className={s.statUnit}>{st.unit}</div>
              <div className={s.statNote}>{st.note}</div>
            </li>
          ))}
        </ul>

        <div className={s.chartGrid}>
          <div className={s.chartCell}>
            <h3 className={s.chartTitle}>
              plot-function <span className={s.dim}>· two functions, braille</span>
            </h3>
            <BraillePlot
              rows={fn.rows}
              yTicks={fn.yTicks}
              xTicks={fn.xTicks}
              labelCols={4}
              maxFont={14}
              label="Braille line plot of two functions from 0 to 4π: sin x in green, and a damped cosine, e to the minus x over 6 times cos 2x, in amber, which starts at 1 and decays toward zero."
            />
            <p className={s.legend}>
              <span className={faceClass("s0")}>━ sin x</span>
              <span className={faceClass("s1")}>━ e^(−x/6)·cos 2x</span>
            </p>
            <Snippet>
              (plot-function (list (list &quot;sin&quot; sin) (list &quot;damped&quot; damped)) 0
              12.57 &apos;width 56 &apos;height 13)
            </Snippet>
          </div>

          <div className={s.chartCell}>
            <h3 className={s.chartTitle}>
              plot-histogram <span className={s.dim}>· 2000 samples, {hist.bins} bins</span>
            </h3>
            <figure
              className={s.glyphFig}
              role="img"
              aria-label="Histogram of 2000 standard-normal samples in 30 bins, drawn with block characters; a bell shape peaking in the middle."
            >
              <pre className={`${s.glyphPre} ${faceClass("s0")}`} aria-hidden="true">
                {hist.rows.join("\n")}
                {"\n"}
                <span className={s.axisLine}>{"▔".repeat(hist.bins)}</span>
              </pre>
            </figure>
            <Snippet>(plot-histogram samples &apos;bins 30 &apos;height 7)</Snippet>

            <h3 className={s.chartTitle}>
              plot-scatter <span className={s.dim}>· two clusters</span>
            </h3>
            <BraillePlot
              rows={sc.rows}
              yTicks={sc.yTicks}
              xTicks={sc.xTicks}
              labelCols={3}
              maxFont={14}
              label="Braille scatter plot of two clusters of 70 points each: a green cluster at the upper left and an amber cluster at the lower right."
            />
          </div>

          <div className={s.chartCell}>
            <h3 className={s.chartTitle}>
              plot-bars <span className={s.dim}>· lines of code</span>
            </h3>
            <div
              className={s.bars}
              role="img"
              aria-label="Bar chart of lines of code: Rust 60,615; Scheme 20,178; Markdown docs 3,419."
            >
              {BARS.map((b) => (
                <div key={b.label} className={s.barRow} aria-hidden="true">
                  <span className={s.barLabel}>{b.label}</span>
                  <span className={`${s.barGlyphs} ${faceClass(b.face)}`}>{b.bar}</span>
                  <span className={s.barValue}>{b.value.toLocaleString("en-US")}</span>
                </div>
              ))}
            </div>
            <Snippet>(plot-bars loc &apos;width 26)</Snippet>
          </div>

          <div className={s.chartCell}>
            <h3 className={s.chartTitle}>
              plot-heatmap <span className={s.dim}>· commits, weekday × hour</span>
            </h3>
            <figure
              className={s.heatFig}
              role="img"
              aria-label="Viridis heatmap of this repository's commits by weekday and hour of day. Most activity falls in the afternoon and evening, with the hottest cell on Thursday at 21:00."
            >
              <div className={s.heat} aria-hidden="true">
                {HEATMAP.map((row, r) => (
                  <div key={r} className={s.heatRow}>
                    <span className={s.heatLabel}>{HEAT_ROWS[r]}</span>
                    {row.map((c, i) => (
                      <span key={i} className={s.heatCell} style={{ background: c }} />
                    ))}
                  </div>
                ))}
                <div className={s.heatRow}>
                  <span className={s.heatLabel} />
                  <span className={s.heatAxis}>
                    <span>00</span>
                    <span>06</span>
                    <span>12</span>
                    <span>18</span>
                    <span>23</span>
                  </span>
                </div>
                <div className={s.heatScale}>
                  <span className={s.dim}>0</span>
                  {VIRIDIS.map((c) => (
                    <span key={c} className={s.heatCell} style={{ background: c }} />
                  ))}
                  <span className={s.dim}>33</span>
                </div>
              </div>
            </figure>
            <Snippet>(plot-heatmap commits &apos;row-labels days)</Snippet>
          </div>
        </div>
      </Pane>
    </section>
  );
}

function Snippet({ children }: { children: ReactNode }) {
  return (
    <p className={s.snippet}>
      <span className={s.prompt} aria-hidden="true">
        M-:
      </span>{" "}
      <code>{children}</code>
    </p>
  );
}

/* ── Built-in packages, as a magit-style section list ────────────────── */

const PACKAGES: { name: string; keys: string[]; what: string; href: string }[] = [
  { name: "magit", keys: ["C-x g"], what: "Stage, commit, branch and read diffs in a buffer, backed by gitoxide.", href: "/docs/git" },
  { name: "org-mode", keys: ["C-c a"], what: "Headings, TODOs, agenda, capture, tables and babel — a real org, in Scheme.", href: "/docs/org-mode" },
  { name: "evil", keys: ["(evil-mode)"], what: "Vim motions, operators, text objects, registers, dot-repeat.", href: "/docs/modal-editing" },
  { name: "helix", keys: ["(helix-mode)"], what: "Selection-first editing with multiple selections.", href: "/docs/helix" },
  { name: "lsp", keys: ["M-.", "M-?"], what: "Definitions, references, hover, rename, code actions, diagnostics.", href: "/docs/lsp" },
  { name: "dap", keys: ["M-x", "dap-debug"], what: "Step debugging over DAP, with breakpoint signs in the gutter.", href: "/docs" },
  { name: "term", keys: ["C-c C-t"], what: "An alacritty-backed VT terminal running your login shell, in a buffer.", href: "/docs/terminal" },
  { name: "completion", keys: ["Tab"], what: "A corfu-style popup over completion-at-point functions, with LSP behind it.", href: "/docs/completion" },
  { name: "plot", keys: ["M-x", "plot-demo"], what: "Braille and block-glyph charts, colored with faces.", href: "/docs/plotting" },
];

function Packages() {
  return (
    <section className={s.section} aria-labelledby="packages-title">
      <SectionHead
        id="packages-title"
        comment="If you've used Emacs"
        title="You'll recognize the neighborhood."
      >
        <p>
          The packages you would install first are already here, rebuilt in Scheme and
          bound to the keys your fingers remember.
        </p>
      </SectionHead>

      <Pane buffer="magit: macros" mode="Magit" pos="Top" minor="evil">
        <h3 className={s.paneHeading}>
          <span className={s.dim}>▾</span> Built in{" "}
          <span className={s.dim}>({PACKAGES.length})</span>
        </h3>
        <ul className={s.pkgList}>
          {PACKAGES.map((p) => (
            <li key={p.name}>
              <Link href={p.href} className={s.pkg}>
                <span className={s.pkgName}>{p.name}</span>
                <span className={s.pkgKeys}>
                  {p.keys.map((k) =>
                    k.startsWith("(") || k === "dap-debug" || k === "plot-demo" ? (
                      <code key={k} className={s.pkgCmd}>
                        {k}
                      </code>
                    ) : (
                      <Kbd key={k}>{k}</Kbd>
                    ),
                  )}
                </span>
                <span className={s.pkgWhat}>{p.what}</span>
              </Link>
            </li>
          ))}
        </ul>
      </Pane>
    </section>
  );
}

/* ── init.scm ────────────────────────────────────────────────────────── */

const INIT_SCM = `;; ~/.config/macros/init.scm
(evil-mode)                        ; or (helix-mode)
(set-option "font-family" "JetBrains Mono")

;; define-command = define + register for M-x
(define-command (insert-date)
  (insert-string (call-process "date" '("+%Y-%m-%d"))))

(bind-key "fundamental" "ctrl-c d" 'insert-date)

;; charts are text: drop one at point
(plot-insert (plot-normal 0 1 'shade (list -1.96 1.96)))`;

const SPECIAL = new Set(["define-command", "define", "lambda", "let", "let*", "if", "cond"]);

/** Just enough Scheme highlighting for one buffer. */
function highlight(line: string): ReactNode[] {
  const out: ReactNode[] = [];
  const re = /(;.*$)|("(?:[^"\\]|\\.)*")|('[\w\-+*/!?<>=]+)|(-?\d+(?:\.\d+)?)(?![\w-])|([()])|([^\s()";']+)|(\s+)|(.)/g;
  let m: RegExpExecArray | null;
  let prevParen = false;
  let i = 0;
  while ((m = re.exec(line))) {
    const [tok, comment, str, sym, num, paren, word] = m;
    let cls: string | undefined;
    if (comment) cls = s.tokComment;
    else if (str) cls = s.tokStr;
    else if (sym) cls = s.tokSym;
    else if (num) cls = s.tokNum;
    else if (paren) cls = s.tokParen;
    else if (word) cls = prevParen ? (SPECIAL.has(word) ? s.tokKw : s.tokFn) : undefined;
    out.push(
      <span key={i++} className={cls}>
        {tok}
      </span>,
    );
    if (!/^\s+$/.test(tok)) prevParen = tok === "(";
  }
  return out;
}

function InitScm() {
  const lines = INIT_SCM.split("\n");
  return (
    <section className={`${s.section} ${s.split}`} aria-labelledby="init-title">
      <SectionHead id="init-title" comment="Scheme all the way down" title="Your config is the editor's language.">
        <p>
          Commands, keymaps, modes and the mode-line are Steel Scheme. Define a command
          and it is on <Kbd>M-x</Kbd>; bind it with a <code className={s.inlineCode}>&apos;symbol</code>{" "}
          and load order stops mattering. Built-ins are the same kind of code as yours, so
          reading them is how you learn.
        </p>
        <ul className={s.keyHints}>
          <li>
            <Kbd>M-x</Kbd> run any command by name
          </li>
          <li>
            <Kbd>C-x C-f</Kbd> find a file
          </li>
          <li>
            <Kbd>C-h t</Kbd> the built-in tutorial
          </li>
        </ul>
        <p>
          <Link href="/docs/scripting" className={s.textLink}>
            Scripting guide →
          </Link>{" "}
          &nbsp;
          <Link href="/docs/example-config" className={s.textLink}>
            A real init.scm →
          </Link>
        </p>
      </SectionHead>

      <Pane buffer="init.scm" mode="Scheme" pos="All" flag="-:**-" minor="LSP evil">
        <pre className={s.code}>
          {lines.map((ln, n) => (
            <span key={n} className={s.codeLine}>
              <span className={s.lineNo} aria-hidden="true">
                {String(n + 1).padStart(2, " ")}
              </span>
              {highlight(ln)}
              {"\n"}
            </span>
          ))}
        </pre>
      </Pane>
    </section>
  );
}

/* ── Closing call to action ──────────────────────────────────────────── */

function Closing() {
  return (
    <section className={s.closing} aria-labelledby="closing-title">
      <h2 id="closing-title" className={s.h2}>
        Open a buffer. Change anything.
      </h2>
      <p className={s.lede}>
        Version {VERSION} for macOS, Windows and Linux. It updates itself, and you can
        turn that off.
      </p>
      <div className={s.ctas}>
        <Link href="/download" className={s.btnPrimary}>
          Download Macros
        </Link>
        <Link href="/docs" className={s.btnGhost}>
          Browse the docs
        </Link>
      </div>
    </section>
  );
}

/* ── Footer: the mode-line and echo area ─────────────────────────────── */

function ModeLineFooter() {
  return (
    <footer className={s.footer}>
      <div className={`${s.modeline} ${s.footerModeline}`}>
        <span className={s.mlFlag}>-:%%-</span>
        <span className={s.mlBuf}>macros.dev</span>
        <span className={s.mlPos}>Bot</span>
        <span className={s.mlMode}>
          (Phosphor<span className={s.mlMinor}> MIT</span>)
        </span>
        <span className={s.mlRight}>v{VERSION}</span>
      </div>
      <nav className={s.echoArea} aria-label="Footer">
        <span className={s.dim}>Wrote macros.dev —</span>
        <Link href="/download">download</Link>
        <Link href="/docs">docs</Link>
        <Link href="/docs/getting-started">getting started</Link>
        <Link href="/docs/keybindings">keybindings</Link>
        <Link href="/docs/plotting">plotting</Link>
        <a href={REPO} target="_blank" rel="noreferrer">
          codeberg
        </a>
        <span className={s.dim}>· free software, MIT</span>
      </nav>
    </footer>
  );
}
