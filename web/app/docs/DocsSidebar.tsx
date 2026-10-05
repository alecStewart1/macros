"use client";

import { useEffect, useMemo, useState } from "react";
import Link from "next/link";
import { usePathname } from "next/navigation";
import type { NavGroup } from "@/lib/docs";

export function DocsSidebar({ groups }: { groups: NavGroup[] }) {
  const pathname = usePathname();
  const [q, setQ] = useState("");
  // Phones only (CSS hides the toggle on wider screens): the page list starts
  // collapsed so the article is on the first screen. Searching always shows it.
  const [open, setOpen] = useState(false);
  // The docs layout (and this sidebar) stays mounted across client
  // navigations, so collapse the menu once a page link is followed.
  useEffect(() => setOpen(false), [pathname]);
  const current = groups.flatMap((g) => g.items).find(
    (i) => pathname === `/docs/${i.slug}` || pathname === `/docs/${i.slug}/`,
  );

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
          borderRadius: 8,
          border: "1px solid var(--border2)",
          background: "var(--surface)",
          color: "var(--text)",
          fontFamily: "'IBM Plex Sans', sans-serif",
        }}
      />

      <button
        type="button"
        className="docs-menu-toggle"
        aria-expanded={open}
        aria-controls="docs-nav-groups"
        onClick={() => setOpen((o) => !o)}
      >
        <span>{current ? current.label : "All pages"}</span>
        <span aria-hidden="true">{open ? "▴" : "▾"}</span>
      </button>

      <div
        id="docs-nav-groups"
        className="docs-nav-groups"
        data-open={open || q.trim() !== "" ? "true" : "false"}
        style={{ marginTop: 18 }}
      >
      {filtered.map((group) => (
        <div key={group.title} style={{ marginBottom: 22 }}>
          <div className="docs-group-title">
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
                >
                  {item.label}
                </Link>
              );
            })}
          </div>
        </div>
      ))}

      </div>

      {filtered.length === 0 && (
        <p style={{ color: "var(--text3)", fontSize: 13, padding: "0 10px" }}>
          No pages match “{q}”.
        </p>
      )}
    </nav>
  );
}
