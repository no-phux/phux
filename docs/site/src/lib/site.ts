// Public identity lives here. Commands, package names and protocol identifiers
// are separate contracts; changing the display name does not rename them.
const name = "phux";

export const SITE = {
  name,
  domain: "phux.sh",
  url: "https://phux.sh",
  docsDomain: "docs.phux.sh",
  docsUrl: "https://docs.phux.sh",
  category: "Programmable terminal runtime",
  tagline: "Terminals you can build on.",
  description:
    `${name} keeps shells running and puts their input, output, and events behind a public API. Connect from a terminal, desktop, browser, or agent—locally or across machines.`,
  github: "https://github.com/no-phux/phux",
  // One switch for the visual system. Mode tokens live in global.css.
  designMode: "terminal",
  // Set when the WS demo backend is deployed. When empty the terminal island
  // renders the static poster + a "coming online" state instead of dialing out.
  demoWsUrl: import.meta.env.PUBLIC_PHUX_DEMO_WS ?? "",
} as const;

// Shared by the visible docs landing and its searchable Markdown representation.
export const OVERVIEW = {
  title: "Get started",
  summary: "Start locally, automate a terminal, or connect another machine.",
  paths: [
    { href: "/quickstart", title: "Quickstart", text: "Start, split, and detach." },
    { href: "/consumers/getting-started", title: "Coding agents", text: "Choose an integration and verify it." },
    { href: "/remote-access", title: "Remote access", text: "Connect hosts over SSH or QUIC." },
    { href: "/concepts/coming-from", title: "From tmux", text: "Find familiar keys and concepts." },
    { href: "/concepts/when-to-use", title: "Compare tools", text: `${SITE.name}, tmux, Herdr, and cmux.` },
    { href: "/troubleshooting", title: "Troubleshooting", text: "Diagnose installation, connection, and agent issues." },
  ],
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
