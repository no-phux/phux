# Compensating code: root causes R1 to R5

Part of [Compensating code in phux](./compensating-code.md), research at
`ed579facc7ec`. Paths are relative to the crate named nearby, or to
`crates/phux-server/src/` for bare `state/`, `runtime/`, `hub/` and `upgrade/`
paths. The [cluster index](./compensating-code-index.md) gives a start path
for every cluster.

Each section gives the design fact, what it costs today, the redesign, what
the redesign removes, and its price or what stays. R6 to R13 are in
[part two](./compensating-code-redesigns-2.md).

## R1. Terminal content is replicated by running a second emulator on a forwarded byte stream

**The fact.** The server holds the canonical libghostty grid. Each client
holds its own libghostty replica and is fed the PTY's raw VT bytes. Live
output travels through a lossy `tokio::broadcast` and a bounded
per-connection mailbox to a client that detaches on any skipped or repeated
sequence number. Server and client reflow independently on resize.

**What it costs (roughly 17,000 lines).**

- Server: gap detection, tombstones, capture and republish with retry
  clocks and backoff (`runtime/pump.rs:80-104`,
  `runtime/apps/terminal/attach/output_pump_resync.rs`), a writer-side
  generation fence because producers enqueue from different tasks
  (`runtime/connections/writer/generation.rs:12-15`), three attach gates
  and a rollback path (`runtime/session_attach/`), a resync requested by
  overloading the resize mailbox with `cols: 0, rows: 0`
  (`attach/bootstrap_queue.rs:228-243`).
- Engine: reverse-rendering the grid back into VT for attach, resync and
  upgrade, with known lossy edges (`phux-terminal/src/grid/synthesizer*`,
  about 1,300 lines); a 50 ms resize-resync debounce; a per-consumer
  rendered copy of the grid because libghostty's dirty bits are consumed by
  the first reader (`grid/synthesizer/incremental.rs:8-16`); the whole
  "terminal on loan" path because a native snapshot takes the terminal by
  value (`actor/canonical.rs:116-131`); cursor, generation and tombstone
  bookkeeping across three maps for history cuts that any reflow
  invalidates.
- Client: per-terminal generation and tombstone tables, staging limits,
  "retire the generation on any decode error" because the replica is not
  transactional (`phux-client-core/src/engine.rs:166-169`), an attach
  barrier with a ping/pong to manufacture cross-stream order, and
  "resync" implemented as dropping the socket and redialing.
- Hub: per-subscriber snapshot gates, detach barriers and retention
  budgets so one upstream generation can serve consumers that attached at
  different times (`hub/relay/`, about 2,400 lines).
- TUI: two geometries (server-owned grid, client-owned tile) reconciled on
  every paint. Web: the libghostty C ABI transcribed by hand across a wasm
  boundary (`clients/phux-vt-web/src/lib.rs:41-140`).

**Redesign: ship state, not the stream.** One replication contract. A
consumer holds version V of a terminal's visible state. The server sends the
difference from V to now. The consumer acknowledges. The server stamps each
row with the version at which it last changed, so any number of consumers
diff against their own acknowledged version with no per-consumer copy.
Scrollback is paged from the server by stable logical-line number. Modes the
client needs (mouse tracking, bracketed paste, alternate screen, cursor
style, prompt marks, exit status) are fields of that state.

The code already contains this as a second mode: per-consumer row diffs
against an acknowledged reference (`grid/synthesizer/incremental.rs`,
`actor/run_loop_tick.rs:226-251`). The redesign is to make it the only one.

**What disappears.** There is no gapless stream, so there is nothing to
tombstone: generations, the writer fence, gap-resync retries, the staleness
budget, attach gates and final-screen drain tokens all go. A bootstrap is a
diff from version 0 and a reattach is a diff from the last acknowledgement.
A slow consumer gets one larger diff. Resize is a diff at new dimensions, so
independent reflow, the debounce and "two geometries" go. The client replica
becomes plain data: no `!Send` engine thread in the client runtime, no wasm
ABI transcription, and mode facts stop being scraped from a replica
(`phux run` parsing `$?` off the screen, `phux-client/src/run.rs:1-18`).
The hub relays versions without gates.

**The price.** Selection, search and word boundaries currently belong to
the client's libghostty (a recorded decision). They either move to a small
grid-level implementation or stay in a client libghostty used only as a
container. Inline images need content-addressed blobs plus placements in the
state. The native VT client paints from diffs the way the TUI already does.
Bandwidth for full-screen churn is bounded by screen size per tick, which is
better than raw bytes under floods and worse for a slow trickle on a large
screen.

## R2. Several copies of server truth, joined by mirroring, with no single ordered history

**The fact.** A mutation commits to SQLite, then is mirrored into the
in-memory organization, while engines die on their own schedule. Registry
removal, journal entry, wire announcement and process death are four
separately timed states. Level reads carry no version; many pushes are
`try_send` under the state mutex and can be dropped; some writes
(`SET_METADATA`, resize, legacy rename) have no reply.

