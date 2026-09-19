---
audience: consumers, contributors, agents
stability: evolving
last-reviewed: 2026-09-12
---

# The internal Rust client library

**TL;DR.** `phux-client` is the unpublished, in-tree Rust library behind
the CLI and MCP adapter. Its programmatic control surface is a set of
async free functions for selection, snapshots, input, commands, waits,
events, and asks; it has no typed client facade or stable public SDK
contract. External callers should use the CLI or MCP adapter.

---

## What it is

There is no published `phux-client-sdk` crate and no gRPC service.
`phux-client` exists inside the workspace and powers the CLI agent verbs
and the [MCP adapter](./mcp.md), but downstream consumers cannot depend
on a versioned crates.io package. It wraps the `phux-protocol` codec and
the internal resolution, snapshot, run, and wait functions used by the
binaries. The crate is workspace-internal (`publish = false`).

Native embedders — Cockpit on macOS, and any other in-tree or
out-of-tree host that wants a C ABI rather than a CLI subprocess — use
`phux-client-ffi`. That crate is also workspace-internal: a stable
native C ABI over the `phux-client-core` session kernel, not a crates.io
SDK.

Rust native adapters that provide their own ABI layer may depend directly on
two narrower crates instead of importing `phux-client` or its C ABI:

- `phux-client-core` owns synchronous stream state (`SessionKernel`),
  predictive reconciliation, and `input_replay::InputReplayJournal`.
- `phux-dial` owns native QUIC/WebSocket establishment and pinned TLS, ending
  at an opaque byte stream.

This is intentionally not a socket/session runtime. The host owns its async
executor, reconnect loop, frame reader/writer, credentials, monotonic clock,
operation-id CSPRNG, and UI delivery. It feeds decoded kernel inputs through
`SessionKernel::update_at`, executes the returned effects without re-entering
the update, and keeps the kernel's replica state alive across UI projections.
Topology browsing is a normal state projection; no native server engine is
imported into the browser-safe core.

For acknowledged input, keep one `InputReplayJournal` across reconnects. Mint
one non-zero 128-bit id per user action, call `submit_at` with the immutable
event batch and host monotonic time, send each `next_frames_at` result in
order, roll back only the suffix never handed to transport, and pass owned
command results to `resolve`. Call `connection_lost`, then
`begin_connection_at` with the negotiated server incarnation and
`ACKNOWLEDGED_INPUT` capability. Never journal fire-and-forget raw input. The
journal enforces per-Terminal ordering, bounded retention, same-incarnation
replay, and the ten-minute dedupe horizon; its `Unknown` versus `Refused`
report is the UI's safe-to-retype boundary.

For a mobile dial, construct `WsDial` or `QuicDial` with `CertTrust::Pinned`
and call the transport's `dial_with_identity(..., &TlsClientIdentity::None)`.
Those explicit paths never read `PHUX_WORKLOAD_CERT` or `PHUX_WORKLOAD_KEY`; a
host that really owns an mTLS identity can instead pass explicit `PemFiles`
paths. The returned WebSocket or QUIC stream is still only transport. The
adapter owns HELLO negotiation, frame flow, reconnect timing, and delivery
into `SessionKernel`.

For finished-pane history, opt a projected terminal into
`set_retain_replica_on_close` before close, then transfer its final engine
generation with `take_closed_replica`. Retention is off by default. This avoids
a parallel raw-byte archive without growing clients that do not use it. Bounded frontends may detach
an attached terminal after the aggregate ready barrier with `detach_terminal`,
then explicitly subscribe again when it becomes resident; the server, not a
second client buffer, remains authoritative while it is detached.

Cockpit remains the first-class C consumer of the same seams. Its Zig provider
calls `phux-client-ffi`, which is a thin native-engine adapter over
`SessionKernel`; its remote tunnel is `phux-client-runtime`'s, behind a C
handle (ADR-0133). Do not route that C ABI through a mobile FFI wrapper. Cockpit's
current key, mouse, focus, and paste calls intentionally use the kernel's raw
`KernelAction::Input` path: they are latency-sensitive, fire-and-forget input
and therefore must not enter the acknowledged replay journal. Cockpit does not
currently paint predictive cells, so it has no second predictor policy. If
either acknowledged reconnect replay or predictive paint is added to that UI,
the C exports must adapt `phux-client-core::input_replay` or `predict` directly
rather than reimplementing either state machine in Zig.

Its transport operations speak **L1**, the terminal substrate
([`../spec/L1.md`](../spec/L1.md)): terminal lifecycle, input atoms,
snapshots, and events. The crate also contains client-side L3 helpers
for session/window selection and layout; those model conventions over L1
state rather than adding a privileged wire tier
([`../spec/L3.md`](../spec/L3.md)).

It is one consumer among peers — the reference TUI, the
[web client](./web.md), the [CLI agent surface](./agents.md), and the
[MCP adapter](./mcp.md) — none protocol-privileged
([ADR-0017](../adr/0017-tui-not-protocol-privileged.md)).

## How it fits the projection thesis

[ADR-0030](../adr/0030-engine-delegated-wire-and-projection-consumers.md)
states the wire carries opaque terminal bytes, not structured screen
state. A consumer that wants structure computes it from an engine it
runs. The reference shape for that is [phux-web](./web.md): Rust to
WASM, loading `ghostty-vt.wasm`, projecting the grid locally.

`phux-client` is the native-side library for the same pattern. Today it
leans on the server's engine-convenience snapshots (`GET_SCREEN` /
`GET_TERMINAL_STATE`) to read screen state rather than running a local
engine; those are a convenience over the shared engine, not a normative
structured wire tier. A consumer that wants to own its projection
follows phux-web's carry-your-own-engine shape instead. Either way the
wire stays identical; only the projection differs.

## Free-function surface

There is no `Agent` handle. The CLI and MCP adapter compose the crate's
module functions directly:

- `selector::{parse, resolve, resolve_with_tags, pick_target_pane}`
  parses the CLI target grammar and resolves it against a
  `SessionSnapshot`.
- `snapshot::{get_screen, get_screen_scrollback}` reads structured
  screen state without attaching or resizing the terminal.
- `send_keys::{send, send_to}` routes input to a focused or
  already-resolved terminal.
- `run::{run, run_in}` submits a command and returns a `RunOutcome`
  with its captured `RunResult` or timeout state.
- `wait::poll_until` polls screen state for a `Condition` and returns a
  `WaitResult`.
- `watch::{watch_events, collect_events}` consumes the pushed
  `AgentEvent` stream continuously or with finite bounds.
- `ask::report` reports an `AskedPayload` to the existing event stream.

The async operation functions open the connections they need and return
`attach::AttachError` for transport, protocol, or server refusal
failures. The selector helpers are synchronous and operate on
caller-provided snapshots. These are workspace-internal Rust APIs, not a
compatibility facade. Use the [CLI agent surface](./agents.md) or
[MCP adapter](./mcp.md) outside the workspace. Their `ScreenState`,
`RunResult`, and `WaitOutcome` JSON shapes are versioned.

## Where to read

- The structured shapes (`ScreenState`, `RunResult`, exit-code
  semantics): [`agents.md`](./agents.md).
- The thin JSON-RPC wrapper over the same `phux-client` functions:
  [`mcp.md`](./mcp.md).
- The L1 message catalog the library speaks:
  [`../spec/L1.md`](../spec/L1.md).
- The wire codec the library encodes against:
  [`../spec/appendix-encoding.md`](../spec/appendix-encoding.md).
- The projection thesis that places this crate among its peers:
  [ADR-0030](../adr/0030-engine-delegated-wire-and-projection-consumers.md)
  §4.
