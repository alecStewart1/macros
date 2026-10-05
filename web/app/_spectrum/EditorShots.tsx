import type { ReactNode } from "react";
import s from "./editorShots.module.css";

/* Synthetic "screenshots": real Macros buffers (code from this repo, the real
   magit layout) drawn in HTML with the macros-dark theme's own face colors
   (scheme/themes/macros-dark.scm), so they stay crisp, light and in sync with
   the text. Each window is one role="img" with a description. */

type Tone =
  | "kw" | "fn" | "str" | "com" | "num" | "type" | "br" | "var"
  | "mHead" | "mLabel" | "mBranch" | "mHash" | "mFile" | "mHunk" | "mAdd" | "mDel" | "mCtx"
  | "oTodo" | "oDone" | "oTag" | "oDate" | "oTable" | "oMeta" | "oH1" | "oH2" | "oCheck"
  | "err" | "dim";

type Seg = [string, Tone?];
type Line = Seg[];

const SCHEME_KW = new Set(["define", "define-command", "let", "let*", "lambda", "if", "when", "unless", "cond", "else", "and", "or"]);

/** Scheme → tone runs: keywords, call heads, strings, numbers, comments, dim brackets. */
function scheme(src: string): Line[] {
  return src.split("\n").map((text) => {
    const out: Line = [];
    const re = /(;.*$)|("(?:[^"\\]|\\.)*")|([()])|(-?\d+(?:\.\d+)?(?![\w-]))|([^\s()";]+)|(\s+)/g;
    let m: RegExpExecArray | null;
    let afterParen = false;
    while ((m = re.exec(text))) {
      const [tok, com, str, paren, num, ident] = m;
      let tone: Tone | undefined;
      if (com) tone = "com";
      else if (str) tone = "str";
      else if (paren) tone = "br";
      else if (num) tone = "num";
      else if (ident) tone = SCHEME_KW.has(ident) ? "kw" : afterParen ? "fn" : "var";
      if (!/^\s+$/.test(tok)) afterParen = tok === "(";
      out.push([tok, tone]);
    }
    return out;
  });
}

const RUST_KW = new Set(["pub", "fn", "let", "if", "else", "match", "return", "for", "in", "mut", "const", "use"]);

function rust(src: string): Line[] {
  return src.split("\n").map((text) => {
    const out: Line = [];
    const re = /(\/\/.*$)|("(?:[^"\\]|\\.)*")|('.')|(\b\d+(?:\.\d+)?\b)|([A-Za-z_][\w]*!?)|([{}()[\];,.<>:&|=+\-*/!?']+)|(\s+)/g;
    let m: RegExpExecArray | null;
    while ((m = re.exec(text))) {
      const [tok, com, str, chr, num, ident] = m;
      let tone: Tone | undefined;
      if (com) tone = "com";
      else if (str || chr) tone = "str";
      else if (num) tone = "num";
      else if (ident) {
        if (RUST_KW.has(ident)) tone = "kw";
        else if (/^[A-Z]/.test(ident) || ["f64", "usize", "str", "bool"].includes(ident)) tone = "type";
        else if (text.slice(re.lastIndex).startsWith("(")) tone = "fn";
        else tone = "var";
      }
      out.push([tok, tone]);
    }
    return out;
  });
}

function Window({
  title,
  label,
  mode,
  pos,
  state = "NORMAL",
  lines,
  numbers = true,
  current,
  echo,
  overlay,
}: {
  title: string;
  label: string;
  mode: string;
  pos: string;
  state?: string;
  lines: Line[];
  numbers?: boolean;
  current?: number;
  echo?: ReactNode;
  overlay?: ReactNode;
}) {
  return (
    <figure className={s.win} role="img" aria-label={label}>
      <div className={s.titlebar} aria-hidden="true">
        <span className={s.lights}>
          <i />
          <i />
          <i />
        </span>
        <span className={s.title}>{title}</span>
      </div>
      <div className={s.body} aria-hidden="true">
        {lines.map((line, i) => (
          <div key={i} className={`${s.line} ${current === i ? s.current : ""}`}>
            {numbers && <span className={s.ln}>{i + 1}</span>}
            <span className={s.text}>
              {line.length === 0
                ? " "
                : line.map(([t, tone], j) => (
                    <span key={j} className={tone ? s[tone] : undefined}>
                      {t}
                    </span>
                  ))}
            </span>
          </div>
        ))}
        {overlay}
      </div>
      <div className={s.modeline} aria-hidden="true">
        <span className={s.state}>{state}</span>
        <span className={s.file}>{title}</span>
        <span className={s.mode}>{mode}</span>
        <span className={s.pos}>{pos}</span>
      </div>
      <div className={s.echo} aria-hidden="true">
        {echo ?? " "}
      </div>
    </figure>
  );
}

