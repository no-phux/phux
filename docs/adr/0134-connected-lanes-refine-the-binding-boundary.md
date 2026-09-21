---
audience: contributors, agents
stability: stable
last-reviewed: 2026-09-21
---

# 0134 — Connected lanes refine the binding boundary

**TL;DR.** Landing ADR-0133's rungs sharpened three of its words. A binding
holds no *connected-client* state machine, which leaves a consumer's local
playground projector outside the rule; the runtime owns each *connected
resource's* engine thread; and a binding may keep its per-frame decode point
on its owning thread while the runtime owns the socket. The generator
question is settled (phux-mobile ADR-0031 keeps UniFFI), the mobile shim
lives in this repository, and the C ABI carries an embedded lane and a
connected lane that must never run at once.

Status: Accepted
Date: 2026-09-21

## Context

ADR-0133 was written before its rungs existed. Its body is immutable
(docs/CONVENTIONS.md), and two landings amended it in place; this ADR
carries those amendments and the accepted body is restored.

- phux-mobile keeps a standalone `TerminalEngine` for its local playground
  and tests. It hosts an engine and projects a grid without any connection.
  Decision 2's "no state machine" read as forbidding it; the intent was to
  forbid a second *connection* state machine.
- `phux-client-ffi`'s per-frame behavior — retired-close suppression,
  bootstrap-profile validation, agent-generation tracking — reads workspace
  subscription state that only the embedder's owning thread may touch. A
  connected driver that decoded frames on its own thread would move that
  state across threads.
- The BoltFFI / direct `client.h` / UniFFI spike ran on the thin shim and
  the POD grid (phux-mobile ADR-0031). The shim's source and its generated
  Swift now ship from this repository as one revision-pinned artifact.
- Cockpit moved onto the runtime's connected client (rung 10). Thirty-six
  Cockpit test files stage malformed frames through the embedded lane.

## Decision

1. **A binding crate holds no connected-client state machine.** A
   connection loop, a `select!` over a transport, or a backoff constant in
   a binding is in the wrong crate. A standalone terminal projector used
   only for a consumer's local playground or tests is outside that
   boundary; it still uses core's cell layout and defines no second one.
2. **The runtime owns each connected resource's engine thread and
   double-buffers its view.** ADR-0133 decision 4, restated per resource.
3. **A binding may keep the decode point without keeping the socket.**
   The runtime's connected driver retains inbound frames
   (`ControlOptions::deliver_inbound`) and the binding feeds them from its
   owning thread. Decision 1 forbids a second connection state machine,
   not a binding's own per-frame projection.
4. **The generator decision is closed.** phux-mobile ADR-0031 keeps UniFFI
   0.28: the cell storage crosses as byte arenas, so BoltFFI's zero-copy
   record path does not apply, and direct C would need another
   connected-runtime ABI plus two hand-written language adapters. The
   projection shim is `crates/phux-mobile-ffi`, so generated source and
   native artifact share one revision.
5. **The C ABI carries two lanes.** `phux_client_new` is the embedded lane,
   where the embedder owns the socket; `phux_client_connect` is the
   connected lane over `Runtime::connect`. Every consumer runs the
   connected lane. The embedded lane stays as the harness seam and the two
   are mutually exclusive on one handle at runtime.

## Why

Each refinement was forced by a real consumer, not by taste: the
playground projector exists and is useful; the C ABI's owning-thread
contract is what Cockpit's Zig relies on; the spike produced numbers; the
harness seam is how thirty-six test files inject faults. Recording them as
a superseding ADR keeps ADR-0133 as ratified and keeps the rule readers
apply — one connection state machine, in the runtime — exact.

## Tradeoffs

- Two lanes on one C ABI are two paths to keep mutually exclusive and two
  to document; the embedded lane exists for tests, not products.
- A per-consumer playground projector is a second engine host to keep
  honest: it must keep using core's cell layout.
- The workspace is twenty crates: the runtime carries the tokio and quinn
  edges; the mobile UniFFI crate projects it without state.

## Alternatives

**Amend ADR-0133 in place.** Rejected by docs/CONVENTIONS.md: the accepted
body is the record; readers must be able to trust it.

**Delete the embedded lane.** Rejected: it is the fault-injection seam for
Cockpit's contract tests, and the connected lane cannot stage a malformed
frame.

**Decode in the runtime for the C ABI.** Rejected: it moves owning-thread
state across threads for one consumer's convenience.
