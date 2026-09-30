---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-22
---

# Module structure

**TL;DR.** Per-crate module trees as they exist in tree today, kept as a
navigational map rather than an exhaustive listing. New modules should land
in the shape that fits the crate; do not retrofit older layouts onto new
work.

---

Twenty crates, roughly in dependency order. Directory-level entries only;
the crate's own `lib.rs` and module docs own the file-level detail. The
`phux-tui` / `phux-client-core` split is
[`render-layering.md`](./render-layering.md); crate edges are
[`crate-graph.md`](./crate-graph.md).

## `phux-protocol`

```
src/
  lib.rs          — re-exports, PROTOCOL_VERSION
  ids.rs          — SessionId, WindowId, ResourceId (Local / Satellite), ClientId
  caps.rs         — HELLO/HELLO_OK features, bootstrap profiles and codecs
  kinds.rs        — kind catalog and verb classification of every frame (ADR-0125)
  scope.rs        — workload scope grants and their canonical bytes
  policy.rs, sgr.rs, kitty_replay.rs
  input/          — INPUT_* event types (docs/spec/input.md)
  wire/           — TLV codec and framing (docs/spec/proto.md Appendix A)
```

`input` and `wire` sit behind the `server` feature so the no-feature build
compiles without `libghostty-vt`. `wire/frame/`, `wire/decode.rs`, and
`wire/info.rs` reference each other deliberately: one recursive vocabulary
and its parser, not separable layers.

## `phux-core`

```
src/
  ids.rs          — typed slotmap keys
  registry.rs     — Registry: SlotMaps, the only constructor, cascading deletes
  session.rs, window.rs (slots + binary split-tree LayoutNode)
  resource.rs     — ResourceKind, ResourceDescriptor, AgentFacet
  terminal.rs     — TerminalFacet (dims/cwd/title; no PTY, no libghostty)
  screen.rs, session_list.rs, process.rs
                  — the GET_SCREEN, `phux ls --json`, and GET_TERMINAL_STATE
                    documents, shared by producer and consumers
```

One descriptor serves every kind; `kind` decides which facet is populated.
`Registry::new_agent_session` refuses any parent that is not a live
Terminal. Selector resolution is client-side (`phux-client::selector`).

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
  state/          — ServerState, one module per concern: sessions, the
                    ResourceTable, resolve, bindings, clients, metadata,
                    leases, hub, agent tracking, event journal (ADR-0123),
                    retained exits (ADR-0124), approvals (ADR-0128), roles
  resource/       — ResourceCore, ResourceHandle and its facets, event sink
    agent_session/  — record ring, append validation, derived state (ADR-0103)
    terminal/       — TerminalActor: libghostty Terminal, PTY threads,
                      bootstrap cuts (ADR-0070), OSC-133, process facet
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
  telemetry.rs, health.rs, perf.rs, mailbox.rs
```

Runtime code holds a `ResourceHandle` and reaches kind-only channels through
`terminal()` or `agent_session()`, the two producers of `WrongResourceKind`;
nothing else matches on the facet enum. `ResourceTable` holds every map
keyed on a live resource and never looks inside a facet.
`ServerState::resolve_resource` is the one seam classifying a wire id as
local, satellite, or unknown. PTY I/O runs on two `std::thread`s inside
`resource/terminal/`, bridged to the actor over `mpsc`.

## `phux-client`

The headless client library (ADR-0100): no `ratatui`, no controlling
terminal. Every agent verb and the `phux-mcp` adapter are thin projections
over its functions ([`../consumers/sdk.md`](../consumers/sdk.md)).

```
src/
  attach/         — Dial (UDS / QUIC / WS), frame I/O, HELLO, stdin parser,
                    acknowledged-input replay journal, AttachError / AttachEnd
  selector.rs     — TARGET resolution (ADR-0021)
  snapshot.rs, run.rs, send_keys.rs, wait.rs, watch.rs, resize.rs, ask.rs,
  spatial.rs, layout_ops.rs, spawn.rs, kill.rs, signal.rs, detach.rs,
  session.rs, session_list.rs, tags.rs, approvals.rs, conditional_kill.rs,
  agent_*.rs, vcs.rs, explain.rs, perf.rs, record.rs, upgrade.rs, state.rs
                  — the library half of each CLI verb, shared with MCP
  resource.rs, resource/ — `phux resource show|wait|methods` and cursors
  testkit.rs      — the scripted server client-side tests speak to
```

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
    input_dispatch/, action_registry.rs, actions.rs — keybinding pipeline
    render.rs, paint.rs, repaint.rs, reflow.rs — TerminalRenderer
    pane_state.rs, fleet.rs, focus.rs, copy.rs, directory_picker.rs, ...
  render/         — ratatui chrome
    chrome/       — status bar, sidebar, dividers
    overlay/      — copy mode, menus, prompts, settings page, toasts, which-key
```

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
  layout/         — layout tree, split math, persisted CBOR envelope
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
  (fingerprint-pinned TLS 1.3 plus bearer token). Under `provision` it also
  mints and reads the self-signed pair (`cert.rs`) that both `phux-server`
  and `phux-relay` terminate with, and `window.rs` owns the congestion-tracked
  QUIC send window ([`transport.md`](./transport.md)).
- **`phux-client-runtime`** — the one client orchestration layer (ADR-0133;
  [`client-runtime.md`](./client-runtime.md)): registry target resolution,
  dial plans, the reconnect `Ladder`, the relay tunnel, the sans-IO
  `ControlPlane`, the engine owner thread, grid publication, and
  `Runtime::connect`. Rust API only.
- **`phux-client-ffi`** — the one binding crate (ADR-0135). `projection/`
  derives the product vocabulary from the runtime once; `c/` (feature
  `c-abi`, default) is the C ABI in `include/phux/client.h` that Cockpit
  links; `uniffi/` (feature `uniffi`) is the Swift/Kotlin surface
  phux-mobile consumes. Packaging lives in `scripts/build-*ffi*.sh`
  ([`../RELEASING.md`](../RELEASING.md)).
- **`phux-relay`** — the reference relay (ADR-0051, ADR-0052); never parses
  phux frames.
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
- **`phux-perf`** — lock-free telemetry primitives and the `PerfReport`
  behind `GET_PERF` (ADR-0096); no workspace dependencies.
- **`phux-server-testkit`** — shared scaffolding for the server's wire
  integration tests.
- **`portable-pty-adopt`** — re-adopts a running PTY into `portable-pty`
  for the upgrade handoff (ADR-0032).

## Status

| Gap | Today | Owner | Tracked |
|---|---|---|---|
| Alternate-screen history harvest driver | Not implemented; the unwired merge helper was removed. ADR-0078 is Proposed. | [ADR-0078](../adr/0078-alternate-screen-history.md) | not scheduled |
