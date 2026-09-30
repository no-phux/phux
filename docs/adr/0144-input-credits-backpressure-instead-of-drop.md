---
audience: contributors
stability: stable
last-reviewed: 2026-09-30
---

# 0144 — Input credits: backpressure instead of drop

**TL;DR.** Typed input is never dropped because a pane is busy. Each Terminal
owns 64 input credits; every request bound for its encoded-input mailbox holds
one until the PTY writer has written it. A connection's read loop takes the
credit before handing an event to the input lane and, while the pane is
saturated, waits: it reads no further frame, so the transport's flow control
pushes back on the sender. The lane never waits. A pane that drains nothing
for five seconds refuses input with an explicit `RESOURCE_EXHAUSTED`.

Status: Accepted
Date: 2026-09-30

## Context

ADR-0044 moved routing and encoding onto the input lane, which hands encoded
bytes to a pane actor through a bounded mailbox with `try_send`. The spec
called the input fire-and-forget, so every full queue on the path dropped the
event with a `warn!`: the lane's own queue, the pane's 64-slot mailbox, and
the actor's queue to the PTY writer thread. A dropped keystroke is invisible
on the wire, and a dropped interior of typed text leaves the shell a
different, valid command.

The drops were not theoretical. When stream framing began handing out
buffered frames with no await point, one socket read carrying a typed line
filled the mailbox before the actor, on the same runtime thread, could drain
it, and everything past 64 keys vanished. Restoring a yield per buffered frame
restored pacing, but pacing is scheduling, not a guarantee: many flooding
panes on the one runtime thread, or a program slow to read its terminal, still
fill the queues.

## Decision

- **Credits, per Terminal.** A `TerminalHandle` carries a pool of
  `INPUT_CREDITS` (64, the mailbox depth). An `EncodedInputRequest` holds a
  credit from the moment its sender takes it until the request is dropped:
  after the writer thread's `write(2)`, or wherever it is discarded. Credited
  requests in flight never exceed the mailbox depth, so the lane's `try_send`
  always finds room.
- **The writer queue keeps credited slots.** The actor forwards requests to the
  PTY writer thread over a queue of `INPUT_CREDITS + 16`. Uncredited sends (the
  terminal's own query replies, and input on the lane-less test path) go
  through `try_send_to_writer`, which refuses them once only the credited
  slots remain. A credited request is therefore never refused on this hop.
- **Senders wait; the lane does not.** A connection's read loop takes the
  credit for each lane-bound `INPUT_*` event and `ROUTE_INPUT` command before
  routing it, waiting when none is free. It holds no lock while waiting, and
  cancellation still preempts the wait. The lane's inbound queue is awaited
  (`send`, not `try_send`) for the same reason; the lane drains it
  unconditionally. `APPLY_INPUT` and voice input take a credit on the lane
  without waiting and are refused `RESOURCE_EXHAUSTED` before handoff if none
  is free; both are acknowledged and retryable.
- **A bounded wait, then an explicit refusal.** Waiting for a credit is
  bounded by `INPUT_STALL_LIMIT` (five seconds). A pane that drains nothing
  for that long refuses the event with an uncorrelated
  `ERROR(RESOURCE_EXHAUSTED)` for an `INPUT_*` frame, or that result for
  `ROUTE_INPUT`. The connection remembers the stall and refuses further input
  to that pane at once until a credit frees, so a wedged pane cannot freeze
  the connection for five seconds per key.
- **No silent refusal on the lane.** Input that reaches the lane without a
  usable credit (it raced a pane replacement) is refused the same way:
  `ROUTE_INPUT` gets the error, and an `INPUT_*` sender gets an uncorrelated
  `ERROR`.

## Why

- **The transport already has flow control.** Stopping the read loop turns a
  full pane into a full socket, and UDS, TCP, and QUIC all push back on the
  sender. No new wire signal is needed for the normal case.
- **Ordering is preserved by construction.** One read loop per connection
  takes credits in wire order, the credit pool is FIFO-fair, and the lane and
  mailbox are FIFO.
- **No deadlock.** Credits return when the writer thread finishes a write,
  which depends only on the child reading its terminal. The actor forwards
  without waiting, the lane never waits, and no waiter holds the state lock.
  A child that never reads is the one case that cannot drain, and the stall
  bound turns it into an explicit refusal.
- **Echo latency is untouched.** An uncontended credit is taken with
  `try_acquire` under one short state-lock read; the lane thread, its
  QoS promotion, and the actor's biased input arm (ADR-0044) are unchanged.

## Tradeoffs

- **A saturated pane stalls its connection.** While one pane is saturated, the
  connection reads nothing else, including input to other panes and control
  frames, for up to the stall bound. Other connections and other panes'
  delivery are unaffected. Head-of-line blocking per connection is the price
  of keeping one FIFO per connection.
- **Refusal is still loss, but visible.** After the stall bound an event is
  refused. The spec now says so on the wire, and callers that need an
  outcome per batch use `APPLY_INPUT`.
- **Credits are per Terminal, not per connection.** Several connections typing
  into one saturated pane share its 64 credits fairly; none can starve the
  others.

## Alternatives

- **A larger mailbox.** Moves the cliff; a stalled reader fills any bound.
- **Block the lane on a full mailbox.** One stalled pane would stall every
  pane's input, the failure ADR-0044's lane exists to prevent.
- **Per-pane backlogs on the lane with a retry wakeup.** Unbounded unless it
  also pushes back on senders, which is what credits do without the extra
  queue.
- **An input acknowledgement on the wire for every event.** A protocol change
  for every client, when transport flow control already carries the signal;
  `APPLY_INPUT` covers callers that need per-batch outcomes.
