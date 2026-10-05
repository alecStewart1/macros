import { heatStrip } from "../_spectrum/textplot";

/** Small stable hash, so each page gets its own (but build-stable) strip. */
function seedOf(key: string): number {
  let h = 2166136261;
  for (let i = 0; i < key.length; i++) h = Math.imul(h ^ key.charCodeAt(i), 16777619);
  return (h >>> 0) % 100000;
}

/** A one-line half-block heat strip in the viridis ramp: the docs' page
 *  accent, drawn the same way the editor draws plot-heatmap 'half. */
export function DocStrip({ seed, cells = 40 }: { seed: string; cells?: number }) {
  return (
    <span className="doc-strip" aria-hidden="true">
      {heatStrip(seedOf(seed), cells).map((seg, i) => (
        <span key={i} style={{ color: seg.fg, backgroundColor: seg.bg }}>
          {seg.t}
        </span>
      ))}
    </span>
  );
}
