# Architecture

For people changing phux. [Concepts](./concepts.md) is the user-level model;
[protocol](./protocol.md) is the wire.

## One server, many clients

Each user runs one `phux-server` process per profile. It owns the
pseudo-terminals, the processes in them, and a libghostty `Terminal` per
pane holding the canonical screen state. Everything else is a client: the
TUI, the desktop app, every `phux` subcommand, the MCP adapter, a remote
`phux attach`.

Inside the server, one tokio current-thread runtime drives a `LocalSet`.
Each pane is a task on it that owns its PTY master, its libghostty
terminal, and its input encoder; libghostty's `Terminal` is `!Send`, which
is the reason for the single-threaded runtime. Around that runtime: a
writer thread per pane, a shared poller that wakes quiet PTY readers (a
busy pane gets its own reader thread), one input-lane thread that routes
and encodes keystrokes, and blocking pools for uploads and path searches.
Shared state is a `ServerState` behind a mutex.

`phux upgrade` re-executes the server binary in place: the old process
hands its listening socket and every PTY file descriptor to the new one,
which re-adopts them. Processes in panes never notice.

Runtime and state paths carry the profile name, so a development build
(`phux-dev`) never shares a socket, sessions, or logs with an installed
release.

## Bytes out, events in

The wire is asymmetric. Server to client, terminal content is the raw VT
byte stream from the PTY, forwarded as `RESOURCE_OUTPUT`, with a bootstrap
sequence to bring a fresh replica up to date. Each rendering client runs
its own libghostty replica and feeds it those bytes. Client to server, input
is structured: key, mouse, focus, and paste events with libghostty's atoms,
encoded to bytes by the server against the terminal's current modes. Both
ends therefore agree on what a terminal is without either trusting the
other's rendering.

A headless client that only wants text asks the server for `GET_SCREEN`
and gets the server's view, which is what `phux snapshot` does.

## Crates

All under `crates/`. Every crate is `publish = false` except
`phux-protocol`. Lower rows depend on nothing above them.

| Crate                 | Role                                                                                                                                                  |
| --------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------- |
| `phux-protocol`       | wire format, message catalog, codec, capabilities, the kind catalog that doubles as the authorization classifier; pure Rust, no libghostty by default |
| `phux-core`           | domain records: sessions, resources, arrangements, and the pure `apply()` that organizes them; no I/O                                                 |
| `phux-config-core`    | config file location, `extends` layering, `[[remote]]`, `[organize]`, profile and socket paths                                                        |
| `phux-perf`           | lock-free histograms and counters behind `GET_PERF` and `phux perf`                                                                                   |
| `phux-agent-rules`    | agent detection manifests: regions, rules, `phux agent explain`                                                                                       |
| `phux-crash`          | fatal-signal handler that restores the terminal before re-raising                                                                                     |
| `portable-pty-adopt`  | rebuild portable-pty handles from inherited fds after `execve`                                                                                        |
| `quictun`             | blind QUIC relay; knows nothing about phux                                                                                                            |
| `phux-work`           | SQLite work store with a single-writer keeper process                                                                                                 |
| `phux-config`         | the TUI `config.toml` schema, loader, widget contract                                                                                                 |
| `phux-dial`           | outbound QUIC and WebSocket dialing: TLS pinning, bearer token                                                                                        |
| `phux-client-core`    | sans-IO session kernel, engine adapter, layout, multi-pane, predictive echo; no tokio, no ratatui, compiles to wasm                                   |
| `phux-record`         | asciicast codec, VT replay, GIF/APNG export                                                                                                           |
| `phux-mission`        | mission documents over the work store                                                                                                                 |
| `phux-plugin`         | plugin action runtime contracts                                                                                                                       |
| `phux-relay`          | quictun bound to phux's ALPNs and state directory                                                                                                     |
| `phux-client-runtime` | the one client orchestration layer: dial planning, reconnect, relay tunnel, control plane, engine owner thread, grid publication                      |
| `phux-client`         | headless client: the connection facade and the verbs behind the CLI and MCP                                                                           |
| `phux-client-ffi`     | the one binding crate: a projection with a C ABI encoder and a UniFFI encoder                                                                         |
| `phux-server`         | the daemon                                                                                                                                            |
| `phux-agent-host`     | guardian process for native agent sessions over ACP                                                                                                   |
| `phux-tui`            | the terminal UI: attach driver, ratatui chrome, overlays; the only crate that links ratatui                                                           |
| `phux-mcp`            | the `phux-mcp` binary: MCP over stdio                                                                                                                 |
| `phux-server-testkit` | fixtures for server wire tests; dev-only                                                                                                              |
| `phux`                | the `phux` binary: argument parsing and dispatch                                                                                                      |

