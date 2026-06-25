import { Nav } from "../components/Nav";
import { Footer } from "../components/Footer";
import { DocsSidebar } from "./DocsSidebar";
import { NAV_GROUPS } from "@/lib/docs";

export default function DocsLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  return (
    <>
      <Nav />
      <div
        className="docs-shell"
        style={{
          maxWidth: 1280,
          margin: "0 auto",
          display: "flex",
          alignItems: "flex-start",
        }}
      >
        <aside
          className="docs-rail"
          style={{
            position: "sticky",
            top: 57,
            alignSelf: "flex-start",
            width: 264,
            flexShrink: 0,
            height: "calc(100vh - 57px)",
            overflowY: "auto",
            padding: "28px 16px 40px",
            borderRight: "1px solid var(--border)",
          }}
        >
          <DocsSidebar groups={NAV_GROUPS} />
        </aside>

        <div style={{ flex: 1, minWidth: 0 }}>{children}</div>
      </div>
      <Footer />
    </>
  );
}
