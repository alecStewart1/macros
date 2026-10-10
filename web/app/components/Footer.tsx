import Link from "next/link";
import { Logo } from "./Logo";

export function Footer() {
  return (
    <footer
      style={{
        borderTop: "1px solid var(--border)",
        background: "var(--surface)",
        marginTop: 80,
      }}
    >
      <div
        style={{
          maxWidth: 1120,
          margin: "0 auto",
          padding: "40px 24px",
          display: "flex",
          flexWrap: "wrap",
          gap: 32,
          alignItems: "flex-start",
          justifyContent: "space-between",
        }}
      >
        <div style={{ maxWidth: 300 }}>
          <Logo size={24} />
          <p
            style={{
              color: "var(--text2)",
              fontSize: 13.5,
              lineHeight: 1.6,
              marginTop: 14,
            }}
          >
            An Emacs-style, GPU-rendered editor scripted in Steel. Free software,
            licensed MIT.
          </p>
        </div>

        <FooterCol
          title="Documentation"
          links={[
            ["Overview", "/docs"],
            ["Getting started", "/docs/getting-started"],
            ["Configuration", "/docs/configuration"],
            ["Keybindings", "/docs/keybindings"],
            ["Scripting", "/docs/scripting"],
          ]}
        />
        <FooterCol
          title="Features"
          links={[
            ["Modal editing", "/docs/modal-editing"],
            ["Git (Magit)", "/docs/git"],
            ["Language servers", "/docs/lsp"],
            ["Org mode", "/docs/org-mode"],
            ["Terminal", "/docs/terminal"],
          ]}
        />
        <FooterCol
          title="Project"
          links={[
            ["Download", "/download"],
            ["Packages", "/docs/packages"],
            ["Source code", "https://github.com/macros-app/macros"],
            ["Report an issue", "https://github.com/macros-app/macros/issues"],
          ]}
        />
      </div>

      <div
        style={{
          maxWidth: 1120,
          margin: "0 auto",
          padding: "0 24px 32px",
          color: "var(--text3)",
          fontSize: 12.5,
        }}
      >
        © {new Date().getFullYear()} Macros. Built with paren­theses.
      </div>
    </footer>
  );
}

function FooterCol({
  title,
  links,
}: {
  title: string;
  links: [string, string][];
}) {
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
      <div
        style={{
          fontSize: 12,
          fontWeight: 600,
          textTransform: "uppercase",
          letterSpacing: "0.08em",
          color: "var(--text3)",
          marginBottom: 2,
        }}
      >
        {title}
      </div>
      {links.map(([label, href]) =>
        href.startsWith("http") ? (
          <a
            key={href}
            className="nav-link"
            href={href}
            target="_blank"
            rel="noreferrer"
          >
            {label}
          </a>
        ) : (
          <Link key={href} className="nav-link" href={href}>
            {label}
          </Link>
        ),
      )}
    </div>
  );
}
