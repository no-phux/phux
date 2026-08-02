---
audience: contributors
stability: stable
last-reviewed: 2026-08-02
---

# 0034 — Kitty graphics / image passthrough through the cell renderer

**TL;DR.** phux is a cell renderer, not a byte replayer, so out-of-band
Kitty graphics (`APC _G`) are forwarded by the server but dropped by the
client, which repaints from its libghostty cell grid only. Prefer the
Kitty **Unicode-placeholder** protocol — placements become cell content
the existing cell walker already paints — with a client-side **APC re-emit**
fallback. Snapshot/reattach needs the server grid synthesizer to replay
live image state. Proposed; no code in this ADR.

Status: Accepted
Date: 2026-06-17

## Context

The wire is asymmetric (ADR-0013): server-to-client terminal content is
opaque VT bytes; the server forwards `APC _G` graphics verbatim when the
client negotiated `ImageProtocol::KittyGraphics`
(`phux-server::downsample::handle_apc`). The docs' "modern protocols ride
through as bytes" claim reads as if that is the whole story.

It is not. The client does not replay the byte stream onto the screen. It
feeds incoming bytes into its own libghostty `Terminal`
(`vt_write`, `attach::server_frame`) and then **re-renders cells only**:
the render path walks the grid row by row and emits `CUP` + styled cell
content (`attach::render::render_at_inner`). Kitty graphics are
**out-of-band** image state, not grid cells, so they never reach the
output the renderer produces. The image arrives at the client's
libghostty mirror and is then dropped on repaint. The same is true across
a resize resync or reattach, which repaint from `TERMINAL_SNAPSHOT`.

libghostty-vt already exposes the state the renderer would need:
`Terminal::kitty_graphics()`, a `PlacementIterator` over placements,
`Image` with `data()` / `format()`, and `Placement::is_virtual()`. phux
never calls any of it. So the gap is a phux render-layer gap, not a
backend limitation.

A prerequisite is in flight separately: pixel-size reporting (so a
graphics program knows the cell pixel geometry) is being fixed in a
sibling PR; image placement that depends on pixel metrics builds on it.

## Decision

Make the cell renderer image-aware in two tiers, **Proposed** for design
review before any code:

1. **Prefer the Kitty Unicode-placeholder protocol.** A placement created
   with the Unicode placeholder (`U+10EEEE` plus diacritic row/column
   encoding) lives *as cell content* in the grid. The existing cell
   walker already visits those cells; the renderer emits the placeholder
   codepoints (and the image-id encoded in the cell's foreground) so the
   outer terminal composites the image. `Placement::is_virtual()` flags
   exactly these placeholder placements, so the renderer can route them
   through the cell path it already owns. This fits phux's "structure is a
   projection of the cell grid" model with the least new machinery: the
   image transmission (`APC _Gf=...,t=...`) is sent once, and placement is
   ordinary cell painting.

2. **Fallback: client-side APC placement re-emit.** For non-placeholder
   (classic) placements, the renderer queries `kitty_graphics()`,
   iterates placements, transmits the `Image` data the mirror holds, and
   re-emits an `APC _Ga=p` placement with `CUP` positioning derived from
   the placement's grid location. This reconstructs the on-screen image
   from authoritative mirror state rather than replaying the original
   byte stream, so it survives the clip/offset math the cell renderer
   already applies per pane.

3. **Snapshot / reattach.** Because the client repaints from
   `TERMINAL_SNAPSHOT` on attach and resize, the **server grid
   synthesizer must replay live image state** into the snapshot it builds
   (transmit the images its mirror holds, then their placements), so a
   fresh client reconstructs the same images a live one shows. Without
   this, images vanish on reattach even after the render path learns to
   paint them.

This stays within ADR-0013: the wire still carries opaque bytes and the
client still renders from its own engine. The change is that the renderer
stops discarding the image state libghostty already parsed.

## Why

- **It matches the architecture.** phux's invariant is that the screen is
  a projection of the client's libghostty grid (ADR-0030). Reading image
  placements off that same grid keeps one source of truth; replaying the
  raw byte stream would fork a second one and reintroduce the drift
  ADR-0013 removed.
- **Placeholders need almost no new code.** Virtual placements are
  already cell content; the cell walker already runs. Tier 1 is the
  cheapest correct path and the one modern image-emitting programs
  increasingly target.
- **The fallback covers classic placements** without a wire change, using
  APIs the backend already exposes.
- **Snapshot replay is non-optional.** Reattach is a headline phux
  feature; an image story that breaks on reattach is not shipped.

## Tradeoffs

- **Two code paths.** Placeholder and classic placements are handled
  differently; the classic re-emit path is more code and more terminal-
  compatibility risk than the placeholder path.
- **Snapshot size.** Replaying image data into snapshots grows them; large
  images make reattach heavier. A size budget or lazy transmit may be
  needed.
- **Pixel-geometry dependency.** Correct placement sizing depends on the
  pixel-reporting fix landing first; until then placement is best-effort.
- **Per-client capability fan-out.** A client that did not negotiate
  Kitty graphics still must not receive image bytes; the render-layer
  change rides the existing capability gate, but the snapshot synthesizer
  must honor it too.

## Alternatives

- **Replay the raw byte stream to the outer terminal (no cell render).**
  Rejected: it forks a second source of truth from the libghostty grid,
  reintroducing the re-parse/drift problem ADR-0013 closed, and breaks the
  per-pane clip/offset compositing the cell renderer does.
- **Strip all images at the server (status quo, made explicit).**
  Rejected: it permanently contradicts the "modern protocols survive
  reattach" promise for images, which is a stated differentiator.
- **A new structured image frame on the wire.** Rejected for now: it
  re-creates the re-encode-on-the-wire problem (ADR-0013) for a payload
  the engine already holds; the placeholder + snapshot-replay path needs
  no new wire type.
