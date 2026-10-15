import type { Metadata } from "next";
import Link from "next/link";
import { Nav } from "../components/Nav";
import { Footer } from "../components/Footer";
import { DocStrip } from "../docs/DocStrip";
import { allPosts, formatDate } from "@/lib/blog";

export const metadata: Metadata = {
  title: "Blog",
  description: "Notes on building with Macros: packages, scripting, and the tools around the editor.",
};

export default function BlogIndex() {
  const posts = allPosts();
  return (
    <>
      <Nav />
      <main style={{ maxWidth: 820, margin: "0 auto", padding: "44px 24px 72px" }}>
        <DocStrip seed="blog" cells={56} />
        <div className="doc-eyebrow">Blog</div>
        <h1
          style={{
            fontSize: "clamp(34px, 5vw, 48px)",
            fontWeight: 700,
            letterSpacing: "-1.4px",
            lineHeight: 1.05,
            margin: "0 0 16px",
          }}
        >
          Notes from the editor
        </h1>
        <p style={{ fontSize: 17, lineHeight: 1.65, color: "var(--text2)", maxWidth: 640, margin: "0 0 40px" }}>
          How we build with Macros: packages, scripting, and the tools around it.
        </p>

        <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
          {posts.map((p) => (
            <Link
              key={p.slug}
              href={`/blog/${p.slug}`}
              className="feature-card"
              style={{ display: "block", padding: "18px 20px", textDecoration: "none", color: "inherit" }}
            >
              <div style={{ fontSize: 12.5, color: "var(--text3)", marginBottom: 6 }}>{formatDate(p.date)}</div>
              <div style={{ fontSize: 18, fontWeight: 600, marginBottom: 6 }}>{p.title}</div>
              <div style={{ fontSize: 14, color: "var(--text2)", lineHeight: 1.55 }}>{p.description}</div>
            </Link>
          ))}
        </div>
      </main>
      <Footer />
    </>
  );
}
