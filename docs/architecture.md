# Architecture

For people changing phux. [Concepts](./concepts.md) is the user-level model;
[protocol](./protocol.md) is the wire. [Organization](./organization.md) owns
durable records, activations and restore; [remote authority](./remote.md#connection-authority)
owns grants and revocation; [embedding](./embedding.md#runtime-consumer-contract)
owns the runtime consumer boundary. [Substrate](./substrate.md) is the org's
principals, home keeper, and adapters.

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
and encodes keystrokes, and blocking pools for uploads, downloads, and path
searches.
Shared state is a `ServerState` behind a mutex; never hold its synchronous
lock across an await or send a live `!Send` terminal across threads. Input
lanes receive Send mode snapshots, not another borrower of the terminal.

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
`phux-protocol`. Rows describe responsibilities, not a dependency ordering;
Cargo manifests define the actual edges.

| Crate                 | Role                                                                                                                                                  |
| --------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------- |
| `phux-protocol`       | wire format, message catalog, codec, capabilities, the kind catalog that doubles as the authorization classifier; pure Rust, no libghostty by default |
| `phux-core`           | domain records: sessions, resources, arrangements, and the pure `apply()` that organizes them; no I/O                                                 |
| `phux-config-core`    | config file location, `extends` layering, `[[remote]]`, `[organize]`, profile and socket paths                                                        |
| `phux-perf`           | lock-free histograms and counters behind `GET_PERF` and `phux perf`                                                                                   |
| `phux-git`            | the user's own `git` (and `gh`) for a checkout: status, diffs, history, branches, files, staging; shared by the desktop Changes panel and `CHECKOUT` |
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
| `phux-usage`          | plan-usage collector behind `phux agent usage`: vendor limit sources and the `phux.usage/v1` document; no wire                                        |
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
bridges their id spaces through [IdBridge](../crates/phux-server/src/id_bridge.rs):
core generational keys are not wire ids; wire ids are monotonically allocated
and retired, not recycled within an incarnation. Where both are in scope, import the protocol type
with a `Wire` prefix (`use phux_protocol::ids::ResourceId as WireResourceId`).

Outside the root workspace: `clients/desktop/native` (the desktop and
`phux-gpui` workspace), `clients/phux-web` (browser client), `clients/phux-vt-web`
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
The [runtime consumer contract](./embedding.md#runtime-consumer-contract)
owns epochs, delivery acceptance and callback retirement. The runtime's
`draw::DrawList` shares colour, face, cursor, block and
decoration rules across GPUI and the C ABI's `c/draw_list.rs`; other
toolkits paint that same list. See [embedding](./embedding.md). `phux-client-ffi` derives one binding
vocabulary and encodes it twice: `extern "C"` for native embedders, UniFFI
for Swift and Kotlin.

The [pane identity seam](./desktop-panes.md#classification-and-extension)
keeps the same classification across cards, tabs, and navigator rows. The
[Changes panel seam](./desktop-changes.md#implementation-guarantees) keeps
local Git work off the UI thread and rejects results for a stale folder.

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
- The desktop navigator's host filter and per-session disclosure are independent
  of selection. Every session keeps its pane rows and count when inactive;
  sidebar tabs have a fixed-height scrollable shelf outside the session tree.
  Before switching authorities, retain the leaving connection's inventory so a
  still-dialing observer cannot remove that machine from the navigator. Initial
  layout adoption seeds display preferences; navigation restores destination
  tabs/splits without replacing the current window's display preferences.

Regression tests assert bounds are identical across the interaction, for
example `switching_tabs_never_resizes_the_chrome` and
`moving_the_highlight_never_resizes_the_palette`.

## Frames, wakes, and input

The runtime owns client orchestration; the [renderer contract](./embedding.md#frames-wakes-and-input)
owns drain/invalidation and unknown-input recovery. Phux Desktop requires
visible, unclipped drawable presentation before clearing an unknown-input
fence. The unpatched embedding lane does not provide that product guarantee.

## Agents, authority, work

Agent state is derived on the server, level-triggered on a tick:
identify the agent from the foreground process group, wait out a startup
grace, apply the manifest rules, apply hysteresis, publish only changes.
A host integration's lifecycle stream, when present, outranks detection.
The record is published as `phux.agent/v1` metadata and consumed by every
client.

Connection authority is checked on every dispatch, not just at admission.
The kind catalog classifies each frame; the grant must cover its verbs and
authoritative subject. [Connection authority](./remote.md#connection-authority)
distinguishes ordinary owner sockets, delegated sockets, scoped mTLS, live
revocation and reverse-hub trust. [Agents](./agents.md#safety) owns input
leases and held approvals; humans retain input priority.

[Work and missions](./organization.md#work-and-missions) use a separate
keeper/store, distinct from the organization database. The org's home
keeper is the [substrate decision](./substrate.md). A multi-host
coordinator and autonomous Lead are not shipping contracts.

## Where to look

| To change       | Start at                                                                                                                                                                                                     |
| --------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| server behavior | `phux-server/src/runtime/mod.rs` (accept, lifecycle), `runtime/client.rs` (`handle_client`, `dispatch`, HELLO), `state/mod.rs`, `resource/terminal/`                                                         |
| organization | `phux-core/src/apply.rs`, `phux-server/src/state/org.rs`, `org_store.rs`; [durability and execution](./organization.md) |
| input handling  | `phux-server/src/runtime/input_lane/`                                                                                                                                                                        |
| TUI rendering   | `phux-tui/src/attach/render.rs`, `attach/paint.rs`, `render/chrome`, `render/overlay`                                                                                                                        |
| the wire        | `phux-protocol/src/wire/frame/mod.rs` (types and tags), `wire/field.rs` (field ids), `caps/features.rs` (feature bits), `kinds.rs` (classification); then regenerate `docs/spec/wire-schema.json` and `just docs-gen` |
| a CLI verb      | `crates/phux/src/commands/mod.rs` (`enum Command`), `src/lib.rs` (dispatch), `commands/<verb>.rs`, wire logic in `phux-client/src/<verb>.rs`; then `just docs-gen`                                           |
| config          | `phux-config/src/schema.rs`, `default.toml`; then `just docs-gen`                                                                                                                                            |
| agent detection | `phux-agent-rules/rules/*.toml`, `phux-server/src/agent_detect/`                                                                                                                                             |
| authorization   | `phux-server/src/policy.rs`, `workload.rs`, `phux-protocol/src/kinds.rs`                                                                                                                                     |
| remote access   | `crates/phux/src/commands/enroll.rs`, `remote_target.rs`, `phux-client-runtime/src/dial.rs`, `phux-server/src/transport/`                                                                                    |

## Decisions

The current architectural constraints, with their reasons, are in
[decisions](./decisions.md).

## Historical decisions

The 189 numbered ADRs are preserved unchanged at the last integrated
ADR-bearing main snapshot,
[`d21db2e345aeaff43a681b3a04dfd43f2cc082ca`](https://github.com/no-phux/phux/tree/d21db2e345aeaff43a681b3a04dfd43f2cc082ca/docs/adr).
This is main history, not an ephemeral PR-branch commit. Current contracts
remain in the live guides and the constraints above; history supplies the
original trade-offs, rejected alternatives, and context.

From a Git checkout, browse the index or search the complete archive:

```sh
git show d21db2e345aeaff43a681b3a04dfd43f2cc082ca:docs/adr/README.md
git grep -n -i 'lease' d21db2e345aeaff43a681b3a04dfd43f2cc082ca -- docs/adr/
git show d21db2e345aeaff43a681b3a04dfd43f2cc082ca:docs/adr/0187-agents-coordinate-through-phux.md
```

A shallow clone may not have that object. Fetch the exact snapshot, then use
the same commands (or browse the pinned link without a local checkout):

```sh
git fetch origin d21db2e345aeaff43a681b3a04dfd43f2cc082ca
```

Do not revive historical constraints merely because a search found them. Check
supersession, proposals, current code/tests, and the live contract. Conversely,
if a still-operative constraint exists only in history, promote it to its live
home with evidence. New decisions extend the current architecture and link
their PR rather than creating a second competing source of authority. The
[substrate](./substrate.md) decision is that extension: principals, one home
keeper, and one-way adapters, linked from [Decisions](#decisions) and
[#1689](https://github.com/no-phux/phux/pull/1689).
