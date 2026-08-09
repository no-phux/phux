---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-08-07
---

# Module structure

**TL;DR.** Per-crate module trees as they exist in tree today, kept as a
navigational map rather than an exhaustive listing. New modules should land
in the shape that fits the crate; do not retrofit older layouts onto new
work. The render-layering split inside `phux-client` is documented
separately in [`render-layering.md`](./render-layering.md); crate
dependency edges are documented in [`crate-graph.md`](./crate-graph.md).

---

What is in tree today. New modules land in the shape that fits the crate;
do not retrofit older layouts onto new work. Fourteen crates make up the
workspace; the sections below cover them roughly in dependency order
(wire, domain, daemon, clients, config, binary, then the smaller
special-purpose crates).

## `phux-protocol`

```
src/
  lib.rs              — re-exports, top-level docs, PROTOCOL_VERSION
  ids.rs              — SessionId, WindowId, PaneId, TerminalId, ClientId
  caps.rs             — HELLO/HELLO_OK capability negotiation (features,
                        native-bootstrap profile bits, ADR-0070)
  policy.rs           — shared ALPN / transport-policy constants
  sgr.rs              — SGR color/style wire atoms
  kitty_replay.rs      — kitty-keyboard-protocol replay helpers
  input/              — INPUT_* event types (docs/spec/input.md)
    key.rs, mouse.rs, focus.rs, paste.rs, mod.rs
  wire/               — TLV codec (docs/spec/proto.md Appendix A)
    frame.rs          — FrameKind + length-prefix framing
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
explicit compatibility profile.

## `phux-core`

```
src/
  lib.rs              — re-exports
  ids.rs              — typed slotmap keys
  registry.rs         — Registry: SlotMaps + cascading deletes
  session.rs          — Session
  window.rs           — Window + binary split-tree LayoutNode
  terminal.rs         — Pane/Terminal metadata (no PTY, no libghostty state)
  screen.rs           — ScreenState: the GET_SCREEN / snapshot projection
  session_list.rs     — SessionListJson: the `phux ls --json` projection
```

Selectors and config still live outside this crate — selector resolution is
client-side (`phux-client::selector`, per ADR-0021), and config is its own
crate (`phux-config`).

## `phux-server`

The daemon. One `ServerRuntime` per user, a single-threaded tokio runtime,
UDS listener (ADR-0003, ADR-0007).

```
src/
  lib.rs              — re-exports (ServerRuntime, ServerState, ...)
  runtime/            — tokio current-thread executor + UDS accept loop;
                        spawns per-client tasks on a LocalSet (ADR-0014)
    mod.rs, attach.rs, client.rs, commands.rs, input_lane.rs, resume.rs,
    upgrade.rs, upload.rs
  state/              — ServerState: sessions, windows, terminals, leases,
                        metadata, hub table, agent tracking, config —
                        one module per concern rather than one large file
    mod.rs, sessions.rs, terminals.rs, session_table.rs, terminal_table.rs,
    client.rs, client_table.rs, metadata.rs, leases.rs, lease_table.rs,
    hub.rs, hub_state.rs, agent.rs, agent_tracking.rs, cwd.rs, events.rs,
    hook_dispatch.rs, lifecycle.rs, snapshot.rs, viewport.rs, ...
  terminal_actor/     — owns one pane's libghostty `Terminal` (!Send, in a
                        RefCell on the LocalSet), its input encoders, PTY
                        reader/writer threads, the actor-global live
                        sequence, and coherent bootstrap capture cuts
                        (ADR-0070)
    mod.rs, osc133.rs (OSC-133 command-boundary scanner, phux-foz.4),
    requests.rs, spawn.rs, sync.rs, tick.rs
  grid/               — synthesized-VT compatibility bootstrap/StateSync
                        emitter (never used to construct native records)
    mod.rs, reference.rs, synthesizer.rs
  downsample.rs       — compatibility-profile rewrite of outbound VT bytes
                        (truecolor -> 256/16, OSC 8 / image / KIP gating);
                        native checkpoint/history/raw live bytes bypass it
  agent_detect/       — level-triggered per-terminal agent-state detector
                        (ADR-0046): mod.rs is the state machine (adaptive
                        tick, hysteresis, edge-filtered publish); regions.rs
                        slices the live screen; rules.rs loads the TOML
                        manifests; identify.rs names the agent from the
                        PTY's foreground process; record.rs is the
                        phux.agent/v1 JSON shape
  agent_state.rs      — arbitration between an explicit SET_METADATA and
                        the detector's writes (ADR-0046)
  agent_asked.rs      — the `phux ask` / `asked` event ingress (ADR-0036)
  hooks.rs            — server-side event-hook dispatcher (config
                        `[[hooks.<name>]]` plus plugin `[[events]]`),
                        argv-only execution, no in-process host
  hub/                — federation hub: satellite registry, outbound
                        dialer/link supervisor, byte relay/splice
                        (phux-v45, ADR-0007)
    mod.rs, link.rs, relay.rs
  transport/          — QUIC / TLS / WebTransport listener bindings for
                        remote (non-UDS) attach (ADR-0007, ADR-0031)
    quic.rs, tls.rs, webtransport.rs
  upgrade/            — graceful server re-exec / PTY handoff (ADR-0032)
    mod.rs, blob.rs
  native_state.rs     — native checkpoint bootstrap plumbing (ADR-0070)
  input/              — server-side encoders bridging wire input -> PTY
                        bytes; each pane owns its own PerPane{Key,Mouse,
                        Focus,Paste} encoder, refreshed from Terminal state
    key.rs, mouse.rs, focus.rs, paste.rs, mod.rs
  auth.rs, connector.rs, cwd_query.rs, proc_query.rs, id_bridge.rs,
  policy.rs, search.rs, extract.rs, telemetry.rs
    — auth token checks, outbound connector dialing, kernel cwd/process
      introspection, core<->wire id translation, tracing setup