/* ── 1. Steel Scheme, evaluated live (scheme/plot.scm) ─────────────────── */

const PLOT_SCM = `;; Density of the normal distribution N(mu, sigma) at x.
(define (plot-normal-pdf x mu sigma)
  (let ((z (/ (- x mu) sigma)))
    (/ (exp (* -0.5 z z)) (* sigma (sqrt (* 2.0 plot--pi))))))

;; "68.3%" — a probability to one decimal place.
(define (plot--percent p)
  (let ((n (plot--int (* 1000.0 (max 0.0 (min 1.0 p))))))
    (string-append (number->string (quotient n 10)) "."
                   (number->string (remainder n 10)) "%")))

(define-command (normal-report)
  (plot-show (plot-normal 0 1 'shade (list -1 1)) "*normal*"))`;

/* ── 2. Magit status (this branch, abridged) ───────────────────────────── */

const MAGIT: Line[] = [
  [["Head:     ", "mLabel"], ["feat/text-plotting", "mBranch"], [" feat(mcp): MCP server for debugging"]],
  [["Merge:    ", "mLabel"], ["origin/main", "mBranch"], [" chore(deps): cargo update"]],
  [],
  [["Unstaged changes (44)", "mHead"]],
  [["modified   ", "mLabel"], ["scheme/plot.scm", "mFile"]],
  [["modified   ", "mLabel"], ["scheme/session.scm", "mFile"]],
  [["@@ -325,9 +325,19 @@", "mHunk"]],
  [["   (savehist-load)", "mCtx"]],
  [["-  (set! *desktop-primary* (session-claim-primary!))", "mDel"]],
  [["+  (set! *desktop-primary* (and (null? (session-launch-files))", "mAdd"]],
  [["+                              (session-claim-primary!)))", "mAdd"]],
  [["modified   ", "mLabel"], ["src/mcp.rs", "mFile"]],
  [["modified   ", "mLabel"], ["src/textplot.rs", "mFile"]],
  [],
  [["Recent commits", "mHead"]],
  [["19285c2 ", "mHash"], ["feat(mcp): MCP server for debugging and package dev"]],
  [["d291a01 ", "mHash"], ["fix(plot): round y axes out to whole tick steps"]],
  [["a05805e ", "mHash"], ["feat(plot): text charts — canvas primitives, plot.scm"]],
];

/* ── 3. Org mode ───────────────────────────────────────────────────────── */

const ORG: Line[] = [
  [["#+title: ", "oMeta"], ["Release 0.5", "oH1"]],
  [],
  [["* ", "oH1"], ["Text charts ", "oH1"], ["[3/4]", "oCheck"], ["                       :plot:", "oTag"]],
  [["** ", "oH2"], ["DONE ", "oDone"], ["Braille canvas + plot.scm", "oH2"]],
  [["** ", "oH2"], ["DONE ", "oDone"], ["Shaded normal curves", "oH2"]],
  [["** ", "oH2"], ["DONE ", "oDone"], ["Heatmaps, half-block mode", "oH2"]],
  [["** ", "oH2"], ["TODO ", "oTodo"], ["Docs page polish", "oH2"]],
  [["   DEADLINE: ", "oMeta"], ["<2026-10-09 Fri>", "oDate"]],
  [["* ", "oH1"], ["MCP server ", "oH1"], ["[1/2]", "oCheck"], ["                         :tools:", "oTag"]],
  [["  - [X] ", "oCheck"], ["Bearer token + Host/Origin checks"]],
  [["  - [ ] ", "oCheck"], ["Screenshot tool"]],
  [],
  [["| area   | files | tests |", "oTable"]],
  [["|--------+-------+-------|", "oTable"]],
  [["| plot   |     3 |    15 |", "oTable"]],
  [["| mcp    |     2 |     7 |", "oTable"]],
];

