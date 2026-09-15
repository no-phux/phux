---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-15
---

# Module structure

**TL;DR.** Per-crate module trees as they exist in tree today, kept as a
navigational map rather than an exhaustive listing. New modules should land
in the shape that fits the crate; do not retrofit older layouts onto new
work.

---

Eighteen crates make up the workspace; the sections below cover them
roughly in dependency order (wire, domain, daemon, clients, config,
binary, then the smaller special-purpose crates). The render-layering
split between `phux-tui` and `phux-client-core` is
[`render-layering.md`](./render-layering.md); crate edges are
[`crate-graph.md`](./crate-graph.md).

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
  kinds.rs            — the kind catalog and the closed verb
                        classification of every client frame (ADR-0125)
  scope.rs            — workload scope grants: selectors, the canonical
                        TerminalScopeSet / effective-set bytes, and the
                        registry grammar (workload-auth.md §5)
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
the docs.rs / crates.io rationale. Attach content is `BOOTSTRAP_BEGIN` /
bounded opaque `BOOTSTRAP_CHUNK`s / `BOOTSTRAP_READY`, with retained
history pulled afterward
([ADR-0070](../adr/0070-native-engine-state-bootstrap.md)).
Native checkpoint, history, cursor, and raw PTY payloads are engine-owned
bytes and are never scanned or rewritten by phux; synthesized VT remains an
explicit compatibility profile. The substrate names are `ResourceId`,
`RESOURCE_OUTPUT`, and `SPAWN_RESOURCE`.

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
  process.rs          — TerminalProcessState: the typed `process` object of
                        `GET_TERMINAL_STATE` (child pid/start, foreground
                        pgid/name, cwd, OSC-133 prompt state, exit), and the
                        Terminal engine's exit-outcome shape; shared by the
                        server (producer) and any consumer (deserializer)
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
    mod.rs, attach.rs, client.rs, commands.rs, resource_commands.rs
    (AgentSession spawn and `APPEND_RESOURCE_OUTPUT`), directory.rs (the
    LIST_DIRECTORY host query), pump.rs, resume.rs, upgrade.rs, upload.rs,
    voice.rs, whoami.rs (the read-only phux.whoami/v1 key),
    ephemeral_listener.rs (OPEN_LISTENER: a QUIC listener for one attach),
    operation_dedupe.rs (the one bounded dedupe record shared by
    APPLY_INPUT ids, spawn keys, and session-create tokens; ADR-0126),
    idempotent_create.rs (keyed SPAWN_RESOURCE and session create on it),
    revocation.rs (the live-authority watcher: re-judges every connection
    a workload grant or pairing-store bearer admitted, on registry/store
    change and at expiry; workload-auth.md §7, ADR-0116),
    dispatch_guard.rs (turns a `policy::enforce` denial into the reply
    each of the three call sites sends: correlated `PERMISSION_DENIED`,
    a rate-limited uncorrelated `ERROR`, or a dropped frame)
    input_lane/       — the dedicated input-encoding thread (ADR-0044) and
                        the acknowledged-input journal (ADR-0053)
  state/              — ServerState: sessions, windows, resources, leases,
                        metadata, hub table, agent tracking, config —
                        one module per concern rather than one large file
    mod.rs, sessions.rs, terminals.rs, session_table.rs, resource_table.rs
    (ResourceTable: ResourceHandle per live resource, its cancel token,
    subscribers, output pumps, and the engine JoinSet), resolve.rs
    (`resolve_resource`: local handle, satellite relay, or unknown),
    bindings.rs (parent cascade and `CloseReason`), client.rs,
    client_table.rs, metadata.rs, leases.rs, lease_table.rs, hub.rs,
    hub_state.rs, agent.rs, agent_tracking.rs, cwd.rs, events.rs,
    hook_dispatch.rs, lifecycle.rs, reap.rs, snapshot.rs, viewport.rs,
    conditional_kill.rs (KILL_RESOURCE_IF: instance token and spawn
    provenance checked and applied in one borrow, ADR-0109),
    satellite_spawns.rs (the hub's bounded record of which consumer
    asked for each satellite resource, ADR-0109),
    journal.rs (the server-wide bounded ring of stamped, sequenced
    events every emission site records into under the state lock;
    `after_seq` replay and the `journal_gap`/`source_gap` accounting,
    ADR-0123),
    retained.rs (the exit-facet timer wheel and count bound behind
    `retain_secs`: purges an exited-but-retained Terminal on TTL or
    when the retained set overflows, ADR-0124), ...
  resource/           — the generic resource core and the engines behind it
    mod.rs            — ResourceCore (engine-side: kind, parent, wire id,
                        checked u64 output sequence, output broadcast,
                        event-subscriber registry + fan-out, cancel token +
                        exit notify, control mailbox), ResourceHandle (the
                        Send + Clone channel set the runtime holds: kind,
                        parent, output, consumer attach/detach/ack, event
                        subscribe/unsubscribe, upgrade, control, plus
                        `facet`), ResourceFacetHandle (Terminal and
                        AgentSession variants), WrongResourceKind
    event_sink.rs     — the engine-to-runtime bounded event channel
                        (`try_send`, never blocks an engine) and the loss
                        counter behind it; the runtime drains it into the
                        journal and turns a full sink into a `source_gap`
                        instead of a silent drop (ADR-0123)
    agent_session/    — the AgentSession engine: record ring, append
                        validation, seq/time stamp, bootstrap from
                        retained `AgentEventsJsonlV1` records, derived
                        state (ADR-0103); record.rs, ring.rs
    terminal/         — the Terminal engine: TerminalActor owns one pane's
                        libghostty `Terminal` (!Send, in a RefCell on the
                        LocalSet), its input encoders, PTY reader/writer
                        threads, and coherent bootstrap capture cuts
                        (ADR-0070); embeds a ResourceCore and serves the
                        TerminalHandle facet (input, snapshot, screen,
                        resize, cwd, palette, native checkpoints, cols/rows)
      mod.rs, construct.rs, run_loop.rs, io.rs, native.rs, consumers.rs,
      events.rs, osc133.rs (OSC-133 command-boundary scanner, phux-foz.4;
      also tracks the `A`/`B`/`C`/`D` prompt-state machine, PHA-406 D5),
      requests.rs, spawn.rs, sync.rs, tick.rs,
      process_facet.rs (the `process` object of `GET_TERMINAL_STATE`:
      child pid/start-time, foreground pgid/basename, cwd, prompt state,
      exit — kernel- and mark-sourced, `None` rather than a guess when a
      query fails; PHA-406 D5)
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
                        phux.agent/v1 JSON shape; live_session.rs is the
                        live AgentSession-child probe so stream evidence
                        outranks screen inference
  agent_state.rs      — arbitration between an explicit SET_METADATA and
                        the detector's writes (ADR-0046); evidence ladder
                        is Stream > Hook > Process > Screen
  agent_asked.rs      — the `phux ask` / `asked` event ingress (ADR-0036);
                        the AskedSource ladder is Scrape < Sentinel < Hook
                        < Stream
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
  policy.rs, policy/enforce.rs
                      — the per-connection grant minted at HELLO and the
                        dispatch guard every frame, command, and QUIC
                        stream bind passes (workload-auth.md §6-§8)
  auth.rs, connector.rs, cwd_query.rs, proc_query.rs, id_bridge.rs,
  search.rs, extract.rs, telemetry.rs
    — auth token checks, outbound connector dialing, kernel cwd/process
      introspection, core<->wire id translation, tracing setup
  workload.rs, workload/
    — mTLS workload authority material and registry (ADR-0116): the CA
      separate from the server leaf so clients pin one fingerprint across
      leaf renewals; split into material.rs (the public enrollment shapes
      `phux workload add-key` accepts, never echoing key material),
      store.rs (owner-only, no-follow, lock-and-rename persistence), and
      reload.rs (stat-generation hot reload so a running server observes a
      new registry generation without a restart)
