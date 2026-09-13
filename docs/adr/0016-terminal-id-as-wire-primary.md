---
audience: contributors
stability: stable
last-reviewed: 2026-05-28
---

# 0016 — `TerminalId` as the wire primary; `PaneId` is a consumer-side alias

**TL;DR.** The wire identity for a managed terminal is `TerminalId`, a tagged union of `LOCAL { id: u32 }` and `SATELLITE { host, id }` per ADR-0015's federation cross-cut. `PaneId` is removed from the wire. The string "pane" survives in the reference TUI's user-facing vocabulary; the leaf of the TUI's layout tree is a `TerminalId`. Every `PANE_*` wire message renames to `TERMINAL_*`.

Status: Superseded by ADR-0102
Date: 2026-05-26
Superseded by [ADR-0102](./0102-resources-the-server-serves-kinds.md): `ResourceId` replaces `TerminalId` as the wire primary; Terminal becomes kind 0.

## Context

The wire identity for a managed terminal is currently called
`PaneId`. The name leaked in from the TUI consumer's vocabulary: a
"pane" is a leaf of a layout tree displayed on screen. The thing the
server has actually been managing all along is the libghostty
`Terminal` and its PTY — entities that exist independent of any
layout. Calling them "panes" entangles the identity of the substrate
primitive with a TUI-shaped role.

[ADR-0015](./0015-protocol-layering.md) declares L1 as the substrate
layer: terminals, no sessions, no windows, no panes. The identity
inconsistency between "what the protocol manages" and "what we call
it" needs to be resolved before SPEC.md can be restructured along the
layer lines ADR-0015 establishes.

## Decision

The wire identity for a managed terminal is **`TerminalId`**.

```
TerminalId = tagged_union {
    LOCAL     { id: u32 },                                 // tag = 0
    SATELLITE { host: SatelliteHost, id: u32 },            // tag = 1
}
```

v0.1 servers construct `LOCAL` only. v0.1 decoders MUST accept the
`SATELLITE` tag and respond `ERROR { code: UnsupportedSatelliteRoute }`
if not configured as a federation hub. The shape matches the existing
`SessionId` discriminant reserved by
[ADR-0007](./0007-mosh-class-transport-and-satellites.md); this ADR
extends that scheme uniformly to every identity, as required by
ADR-0015.

`PaneId` is removed from the wire. In the reference TUI's L3 metadata
conventions, the leaf node of the TUI's layout tree contains a
`TerminalId` directly. The string "pane" remains in the TUI's user-
facing vocabulary (CLI subcommands, status bar widgets, keybinding
action names) because that's the word users expect; it carries no
protocol meaning.

Every existing wire message that takes a `PaneId` is updated:
`INPUT_KEY`, `INPUT_MOUSE`, `INPUT_FOCUS`, `INPUT_PASTE`, `INPUT_RAW`,
`PANE_OUTPUT` → `TERMINAL_OUTPUT`, `PANE_SNAPSHOT` → `TERMINAL_SNAPSHOT`,
`BELL`, `OSC_EVENT` → `TERMINAL_EVENT`. The message catalog in SPEC.md
gets a corresponding rename pass (see ADR-0015 §"Doc impact").

In code, `phux-core::Pane` becomes `phux-core::Terminal` (or moves to
`phux-server` if the per-terminal state is purely a server concern;
the crate boundary is decided in the ARCHITECTURE.md cascade, not
here). The wire-side `phux-protocol::ids::PaneId` newtype becomes
`TerminalId` with the tagged-union shape above.

## Rationale

- **Identity follows what the server owns.** The server owns a PTY
  and a libghostty `Terminal`. Calling that pair a "Pane" is what we
  call it *when displayed*. The identity should match the thing, not
  the role.
- **Federation forward-compat is uniform.** ADR-0015's cross-cutting
  federation axis applies to every identity. `TerminalId` getting
  the `SATELLITE` tag from day one means no breaking wire change when
  satellites land.
- **L1-only consumers stop being lied to.** An agent that observes
  `INPUT_KEY { pane_id: 7 }` is being told "this is a pane" when it
  has never asked about layout. Renaming to `TerminalId` removes the
  false implication.

## Tradeoffs

- **Coordinated rename across the workspace.** Every crate touches
  the type. `phux-protocol` is the wire-stable home; everything
  downstream tracks. Mechanical but broad.
- **Snapshot tests churn.** The `insta` snapshots of representative
  wire bytes (SPEC §16 + `crates/phux-protocol/tests/snapshots/`)
  re-baseline because field names change.
- **One temporary asymmetry.** Until SPEC.md is restructured per
  ADR-0015, the message catalog will have `TerminalId` on terminal-
  layer messages and `PaneId` / `WindowId` mentioned in residual L3-
  ish messages that haven't been demoted to metadata yet. Plan: do
  the rename and the SPEC restructure in adjacent commits.

## Alternatives considered

- **Keep `PaneId` and document the role-vs-identity gap.** Cheapest;
  loses the conceptual clarity that motivates ADR-0015. The point
  of layering is that consumers can opt out of TUI vocabulary;
  keeping the name forces every consumer to know what a pane is.

- **Two types: `TerminalId` for the wire, `PaneId` as a TUI alias.**
  Considered. `PaneId` becomes a type alias for "a `TerminalId` that
  appears as a leaf of a layout tree", which is exactly what the
  TUI's metadata conventions already describe. A separate type adds
  no information beyond what the metadata schema already encodes.
  Reject; let the TUI's design doc say "the leaf of the layout tree
  is a `TerminalId`" and be done.

- **`TerminalRef` instead of `TerminalId`** (a fatter type that
  carries scope/permissions/etc.). Premature; today we only need
  identity + routing. If permissions become a wire concept later
  they can hang off the surrounding message.
