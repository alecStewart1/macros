import type { CSSProperties, ReactNode } from "react";
import type { Row } from "./braille";
import type { XTick } from "./charts";
import s from "./phosphor.module.css";

/** Face name → CSS-module class. Empty face = unlit cells. */
export function faceClass(face: string): string | undefined {
  return face ? s[`face_${face}`] : undefined;
}

/** A braille plot laid out like Macros' plot frame: y labels, an L-shaped
 *  axis, the dot canvas, x labels underneath. The canvas holds nothing but
 *  braille glyphs (U+2800 for blanks), so columns stay aligned whichever
 *  font supplies them. Font size scales with the container (cqi) so the
 *  whole plot fits a phone without horizontal scroll. */
export function BraillePlot({
  rows,
  yTicks = {},
  xTicks = [],
  label,
  labelCols = 4,
  maxFont = 15,
  footer,
}: {
  rows: Row[];
  yTicks?: Record<number, string>;
  xTicks?: XTick[];
  label: string;
  labelCols?: number;
  maxFont?: number;
  footer?: ReactNode;
}) {
  const cols = rows[0]?.reduce((n, seg) => n + seg.text.length, 0) ?? 0;
  const style = {
    "--cols": cols + labelCols + 2,
    "--label-cols": labelCols,
    "--fmax": `${maxFont}px`,
  } as CSSProperties;

  return (
    <figure className={s.plot} role="img" aria-label={label} style={style}>
      <div className={s.plotInner} aria-hidden="true">
        <div className={s.yAxis}>
          {rows.map((_, r) => (
            <div key={r} className={s.plotRow}>
              {yTicks[r] ?? ""}
            </div>
          ))}
        </div>
        <div className={s.canvasCol}>
          <div className={s.canvas}>
            {rows.map((row, r) => (
              <div key={r} className={s.plotRow}>
                {row.map((seg, i) => (
                  <span key={i} className={faceClass(seg.face)}>
                    {seg.text}
                  </span>
                ))}
              </div>
            ))}
          </div>
          {xTicks.length > 0 && (
            <div className={s.xAxis}>
              {xTicks.map((t, i) => (
                <span
                  key={i}
                  className={t.accent ? s.xTickAccent : s.xTick}
                  style={{
                    left: `${t.at * 100}%`,
                    transform:
                      t.at <= 0.001
                        ? "none"
                        : t.at >= 0.999
                          ? "translateX(-100%)"
                          : "translateX(-50%)",
                  }}
                >
                  {t.text}
                </span>
              ))}
            </div>
          )}
          {footer}
        </div>
      </div>
    </figure>
  );
}