```

**The facet rule.** Runtime code holds a `ResourceHandle` and reaches a
kind-only channel through `ResourceHandle::terminal()` or
`ResourceHandle::agent_session()`, the two producers of
`WrongResourceKind`. Each caller maps that error into its own reply
shape (`runtime/commands.rs` has the one `CommandResult` mapping);
nothing else in the crate matches on the facet enum, so a further engine
adds a variant and a constructor, not a sweep of the runtime.

**`ResourceTable`.** `state/resource_table.rs` holds every map keyed on a
live resource — `ResourceHandle`s, cancellation tokens, the `JoinSet` that
owns the engine futures, per-resource client subscriptions, and the
`ATTACH_RESOURCE` and session-attach output pumps — and never looks inside a
facet. `ServerState::spawn_resource_actor` mints the wire id, registers the
handle, and spawns the engine future in one call under the state lock;
`ServerState::reap_terminal` removes the domain entity (cascading to the
window and session when they empty) and calls `ResourceTable::
forget_resource` in the same acquisition.

**Satellite routing.** `ServerState::resolve_resource` is the one seam
that classifies a wire id as local handle, satellite relay, or unknown.
Command handlers in `runtime/commands.rs` route through it (ADR-0007).

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
  conditional_kill.rs — bind a spawn to the server's instance token and
                        build, send, and classify KILL_RESOURCE_IF (ADR-0109)
  selector.rs         — client-side TARGET selector resolution (ADR-0021)
  snapshot.rs, run.rs, send_keys.rs, wait.rs, watch.rs, resize.rs,
  layout_ops.rs, ask.rs, agent_meta.rs, agent_prompt.rs, agent_wait.rs,
  agent_session.rs (`phux agent session` / `emit` / `log`),
  vcs.rs, explain.rs, perf.rs, record.rs, upgrade.rs
                      — one module per agent-CLI verb's library half
                        (docs/consumers/agents.md); layout_ops, agent_meta,
                        vcs, and perf are also read by the TUI chrome;
                        upgrade.rs is UPGRADE (`phux upgrade`, ADR-0032)
  agent_record.rs     — phux.agent/v1 read/write/index (`phux agent
                        set`/`clear`/`ls`, ADR-0040); the record type and
                        its encode/parse convention live in agent_meta.rs
  agent_session_record.rs
                      — phux.agent-session/v1 provider-native provenance:
                        the AgentSessionRecord type, its persist/fetch-index
                        round trips, and spawn_with_agent_session (SPAWN_RESOURCE
                        plus the optional provenance write and its
                        KILL_RESOURCE rollback), shared by `phux spawn` /
                        `phux launch`. Distinct from both agent_record.rs
                        (a different, human-declared record) and
                        agent_session.rs below (a different, server-tracked
                        resource kind) despite the similar names.
  detach.rs           — DETACH_CLIENTS classification (`phux detach`)
  kill.rs             — SHUTDOWN / KILL_RESOURCES / KILL_RESOURCE and the
                        keep-empty clear (`phux kill`); selector resolution
                        and the whole-session-vs-per-pane choice stay with
                        the caller (`phux kill`, and the MCP `phux_kill`
                        composing the same primitives in-process)
  session.rs          — session-identity L3 writes: `rename` (`phux
                        rename`), whose request id is now a caller parameter
                        rather than hardcoded inside the write (the CLI
                        still passes a fixed id today; this only matters
                        once a caller composes more than one rename per
                        connection), and create-without-attach (`phux
                        new`/`phux new --json`/`--empty`), including the
                        atomic-agent-session-restore capability preflight;
                        duplicate-name rejection and CLI wording stay in
                        `crates/phux/src/commands/new.rs`
  session_list.rs     — the `phux ls --json` document (SessionListJson)
                        built from one GET_STATE view; `phux ls` prints it
                        and MCP `phux_ls` returns it
  signal.rs           — ACQUIRE_INPUT / RELEASE_INPUT / SIGNAL_TERMINAL
                        command builders and their shared outcome
                        (`phux take` / `phux give` / `phux signal`, ADR-0033)
  spatial.rs          — insert-pane / move-pane / swap-pane: selector
                        resolution, the plan, execution (LayoutOps or the
                        cross-session pane_move), the result document, and
                        the refusal codes, shared by the CLI verbs and the
                        MCP spatial tools
  spawn.rs            — SPAWN_RESOURCE, and the ownership-verify +
                        KILL_RESOURCE rollback dance behind explicit
                        placement (`phux spawn`, `phux launch`); the
                        `--json` result document and the SpawnError
                        sentences both surfaces print
  tags.rs             — phux.tags/v1 read/write (`phux tag`, ADR-0027)
  resource.rs, resource/
                      — the `phux resource` noun (PHA-406): resource.rs
                        picks Terminal-kind panes out of a snapshot and
                        spells the shared JSON vocabulary (lifecycle, exit
                        facet, close reason, control action); cursor.rs is
                        the `server_id:seq` cursor type `--after` parses
                        and prints; wait.rs is the D2 algorithm (subscribe
                        with `after_seq`, then a level `GET_STATE` read —
                        race-free and idempotent); show.rs is one
                        resource's inspection record (kind, lifecycle,
                        exit, process, input holder, tags, agent); methods.rs
                        intersects the `phux-protocol::kinds` catalog with
                        what the server negotiated and the resource's kind
                        (D4)
  state.rs            — GET_STATE / GET_PERF reads and the degradation notices
  testkit.rs          — the one scripted server every client-side test
                        speaks to (feature `testkit`; phux-tui, phux-mcp,
                        and the binary opt in from dev-dependencies)
```

