---
audience: contributors
stability: stable
last-reviewed: 2026-07-24
---

# 0053 — Acknowledged idempotent input batches

**TL;DR.** Add one bounded `APPLY_INPUT` command under the existing command
envelope. A consumer supplies a random operation id and an ordered input batch;
the server encodes it atomically, writes and flushes it as one PTY job, and
caches the result. Reconnect retries reuse the operation id. Legacy interactive
input remains fire-and-forget.

Status: Accepted
Date: 2026-07-24

## Context

The existing `INPUT_*` frames and `ROUTE_INPUT` command are intentionally
fire-and-forget. A consumer can know that a pane was writable immediately before
queueing, but not whether input crossed a failing transport, entered the pane
mailbox, or reached the PTY. Replaying after reconnect may duplicate a command;
not replaying may silently lose it. Terminal echo is not a receipt: echo may be
disabled, a TUI may repaint, and a command may produce no output.

This ambiguity is unacceptable for a remote consumer that presents a composed
command as sent. It cannot be repaired with `COMMAND.request_id`, which is only
connection-local reply correlation and is remapped by federation.

## Decision

1. Add `APPLY_INPUT` as a new L1 `Command` variant under the existing
   `COMMAND` / `COMMAND_RESULT` envelope. Do not add a second result frame
   family and do not change `ROUTE_INPUT` semantics.
2. Each operation carries a non-zero, consumer-generated 128-bit random id, one
   local `TerminalId`, and an ordered batch of existing structured `InputEvent`
   atoms. The exact wire shape and bounds live in
   [`L1.md`](../docs/spec/L1.md).
3. The server validates every event, captures one terminal-mode snapshot,
   encodes the complete batch before writing anything, and submits the result as
   one job through a bounded PTY-writer queue. `OK` is emitted only after
   `write_all` and `flush` succeed. Unsafe paste, authority, capacity, and
   encoding failures before handoff refuse the whole operation.
4. The terminal-owning server caches the canonical payload digest and final
   result by operation id. A same-id, same-payload retry returns the cached
   result without writing again; same-id, different-payload reuse is invalid.
   Accepted and indeterminate results remain cached for ten minutes, up to
   65,536 entries. A refusal before PTY handoff keeps only the id-to-digest
   binding, so the unchanged operation may be evaluated again after repair but
   the id can never name different input.
5. `HELLO_OK.server_id` is a random 128-bit server-incarnation id. Dedupe state
   is process-memory state; any restart or re-exec that loses it also changes
   the incarnation. A client may automatically retry an unresolved operation
   only against the same incarnation and inside the retry horizon. Otherwise it
   reports delivery as unknown and does not replay.
6. A PTY write that fails, loses its completion signal, or exceeds the bounded
   completion wait returns `INPUT_DELIVERY_UNKNOWN`. The server caches that
   result and never retries the operation. This avoids duplicates without
   claiming delivery.
7. Version one is local-terminal only. A satellite-tagged target returns
   `UNSUPPORTED_SATELLITE_ROUTE`; federation needs destination capability
   negotiation and destination-owned dedupe before it can preserve this
   contract.
8. Latency-sensitive interactive `INPUT_*` and existing `ROUTE_INPUT` remain
   fire-and-forget. Consumers use `APPLY_INPUT` for atomic submit, paste, agent
   answers, and other actions where honest completion matters.

## Why

- The command envelope already owns correlation, typed errors, and concurrent
  event interleaving. A new acknowledgment frame would duplicate those rules.
- A separate operation id survives reconnect while `request_id` remains free to
  change per connection.
- One encoded PTY job prevents partial validation and interleaving between the
  batch's events.
- Caching indeterminate outcomes is conservative: it may leave the user with an
  explicit unknown, but it never turns uncertainty into a duplicate write.
- A changing server incarnation makes loss of volatile dedupe state observable
  instead of silently weakening idempotency after restart.

## Tradeoffs

- The guarantee is bounded, not durable. A process crash may leave delivery
  unknown; the client must not replay against the new incarnation.
- Waiting for PTY completion adds latency versus fire-and-forget input. The wait
  is bounded at five seconds; a stalled writer becomes
  `INPUT_DELIVERY_UNKNOWN` rather than wedging shutdown. It is served off the
  input lane, so a stalled write costs its own operation and nothing else
  (phux-w7z2.58; the first implementation blocked the shared lane and stalled
  input to every pane for the length of the wait).
- The dedupe cache consumes bounded process memory and rejects unseen ids when
  full rather than evicting an unexpired safety record.
- The lane admits one acknowledged operation per Terminal (phux-w7z2.58;
  originally one per server). A concurrent submit to the same Terminal is
  refused with `RESOURCE_EXHAUSTED`; the caller retries rather than retaining
  another batch behind a stalled PTY write. Distinct Terminals do not exclude
  each other.
- The first version does not support satellite targets. Refusing them is less
  convenient than relay support, but safer than advertising a guarantee an
  older destination cannot negotiate.
- `OK` means bytes were written and flushed to the PTY master. It does not mean
  the foreground program consumed them or that a shell command completed.

## Alternatives

- **Replay legacy `INPUT_*` after reconnect.** Rejected: a transport failure
  cannot reveal whether the first copy reached the PTY, so replay duplicates.
- **Infer delivery from output or echo.** Rejected: it fails for disabled echo,
  TUIs, control input, and commands with no output.
- **Overload `ROUTE_INPUT`.** Rejected: older servers could accept the first
  event and ignore an additive tail, producing silent partial execution; its
  established fire-and-forget behavior also remains useful.
- **Use `request_id` for dedupe.** Rejected: it is scoped to one connection and
  may be remapped by intermediaries.
- **Persist a write-ahead ledger across crashes.** Deferred: coupling durable
  storage transactionally to PTY writes is disproportionate for the first
  remote-consumer guarantee. Incarnation changes preserve honesty meanwhile.

## Related

- ADR-0021 — control-plane command envelope.
- ADR-0024 — protocol-owned structured input atoms.
- ADR-0033 — input authority leases.
- ADR-0044 — dedicated input lane.
- Bead: phux-m3cb.