/* ── 4. Rust with LSP completion (src/textplot.rs) ─────────────────────── */

const TEXTPLOT_RS = `/// Fixed-decimal formatting with no \`-0\`.
pub fn format_number(v: f64, decimals: usize) -> String {
    let s = format!("{v:.decimals$}");
    if s.trim_start_matches('-')
        .chars()
        .all(|c| c == '0' || c == '.')
    {
        s.trim_start_matches('-').to_string()
    } else {
        s
    }
}

pub fn axis_label(v: f64) -> String {
    format_`;

export function EditorShots() {
  const rustLines = rust(TEXTPLOT_RS);
  return (
    <div className={s.grid}>
      <Window
        title="plot.scm"
        label="Macros editing scheme/plot.scm, Steel Scheme with syntax highlighting. The echo area shows M-: (plot--percent 0.6826894) evaluating to 68.3% in the running editor."
        mode="(Steel Macros LSP)"
        pos="L7  C2  Top"
        lines={scheme(PLOT_SCM)}
        current={6}
        echo={
          <>
            <span className={s.prompt}>M-: </span>
            <span className={s.fn}>(plot--percent</span> <span className={s.num}>0.6826894</span>
            <span className={s.br}>)</span>
            <span className={s.dim}> ⇒ </span>
            <span className={s.str}>&quot;68.3%&quot;</span>
          </>
        }
      />
      <Window
        title="magit: macros"
        label="A Magit status buffer for the Macros repository: branch feat/text-plotting, unstaged changes with an expanded diff hunk in scheme/session.scm, and recent commits."
        mode="(Magit evil)"
        pos="L9  Top"
        lines={MAGIT}
        numbers={false}
        current={8}
        echo={
          <>
            <span className={s.dim}>s</span> stage <span className={s.dim}>  u</span> unstage{" "}
            <span className={s.dim}>  c</span> commit <span className={s.dim}>  P</span> push{" "}
            <span className={s.dim}>  ?</span> help
          </>
        }
      />
      <Window
        title="release.org"
        label="An Org mode file: a release checklist with TODO and DONE headings, progress cookies, tags, a deadline, checkboxes and a table."
        mode="(Org)"
        pos="L7  C9  All"
        state="INSERT"
        lines={ORG}
        current={6}
        echo={
          <>
            <span className={s.dim}>C-c C-t</span> cycle TODO <span className={s.dim}>  TAB</span> fold{" "}
            <span className={s.dim}>  C-c a</span> agenda
          </>
        }
      />
      <Window
        title="textplot.rs"
        label="Rust in Macros with a language server: src/textplot.rs open, a completion popup offering format_number and format_compact while typing format_, with a diagnostic on the line."
        mode="(Rust LSP rust-analyzer)"
        pos="L15  C12  Bot"
        state="INSERT"
        lines={rustLines}
        current={14}
        echo={
          <>
            <span className={s.err}>●</span> expected expression, found end of input{" "}
            <span className={s.dim}>(rust-analyzer)</span>
          </>
        }
        overlay={
          <div className={s.popup} style={{ top: `calc(${15} * var(--lh) + 6px)` }}>
            <div className={`${s.item} ${s.sel}`}>
              <span className={s.kind}>fn</span> format_compact<span className={s.sig}>(v: f64) → String</span>
            </div>
            <div className={s.item}>
              <span className={s.kind}>fn</span> format_number<span className={s.sig}>(v: f64, decimals…</span>
            </div>
            <div className={s.item}>
              <span className={s.kind}>mac</span> format!<span className={s.sig}>(…)</span>
            </div>
          </div>
        }
      />
    </div>
  );
}
