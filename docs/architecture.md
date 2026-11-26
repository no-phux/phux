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

The Terminal actor keeps two private temporal owners, on that same thread.
Publication owns consumer caches, live gates, native cuts and cancellation:
bootstrap and activation replies carry drop-owned exact capture capabilities,
and only the actual pump handoff transfers their lifetime. Withdrawal cannot
retire a replacement, even when its opaque cursor is unchanged. The process
owner closes direct and queued input admission before observing EOF, preserves
already-handed-off input uncertainty, and retains the original child identity
and observed exit after releasing the PTY. Neither owner adds a data-plane hop;
rendering remains pooled and queues, credits and native capture interest are
bounded. Host capture contexts expose client-scoped stream effects, not general
server-state borrows.

A stream attach reserves before it awaits. `ATTACH_RESOURCE`, `STREAM_BIND`
and session `ATTACH` each admit their exact generation in the host's resource
table under one lock, which cancels the predecessor's interest immediately.
The source then waits for the predecessor's actual running task before it
emits; an unpublished establishment future never holds up a successor.
Publication, cancellation, fault cleanup, detach and reaping act only on the
reservation they hold, so a delayed older source cannot retire a newer one. A
session `ATTACH` withdraws the reservation of a stream it does not open (an
agent session, a deferred Terminal), and a pane closed mid-`ATTACH` vacates
its slot without cancelling its siblings' publication.

Natural close drains instead of aborting. Close asks the admitted emitter for
a final capture, including an emitter that a pending publication creates after
close began, and the generation keeps its authority after the registry forgets
the resource. `RESOURCE_CLOSED` follows that emitter's final generation.

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
| `phux-plugin-index` | builds the static plugin index from the `phux-plugin` GitHub topic; CI-only, unpublished |
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
- Resource-kind correspondence and parent-first upgrade ordering are checked in
  [`upgrade/kinds/tests.rs`](../crates/phux-server/src/runtime/upgrade/kinds/tests.rs).
  Host-operation and consumer tests cover authority, publication and lifecycle;
  source-directory placement is not an engine-ownership proof.
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

The native connection driver owns shared-control locking, resumable engine
round trips, clocks/backoff and attempt tracing. It releases the control lock
while an engine batch runs, then folds outcomes only into the connection epoch
that issued them; a retired completion cannot reset a replacement publication.
`ControlPlane::new(now, options)` and `observe_time(now)` take explicit monotonic
observations: input, upload and organization expiry never sample a platform
clock. Connected and embedded `Client` frame pumps use the same native handoff.
Direct control-plane drivers must supply fresh observations themselves. Native
client construction/lifetime and callback retirement live separately from the
public synchronous handle.

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

Each resource kind implements the capture/bootstrap and frame-fact
`ResourceApp` interface in `phux-server/src/runtime/apps/`. Host operation
owners in `runtime/commands/`, `runtime/spawn/`, `runtime/upgrade/kinds/` and
`state/org/operation.rs` resolve current authority and exact affected resources,
retain registration ownership, and coordinate persistence, effects and publication.
Apps receive stream/capture capabilities and engine facts, not a generic
command, spawn or upgrade context with unrelated state-table access.
`app_for` is exhaustive over kinds; upgrade ordering is checked against the
catalog. Kind-owned companions preserve direct engine access, and the
executable still injects the `TerminalEngineFactory`.

## Decisions

Current constraints and their reasons are in [decisions](./decisions.md).

## Historical decisions

The retired numbered ADRs are archived, not live, at
[`d21db2e3`](https://github.com/no-phux/phux/tree/d21db2e345aeaff43a681b3a04dfd43f2cc082ca/docs/adr);
search them with `git grep -i <term> d21db2e345aeaff43a681b3a04dfd43f2cc082ca -- docs/adr/`.
