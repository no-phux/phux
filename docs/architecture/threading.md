---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-12
---

# Threading and I/O

**TL;DR.** libghostty's `Terminal` is `!Send`, so resource engines run as
`spawn_local` tasks on a current-thread tokio runtime and `LocalSet`.
`ServerState` uses a `std::sync` mutex shared with the input lane, tests,
and embedded callers. No lock is held across an await. Input routing and
encoding run on a separate OS thread.

---

## One current-thread runtime with a LocalSet

The server polls file descriptors and fans out bytes on one tokio
current-thread executor. tokio supplies the Unix-socket, signal, and frame-codec
integrations this needs (`tokio-uds`, `signal-hook-tokio`, `tokio-util`).

libghostty's `Terminal` is `!Send`: it cannot move across threads. Tasks that
feed and read it therefore run on a `LocalSet` pinned to the runtime thread,
rather than as tasks on a multi-threaded executor.

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
[ADR-0102](../adr/0102-resources-the-server-serves-kinds.md)). The core
owns the output sequence, the output broadcast sender, the event-subscriber
list, the cancel token, and the control mailbox, and it adds no shared cells
across tasks — its one `RefCell` (the subscriber list) is borrowed only by
the task that owns the core. The placement rule is the engine's to keep: an
engine that owns `!Send` state runs as that one task and is the sole
borrower of the state. The Terminal engine (`resource::terminal::
TerminalActor`) is why the rule exists; it holds the `Terminal` in a
`RefCell` that no other task ever touches. The AgentSession engine
(`resource::agent_session`) is the second engine on the same LocalSet
and cancellation tree; it owns no `!Send` state.

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

Server state lives behind an `Arc<Mutex<ServerState>>` from `std::sync`, not
`tokio::sync`. The mutex gives runtime tasks, tests, and embedded callers one
consistent view while the `!Send` engines stay on the runtime thread.

The lock is never held across an `.await`. Each acquisition is a short
critical section: take the lock, read or mutate `ServerState`, drop the lock,
then await any I/O. Holding a `std::sync::Mutex` across a yield point risks
deadlocking the single thread. Group operations such as `KILL_RESOURCES`
apply all-or-nothing under one acquisition. The Terminal exit path likewise
gathers subscribers, reaps the domain entity, and forgets the table entry
under one lock; only the `RESOURCE_CLOSED` sends are awaited afterward
([data-model.md](./data-model.md)).

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

Other per-Terminal work (PTY feed, outbound capability rewriting, bootstrap
compression) could follow the same rule if a profile demands it; none does
today.

## Status

No remaining target-versus-shipped gaps.

| Gap | Today | Owner | Tracked |
|---|---|---|---|
