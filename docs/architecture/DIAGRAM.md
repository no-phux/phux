---
audience: humans, contributors, agents
stability: scratch
last-reviewed: 2026-09-09
---

# System shape diagram

**TL;DR.** phux is a libghostty-backed control plane that serves resources. The canonical state of every resource lives server-side behind a kind engine; clients attach over one frame codec on any of five byte streams and keep local replicas for rendering. This sketch shows the path from a Terminal's PTY through the server and the wire to a client's screen, and where a producer-fed AgentSession joins it.

---

```
┌──────────────┐                     ┌────────────────────────────┐
│     PTY      │  shell / agent      │  agent harness shim        │
│   (child)    │  process            │  (`phux agent emit`)       │
└────────┬─────┘                     └──────────────┬─────────────┘
         │ VT bytes                                 │ JSONL records
         ▼                                          ▼ APPEND_RESOURCE_OUTPUT
┌────────────────────────────────────────────────────────────────────┐
│   SERVER (one per user, one current-thread runtime + LocalSet)     │
│                                                                    │
│   ServerState (std::sync::Mutex; never held across an await)       │
│     registry (sessions, windows, resources + parents) · id bridge  │
│     L3 metadata store · leases · ResourceTable (ResourceHandles)   │
│                                                                    │
│   one spawn_local task per resource = ResourceCore + kind engine   │
│   ┌──────────────────────────────┐   ┌──────────────────────────┐  │
│   │ Terminal engine              │   │ AgentSession engine      │  │
│   │ (resource::terminal,         │◄──│ (record ring; parent =   │  │
│   │  ADR-0014)                   │   │  the Terminal)           │  │
│   │  - libghostty Terminal       │   │  see Status              │  │
│   │  - PTY reader/writer threads │   └──────────────────────────┘  │
│   │  - input encoders            │                                 │
│   │  - bootstrap cuts (ADR-0070) │                                 │
│   │  - state-sync + detector tick│                                 │
│   └──────────────────────────────┘                                 │
└───────────────────┬────────────────────────────────┬───────────────┘
                    │ per-connection                 │
                    │ FrameReader / FrameWriter      │
                    ▼                                ▼
       ┌────────────────────────────────────────────────────────┐
       │  one frame codec, five byte streams                    │
       │  UDS · WebSocket · QUIC · WebTransport · SSH-stdio     │
       │                                                        │
       │  server → client: BOOTSTRAP_* / RESOURCE_OUTPUT bytes, │
       │                   lifecycle, EVENT, metadata           │
       │  client → server: INPUT_* atoms (Terminal kind only),  │
       │                   commands, metadata, append           │
       └──────────────┬─────────────────────────┬───────────────┘
                      │                         │
                      ▼                         ▼
┌────────────────────────────────┐   ┌──────────────────────────────┐
│  TUI client (phux-tui)         │   │  headless consumers          │
│                                │   │  phux agent verbs, phux-mcp, │
│  session kernel                │   │  phux-web, Cockpit via FFI   │
│  (phux-client-core)            │   │  (same kernel, no ratatui)   │
│   - libghostty replica per     │   └──────────────────────────────┘
│     Terminal-kind resource     │
│   - predictive echo            │
│  ratatui chrome                │
│   - layout tree, status bar,   │
│     sidebar, keybindings       │
│   - L3 metadata rendering      │
└────────────────┬───────────────┘
                 ▼
          terminal screen
```

---

## Key invariants

### Canonical state (server)

Each served resource is one `spawn_local` task: a generic `ResourceCore`
(output sequence and broadcast, event fan-out, cancel token, control
mailbox) embedded in the engine for its kind. The runtime holds a
`Send + Clone` `ResourceHandle` per resource in the `ResourceTable` and
reaches Terminal-only channels through `ResourceHandle::terminal()`. For
the Terminal kind the engine owns the `libghostty_vt::Terminal`: the full
parsed grid, modes, parser continuation, and retained history, fed by the
PTY and supervised on the `LocalSet` ([threading.md](./threading.md)).
Sessions and windows are grouping metadata over resources, not a
collection tier ([data-model.md](./data-model.md)).

