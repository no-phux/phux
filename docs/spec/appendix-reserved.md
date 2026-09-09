---
audience: consumers, contributors, agents
stability: stable
last-reviewed: 2026-08-02
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

- `WORKLOAD_RESPONSE = 0x04` and `WORKLOAD_CHALLENGE = 0x84` are allocated
  to the endpoint-neutral `phux-workload/v1` profile
  ([workload-auth.md](./workload-auth.md)); `0x05..=0x0F` and
  `0x85..=0x8F` remain open for connection lifecycle.
- `0x14` is allocated, `0x15` is retired, `HISTORY_REQUEST = 0x16` and
  `INPUT_TERMINAL_REPLY = 0x17` are allocated, and `0x18..=0x1F` remain open.
  `0x91` is permanently retired.
  `BOOTSTRAP_BEGIN..BOOTSTRAP_TOMBSTONE = 0x93..=0x97`,
  `HISTORY_TOMBSTONE = 0x98`, and `HISTORY_REJECTED = 0x99` are allocated;
  `FRAME_COMPRESSED = 0x9A` (proto.md §6.4) is allocated from the hot-path
  reserve, which it belongs in: it wraps the hot path's largest frames.
  `0x9B..=0x9F` remain open for hot-path messages.
- Message IDs `0x24..=0x2F` and `0xA3..=0xAF`: reserved for further L1
  Terminal lifecycle / per-pane control frames (phux-4li.10 allocated
  `0x22..=0x23` C→S and `0xA1..=0xA2` S→C from these ranges; ADR-0056
  allocated `MOVE_TERMINAL = 0x2A` and `TERMINAL_MOVED = 0xA8`). The
  `SPAWN_PROCESS` / `KILL_PROCESS` / `PROCESS_SPAWNED` / `PROCESS_CLOSED` /
  `PROCESS_OUTPUT` family once pencilled into `0x24..=0x25` /
  `0xA3..=0xA5`, and the `FORWARD_PORT` / `CLOSE_PORT_FORWARD` /
  `PORT_FORWARD_STATUS` family into `0x28..=0x29` / `0xA6`, are subsumed by
  `ResourceKind` ([L1.md §1.1](./L1.md)): a non-PTY process or a forwarded
  port is a kind served through the existing spawn / output / close frames,
  not a parallel frame family. Those discriminants stay unallocated.
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
| `0x15` | `PUT_FILE`                  | [L1.md](./L1.md) | shipped |
| `0x16` | `SHUTDOWN`                  | [L1.md](./L1.md) | shipped |
| `0x17` | `REPORT_AGENT_STATE`        | [L1.md](./L1.md) | shipped |
| `0x18` | `GET_PERF`                  | [L1.md](./L1.md) | shipped |
| `0x19` | `TRANSCRIBE`                | [L1.md](./L1.md) | shipped |
| `0x1a` | `APPEND_RESOURCE_OUTPUT`    | [L1.md §5.5](./L1.md) | shipped |

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

`ErrorCode = 5` is permanently reserved for the withdrawn `OUT_OF_TIER`
proposal and is never reused. `CODEC_UNAVAILABLE = 6` is allocated by ADR-0070.
`WRONG_RESOURCE_KIND = 208`, `NOT_PRODUCER = 209`, `RECORD_INVALID = 210`,
and `OVERFLOW = 211` are allocated by the resource model
([proto.md §9](./proto.md); [L1.md §1.1, §5.5](./L1.md)).

`SpawnError` ([L1.md §3.1](./L1.md)) allocates sequentially from `0x00`:
`0x00..=0x03` are the group / spawn / satellite codes and `0x04
UNSUPPORTED_KIND`, `0x05 PARENT_NOT_FOUND`, `0x06 PARENT_KIND_MISMATCH` are
the resource-binding codes ([L1.md §1.2](./L1.md)). `ResourceKind` allocates
`TERMINAL = 0` and `AGENT_SESSION = 1`; a tag once allocated is never reused,
and a decoder maps an unallocated tag to `Unknown { tag }` ([L1.md §1.1](./L1.md)).
`BootstrapCodec` ([L1.md §4.3](./L1.md)) allocates `0` (synthesized VT v1),
`1` (native, followed by the engine version byte), and `3`
(`AgentEventsJsonlV1`); `2` is skipped so the codec tag never shares a value
with the native v2 version byte that follows tag `1` in a dump.

`CloseReason` ([L1.md §1.2](./L1.md)) allocates sequentially from `0` —
`0..=3` are taken — and follows the `DetachReason` decode rule below: an
absent field and an unallocated value both read as an *unstated* reason.

`DetachReason` ([proto.md §7.2](./proto.md)) allocates sequentially from `0`
— `0..=7` are taken, with workload-auth values `5..=7` still spec-only — and
`255` is permanently reserved for `INTERNAL_ERROR`.
It differs from the enums above in how an unallocated value decodes: a
consumer MUST read one it does not recognise as an *unstated* reason rather
than as a decode error, because `DETACHED` is the termination signal and
failing it would both hide the ending and make each new value a fleet-wide
break. New values are therefore additive and need no version bump.

(Earlier drafts of the SPEC reserved a `DiffOp` tag range here; per
[ADR-0013](../../ADR/0013-libghostty-bytes-on-wire.md), Terminal
content is now a VT byte stream and `DiffOp` no
longer exists as a wire concept.)
