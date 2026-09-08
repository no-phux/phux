# FUMADOCS.md — docs shell: Fumadocs-on-Astro decision record

> Decision, recorded 2026-08-05. The docs were a hand-rolled Astro shell
> (`DocsLayout.astro` + `src/lib/docs.ts` + seven per-section routers). They
> worked, but had no search, no page TOC, and no batteries. This file records
> what we chose and why; the code should match it. If they diverge, this file
> wins (or this file is wrong — fix it here first).

## The decision

**Keep Astro. Adopt Fumadocs *on top of it*, for the docs pages only.**

- Astro stays the whole-site framework. No Next.js, no replatform. The landing
  (`/`) and `/embed` keep their zero-JS Astro rendering, and the live
  terminal island (`<PhuxTerminal>`) is untouched.
- Fumadocs replaces the hand-rolled docs chrome — sidebar, breadcrumbs,
  prev/next, "view source" — with its own `DocsLayout`/`DocsPage`, and adds the
  three things we were missing: **full-text search**, **per-page TOC**, and the
  Fumadocs markdown component kit (callouts, tabs, generated anchors, Shiki
  code via `rehypeCode`).
- The content pipeline stays ours. `scripts/sync-docs.ts` remains the single
  source of truth that pulls from the enclosing phux repository (`../docs` +
  `../../ADR`) into
  `src/content/docs/_synced/`; Fumadocs consumes that as an Astro content
  collection through a small `src/lib/source.ts` adapter.
- The deploy stays **fully static** (Cloudflare Pages, `bun run build`, no
  adapter). Search runs client-side off a pre-built index emitted at build time.

## Why Fumadocs, not a framework swap

Fumadocs is framework-agnostic and ships an official Astro path (React islands
rendering `fumadocs-ui`). We already have Astro + React islands + Tailwind 4, so
the integration is additive: install `fumadocs-core` + `fumadocs-ui`, adapt
content collections, theme it. The only real cost is that the docs chrome
becomes a client-hydrated React island where it used to be server-rendered
HTML — an acceptable trade for search + TOC, and the content body itself stays
server-rendered Astro output.

We are **not** adopting Fumadocs' default SaaS look. The theme is overridden to
the phux palette (see "Themeing" below).

## What stays untouched

- `src/pages/index.astro` (landing), `src/pages/embed.astro`
- `src/components/PhuxTerminal.tsx` + `src/components/terminal/*` + `src/lib/phux-web/*`
- `worker/**`, `edge/**`, the `/embed` status handshake with phall.io
  (`PhuxDeck.astro`), and the `wss` demo URL config (`PUBLIC_PHUX_DEMO_WS`)
- URLs: `/docs`, `/quickstart`, `/quickstart/install`, `/wire/l1`, ...,
  `/decisions/adr-0017`, etc. all keep their exact paths.

## What changes

### 1. Content layout (`scripts/sync-docs.ts` + `src/content.config.ts`)

Fumadocs wants file-system-shaped folders with `folder/index.md` and a
`meta.json` per folder. The sync script is extended so each section README that
also has children writes `prefix/index.md` instead of `prefix.md`, and each
section folder gets a `meta.json` (`title` + explicit `pages` order derived from
the existing discovery/nav-order logic). The glob loader's `generateId`
normalizes `prefix/index` → `prefix`, so slugs and URLs are **unchanged**.

### 2. Source adapter (`src/lib/source.ts`)

`loader({ source: { files } })` from `fumadocs-core/source`, fed from the `docs`
+ `meta` collections. Page paths come straight from the (normalized) entry ids,
so the page tree matches the URL tree exactly.

### 3. Routing

The seven per-section routers (`docs/index.astro`, `concepts/[...slug].astro`,
`wire/[...slug].astro`, …) collapse into **one** catch-all
`src/pages/[...slug].astro` that renders `Fumadocs Docs` island. Static pages
(`/`, `/embed`) take precedence over the catch-all; unknown slugs 404.

### 4. Search

Static client (`fumadocs-core/search/client/orama-static` /
`staticClient`), which fetches `/api/search`. An `astro:build:done` hook in
`astro.config.mjs` builds the index from the synced docs with
`createFromSource(source).export()` and writes `dist/api/search.json` — pure
static, no server endpoint, no adapter.

### 5. Themeing

Fumadocs UI is themed via its `--fd-*` CSS variables. We override the `.dark`
set with near-black surfaces, quiet neutral rules, `#9d8bff` flux-violet
primary, and `#5cd6d6` cyan signal. Product mode uses proportional UI/prose
type and reserves monospace for technical data. Dark-only, theme switch
disabled, `color-scheme: dark`.

### 6. Housekeeping

`src/lib/docs.ts` and `src/layouts/DocsLayout.astro` are deleted (their job is
Fumadocs' now). Nav/footer copy moves into the Fumadocs chrome. `CONTENT.md`
and `DEPLOY.md` notes that reference the old layout stay accurate (URLs don't
change).

## Risks / trade-offs

- Astro is bumped to v7 (Fumadocs' Astro path tracks it). The React islands and
  Tailwind 4 must still build — verified by `astro check` + `astro build` before
  merge.
- The docs shell hydrates on the client (a React island) where it was static
  HTML before. Content itself remains server-rendered.
- Fumadocs moves fast (the search engine flipped from Orama to ZBSearch under
  the same client API). We pin to installed versions and adapt.
- `_synced/` stays gitignored + regenerated; `meta.json` files are generated by
  `sync-docs.ts`, never hand-edited.

## Future (not now)

- Per-page OG images (`[...slug]/image.webp.ts`) once the site stops relying on
  the single `public/og.png`.
- Tag filters on search, i18n, Layout Tabs if a second doc tree ever appears.
