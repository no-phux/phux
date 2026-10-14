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
  // Set when the WS demo backend is deployed. When empty the terminal island
  // renders the static poster + a "coming online" state instead of dialing out.
  demoWsUrl: import.meta.env.PUBLIC_PHUX_DEMO_WS ?? "",
} as const;

// Shared by the visible docs landing and its searchable Markdown representation.
export const OVERVIEW = {
  title: "Get started",
  summary: "A quick map of servers, sessions, resources, and views. Then start a terminal and come back to it.",
  pitch: {
    headline: "Your work stays on the server",
    text:
      "phux runs a background server that owns your work. A terminal keeps running when you detach; the TUI, desktop, browser, and agents can attach to it again. Start with the map below, then ",
    linkLabel: "run the quickstart",
    linkHref: "/quickstart",
  },
  terms: [
    { name: "Server", text: "The background process that owns your work." },
    { name: "Session", text: "A named group of resources on one server. Attach to it to pick up where you left off." },
    { name: "Resource", text: "A terminal running a shell, editor, or agent; an agent conversation can also be a resource. Each belongs to one session." },
    { name: "View", text: "How a client shows a resource: a pane in the TUI or a tile in an app." },
    { name: "Arrangement", text: "A session's tabs and splits. It places views of resources; it does not own the resources." },
  ],
  paths: [
    { href: "/quickstart", title: "Quickstart", text: "Start, split, and detach." },
    { href: "/consumers/getting-started", title: "Coding agents", text: "Choose an integration and verify it." },
    { href: "/remote-access", title: "Remote access", text: "Connect hosts over SSH or QUIC." },
    { href: "/concepts/coming-from", title: "From tmux", text: "Find familiar keys and concepts." },
    { href: "/concepts/when-to-use", title: "Compare tools", text: `${SITE.name}, tmux, Herdr, and cmux.` },
    { href: "/troubleshooting", title: "Troubleshooting", text: "Diagnose installation, connection, and agent issues." },
  ],
} as const;

// The visible handoff and the overview's Markdown/search representation agree.
export const AGENT_START = {
  prompt: "Read https://docs.phux.sh/llms.txt and help me get started with phux. Choose the setup for my agent host, check my installed version, and verify a read before sending input. Use exact pane targets and bounded waits. Ask before interrupting or destroying existing work.",
  skills: "npx skills add no-phux/skills",
  mcp: "claude mcp add --transport http phux-docs https://phux.sh/mcp",
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
