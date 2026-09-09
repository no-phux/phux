---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-09
---

# Module structure

**TL;DR.** Per-crate module trees as they exist in tree today, kept as a
navigational map rather than an exhaustive listing. New modules should land
in the shape that fits the crate; do not retrofit older layouts onto new
work. The render-layering split between `phux-tui` and `phux-client-core`
is documented separately in [`render-layering.md`](./render-layering.md); crate
dependency edges are documented in [`crate-graph.md`](./crate-graph.md).

---

What is in tree today. New modules land in the shape that fits the crate;
do not retrofit older layouts onto new work. Eighteen crates make up the
workspace; the sections below cover them roughly in dependency order
(wire, domain, daemon, clients, config, binary, then the smaller
special-purpose crates).

## `phux-protocol`

```
src/
  lib.rs              — re-exports, top-level docs, PROTOCOL_VERSION
  ids.rs              — SessionId, WindowId, ResourceId (the wire resource
                        id: Local / Satellite), ClientId, StreamId,
                        BootstrapId
  caps.rs             — HELLO/HELLO_OK capability negotiation (features,
                        bootstrap profiles and codecs, ADR-0070)
  policy.rs           — shared ALPN / transport-policy constants
  sgr.rs              — SGR color/style wire atoms
  kitty_replay.rs      — kitty-keyboard-protocol replay helpers
  input/              — INPUT_* event types (docs/spec/input.md)
    key.rs, mouse.rs, focus.rs, paste.rs, mod.rs
  wire/               — TLV codec (docs/spec/proto.md Appendix A)
    frame/            — FrameKind + discriminants + length-prefix framing
    encode.rs, decode.rs, field.rs, info.rs, error.rs
```

The `input` and `wire` modules are gated behind the `server` cargo feature
so the no-feature shell compiles without `libghostty-vt`; see `lib.rs` for
the docs.rs / crates.io rationale. Protocol 0.7 permanently retired
`TERMINAL_SNAPSHOT = 0x91`: attach content is `BOOTSTRAP_BEGIN` / bounded
opaque `BOOTSTRAP_CHUNK`s / `BOOTSTRAP_READY`, with retained history pulled
afterward ([ADR-0070](../../ADR/0070-native-engine-state-bootstrap.md)).
Native checkpoint, history, cursor, and raw PTY payloads are engine-owned
bytes and are never scanned or rewritten by phux; synthesized VT remains an
explicit compatibility profile. The wire still spells the resource id and
the substrate frames with the Terminal vocabulary (`ResourceId`,
`RESOURCE_OUTPUT`, `SPAWN_RESOURCE`); the 0.9.0 rename is tracked in the
Status table below.

The mutual references among `wire/frame/`, `wire/decode.rs`, and
`wire/info.rs` are deliberate. `frame` and `info` define one recursive wire
vocabulary, while `decode` owns the bounds-checked parser that constructs that
vocabulary. Splitting the parser helpers or recursive types into artificial
leaf modules would move the same codec recursion without creating a clearer
layer boundary, so this sibling cycle is accepted (phux-4fbs.5).

## `phux-core`

```
src/
  lib.rs              — re-exports
  ids.rs              — typed slotmap keys; `ResourceId` is the domain name
                        for the `ResourceId` key
  registry.rs         — Registry: SlotMaps + cascading deletes (a parent
                        resource takes its children with it)
  session.rs          — Session
  window.rs           — Window (Terminal-kind slots) + binary split-tree
                        LayoutNode
  resource.rs         — ResourceKind, ResourceDescriptor (kind + parent +
                        window + the kind's facet), AgentFacet
  terminal.rs         — TerminalFacet: dims/cwd/title (no PTY, no
                        libghostty state)
  screen.rs           — ScreenState: the GET_SCREEN / snapshot projection of
                        the Terminal facet
  session_list.rs     — SessionListJson: the `phux ls --json` projection
```

