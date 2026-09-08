---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-08
---

# Render layering: ratatui chrome over libghostty pane interiors

**TL;DR.** The phux TUI uses two renderers on disjoint screen regions.
libghostty paints pane interiors on the hot path so kitty graphics,
sixel, OSC 8, and the Kitty key protocol pass through unchanged.
ratatui paints the chrome (status bar, dividers, modals); the layers
composite rather than interleave. Crate splits make both boundaries
compiler-enforced: ratatui lives only in phux-tui.

---

Per [ADR-0020](../../ADR/0020-layered-render.md), the TUI uses two
renderers for disjoint screen regions. libghostty paints pane interiors
on the hot path — kitty graphics, sixel, OSC 8 hyperlinks, and the
Kitty key protocol all pass through unchanged. `ratatui` paints the
chrome: status bar, pane dividers, borders, modals, future tab bar.
The layers composite rather than interleave; chrome carves skip-cell
rectangles for pane rects so libghostty owns those cells exclusively.

The `ratatui` dependency is scoped to a single crate, `phux-tui`
(under `src/render/`, submodules `chrome` and `overlay`). The
pane-interior substrate — pane mirror, predict layer, layout math, and
multi-pane composition — lives in a separate crate, `phux-client-core`,
which carries **no `ratatui` dependency**; the headless control-plane
client — connection, transports, the agent verbs — lives in
`phux-client`, which carries none either
([ADR-0100](../../ADR/0100-the-tui-is-its-own-crate.md)). Both
boundaries are therefore enforced by the compiler: a `use ratatui` in
the substrate or in the headless library fails to build because the
crate cannot name it. This replaced the original
`scripts/check-ratatui-boundary.sh` grep guard. The attach loop lives
in `phux-tui` (it composites chrome over panes, so it legitimately
depends on the chrome, the substrate, and the headless client).
