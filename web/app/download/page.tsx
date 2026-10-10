import type { Metadata } from "next";
import { Nav } from "../components/Nav";
import { Footer } from "../components/Footer";
import { Mark } from "../components/Logo";

export const metadata: Metadata = {
  title: "Download",
  description: "Download Macros for macOS, Windows, and Linux.",
};

const VERSION = process.env.NEXT_PUBLIC_APP_VERSION || "0.1.0";
const REPO = "https://github.com/macros-app/macros";

// Release artifacts live on R2 behind downloads.macros.dev. Filenames are fixed
// by scripts/publish-release.mjs; the in-app updater resolves the same URLs from
// /latest.json (web/scripts/gen-version.mjs), so keep all three in lockstep.
const DOWNLOAD_BASE = "https://downloads.macros.dev";
const dl = (file: string) => `${DOWNLOAD_BASE}/${file}`;

type Platform = {
  os: string;
  note: string;
  links: { label: string; href: string }[];
};

const PLATFORMS: Platform[] = [
  {
    os: "macOS",
    note: "Apple Silicon · macOS 12+",
    links: [
      { label: "Apple Silicon (.zip)", href: dl(`Macros-macos-aarch64-${VERSION}.zip`) },
    ],
  },
  {
    os: "Windows",
    note: "x64 · Windows 10/11",
    links: [{ label: "Download .msi", href: dl(`Macros-windows-x86_64-${VERSION}.msi`) }],
  },
  {
    os: "Linux",
    note: "x86_64 · .deb / .rpm",
    links: [
      { label: ".deb (x86_64)", href: dl(`macros_${VERSION}_amd64.deb`) },
      { label: ".rpm (x86_64)", href: dl(`macros-${VERSION}-1.x86_64.rpm`) },
    ],
  },
];

export default function DownloadPage() {
  return (
    <>
      <Nav />
      <main style={{ maxWidth: 880, margin: "0 auto", padding: "64px 24px 0" }}>
        <div
          style={{
            display: "inline-flex",
            alignItems: "center",
            gap: 8,
            fontSize: 12.5,
            fontWeight: 500,
            color: "var(--text2)",
            background: "var(--card)",
            border: "1px solid var(--border2)",
            borderRadius: 999,
            padding: "5px 14px",
            marginBottom: 22,
          }}
        >
          <span style={{ color: "var(--accent)" }}>●</span>
          Now in preview · v{VERSION}
        </div>

        <div style={{ display: "flex", alignItems: "center", gap: 16, marginBottom: 18 }}>
          <Mark size={56} />
          <div>
            <h1
              style={{
                fontSize: 40,
                fontWeight: 700,
                letterSpacing: "-1px",
                margin: 0,
              }}
            >
              Download Macros
            </h1>
            <p className="mono" style={{ color: "var(--text3)", margin: "6px 0 0", fontSize: 13 }}>
              latest release · v{VERSION}
            </p>
          </div>
        </div>

        <p style={{ color: "var(--text2)", fontSize: 16, lineHeight: 1.6, maxWidth: 620 }}>
          Free and open source. Each button downloads the latest release (v
          {VERSION}) directly. macOS keeps itself up to date automatically; on
          Windows and Linux, re-download to update.
        </p>

        <div
          style={{
            marginTop: 32,
            border: "1px solid var(--border)",
            borderRadius: 3,
            overflow: "hidden",
            background: "var(--card)",
          }}
        >
          {PLATFORMS.map((p, i) => (
            <div
              key={p.os}
              style={{
                display: "flex",
                alignItems: "center",
                justifyContent: "space-between",
                gap: 16,
                padding: "20px 22px",
                borderTop: i === 0 ? "none" : "1px solid var(--border)",
                flexWrap: "wrap",
              }}
            >
              <div>
                <div style={{ fontSize: 17, fontWeight: 600 }}>{p.os}</div>
                <div style={{ fontSize: 13.5, color: "var(--text2)", marginTop: 3 }}>
                  {p.note}
                </div>
              </div>
              <div style={{ display: "flex", gap: 10, flexWrap: "wrap" }}>
                {p.links.map((l) => (
                  <a
                    key={l.label}
                    className="nav-pill nav-pill-light"
                    href={l.href}
                    target="_blank"
                    rel="noreferrer"
                  >
                    {l.label}
                  </a>
                ))}
              </div>
            </div>
          ))}
        </div>

        <div
          style={{
            marginTop: 28,
            padding: "20px 22px",
            border: "1px solid var(--border)",
            borderRadius: 3,
            background: "var(--surface)",
          }}
        >
          <h2 style={{ fontSize: 16, fontWeight: 600, margin: "0 0 10px" }}>
            Build from source
          </h2>
          <pre
            style={{
              margin: 0,
              background: "var(--code-bg)",
              border: "1px solid var(--code-border)",
              borderRadius: 3,
              padding: "14px 16px",
              overflowX: "auto",
              fontSize: 13,
              lineHeight: 1.6,
            }}
          >
            <code className="mono">{`git clone ${REPO}
cd macros
cargo run --release`}</code>
          </pre>
        </div>
      </main>
      <Footer />
    </>
  );
}
