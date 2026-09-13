---
audience: contributors
stability: stable
last-reviewed: 2026-05-28
---

# 0014 — Server-side `Terminal` placement: per-pane PaneActor on a `LocalSet`

**TL;DR.** Each pane is owned by a per-terminal actor: a single `spawn_local` task on the server's current-thread runtime that holds the libghostty `Terminal`, its `SnapshotSynthesizer`, and its PTY master. No other task ever borrows the `Terminal`. Cross-task coordination uses mpsc for unicast and broadcast for `PANE_OUTPUT` fanout. Preserves ADR-0003's one-event-loop invariant.

Status: Accepted
Date: 2026-05-25
See [ADR-0016](./0016-terminal-id-as-wire-primary.md) for the rename: PaneActor, PaneId, PaneHandle, and PaneInput below are TerminalActor, TerminalId, and the matching Terminal* types in the tree.
See [ADR-0102](./0102-resources-the-server-serves-kinds.md) for the actor's current scope: it is the Terminal engine inside a kind-agnostic resource core.

## Context

ADR-0013 places a libghostty `Terminal` on the server: PTY bytes flow
in via `Terminal::vt_write`, snapshots flow out via `RenderState` /
`grid_ref`, and per-client downsampling rewrites the resulting byte
stream. That leaves one question unanswered: **where does the
`Terminal` live inside the server process?**

The constraint is hard. `libghostty_vt::Terminal<'alloc, 'cb>` holds
an `Object<'alloc, ffi::TerminalImpl>` wrapping `NonNull<TerminalImpl>`
(`libghostty-vt::alloc.rs`). There is no `unsafe impl Send` or
`unsafe impl Sync` anywhere in the crate. `RenderState`, `RowIterator`,
and `CellIterator` (the types `SnapshotSynthesizer` reads from) inherit
the same `!Send + !Sync` shape. The renderer side cannot cross a
thread boundary.

