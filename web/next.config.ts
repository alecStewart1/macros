import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import type { NextConfig } from "next";

// App version source of truth, in priority order:
//   1. `APP_VERSION` env (set by deploy-web.yml's `version` input) — lets a
//      manual web deploy ship a specific version on the downloads page.
//   2. the `[package].version` in the repo-root Cargo.toml.
// Read at build time and inlined as `process.env.NEXT_PUBLIC_APP_VERSION`, so
// the downloads page reflects the version on every deploy with no runtime
// lookup (this is a fully static site).
function readCargoVersion(): string | undefined {
  try {
    // Build cwd is the `web/` dir, so Cargo.toml is one level up.
    const text = readFileSync(resolve(process.cwd(), "..", "Cargo.toml"), "utf8");
    let inPackage = false;
    for (const raw of text.split("\n")) {
      const line = raw.trim();
      if (line.startsWith("[")) {
        inPackage = line === "[package]";
        continue;
      }
      if (inPackage) {
        const m = line.match(/^version\s*=\s*"([^"]+)"/);
        if (m) return m[1];
      }
    }
  } catch {
    // Cargo.toml unreadable (unexpected outside the monorepo) — fall through.
  }
  return undefined;
}

const nextConfig: NextConfig = {
  // Pure static export: `next build` emits an `out/` directory of HTML/CSS/JS
  // that Cloudflare serves directly (no Worker runtime, no SSR). The deploy
  // workflow uploads `out/` as Worker static assets.
  output: "export",
  trailingSlash: true,
  images: {
    // next/image's optimizer needs a server; static export has none.
    unoptimized: true,
  },
  env: {
    NEXT_PUBLIC_APP_VERSION:
      process.env.APP_VERSION || readCargoVersion() || "0.1.0",
  },
};

export default nextConfig;
