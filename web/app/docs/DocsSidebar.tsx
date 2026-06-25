"use client";

import { useMemo, useState } from "react";
import Link from "next/link";
import { usePathname } from "next/navigation";
import type { NavGroup } from "@/lib/docs";

export function DocsSidebar({ groups }: { groups: NavGroup[] }) {
  const pathname = usePathname();
  const [q, setQ] = useState("");

  const filtered = useMemo(() => {
    const needle = q.trim().toLowerCase();
    if (!needle) return groups;
    return groups
      .map((g) => ({
        ...g,
        items: g.items.filter(
          (i) =>
            i.label.toLowerCase().includes(needle) ||
            i.blurb.toLowerCase().includes(needle) ||
            i.slug.includes(needle),
        ),
      }))
      .filter((g) => g.items.length > 0);
  }, [q, groups]);

  return (
    <nav>
      <input
        className="docs-search-input"
        type="search"
        placeholder="Search docs…"
        value={q}
        onChange={(e) => setQ(e.target.value)}
        style={{
          width: "100%",
          fontSize: 13.5,
          padding: "8px 12px",
          marginBottom: 22,
          borderRadius: 8,
          border: "1px solid var(--border2)",
          background: "var(--card)",
          color: "var(--text)",
          fontFamily: "'IBM Plex Sans', sans-serif",
        }}
      />

      {filtered.map((group) => (
        <div key={group.title} style={{ marginBottom: 22 }}>
          <div
            style={{
              fontSize: 11.5,
              fontWeight: 600,
              textTransform: "uppercase",
              letterSpacing: "0.07em",
              color: "var(--text3)",
              padding: "0 10px",
              marginBottom: 6,
            }}
          >
            {group.title}
          </div>
          <div style={{ display: "flex", flexDirection: "column", gap: 1 }}>
            {group.items.map((item) => {
              const href = `/docs/${item.slug}`;
              const active =
                pathname === href || pathname === `${href}/`;
              return (
                <Link
                  key={item.slug}
                  href={href}
                  className="docs-nav-link"
                  aria-current={active ? "page" : undefined}
                  style={{
                    display: "block",
                    padding: "6px 10px",
                    borderRadius: 7,
                    fontSize: 13.5,
                    textDecoration: "none",
                    color: active ? "var(--accent)" : "var(--text2)",
                    background: active ? "var(--accent-dim)" : "transparent",
                    fontWeight: active ? 600 : 400,
                  }}
                >
                  {item.label}
                </Link>
              );
            })}
          </div>
        </div>
      ))}

      {filtered.length === 0 && (
        <p style={{ color: "var(--text3)", fontSize: 13, padding: "0 10px" }}>
          No pages match “{q}”.
        </p>
      )}
    </nav>
  );
}
