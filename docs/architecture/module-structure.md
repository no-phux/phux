---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-10-17
---

# Module structure

**TL;DR.** Directory-level maps of phux's twenty-one crates. Follow each crate's
current layout when adding modules; do not retrofit older layouts onto new
work. File-level detail belongs in the crate's `lib.rs` and module docs.

---

Crates appear roughly in dependency order. For rendering ownership, see
[`render-layering.md`](./render-layering.md); for dependency edges, see
[`crate-graph.md`](./crate-graph.md).

## `phux-protocol`

```
src/
  lib.rs          — re-exports, PROTOCOL_VERSION
  ids.rs          — live handles (SessionId, ResourceId Local / Satellite,
                    ClientId, the legacy WindowId) and 16-byte record keys
  caps.rs         — HELLO/HELLO_OK features (ORGANIZATION in features_ext),
                    bootstrap profiles and codecs
  kinds.rs        — kind catalog and verb classification of every frame (ADR-0125)
  scope.rs        — workload scope grants and their canonical bytes, including
                    the ORGANIZE and TERMINATE verbs (ADR-0163)
  policy.rs, sgr.rs, kitty_replay.rs
  input/          — INPUT_* event types (docs/spec/input.md)
  wire/           — TLV codec and framing (docs/spec/proto.md Appendix A)
```

`input` and `wire` sit behind the `server` feature so the no-feature build
compiles without `libghostty-vt`. `wire/frame/`, `wire/decode.rs`, and
`wire/info.rs` reference each other deliberately: one recursive vocabulary
and its parser, not separable layers. The organization commands, their
`Accepted` and `Refused` results, and the organization events are
`wire/frame/` variants under the existing `COMMAND` and `EVENT` envelopes
(`docs/spec/L1.md` §5.7); the crate encodes them and enforces no domain
rule.

## `phux-core`

```
src/
  ids.rs          — typed slotmap keys: the live handles
  keys.rs         — 16-byte record keys (AuthorityId, SessionKey, ResourceKey,
                    ArrangementKey, WorkspaceKey, OperationId, InstanceId),
                    ActivationId, Revision, LeaseEpoch
  registry.rs     — Registry: SlotMaps plus key-to-handle indices, the only
                    constructor, cascading deletes
  session.rs      — Session: key, name, lifetime, members, arrangement
  resource.rs     — ResourceKind, the resource record, AgentFacet
  activation.rs   — Activation, ExecState, Outcome, ExitPolicy,
                    SessionLifetime, Stamp, ProcIdentity
  arrangement.rs  — LayoutNode<L> split tree, Arrangement, Group, EntryRef,
                    validation against members
  org.rs          — Org snapshot, Command, Refusal, Transition, Effect,
                    Authority (with its Intake session), Receipt
  apply.rs        — apply(org, command, actor, now, fresh): the organization
                    rules, returning a transition and effects or a refusal
  terminal.rs     — TerminalFacet (dims/cwd/title; no PTY, no libghostty)
  screen.rs, session_list.rs, process.rs
                  — the GET_SCREEN, `phux ls --json`, and GET_TERMINAL_STATE
                    documents, shared by producer and consumers
```

One descriptor serves every kind; `kind` decides which facet is populated.
`Registry::new_agent_session` refuses any parent that is not a live
Terminal. `apply` is the one implementation of the organization rules and
does no I/O: keys arrive minted by the server, and the clock is passed in.
The crate does not depend on `phux-protocol` (ADR-0011). Selector
resolution is client-side (`phux-client::selector`).
[`data-model.md`](./data-model.md) describes the records and invariants.

## `phux-agent-rules`

The agent-manifest evaluator (ADR-0046): `regions.rs` (viewport
sub-slices), `rules.rs` (manifest load, compile, evaluate), `explain.rs`
(the offline `phux agent explain` document), `fixtures/`, and the built-in
`rules/*.toml`. Both the server's detector and the CLI depend on it.

## `phux-server`

The daemon: one `ServerRuntime` per user on a current-thread tokio runtime
(ADR-0003, ADR-0007), serving a `ResourceCore` plus one kind engine per
resource.

