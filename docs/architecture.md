---
group: internals
order: 0
---

# Architecture

How phux is built, where each responsibility lives, and which boundaries the
build enforces. [Concepts](./concepts.md) is the user model,
[protocol](./protocol.md) the wire, [organization](./organization.md) the
durable records, and [decisions](./decisions.md) the constraints.

## One server, many clients

Each user runs one `phux-server` per profile. Runtime and state paths carry
the profile name, so a development build (`phux-dev`) never shares a socket,
sessions, or logs with an installed release. The server hosts resources: an
identity, a kind, a lifecycle, and an ordered output stream. Everything else
is a client: the TUI, Phux Rune, every `phux` subcommand, the MCP adapter, a
remote `phux attach`.

The server runs one tokio current-thread runtime with a `LocalSet`. Each
Terminal is a task on it that owns its PTY master, its libghostty terminal
([libghostty in phux](./libghostty.md)) and its input encoder. libghostty's
`Terminal` is `!Send`, which is why the runtime is single-threaded. Around it
run a writer thread per pane, a shared poller for quiet PTY readers (a busy
pane gets its own reader thread), an input-lane thread that routes and
encodes keys, and blocking pools for transfers and path searches.

Shared state is `ServerState` behind a mutex. Never hold its lock across an
await, and never send a live terminal across threads; input lanes receive
`Send` mode snapshots instead.

`phux upgrade` re-executes the server in place. The old process validates the
candidate binary, then hands it the listening socket and every PTY file
descriptor; processes in panes never notice.

## Bytes out, events in

The wire is asymmetric. Server to client, Terminal content is the raw VT byte
stream from the PTY (`RESOURCE_OUTPUT`) after a bootstrap that brings a fresh
replica up to date. The TUI and Rune feed those bytes to their own libghostty
replicas; the [native terminal client](./clients.md) can instead negotiate
synthesized VT and print one stream to its host emulator. Client to server,
input is structured key, mouse, focus and paste events, which the server
encodes against the terminal's current modes.

A client that wants only text sends `GET_SCREEN` and gets the server's view;
that is `phux snapshot`.

## Crates

Only `phux-protocol` is published. Cargo manifests define the edges.

| Crate | Role |
| --- | --- |
| `phux-protocol` | wire codec, message catalog, capabilities, kind catalog (also the authorization classifier); pure Rust |
| `phux-vt` | libghostty glue: conversions, render pool, replay helpers |
| `phux-layout-codec` | legacy TUI layout envelope |
| `phux-core` | domain records and the pure `apply()` that organizes them; no I/O |
| `phux-config-core` | config location, `extends` layering, profile and socket paths |
| `phux-config` | `config.toml` schema, loader, defaults, validators |
| `phux-design` | [design tokens](./design-language.md) and the theme solver |
| `phux-perf` | lock-free histograms behind `phux perf` |
| `phux-git` | the user's `git`/`gh` behind the server's checkout methods |
| `phux-agent-rules` | agent detection manifests |
| `phux-crash` | fatal-signal handler that restores the terminal |
| `portable-pty-adopt` | rebuild PTY handles from fds inherited across `execve` |
| `quictun` | blind QUIC relay; knows nothing about phux |
| `phux-dial` | outbound QUIC and WebSocket dialing, TLS pinning, relay binding |
| `phux-work` | SQLite work store with a single-writer keeper, and missions |
| `phux-record` | asciicast codec, VT replay, GIF/APNG export |
| `phux-plugin` | plugin action runtime contracts |
| `phux-usage` | plan-usage collector (`phux.usage/v1`) |
| `phux-frozen` | frozen fixtures for persisted formats; test support |
| `phux-client-core` | sans-IO session kernel, layout, predictive echo; compiles to wasm |
| `phux-client-runtime` | native connected client: dialing, reconnect, engine thread, grid publication |
| `phux-client` | headless client behind the CLI and MCP |
| `phux-client-ffi` | the one binding crate (UniFFI Swift and Kotlin) |
| `phux-resource` | kind-neutral substrate: stream, lifecycle, facets, companions |
| `phux-agent-session` | the `AgentSession` record stream and `phux.agent/v1` state |
| `phux-terminal` | the Terminal kind: PTY, libghostty actor, replay, input encoding, agent detection |
| `phux-terminal-api` | engine-independent Terminal contracts and ports |
| `phux-server` | the daemon; links no VT library and no PTY |
| `phux-agent-host` | guardian process for native agent sessions over ACP |
| `phux-tui` | the terminal UI and native VT driver; the only ratatui chrome |
| `phux-mcp` | MCP over stdio |
| `phux-server-testkit` | server wire-test fixtures; dev-only |
| `phux-wire-fixtures` | client-side frame builders; wasm-safe, dev-only |
| `phux` | the `phux` binary: argument parsing and dispatch |

`phux-core` and `phux-protocol` do not depend on each other. The server
bridges their ids in [`id_bridge.rs`](../crates/phux-server/src/id_bridge.rs):
core generational keys are not wire ids, and wire ids are never recycled
within an incarnation. Where both are in scope, alias the wire type with a
`Wire` prefix (`ResourceId as WireResourceId`).

