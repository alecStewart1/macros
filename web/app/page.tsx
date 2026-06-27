import Link from "next/link";
import { Nav } from "./components/Nav";
import { Footer } from "./components/Footer";

export default function Home() {
  return (
    <>
      <Nav />
      <main>
        <Hero />
        <Screenshot />
        <FeatureStrip />
        <WhyIBuiltThis />
      </main>
      <Footer />
    </>
  );
}

/* ── Hero ──────────────────────────────────────────────────────────────── */

function Hero() {
  return (
    <section
      style={{
        position: "relative",
        textAlign: "center",
        padding: "92px 24px 56px",
        background:
          "radial-gradient(60% 60% at 50% 0%, var(--hero-glow), transparent 70%)",
      }}
    >
      <h1
        className="lp-hero-title"
        style={{
          fontSize: 60,
          lineHeight: 1.05,
          fontWeight: 700,
          letterSpacing: "-1.6px",
          margin: "0 auto 22px",
          maxWidth: 880,
        }}
      >
        The Emacs you know,{" "}
        <span style={{ color: "var(--accent)" }}>with a modern engine.</span>
      </h1>

      <p
        style={{
          fontSize: 19,
          lineHeight: 1.6,
          color: "var(--text2)",
          maxWidth: 640,
          margin: "0 auto 34px",
        }}
      >
        Macros takes the thing that made Emacs worth learning, an editor written
        in its own extension language and customizable to the core, and builds it
        again on{" "}
        <strong style={{ color: "var(--text)", fontWeight: 600 }}>Steel</strong>,
        a fast embedded Scheme, with a GPU renderer underneath. It is batteries
        included: Magit, evil-mode, org-mode and roam, helm, transient, and LSP
        all ship in the box. Every bit of it is Scheme you can read and change.
      </p>

      <div
        style={{
          display: "flex",
          gap: 12,
          justifyContent: "center",
          flexWrap: "wrap",
        }}
      >
        <Link className="btn-accent" href="/download">
          Download for macOS, Windows, Linux
        </Link>
        <Link className="btn-ghost" href="/docs/getting-started">
          Read the docs
        </Link>
      </div>

      <div
        className="mono"
        style={{
          marginTop: 22,
          fontSize: 13,
          color: "var(--text3)",
        }}
      >
        free &amp; open source
      </div>
    </section>
  );
}

/* ── Screenshot ────────────────────────────────────────────────────────── */

function Screenshot() {
  return (
    <section style={{ padding: "8px 24px 24px" }}>
      {/* Rendered directly (no wrapping card): the screenshot's own window
          chrome and transparency sit on the page background, at its natural
          aspect ratio. */}
      {/* eslint-disable-next-line @next/next/no-img-element */}
      <img
        src="/screenshots/macros-dark.webp"
        alt="The Macros editor in the macros-dark theme"
        style={{
          display: "block",
          width: "100%",
          maxWidth: 1040,
          height: "auto",
          margin: "0 auto",
        }}
      />
    </section>
  );
}

/* ── Secondary feature strip ───────────────────────────────────────────── */

const STRIP: { title: string; body: string }[] = [
  {
    title: "Org mode",
    body: "Headings, TODOs, agenda, capture, tables, and babel. A real org-mode, written in Scheme.",
  },
  {
    title: "Embedded terminal",
    body: "A real VT/ANSI terminal (alacritty-backed) running your login shell, right inside a buffer.",
  },
  {
    title: "Completion",
    body: "A corfu-style in-buffer popup, with pluggable completion-at-point functions and an LSP fallback.",
  },
  {
    title: "Dired and Treemacs",
    body: "A directory editor and a file-tree sidebar with icons. Move around the filesystem without reaching for the mouse.",
  },
  {
    title: "Diagnostics",
    body: "A flycheck-style diagnostics list with gutter dots, plus next-error and previous-error to walk them.",
  },
  {
    title: "Rainbow delimiters",
    body: "Bracket coloring by nesting depth, driven by tree-sitter so it stays right inside strings and comments.",
  },
  {
    title: "Magit",
    body: "A Magit-style git porcelain for staging, committing, branching, and reading diffs — all in a buffer.",
  },
  {
    title: "LSP",
    body: "Language servers for go-to-definition, references, hover, rename, code actions, and diagnostics, in real buffers.",
  },
];

