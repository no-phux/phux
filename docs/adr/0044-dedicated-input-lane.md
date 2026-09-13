---
audience: contributors
stability: stable
last-reviewed: 2026-07-15
---

# 0044 — Dedicated input lane

**TL;DR.** Local input routing and encoding run on one dedicated OS thread from actor-published `Send` terminal-mode snapshots. A keystroke is gated and encoded in parallel with output work; the `!Send` `Terminal` stays actor-owned and receives bytes through a bounded non-blocking mailbox.

Status: Accepted
Date: 2026-07-11

## Context

ADR-0003 gives the server one current-thread tokio runtime; ADR-0014 puts
every per-client task and every `!Send` `Terminal`-owning pane actor on one
`LocalSet`. They time-share a single core. A large PTY-output broadcast tick
and an inbound keystroke therefore contend for the same thread.

Commit 284fcbd bounded that contention *inside* the pane actor's `select!`
(input arm biased ahead of output; output coalesce capped at 48KB and
yielding). But the **routing** stage that runs *before* the mailbox — resolve
the wire pane id, walk the subscription set, check the input lease (ADR-0033),
`try_send` onto the actor's mailbox — still executed on the main thread, behind
whatever output work the runtime was mid-poll on. The keystroke's route waited
for a yield point.

The original lane moved only routing. Encoding still read live terminal state
on the actor, leaving half the latency path behind the output parser. phux-51n6.6
adds the exact `Send` state boundary needed to move that remaining stage.

## Decision

Spawn one dedicated OS thread — the input lane — at server start. Each per-client
read loop, on decoding a **local** `INPUT_*` frame or `ROUTE_INPUT` command,
hands a `Send` `RoutedInput` (client id, wire pane id, decoded event, authority
policy, and for commands a reply sender) to the lane over a bounded channel
instead of routing inline. The lane blocks on that channel and runs the same
existing subscription/lease or headless lease handler off the main runtime.
The pane actor and its `Terminal` remain unmoved. After construction, PTY
output batches, resize, and restored-seed replay, the actor publishes a `watch`
snapshot containing every value read by the encoders: libghostty's exact key
options, effective mouse tracking/format, DEC 1004/2004, grid dimensions, and
cell pixels. The lane owns one stateful key/mouse/focus/paste encoder set per
generational pane and applies those snapshots. It hands encoded bytes to the
actor through a bounded mailbox with `try_send`; the actor only forwards them
to its PTY writer bridge.

The exact key and mouse capture/apply API is pinned in libghostty-rs. In
particular, modifyOtherKeys and mouse mode precedence come from libghostty's
own terminal-derived options, not a phux reimplementation.

This is sound because the crossed state is fully `Send`: `SharedState` is
`Arc<Mutex<ServerState>>`, snapshots are copyable values, and encoded bytes use
a `Send` `mpsc::Sender<Vec<u8>>`. Making the original lane compile required
one wart fix: dead `UnsubscribeFromEventsRequest` carried a
`*const Sender<Outbound>` (raw
pointer, `!Send`) that poisoned the whole state tree; it is now a `usize`
address with identical identity semantics.

Satellite-tagged pane ids stay on the main thread (their delivery is a hub-link
relay, not a mailbox send). Tests that drive `handle_client` directly pass
`None` and route inline — identical behavior, on-thread. Local `ROUTE_INPUT`
waits for the lane's correlated result before its command acknowledgement is
emitted, preserving its existing error and lease-check semantics.

## Why

- **Root, not per-`select!`.** 284fcbd made the actor fair once input is in the
  mailbox; this makes route and encode finish off the contended thread. The
  actor's biased `select!` only forwards the ready bytes.
- **The `!Send` constraint is respected, not fought.** The `Terminal` never
  moves; only provably-`Send` routing state, snapshots, and bytes cross.
- **Lease and ordering correctness are preserved by construction** (below), so
  no new correctness surface is opened.

## Tradeoffs

- **Snapshot publication is eventual at actor mutation boundaries.** Modes are
  published after each bounded output batch, seed, and resize. Input racing an
  unparsed PTY chunk observes the previous snapshot, matching the old actor
  scheduling possibility where biased input ran before that output chunk.
- **One extra OS thread** for the server's lifetime. It parks on the channel
  (no busy-wait) and joins on shutdown.
- **A same-client input-vs-lease timing shift.** A client's `INPUT_*` now
  routes on the lane while its `ACQUIRE_INPUT`/`RELEASE_INPUT` (an L2 `COMMAND`)
  still runs inline, so their relative order is no longer strictly wire-ordered.
  The lease gate is re-evaluated atomically under the state `Mutex` at delivery,
  so every outcome stays legal under the fire-and-forget input contract
  (SPEC §12.2): a key racing just past its sender's own `RELEASE_INPUT` is
  delivered if the wheel is free and dropped if another client grabbed it.
  Cross-client exclusion never weakens.
- **Per-client input order is unchanged**: the read loop enqueues `INPUT_*` and
  local `ROUTE_INPUT` in wire order, the channel and lane are FIFO, and the
  mailbox is FIFO. Both input surfaces share this one queue.

## Alternatives

- **Make the whole runtime multi-threaded.** Rejected: the `Terminal` is
  `!Send` (ADR-0003/0014); a work-stealing pool cannot host the actor.
- **A second full tokio runtime.** Unnecessary: routing and encoding are
  synchronous (non-blocking `try_send`, no await), so a plain thread with `blocking_recv`
  is simpler and has no runtime-in-runtime hazards.
- **Only strengthen the actor `select!` priority.** Already done (284fcbd); it
  cannot address contention *before* the mailbox, which is on the main thread.
- **Mirror the lease/subscription state into a separate `Send` structure the
  lane owns.** Rejected: two sources of truth for the lease is exactly the
  correctness surface this ADR avoids by reusing shared destination-resolution
  helpers under the one `Mutex`.