### Local replica (client)

A client keeps its own `libghostty_vt::Terminal` per attached Terminal-kind
resource as a replica for rendering. It is never the source of truth: it
renders, caches what was painted last frame, reconciles predictions when
server bytes arrive, and serves scrollback search and selection locally.
Both engines are the same libghostty parser; nothing re-encodes in the
middle (ADR-0013).

### The frame seam, not a trait

There is no `Transport` trait. The server's accept loop is generic over a
listener that yields a `FrameReader` / `FrameWriter` pair per connection;
the client holds one enum of each behind its `Connection`; the hub's
satellite links have their own pair. Above the seam nothing names a stream
type. Details in [transport.md](./transport.md).

### Data direction

- **PTY -> server -> wire -> client**: VT bytes (Terminal output), after a
  per-client capability rewrite on the synthesized profiles and untouched
  on the native profile (ADR-0070).
- **Client -> wire -> server -> PTY**: structured input events (key, mouse,
  focus, paste), encoded to PTY bytes on the server's input lane.
- **Producer -> wire -> server -> subscribers**: for a producer-fed kind,
  appended records fan out as opaque output bytes under that kind's codec
  (ADR-0103; see Status).

The wire is asymmetric: one direction is bytes, the other is structured
events. That is the core invariant from ADR-0013.

---

## Scopes

| Scope | Lives | Carries |
|---|---|---|
| **Resource** | Server (L1 wire) | Id, kind, optional parent, lifecycle, ordered output stream, bootstrap, events |
| **Terminal** (kind 0) | Server (L1 facet) | PTY, canonical grid, cols/rows/title/cwd, structured input |
| **AgentSession** (kind 1) | Server (L1 facet) | Provider, native id, derived state, appended JSONL records; always a Terminal child |
| **Session / Window** | Server registry + TUI (L3 metadata) | Grouping, layout tree, focus |
| **Pane** | TUI client | A Terminal-kind resource in a layout slot |

The wire defines resources and their facets. The TUI defines sessions,
windows, and panes as one way to arrange Terminal-kind resources. A headless
consumer speaks L1 plus whatever L3 keys it chooses.

---

## Cold-read digest

1. **Start top-left**: the PTY emits VT bytes; a harness shim emits records.
2. **Into the server**: one engine per resource owns canonical state.
3. **Across the seam**: one codec, five streams; bytes one way, structured
   events the other.
4. **Client side**: a libghostty replica per Terminal-kind resource mirrors
   the server for rendering.
5. **Chrome**: ratatui decorates the grid with layout, status bar, sidebar.

---

## Status

| Gap | Today | Owner | Tracked |
|---|---|---|---|
| AgentSession engine and `APPEND_RESOURCE_OUTPUT` | `ResourceFacetHandle` has only the `Terminal` variant; nothing in `phux-server` accepts appended records, and agent state comes from hooks and screen scraping. | [ADR-0103](../../ADR/0103-agent-session-resource-and-producer-fed-streams.md) | phux-am9y.9 |
| Server-side cascade close with `CloseReason::ParentClosed` | Parent bindings and their cascade exist in the `phux-core` registry; the runtime spawns no child resources and `RESOURCE_CLOSED` carries no reason. | [ADR-0104](../../ADR/0104-parent-bindings-are-l1-lifecycle.md) | phux-am9y.10 |

## See also

- [`docs/CONCEPTS.md`](../CONCEPTS.md) — the mental model
- [`transport.md`](./transport.md) — the frame seam and the five streams
- [`process-model.md`](./process-model.md) — server/client lifecycle
- [`render-layering.md`](./render-layering.md) — client-side rendering split
- [ADR-0013](../../ADR/0013-libghostty-bytes-on-wire.md) — libghostty bytes on the wire
- [ADR-0007](../../ADR/0007-mosh-class-transport-and-satellites.md) — transports and satellites
