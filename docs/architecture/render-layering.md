---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-12
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

## Pane interiors are cell-diffed

The pane painter (`attach/render.rs`) visits the rows libghostty reports
dirty, but it does not rewrite them whole. Each pane keeps a front
buffer: the cluster and resolved pen it last wrote to every cell of the
outer terminal. A dirty row is compared against it and only the changed
spans are emitted, each positioned with a `CUP` (or bridged by
rewriting a short unchanged gap when that is fewer bytes). A full-screen
animation that dirties every row every frame therefore costs about what
actually changed, not a full-screen repaint (`phux-esge`).

The front buffer is a claim about the outer terminal, so it holds only
while nothing else writes over pane cells. The disjointness invariant
above keeps the steady-state chrome out of pane rects. Every exception
must invalidate the front buffer through
`TerminalRenderer::invalidate_front` (all rows) or
`invalidate_front_rows` (some rows):

- screen clears (the full-frame clear, the SIGWINCH clear, the
  full-screen overlay path), an incremental frame that failed to reach
  the terminal, and a frame the stdout writer dropped;
- modal overlays and the copy-mode status strip;
- the predictive-echo overlay, for the rows its guesses cover.

The renderer invalidates on its own for a forced paint (the full-frame
path after its `ED2`), a moved origin or clipped extent, a replica
generation change, an alternate-screen switch, a selection change, and
kitty graphics replayed over the pane. An invalidated row is repainted
whole the next time it is dirty, exactly as before the diff, so a
missing invalidation shows up as stale cells and an extra one only
costs bytes.
