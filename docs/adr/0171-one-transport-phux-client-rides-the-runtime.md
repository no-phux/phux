---
audience: contributors
stability: stable
last-reviewed: 2026-10-15
---

# 0171 — One transport: phux-client rides the client runtime

**TL;DR.** `phux-client-runtime` is the only code that dials, negotiates
HELLO, frames, keeps alive, reconnects, and multiplexes QUIC terminal
streams. `phux-client`'s `Connection` becomes a facade over a runtime
session: the request/reply and streaming API its callers use, with no
transport of its own. Organization edits have one sequencer, which
request/reply callers drive through an adapter. The TUI keeps its attach
loop and replicas for now, but its bytes ride the runtime transport.

Status: Accepted
Date: 2026-10-15

## Context

[ADR-0133](./0133-one-client-runtime-below-every-binding.md) made
`phux-client-runtime` the one home for client orchestration, and the
desktop and FFI ride it. The CLI, MCP, and TUI do not: `phux-client` keeps
a second transport (`attach/connection.rs`: dial wrappers, HELLO, SPEC §5
framing, correlated requests, QUIC terminal-stream binding, corking).
`phux organize` (ADR-0174) paid for that split three ways: apply and undo
written twice (async over `Connection`, and a sans-IO sequencer over the
runtime lane), repository discovery moved down a layer so the desktop
could reach it, and a new dependency edge.

## Decision

1. **The runtime owns the transport.** Dial, HELLO and capability
   negotiation, framing, keepalive, the reconnect ladder, fatal-refusal
   classification, and QUIC per-terminal streams (bind, unbind, corked
   batches) live in `phux-client-runtime` and nowhere else.
2. **One-shot sessions.** The runtime offers a session that attaches to
   nothing and never redials (a verb's lifetime), with an awaitable
   correlated request for every request frame (`COMMAND`, `GET_STATE`,
   metadata reads, spawn, move) that returns the reply plus the notices
   interleaved before it, and raw framed send and receive for streaming
   consumers.
3. **`phux-client` is a facade.** `phux_client::attach::connection::Connection`
   keeps its public API so its callers do not change, implemented over a
   runtime session. Every `phux-client` helper (`org`, `session`, `state`,
   `kill`, `spawn`, ...) stays a request/reply convenience above it. No
   socket, framing, or handshake code remains in `phux-client`.
4. **One organization sequencer.** `phux_client_runtime::organize::Sequencer`
   is the only apply/undo implementation. `phux_client::organize` drives
   it with an adapter that sends each submission as a request and feeds
   the reply back as the outcome.
5. **The TUI moves in two rungs.** First its transport (this ADR, through
   the facade). Its attach loop and libghostty replicas move onto the
   runtime engine owner thread in a later rung, under ADR-0133.

## Why

Two transports drift: every capability lands twice or lands in one and is
missing in the other (the desktop had no organize until a second
implementation was written). A facade moves every request/reply caller at
once without rewriting them, and makes the remaining difference between the
TUI and the desktop the engine, not the wire.

## Tradeoffs

- The facade carries a compatibility surface (`send`/`recv`) that a later
  rung shrinks as the TUI moves onto the engine.
- A one-shot verb now runs the runtime's session machinery; its cost is
  pinned by the existing perf gates.

## Alternatives

**Rewrite every caller onto `runtime::Client`.** The same end state, but
38 files of churn in one step and no shippable midpoint.

**Leave both transports.** Every client capability keeps landing twice.