## `phux-tui`

The reference TUI (ADR-0100): the interactive front end over `phux-client`.
Under ADR-0013 it owns a `libghostty_vt::Terminal` per attached pane and
uses `RenderState` to drive redraw; under
[ADR-0070](../adr/0070-native-engine-state-bootstrap.md) it can instead
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
                        the local Terminal and paints the changed cells of
                        dirty rows (diffed against a per-pane front buffer,
                        phux-esge) + chrome
    input_dispatch/, action_registry.rs, actions.rs
                      — the configurable keybinding-to-action pipeline
    fleet.rs, focus.rs — multi-session/pane fleet view and focus tracking
    directory_picker.rs — rows for the go-to-directory picker over a
                        LIST_DIRECTORY reply (L3.md §4)
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

SIGWINCH ships `VIEWPORT_RESIZE` through `attach/driver`. See
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
  handshake.rs        — shared HELLO_OK acceptance (exact protocol triple,
                        advertised profile, native feature intersection,
                        payload limits)
  session.rs, session/  — the synchronous session kernel
                        (agent_stream.rs, kernel_rig.rs, property_tests.rs,
                        tests.rs)
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

The session kernel is keyed by the wire `ResourceId` and records
`ResourceKind` per id. A Terminal-kind resource gets one replica generation
(`ReplicaKey`: terminal, stream, bootstrap, profile), staged through
`BootstrapBegin` / `BootstrapChunk` / `BootstrapReady` and published
atomically. An AgentSession-kind resource has no replica; `agent_stream.rs`
parses `AgentEventsJsonlV1` records and derives session state. Input aimed
at a non-Terminal is `NotATerminal`.

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
  main.rs             — usage-rs subcommand dispatch and entry point
  commands/           — one module (or submodule tree) per verb
    ls.rs, new.rs, attach.rs, detach.rs, kill.rs, rename.rs, resize.rs,
    spatial.rs (insert-pane/move-pane/swap-pane), spawn.rs, launch.rs,
    send_keys.rs, paste.rs, run.rs, wait.rs, watch.rs, snapshot.rs, ask.rs,
    tag.rs, play.rs, rec/, workspace.rs + workspace/archive/, host.rs,
    remote.rs, satellite.rs + satellite/, plugin.rs + plugin/,
    agent/ (list/show/explain/set/clear/install-claude/config,
    session open|close, emit, log, the `--phux-hook` shim),
    server.rs, service.rs, supervise.rs, upgrade.rs, doctor.rs, logs.rs,
    config.rs + config/, config_action.rs, enroll.rs, pair.rs, relay.rs,
    stdio_bridge.rs, worktree.rs, status.rs, whoami.rs, completion.rs,
    bootstrap.rs + ssh_bootstrap.rs (the two ends of `attach --ssh`),
    resource.rs (`phux resource show|wait|methods`, PHA-406: a thin
    projection over `phux_client::resource` — TARGET resolution, the
    exit-code mapping, and the human text; the `--json` documents are the
    library's), workload.rs (`phux workload add-key|list|revoke|...`,
    ADR-0116)
  refdocs/            — generators for docs/reference/ (cli.rs, config.rs,
                        actions.rs, widgets.rs, hooks.rs, exit_codes.rs,
                        deprecations.rs, files.rs, kinds.rs — the
                        `docs/reference/kinds.md` render of the
                        `phux-protocol::kinds` catalog, ADR-0125; parity.rs
                        — the `docs/reference/parity.md` render of the MCP
                        tool table) — see CONVENTIONS.md "Generated
                        reference docs"
  selector.rs         — CLI-side TARGET parsing entry point
  exit_codes.rs, json_err.rs, output.rs, deprecations.rs,
  help_inventory.rs   — shared exit-code table, the `--json` error
                        contract, stdout-safe printing, deprecated-verb
                        shims, and the help-text inventory the refdocs
                        generator walks
  feature_names.rs    — `phux status --json .features` and
                        `phux --capabilities --json` kind-catalog gates;
                        names come from `ServerFeature::snake_name` in
                        `phux-protocol` (caps.rs is the single list)