```
src/
  runtime/        — accept loops, per-client tasks, command dispatch, attach,
                    upgrade, approvals, keyed/idempotent operations,
                    revocation watcher; input_lane/ (ADR-0044, ADR-0053)
  state/          — ServerState, one module per concern: org (`commit`, the
                    only organization mutation path), sessions, the
                    ResourceTable, resolve, bindings, clients, metadata,
                    leases, hub, agent tracking, event journal (ADR-0123),
                    retained exits (ADR-0124), approvals (ADR-0128), roles,
                    legacy_snapshot (the synthesized one-window GET_STATE)
  org_store.rs    — OrgStore trait; SqliteOrgStore at
                    <state_dir>/org/v1.sqlite3 and MemOrgStore; schema
                    migrations, receipt and tombstone pruning (ADR-0162)
  reconcile.rs    — startup reconciliation of recorded activations against
                    ProcIdentity: a pure decision function plus a syscall shim
  resource/       — ResourceCore, ResourceHandle and its facets, event sink
    agent_session/  — record ring, append validation, derived state (ADR-0103)
    terminal/       — TerminalActor: libghostty Terminal, PTY threads,
                      bootstrap cuts (ADR-0070), engine shell-integration
                      effects (OSC 133/9;4/7501), process facet
  grid/           — synthesized-VT compatibility bootstrap / StateSync
  native_state.rs, downsample.rs
                  — native checkpoint plumbing; compatibility-profile rewrite
  input/          — per-Terminal key/mouse/focus/paste encoders
  agent_detect/, agent_state.rs, agent_asked.rs
                  — detector (ADR-0046), SET_METADATA arbitration, ask ingress
  hooks.rs        — event-hook dispatcher, argv-only
  hub/            — federation: satellite links, relay, operation fence
  transport.rs, transport/ — UDS, WebSocket, QUIC, TLS, WebTransport
  upgrade/        — graceful re-exec and PTY handoff (ADR-0032)
  policy.rs, policy/ — per-connection grant and the dispatch guard
  workload.rs, workload/ — mTLS workload authority and registry (ADR-0116)
  auth.rs, connector.rs, cwd_query.rs, proc_query.rs, id_bridge.rs,
  telemetry.rs, health.rs, perf.rs, mailbox.rs, daemon.rs
```

Runtime code holds a `ResourceHandle` and reaches kind-only channels through
`terminal()` or `agent_session()`, the two producers of `WrongResourceKind`;
nothing else matches on the facet enum. `ResourceTable` holds every map
keyed on a live resource and never looks inside a facet.
`ServerState::resolve_resource` is the one seam classifying a wire id as
local, satellite, or unknown. PTY I/O runs on two `std::thread`s inside
`resource/terminal/`, bridged to the actor over `mpsc`.

Every organization change, whether a typed command, a translated legacy
metadata write, or a resource exit, goes through `ServerState::commit`:
`phux-core::apply`, then one org-store transaction, then the in-memory
mirror, the reply, and the journal events. Resource executors report
activation facts back through the same path and never edit membership.
Startup opens the store, imports a pre-store registry once, resumes from
an upgrade blob, reconciles, and only then seeds.

## `phux-client`

The headless client library (ADR-0100): no `ratatui`, no controlling
terminal. Every agent verb and the `phux-mcp` adapter are thin projections
over its functions ([`../consumers/sdk.md`](../consumers/sdk.md)).

```
src/
  attach/         — Connection, the facade over the runtime's one-shot
                    session (ADR-0171: no socket, framing, or handshake
                    code here), stdin parser, acknowledged-input replay
                    journal, AttachError / AttachEnd
  selector.rs     — TARGET resolution (ADR-0021)
  snapshot.rs, run.rs, send_keys.rs, wait.rs, watch.rs, resize.rs, ask.rs,
  spatial.rs, layout_ops.rs, spawn.rs, kill.rs, signal.rs, detach.rs,
  session.rs, session_list.rs, tags.rs, approvals.rs, conditional_kill.rs,
  agent_*.rs, vcs.rs, explain.rs, perf.rs, record.rs, upgrade.rs, state.rs
                  — the library half of each CLI verb, shared with MCP
  resource.rs, resource/ — `phux resource show|wait|methods` and cursors
  testkit.rs      — the scripted server client-side tests speak to
```

`resource::session_of` is the one join from a resource to its session;
no client goes through a window. `kill.rs` kills a whole session with one
`SESSION_KILL` (the server ends every member and deletes the session), or
one `KILL_RESOURCES` against a server without it. `layout_ops.rs` edits the session
arrangement with a compare-and-swap that retries on conflict.

