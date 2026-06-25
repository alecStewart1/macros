import type { Metadata } from "next";
import Link from "next/link";
import { NAV_GROUPS } from "@/lib/docs";

export const metadata: Metadata = {
  title: "Documentation",
  description:
    "Documentation for Macros — install, configure, and master the editor's features, commands, and scripting API.",
};

export default function DocsOverview() {
  return (
    <article style={{ padding: "44px 40px 24px", maxWidth: 880 }}>
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
        Documentation
      </div>
      <h1
        style={{
          fontSize: 38,
          fontWeight: 700,
          letterSpacing: "-0.8px",
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
        <Link href="/docs/getting-started" style={{ color: "var(--accent)" }}>
          Getting started
        </Link>
        , then open the in-editor tutorial with <kbd>C-h t</kbd>.
      </p>

      <div style={{ marginTop: 40, display: "flex", flexDirection: "column", gap: 38 }}>
        {NAV_GROUPS.map((group) => (
          <section key={group.title}>
            <h2
              style={{
                fontSize: 14,
                fontWeight: 600,
                textTransform: "uppercase",
                letterSpacing: "0.07em",
                color: "var(--text3)",
                margin: "0 0 14px",
              }}
            >
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
