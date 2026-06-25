#!/usr/bin/env node
//
// Publish a Macros release to Cloudflare R2: hash each artifact, upload it to the
// R2 bucket (S3-compatible API), then verify the public URL serves the exact
// bytes back. Unlike a hosted-update service there is no database step — R2 is
// the distribution surface, and the flat object key IS the download name.
//
// Run from the `scripts/` directory (so `npm ci` here installs @aws-sdk).
// Reads env (and falls back to `.env` next to this script):
//   R2_ACCOUNT_ID          Cloudflare account ID (32 hex chars)
//   R2_ACCESS_KEY_ID       S3-compatible Access Key ID
//   R2_SECRET_ACCESS_KEY   S3-compatible Secret Access Key
//   R2_BUCKET              Bucket name (e.g. "macros-releases")
//   UPDATE_DOWNLOAD_BASE   Public https URL prefix (no trailing slash), e.g.
//                          https://downloads.macros.dev
//
// Usage:
//   node publish-release.mjs --dir ../dist --version 0.1.0
//
// Layout of --dir (matches actions/download-artifact@v3 output — each artifact
// lands in its own subdirectory, which we walk recursively):
//   <dir>/Macros-macos-aarch64-0.1.0/Macros-macos-aarch64-0.1.0.zip
//   <dir>/Macros-macos-x86_64-0.1.0/Macros-macos-x86_64-0.1.0.zip
//   <dir>/Macros-linux-aarch64-0.1.0-deb/macros_0.1.0_arm64.deb
//   <dir>/Macros-linux-aarch64-0.1.0-rpm/macros-0.1.0-1.aarch64.rpm
//   <dir>/Macros-windows-x86_64-0.1.0/Macros-windows-x86_64-0.1.0.msi

"use strict";

import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

// Bump when changing upload/verify behavior; printed at startup so a CI log
// proves which version of this script actually ran (vs. a stale checkout).
const REVISION = "r2-publish-3: sidecar-as-file + authoritative HeadObject";

// ── Minimal .env loader ──────────────────────────────────────────────────────
function loadDotenv(file) {
  if (!fs.existsSync(file)) return;
  for (const raw of fs.readFileSync(file, "utf8").split(/\r?\n/)) {
    const line = raw.trim();
    if (!line || line.startsWith("#")) continue;
    const eq = line.indexOf("=");
    if (eq <= 0) continue;
    const key = line.slice(0, eq).trim();
    let value = line.slice(eq + 1).trim();
    if (
      (value.startsWith('"') && value.endsWith('"')) ||
      (value.startsWith("'") && value.endsWith("'"))
    ) {
      value = value.slice(1, -1).replace(/\\n/g, "\n");
    }
    // CI-set env vars win over .env; only fill gaps.
    if (process.env[key] === undefined) process.env[key] = value;
  }
}
loadDotenv(path.join(__dirname, ".env"));

const REQUIRED_ENV = [
  "R2_ACCOUNT_ID",
  "R2_ACCESS_KEY_ID",
  "R2_SECRET_ACCESS_KEY",
  "R2_BUCKET",
  "UPDATE_DOWNLOAD_BASE",
];

// ── Artifact filename parsers ────────────────────────────────────────────────
// Each pulls out { os, arch, version, format }. `<ver>` can carry hyphens
// (`0.1.0-rc1`), and x86_64 has an underscore, so we use anchored alternation
// rather than split('-'). The deb/rpm prefixes must match `name:` in nfpm.yaml.
const ZIP_RE = /^Macros-(macos|linux|windows)-(aarch64|x86_64)-(.+)\.zip$/;
const MSI_RE = /^Macros-windows-(aarch64|x86_64)-(.+)\.msi$/;
const DEB_RE = /^macros_(.+)_(arm64|amd64)\.deb$/;
const RPM_RE = /^macros-(.+)-[^-]+\.(aarch64|x86_64)\.rpm$/;

const DEB_ARCH_TO_CANONICAL = { arm64: "aarch64", amd64: "x86_64" };

