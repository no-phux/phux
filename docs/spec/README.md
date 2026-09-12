---
audience: consumers, contributors, agents
stability: stable
last-reviewed: 2026-09-12
---

# Protocol reference

**TL;DR.** The normative terminal wire. HELLO fixes version, layers, and
bootstrap; L1 is required, L3 is optional, L2 is unused. Start with the
tutorial to implement a consumer, or the catalogs to look up a frame.
Durable work is a different endpoint. Encoding and reserved appendices
are the codec source of truth.

---

The product model — what a Terminal is, why the wire is asymmetric, why
both ends run libghostty — lives in [CONCEPTS.md](../CONCEPTS.md). This
directory is the byte contract.

## Two-minute model

The wire is asymmetric. Server to client, terminal content is VT bytes
forwarded from the PTY. Client to server, input is structured key, mouse,
focus, and paste events. HELLO is the one negotiation: it admits
`major.minor` (this version: `0.9`), intersects layers, and selects one
bootstrap profile. After HELLO_OK those terms do not change.

L1 is required. L3 is optional and opted into via `HELLO.layers`. L2 is a
hole: the discriminant range is reserved and unused
([L2.md](./L2.md)). Grouping is L3 metadata plus client logic; atomic
multi-terminal teardown is the L1 `KILL_RESOURCES` op.

The coordinator is a different endpoint with its own HELLO, version, and
frame catalog ([coordinator.md](./coordinator.md)). It is not step 2 of
terminal onboarding. A client that only wants terminals never speaks it.

## Two reader paths

- **Implement a consumer.** Read [TUTORIAL.md](./TUTORIAL.md) once, then
  the specs each step links. That path is one terminal session: HELLO,
  attach, bootstrap, output, input, detach.
- **Look up a frame, tag, or error.** Use the catalogs in
  [proto.md](./proto.md), [L1.md](./L1.md), [L3.md](./L3.md), and
  [input.md](./input.md). Status cells are checked against the codec.

## Files

| File | Owns |
|---|---|
| [TUTORIAL.md](./TUTORIAL.md) | **Start here:** a complete session walkthrough (HELLO → attach → output → input → detach) |
| [proto.md](./proto.md) | Framing, version negotiation, capabilities, flow control, transport |
| [workload-auth.md](./workload-auth.md) | Endpoint-neutral `phux-workload/v1` proof, canonical endpoint-owned scopes, registry intersection, and live revocation |
| [coordinator.md](./coordinator.md) | Separate durable-work endpoint — authority, operations, snapshots, events, and Terminal bindings |
| [L1.md](./L1.md) | Terminal substrate — the REQUIRED conformance tier |
| [L2.md](./L2.md) | Reserved, unused — no collection tier (dissolved per ADR-0030) |
| [L3.md](./L3.md) | Metadata storage — OPTIONAL |
| [input.md](./input.md) | INPUT_KEY / INPUT_MOUSE / INPUT_FOCUS / INPUT_PASTE / INPUT_RAW |
| [appendix-encoding.md](./appendix-encoding.md) | Encoding primitives and the normative payload shape (positional, big-endian, length-prefixed) |
| [appendix-reserved.md](./appendix-reserved.md) | Reserved discriminant ranges |
| [CHANGELOG.md](./CHANGELOG.md) | Wire-format change log, version-stamped |

## Versions

The terminal protocol version lives in `crates/phux-protocol/src/` (grep
`PROTOCOL_VERSION`). The top entry in [CHANGELOG.md](./CHANGELOG.md) must match
it; CI gate `spec-version-sync` enforces this. The coordinator endpoint has an
independent version namespace and keeps its history in
[coordinator.md §16](./coordinator.md#16-coordinator-protocol-history); a
coordinator change does not bump the terminal protocol.
