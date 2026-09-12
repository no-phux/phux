// Single source of truth for site metadata — pages and components read from here
// so nothing drifts. Mirrors the wire/phall.io `SITE` convention.

export const SITE = {
  name: "phux",
  domain: "phux.sh",
  url: "https://phux.sh",
  tagline: "you and your agents share the same terminals",
  description:
    "phux is a terminal multiplexer whose panes are a view. Each terminal is an addressable object on a wire: you, Cockpit, a script, or an agent attach to the same live emulator.",
  github: "https://github.com/no-phux/phux",
  // One switch for the visual system. Mode tokens live in global.css.
  designMode: "terminal",
  // Set when the WS demo backend is deployed. When empty the terminal island
  // renders the static poster + a "coming online" state instead of dialing out.
  demoWsUrl: import.meta.env.PUBLIC_PHUX_DEMO_WS ?? "",
} as const;

export const NAV = [
  { href: "/docs", label: "docs" },
  { href: "/quickstart", label: "quickstart" },
  { href: "/concepts", label: "concepts" },
  { href: "/consumers/agents", label: "agents" },
  { href: "/wire", label: "protocol" },
  { href: SITE.github, label: "github", external: true },
] as const;
