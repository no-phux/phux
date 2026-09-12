---
audience: contributors, agents
stability: stable
last-reviewed: 2026-09-12
---

# Architecture reference

**TL;DR.** Internal structure of phux — the glance diagram, process model,
threading, transport, crate graph, data model, state sync, rendering, and
the quality bar. Not normative (the wire spec is); not user-facing (the
consumer docs are). What you read to understand how phux is built.

---

## Files

| File | Owns |
|---|---|
| [DIAGRAM.md](./DIAGRAM.md) | Glance sketch: PTY and producer in, resource engines, the frame seam, client replicas and chrome |
| [process-model.md](./process-model.md) | Per-user server, single process, current-thread runtime; supervision (ADR-0003, ADR-0014) |
| [threading.md](./threading.md) | `!Send`/`!Sync` constraints, one LocalSet task per resource engine, the std mutex discipline |
| [transport.md](./transport.md) | The frame seam and the five byte streams: UDS, WebSocket, QUIC, WebTransport, SSH-stdio; `phux-dial` (ADR-0007) |
| [crate-graph.md](./crate-graph.md) | Crate dependency edges, the protocol-core independence (ADR-0011), and how the crates map onto L1/L3 |
| [data-model.md](./data-model.md) | Sessions, windows, resources (kinds, facets, parent bindings), layouts as in-process types — distinct from wire shape |
| [state-sync.md](./state-sync.md) | What happens on attach: the three Terminal bootstrap profiles, native checkpoint versus synthesized VT, StateSync (ADR-0018, ADR-0070) |
| [render-layering.md](./render-layering.md) | ratatui chrome over libghostty pane interiors (ADR-0020) |
| [predictive-echo.md](./predictive-echo.md) | Client-side prediction loop and reconciliation |
| [verification.md](./verification.md) | The test and performance quality bar: unit, integration, golden snapshots, hot-path discipline, allocation budget |
| [module-structure.md](./module-structure.md) | Per-crate module layout as it exists in tree today |

The former `l2-server-design.md` lives in
[`research/archive/`](../../research/archive/2026-06-06-l2-server-design.md).
There is no L2 collection tier; see [`../spec/L2.md`](../spec/L2.md).

## What's not here

- Wire bytes — that's [`../spec/`](../spec/).
- TUI surfaces — that's [`../consumers/tui.md`](../consumers/tui.md).
- Decisions — that's [`../../ADR/`](../../ADR/). Architecture docs
  describe what the code is; ADRs explain why it's that shape.
- What phux is — that's [`../CONCEPTS.md`](../CONCEPTS.md).

## When this directory is wrong

Code is the implementation; these documents describe it. Where the code
and the target shape differ, each document says so in its single `Status`
table, pointing at the owning ADR and the tracked bead
([`../CONVENTIONS.md`](../CONVENTIONS.md)). Product-wide gaps live in
[`../CONCEPTS.md`](../CONCEPTS.md). If a document and the code disagree
without such a row, file an issue: either the code drifted or the doc did,
and the response is to reconcile, not to let either rot.