One descriptor struct serves every kind; `kind` decides which facet is
populated, and the `Registry` is the only constructor. `Registry::
new_terminal` places a Terminal in a window slot; `Registry::
new_agent_session` binds an AgentSession to a live Terminal parent and
refuses any other parent kind. `screen.rs` is unchanged by the kind split
because it projects the Terminal facet only.

Selectors and config still live outside this crate — selector resolution is
client-side (`phux-client::selector`, per ADR-0021), and config is its own
crate (`phux-config`).

## `phux-server`

The daemon. One `ServerRuntime` per user, a single-threaded tokio runtime,
UDS listener plus optional remote listeners (ADR-0003, ADR-0007). What it
serves are resources: a `ResourceCore` per served thing with one kind engine
behind it.

```
src/
  lib.rs              — re-exports (ServerRuntime, ServerState, the
                        resource types, ...)
  runtime/            — tokio current-thread executor + accept loops;
                        spawns per-client tasks on a LocalSet (ADR-0014)
    mod.rs, attach.rs, client.rs, commands.rs, pump.rs, resume.rs,
    upgrade.rs, upload.rs, voice.rs
    input_lane/       — the dedicated input-encoding thread (ADR-0044) and
                        the acknowledged-input journal (ADR-0053)
  state/              — ServerState: sessions, windows, resources, leases,
                        metadata, hub table, agent tracking, config —
                        one module per concern rather than one large file
    mod.rs, sessions.rs, terminals.rs, session_table.rs, resource_table.rs
    (ResourceTable: ResourceHandle per live resource, its cancel token,
    subscribers, output pumps, and the engine JoinSet), client.rs,
    client_table.rs, metadata.rs, leases.rs, lease_table.rs, hub.rs,
    hub_state.rs, agent.rs, agent_tracking.rs, cwd.rs, events.rs,
    hook_dispatch.rs, lifecycle.rs, reap.rs, snapshot.rs, viewport.rs, ...
  resource/           — the generic resource core and the engines behind it
    mod.rs            — ResourceCore (engine-side: kind, parent, wire id,
                        checked u64 output sequence, output broadcast,
                        event-subscriber registry + fan-out, cancel token +
                        exit notify, control mailbox), ResourceHandle (the
                        Send + Clone channel set the runtime holds: kind,
                        parent, output, consumer attach/detach/ack, event
                        subscribe/unsubscribe, upgrade, control, plus
                        `facet`), ResourceFacetHandle (one variant per
                        engine), WrongResourceKind
    terminal/         — the Terminal engine: TerminalActor owns one pane's
                        libghostty `Terminal` (!Send, in a RefCell on the
                        LocalSet), its input encoders, PTY reader/writer
                        threads, and coherent bootstrap capture cuts
                        (ADR-0070); embeds a ResourceCore and serves the
                        TerminalHandle facet (input, snapshot, screen,
                        resize, cwd, palette, native checkpoints, cols/rows)
      mod.rs, construct.rs, run_loop.rs, io.rs, native.rs, consumers.rs,
      events.rs, osc133.rs (OSC-133 command-boundary scanner, phux-foz.4),
      requests.rs, spawn.rs, sync.rs, tick.rs
  terminal_actor      — `pub use resource::terminal as terminal_actor`: the
                        path every existing `terminal_actor::` import
                        resolves through
  mailbox.rs          — Outbound (per-client) and TerminalInput (per-engine)
                        message shapes; a crate-root leaf so `state` and
                        `resource` never import each other
  grid/               — synthesized-VT compatibility bootstrap/StateSync
                        emitter (never used to construct native records)
    mod.rs, reference.rs, synthesizer.rs
  native_state.rs     — native checkpoint bootstrap plumbing (ADR-0070)
  downsample.rs       — compatibility-profile rewrite of outbound VT bytes
                        (truecolor -> 256/16, OSC 8 / image / KIP gating);
                        native checkpoint/history/raw live bytes bypass it
  input/              — server-side encoders bridging wire input -> PTY
                        bytes; each Terminal owns its own PerTerminal{Key,
                        Mouse,Focus,Paste} encoder, refreshed from
                        Terminal state
    key.rs, mouse.rs, focus.rs, paste.rs, mod.rs
  agent_detect/       — level-triggered per-terminal agent-state detector
                        (ADR-0046): mod.rs is the state machine (adaptive
                        tick, hysteresis, edge-filtered publish); regions.rs
                        slices the live screen; rules.rs loads the TOML
                        manifests; identify.rs names the agent from the
                        PTY's foreground process; record.rs is the
                        phux.agent/v1 JSON shape
  agent_state.rs      — arbitration between an explicit SET_METADATA and
                        the detector's writes (ADR-0046)
  agent_asked.rs      — the `phux ask` / `asked` event ingress (ADR-0036);
                        the AskedSource ladder is Scrape < Sentinel < Hook
  agent_explain.rs    — the `phux agent explain` evidence report
  hooks.rs            — server-side event-hook dispatcher (config
                        `[[hooks.<name>]]` plus plugin `[[events]]`),
                        argv-only execution, no in-process host
  hub/                — federation hub: satellite registry, outbound
                        dialer/link supervisor, byte relay/splice
                        (phux-v45, ADR-0007)
    mod.rs, link.rs, relay.rs
  transport.rs, transport/
                      — per-transport listeners and frame reader/writer
                        pairs: UDS and WebSocket in transport.rs, quic.rs,
                        tls.rs, webtransport.rs (ADR-0007, ADR-0031); see
                        transport.md
  upgrade/            — graceful server re-exec / PTY handoff (ADR-0032)
    mod.rs, blob.rs
  health.rs, perf.rs, history_merge.rs
                      — start-history crash-loop reporting, the ADR-0096
                        metric statics, and the (unwired) ADR-0078
                        viewport-alignment core
  auth.rs, connector.rs, cwd_query.rs, proc_query.rs, id_bridge.rs,
  policy.rs, search.rs, extract.rs, telemetry.rs
    — auth token checks, outbound connector dialing, kernel cwd/process
      introspection, core<->wire id translation, tracing setup
```

