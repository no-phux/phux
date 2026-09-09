---
audience: contributors
stability: stable
last-reviewed: 2026-05-28
---

# 0008 — Use libghostty-vt's types directly; stop reimplementing them

**TL;DR.** Where libghostty already models a plain type (no allocator lifetime, no FFI handle), phux re-exports it directly: input atoms, mouse atoms, style atoms. No parallel enums, no discriminant-pin tests, no per-variant conversion. The narrow "protocol-independent-of-emulator" claim from ADR-0002 is narrowed to multiplexer domain types only.

Status: Accepted

This ADR supersedes parts of ADR-0002 (the
"protocol-independent-of-emulator" stance) for input and style types,
and supersedes the discriminant-equality claim and the post-hoc
divergence amendment in ADR-0006.

Date: 2026-05-25
Superseded in part by [ADR-0013](./0013-libghostty-bytes-on-wire.md): the style atoms and the phux-defined Cell, Grid, DiffOp, and cursor types left the wire with the cell-diff format; the input-atom re-export stands.
See [ADR-0016](./0016-terminal-id-as-wire-primary.md) for the rename: pane_id below is terminal_id on the wire.

## Context

Through wave 1 of the protocol epic, phux-protocol defined its own
parallel-universe enums for libghostty's input and style atoms:
`PhysicalKey` (177 variants matching `key::Key`), `KeyAction`, `ModSet`,
`MouseAction`, `MouseButton`, `Color`, `Underline`, plus a struct
`FocusEvent { gained: bool }`. We added discriminant-pin tests (177-line
table for `PhysicalKey`) to catch drift, and a server-side conversion
layer to bridge to libghostty's encoders.

Wave 2 (phux-6yl.2) exposed a problem: discriminants weren't actually
equal at the live pinned libghostty rev (`KeyAction::Press` was `0` on our
wire but `1` in libghostty; `Mods::CTRL` was `2` on our wire but `4` in
libghostty). The original ADR-0006 amendment tried to rationalize this by
calling our wire "canonical" and treating libghostty as a "backend whose
ABI may shift" — which sounded principled but is actually wrong for this
project:

- libghostty *is* our canonical terminal backend by construction.
  ADR-0002 (cell-level diff protocol) and ADR-0004 (libghostty-vt as
  grid source) both rest on libghostty being central, not interchangeable.
- We have zero third-party clients. The "wire stability across
  backends" we were protecting is theoretical.
- Maintaining parallel enums means: add a key upstream → manually mirror
  variant + tests + conversion → indefinite toil.
- Ghostty PR #12794 (selection APIs) lands this week. libghostty-rs will
  pick it up. We will keep wanting libghostty's evolution to flow into
  phux on `cargo update`, not via a mirror-maintenance treadmill.

## Decision

**Where libghostty already models a type and the type is plain (no
allocator lifetime, no FFI handle), phux re-exports it directly.** No
mirroring; no parallel enum; no discriminant-pin tests.

### What re-exports

| phux-flavored name | Source                                  |
|--------------------|-----------------------------------------|
| `PhysicalKey`      | `libghostty_vt::key::Key`               |
| `KeyAction`        | `libghostty_vt::key::Action`            |
| `ModSet`           | `libghostty_vt::key::Mods`              |
| `MouseAction`      | `libghostty_vt::mouse::Action`          |
| `MouseButton`      | `libghostty_vt::mouse::Button`          |
| `FocusEvent`       | `libghostty_vt::focus::Event`           |
| `Color`            | `libghostty_vt::style::StyleColor`      |
| `RgbColor`         | `libghostty_vt::style::RgbColor`        |
| `PaletteIndex`     | `libghostty_vt::style::PaletteIndex`    |
| `Underline`        | `libghostty_vt::style::Underline`       |

### What stays phux-defined

Two categories.

**1. Wire-friendly outer structs over libghostty's allocator-bound events.**
libghostty's `key::Event<'alloc>` and `mouse::Event<'alloc>` are FFI
handles bound to an allocator lifetime — not safe to put in a `Vec` or
send across an async boundary. We define plain structs with the same
fields, composing libghostty's atoms.

- `phux_protocol::input::KeyEvent` (composes `KeyAction` + `PhysicalKey`
  + `ModSet` + text fields)
- `phux_protocol::input::MouseEvent` (composes `MouseAction` +
  `MouseButton` + `ModSet` + `f64` pixel position)

**2. Multiplexer concepts libghostty doesn't model.**

Per ADR-0013, the wire-content shape on the output side collapses to
opaque byte payloads. What stays phux-defined on the wire is:

- `FrameKind`, `SessionId`, the envelope layer, lifecycle frames
  (`ATTACHED`, `DETACHED`, `PANE_OPENED`, etc.) — phux's wire format
  and multiplexer domain. Unaffected by ADR-0013.
- `PaneOutput { pane_id, bytes }` and `PaneSnapshot { pane_id, cols,
  rows, vt_replay_bytes }` — phux-defined envelopes whose *payload*
  is opaque VT bytes (ADR-0013).
- `PasteTrust` / `PasteEvent` — libghostty's `paste` module is *free
  functions* (`is_safe`, `encode`), not a typed event. `PasteTrust` is
  phux-defined per-pane policy metadata, not a mirror of anything.

