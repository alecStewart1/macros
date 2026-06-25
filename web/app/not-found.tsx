import Link from "next/link";
import { Nav } from "./components/Nav";
import { Footer } from "./components/Footer";
import { Mark } from "./components/Logo";

export default function NotFound() {
  return (
    <>
      <Nav />
      <main
        style={{
          maxWidth: 640,
          margin: "0 auto",
          padding: "120px 24px",
          textAlign: "center",
        }}
      >
        <div style={{ display: "flex", justifyContent: "center", marginBottom: 20 }}>
          <Mark size={64} />
        </div>
        <h1 className="mono" style={{ fontSize: 48, fontWeight: 700, margin: "0 0 8px" }}>
          (404)
        </h1>
        <p style={{ fontSize: 17, color: "var(--text2)", margin: "0 0 28px" }}>
          That page is unbound. Try the docs or head home.
        </p>
        <div style={{ display: "flex", gap: 12, justifyContent: "center" }}>
          <Link className="btn-accent" href="/">
            Home
          </Link>
          <Link className="btn-ghost" href="/docs">
            Documentation
          </Link>
        </div>
      </main>
      <Footer />
    </>
  );
}
