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
| [phux-and-herdr.md](./phux-and-herdr.md) | Stable system-shape comparison: application projections versus a peer-consumer resource wire |
| [process-model.md](./process-model.md) | Per-user server, single process, current-thread runtime; supervision (ADR-0003, ADR-0014) |
| [threading.md](./threading.md) | `!Send`/`!Sync` constraints, one LocalSet task per resource engine, the std mutex discipline |
| [client-runtime.md](./client-runtime.md) | The one layer below every client binding (ADR-0133): sans-IO control plane, engine owner thread, published grid frames, reconnecting driver |
| [desktop.md](./desktop.md) | Accepted Solid/native desktop seams, runtime views, identity, geometry, and tooling boundaries (ADR-0139); implementation gaps explicit |
| [desktop-verification.md](./desktop-verification.md) | Desktop dependency order and native, independent-view, fidelity, performance, and package acceptance evidence |
| [transport.md](./transport.md) | The frame seam and the five byte streams: UDS, WebSocket, QUIC, WebTransport, SSH-stdio; `phux-dial` (ADR-0007) |
| [crate-graph.md](./crate-graph.md) | Crate dependency edges, the protocol-core independence (ADR-0011), and how the crates map onto L1/L3 |
| [data-model.md](./data-model.md) | Sessions, windows, resources (kinds, facets, parent bindings), layouts as in-process types — distinct from wire shape |
| [state-sync.md](./state-sync.md) | What happens on attach: the three Terminal bootstrap profiles, native checkpoint versus synthesized VT, StateSync (ADR-0018, ADR-0070) |
| [render-layering.md](./render-layering.md) | ratatui chrome over libghostty pane interiors (ADR-0020) |
| [predictive-echo.md](./predictive-echo.md) | Client-side prediction loop and reconciliation |
| [verification.md](./verification.md) | The test and performance quality bar: unit, integration, golden snapshots, hot-path discipline, allocation budget |
| [module-structure.md](./module-structure.md) | Per-crate module layout as it exists in tree today |

There is no L2 collection tier; see [`../spec/L2.md`](../spec/L2.md).

## What's not here

- Wire bytes — that's [`../spec/`](../spec/).
- TUI surfaces — that's [`../consumers/tui.md`](../consumers/tui.md).
- Decisions — that's [`../adr/`](../adr/). Architecture docs
  describe what the code is; ADRs explain why it's that shape.
- What phux is — that's [`../CONCEPTS.md`](../CONCEPTS.md).

Where code and target shape differ, each document says so in its single
`Status` table ([`../CONVENTIONS.md`](../CONVENTIONS.md)); product-wide gaps
live in [`../CONCEPTS.md`](../CONCEPTS.md).
