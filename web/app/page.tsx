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
    title: "Dired and the sidebar",
    body: "A directory editor plus an expandable file-tree sidebar with icons and file operations. Move around the filesystem without reaching for the mouse.",
  },
  {
    title: "Diagnostics",
    body: "A flycheck-style diagnostics list with gutter dots, plus next-error and previous-error to walk them.",
  },
  {
    title: "Modal editing",
    body: "Opt into Vim-style (evil) or selection-first (helix) modal editing — built on the keymaps, fully overridable in Scheme.",
  },
  {
    title: "Magit",
    body: "A Magit-style git porcelain for staging, committing, branching, and reading diffs — all in a buffer, powered by gitoxide (gix) behind the scenes.",
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