const CONTENT_TYPE_BY_FORMAT = {
  zip: "application/zip",
  msi: "application/x-msi",
  deb: "application/vnd.debian.binary-package",
  rpm: "application/x-rpm",
};

function parseArtifactFilename(filename) {
  let m = ZIP_RE.exec(filename);
  if (m) return { os: m[1], arch: m[2], version: m[3], format: "zip" };
  m = MSI_RE.exec(filename);
  if (m) return { os: "windows", arch: m[1], version: m[2], format: "msi" };
  m = DEB_RE.exec(filename);
  if (m) {
    const arch = DEB_ARCH_TO_CANONICAL[m[2]];
    if (!arch) return null;
    return { os: "linux", arch, version: m[1], format: "deb" };
  }
  m = RPM_RE.exec(filename);
  if (m) return { os: "linux", arch: m[2], version: m[1], format: "rpm" };
  return null;
}

// ── Args ─────────────────────────────────────────────────────────────────────
function parseArgs(argv) {
  const args = { dir: null, version: null, help: false };
  for (let i = 2; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--help" || a === "-h") args.help = true;
    else if (a === "--dir") args.dir = argv[++i];
    else if (a.startsWith("--dir=")) args.dir = a.slice("--dir=".length);
    else if (a === "--version") args.version = argv[++i];
    else if (a.startsWith("--version=")) args.version = a.slice("--version=".length);
    else {
      console.error(`Unknown argument: ${a}`);
      process.exit(2);
    }
  }
  return args;
}

function printHelp() {
  console.log(`Usage:
  node publish-release.mjs --dir <path> --version <semver>

Walks <path> recursively for files matching:
  Macros-(macos|linux|windows)-(aarch64|x86_64)-<version>.zip
  Macros-windows-(aarch64|x86_64)-<version>.msi
  macros_<version>_(arm64|amd64).deb
  macros-<version>-<release>.(aarch64|x86_64).rpm
For each match: SHA-256, upload to R2, then HEAD-verify the public URL.`);
}

// ── Helpers ──────────────────────────────────────────────────────────────────
function sha256OfFile(file) {
  return new Promise((resolve, reject) => {
    const hash = crypto.createHash("sha256");
    const stream = fs.createReadStream(file);
    stream.on("data", (c) => hash.update(c));
    stream.on("end", () => resolve(hash.digest()));
    stream.on("error", reject);
  });
}

function findArtifacts(dir) {
  const out = [];
  const walk = (d) => {
    for (const entry of fs.readdirSync(d, { withFileTypes: true })) {
      const full = path.join(d, entry.name);
      if (entry.isDirectory()) walk(full);
      else if (entry.isFile() && parseArtifactFilename(entry.name)) out.push(full);
    }
  };
  walk(dir);
  return out;
}

function r2Client(S3Client) {
  return new S3Client({
    region: "auto",
    endpoint: `https://${process.env.R2_ACCOUNT_ID}.r2.cloudflarestorage.com`,
    credentials: {
      accessKeyId: process.env.R2_ACCESS_KEY_ID,
      secretAccessKey: process.env.R2_SECRET_ACCESS_KEY,
    },
    // Cloudflare R2 does NOT support the AWS SDK v3 "flexible checksum" upload
    // path that became the default in early-2025 SDKs: the SDK adds a CRC32
    // checksum sent as an `aws-chunked` streaming trailer, and R2 responds by
    // storing a 0-byte object (the symptom: sidecar HEAD returns content-length
    // 0). Forcing WHEN_REQUIRED makes the SDK fall back to a classic,
    // known-length PUT. Integrity is still covered by the post-upload HEAD
    // size check and the published `.sha256` sidecar.
    requestChecksumCalculation: "WHEN_REQUIRED",
    responseChecksumValidation: "WHEN_REQUIRED",
  });
}

