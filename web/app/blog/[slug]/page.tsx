import type { Metadata } from "next";
import Link from "next/link";
import { notFound } from "next/navigation";
import { Nav } from "../../components/Nav";
import { Footer } from "../../components/Footer";
import { DocStrip } from "../../docs/DocStrip";
import { allPosts, formatDate, getPost } from "@/lib/blog";

export const dynamicParams = false;

export function generateStaticParams() {
  return allPosts().map((p) => ({ slug: p.slug }));
}

export async function generateMetadata({
  params,
}: {
  params: Promise<{ slug: string }>;
}): Promise<Metadata> {
  const { slug } = await params;
  const post = getPost(slug);
  if (!post) return { title: "Not found" };
  return { title: post.title, description: post.description };
}

export default async function BlogPost({
  params,
}: {
  params: Promise<{ slug: string }>;
}) {
  const { slug } = await params;
  const post = getPost(slug);
  if (!post) notFound();

  return (
    <>
      <Nav />
      <article className="doc-article" style={{ maxWidth: 820, margin: "0 auto", padding: "44px 24px 72px" }}>
        <DocStrip seed={slug} />
        <div className="doc-eyebrow">
          <Link href="/blog" style={{ color: "inherit", textDecoration: "none" }}>
            Blog
          </Link>{" "}
          · {formatDate(post.date)}
        </div>
        <div className="prose" dangerouslySetInnerHTML={{ __html: post.html }} />
      </article>
      <Footer />
    </>
  );
}