```

The CLI's subcommand surface is wide and wired: session/window/pane
lifecycle, spatial edits, agent introspection (`phux agent ...`),
recording/playback, workspace save/restore, and host/satellite/plugin
management are all live verbs, not aspirational ones. The authoritative
catalog is generated, not hand-maintained here — see
[`docs/reference/`](../reference/) (from `just docs-gen`) and
[`docs/consumers/tui.md`](../consumers/tui.md) /
[`docs/consumers/agents.md`](../consumers/agents.md) for the narrated
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
  `cert::CertError` into it. It also owns the one piece of QUIC *sending*
  policy (`window.rs`): the congestion-tracked send window — quinn's window
  held to the congestion window plus 16 KiB, re-read before every partial
  write — that the server's QUIC and WebTransport writers and `phux-relay`'s
  consumer-facing leg all write through (see
  [`transport.md`](./transport.md)).
- **`phux-relay`** — the reference relay (ADR-0051, ADR-0052): splices an
  inbound consumer connection onto an outbound connector tunnel. Never
  parses phux frames — only the connector's auth preamble.
- **`phux-record`** — the offline session-recording codec and exporter
  (ADR-0060): pure and synchronous (no tokio, no `phux-protocol`), so the
  same code serves the live recording tee, headless `phux rec`, and an
  offline `--from cast -o gif` re-render.
- **`phux-mcp`** — a minimal hand-rolled JSON-RPC/stdio MCP adapter
  (ADR-0022 §5) wrapping `phux-client`'s agent surface tool-for-tool; no
  separate core. `resource_tools.rs` is the `phux_resource_show|wait|methods`
  trio (PHA-406), in-process over `phux_client::resource` so its documents
  cannot drift from the CLI's; `annotations.rs` derives every tool's
  `readOnlyHint`/`destructiveHint` from the `phux-protocol::kinds` catalog
  instead of a hand-set flag per tool (ADR-0125), through
  `src/tool_table.rs`. That file is the adapter's one table of tools: the
  CLI verb each mirrors, whether it runs in-process or through the bounded
  CLI residue (`cli_adapter.rs`, which refuses any tool not marked
  residue), and what it touches. The parity gate (`tests/parity.rs`) holds
  that table to the live catalog, the CLI grammar, and the kind table;
  `phux`'s refdocs compile the same file to render
  `docs/reference/parity.md`.
- **`phux-plugin`** — the shared plugin-runtime surface (argv execution,
  timeouts, env injection) used by both the CLI's `config run` and the
  server's `hooks.rs` dispatcher.
- **`phux-client-ffi`** — a stable native C bridge over
  `phux-client-core`'s synchronous session kernel, for non-Rust native
  embedders; compile-time excluded on wasm. Its `remote` module is the
  embedder half of `phux --remote`: it resolves a host in the CLI's
  `[[remote]]` registry and relays frames between an embedder-owned
  Unix-domain socket pair and a QUIC/WSS dial (`phux_remote_tunnel_*`).
  Its `directory` module carries the `LIST_DIRECTORY` host query for a
  go-to-directory picker, retaining one correlated listing per client
  (`phux_client_list_directory`, `phux_client_directory_*`). Its `log`
  module installs the bridge's one `tracing` subscriber on standard error
  (`phux_client_log_init`), so an embedder that redirects descriptor 2 to a
  file gets the tunnel's lifecycle beside its own lines. Named projections
  (ADR-0129) are L3 metadata key ops on keys shaped
  `<prefix>.layout/v1/<session-id>` (`phux_client_projection_get` /
  `_set` / `_delete`), not a new resource kind. The bridge
  subscribes to the connection-wide `AgentEvent` stream on every
  `ATTACH_READY` (as a `KernelSend::SubscribeEvents` effect the kernel
  itself emits, `phux-client-core` having no transport of its own to send
  one from) and folds cwd/command-boundary/process-exit events into
  `PHUX_CLIENT_STATUS_CWD` / `_COMMAND_STARTED` / `_COMMAND_FINISHED` /
  `_EXITED` effects (PHA-406/PHA-284; `include/phux/client.h`).
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
| Alternate-screen history harvest driver | `history_merge.rs` is a tested pure function; nothing calls it. ADR-0078 is Proposed. | [ADR-0078](../adr/0078-alternate-screen-history.md) | not scheduled |