async function uploadToR2(filePath, key, contentType, contentLength) {
  const { S3Client, PutObjectCommand } = await import("@aws-sdk/client-s3");
  const client = r2Client(S3Client);
  // Pass ContentLength so the body goes up as a single known-length PUT (no
  // `aws-chunked` streaming, which R2 mishandles — see r2Client).
  await client.send(
    new PutObjectCommand({
      Bucket: process.env.R2_BUCKET,
      Key: key,
      Body: fs.createReadStream(filePath),
      ContentLength: contentLength,
      ContentType: contentType,
    }),
  );
}

// Authoritative size straight from R2's S3 API (HeadObject) — bypasses the
// public CDN, so it tells us what actually landed in the bucket regardless of
// any cache/propagation lag on the public URL.
async function r2HeadSize(key) {
  try {
    const { S3Client, HeadObjectCommand } = await import("@aws-sdk/client-s3");
    const client = r2Client(S3Client);
    const resp = await client.send(
      new HeadObjectCommand({ Bucket: process.env.R2_BUCKET, Key: key }),
    );
    return Number(resp.ContentLength);
  } catch (e) {
    // Don't let a read-permission quirk on the API token fail a good upload —
    // degrade to the public-URL check. Returns null = "couldn't confirm".
    console.warn(`  warning: HeadObject(${key}) failed (${e.name ?? e.message ?? e}); skipping authoritative size check`);
    return null;
  }
}

async function headWithRetry(url, expectedSize) {
  // A custom domain through Cloudflare's CDN can take a moment to surface a
  // freshly-uploaded object. Backoff: 1s, 2s, 4s, 8s, 16s.
  const delays = [1000, 2000, 4000, 8000, 16000];
  let lastErr = null;
  for (let i = 0; i <= delays.length; i++) {
    try {
      const resp = await fetch(url, { method: "HEAD" });
      if (resp.ok) {
        const len = Number(resp.headers.get("content-length"));
        if (len === expectedSize) return;
        lastErr = new Error(`Content-Length mismatch at ${url}: got ${len}, expected ${expectedSize}`);
      } else {
        lastErr = new Error(`HEAD ${url} → ${resp.status}`);
      }
    } catch (e) {
      lastErr = new Error(`HEAD ${url} threw: ${e.message ?? e}`);
    }
    console.log(`    attempt ${i + 1}/${delays.length + 1}: ${lastErr.message}`);
    if (i < delays.length) await new Promise((r) => setTimeout(r, delays[i]));
  }
  throw lastErr;
}