`agent_record.rs` (`phux.agent/v1`), `agent_session_record.rs`
(`phux.agent-session/v1` provenance), and `agent_session.rs` (the
AgentSession resource verbs) are three distinct things despite the names.

## `phux-tui`

The reference TUI (ADR-0100). Owns a libghostty replica per attached pane
and is the only crate that links `ratatui`.

```
src/
  settings.rs     — TuiSettings derived from config, swapped whole on reload
  attach/
    driver/       — the tokio::select! lifecycle; nothing imports from it
    server_frame/ — server frames to client-side effects
    input_dispatch/, action_registry.rs, actions.rs — keybinding pipeline;
                    each action is one registry row with its handler (ADR-0172)
    jobs.rs       — background jobs and their one driver arm (ADR-0172)
    render.rs, paint.rs, repaint.rs, reflow.rs — TerminalRenderer
    pane_state.rs, fleet.rs, focus.rs, copy.rs, directory_picker.rs, ...
  render/         — ratatui chrome
    chrome/       — status bar, sidebar, dividers
    overlay/      — copy mode, menus, prompts, kill confirm, settings page,
                    toasts, which-key
```

The TUI's tabs are the session arrangement's groups, read and replaced
through the runtime rather than the layout metadata key. `close-view`
folds a leaf out of the arrangement and ends nothing; `kill-window` opens
the kill confirm and terminates only on commit.

## `phux-client-core`

The frontend-neutral session kernel and pane-interior substrate. No
`ratatui`, `crossterm`, `tokio`, or `web-sys`, so it compiles for native and
wasm unchanged.

```
src/
  engine.rs, engine/ — terminal adapter trait + libghostty impl (`native-engine`)
  grid.rs, grid/  — the one POD cell layout and the GridProjector
  session.rs, session/ — the synchronous session kernel
  handshake.rs, rename.rs, history.rs, perf.rs
  layout/         — ArrangementDoc (the client edit model of a session
                    arrangement: groups of split trees), split math;
                    layout::legacy holds the phux.tui.layout/v1 CBOR codec
  org_ops.rs      — sans-IO organization operation journal and the
                    kill-workflow planner
  multi_pane/     — layout tree to pane rectangles and divider cells
  predict/        — predictive local echo
```

The kernel is keyed by wire `ResourceId`. A Terminal gets one replica
generation staged through `BootstrapBegin` / `Chunk` / `Ready`; an
AgentSession has no replica and derives state from its records.
`phux-client` and `phux-tui` re-export these modules under stable paths.

## `phux-config`

```
src/
  schema.rs, loader.rs, layer.rs, check.rs, error.rs, vocab.rs
  instance.rs     — profiles and build kind; socket.rs, production.rs — the
                    dev-never-reaches-production guards (operations.md)
  keybind.rs      — keybind parser and trie resolver
  connector.rs, remote.rs, satellite.rs — machine-registry schema
  plugin.rs, plugin/ — plugin manifests: load, link, validate, version
  integration.rs, distro.rs, scaffold.rs
  settings/       — scalar-settings catalogue and comment-preserving writer (ADR-0101)
  widget/         — StatusWidget trait, registry, built-in widgets
```

## `phux` (binary)

```
src/
  main.rs         — usage-rs dispatch
  commands/       — one module or tree per verb
  refdocs/        — generators for docs/reference/ (see CONVENTIONS.md)
  exit_codes.rs, output.rs, deprecations.rs, capabilities.rs,
  environment.rs, companion.rs, skill.rs
  help_inventory.rs — test-time lint over the clap help tree
```

The verb catalog is generated: [`docs/reference/`](../reference/). Opt-in
features: `dhat-heap` here, `tokio-console` via `phux-server`.

## The smaller crates

- **`phux-dial`** — the shared outbound TLS/QUIC/WebSocket dialer
  (fingerprint-pinned TLS 1.3 plus bearer token). It re-exports `quictun`'s
  congestion-tracked QUIC send window (`window`;
  [`transport.md`](./transport.md)) and, under `provision`, the self-signed
  pair (`cert`) that both `phux-server` and `phux-relay` terminate with.