`phux-core` and `phux-protocol` do not depend on each other; the server
bridges their id spaces. Where both are in scope, import the protocol type
with a `Wire` prefix (`use phux_protocol::ids::ResourceId as WireResourceId`).

Outside the workspace, each with its own `Cargo.toml`: `clients/desktop`
(GPUI app), `clients/phux-web` (browser client), `clients/phux-vt-web`
(driver for libghostty compiled to wasm). `integrations/` holds the npm
packages for agent hosts; they run the `phux` CLI and never speak the wire.

## Clients

`phux-client-core` is the part every client shares: a synchronous kernel
that takes wire frames in and produces effects (paint this, send that),
with no I/O of its own. `phux-client-runtime` wraps it with the async
parts: dialing, reconnecting, the engine thread that owns libghostty
replicas, and double-buffered grid publication. The TUI, the desktop app,
and the FFI bindings all sit on the runtime; the browser client sits on the
core alone and does its own I/O in the page.

The TUI paints pane interiors from libghostty through a cell-diffed front
buffer and paints chrome with ratatui in disjoint regions. The desktop app
uses `clients/desktop/native/phux-gpui`, the embeddable GPUI terminal view.
The runtime's `draw::DrawList` shares colour, face, cursor, block and
decoration rules across GPUI and the C ABI's `c/draw_list.rs`; other
toolkits paint that same list. See [embedding](./embedding.md). `phux-client-ffi` derives one binding
vocabulary and encodes it twice: `extern "C"` for native embedders, UniFFI
for Swift and Kotlin.

## Layout stability

A click, a selection, a hover or a scroll never resizes or moves other UI
unless the resize is the point, such as dragging the sidebar edge or a split.
Chrome keeps the size its style names. Something that appears on interaction
either has space held for it or floats over the content:

- A cached chrome slot (titlebar, sidebar, overlays) is a layout leaf with no
  measured content, so `chrome_slot` makes it refuse to shrink. Fixed boxes in
  a flex row take `flex_shrink_0`.
- A label that changes on click is laid out in a slot sized by its widest
  label. A hint shown only on the highlighted row stays laid out, invisible,
  on the others.
- Controls depend on the mode, never on the highlighted row. Open details
  keep their height when the highlighted row has none.

Regression tests assert bounds are identical across the interaction, for
example `switching_tabs_never_resizes_the_chrome` and
`moving_the_highlight_never_resizes_the_palette`.

## Frames, wakes, and input

One listener and one event drain exist per runtime client. Drain, then
invalidate affected terminals. A removed slot clears stale presentation even
when its generation equals the cached number. Never publish a cached frame as
evidence of recovery.

Raw-send success is a local queue result, not delivery. Unknown delivery is
terminal-wide. `acknowledge_projection` runs only from GPUI's drawable-presented
callback, after the window is visible and the terminal bounds are unclipped.
Paint and `on_next_frame` do not clear the fence. A late callback must not
clear a newer fence.

## Agents, authority, work

Agent state is derived on the server, level-triggered on a tick:
identify the agent from the foreground process group, wait out a startup
grace, apply the manifest rules, apply hysteresis, publish only changes.
A host integration's lifecycle stream, when present, outranks detection.
The record is published as `phux.agent/v1` metadata and consumed by every
client.

Every connection holds one grant, minted at `HELLO`: the owner's full
authority over the Unix socket, or a scoped grant from the mTLS workload
registry for anything remote. Each dispatched frame is classified by the
kind catalog into the verb it needs, and the grant must hold that verb.
Scoped grants can be configured to hold kill and signal actions for human
approval. Input leases let one connection take exclusive input to a pane;
agent and automation principals cannot take a lease a human holds.

`phux-work` is a per-host SQLite store with one keeper process as its only
writer; `phux mission` builds on it. A multi-host coordinator is designed
and not built.

## Where to look

