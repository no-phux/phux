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

The decision is [ADR-0020](../adr/0020-layered-render.md). Chrome carves
skip-cell rectangles for pane rects so libghostty owns those cells
exclusively. `ratatui` lives under `phux-tui/src/render/` (`chrome`,
`overlay`); `phux-client-core` (pane mirror, predict, layout, multi-pane)
and `phux-client` (connection, agent verbs) cannot name it
([ADR-0100](../adr/0100-the-tui-is-its-own-crate.md)). The attach loop lives
in `phux-tui` because it composites chrome over panes.

## Pane interiors are cell-diffed

The pane painter (`attach/render.rs`) visits the rows libghostty reports
dirty, but it does not rewrite them whole. Each pane keeps a front
buffer: the cluster and resolved pen it last wrote to every cell of the
outer terminal. A dirty row is compared against it and only the changed
spans are emitted, each positioned with a `CUP` (or bridged by
rewriting a short unchanged gap when that is fewer bytes). A full-screen
animation that dirties every row every frame therefore costs about what
actually changed, not a full-screen repaint.

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
whole the next time it is dirty. Missing invalidation leaves stale cells;
unnecessary invalidation costs bytes.

## Status

No remaining target-versus-shipped gaps.

| Gap | Today | Owner | Tracked |
|---|---|---|---|
