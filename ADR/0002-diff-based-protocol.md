---
audience: contributors
stability: stable
last-reviewed: 2026-05-28
---

# 0002 — Diff-based wire protocol, not VT byte replay

**TL;DR.** Historical decision to ship structured cell-level diffs from server to clients instead of VT byte replay. The cost model (parsing as the dominant cost; one parse vs three) turned out to be wrong at modern libghostty speeds, and protocol-design cost was perpetual. Superseded in full by ADR-0013; preserved as historical context.

Status: Superseded by ADR-0013
Date: 2026-05-24
Superseded by [ADR-0013](./0013-libghostty-bytes-on-wire.md): the whole decision; VT bytes on the wire replace structured cell diffs.

## Context

A multiplexer must transport pane state from a server to one or more
clients. Two approaches exist:

- **VT byte replay** (tmux): the server emits raw VT bytes for the
  client to re-parse.
- **Structured diff** (Mosh, RDP, this proposal): the server holds the
  canonical grid; the wire carries cell-level diffs; the client
  renders directly.

## Decision

phux uses a structured cell-level diff protocol. See `SPEC.md` §8.

## Rationale

- **Avoids round-tripping through a parse state machine.** tmux: parse
  VT → grid → synthesize VT → parse VT again at the client. phux:
  parse VT → grid → diff → render. One parse instead of three.
- **Frontend-agnostic.** A native GUI client (libghostty surface)
  renders diffs straight to a GPU surface. A TUI client converts to VT
  at the edge. Same protocol; both clients are possible.
- **Server is authoritative.** No client-side parser inconsistencies.
  SGR ambiguity, true-color vs 256-color downsampling, hyperlink
  interpretation — all resolved server-side before the cell hits the
  wire.
- **Frame pacing is natural.** Diffs can be coalesced, dropped, or
  replaced by a snapshot under backpressure. A byte stream cannot be
  paced; a structured frame stream can.
- **Faithful key events.** Once we accept that the protocol carries
  *semantics*, the natural extension is to do the same for input.
  `KeyEvent` structures preserve modifier-rich events through the
  multiplexer in a way raw bytes cannot. This is independently
  important; the diff decision opened the door.

## Tradeoffs

- **TUI client carries a small composer.** Cell diffs → VT for the
  outer terminal is real work. But it is well-understood work (see
  tmux's `tty.c`), and it is done once at the edge instead of once per
  emit at the server.
- **Protocol design surface is larger.** A byte stream has no design;
  a diff protocol has a wire format that must be specified, tested,
  and versioned. We accept the cost because we get architectural
  payoffs that compound over the system's lifetime.

## Prior art

- Mosh's State Synchronization Protocol does this for SSH.
- RDP and SPICE do this for full GUI sessions.
- libghostty-vt's `RenderState` is designed to enable diff consumers.

## Alternatives considered

- **Raw VT byte forwarding (tmux's model).** Simplest possible
  protocol. Loses everything described above. Structurally caps the
  ceiling of what a multiplexer on top can become.
- **Hybrid: VT bytes for legacy clients, diffs for new ones.** Doubles
  protocol surface, hides decisions, encourages "just hack it in as
  VT". Rejected.