- **`phux-client-runtime`** — the one client orchestration layer (ADR-0133;
  [`client-runtime.md`](./client-runtime.md)): registry target resolution,
  dial plans, the reconnect `Ladder`, the relay tunnel, the sans-IO
  `ControlPlane`, the engine owner thread, grid publication,
  `Runtime::connect`, and the one-shot `session::Session` that verbs ride
  (ADR-0171). Rust API only. `release.rs` is the release model `phux update`
  and Desktop share: channels, builds, the one asset name derivation, and
  the channel manifest parser (ADR-0176); `release/signature.rs` holds the
  embedded release keys and the minisign check both updaters run before
  they trust a digest (ADR-0178). `control/organization.rs` is the
  organization replica: seeded by paged `GetOrganization`, advanced by
  journal events in sequence, refetched on a gap; it retries pending
  operations with the same operation id after reconnect, reports the
  outcome as unknown past the receipt horizon, maps keys to handles, and
  projects the owner-local Intake, separately from attention Inbox.
  `approvals.rs` is the one parse of the server-held approval record and
  decision outcome (ADR-0128), shared with `phux-client`'s verbs;
  `control/approvals.rs` is its opt-in live projection on the control
  connection (list, follow the journal, decide once, never retry).
- **`phux-client-ffi`** — the one binding crate (ADR-0135). `projection/`
  derives the product vocabulary from the runtime once; `c/` (feature
  `c-abi`, default) is the C ABI in `include/phux/client.h` for native
  embedders; `uniffi/` (feature `uniffi`) is the Swift/Kotlin surface
  phux-mobile consumes. The projection is lossless: keys, revisions,
  lifetime, members, arrangements, and execution state all cross.
  `c/arrangement/` wraps the shared arrangement and `c/organization.rs`
  the organization commands. Packaging lives in `scripts/build-*ffi*.sh`
  ([`../RELEASING.md`](../RELEASING.md)).
- **`phux-relay`** — the reference relay (ADR-0051, ADR-0052): `quictun`'s
  relay bound to phux's ALPNs, `phxT` tunnel tag, and state directory
  (ADR-0181); never parses phux frames.
- **`quictun`** — the blind QUIC tunnel relay, with no phux dependency
  (ADR-0181). `relay/` is the endpoint, SNI gate, route-token store, and
  splice; `tunnel.rs` the wire (preamble, close codes, connection-ID tag);
  `connect.rs` the service's tunnel and consumer's dial; `pin.rs` leaf
  pinning; `window.rs`, `cert.rs`, and `secret_file.rs` the modules
  `phux-dial` re-exports.
- **`phux-record`** — offline recording codec and exporter (ADR-0060), pure
  and synchronous.
- **`phux-mcp`** — hand-rolled JSON-RPC/stdio MCP adapter over
  `phux-client`. `tool_table.rs` is its one tool table, held to the CLI and
  kind catalog by `tests/parity.rs` and rendered into
  `docs/reference/parity.md`.
- **`phux-plugin`** — argv plugin execution shared by `config run` and the
  server's hook dispatcher.
- **`phux-crash`** — fatal-signal handler trimmed from an upstream crate
  (Apache-2.0 only) that
  restores the terminal from an alternate signal stack before re-raising.
- **`phux-work`** — durable work store, the interim single-host form of the
  ADR-0092 coordinator. SQLite at `<state_dir>/work/work-v1.sqlite`, mode
  `0600`, schema v1: objectives, runs, bindings, signals, and an append-only
  event log. One keeper process per store (`run_keeper`, run as
  `phux mission keeper`) holds the lock and is the only writer; every
  surface calls it through `Client`, which starts a keeper when none
  answers. The keeper socket is a private exchange between equal builds,
  not `phux-coordinator/1`.
- **`phux-mission`** — mission control over `phux-work`: the
  `phux.mission/v1` documents
  (`crates/phux-mission/schema/phux.mission.v1.schema.json`). It renders
  what the work client returns and never opens the store. `list`, `show`,
  `edit`, `archive`, `restore`, and `keeper` contact no phux server.
  `start` and `add --start` dial one to create the worktree session.
- **`phux-perf`** — lock-free telemetry primitives and the `PerfReport`
  behind `GET_PERF` (ADR-0096); no workspace dependencies.
- **`phux-server-testkit`** — shared scaffolding for the server's wire
  integration tests.
- **`portable-pty-adopt`** — re-adopts a running PTY into `portable-pty`
  for the upgrade handoff (ADR-0032).

## Status

| Gap | Today | Owner | Tracked |
|---|---|---|---|
| Alternate-screen history harvest driver | Not implemented; the unwired merge helper was removed. ADR-0078 is accepted forward-compat. | [ADR-0078](../adr/0078-alternate-screen-history.md) | phux-w7z2.10 |
