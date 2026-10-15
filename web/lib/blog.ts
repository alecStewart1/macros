import fs from "node:fs";
import path from "node:path";
import matter from "gray-matter";
import { marked } from "marked";

/* ── Blog posts ────────────────────────────────────────────────────────────
   One markdown file per post in content/blog/. The slug is the filename
   (without .md) and the URL is /blog/<slug>. Front matter carries the title,
   a one-line description for the index and <meta>, and an ISO date that
   orders the index newest first. */

export const BLOG_DIR = path.join(process.cwd(), "content", "blog");

export type PostMeta = {
  slug: string;
  title: string;
  description: string;
  date: string;
};

export type Post = PostMeta & { html: string };

function readPost(slug: string): { meta: PostMeta; content: string } | null {
  const file = path.join(BLOG_DIR, `${slug}.md`);
  if (!fs.existsSync(file)) return null;
  const { data, content } = matter(fs.readFileSync(file, "utf8"));
  // gray-matter parses an unquoted YAML date into a Date.
  const date = data.date instanceof Date ? data.date.toISOString().slice(0, 10) : String(data.date ?? "");
  return {
    meta: {
      slug,
      title: (data.title as string) || slug,
      description: (data.description as string) || "",
      date,
    },
    content,
  };
}

/** Every post, newest first. */
export function allPosts(): PostMeta[] {
  if (!fs.existsSync(BLOG_DIR)) return [];
  return fs
    .readdirSync(BLOG_DIR)
    .filter((f) => f.endsWith(".md"))
    .map((f) => readPost(f.slice(0, -3))?.meta)
    .filter((p): p is PostMeta => !!p)
    .sort((a, b) => b.date.localeCompare(a.date));
}

export function getPost(slug: string): Post | null {
  const p = readPost(slug);
  if (!p) return null;
  const html = marked.parse(p.content, { gfm: true, async: false }) as string;
  return { ...p.meta, html };
}

/** "2026-10-15" → "October 15, 2026", without the reader's time zone shifting the day. */
export function formatDate(iso: string): string {
  const d = new Date(`${iso}T00:00:00Z`);
  if (Number.isNaN(d.getTime())) return iso;
  return d.toLocaleDateString("en-US", { year: "numeric", month: "long", day: "numeric", timeZone: "UTC" });
}