function FeatureStrip() {
  return (
    <Section
      eyebrow="If you've used Emacs"
      title="You'll recognize the neighborhood, popular packages are built-in"
    >
      <div
        className="lp-grid-3"
        style={{
          display: "grid",
          gridTemplateColumns: "repeat(3, 1fr)",
          gap: 16,
          marginTop: 36,
        }}
      >
        {STRIP.map((f) => (
          <div
            key={f.title}
            style={{
              padding: "20px 4px",
              borderTop: "1px solid var(--border)",
            }}
          >
            <h3 style={{ fontSize: 16, fontWeight: 600, margin: "0 0 7px" }}>
              {f.title}
            </h3>
            <p
              style={{
                fontSize: 14,
                lineHeight: 1.6,
                color: "var(--text2)",
                margin: 0,
              }}
            >
              {f.body}
            </p>
          </div>
        ))}
      </div>
    </Section>
  );
}

/* ── Why I built this ──────────────────────────────────────────────────── */

function WhyIBuiltThis() {
  return (
    <Section eyebrow="Why I built this" title="Going back to where it started">
      <div
        style={{
          marginTop: 28,
          display: "flex",
          flexDirection: "column",
          gap: 18,
          fontSize: 17,
          lineHeight: 1.7,
          color: "var(--text2)",
        }}
      >
        <p style={{ margin: 0 }}>
          I&apos;ve used Emacs for more than twenty years, and I genuinely love
          it. It&apos;s been a constant companion through my whole career, and so
          much of what I&apos;ve enjoyed about programming traces back to it. I
          never would have learned Lisp if it weren&apos;t for Emacs, and learning
          Lisp shaped my career in ways I couldn&apos;t have predicted. It set me
          down the path of programming languages, which led to a roughly 10+ year
          stretch where I wrote compilers and related tooling, a completely
          different trajectory than the one I had been on.
        </p>
        <p style={{ margin: 0 }}>
          Tinkering in it and trying new packages has always been a joy: getting
          the one thing just right. Emacs with Magit
          is the only git porcelain I&apos;ve ever actually missed when working
          somewhere else. Wiring up keybindings to search and navigate a
          codebase with Helm sharpened how quickly I could grok an unfamiliar
          repo. Org mode is the only note-taking habit that ever
          stuck for me, and using it as a data science notebook with R, instead
          of the usual tools, was genuinely useful and a lot of fun.
        </p>
        <p style={{ margin: 0 }}>
          Mostly, though, this is fun. The state of Emacs felt stale, and I
          wanted to tinker again, to go back to where it all started, except
          this time I write in Scheme.
        </p>
      </div>
    </Section>
  );
}

/* ── Section shell ─────────────────────────────────────────────────────── */

function Section({
  eyebrow,
  title,
  children,
}: {
  eyebrow: string;
  title: string;
  children: React.ReactNode;
}) {
  return (
    <section style={{ padding: "56px 24px 0" }}>
      <div style={{ maxWidth: 1040, margin: "0 auto" }}>
        <div
          style={{
            fontSize: 12.5,
            fontWeight: 600,
            textTransform: "uppercase",
            letterSpacing: "0.1em",
            color: "var(--accent)",
            marginBottom: 12,
          }}
        >
          {eyebrow}
        </div>
        <h2
          style={{
            fontSize: 32,
            fontWeight: 700,
            letterSpacing: "-0.7px",
            margin: 0,
            maxWidth: 720,
            lineHeight: 1.15,
          }}
        >
          {title}
        </h2>
        {children}
      </div>
    </section>
  );
}
