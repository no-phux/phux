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
  tagline: "Persistent terminals for people and agents.",
  description:
    `${name} keeps shells running and puts their input, output, and events behind a public API. Connect from a terminal, desktop, browser, or agent—locally or across machines.`,
  github: "https://github.com/no-phux/phux",
  // One switch for the visual system. Mode tokens live in global.css.
  designMode: "terminal",
  // Names the /og.png artwork. Link previews cache by URL, so change this
  // whenever src/pages/og.png.ts draws a different picture.
  previewArtwork: "split-window",
  // Set when the WS demo backend is deployed. When empty the terminal island
  // renders the static poster + a "coming online" state instead of dialing out.
  demoWsUrl: import.meta.env.PUBLIC_PHUX_DEMO_WS ?? "",
} as const;

// Shared by the visible docs landing and its searchable Markdown representation.
export const OVERVIEW = {
  title: "Get started",
  summary: "Detach without stopping your shell. Return to it, or let an agent inspect it.",
  pitch: {
    headline: "Leave the view. Keep the shell.",
    text:
      "A background server owns the terminal. The TUI, desktop, and agents connect to it.",
  },
  firstRun: [
    { label: "Start", action: "phux", detail: "Open a shell." },
    { label: "Detach", action: "Ctrl-A, then d", detail: "Leave it running." },
    { label: "Return", action: "phux", detail: "Same shell, same output." },
  ],
  paths: [
    { href: "/consumers/getting-started", title: "Coding agents", text: "Choose an integration and verify it." },
    { href: "/remote-access", title: "Remote access", text: "Connect hosts over SSH or QUIC." },
    { href: "/concepts/coming-from", title: "From tmux", text: "Find familiar keys and concepts." },
  ],
} as const;

// The visible handoff and the overview's Markdown/search representation agree.
export const AGENT_START = {
  prompt: "Read https://docs.phux.sh/llms.txt and help me get started with phux. Choose the setup for my agent host, check my installed version, and verify a read before sending input. Use exact pane targets and bounded waits. Ask before interrupting or destroying existing work.",
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
  { href: "/demos", label: "Demos" },
  { href: docsHref("/consumers"), label: "Apps" },
  { href: docsHref("/consumers/getting-started"), label: "Agents" },
  { href: SITE.github, label: "GitHub", external: true },
] as const;

export const DOCS_NAV = [
  { href: "/llms.txt", label: "Agent guide" },
  { href: SITE.url, label: "Site" },
  { href: SITE.github, label: "GitHub", external: true },
] as const;

export const NAV = MARKETING_NAV;
