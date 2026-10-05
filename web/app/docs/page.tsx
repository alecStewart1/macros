import type { Metadata } from "next";
import Link from "next/link";
import { NAV_GROUPS } from "@/lib/docs";
import { DocStrip } from "./DocStrip";

export const metadata: Metadata = {
  title: "Documentation",
  description:
    "Documentation for Macros — install, configure, and master the editor's features, commands, and scripting API.",
};

export default function DocsOverview() {
  return (
    <article className="doc-article" style={{ padding: "44px 40px 24px", maxWidth: 880 }}>
      <DocStrip seed="docs-overview" cells={56} />
      <div className="doc-eyebrow">Documentation</div>
      <h1
        style={{
          fontSize: "clamp(34px, 5vw, 48px)",
          fontWeight: 700,
          letterSpacing: "-1.4px",
          lineHeight: 1.05,
          margin: "0 0 16px",
        }}
      >
        Learn Macros
      </h1>
      <p
        style={{
          fontSize: 17,
          lineHeight: 1.65,
          color: "var(--text2)",
          maxWidth: 640,
          margin: "0 0 14px",
        }}
      >
        Everything you need to install, configure, and extend Macros. New here?
        Start with{" "}
        <Link href="/docs/getting-started" style={{ color: "var(--link)" }}>
          Getting started
        </Link>
        , then open the in-editor tutorial with <kbd>C-h t</kbd>.
      </p>

      <div style={{ marginTop: 40, display: "flex", flexDirection: "column", gap: 38 }}>
        {NAV_GROUPS.map((group) => (
          <section key={group.title}>
            <h2 className="docs-group-title" style={{ padding: 0, margin: "0 0 14px" }}>
              {group.title}
            </h2>
            <div
              style={{
                display: "grid",
                gridTemplateColumns: "repeat(auto-fill, minmax(260px, 1fr))",
                gap: 12,
              }}
            >
              {group.items.map((item) => (
                <Link
                  key={item.slug}
                  href={`/docs/${item.slug}`}
                  className="feature-card"
                  style={{
                    display: "block",
                    padding: "16px 18px",
                    textDecoration: "none",
                    color: "inherit",
                  }}
                >
                  <DocStrip seed={item.slug} cells={10} />
                  <div style={{ fontSize: 15, fontWeight: 600, marginBottom: 5 }}>
                    {item.label}
                  </div>
                  <div style={{ fontSize: 13, color: "var(--text2)", lineHeight: 1.5 }}>
                    {item.blurb}
                  </div>
                </Link>
              ))}
            </div>
          </section>
        ))}
      </div>
    </article>
  );
}
