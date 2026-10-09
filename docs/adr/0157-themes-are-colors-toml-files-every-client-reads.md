---
audience: contributors
stability: stable
last-reviewed: 2026-10-09
---

# 0157 — Themes are `colors.toml` files every client reads

**TL;DR.** A phux theme is a directory holding an Omarchy-schema
`colors.toml`: one `mode` and about thirty named colours. Themes arrive
through plugins or `phux theme install <git-url>` into one catalog under
the XDG data dir, are chosen by name in `config.toml`, and every client
(TUI, Cockpit, mobile) maps the same semantic keys onto its own chrome and
onto the ANSI-16 palette. The wire does not change.

Status: Proposed
Date: 2026-10-09

## Context

Three clients colour themselves three different ways. The TUI has a
free-form `[theme]` slot map (ADR-0026) with 23 named slots. Cockpit has
`theme = <name>` over six hardcoded presets that set only background,
foreground and selection, plus a lower-precedence layer adopted from the
user's Ghostty config. Mobile is dark-only with one hardcoded palette per
platform. None of them can install a theme, and a theme written for one
is unusable on the others.

Omarchy already solved the authoring side for the desktop this project
is developed on: a theme is a git repo with a semantic `colors.toml`
(`background`, `foreground`, `accent`, `selection`, `muted`, the eight
ANSI colours, their bright variants, derived shades, `mode = dark|light`),
installed by URL, selected by name, with per-app files generated from
templates. Twenty-two such themes ship with Omarchy and more exist as
community repos.

Plugins (`phux-plugin.toml`, ADR-0041) already give phux an install,
lock, enable and disable path from a git URL. The manifest has no theme
field.

## Decision

1. **Format.** A theme is a directory containing `colors.toml` in the
   Omarchy schema. phux reads the keys it needs and applies Omarchy's
   fallback rules for derived shades; unknown keys are ignored. `mode`
   is required. Nothing in a theme is executed.
2. **Catalog.** A name resolves first against
   `$XDG_DATA_HOME/phux/themes/<name>/`, where `phux theme install
   <git-url>` clones a theme repo (name derived from the URL as Omarchy
   does), then against the `[[themes]]` entries (`name`, `path`) of
   enabled plugins. `phux theme list|show|set|remove` manage it, and
   `phux theme show --json` is how a client that does not parse plugin
   manifests reads a resolved theme.
3. **Selection.** `[theme]` gains two reserved keys. `name = "<catalog
   name>"` selects an installed theme; `file = "<path>"` points at any
   `colors.toml` directly, so `file =
   "~/.local/state/omarchy/current/theme/colors.toml"` follows the
   Omarchy theme switcher with no generated files. Explicit slot keys
   still win over the selected theme (ADR-0026 layering holds).
   `phux theme set` edits the one key through the settings writer
   (ADR-0101) and triggers `config reload`.
4. **Terminal palette mapping is fixed** and identical in every client,
   taken from Omarchy's Ghostty template: palette 0 = `background`,
   1..6 = `red` `green` `yellow` `blue` `magenta` `cyan`, 7 =
   `foreground`, 8 = `muted`, 9..14 = the bright variants, 15 =
   `bright_foreground`; cursor = `bright_foreground`; selection =
   `selection`. The client runtime gains a palette override that
   replaces an ANSI-16 entry only while the application has not set it
   with OSC 4, so apps keep winning and OSC 104 reverts to the theme.
   It is exposed through the C ABI and UniFFI once, so Cockpit's remote
   panes and mobile share the one implementation.
5. **Chrome mapping is per client** and documented in each client's
   design doc. The TUI maps the semantic keys onto its 23 slots in
   `render/theme.rs`. Cockpit's settings surface keeps its fixed
   colours (Cockpit `DECISIONS.md`). Mobile keeps a bundled default and
   treats imported themes as a per-device catalog synced through
   account prefs (mobile ADR-0017), because it has no local server data
   dir to read.
6. **The server is uninvolved** except that a client advertises the
   selected theme's foreground and background in HELLO
   `default_colors`, which already exists, so OSC 10/11 answer truthfully.

## Why

- **Reuse the authoring ecosystem.** Every Omarchy theme works on phux
  the day this ships; theme authors learn nothing new and publish one
  repo for both.
- **Reuse the install path.** Plugins already fetch, lock, enable and
  disable from git. Themes ride that, and a theme-only repo needs no
  manifest.
- **One mapping, three clients.** Fixing the palette mapping centrally
  is what makes a theme look like the same theme on a Mac, a phone and
  an SSH session.
- **Data, not code.** Omarchy had to deny-list `ghostty.conf` and
  `.lua` from installed themes because they run code. A theme that is
  only `colors.toml` has no such surface.

## Tradeoffs

- phux does not generate per-app files, so a phux theme does not theme
  the user's Ghostty, Neovim or Waybar. Omarchy does that already on
  the desktop this targets.
- Semantic keys were chosen for a desktop, so some TUI slots
  (`agent_working`, `attention`) map onto colours whose names do not
  say what they are for. The mapping table carries that judgement.
- Mobile cannot read the catalog; its theme list is a separate set
  kept in sync by hand or by importing the same URLs.
- The WCAG floor the TUI pins for its defaults cannot be promised for
  an arbitrary theme. `phux theme show` reports contrast; it does not
  refuse.

## Alternatives

- **A phux-native slot format.** Clear ownership, but every theme would
  need to be written twice and the twenty-two existing themes would
  need porting. Rejected.
- **Ghostty theme files as the format.** Cockpit already reads them.
  They carry only the palette plus foreground and background, so chrome
  would still be unthemed and `mode` would be guessed. Rejected.
- **Server-distributed themes via L3 metadata.** Would let every device
  follow one theme automatically. Appearance is per device, the wire
  would carry data no server logic reads, and mobile would still need a
  local store for offline. Rejected.
- **Template generation like Omarchy.** Needed when the consumers are
  third-party apps with their own config syntax. phux's clients are
  ours and can read `colors.toml` natively. Rejected.