Pre-ADR-0013 this category also included `Cell`, `Grid`, `DiffOp`,
`CursorState`, `CursorShape`, and the `CellFlags` `u16` bitfield —
all of which were wire shapes for the structured cell-diff protocol.
ADR-0013 retires them as wire types. They may persist briefly during
the bytes-on-the-wire transition; do not extend them.

### Wire byte stability

Phux still owns the wire bytes. The wire encoder writes phux-stable tag
values (e.g. `COLOR_NONE = 0x00`, `COLOR_PALETTE = 0x01`, `COLOR_RGB =
0x02`) regardless of libghostty's internal `repr(u32)` discriminants. The
decoder matches on those phux-stable tags. Round-trip stability is
enforced by `proptest` and `insta` snapshots in
`phux-protocol/tests/diff_wire_snapshots.rs`. If libghostty renumbers an
enum, our wire bytes are unaffected — the encoder maps phux variants to
phux bytes, not by raw `as u32` cast.

## Rationale

- **libghostty is the canonical backend, not a swappable dep.** The
  whole project bets on it (ADR-0001 picked Rust over Zig precisely
  because of `libghostty-rs`). Pretending the protocol is portable
  across emulators costs effort and buys nothing concrete.
- **Forward-compat is automatic.** When Ghostty merges a new key, a new
  mouse button, a new SGR underline style, a `cargo update` lands it on
  phux's wire. Variant additions are non-breaking because libghostty's
  enums are `#[non_exhaustive]`.
- **Discriminant-pin tests evaporate.** They were tautological the
  moment we re-exported (the types ARE libghostty's). 177 lines of
  test deleted.
- **Server-side conversions collapse.** `*_to_libghostty` functions
  shrink to "compose libghostty's allocator-bound `Event` from our wire
  struct's fields." No enum remapping.
- **Wire stability is preserved separately** via phux-owned tag bytes
  in `wire/diff.rs` and the snapshot tests.

## Tradeoffs

- **phux-protocol now depends on `libghostty-vt`.** Every consumer of
  the protocol crate pulls libghostty-vt-sys's Zig build chain.
  - Mitigation today: phux-server and phux-client both need libghostty
    anyway (server for encoders, client likely for native GUI input
    generation). The added cost is zero for the real consumer set.
  - Mitigation later: if a wire-only consumer ever materializes (a Go
    phux-client, a WASM browser viewer), we contribute a
    `libghostty-vt-types` no-build subcrate upstream and depend on that
    from phux-protocol.
- **We inherit libghostty's naming choices.** `StyleColor::None` (not
  `Default`), `mouse::Button::Unknown` (not `None`), `focus::Event` is
  an enum not a struct. These are aesthetic frictions, not blockers.
- **ADR-0002 is partially superseded by this ADR, and fully
  superseded by ADR-0013.** This ADR narrowed "protocol independent
  of emulator implementation" away from input/style atoms; ADR-0013
  then retires the diff protocol entirely in favor of bytes on the
  wire. The "protocol-independent-of-emulator" claim survives only
  for the multiplexer domain (sessions, windows, panes, lifecycle
  frames), which is phux-defined and not in libghostty's vocabulary.

## What this ADR replaces

- **ADR-0002** §"protocol is independent of any specific emulator
  implementation" — narrowed by this ADR to: the *diff protocol
  shape* and *multiplexer domain* are emulator-independent (input
  atoms and style atoms are not). **ADR-0013 then supersedes ADR-0002
  in full** — the diff protocol shape is gone; only the multiplexer
  domain remains as the emulator-independent surface.
- **ADR-0006** §"The numeric values of `PhysicalKey`, `MouseButton`,
  `MouseAction`, `KeyAction` match libghostty's enums verbatim" — true
  now by construction (same types). The post-hoc divergence amendment
  ("KeyAction/Mods have different discriminants; we remap") is dropped
  — there's no remap because there's no separate type.

## Discovered along the way

- `libghostty_vt::key::Key` is **not** `#[non_exhaustive]`; the other
  re-exported enums are. We document this where it matters.
- `libghostty_vt::paste` is free functions only — no `PasteTrust` enum
  upstream. Our `PasteTrust` is phux-specific policy metadata, now
  documented as such.
- libghostty's `style::Style` (a plain struct with eight per-bool fields)
  was not re-exported; phux originally packed its bools into the
  `CellFlags` `u16` bitfield for compact wire transit. Under ADR-0013
  `CellFlags` is dead as a wire type — bytes on the wire carry SGR
  state inline — so this reconsideration is moot.

## Related

- ADR-0001 — language: Rust (chose Rust *because* of libghostty-rs).
- ADR-0002 — diff-based protocol (this ADR partial-superseded it on
  input/style; **ADR-0013 supersedes it in full**).
- ADR-0013 — libghostty bytes on the wire. The same "use libghostty's
  shape, don't mirror it" insight applied at the protocol layer
  instead of the type layer.
- ADR-0004 — libghostty-vt as grid source.
- ADR-0006 — input mirrors libghostty (partial supersede; the
  discriminant-equality claim and divergence amendment are dropped).
- Ghostty PR #12794 — selection APIs (motivating example: libghostty
  evolution flows in via `cargo update`, and selection support lands
  cleanly through the `phux-abi` epic).