**The facet rule.** Runtime code holds a `ResourceHandle` and reaches a
Terminal-only channel only through `ResourceHandle::terminal()`, the one
place that produces `WrongResourceKind`. Each caller maps that error into
its own reply shape (`runtime/commands.rs` has the one `CommandResult`
mapping); nothing else in the crate matches on the facet enum, so a second
engine adds a variant and a constructor, not a sweep of the runtime.

**`ResourceTable`.** `state/resource_table.rs` holds every map keyed on a
live resource — `ResourceHandle`s, cancellation tokens, the `JoinSet` that
owns the engine futures, per-resource client subscriptions, and the
`ATTACH_RESOURCE` and session-attach output pumps — and never looks inside a
facet. `ServerState::spawn_resource_actor` mints the wire id, registers the
handle, and spawns the engine future in one call under the state lock;
`ServerState::reap_terminal` removes the domain entity (cascading to the
window and session when they empty) and calls `ResourceTable::
forget_resource` in the same acquisition.

**Satellite routing.** Each command handler in `runtime/commands.rs` that
names a resource checks `ResourceId::is_local()` itself and, on a hub,
forwards a satellite-tagged frame through `hub::relay` with the id rewritten
(ADR-0007). There are eight such sites; the single `resolve_resource` seam
that replaces them is tracked in the Status table.

PTY supervision lives inside `resource/terminal/` (two `std::thread`s
bridging blocking `portable_pty` I/O — via `portable-pty-adopt` for
re-adoption on upgrade — to the async actor over `mpsc` channels), not a
separate `pty/` module.

## `phux-client`

The headless client library (ADR-0100): everything a consumer needs to
reach a phux server and speak the wire, and nothing that needs a screen.
Every `phux` agent verb and the `phux-mcp` adapter are thin projections
over the free functions here (`docs/consumers/sdk.md`). No `ratatui`, no
`rustix`, no controlling terminal; the crate never enables tokio's
`io-std`.

