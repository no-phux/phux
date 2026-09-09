---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-09
---

# Threading and I/O

**TL;DR.** Why the server runs on a single current-thread tokio runtime with
a LocalSet (libghostty's `Terminal` is `!Send`, so it cannot move across
threads), why every resource engine is one `spawn_local` task around a
`ResourceCore`, and why server state nonetheless lives behind an
`Arc<Mutex<ServerState>>` using `std::sync` — the lock is never held across
an await, so cross-task, test, and embed paths can share it without a
multi-threaded runtime. A multiplexer is I/O-bound; work-stealing buys
nothing on the hot path.

---

## One current-thread runtime with a LocalSet

A terminal multiplexer is I/O-bound, not CPU-bound: the work is
poll-many-fds-fanout-bytes. A single-threaded executor is simpler and fast
enough. We pick tokio over `mio` or `polling` because the ecosystem we need
(tokio-uds for Unix sockets, signal-hook-tokio for signals, tokio-util frame
codecs) is mature and not worth reinventing. The hot path gains nothing from
work-stealing, so the server does not use the multi-threaded runtime.

The current-thread choice is not only a performance call — it is forced by
the engine. libghostty's `Terminal` is `!Send`: it cannot move across
threads, so the tasks that feed and read it run on a `LocalSet` pinned to the
runtime thread. A multi-threaded runtime would refuse to spawn those tasks
at all.

```rust
fn main() -> std::io::Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?
        .block_on(phux_server::run())
}
```

## One task per resource

Each served resource is one `spawn_local` task: its engine, with the
generic `ResourceCore` embedded in it (ADR-0014, scoped by
[ADR-0102](../../ADR/0102-resources-the-server-serves-kinds.md)). The core
owns the output sequence, the output broadcast sender, the event-subscriber
list, the cancel token, and the control mailbox, and it adds no shared cells
across tasks — its one `RefCell` (the subscriber list) is borrowed only by
the task that owns the core. The placement rule is the engine's to keep: an
engine that owns `!Send` state runs as that one task and is the sole
borrower of the state. The Terminal engine (`resource::terminal::
TerminalActor`) is why the rule exists; it holds the `Terminal` in a
`RefCell` that no other task ever touches.

What crosses tasks is the `ResourceHandle`: `Send + Clone`, built by the
engine's constructor, stored in the `ResourceTable`, and cloned freely by
per-client tasks to subscribe to output, attach a consumer, or send control.
The Terminal facet inside it (`TerminalHandle`) is the same shape — channel
endpoints and two `u16`s — so the whole handle stays `Send`.

Cancellation is one tree: the per-server root token has a child per
resource, held in the `ResourceTable`, and the engines' futures sit in a
`JoinSet` on the same table. Cancelling the root cancels every engine;
dropping `ServerState` on the runtime thread aborts every future, which is
legal because the `JoinSet` is dropped on the thread that spawned its
`!Send` tasks.

## Shared state behind a std::sync Mutex

Server state lives behind an `Arc<Mutex<ServerState>>` from `std::sync` (not
`tokio::sync`). Both facts hold at once and do not contradict: the runtime is
single-threaded for the `!Send` engine, and the state is still wrapped in a
mutex so that multiple tasks — plus the test and embed paths that drive the
server in-process — share one consistent view.

The rule that makes a synchronous mutex safe on an async runtime is that the
lock is **never held across an `.await`**. Each acquisition is a short
critical section: take the lock, read or mutate `ServerState`, drop the lock,
then await any I/O. Holding a `std::sync::Mutex` across a yield point would
risk deadlocking the single thread; the discipline of dropping it first is
what keeps that from happening and what lets group operations such as
`KILL_RESOURCES` apply all-or-nothing under a single acquisition. The same
discipline is what the Terminal exit path relies on: gathering subscribers,
reaping the domain entity, and forgetting the table entry happen in one
critical section, and only the `RESOURCE_CLOSED` sends are awaited after
it ([data-model.md](./data-model.md)).

This reconciles the earlier server-design sketch, which described the state
as actor-owned: the shared-mutex shape is the one that ships, and it coexists
with the current-thread/LocalSet runtime rather than competing with it.

Because the state is shared with the input lane below, `ServerState` must be
`Send` (so `Arc<Mutex<ServerState>>` is `Send`). That is a real constraint on
what may live in it: message types reachable from a `ResourceHandle` (and
its `TerminalHandle` facet) cannot carry `!Send` payloads (a raw pointer, an
`Rc`). The event-unsubscribe request
identifies a subscriber by a `usize` address rather than a
`*const Sender<Outbound>` for exactly this reason.

## The dedicated input lane (ADR-0044)

Local input **routing and encoding** run on their own OS thread, the input
lane, not on the LocalSet. The Terminal engine publishes a copyable snapshot
after each output batch, seed replay, and resize. It contains libghostty's
exact key options, resolved mouse tracking/format, DEC 1004/2004, and
grid/cell geometry. The lane owns one stateful encoder set per generational
Terminal, applies the latest snapshot, and `try_send`s bytes through a
bounded engine mailbox. The engine's input arm only forwards those bytes to
the PTY writer. Only the Terminal kind has an input lane entry: input atoms
are a Terminal-facet operation.

The lane is a plain thread with a bounded channel and `blocking_recv`, not a
second tokio runtime: gating and encoding are synchronous, and both handoffs are
non-blocking. Per-client order is preserved because the read loop, lane channel,
single encoder thread, and encoded-byte mailbox are FIFO. Lease/subscription
semantics are unchanged because production and inline test paths share the same
destination-resolution helpers under the same `Mutex`. Satellite-tagged input
remains structured through the hub relay and is encoded by the destination
server's local lane.

## Hot paths that could go multi-threaded later

The input lane above is the first realized fan-out off the main thread. Others
can follow the same rule — cross only `Send` state, leave the `!Send` engine
put — if a future profile demands it:

- PTY-byte feed and per-client capability rewriting on outbound terminal
  frames. Each Terminal is independent and could move to `spawn_blocking` or
  a dedicated worker thread.
- Compression of large bootstrap bodies before transmission.

None but the input lane is parallelized today; the single-thread shape is
sufficient for the rest at the current scale.

## Status

| Gap | Today | Owner | Tracked |
|---|---|---|---|
| A second engine with no `!Send` state (the AgentSession record ring) on the same LocalSet and cancellation tree | Only the Terminal engine exists; `ResourceFacetHandle` has one variant. | [ADR-0103](../../ADR/0103-agent-session-resource-and-producer-fed-streams.md) | phux-am9y.9 |
