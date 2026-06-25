#!/usr/bin/env node
//
// Generate `public/latest.json` — the static update manifest the desktop app
// polls at https://macros.dev/latest.json. There is no backend database: the
// "latest version" is whatever this build bakes in, resolved (highest priority
// first) from:
//   1. APP_VERSION env (set by deploy-web.yml's `version` input)
//   2. the repo-root Cargo.toml [package].version
//   3. "0.1.0" fallback
//
// This MUST match next.config.ts's NEXT_PUBLIC_APP_VERSION resolution so the
// downloads page and the updater never disagree.
//
// Download URLs point at the R2 distribution bucket (DOWNLOAD_BASE, default
// https://downloads.macros.dev) and use the exact artifact names that
// scripts/publish-release.mjs uploads. The updater fetches `<url>.sha256` for
// integrity — those sidecars are written by the release pipeline, not here.
//
// Runs before `next build` (see package.json), writing into `public/` so the
// file lands at the site root in the static export.

"use strict";

import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const webDir = resolve(__dirname, "..");
const repoRoot = resolve(webDir, "..");

function readCargoVersion() {
  try {
    const text = readFileSync(resolve(repoRoot, "Cargo.toml"), "utf8");
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
    // Cargo.toml unreadable outside the monorepo — fall through.
  }
  return undefined;
}

const version = process.env.APP_VERSION || readCargoVersion() || "0.1.0";

const base = (process.env.DOWNLOAD_BASE || "https://downloads.macros.dev").replace(/\/+$/, "");

// Artifact filenames — keep in lockstep with scripts/publish-release.mjs.
const downloads = {
  "macos-aarch64": `${base}/Macros-macos-aarch64-${version}.zip`,
  "macos-x86_64": `${base}/Macros-macos-x86_64-${version}.zip`,
  "windows-x86_64": `${base}/Macros-windows-x86_64-${version}.msi`,
  "linux-x86_64-deb": `${base}/macros_${version}_amd64.deb`,
  "linux-x86_64-rpm": `${base}/macros-${version}-1.x86_64.rpm`,
  "linux-aarch64-deb": `${base}/macros_${version}_arm64.deb`,
  "linux-aarch64-rpm": `${base}/macros-${version}-1.aarch64.rpm`,
};

const manifest = {
  version,
  // notes is optional; the updater shows it in the "update available" prompt.
  notes: null,
  downloads,
};

const outDir = resolve(webDir, "public");
mkdirSync(outDir, { recursive: true });
const outFile = resolve(outDir, "latest.json");
writeFileSync(outFile, JSON.stringify(manifest, null, 2) + "\n");

console.log(`gen-version: wrote ${outFile} (version ${version}, base ${base})`);
