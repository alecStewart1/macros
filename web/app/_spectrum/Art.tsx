import type { CSSProperties } from "react";
import type { Row } from "./textplot";
import s from "./spectrum.module.css";

/** Renders colored text rows as a single monospace block of text art. */
export function Art({
  rows,
  label,
  className,
  style,
}: {
  rows: Row[];
  label: string;
  className?: string;
  style?: CSSProperties;
}) {
  return (
    <pre role="img" aria-label={label} className={`${s.art} ${className ?? ""}`} style={style}>
      {rows.map((row, i) => (
        <span key={i}>
          {row.map((seg, j) =>
            seg.fg || seg.bg ? (
              <span key={j} style={{ color: seg.fg, backgroundColor: seg.bg }}>
                {seg.t}
              </span>
            ) : (
              seg.t
            ),
          )}
          {i < rows.length - 1 ? "\n" : null}
        </span>
      ))}
    </pre>
  );
}

/**
 * One inline row (feature accents, sparklines in prose). Pass `decorative`
 * for ornamental rows (e.g. inside links or captions): they're hidden from
 * assistive tech instead of adding an image label to the accessible name.
 */
export function ArtLine(
  props: { row: Row; className?: string } & ({ label: string; decorative?: false } | { decorative: true; label?: never }),
) {
  const { row, className } = props;
  const a11y = props.decorative
    ? ({ "aria-hidden": true } as const)
    : ({ role: "img", "aria-label": props.label } as const);
  return (
    <span {...a11y} className={`${s.artLine} ${className ?? ""}`}>
      {row.map((seg, j) => (
        <span key={j} style={{ color: seg.fg, backgroundColor: seg.bg }}>
          {seg.t}
        </span>
      ))}
    </span>
  );
}

/** Minimal Scheme highlighter for the one-line calls under each chart. */
export function Scheme({ code, className }: { code: string; className?: string }) {
  const re = /("[^"]*")|(;[^\n]*)|('[^\s()'"]+)|([()])|(-?\d+(?:\.\d+)?(?![\w-]))|([^\s()'";]+)|(\s+)/g;
  const out: React.ReactNode[] = [];
  let m: RegExpExecArray | null;
  let afterParen = false;
  let k = 0;
  while ((m = re.exec(code))) {
    const [tok, str, com, sym, paren, num, ident] = m;
    let cls: string | undefined;
    if (str) cls = s.tokStr;
    else if (com) cls = s.tokCom;
    else if (sym) cls = s.tokSym;
    else if (paren) cls = s.tokParen;
    else if (num) cls = s.tokNum;
    else if (ident) cls = afterParen ? s.tokFn : s.tokId;
    if (!/^\s+$/.test(tok)) afterParen = tok === "(";
    out.push(
      cls ? (
        <span key={k++} className={cls}>
          {tok}
        </span>
      ) : (
        tok
      ),
    );
  }
  return <code className={`${s.code} ${className ?? ""}`}>{out}</code>;
}
