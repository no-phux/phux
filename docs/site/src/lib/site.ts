// Single source of truth for site metadata — pages and components read from here
// so nothing drifts. Mirrors the wire/phall.io `SITE` convention.

export const SITE = {
  name: "phux",
  domain: "phux.sh",
  url: "https://phux.sh",
  docsDomain: "docs.phux.sh",
  docsUrl: "https://docs.phux.sh",
  tagline: "you and your agents share the same terminals",
  description:
    "phux is a terminal multiplexer whose panes are a view. You, Cockpit, a script, or an agent attach to the same live terminal. When a harness emits, blocked is a fact on that wire. Join another machine with no phux account.",
  github: "https://github.com/no-phux/phux",
  // One switch for the visual system. Mode tokens live in global.css.
  designMode: "terminal",
  // Set when the WS demo backend is deployed. When empty the terminal island
  // renders the static poster + a "coming online" state instead of dialing out.
  demoWsUrl: import.meta.env.PUBLIC_PHUX_DEMO_WS ?? "",
} as const;

/**
 * Absolute docs origin in production builds so marketing chrome lands on
 * docs.phux.sh instead of bouncing through a same-host 301. `astro dev`
 * stays same-origin so the local tree is clickable.
 *
 * Override with PUBLIC_DOCS_ORIGIN when previewing a split locally.
 */
export const DOCS_ORIGIN =
  import.meta.env.PUBLIC_DOCS_ORIGIN ?? (import.meta.env.PROD ? SITE.docsUrl : "");

export function docsHref(path: string): string {
  const normalized = path.startsWith("/") ? path : `/${path}`;
  return DOCS_ORIGIN ? `${DOCS_ORIGIN}${normalized}` : normalized;
}

export const MARKETING_NAV = [
  { href: docsHref("/overview"), label: "Docs" },
  { href: docsHref("/consumers"), label: "Apps" },
  { href: docsHref("/consumers/agents"), label: "Agents" },
  { href: SITE.github, label: "GitHub", external: true },
] as const;

export const DOCS_NAV = [
  { href: SITE.url, label: "Site" },
  { href: SITE.github, label: "GitHub", external: true },
] as const;

export const NAV = MARKETING_NAV;