```

PTY supervision lives inside `terminal_actor/` (two `std::thread`s bridging
blocking `portable_pty` I/O — via `portable-pty-adopt` for re-adoption on
upgrade — to the async actor over `mpsc` channels), not a separate `pty/`
module.

## `phux-client`

The ratatui TUI client. Under ADR-0013 it owns a `libghostty_vt::Terminal`
per attached pane and uses `RenderState` to drive redraw; under
[ADR-0070](../../ADR/0070-native-engine-state-bootstrap.md) it can instead
bootstrap from an exact native checkpoint. `ratatui` is fenced to this
crate; pane-interior substrate lives in `phux-client-core` (see below).

```
src/
  lib.rs              — re-exports of attach::run and the CLI-facing verbs
  attach/             — the attach loop: connection, driver, rendering,
                        input dispatch, fleet/multi-pane orchestration
    mod.rs            — public run(socket, target); ties everything together
    connection.rs     — UDS transport, length-prefixed frame I/O
    quic.rs, ws.rs     — remote transports over phux-dial
    driver.rs         — tokio::select! lifecycle, RawModeGuard RAII. A
                        one-way orchestrator: it owns no shared vocabulary,
                        so no sibling imports from it (phux-4fbs.4, guarded
                        by tests/attach_layering.rs)
    outcome.rs        — AttachError / AttachEnd, the attach exit vocabulary
    pane_state.rs     — PaneSlot, the session-kernel alias, and the
                        client-local VCS / attention indices over them
    server_frame.rs   — decodes server frames into client-side effects
    render.rs, paint.rs, repaint.rs, reflow.rs, rendered.rs
                        — PaneRenderer: feeds TERMINAL_OUTPUT bytes into the
                        local Terminal and paints dirty rows + chrome
    input.rs, input_dispatch.rs, action_registry.rs, actions.rs
                        — StdinParser (keyboard/UTF-8/escape sequences) and
                        the configurable keybinding-to-action pipeline
    fleet.rs, focus.rs — multi-session/pane fleet view and focus tracking
    context_menu.rs, onboarding.rs, plugin_actions.rs, plugin_panes.rs,
    record.rs, terminal_probe.rs, copy.rs, reload.rs, stdout_writer.rs
  render/             — the ratatui chrome layer (status bar, dividers,
                        sidebar, overlays); see render-layering.md
    chrome/           — status_bar.rs, sidebar.rs, dividers.rs
    overlay/          — copy_mode.rs, help.rs, menu.rs, prompt.rs,
                        select_list.rs, selection.rs, toast.rs, which_key.rs
  selector.rs         — client-side TARGET selector resolution (ADR-0021)
  snapshot.rs, run.rs, send_keys.rs, wait.rs, watch.rs, resize.rs,
  layout_ops.rs, ask.rs, agent_meta.rs, vcs.rs, explain.rs
                        — one module per agent-CLI verb's `phux-client`
                        half (docs/consumers/agents.md)
  state.rs            — client-local session/pane state mirror
  testkit.rs          — shared test harness for this crate's own tests
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
                        its libghostty implementation
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
```

`phux-client` depends on this crate and re-exports its modules so consumers
keep stable `phux_client::{layout, multi_pane, predict}` paths. Why the
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
    agent/ (list/show/explain/set/clear/install-claude/config),
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
  it, so the security-sensitive connection path exists once.
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
- **`phux-server-testkit`** — shared scaffolding for `phux-server`'s wire
  integration tests, factored out of a `tests/common` module that used to
  be recompiled once per test binary.
- **`portable-pty-adopt`** — re-adopts an already-running PTY (bare master
  fd + child pid) into `portable-pty`'s trait objects; fills the gap where
  `portable-pty` can only *create* a PTY, needed for the server's
  re-exec/upgrade PTY handoff (ADR-0032).