**What it costs (about 15,000 lines across server, client, TUI, Rune, CLI).**

- A close has several closers that must not double-emit: a close-reason
  ledger, an exit-watch ledger, closed-before-exit tombstones, drain tokens,
  "already reaped" branches (`state/bindings.rs:1-11`,
  `runtime/connections/pane_exit.rs`). Flush is approximated by yielding up
  to 256 times and then 8 more (`pane_exit.rs:397-411`).
- `persist_command_from` and `mirror_persisted` split, retirement
  suppression so mirroring does not put back deleted rows, and a startup
  reconciler that guesses each activation's fate from pid, start time and
  boot id (`state/org.rs`, `reconcile.rs`).
- Clients re-derive on any doubt: roster, topology and organization
  reseeds, poll floors under lossy notifications
  (`phux-client/src/agent_wait.rs:10-15`), read-back barriers after
  reply-less writes ("only an exact byte match proves this write won",
  `layout_ops/ops.rs:365-368`), a 3 s poll after kill.
- Four hand-maintained per-connection reset lists in the client runtime
  (`control/lifecycle.rs:365-473`), and epoch fences in every consumer.
- The work store's mission projector re-reads level state after each gap
  and holds an exit for 2.5 s in case a `done` record lands late.

**Redesign: one log.** Every server-visible fact changes only by an event
appended to one ordered journal, including "process exited" and "close
requested". State is the fold of that journal. Every snapshot carries the
sequence it was taken at. Every request gets a reply, and the reply carries
the sequence at which it committed. A consumer holds a cursor and receives
every later event in order, or one explicit "resnapshot at N". Durable
events live in the store; ephemeral ones (titles, agent state) share the
sequence space in memory.

The pure `apply`, the journal with cursors and the commit-before-publish
rule already exist for organization records. The redesign extends them to
everything and removes the side channels.

**What disappears.** A resource ends by one transition decided in one place,
so the closer ledgers and tombstones go. "Wait until my view has reached
sequence S" replaces read-back barriers, `GET_STATE` barriers, yield loops
and poll floors. A slow consumer falls behind a cursor instead of being
dropped under a lock, so owed-gap bookkeeping goes. Client reseed machinery
reduces to one resnapshot path. Autosave stops hashing the topology every
second.

**What stays.** Crash reconciliation of recorded OS processes is inherent
to keeping a process registry. Retry receipts for lost replies are inherent.

## R3. Three identities per entity, and wire ids that restart

**The fact.** A session or resource has a durable 16-byte key, an
in-process slotmap handle and a `u32` wire id. `phux-core` and
`phux-protocol` may not depend on each other, so the server owns every
translation. Wire ids restart at 1 on a cold start and can be reused.

**What it costs (about 4,000 lines plus roughly 280 call sites).**
`id_bridge.rs` (355), `state/id_space.rs` (303), `org_wire.rs` (449);
lazy interning and retirement of wire ids; two incarnation tokens
(`HELLO_OK.server_id` and `ServerInstance`); the conditional-kill provenance
ledger; `hub/operation_fence.rs` (370); delegated credentials that name a
session by wire id and are therefore all revoked by a cold restart
(`delegation.rs:12-20`); client requalification of attach targets by name;
hub retagging with `FOREIGN_CLIENT_BIT` and a zeroed window id; per-harness
target stores that re-inventory before each use because `@N` may now mean
another pane.

**Redesign: one identity.** The durable key is the identity in core, store,
wire and API. An execution keeps its own activation id. `@7` is a display
alias resolved at the edge, like a short git hash. Hot output frames use the
per-connection stream handle that already exists. The id types live in one
small crate that both core and protocol depend on.

**What disappears.** The bridges, interning, both id-validity tokens, the
kill provenance ledger, the hub's incarnation fence, credential revocation
on restart, requalification, retagging collisions and the integration target
stores. A kill names an activation; if that activation is gone it does
nothing.

**The price.** A wire break. Twelve more bytes on control frames.

## R4. The wire has no single declaration

**The fact.** The binary codec is hand-written. "The set of frames" is
restated in about twelve places, and adding one request frame takes about
17 edits in `phux-protocol` before any consumer. Only the top level of a
frame is self-describing; nested records are positional. Each feature gets a
permanent bit. Typed operations are tunnelled through a string-keyed
metadata store whose write has no reply.

**What it costs (roughly 18,000 lines in the protocol crate and its
consumers).**

- About 2,000 lines of ratchets whose only job is to detect drift between
  the hand lists, including a test that parses Rust source text to compare
  tag constants (`tests/it/kinds_table.rs:159-216`).
- `wire-schema.json` is reverse-engineered by deleting each field from
  samples to see whether decode fails (`wire_schema/derive.rs:90-120`).
  Its hand-written prose is already wrong for `ActorRef`
  (`docs/spec/wire-schema.json:12` omits the two principals that
  `wire/frame/codec.rs:304-317` writes).
