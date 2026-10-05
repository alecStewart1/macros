"use client";

import { useEffect, useMemo, useRef, useState } from "react";
import { centralMass, percent } from "./braille";
import { normalPlot, type XTick } from "./charts";
import { BraillePlot } from "./Plot";
import s from "./phosphor.module.css";

/* One loop of the sweep, in ms: hold at ±1σ, widen to ±1.96σ, hold, return. */
const HOLD_LO = 1400;
const UP = 2400;
const HOLD_HI = 2800;
const DOWN = 1600;
const CYCLE = HOLD_LO + UP + HOLD_HI + DOWN;
const K_LO = 1;
const K_HI = 1.96;

const ease = (t: number) => (t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2);

function kAt(t: number): number {
  if (t < HOLD_LO) return K_LO;
  t -= HOLD_LO;
  if (t < UP) return K_LO + (K_HI - K_LO) * ease(t / UP);
  t -= UP;
  if (t < HOLD_HI) return K_HI;
  t -= HOLD_HI;
  return K_HI - (K_HI - K_LO) * ease(Math.min(1, t / DOWN));
}

export function NormalHero() {
  const [k, setK] = useState(K_LO);
  const [paused, setPaused] = useState(false);
  const [reduced, setReduced] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const elapsed = useRef(0); // position within the cycle, kept across pauses

  // prefers-reduced-motion: show the finished 95% state, never animate.
  useEffect(() => {
    const mq = window.matchMedia("(prefers-reduced-motion: reduce)");
    const sync = () => setReduced(mq.matches);
    sync();
    mq.addEventListener("change", sync);
    return () => mq.removeEventListener("change", sync);
  }, []);

  useEffect(() => {
    if (reduced) {
      setK(K_HI);
      return;
    }
    if (paused) return;

    let raf = 0;
    let last = 0;
    let running = false;

    const tick = (now: number) => {
      if (last) elapsed.current = (elapsed.current + (now - last)) % CYCLE;
      last = now;
      // Quantize so React only re-renders when the plot can actually change.
      setK(Math.round(kAt(elapsed.current) * 100) / 100);
      raf = requestAnimationFrame(tick);
    };
    const start = () => {
      if (running) return;
      running = true;
      last = 0;
      raf = requestAnimationFrame(tick);
    };
    const stop = () => {
      running = false;
      cancelAnimationFrame(raf);
    };

    // Only animate while the hero is on screen.
    const io = new IntersectionObserver(([entry]) =>
      entry.isIntersecting ? start() : stop(),
    );
    if (root.current) io.observe(root.current);
    else start();

    return () => {
      stop();
      io.disconnect();
    };
  }, [paused, reduced]);

  const plot = useMemo(() => normalPlot(k), [k]);
  const mass = percent(centralMass(k));
  const kText = k.toFixed(2);

  // Amber carets under the shade edges, riding along with the sweep.
  const xTicks: XTick[] = useMemo(() => {
    const at = (x: number) => (x + 4) / 8;
    return [
      ...plot.xTicks,
      { at: at(-k), text: "▲", accent: true },
      { at: at(k), text: "▲", accent: true },
    ];
  }, [plot, k]);

  return (
    <div ref={root} className={s.heroPlot}>
      <div className={s.counterRow} aria-hidden="true">
        <div>
          <div className={s.counterLabel}>
            P(−{kText}σ &lt; X &lt; {kText}σ)
          </div>
          <div className={s.counter}>
            {mass}
          </div>
        </div>
        <div className={s.counterSide}>
          <span className={s.dim}>N(μ=0, σ=1)</span>
          <span>
            shaded: <span className={s.amber}>{mass}</span>
          </span>
        </div>
      </div>

      <BraillePlot
        rows={plot.rows}
        yTicks={plot.yTicks}
        xTicks={xTicks}
        labelCols={4}
        maxFont={15}
        label="Braille-dot plot of the standard normal distribution, drawn as text. The area under the curve is shaded between minus k and plus k standard deviations; it sweeps from ±1σ, covering 68.3% of the probability, out to ±1.96σ, covering 95.0%."
      />

      <div className={s.echo}>
        <code className={s.echoCode}>
          <span className={s.tokParen}>(</span>
          <span className={s.tokFn}>plot-normal</span> <span className={s.tokNum}>0 1</span>{" "}
          <span className={s.tokSym}>&apos;shade</span> <span className={s.tokParen}>(</span>
          <span className={s.tokFn}>list</span>{" "}
          <span className={s.tokNum}>
            -{kText} {kText}
          </span>
          <span className={s.tokParen}>))</span>
        </code>
        {!reduced && (
          <button
            type="button"
            className={s.pauseBtn}
            onClick={() => setPaused((p) => !p)}
            aria-pressed={paused}
            aria-label={paused ? "Resume the chart animation" : "Pause the chart animation"}
          >
            {paused ? "▶ resume" : "❚❚ pause"}
          </button>
        )}
      </div>
    </div>
  );
}
