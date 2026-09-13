---
audience: contributors
stability: stable
last-reviewed: 2026-07-15
---

# 0006 — Input event types re-export libghostty-vt's atoms

**TL;DR.** phux-protocol re-exports libghostty-vt's input atoms (`PhysicalKey`, `KeyAction`, `ModSet`, `MouseAction`, `MouseButton`, `FocusEvent`) directly rather than mirroring them. Outer wire-side event structs wrap those atoms with terminal addressing and any phux-specific framing. Server-side conversion to libghostty-vt's allocator-bound `Event` types is a free function in phux-server.

Status: Accepted

This ADR was substantially rewritten on 2026-05-25 to consolidate the
supersession by ADR-0008; see git history for the original draft and
its two amendments. ADR-0008 is the *why*; this ADR is the *what* for
the input wire.

Date: 2026-05-24 (original) / 2026-05-25 (rewrite)
See [ADR-0013](./0013-libghostty-bytes-on-wire.md) for why structured input stays phux-defined client-to-server while pane content became bytes.
See [ADR-0016](./0016-terminal-id-as-wire-primary.md) for the rename: PaneId and pane_id below are TerminalId and terminal_id on the wire, and "pane" is a TUI-consumer convention per [ADR-0015](./0015-protocol-layering.md).

## Context

Server-side, phux feeds input events to libghostty-vt's encoders
(`key::Encoder`, `mouse::Encoder`, `focus::Event::encode`,
`paste::encode`). The encoders take typed event structures and produce
the exact PTY bytes for whatever protocol the inner program currently
expects — KIP at any progressive-enhancement level, legacy fixterms, SGR
/ SGR-Pixels mouse, etc. Round-trip fidelity from the wire into those
encoders is the design constraint SPEC §9 has to satisfy.

The early draft of SPEC §9 modeled inputs from the *application* side
(`Key = CHAR(u32) | NAMED(NamedKey)`, codepoint already layout-resolved).
The first diff spike caught the mismatch: that shape is what an
application receives *after* the terminal has done its work, and feeding
it into a KIP-capable encoder requires lossy translation — alternate
keys, modifier-only events, and side-discriminated modifiers cannot
round-trip.

The shape on the wire therefore has to be libghostty-vt-shaped. The
remaining question is *how* to express that: mirror the upstream enums
into phux-protocol (the original 0006 decision), or re-export them
directly (ADR-0008). This ADR records the latter.

## Decision

phux-protocol re-exports libghostty-vt's input atoms directly. Outer
wire-side event structs wrap those atoms with pane addressing and any
phux-specific framing. The server-side conversion to libghostty-vt's
allocator-bound `Event` types is a free function in phux-server.

### Re-exported atoms

The following atoms are `pub use`d from libghostty-vt unchanged. There
is no parallel type:

| phux-protocol name | Source                            |
|--------------------|-----------------------------------|
| `PhysicalKey`      | `libghostty_vt::key::Key`         |
| `KeyAction`        | `libghostty_vt::key::Action`      |
| `ModSet`           | `libghostty_vt::key::Mods`        |
| `MouseAction`      | `libghostty_vt::mouse::Action`    |
| `MouseButton`      | `libghostty_vt::mouse::Button`    |
| `FocusEvent`       | `libghostty_vt::focus::Event`     |

### phux-owned wire-side wrappers

The outer event structs that cross the wire are phux-defined. They
compose the re-exported atoms and add pane addressing plus any framing
the multiplexer needs:

- `phux_protocol::input::KeyEvent` — `pane_id`, plus libghostty-shaped
  `action`, `key`, `mods`, `consumed_mods`, `composing`, `text`,
  `unshifted_codepoint`.
- `phux_protocol::input::MouseEvent` — `pane_id`, plus `action`,
  `button`, `mods`, and a pane-local pixel `position`.
- Focus is the re-exported `libghostty_vt::focus::Event` plus a
  `pane_id` at the frame layer.
- `phux_protocol::input::PasteEvent` — `pane_id`, raw bytes, and a
  `PasteTrust` policy field. libghostty-vt's `paste` module is free
  functions only (`is_safe`, `encode`); there is no upstream paste-event
  type to re-export. `PasteTrust` is phux-defined per-pane policy.

The outer structs exist because libghostty-vt's `key::Event<'alloc>` and
`mouse::Event<'alloc>` are allocator-bound FFI handles — not safe to
`Vec`-store or send across an async boundary. Wrapping them with plain
fields composing the atoms gives us a wire-friendly representation that
still composes back into the FFI types server-side.

### Server-side bridge

`phux-server` constructs the libghostty-vt allocator-bound events from
the wire-side fields via free functions in
`crates/phux-server/src/input/` — currently `key_event_to_libghostty`
and `mouse_event_to_libghostty`. These are *not* `From` impls: both
`phux_protocol::input::KeyEvent` and `libghostty_vt::key::Event` are
foreign to phux-server, and Rust's orphan rules forbid the cross-crate
`From`. A free function is the only legal expression of the conversion,
and it is plenty — the call sites are exactly the per-pane encoders.

## Rationale

The original 0006 decision was parallel mirror types in phux-protocol
with discriminant-pin tests and a `*_to_libghostty` remap layer. The
stated motivation was a "clean wire format with no leaky libghostty
deps." ADR-0008 reversed that call. The reasons, restated here for a
reader who lands on this ADR first:

- **The leak is illusory.** phux-protocol depends on `libghostty-vt`
  under the `server` feature already; the protocol crate's real
  consumers (phux-server, phux-client) both pull libghostty-vt-sys's
  build chain anyway. Mirroring atoms doesn't avoid the dependency, it
  just adds parallel types alongside it.
- **Mirror types drift.** The wave-2 implementation discovered
  `KeyAction` and `Mods` discriminants had silently diverged from
  upstream at the pinned libghostty rev. Catching that required a
  177-line discriminant-pin table. Re-exporting deletes the table and
  the drift class entirely.
- **Orphan rules kill the clean-conversion benefit.** Even with mirror
  types, the conversion can't be a cross-crate `From` impl — it has to
  be a free function in phux-server. The "infrastructure" the mirror
  was protecting (`impl From<&Wire> for Lg`) doesn't exist in any
  universe; the conversion is a free function either way.
- **Upstream evolution rides along.** When Ghostty merges a new key, a
  new mouse button, or a new KIP refinement, `cargo update` lands it on
  phux's wire. Variant additions are non-breaking because libghostty's
  enums are `#[non_exhaustive]` (with the documented exception of
  `key::Key`).

The original draft's deeper motivation — round-trip fidelity into the
KIP encoder, encoder options staying server-local, native libghostty-
surface clients producing wire events with no flattening — survives
intact. The implementation just got simpler.

## Consequences

- **Less code; no drift.** No mirror enums, no discriminant-pin tests,
  no per-variant conversion arms. Server-side bridge functions
  construct libghostty events from wire-side fields and that's the
  entirety of the input adapter layer.
- **Upstream tracking is atomic.** A libghostty-vt point release is a
  `cargo update`, not a porting exercise.
- **phux-protocol's public API is partially shaped by libghostty-vt's.**
  A libghostty-vt major version bump forces phux-protocol to bump in
  lockstep. Mitigated by the wire-side wrappers (`KeyEvent`,
  `MouseEvent`, `PasteEvent`) staying phux-owned: the atoms inside them
  are libghostty's, but the framing — pane addressing, paste trust,
  any future phux-specific fields — is ours and stable across upstream
  churn.
- **Encoder options stay server-local.** Cursor-key application mode,
  keypad mode, modifyOtherKeys, KIP flags, alt-esc-prefix, backarrow,
  macos-option-as-alt — none of this traverses the wire. The server
  actor owns the `Terminal` and publishes libghostty's exact derived options;
  the dedicated input lane applies them before encoding (ADR-0044).
  Per-pane encoder state is private to the server.
- **Per-pane encoder isolation is preserved.** Mouse, key, focus, paste
  encoders are per-pane. No shared global encoder state. This is the
  invariant ADR-0007 inherits when satellites land.
- **`HYPER` and `META` are not separate modifier bits.** Inherited from
  libghostty (and the underlying reality on most platforms: they are
  XKB-configurable mappings to `SUPER`, not independent kernel-level
  flags). Users wanting tiling-WM-style modifier-only bindings get them
  via KIP's report-events flag plus configuration, not via wire-level
  Hyper/Meta bits.

## Alternatives considered

- **Parallel mirror types in phux-protocol** (the original 0006
  decision). Rejected per the rationale above: dep-graph leak was
  illusory, mirror drift was real, orphan rules nullified the
  clean-conversion benefit, and the maintenance treadmill bought
  nothing concrete.
- **Opaque pre-encoded VT bytes** (`INPUT_RAW` everywhere). Trivially
  faithful at the byte level but discards the structured information
  KIP needs to be encoded correctly per-pane. Forces every client to
  know every encoding the inner program might want — exactly the thing
  the multiplexer should hide. Defeats the structured-input goal of
  SPEC §9.
- **Re-export atoms but flatten wrappers into bare libghostty events
  on the wire.** Rejected: libghostty's `key::Event<'alloc>` and
  `mouse::Event<'alloc>` are allocator-bound. They are not
  serializable across the wire or storable in a `Vec` without owning
  copies of their fields. The plain wrapper structs are doing real
  work, not redundant framing.

## References

- ADR-0008 — use libghostty-vt's types directly. The *why* behind this
  ADR's *what*. Read 0008 if you want the dep-graph and forward-compat
  argument in full.
- ADR-0002 — diff-based protocol. **Superseded in full by ADR-0013**
  (bytes-on-wire for pane content). Partial-supersede-by-0008 note for
  input and style atoms is now subsumed by 0013's broader change on
  the output side; the input side (this ADR) is unaffected by 0013
  and in fact reinforced — see ADR-0013.
- ADR-0013 — libghostty bytes on the wire. The reason structured input
  becomes more, not less, justified: only the server knows pane mode,
  so input cannot be pre-encoded client-side.
- ADR-0004 — libghostty-vt as grid source. Same load-bearing dep on
  the server side.
- ADR-0007 — Mosh-class transport and satellites. Inherits the
  per-pane encoder isolation invariant.
- `crates/phux-protocol/src/input/` — wire-side event structs and
  re-exports.
- `crates/phux-server/src/input/` — the conversion bridge
  (`key_event_to_libghostty`, `mouse_event_to_libghostty`) and the
  per-pane encoders that consume it.
- SPEC §9 — input frame format.