```
src/
  lib.rs              — module list + re-exports of the client-core substrate
  attach/             — the headless half of attaching
    mod.rs            — re-exports: Dial vocabulary, InputReplayJournal,
                        AttachError / AttachEnd
    connection.rs     — Dial (Uds / Quic / Ws), the FrameReader and
                        FrameWriter enums, HELLO negotiation,
                        length-prefixed frame I/O; test seams
                        (`from_stream`, `negotiate`) under the `testkit`
                        feature
    quic.rs, ws.rs    — remote transports over phux-dial
    input.rs          — StdinParser: bytes -> libghostty input atoms
                        (shared by the TUI and the keystroke verbs)
    input_replay.rs   — the ADR-0053 acknowledged-input replay journal
    outcome.rs        — AttachError / AttachEnd, the exit vocabulary every
                        verb reports through
  selector.rs         — client-side TARGET selector resolution (ADR-0021)
  snapshot.rs, run.rs, send_keys.rs, wait.rs, watch.rs, resize.rs,
  layout_ops.rs, ask.rs, agent_meta.rs, agent_prompt.rs, agent_wait.rs,
  vcs.rs, explain.rs, perf.rs, record.rs
                      — one module per agent-CLI verb's library half
                        (docs/consumers/agents.md); layout_ops, agent_meta,
                        vcs, and perf are also read by the TUI chrome
  state.rs            — GET_STATE / GET_PERF reads and the degradation notices
  testkit.rs          — the one scripted server every client-side test
                        speaks to (feature `testkit`; phux-tui, phux-mcp,
                        and the binary opt in from dev-dependencies)
```

## `phux-tui`

The reference TUI (ADR-0100): the interactive front end over `phux-client`.
Under ADR-0013 it owns a `libghostty_vt::Terminal` per attached pane and
uses `RenderState` to drive redraw; under
[ADR-0070](../../ADR/0070-native-engine-state-bootstrap.md) it can instead
bootstrap from an exact native checkpoint. `ratatui` is fenced to this
crate; pane-interior substrate lives in `phux-client-core` (below) and the
headless control plane in `phux-client` (above). `phux_tui::attach`
re-exports the headless attach vocabulary so the driver keeps one set of
paths.

```
src/
  lib.rs              — attach + render + settings, re-exports of the
                        client-core substrate
  settings.rs         — TuiSettings: every value derived from the config,
                        built once per attach (tolerant) and swapped whole
                        on reload (strict); see docs/consumers/tui.md 4.3
  attach/             — the attach loop: driver, rendering, input dispatch,
                        fleet/multi-pane orchestration
    mod.rs            — re-exports (phux_client::attach::*, driver entry
                        points, RenderSink, status_bar); the
                        RenderError -> AttachError conversion
    driver/           — tokio::select! lifecycle, RawModeGuard RAII. A
                        one-way orchestrator: it owns no shared vocabulary,
                        so no sibling imports from it (phux-4fbs.4, guarded
                        by tests/rendering/attach_layering.rs)
      entry.rs, main_loop.rs, loop_state.rs, chrome.rs, config_ui.rs,
      headless.rs, overlay_paint.rs, session_io.rs, subscriptions.rs,
      terminal.rs, viewport.rs
    pane_state.rs     — PaneSlot, the session-kernel alias, and the
                        client-local VCS / attention indices over them
    server_frame/     — decodes server frames into client-side effects
    render.rs, paint.rs, repaint.rs, reflow.rs, rendered.rs
                      — TerminalRenderer: feeds RESOURCE_OUTPUT bytes into
                        the local Terminal and paints dirty rows + chrome
    input_dispatch/, action_registry.rs, actions.rs
                      — the configurable keybinding-to-action pipeline
    fleet.rs, focus.rs — multi-session/pane fleet view and focus tracking
    context_menu.rs, onboarding.rs, plugin_actions.rs, plugin_panes.rs,
    record.rs, terminal_probe.rs, tty_input.rs, copy.rs,
    sidebar_zones.rs, stdout_writer.rs, render_prof.rs
  render/             — the ratatui chrome layer (status bar, dividers,
                        sidebar, overlays); see render-layering.md
    chrome/           — status_bar.rs, sidebar.rs, dividers.rs
    overlay/          — copy_mode.rs, menu.rs, prompt.rs, select_list.rs,
                        selection.rs, settings.rs (the settings page,
                        ADR-0101), toast.rs, which_key.rs, widgets.rs
    theme.rs, breakpoints.rs, sgr.rs
```

