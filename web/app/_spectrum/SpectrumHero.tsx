"use client";

import Link from "next/link";
import { useEffect, useRef, useState } from "react";
import { Mark } from "../components/Logo";
import { RAMP } from "./textplot";
import s from "./spectrum.module.css";

const REPO = "https://codeberg.org/macros-app/macros";
const BG = "#0b0a10";
const FRAME_MS = 1000 / 30; // the field is smooth; 30 fps is plenty
const MAX_RIPPLES = 8;
const RIPPLE_LIFE = 2.6; // seconds
const RIPPLE_SPEED = 380; // css px / second

type Ripple = { x: number; y: number; t0: number; amp: number };

/**
 * Full-bleed heatmap mosaic. Every character cell is a `▀` half block: the
 * top half is the glyph's foreground, the bottom half its background — the
 * same trick `(plot-heatmap … 'mode 'half)` uses. A wave field is sampled at
 * two points per cell, quantized into the 8 viridis levels, and painted into
 * one <canvas> (one batched path per level, so ~8 fills per frame).
 */
export function SpectrumHero() {
  const sectionRef = useRef<HTMLElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const titleRef = useRef<HTMLHeadingElement>(null);
  const [grid, setGrid] = useState<{ cols: number; rows: number } | null>(null);

  useEffect(() => {
    const section = sectionRef.current;
    const canvas = canvasRef.current;
    const title = titleRef.current;
    if (!section || !canvas || !title) return;
    const ctx = canvas.getContext("2d", { alpha: false });
    if (!ctx) return;

    const motion = window.matchMedia("(prefers-reduced-motion: reduce)");

    let W = 0;
    let H = 0;
    let cw = 14; // cell width, css px
    let ch = 28; // cell height (two square halves)
    let cols = 0;
    let rows = 0;
    let mask = new Float32Array(0); // headline legibility mask, per sample
    let level = new Uint8Array(0);
    const ripples: Ripple[] = [];
    const ptr = { x: 0, y: 0, sx: 0, sy: 0, on: 0, target: 0 };
    let lastSpawn = { x: -1e4, y: -1e4, t: -10 };
    let lastActivity = -10;
    let lastAmbient = 0;
    const t0 = performance.now();
    const now = () => (performance.now() - t0) / 1000;

    let raf = 0;
    let lastFrame = 0;
    let visible = true;
    let running = false;

    function layout() {
      const rect = section!.getBoundingClientRect();
      W = Math.max(1, Math.round(rect.width));
      H = Math.max(1, Math.round(rect.height));
      cw = W < 640 ? 10 : W < 1100 ? 12 : 14;
      ch = cw * 2;
      cols = Math.ceil(W / cw);
      rows = Math.ceil(H / ch);
      const dpr = Math.min(2, window.devicePixelRatio || 1);
      canvas!.width = Math.round(W * dpr);
      canvas!.height = Math.round(H * dpr);
      ctx!.setTransform(dpr, 0, 0, dpr, 0, 0);

      // Keep the field bright (level ≥ 3) behind the knocked-out headline so
      // the ink-colored letters always read: soft rounded-rect falloff.
      const tr = title!.getBoundingClientRect();
      const x0 = tr.left - rect.left - 24;
      const x1 = tr.right - rect.left + 24;
      const y0 = tr.top - rect.top - 16;
      const y1 = tr.bottom - rect.top + 16;
      const n = cols * rows * 2;
      mask = new Float32Array(n);
      level = new Uint8Array(n);
      for (let r = 0; r < rows * 2; r++) {
        const py = r * (ch / 2) + ch / 4;
        for (let c = 0; c < cols; c++) {
          const px = c * cw + cw / 2;
          const dx = Math.max(x0 - px, 0, px - x1);
          const dy = Math.max(y0 - py, 0, py - y1);
          const d = Math.hypot(dx, dy);
          const k = Math.min(1, d / 70);
          mask[r * cols + c] = 1 - k * k * (3 - 2 * k);
        }
      }
      setGrid({ cols, rows });
    }

    function spawn(x: number, y: number, t: number, amp = 1) {
      ripples.push({ x, y, t0: t, amp });
      if (ripples.length > MAX_RIPPLES) ripples.shift();
    }

    function frame(t: number, ambient = true) {
      // ease the pointer lens toward the pointer
      ptr.sx += (ptr.x - ptr.sx) * 0.18;
      ptr.sy += (ptr.y - ptr.sy) * 0.18;
      ptr.on += (ptr.target - ptr.on) * 0.08;

      // ambient ripple when nobody is steering, so it never looks frozen
      if (ambient && t - lastActivity > 3 && t - lastAmbient > 3.2) {
        lastAmbient = t;
        spawn(W * (0.15 + 0.7 * Math.random()), H * (0.2 + 0.6 * Math.random()), t, 0.7);
      }
      for (let i = ripples.length - 1; i >= 0; i--)
        if (t - ripples[i].t0 > RIPPLE_LIFE) ripples.splice(i, 1);

      const sigma2 = 2 * 120 * 120;
      const half = ch / 2;
      const R = rows * 2;
      for (let r = 0; r < R; r++) {
        const v = r * half + half / 2;
        for (let c = 0; c < cols; c++) {
          const u = c * cw + cw / 2;
          let f =
            0.5 +
            0.21 * Math.sin(u * 0.0105 + t * 0.55) +
            0.17 * Math.sin(v * 0.016 - t * 0.42 + 2 * Math.sin(u * 0.0037 + t * 0.18)) +
            0.12 * Math.sin((u + v) * 0.0075 + t * 0.85);
          if (ptr.on > 0.01) {
            const dx = u - ptr.sx;
            const dy = v - ptr.sy;
            f += 0.36 * ptr.on * Math.exp(-(dx * dx + dy * dy) / sigma2);
          }
          for (let k = 0; k < ripples.length; k++) {
            const rp = ripples[k];
            const age = t - rp.t0;
            const band = Math.hypot(u - rp.x, v - rp.y) - age * RIPPLE_SPEED;
            if (band > 90 || band < -90) continue;
            const fade = 1 - age / RIPPLE_LIFE;
            f += 0.42 * rp.amp * fade * Math.exp(-(band * band) / 1400) * Math.cos(band * 0.075);
          }
          const i = r * cols + c;
          const m = mask[i];
          f = f < 0 ? 0 : f > 1 ? 1 : f;
          if (m > 0) f = f * (1 - m) + (0.42 + 0.58 * f) * m;
          const L = (f * 8) | 0;
          level[i] = L > 7 ? 7 : L;
        }
      }

      ctx!.fillStyle = BG;
      ctx!.fillRect(0, 0, W, H);
      const gw = cw - 1; // 1px gutter between character cells
      for (let L = 0; L < 8; L++) {
        ctx!.beginPath();
        for (let r = 0; r < R; r++) {
          // top half = ▀ foreground; bottom half = cell background (no seam),
          // the gutter sits under each full cell
          const y = r * half;
          const hh = r & 1 ? half - 1 : half;
          const base = r * cols;
          for (let c = 0; c < cols; c++)
            if (level[base + c] === L) ctx!.rect(c * cw, y, gw, hh);
        }
        ctx!.fillStyle = RAMP[L];
        ctx!.fill();
      }
    }

    function loop(ts: number) {
      raf = requestAnimationFrame(loop);
      const dt = ts - lastFrame;
      if (dt < FRAME_MS) return;
      // carry the remainder so frames land on the FRAME_MS grid (steady fps
      // instead of drifting late by up to one display refresh each frame);
      // resync after a long gap (tab hidden, loop restarted)
      lastFrame = dt > FRAME_MS * 4 ? ts : ts - (dt % FRAME_MS);
      frame(now());
    }

    function start() {
      if (running || motion.matches || !visible || document.hidden) return;
      running = true;
      raf = requestAnimationFrame(loop);
    }
    function stop() {
      running = false;
      cancelAnimationFrame(raf);
    }
    function renderStatic() {
      ripples.length = 0;
      ptr.on = 0;
      frame(7.3, false); // a pleasant, fixed moment of the field (deterministic: no ambient ripple)
    }

    function local(e: PointerEvent) {
      const rect = section!.getBoundingClientRect();
      return { x: e.clientX - rect.left, y: e.clientY - rect.top };
    }
    function onMove(e: PointerEvent) {
      if (motion.matches) return;
      const p = local(e);
      const t = now();
      ptr.x = p.x;
      ptr.y = p.y;
      if (ptr.target === 0) {
        ptr.sx = p.x;
        ptr.sy = p.y;
      }
      ptr.target = 1;
      lastActivity = t;
      if (Math.hypot(p.x - lastSpawn.x, p.y - lastSpawn.y) > 110 && t - lastSpawn.t > 0.14) {
        spawn(p.x, p.y, t, 0.75);
        lastSpawn = { x: p.x, y: p.y, t };
      }
    }
    function onDown(e: PointerEvent) {
      if (motion.matches) return;
      const p = local(e);
      const t = now();
      lastActivity = t;
      spawn(p.x, p.y, t, 1.25);
      lastSpawn = { x: p.x, y: p.y, t };
    }
    function onLeave() {
      ptr.target = 0;
    }

    layout();
    if (motion.matches) renderStatic();
    else frame(now());
    start();

    const ro = new ResizeObserver(() => {
      layout();
      if (motion.matches || !running) renderStatic();
    });
    ro.observe(section);
    ro.observe(title); // web font swap changes the headline box → remask

    // Moving the window between displays changes devicePixelRatio without
    // resizing the section, so re-run layout (backing store size) on change.
    // A resolution query only matches one dpr, so re-arm it each time.
    let dprQuery: MediaQueryList | null = null;
    const onDpr = () => {
      layout();
      if (motion.matches || !running) renderStatic();
      armDpr();
    };
    function armDpr() {
      dprQuery?.removeEventListener("change", onDpr);
      dprQuery = window.matchMedia(`(resolution: ${window.devicePixelRatio || 1}dppx)`);
      dprQuery.addEventListener("change", onDpr);
    }
    armDpr();

    const io = new IntersectionObserver(
      ([entry]) => {
        visible = entry.isIntersecting;
        if (visible) start();
        else stop();
      },
      { threshold: 0 },
    );
    io.observe(section);

    const onVis = () => (document.hidden ? stop() : start());
    document.addEventListener("visibilitychange", onVis);

    const onMotion = () => {
      if (motion.matches) {
        stop();
        renderStatic();
      } else start();
    };
    motion.addEventListener("change", onMotion);

    section.addEventListener("pointermove", onMove);
    section.addEventListener("pointerdown", onDown);
    section.addEventListener("pointerleave", onLeave);

    return () => {
      stop();
      ro.disconnect();
      io.disconnect();
      dprQuery?.removeEventListener("change", onDpr);
      document.removeEventListener("visibilitychange", onVis);
      motion.removeEventListener("change", onMotion);
      section.removeEventListener("pointermove", onMove);
      section.removeEventListener("pointerdown", onDown);
      section.removeEventListener("pointerleave", onLeave);
    };
  }, []);

  return (
    <section ref={sectionRef} className={s.hero} aria-labelledby="spectrum-title">
      <canvas
        ref={canvasRef}
        className={s.heroCanvas}
        role="img"
        aria-label="A full-screen heatmap made of half-block text cells in eight viridis colors, rippling in waves that follow the pointer."
      />
      <div className={s.heroFade} aria-hidden="true" />

      <header className={s.topbar}>
        <Link href="/" className={s.brand} aria-label="Macros home">
          <Mark size={26} />
          <span className={s.brandWord}>(macros)</span>
        </Link>
        <nav className={s.topnav} aria-label="Primary">
          <Link href="/docs" className={s.topLink}>
            Docs
          </Link>
          <Link href="/docs/plotting" className={`${s.topLink} ${s.hideSm}`}>
            Text charts
          </Link>
          <a href={REPO} className={`${s.topLink} ${s.hideSm}`} target="_blank" rel="noreferrer">
            Codeberg
          </a>
          <Link href="/download" className={s.topCta}>
            Download
          </Link>
        </nav>
      </header>

      <div className={s.heroInner}>
        <Link href="/docs/plotting" className={s.chip}>
          <span className={s.chipDot} aria-hidden="true" />
          New: text charts in any buffer
          <span aria-hidden="true"> →</span>
        </Link>
        <h1 id="spectrum-title" ref={titleRef} className={s.heroTitle}>
          <span>Rewrite it</span>{" "}
          <span>while it runs.</span>
        </h1>
      </div>

      <div className={s.heroMeta}>
        <code className={s.heroCaption}>
          <span className={s.tokFn}>(plot-heatmap</span> field{" "}
          <span className={s.tokSym}>&apos;mode &apos;half</span>
          <span className={s.tokParen}>)</span>
          <span className={s.heroDims} aria-hidden="true">
            {grid ? ` ; ${grid.cols}×${grid.rows} cells · 8 levels` : " ; 8 levels"}
          </span>
        </code>
        <div className={s.legend} aria-hidden="true">
          {RAMP.map((c) => (
            <span key={c} style={{ color: c }}>
              █
            </span>
          ))}
          <span className={s.legendText}>move · tap</span>
        </div>
      </div>
    </section>
  );
}