| To change       | Start at                                                                                                                                                                                                     |
| --------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| server behavior | `phux-server/src/runtime/mod.rs` (accept, lifecycle), `runtime/client.rs` (`handle_client`, `dispatch`, HELLO), `state/mod.rs`, `resource/terminal/`                                                         |
| input handling  | `phux-server/src/runtime/input_lane/`                                                                                                                                                                        |
| TUI rendering   | `phux-tui/src/attach/render.rs`, `attach/paint.rs`, `render/chrome`, `render/overlay`                                                                                                                        |
| the wire        | `phux-protocol/src/wire/frame/mod.rs` (types and tags), `wire/field.rs` (field ids), `caps.rs` (feature bits), `kinds.rs` (classification); then regenerate `docs/spec/wire-schema.json` and `just docs-gen` |
| a CLI verb      | `crates/phux/src/commands/mod.rs` (`enum Command`), `src/lib.rs` (dispatch), `commands/<verb>.rs`, wire logic in `phux-client/src/<verb>.rs`; then `just docs-gen`                                           |
| config          | `phux-config/src/schema.rs`, `default.toml`; then `just docs-gen`                                                                                                                                            |
| agent detection | `phux-agent-rules/rules/*.toml`, `phux-server/src/agent_detect/`                                                                                                                                             |
| authorization   | `phux-server/src/policy.rs`, `workload.rs`, `phux-protocol/src/kinds.rs`                                                                                                                                     |
| remote access   | `crates/phux/src/commands/enroll.rs`, `remote_target.rs`, `phux-client-runtime/src/dial.rs`, `phux-server/src/transport/`                                                                                    |

## Decisions

The choices that shape the code, stated as they stand. Code comments cite
them as `ADR-NNNN`; those records live in the
[latest main snapshot](https://github.com/no-phux/phux/tree/3cd9ddac22075c1b3db2674e2edc4e00c91d67f4/docs/adr).
This private Git-only deletion experiment has no approved archive choice.
Owner choice is pending; this snapshot accounts for all 189 numbered records,
including the current coordination, quick-capture and embedding decisions.
Accepted record bodies remain unchanged in that tree.
A new decision that closes off a design space gets a bullet here, with the
PR that made it.

**Process and runtime**

- One server per user per profile; one current-thread runtime; a pane is a
  `LocalSet` task that owns its `!Send` libghostty terminal.
- Upgrades re-exec the server and re-adopt PTYs; a server restart is never
  the reason a process dies.
- Exit is a facet, close is a purge: an exited pane stays inspectable until
  it is removed or its retention expires.
- Events are journaled and resumable; a reconnecting watcher gets what it
  missed or a gap marker, never silence.

**Wire**

- Server to client is VT bytes; client to server is structured input. Both
  sides run libghostty.
- Clients must match the server's protocol `major.minor`. New behavior ships
  as a feature bit or a skippable field; a version bump is for what no
  additive shape can express.
- The kind catalog in `phux-protocol::kinds` is the one table for discovery
  and authorization; anything it does not name is denied.
- `wire-schema.json` is generated from the codec and test-pinned; the code is
  the specification.
- `phux-core` and `phux-protocol` stay independent; the server bridges them.

**Clients**

- `phux-client-core` is sans-IO and dependency-light so it compiles to wasm.
  `phux-client-runtime` is the one place client orchestration lives.
  `phux-client-ffi` is the one binding crate.
- Pane interiors are libghostty's; chrome is the client's. The two never
  paint the same cell.

**Agents and authority**

- The server derives agent state. A host's lifecycle stream outranks the
  detector; a declaration outranks both.
- The owner socket has full authority. Everything remote is a scoped mTLS
  grant plus a bearer token; bearer-only and anonymous paths are gone.
- Clients pin the server's certificate authority at pairing and never learn
  one from a connection. Devices and satellites enroll with single-use
  tickets.
- Humans outrank agents on input leases. Scoped grants can be held for
  approval; the owner never is.

**Not doing**

- No embedded scripting language: commands are typed messages; logic lives
  in scripts that call `phux`.
- No in-process plugin host: plugins are external processes declared in
  config.
- No homegrown selection or search engine beyond find-in-scrollback:
  selection and extraction belong to libghostty.
- No homegrown crypto: TLS, SSH, and Unix permissions.
- No tmux behavior "for compatibility" where phux has a reason to differ.

**Release**

- Release Please versions from conventional commits. Checksums are minisign
  signed and the installer verifies them. One manifest per channel.
- Generated reference pages are byte-pinned by a test; hand edits fail CI.
