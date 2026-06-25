# macros-web

The static marketing + documentation site for **Macros**, served from
[macros.dev](https://macros.dev). Built with Next.js (App Router, static
export), TypeScript, and Tailwind v4. Package manager is **bun**. Hosted on
Cloudflare as static assets.

## Develop

```sh
bun install
bun run dev        # http://localhost:3000
```

## Build

```sh
bun run build      # static export → ./out
```

`next.config.ts` sets `output: "export"`, so `bun run build` emits a fully
static `out/` directory. The app version shown on the downloads page is read
at build time from the repo-root `Cargo.toml` (`[package].version`), or
overridden with the `APP_VERSION` env var.

## Deploy (Cloudflare)

Deploys are manual via the **Deploy Web (Cloudflare)** workflow
(`.forgejo/workflows/deploy-web.yml`). It builds `out/` and uploads it with
Wrangler. Required repo secrets:

- `CLOUDFLARE_API_TOKEN`
- `CLOUDFLARE_ACCOUNT_ID`

`wrangler.jsonc` is assets-only (no Worker entrypoint) — `assets.directory`
points at `./out`. To deploy locally:

```sh
bun run build
bunx wrangler deploy            # or: bunx wrangler deploy --dry-run
```

## Content

### Documentation

Docs are markdown files in `content/docs/<slug>.md`, rendered by
`lib/docs.ts`. To add a page:

1. Create `content/docs/<slug>.md` with `title` / `description` frontmatter.
2. Add a `NavItem` to the right group in `NAV_GROUPS` in `lib/docs.ts`.

The side rail, the `/docs` overview, prev/next paging, and the static routes
all read from `NAV_GROUPS`. `##`/`###` headings get anchor links and feed the
"On this page" table of contents automatically.

### Screenshots

Drop images into `public/screenshots/`. The landing hero looks for
`public/screenshots/editor.png`; until a file exists it shows a styled
placeholder. Reference other images from markdown or components by their
`/screenshots/...` path.

### Brand assets

Logo, wordmark, and icons live in `public/brand/` (copied from the repo's
`assets/brand/`).
