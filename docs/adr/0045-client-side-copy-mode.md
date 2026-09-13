---
audience: contributors
stability: stable
last-reviewed: 2026-07-11
---

# 0045 — Client-side copy-mode over the consumer's own engine

**TL;DR.** Copy-mode — selection, char/line/block modes, one-shot word/line/output grabs, and copy-to-clipboard — is a client-local projection over the focused pane's own libghostty engine, not a wire feature. The abi epic's server-side selection frames are withdrawn; selection state, the `SelectionRect`/`SelectionMode`/`CopyRequest` contract, engine resolution, and OSC 52 emission all live in `phux-client`, and nothing about a selection touches the wire.

Status: Accepted
Date: 2026-07-11

## Context

The `abi` epic scoped copy-mode as a wire feature: the client would send selection anchors, the server would track a selection against its authoritative grid, and the extracted text would come back as a new frame pair (a `SELECT` / `COPY_RESULT` round trip). That shape predates [ADR-0030](./0030-engine-delegated-wire-and-projection-consumers.md), which settled that any structured view of a terminal — and a selection over a grid is exactly that — is a consumer-side projection of the shared engine, never a wire tier. A selection frame would be a second terminal model on the wire: it can drift from the engine, it taxes every consumer that never selects, and it re-creates the re-parse liability [ADR-0013](./0013-libghostty-bytes-on-wire.md) paid to remove. The two designs cannot both stand.

Both ends already run libghostty ([ADR-0013](./0013-libghostty-bytes-on-wire.md)); the client owns one `Terminal` per attached pane and feeds it `TERMINAL_OUTPUT`. Everything copy-mode needs — cell geometry, word/line/semantic boundaries, formatted extraction — is already answerable from that client-side engine.

## Decision

Copy-mode is a **client-local projection**, implemented entirely in `phux-client`:

1. **No wire surface.** No selection frame, no copy round trip, no `PROTOCOL_VERSION` bump. The abi epic's `SELECT` / `COPY_RESULT` frames are withdrawn. This is the direct application of [ADR-0030](./0030-engine-delegated-wire-and-projection-consumers.md) point 1.

2. **Selection is client state.** The overlay tracks a two-corner selection in pane-local viewport cells plus a `SelectionMode` (`Char` linear, `Line`, `Rect` block). Arrow/mouse input mutates it; nothing leaves the client.

3. **Extraction is engine-resolved.** On copy, the dispatcher resolves the selection against the focused pane's own `libghostty_vt::Terminal` — `format_selection_alloc` for the two-corner rectangle (block when the mode is `Rect`), and `select_word`/`select_line`/`select_all`/`select_output` for the one-shot grabs — then writes the text to the host clipboard via OSC 52. The engine is never re-implemented and the grid is never re-encoded.

4. **One shared contract, one owner.** The plain-data contract that both the selection UX and the renderer depend on — `SelectionRect { start, end, rectangle }`, `SelectionMode`, `CopyRequest`, `SelectionGrab` — lives in one leaf module (`render/overlay/selection.rs`) that imports neither the overlay state machine nor the renderer. The highlight geometry (`SelectionRect::contains`, block vs linear) is part of that contract, so the copy path and the on-screen highlight cannot disagree about what a block selection covers.

## Why

Delegating to the client engine is the only choice consistent with [ADR-0030](./0030-engine-delegated-wire-and-projection-consumers.md): the engine is shared and never re-encoded, so a selection computed client-side is exactly what a server-side selection frame would have returned, minus the drift surface and the conformance tax on non-selecting consumers. libghostty already exposes the selection and formatting primitives, so the client calls them rather than re-implementing terminal semantics. OSC 52 puts the text on the host clipboard without a phux-specific clipboard verb.

## Tradeoffs

- **Every consumer that wants copy-mode runs the engine.** This is the same cost the browser client already pays ([ADR-0030](./0030-engine-delegated-wire-and-projection-consumers.md) point 4); we accept it.
- **Scrollback selection is bounded by the client mirror's scrollback**, not the server's full history. Acceptable: the client already owns the scrollback it renders.
- **The block-highlight and the mode UX share one `SelectionRect` field** (`rectangle`) across two files. To keep a fan-out from racing on it, the field and its geometry are declared and landed in the shared contract module first; the UX then populates it and the renderer reads it against an already-landed type. See CONTRIBUTING.md "Multi-agent fan-out" — this is that discipline applied to a shared struct.

## Alternatives

**(A) Server-side selection frames (the abi epic) — rejected.** Track the selection against the server grid and return extracted text as a frame. Rejected by [ADR-0030](./0030-engine-delegated-wire-and-projection-consumers.md): it puts a structured terminal view on the wire, drifts under capability mismatch, and taxes consumers that never select.

**(B) A client-side selection that re-parses VT instead of calling libghostty's selection API — rejected.** Re-implements what the engine already does and re-introduces a second model client-side. We call `format_selection_alloc` / `select_*` directly.
