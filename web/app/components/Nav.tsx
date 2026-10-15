import Link from "next/link";
import { Logo } from "./Logo";

const REPO = "https://github.com/macros-app/macros";

export function Nav() {
  return (
    <header className="topnav">
      <div
        style={{
          maxWidth: 1120,
          margin: "0 auto",
          padding: "13px 24px",
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          gap: 18,
        }}
      >
        <Logo />

        <nav
          style={{
            display: "flex",
            alignItems: "center",
            gap: 22,
          }}
        >
          <Link className="nav-link" href="/docs">
            Docs
          </Link>
          <Link className="nav-link nav-hide-sm" href="/docs/getting-started">
            Get started
          </Link>
          <Link className="nav-link" href="/blog">
            Blog
          </Link>
          <a className="nav-link nav-hide-sm" href={REPO} target="_blank" rel="noreferrer">
            GitHub
          </a>
          <Link className="nav-pill nav-pill-dark" href="/download">
            Download
          </Link>
        </nav>
      </div>
    </header>
  );
}
