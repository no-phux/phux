---
audience: contributors, agents
stability: stable
last-reviewed: 2026-09-21
---

# 0133 — One client runtime below every binding

**TL;DR.** Everything between the sans-IO session kernel and a language
binding — target resolution, dial planning, reconnect policy, the frame
pump, the engine owner thread, and the projected grid — lives once, in
`phux-client-runtime`. `phux-client-ffi` (C) and phux-mobile's UniFFI
bridge become projection shims over it that hold no state machine. The
cell layout is defined once and lent or copied per language. A bindings
generator is a shim detail, chosen per language on merits.

Status: Accepted
Date: 2026-09-19
Superseded in part by [ADR-0134](./0134-connected-lanes-refine-the-binding-boundary.md): decisions 2, 4 and 5 are restated for the connected-client boundary and the settled generator.
See [ADR-0134](./0134-connected-lanes-refine-the-binding-boundary.md) for the decode point a binding keeps without its socket, and the C ABI's two lanes.

## Context

`phux-client-core` is the one sans-IO kernel (ADR-0020, ADR-0100). Three
consumers drive it, and each rebuilt the layer above it:

- `phux-client-ffi` (Cockpit, C): a byte-relay tunnel in `remote/` that
  resolves the `[[remote]]` registry, plans QUIC and WSS dials under the
  CLI's pin-and-token rules, keeps liveness, and cuts WebSocket frames.
  Reconnect is Cockpit's, in Zig. Its grid is a borrowed
  `PhuxTerminalCell` arena: POD, zero-copy.
- phux-mobile (Swift, UniFFI): `wire/conn.rs` plans the same dials, walks
  its own backoff ladder with its own fatal-refusal rule, decodes frames,
  and hosts the engine on an owner thread. Its `GridCell` is a record
  with a `String` per cell, lowered field by field; the render pump names
  that lowering as its dominant cost and gates offscreen panes around it.
- `phux-tui`: the attach loop, with a third reconnect.

`phux-dial` proved the factoring: dialing and keepalive exist once and
all three use them. It stopped one layer early. Above it, a wire or trust
change is edited in two repositories and nothing compiles to catch the
copy that drifts. The question that prompted this ADR — replace UniFFI
with a faster generator — is answered wrongly at that layer: the cost is
the cell shape and the duplicated orchestration, not the generator.

## Decision

1. **`phux-client-runtime` is a new workspace crate** and the only home
   for client orchestration: registry resolution, dial planning with the
   CLI's trust rules, reconnect policy and the fatal-refusal
   classification, WebSocket frame cutting, the relay tunnel, and — as
   later rungs land — the frame pump into `SessionKernel`, the engine
   owner thread, and grid publication. It depends on `phux-dial`,
   `phux-config`, and `phux-protocol` now, and on `phux-client-core`
   once the pump moves. It exposes a Rust API and no FFI.
2. **A binding crate holds no state machine.** `phux-client-ffi` and the
   mobile bridge translate runtime-owned values into their language's
   idiom and nothing else. A loop, a `select!`, or a backoff constant in
   a binding is in the wrong crate.
3. **One cell layout.** The `PhuxTerminalCell` + UTF-8 arena shape moves
   under `phux-client-core` as the single definition. The C ABI lends a
   pointer to it; UniFFI copies it as two byte vectors. No binding
   defines a second cell.
4. **The runtime owns the engine thread and double-buffers the view.**
   Ghostty's `!Send` stays inside the runtime; a consumer acquires a
   published front buffer carrying a generation counter and dirty rows,
   and is never told which thread to call from.
5. **Bindings generators are a shim decision**, made once the shim is
   thin and the grid is POD. The spike is boltffi against direct
   `client.h` import, with UniFFI as the baseline: boltffi's zero-copy is
   built for primitive-field records, which the shared cell layout is,
   and it serves Swift and Kotlin from one surface. UniFFI stays until
   that spike has run.

Rungs land in this order, each shippable on its own: transport into the
runtime (the C tunnel consumes it; Cockpit unchanged); cell layout into
core; engine owner thread and frame pump into the runtime; C ABI over the
runtime; mobile over the runtime at its next `PHUX_REV` pin; the Swift
spike. The beads epic that cites this ADR tracks them.

## Why

One implementation per state machine is what makes a wire change one
edit. A thin, mechanical shim is exactly what a compiler and an agent
check well; a bespoke async ladder duplicated across repositories is
exactly what they do not. Cockpit already runs the borrowed POD grid in
production; mobile's per-cell records are a slower second definition of
the same thing. The tunnel, the ladder, and the trust wording each carry
tests today, and moving the code moves the tests with it.

## Tradeoffs

- One more crate (nineteen), carrying the tokio and quinn edges that
  `phux-client-ffi` already had.
- Until mobile re-pins, the reconnect policy has one in-repo owner and
  no in-repo caller. The pin is the serialization point, not a choice.
- Cockpit keeps its Zig reconnect and its socket-pair tunnel model;
  moving it onto the runtime-owned pump is a later rung, not this one.
- The mobile control plane (uploads, transcribe, directory listings,
  receipts) is a superset of the C ABI's. The runtime grows to it rather
  than the C ABI shrinking.

## Alternatives

**Swap UniFFI for boltffi now.** Faster lowering of the same wrong
shape, and two orchestrations still live in two repositories. It is the
leading candidate in decision 5's spike, once the shape is fixed.

**Stack UniFFI over the C ABI.** Two boundaries with `unsafe` between
them; phux-mobile ADR-0028 already rejected it.

**Move the mobile bridge into `phux-client-ffi`.** Puts Swift idiom in
the C crate and still leaves the transport duplicated with the TUI.