- Record tails, presence bytes, presence inferred from leftover bytes
  (which `docs/compatibility.md` forbids), a five-part snapshot trailer
  whose pieces are rejoined by linear search on id, four coexisting body
  grammars.
- 47 feature bits across two words plus four other capability channels,
  and roughly 70 branches on them in the server and client crates.
- `SET_METADATA` as RPC: key-string parsing inside the authorizer,
  one-shot result keys, a nonce "doorbell" for config reload, a translation
  layer for the legacy layout key (`state/layout_translation.rs`, 309),
  refusals that can only be logged
  (`runtime/connections/metadata_mutation.rs:33-36`).
- A window and group model the server no longer has, synthesized for the
  wire (`state/legacy_snapshot.rs`, 275).
- Three generations of session create, eight ways to end something, three
  input paths.

**Redesign: one schema, every request answered.** Declare each message
once. Generate the codec, the frame enum, the classifier's required row,
correlation rules, the schema document, the reference pages, the MCP tool
table and TypeScript types from that declaration. Tag fields at every level
so an addition is an optional field in place. Give every request a reply in
one envelope. Turn the metadata-key commands into methods. Replace permanent
bits with a served method catalog (`DESCRIBE_API` and the method table
already exist) and a support floor: anything older than the floor is
unconditional and its fallback is deleted.

**What disappears.** The restated lists and their ratchets, the
reverse-engineered schema, tails and trailers, the metadata RPC machinery
and its client read-backs, most feature branches, the hand-written
validators in the integrations (R11), and the byte-pinned generated docs
(R12).

**The price.** A wire major, or a long period carrying both. The
hand-written codec is fast and allocation-gated; generated code has to meet
the same hot-path budget.

## R5. One thread runs everything, and upgrade is an in-place re-exec of that process

**The fact.** libghostty's `Terminal` is `!Send`, so one current-thread
runtime hosts every connection, every pane and all state, behind an
`Arc<Mutex<ServerState>>` that must never be held across an await. An
upgrade serializes live memory to a blob and `execve`s a possibly different
binary, handing over every PTY descriptor.

**What it costs (roughly 10,000 lines).**

- Work pushed onto side threads and then re-ordered: an input-lane thread,
  a waiter thread, a writer thread per pane, a shared poller with a
  parked/hot reader state machine, and three PTY write paths serialized by
  a hand-built phase/pending/lane-ahead protocol
  (`phux-terminal/src/actor/fast_pty.rs:1-13`). Mode state is copied off
  the engine on every chunk so another thread can encode input, with a
  byte-equality guard test between the two encoders.
- Credits, stall limits, coalescing caps, a 14-slot fairness rotation, and
  deadlines around every actor query so one wedged pane cannot stall the
  rest.
- A thread-local raw-pointer reentrancy scheme on the state mutex
  (`state/mod.rs:285-372`). Sends under the lock are `try_send`, which is
  where dropped notifications come from.
- Child exit observed by polling at 1 to 20 ms under a 500 ms budget,
  because nothing may block the actor (`actor/io/lifecycle.rs`).
- Upgrade: the blob as a cross-version format, import and merge against the
  store, frozen fixtures so a field rename cannot silently drop data, a
  candidate preflight because "a resume failure kills every pane", macOS
  AMFI and `ETXTBSY` retries, seal and unseal of agent sessions, sidecar
  pause and resume, and `PaneExiting` aborts (about 3,100 non-test lines in
  `upgrade/`, `state/upgrade_blob/`, `runtime/upgrade/`).

**Redesign: split by rate of change.** A terminal host owns one pane's PTY
master, child, emulator and input encoding on one thread, so byte order is
intrinsic and `!Send` is confined to the only place that needs it. It speaks
a narrow local protocol: state diffs out (R1), structured input in. The
control plane is everything else: connections, authorization, organization,
hub, agents, plugins. It keeps its state in the store (R2) and owns no
terminal.

Run the terminal host as a process and upgrade becomes restarting the
control plane. The hosts keep running and reconnect.

**What disappears.** The input lane, the fast path and its ordering
protocol, mode snapshots and the duplicate encoder, the reader migration,
credits and stall limits, the fairness rotation, exit polling (the host
blocks in `waitpid`), the state mutex and its reentrancy scheme, and the
whole re-exec handoff with its blob, fixtures, preflight and platform
probes. A control-plane crash stops ending every program, which
`docs/concepts.md` currently lists as the cost of a server crash.

**The price.** One small process per pane. An emulator fix reaches
existing panes only through a per-pane handoff, which is one terminal's
snapshot instead of the whole server's memory. On macOS the privacy
attribution of pane children moves to the host binary. The in-process half
step (one thread per terminal, state in the store) removes the side-thread
ordering but keeps the re-exec.
