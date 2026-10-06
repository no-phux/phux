---
audience: contributors
stability: stable
last-reviewed: 2026-10-05
---

# 0147 — Plugin overlay panes float outside the layout

**TL;DR.** A plugin pane with `placement = "overlay"` is an ordinary
server Terminal that the TUI never adds to a layout window. While it
lives, the client draws it as a modal box over the pane area and routes
input to it. Any action dismisses it by killing its Terminal, and it
closes when its process exits. The overlay is per client: no wire frame,
no layout metadata.

Status: Accepted
Date: 2026-10-05

## Context

`PluginPanePlacement::Overlay` was valid manifest schema that the TUI
skipped with a warning, because the TUI had no floating live-terminal
surface. Its overlays are ratatui modals (ADR-0026), and every pane it
paints is a tile of the shared layout tree (ADR-0019). Overlays are the
placement plugins reach for most: a picker, a dashboard, a quick prompt
that should not reshape the user's layout. The server needs no change to
host one, and ADR-0017 keeps the TUI from asking for one.

## Decision

1. **Spawn without placement.** The overlay is spawned with the same
   `SPAWN_RESOURCE` a split uses, sized to the box interior. The reply
   seeds a pane slot marked floating, and no window adopts it, so the
   shared layout and other clients never see it.
2. **Modal box.** The box is centered over the pane area at 80% of each
   axis (at least 24x8 when the area allows), with a titled border.
   While it is open the client paints it as the focused pane, and the
   panes beneath pause painting as they do under any modal. They repaint
   whole when it closes.
3. **Input.** Keys, pastes, and pointer events inside the box go to the
   overlay. The prefix table still resolves. Any resolved action first
   dismisses the overlay; for `kill-pane` that is all it does. A press
   outside the box also dismisses it.
4. **Lifetime.** Dismissal kills the Terminal. The overlay closes when
   its process exits, including a retained exit (ADR-0124). At most one
   overlay is open; a second spawn racing an open one is killed.

## Consequences

- No wire, server, or layout change; the overlay is client presentation
  over an ordinary Terminal.
- The overlay is not shared. Another client attached to the session sees
  an unplaced Terminal and draws nothing for it. A client that ends
  without dismissing (a crash, a server-initiated detach) leaves that
  Terminal running until its process exits or someone kills it.
- Background panes do not update on screen while an overlay is open.

## Alternatives

- **A hidden layout window.** Rejected: every window surface (tabs,
  `select-window` indices, the sidebar, peers) would need to learn to
  skip it, and it would persist in shared metadata.
- **Render the Terminal inside a ratatui modal.** Rejected: it gives up
  the VT renderer's cursor, graphics, and damage tracking.
- **Keep background panes painting and repaint the box over them.**
  Rejected for now: a busy pane under the box would repaint the overlay
  on every frame.