What this crate deliberately does not yet do: full client-side coverage of
every `docs/consumers/tui.md` keybinding action, and `VIEWPORT_RESIZE`
routing all the way to a live SIGWINCH handler. See
[`predictive-echo.md`](./predictive-echo.md) for the predictive-local-echo
design layered on top of the mirror Terminal (implemented in
`phux-client-core::predict`, wired here).

## `phux-client-core`

Frontend-neutral session and pane-interior substrate, extracted from
`phux-client` under ADR-0020/phux-0fv so the `ratatui` boundary is
compiler-enforced: this crate has no `ratatui`, `crossterm`, `tokio`, or
`web-sys` dependency, so it can compile for a native or a wasm frontend
unchanged.

```
src/
  lib.rs              — re-exports
  engine.rs, engine/ghostty.rs — the generic terminal adapter trait plus
                        its libghostty implementation (feature
                        `native-engine`)
  session.rs, session/  — the synchronous protocol-0.7 session kernel
                        (kernel_rig.rs, property_tests.rs, tests.rs)
  history.rs          — client-owned scrollback cache (ADR-0070)
  layout/             — pane-geometry layout tree + split math + the CBOR
                        metadata envelope persisted server-side
    mod.rs, serialize.rs
  multi_pane/         — layout tree -> per-pane rectangles + divider cells
                        (pure compute; chrome rasterizes the result)
    mod.rs, layout.rs, mouse.rs, rasterize.rs
  predict/            — Mosh-class predictive local echo over the pane
                        mirror
    mod.rs, overlay.rs, reconcile.rs, state.rs
  perf.rs             — the crate's ADR-0096 metric statics
```

The session kernel is keyed by the wire `ResourceId` and treats every
resource it is told about as a Terminal: one replica generation
(`ReplicaKey`: terminal, stream, bootstrap, profile) per attached id, staged
through `BootstrapBegin` / `BootstrapChunk` / `BootstrapReady` and published
atomically. It carries no kind; the kind-aware kernel is tracked in the
Status table.

`phux-client` and `phux-tui` both depend on this crate and re-export its
modules so consumers keep stable `phux_client::{layout, multi_pane, predict}`
paths. Why the
split exists and how the boundary is enforced is owned by
[`render-layering.md`](./render-layering.md); crate edges are in
[`crate-graph.md`](./crate-graph.md).

## `phux-config`

```
src/
  lib.rs              — parse_str + re-exports
  schema.rs           — typed TOML schema (Config, KeybindingsCfg, ...)
  loader.rs           — XDG resolution + agent round-trip
  layer.rs            — config layering/merge (defaults + user + env)
  keybind.rs          — keybind parser + trie resolver
  check.rs            — `phux config check` validation
  connector.rs, remote.rs, satellite.rs — the `[[remote]]`/`[[satellites]]`
                        machine-registry schema and validation
  plugin.rs, plugin/  — plugin manifest schema, loading, linking, version
                        and workspace resolution
    link.rs, loader.rs, source.rs, validate.rs, version.rs, workspace.rs
  integration.rs      — configured `[[agents]]` / launch-integration schema
  distro.rs           — first-run scaffolding/distro detection
  scaffold.rs         — default config file generation
  vocab.rs, error.rs, socket.rs — shared enums, ConfigError with line:col
                        spans, socket-path resolution
  settings/           — the scalar-settings catalogue pinned to the schema,
                        the provenance snapshot, and the comment-preserving
                        writer behind the TUI settings page (ADR-0101)
    mod.rs, write.rs
  widget/             — StatusWidget trait + registry
    mod.rs, status_bar.rs
    widgets/          — cwd.rs, exec.rs, exit_status.rs, help_hints.rs,
                        session_name.rs, time.rs, windows.rs
```

