---
audience: consumers, contributors, agents
stability: stable
last-reviewed: 2026-07-09
---

# Protocol reference

**TL;DR.** The normative phux wire protocols. Start with the terminal tutorial
for one complete connection; use proto/workload-auth/L1/L3/input for terminal
synchronization and authenticated endpoint admission; use coordinator.md for
the independently versioned durable-work endpoint. Encoding and reserved
appendices remain the source of truth for implementations.

---

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
