// Single source of truth for site metadata — pages and components read from here
// so nothing drifts. Mirrors the wire/phall.io `SITE` convention.

export const SITE = {
  name: "phux",
  domain: "phux.sh",
  url: "https://phux.sh",
  tagline: "share real terminals with your agents",
  description:
    "phux makes every terminal an object on a wire. Humans, GUIs, and agents can observe or drive the same real terminal while the libghostty-backed stream passes through untouched.",
  github: "https://github.com/phall1/phux",
  // One switch for the visual system. Mode tokens live in global.css.
  designMode: "terminal",
  // Set when the WS demo backend is deployed. When empty the terminal island
  // renders the static poster + a "coming online" state instead of dialing out.
  demoWsUrl: import.meta.env.PUBLIC_PHUX_DEMO_WS ?? "",
} as const;

export const NAV = [
  { href: "/docs", label: "docs" },
  { href: "/quickstart", label: "quickstart" },
  { href: "/consumers/agents", label: "agents" },
  { href: "/wire", label: "protocol" },
  { href: SITE.github, label: "github", external: true },
] as const;
