import type { Metadata } from "next";
import Link from "next/link";
import { notFound } from "next/navigation";
import {
  allDocSlugs,
  flatNav,
  getDoc,
  navItem,
  NAV_GROUPS,
} from "@/lib/docs";
import { DocStrip } from "../DocStrip";

export const dynamicParams = false;

export function generateStaticParams() {
  return allDocSlugs().map((slug) => ({ slug }));
}

export async function generateMetadata({
  params,
}: {
  params: Promise<{ slug: string }>;
}): Promise<Metadata> {
  const { slug } = await params;
  const doc = getDoc(slug);
  if (!doc) return { title: "Not found" };
  return { title: doc.title, description: doc.description };
}

export default async function DocPage({
  params,
}: {
  params: Promise<{ slug: string }>;
}) {
  const { slug } = await params;
  const doc = getDoc(slug);
  if (!doc) notFound();

  // Prev / next paging follows nav order.
  const flat = flatNav();
  const idx = flat.findIndex((i) => i.slug === slug);
  const prev = idx > 0 ? flat[idx - 1] : undefined;
  const next = idx >= 0 && idx < flat.length - 1 ? flat[idx + 1] : undefined;
  const item = navItem(slug);

  return (
    <div style={{ display: "flex", alignItems: "flex-start" }}>
      <article className="doc-article" style={{ flex: 1, minWidth: 0, padding: "44px 40px 24px", maxWidth: 820 }}>
        <DocStrip seed={slug} />
        {item && <div className="doc-eyebrow">{groupTitleFor(slug)}</div>}

        <div className="prose" dangerouslySetInnerHTML={{ __html: doc.html }} />

        <nav
          className="doc-pager"
          style={{
            display: "flex",
            justifyContent: "space-between",
            gap: 16,
            marginTop: 56,
            paddingTop: 24,
            borderTop: "1px solid var(--border)",
          }}
        >
          {prev ? (
            <Link href={`/docs/${prev.slug}`} style={pagerStyle}>
              <span style={pagerLabel}>← Previous</span>
              <span style={pagerTitle}>{prev.label}</span>
            </Link>
          ) : (
            <span />
          )}
          {next ? (
            <Link
              href={`/docs/${next.slug}`}
              style={{ ...pagerStyle, textAlign: "right" }}
            >
              <span style={pagerLabel}>Next →</span>
              <span style={pagerTitle}>{next.label}</span>
            </Link>
          ) : (
            <span />
          )}
        </nav>
      </article>

      {doc.toc.length > 0 && (
        <aside
          className="docs-toc"
          style={{
            position: "sticky",
            top: 57,
            alignSelf: "flex-start",
            width: 210,
            flexShrink: 0,
            maxHeight: "calc(100vh - 57px)",
            overflowY: "auto",
            padding: "52px 20px 40px 4px",
          }}
        >
          <div className="docs-group-title" style={{ padding: 0, marginBottom: 12 }}>
            On this page
          </div>
          <div style={{ display: "flex", flexDirection: "column", gap: 7 }}>
            {doc.toc.map((t) => (
              <a
                key={t.id}
                href={`#${t.id}`}
                className="nav-link"
                style={{
                  fontSize: 12.5,
                  paddingLeft: t.depth === 3 ? 12 : 0,
                  lineHeight: 1.4,
                }}
              >
                {t.text}
              </a>
            ))}
          </div>
        </aside>
      )}
    </div>
  );
}

const pagerStyle: React.CSSProperties = {
  display: "flex",
  flexDirection: "column",
  gap: 4,
  padding: "14px 18px",
  border: "1px solid var(--border)",
  borderRadius: 10,
  background: "var(--surface)",
  textDecoration: "none",
  color: "inherit",
  flex: "0 1 auto",
  maxWidth: "48%",
};
const pagerLabel: React.CSSProperties = {
  fontSize: 12,
  color: "var(--text3)",
  fontWeight: 500,
};
const pagerTitle: React.CSSProperties = {
  fontSize: 14.5,
  fontWeight: 600,
  color: "var(--text)",
};

function groupTitleFor(slug: string): string {
  for (const g of NAV_GROUPS) {
    if (g.items.some((i) => i.slug === slug)) return g.title;
  }
  return "Documentation";
}