## `phux` (binary)

```
src/
  main.rs             — clap subcommand dispatch and entry point
  commands/           — one module (or submodule tree) per verb
    ls.rs, new.rs, attach.rs, detach.rs, kill.rs, rename.rs, resize.rs,
    spatial.rs (insert-pane/move-pane/swap-pane), spawn.rs, launch.rs,
    send_keys.rs, paste.rs, run.rs, wait.rs, watch.rs, snapshot.rs, ask.rs,
    tag.rs, play.rs, rec/, workspace.rs + workspace/archive/, host.rs,
    remote.rs, satellite.rs + satellite/, plugin.rs + plugin/,
    agent/ (list/show/explain/set/clear/install-claude/config, the
    `--phux-hook` shim and its hook_payload reader),
    server.rs, service.rs, supervise.rs, upgrade.rs, doctor.rs, logs.rs,
    config.rs + config/, config_action.rs, enroll.rs, pair.rs, relay.rs,
    stdio_bridge.rs, worktree.rs, status.rs, completion.rs
  refdocs/            — generators for docs/reference/ (cli.rs, config.rs,
                        actions.rs, widgets.rs, hooks.rs, exit_codes.rs,
                        deprecations.rs, files.rs) — see CONVENTIONS.md
                        "Generated reference docs"
  selector.rs         — CLI-side TARGET parsing entry point
  exit_codes.rs, json_err.rs, output.rs, deprecations.rs,
  help_inventory.rs   — shared exit-code table, the `--json` error
                        contract, stdout-safe printing, deprecated-verb
                        shims, and the help-text inventory the refdocs
                        generator walks
```

The CLI's subcommand surface is wide and wired: session/window/pane
lifecycle, spatial edits, agent introspection (`phux agent ...`),
recording/playback, workspace save/restore, and host/satellite/plugin
management are all live verbs, not aspirational ones. The authoritative
catalog is generated, not hand-maintained here — see
[`docs/reference/`](../reference/) (from `just docs-gen`) and
[`docs/consumers/tui.md`](../consumers/tui.md) §1 /
[`docs/consumers/agents.md`](../consumers/agents.md) §2 for the narrated
per-verb contract. Opt-in cargo features: `dhat-heap` (this binary) and
`tokio-console` (via `phux-server`).

## The smaller crates

These round out the workspace; each is a narrow, single-purpose surface
rather than a layer with its own internal architecture worth diagramming:

- **`phux-dial`** — the shared outbound TLS/QUIC/WebSocket establishment
  layer: fingerprint-pinned TLS 1.3 plus an ADR-0031 bearer token. Both
  `phux-client`'s attach loop and the server's federation hub dial through
  it, so the security-sensitive connection path exists once. Under the
  `provision` feature it also owns the *other* end of that trust story
  (`cert.rs`): minting the persisted self-signed pair whose fingerprint the
  dialer pins, and reading it back. `phux-server` and `phux-relay` both
  terminate TLS on identical terms; ADR-0051 forbids the relay depending on
  `phux-server`, so the one implementation lives here, in the crate both
  already sit on. Each caller keeps its own error vocabulary and maps
  `cert::CertError` into it.
- **`phux-relay`** — the reference relay (ADR-0051, ADR-0052): splices an
  inbound consumer connection onto an outbound connector tunnel. Never
  parses phux frames — only the connector's auth preamble.