The server today runs `tokio::runtime::Builder::new_current_thread`
(ADR-0003: "one scheduler, one event loop, one source of truth").
Per-client connection handlers are spawned with `tokio::spawn`
(`crates/phux-server/src/runtime.rs:258`), which currently requires
`Send + 'static` futures — and is why `ServerState` lives behind
`Arc<Mutex<_>>` even though every task runs on one thread
(`crates/phux-server/src/state.rs:21–38` documents this deferral and
names this ADR's flip as the planned resolution).

The use cases that pin the shape:

- **byc.8 ATTACH handler.** On attach, call
  `SnapshotSynthesizer::synthesize(&terminal)` on the target pane and
  ship the resulting `vt_replay_bytes` as `PANE_SNAPSHOT`.
- **byc.5 PTY pump (not yet implemented).** A loop that reads PTY
  bytes, calls `terminal.vt_write(&bytes)`, and forwards the same
  bytes to subscribed clients as `PANE_OUTPUT`. `portable-pty 0.8`
  is in `Cargo.toml` but unused.
- **byc.6.4 multi-client fanout.** Two clients attached to one pane
  share the same `Terminal`; both subscribe to its `PANE_OUTPUT`
  stream; their inputs converge into the pane's input log
  (`crates/phux-server/src/state.rs:147`).

Two patterns were considered: a `tokio::task::LocalSet` with
`spawn_local` (so `!Send` futures are legal), or a dedicated OS
thread per pane behind an actor channel.

## Decision

The server runs a `tokio::task::LocalSet` on its existing
current-thread runtime. Each pane is owned by a **PaneActor**: a
single `spawn_local` task that holds the pane's `Terminal`, its
`SnapshotSynthesizer`, and its PTY master, and drives a `select!`
loop over its input channels. No other task ever borrows the
`Terminal`. Cross-task coordination uses `tokio::sync::mpsc` for
unicast (input, snapshot requests) and `tokio::sync::broadcast` for
`PANE_OUTPUT` fanout to subscribed clients.

The single `tokio::spawn` at `runtime.rs:258` becomes `spawn_local`.
An audit confirms this is the only non-test spawn site in
`crates/phux-server/`; `tests/socket_lifecycle.rs:40` keeps
`tokio::spawn` because tests own their own runtime.

### Actor shape (illustrative)

```rust
struct PaneActor {
    terminal: Terminal<'alloc, 'cb>,
    synth:    SnapshotSynthesizer,
    pty:      portable_pty::Master,
    input:    mpsc::Receiver<PaneInput>,
    snapshot: mpsc::Receiver<SnapshotRequest>,
    output:   broadcast::Sender<Bytes>,
    shutdown: oneshot::Receiver<()>,
}

impl PaneActor {
    async fn run(mut self) {
        loop {
            tokio::select! {
                bytes = self.pty.read() => {
                    self.terminal.vt_write(&bytes);
                    let _ = self.output.send(bytes);
                }
                Some(input) = self.input.recv() => {
                    self.pty.write(input.bytes).await;
                }
                Some(req) = self.snapshot.recv() => {
                    let snap = self.synth.synthesize(&self.terminal);
                    let _ = req.reply.send(snap);
                }
                _ = &mut self.shutdown => break,
            }
        }
    }
}
```

byc.8's ATTACH handler sends a `SnapshotRequest { reply: oneshot }`
to the target pane's actor, awaits the reply, ships
`PANE_SNAPSHOT`, then subscribes the client's outbound mailbox to
the pane's `broadcast::Sender<Bytes>` for live `PANE_OUTPUT`.

## Rationale

- **One borrower, no `RefCell`.** Only the PaneActor ever holds the
  `Terminal`. No `Rc<RefCell<Terminal>>` juggling, no runtime-borrow
  panic surface, no aliasing reasoning required at call sites.
- **Smallest delta from today.** One `tokio::spawn` → `spawn_local`.
  `Arc<Mutex<ServerState>>` stays for the cross-task bits. The
  existing per-client task shape is preserved — it just loses its
  `Send` bound.
- **ADR-0003 invariant preserved.** One scheduler, one event loop.
  No new threads, no new schedulers built on top of tokio's
  scheduler.
- **Zero-cost snapshot synth path.** `SnapshotSynthesizer::synthesize`
  is a direct `&Terminal` borrow inside the actor — no channel hop,
  no copy, no serialization. The oneshot reply only carries the
  resulting `Bytes`, which is `Send` and ships cheaply.
- **Multi-client fanout is orthogonal.** `tokio::sync::broadcast` for
  `PANE_OUTPUT` and per-client `mpsc::Sender<OutboundFrame>` for
  everything else. byc.6.4 falls out of this shape; the actor doesn't
  know how many subscribers it has.
- **PTY ergonomics.** `portable-pty` + tokio's `AsyncFd` already
  multiplex PTY fds at the OS level. A centralized I/O reactor would
  duplicate that work; per-pane `spawn_local` keeps the loop
  co-located with the `Terminal` that consumes its output.

## Rejected alternatives

### Option B: dedicated OS thread per pane (actor on `std::thread`)

A pane's `Terminal` lives on a `std::thread` that owns it; async tasks
on the tokio side communicate via mpsc/broadcast. Same channel shape
as the chosen design — the difference is where the executor lives.

Rejected because:

- **Hypothetical parallelism.** Typical phux sessions have 1–20
  panes. Per-pane OS threads buy parallelism that nothing in the
  workload demands. If single-core saturation ever becomes real, the
  right move is sharding sessions across multiple current-thread
  runtimes, not introducing per-pane threads under one runtime.
- **Worse testability.** `tokio::test(start_paused = true)` works
  cleanly with `LocalSet`. `std::thread` actors require real wall
  clock and `std::thread::sleep`, which flakes. byc.6.* integration
  tests are async-channel-shaped already.
- **Lifecycle and panic surface.** Each thread needs explicit
  `Shutdown` plumbing and a join path; a panic in the actor must not
  leave waiting oneshots dangling. The `LocalSet` version inherits
  the runtime's existing shutdown and panic propagation.
- **ADR-0003 friction.** "One event loop" is the invariant we keep
  citing back at ourselves. Per-pane threads erode it without
  delivering a load-bearing benefit.

### Option C: `Rc<RefCell<Terminal>>` shared across multiple `spawn_local` tasks

A separate PTY-pump task and a separate snapshot-synth path each
borrow the same `Terminal` through a `RefCell`. Rejected because it
trades one (well-understood) invariant — single borrower — for two
runtime-checked borrow paths whose only purpose is to split work
that wasn't actually parallel to begin with.

## Consequences

- Per-client and per-pane task code paths are `!Send` throughout the
  server. Any future use of `tokio::spawn` in `phux-server` is a
  smell — the seam to audit if a `Send` bound shows up unexpectedly.
- `phux-byc.5` becomes "implement `PaneActor::run` with PTY read,
  input write, snapshot request, and shutdown branches."
- `phux-byc.8` becomes "ATTACH handler sends `SnapshotRequest` to
  the target pane's actor and subscribes the client to the pane's
  broadcast channel." It does **not** borrow the `Terminal` directly.
- `ServerState` keeps `Arc<Mutex<_>>` for cross-task bits; gains a
  `panes: HashMap<PaneId, PaneHandle>` where `PaneHandle` holds the
  mpsc/broadcast senders for one actor. The handle is `Send`; the
  actor it points at is not.
- Federation (ADR-0007 / phux-nol) is unaffected. Cross-host panes
  are process-to-process over QUIC, not thread-to-thread; the
  PaneActor shape adapts at the hub layer the same way Option B
  would have.

## References

- ADR-0003 — single server, one event loop (invariant this ADR
  preserves).
- ADR-0013 — libghostty bytes on the wire (the pivot that made this
  question live).
- `research/2026-05-25-libghostty-renderstate.md` — `!Send`/`!Sync`
  audit of libghostty-rs surface.
- `crates/phux-server/src/runtime.rs:258` — the single
  `tokio::spawn` → `spawn_local` flip.
- `crates/phux-server/src/state.rs:21–38` — the deferral note this
  ADR resolves.
- `crates/phux-server/src/grid.rs` — `SnapshotSynthesizer`, the
  primary `&Terminal` consumer.
- bd: `phux-28f` (decision), `phux-byc.5` (PaneActor impl),
  `phux-byc.8` (ATTACH handler).