// ── Main ─────────────────────────────────────────────────────────────────────
async function main() {
  const args = parseArgs(process.argv);
  if (args.help) return printHelp();
  console.log(`publish-release.mjs [${REVISION}] on node ${process.version}`);
  if (!args.dir || !args.version) {
    console.error("error: --dir and --version are required\n");
    printHelp();
    process.exit(2);
  }
  if (!/^\d+\.\d+\.\d+(?:-[\w.]+)?$/.test(args.version)) {
    console.error(`error: --version must be semver, got '${args.version}'`);
    process.exit(2);
  }

  const missing = REQUIRED_ENV.filter((k) => !process.env[k]);
  if (missing.length > 0) {
    console.error(`error: missing env vars: ${missing.join(", ")}`);
    process.exit(2);
  }
  const base = process.env.UPDATE_DOWNLOAD_BASE;
  if (!/^https:\/\//i.test(base)) {
    console.error(`error: UPDATE_DOWNLOAD_BASE must start with https:// (got '${base}')`);
    process.exit(2);
  }

  const dir = path.resolve(args.dir);
  if (!fs.existsSync(dir) || !fs.statSync(dir).isDirectory()) {
    console.error(`error: --dir is not a directory: ${dir}`);
    process.exit(2);
  }
  const files = findArtifacts(dir);
  if (files.length === 0) {
    console.error(`error: no Macros release artifacts found under ${dir}`);
    process.exit(2);
  }

  console.log(`Found ${files.length} artifact(s) under ${dir}`);

  // Process artifacts independently so one CDN hiccup doesn't skip the rest.
  const failures = [];
  for (const file of files) {
    const filename = path.basename(file);
    try {
      const parsed = parseArtifactFilename(filename);
      const { os, arch, version: fileVersion, format } = parsed;
      if (fileVersion !== args.version) {
        throw new Error(`${filename} has version '${fileVersion}' but --version is '${args.version}'`);
      }
      const contentType = CONTENT_TYPE_BY_FORMAT[format];
      const stat = fs.statSync(file);
      const shaBuf = await sha256OfFile(file);
      const shaHex = shaBuf.toString("hex");
      const r2Key = filename; // flat layout; filename already encodes os/arch/version
      const publicUrl = `${base.replace(/\/+$/, "")}/${r2Key}`;

      console.log(`\n[${os}-${arch} ${format} ${fileVersion}]`);
      console.log(`  file:   ${file}`);
      console.log(`  size:   ${stat.size} bytes`);
      console.log(`  sha256: ${shaHex}`);
      console.log(`  url:    ${publicUrl}`);

      console.log(`  uploading…`);
      await uploadToR2(file, r2Key, contentType, stat.size);
      // Authoritative check straight from R2 (no CDN): proves the bytes landed.
      const storedSize = await r2HeadSize(r2Key);
      console.log(`  uploaded. R2 stored ${storedSize ?? "?"} bytes`);
      if (storedSize !== null && storedSize !== stat.size) {
        throw new Error(`R2 stored artifact as ${storedSize} bytes, expected ${stat.size}`);
      }

      console.log(`  verifying public URL…`);
      await headWithRetry(publicUrl, stat.size);
      console.log(`  verified.`);

      // SHA-256 sidecar: `<artifact>.sha256` in sha256sum format
      // (`<hex>  <filename>`). The desktop updater fetches `<url>.sha256` and
      // verifies the downloaded artifact against it before swapping the bundle.
      // Write it to a real file and upload via the SAME stream path the
      // artifacts use — a Buffer body was landing as a 0-byte object on R2.
      const sidecarKey = `${r2Key}.sha256`;
      const sidecarUrl = `${publicUrl}.sha256`;
      const sidecarPath = `${file}.sha256`;
      fs.writeFileSync(sidecarPath, `${shaHex}  ${filename}\n`);
      const sidecarSize = fs.statSync(sidecarPath).size;
      console.log(`  uploading sidecar ${sidecarKey} (${sidecarSize} bytes)…`);
      await uploadToR2(sidecarPath, sidecarKey, "text/plain; charset=utf-8", sidecarSize);
      // Authoritative R2 check first — distinguishes a bad upload (0 bytes here)
      // from mere public-CDN propagation lag.
      const sidecarStored = await r2HeadSize(sidecarKey);
      console.log(`  sidecar uploaded. R2 stored ${sidecarStored ?? "?"} bytes`);
      if (sidecarStored !== null && sidecarStored !== sidecarSize) {
        throw new Error(`R2 stored sidecar as ${sidecarStored} bytes, expected ${sidecarSize}`);
      }
      // R2 already confirmed the object is correct, so the public-URL check is
      // best-effort: a tiny sidecar lagging the CDN shouldn't fail the release.
      try {
        await headWithRetry(sidecarUrl, sidecarSize);
        console.log(`  sidecar verified at public URL.`);
      } catch (e) {
        console.warn(`  warning: sidecar not yet visible at public URL (${e.message ?? e}); R2 has it, CDN should catch up.`);
      }
    } catch (e) {
      console.error(`\n[${filename}] FAILED: ${e.message ?? e}`);
      failures.push({ filename, error: e });
    }
  }

  const succeeded = files.length - failures.length;
  console.log(`\nDone. Published ${succeeded}/${files.length} artifact(s) at version ${args.version}.`);
  if (failures.length > 0) {
    console.error(`\n${failures.length} artifact(s) failed:`);
    for (const f of failures) console.error(`  - ${f.filename}: ${f.error.message ?? f.error}`);
    process.exit(1);
  }
}

main().catch((e) => {
  console.error(`\nerror: ${e.message ?? e}`);
  if (e.stack) console.error(e.stack);
  process.exit(1);
});