Outside the root workspace: `clients/rune` (Phux Rune, own workspace and
lock), `clients/phux-web` (browser client), `clients/phux-vt-web`
(libghostty compiled to wasm). `integrations/` holds agent-host packages; they
run the `phux` CLI and never speak the wire.

## Enforced boundaries

These hold because a gate fails when they break.

- `bun tools/main.ts build-features-check` reads Cargo's production graph:
  `phux-server`, `phux-terminal-api` and `phux-config` reach no
  `phux-terminal`, libghostty or PTY crate; `phux-server` reaches no
  `phux-client-core`; `phux-mcp` reaches no ratatui.
- [`seam_tests.rs`](../crates/phux-server/src/runtime/apps/seam_tests.rs):
  only an app reaches its engine; host modules use `ResourceApp`.
- [`substrate_boundary.rs`](../crates/phux-resource/tests/substrate_boundary.rs):
  `phux-resource` names no kind's concepts.
- `phux-client-core` builds for wasm in the browser lane (`just/web.just`).
- `design-gen --check` and the generated reference pages fail on hand edits
  or stale exports.

## Clients

`phux-client-core` takes wire frames in and returns effects, with no I/O.
`phux-client-runtime` adds dialing, reconnect, an engine thread for replicas,
and a publication table; Rune and the FFI use it. The TUI and the browser run
the kernel in their own I/O loops. The [consumer contract](./embedding.md)
owns epochs, delivery and draw-list rules.

The TUI paints pane interiors from libghostty through a cell-diffed buffer and
chrome with ratatui in disjoint regions. Rune paints the runtime's draw list
with the [Rune](https://github.com/anomalyco/rune) framework.

### Layout stability

A click, selection, hover or scroll never resizes or moves other UI unless
resizing is the point (dragging a split or the sidebar edge). Chrome slots
refuse to shrink; a label that changes on click sits in a slot sized for its
widest value; a hint shown on one row stays laid out, invisible, on the
others; controls depend on the mode, never on the highlighted row.
Regression tests assert identical bounds across the interaction.

## Agents and authority

The server derives agent state on a tick from the foreground process and the
manifest rules, with hysteresis; a host integration's lifecycle stream
outranks detection. The record is published as `phux.agent/v1` metadata.

Authority is checked on every dispatch, not only at admission: the kind
catalog classifies each frame and the grant must cover it
([remote](./remote.md), [agents](./agents.md)). Work and missions use a
separate keeper store ([organization](./organization.md#work-and-missions),
[substrate](./substrate.md)).

## Where to look

| To change | Start at |
| --- | --- |
| server behavior | `phux-server/src/runtime/mod.rs`, `runtime/connections/` (`hello`, `dispatch`, `writer`), `state/mod.rs`, `runtime/apps/` |
| organization | `phux-core/src/apply.rs`, `phux-server/src/state/org.rs`, `org_store/` |
| input handling | `phux-server/src/runtime/apps/terminal/input_lane/` |
| TUI rendering | `phux-tui/src/attach/render.rs`, `attach/paint.rs`, `render/chrome`, `render/overlay` |
| the wire | `phux-protocol/src/wire/frame/catalog.rs`, `wire/frame/{tags,command_tags}.rs`, `wire/field/`, `caps/server_features.rs`, `kinds.rs`; then [regenerate](./protocol.md) |
| a CLI verb | `phux/src/commands/cli.rs`, `commands/cli/`, `commands/<verb>.rs`, `src/dispatch.rs`, `phux-client/src/`; then `just docs-gen` |
| config | `phux-config/src/schema.rs`, `src/default/*.toml`; then `just docs-gen` |
| agent detection | `phux-agent-rules/rules/*.toml`, `phux-terminal/src/agent_detect/` |
| authorization | `phux-server/src/policy.rs`, `workload.rs`, `phux-protocol/src/kinds.rs` |
| remote access | `phux/src/commands/enroll.rs`, `remote_target.rs`, `phux-client-runtime/src/dial.rs`, `phux-server/src/transport/` |

Each resource kind implements `ResourceApp` in `phux-server/src/runtime/apps/`.
`app_owning` routes facet commands by the catalog rows authorization uses;
`app_for` routes spawn, stream frames, bootstrap and upgrade by kind. `state/`
sits below the apps, reaches engines through `ResourceOps`, and holds app state
in typed slots it cannot read (`state/app_slots.rs`). A new kind adds an
exhaustive `app_for` arm, every `ResourceApp` and `ResourceOps` method, and an
`APP_KINDS` entry checked against the catalog. A surface only some kinds offer
(cell size, input lease, record stream) is a companion typed by its owning
crate. The executable injects the `TerminalEngineFactory`; the host selects
no engine. See [resource kinds](./resource-kinds.md).

## Decisions

Current constraints and their reasons are in [decisions](./decisions.md).

## Historical decisions

The retired numbered ADRs are archived, not live, at
[`d21db2e3`](https://github.com/no-phux/phux/tree/d21db2e345aeaff43a681b3a04dfd43f2cc082ca/docs/adr);
search them with `git grep -i <term> d21db2e345aeaff43a681b3a04dfd43f2cc082ca -- docs/adr/`.
