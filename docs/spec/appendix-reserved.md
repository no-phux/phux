---
audience: consumers, contributors, agents
stability: stable
last-reviewed: 2026-06-06
---

# Appendix B — Reserved ranges

**TL;DR.** The reserved-discriminant ranges for future protocol
extensions: which message-ID slots are earmarked for which categories
(lifecycle, hot path, control plane, events, L1 lifecycle), the
command-tag allocations within the lifecycle range, and the
enum-allocation discipline for `PhysicalKey` and `ErrorCode`.
Implementers extending the protocol pick from these ranges via PR.

---

## 1. Reserved message-ID ranges

For implementers extending the protocol:

- Message IDs `0x04..=0x0F` and `0x83..=0x8F`: reserved for connection-
  lifecycle messages.
- Message IDs `0x14..=0x1F` and `0x93..=0x9F`: reserved for hot-path
  messages.
- Message IDs `0x24..=0x2F` and `0xA3..=0xAF`: reserved for further L1
  Terminal lifecycle / per-pane control frames (phux-4li.10 allocated
  `0x22..=0x23` C→S and `0xA1..=0xA2` S→C from these ranges).
- Message IDs `0x31..=0x3F` and `0xC2..=0xCF`: reserved for control
  plane.
- Message IDs `0x41..=0x4F` and `0xB3..=0xBF`: reserved for events
  (phux-y2t allocated `SUBSCRIBE_EVENTS = 0x41` C→S and `EVENT = 0xB3`
  S→C from these ranges; `0x42..=0x4F` and `0xB4..=0xBF` remain open).

## 2. Command-tag allocations

Commands ride the generic `COMMAND` envelope ([L1.md §1](./L1.md)) and carry
their own one-byte tag inside it. Allocated tags:

| Tag    | Command                     | Owner            | Status  |
|--------|-----------------------------|------------------|---------|
| `0x07` | `GET_SCREEN`                | [L1.md](./L1.md) | shipped |
| `0x08` | `ROUTE_INPUT`               | [L1.md](./L1.md) | shipped |
| `0x09` | `KILL_TERMINALS`            | [L1.md](./L1.md) | shipped |
| `0x0c` | `GET_TERMINAL_STATE`        | [L1.md](./L1.md) | shipped |
| `0x0d` | `SUBSCRIBE_TERMINAL_EVENTS` | [L1.md](./L1.md) | shipped |
| `0x0e` | `UPGRADE`                   | [L1.md](./L1.md) | shipped |
| `0x0f` | `ACQUIRE_INPUT`             | [L1.md](./L1.md) | shipped |
| `0x10` | `RELEASE_INPUT`             | [L1.md](./L1.md) | shipped |
| `0x11` | `SIGNAL_TERMINAL`           | [L1.md](./L1.md) | shipped |
| `0x12` | `REPORT_ASKED`              | [L1.md](./L1.md) | shipped |
| `0x13` | `DETACH_CLIENTS`            | [L1.md](./L1.md) | shipped |
| `0x14` | `APPLY_INPUT`               | [L1.md](./L1.md) | shipped |

`KILL_TERMINALS` at tag `0x09` reuses the slot freed by the removed
`CREATE_SESSION` command. Per
[ADR-0030](../../ADR/0030-engine-delegated-wire-and-projection-consumers.md)
(option B), the leaked session/collection lifecycle verbs are withdrawn and
their tags are freed:

- `0x09` — formerly `CREATE_SESSION`; reallocated to `KILL_TERMINALS`.
- `0x0a` — formerly `RENAME_SESSION`; freed, reserved, not reallocated.
  Rename is now an L3 metadata `SET` on `phux.session.name/v1`
  ([L3.md §3](./L3.md)).
- `0x0b` — formerly `KILL_COLLECTION`; freed, reserved, not reallocated.
  Group teardown is `KILL_TERMINALS`.

A freed tag SHALL NOT be reallocated to an unrelated command without a
`PROTOCOL_VERSION` bump, so that an old client speaking a withdrawn verb
fails loudly rather than invoking new behavior.

## 3. Reserved enum ranges

`PhysicalKey` enum values and `ErrorCode` enum values are allocated
sequentially. Implementers proposing new values open a PR against
this document.

(Earlier drafts of the SPEC reserved a `DiffOp` tag range here; per
[ADR-0013](../../ADR/0013-libghostty-bytes-on-wire.md), Terminal
content is now a VT byte stream and `DiffOp` no
longer exists as a wire concept.)