- **`phux-record`** — the offline session-recording codec and exporter
  (ADR-0060): pure and synchronous (no tokio, no `phux-protocol`), so the
  same code serves the live recording tee, headless `phux rec`, and an
  offline `--from cast -o gif` re-render.
- **`phux-mcp`** — a minimal hand-rolled JSON-RPC/stdio MCP adapter
  (ADR-0022 §5) wrapping `phux-client`'s agent surface tool-for-tool; no
  separate core.
- **`phux-plugin`** — the shared plugin-runtime surface (argv execution,
  timeouts, env injection) used by both the CLI's `config run` and the
  server's `hooks.rs` dispatcher.
- **`phux-client-ffi`** — a stable native C bridge over
  `phux-client-core`'s synchronous session kernel, for non-Rust native
  embedders; compile-time excluded on wasm.
- **`phux-crash`** — vendored fatal-signal handler (see its NOTICE; the one
  Apache-2.0-only crate in the workspace). SIGSEGV/SIGBUS/SIGABRT do not
  unwind, so neither `RawModeGuard::drop` nor the panic hook runs; this
  handler writes the DECSET resets and restores the saved termios from an
  alternate signal stack before re-raising, so a crash does not strand the
  user in raw mode inside the alt screen.
- **`phux-perf`** — in-process performance telemetry primitives
  (ADR-0096): a lock-free log-linear histogram, counters, gauges, a
  rate-limited warning throttle, `getrusage` process statistics, and the
  `PerfReport` that `GET_PERF` carries as JSON and `phux perf` renders.
  Every hot-path crate declares its metrics as `static`s in a `perf`
  module and lists them in a table; recording is one relaxed atomic add.
  Depends on nothing in the workspace, so it sits under server, client,
  and CLI alike.
- **`phux-server-testkit`** — shared scaffolding for `phux-server`'s wire
  integration tests, factored out of a `tests/common` module so it is
  compiled once rather than once per test binary.
- **`portable-pty-adopt`** — re-adopts an already-running PTY (bare master
  fd + child pid) into `portable-pty`'s trait objects; fills the gap where
  `portable-pty` can only *create* a PTY, needed for the server's
  re-exec/upgrade PTY handoff (ADR-0032).

## Status

| Gap | Today | Owner | Tracked |
|---|---|---|---|
| `resource/agent_session/` engine (record ring, append validation, seq stamping, bootstrap from retained records, state derivation) | `ResourceFacetHandle` has one variant, `Terminal`; no engine accepts appended records and `Registry::new_agent_session` has no server caller. | [ADR-0103](../../ADR/0103-agent-session-resource-and-producer-fed-streams.md) | phux-am9y.9 |
| One `resolve_resource(id) -> Local(&ResourceHandle) \| Remote(relay)` seam | Eight `is_local()` checks in `runtime/commands.rs`, each with its own relay branch. | [ADR-0102](../../ADR/0102-resources-the-server-serves-kinds.md) | phux-am9y.5 |
| Detector precedence Stream > Hook > Process > Screen; `AskedSource::Stream` | `agent_detect` derives from process, title, and screen; `AskedSource` ranks Scrape < Sentinel < Hook. | [ADR-0103](../../ADR/0103-agent-session-resource-and-producer-fed-streams.md) | phux-am9y.11 |
| Kind-aware `phux-client-core` kernel, `phux-client` selectors (`%name`), and TUI projection of AgentSession children | The kernel keys replicas by `ResourceId` with no kind; `phux ls --json` carries no `kind` or `parent`. | [ADR-0103](../../ADR/0103-agent-session-resource-and-producer-fed-streams.md) | phux-am9y.12, phux-am9y.14 |
| Workspace rename `ResourceId` -> `ResourceId`, protocol 0.9.0 frame names | `ResourceId` is a `phux-core` alias only; the wire, `phux-client-core`, and the FFI keep the Terminal spelling. | [ADR-0102](../../ADR/0102-resources-the-server-serves-kinds.md) | phux-am9y.18 |
