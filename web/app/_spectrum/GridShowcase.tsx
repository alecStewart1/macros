"use client";

import { useState } from "react";
import Link from "next/link";
import s from "./gridShowcase.module.css";

/* Real screenshots of a grid buffer (synthetic sales data): one file flipped
   between the grid and its raw CSV, then the formula engine at work. */

const VIEWS = {
  grid: {
    src: "/screenshots/grid-overview.png",
    alt: "orders.csv open as a grid in Macros: auto-fitted columns, prices formatted as currency, discounts as percentages, late and pending statuses colored, and a selected range summarized in the mode line as 36 cells, sum 1,709.03, average 47.47.",
  },
  text: {
    src: "/screenshots/grid-raw.png",
    alt: "The same orders.csv after the toggle: plain comma-separated text in an ordinary text buffer.",
  },
} as const;

type View = keyof typeof VIEWS;

export function GridShowcase() {
  const [view, setView] = useState<View>("grid");

  return (
    <div className={s.root}>
      {/* ── One file, two views ─────────────────────────────────────── */}
      <div className={s.toggleRow}>
        <div className={s.stage}>
          <div className={s.switch} role="group" aria-label="Show orders.csv as">
            <button
              type="button"
              className={view === "grid" ? s.on : undefined}
              aria-pressed={view === "grid"}
              onClick={() => setView("grid")}
            >
              Grid
            </button>
            <button
              type="button"
              className={view === "text" ? s.on : undefined}
              aria-pressed={view === "text"}
              onClick={() => setView("text")}
            >
              CSV text
            </button>
          </div>
          <div className={s.frame}>
            {(Object.keys(VIEWS) as View[]).map((v) => (
              <img
                key={v}
                src={VIEWS[v].src}
                alt={VIEWS[v].alt}
                width={900}
                height={640}
                className={`${s.shot} ${v === view ? s.shown : s.hidden}`}
                aria-hidden={v !== view}
                loading="lazy"
              />
            ))}
          </div>
        </div>

        <div className={s.side}>
          <h3 className={s.h3}>Switch between the grid and the text</h3>
          <p className={s.body}>
            Some days you want cells, and some days you want to see the raw lines.{" "}
            <kbd>C-c C-t</kbd> (<kbd>g r</kbd> in evil) switches the buffer between the two, and any
            edits you haven&apos;t saved yet come along.
          </p>
          <ul className={s.points}>
            <li>
              Your keybindings come with you. <kbd>hjkl</kbd> or <kbd>C-n</kbd>, undo, the kill
              ring, and your own rebinds all work on cells.
            </li>
            <li>Saving only rewrites the rows you changed. Quoting, line endings, and encoding stay as they were.</li>
            <li>What you type is what gets saved, so <code>007</code> stays <code>007</code>.</li>
          </ul>
        </div>
      </div>

      {/* ── Excel compatible ────────────────────────────────────────── */}
      <div className={s.excel}>
        <div className={s.excelHead}>
          <h3 className={s.h3}>Formulas that are familiar</h3>
          <p className={s.body}>
            We wrote the formula engine from scratch in Rust and followed Excel&apos;s rules closely:
            the same syntax, the same error values, even the same rounding quirks. It covers
            roughly 95% of what most people do in a spreadsheet, and the docs list what&apos;s
            missing. While you type, you can arrow over to the cell you mean, and if you forget a
            closing parenthesis it gets added for you.
          </p>
        </div>

        <dl className={s.stats}>
          <div>
            <dt>150</dt>
            <dd>Built-in functions, SUMIFS and XLOOKUP included</dd>
          </div>
          <div>
            <dt>.xlsx</dt>
            <dd>Files open and save with their formulas intact</dd>
          </div>
          <div>
            <dt>48 ms</dt>
            <dd>To recalculate 100,000 formulas</dd>
          </div>
          <div>
            <dt>λ</dt>
            <dd>Write your own functions in Scheme</dd>
          </div>
        </dl>

        <div className={s.pair}>
          <figure className={s.fig}>
            <img
              src="/screenshots/grid-formulas.png"
              alt="A revenue column computed with =F2*G2*(1-H2) and a region summary using =SUMIFS(J:J,C:C,L2). The cell being edited shows its formula with each reference colored and the referenced columns outlined in matching colors."
              width={900}
              height={640}
              className={s.shot}
              loading="lazy"
            />
            <figcaption>Totals by region with SUMIFS. Each reference gets its own color.</figcaption>
          </figure>
          <figure className={s.fig}>
            <img
              src="/screenshots/grid-completion.png"
              alt="Typing =SUMI opens a dropdown listing SUMIF and SUMIFS with their argument lists, beside a panel showing the selected function's signature."
              width={900}
              height={640}
              className={s.shot}
              loading="lazy"
            />
            <figcaption>Start typing a function name and the arguments show up.</figcaption>
          </figure>
        </div>

        <p className={s.more}>
          <Link href="/docs/grid">Read the grid guide →</Link>
          <Link href="/docs/grid-formulas">Formulas and the full function list →</Link>
        </p>
      </div>
    </div>
  );
}
