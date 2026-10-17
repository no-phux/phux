---
audience: contributors, agents
stability: stable
last-reviewed: 2026-09-12
---

# Architecture reference

**TL;DR.** Internal structure of phux: resource ownership, client replicas,
transport, rendering, and verification. These pages describe the implementation
and identify gaps against accepted designs. The wire specification is
normative; consumer docs describe user-facing behavior.

---

## Files

| File | Owns |
|---|---|
| [DIAGRAM.md](./DIAGRAM.md) | Glance sketch: PTY and producer in, resource engines, the frame seam, client replicas and chrome |
| [resource-atlas.html](./resource-atlas.html) | Standalone interactive concept: generalized substrate, resource-kind slots, and federation; proposed extensions are marked, not shipped capabilities |
| [phux-and-herdr.md](./phux-and-herdr.md) | Stable system-shape comparison: application projections versus a peer-consumer resource wire |
| [process-model.md](./process-model.md) | Per-user server, single process, current-thread runtime; supervision (ADR-0003, ADR-0014) |
| [resource-providers.md](./resource-providers.md) | Non-terminal resources: provider declarations and activations, the `FEED` kind, health, grants, and consumers (ADR-0177, Proposed). Designed, not built |
| [usage-collector.md](./usage-collector.md) | Optional usage collector: supervision, Claude Code source, delivery, budgets (ADR-0166). Designed, not built |
| [threading.md](./threading.md) | `!Send`/`!Sync` constraints, one LocalSet task per resource engine, the std mutex discipline |
| [client-runtime.md](./client-runtime.md) | The one layer below every client binding (ADR-0133): sans-IO control plane, engine owner thread, published grid frames, reconnecting driver |
| [desktop.md](./desktop.md) | GPUI desktop host, runtime views, identity, geometry, and package boundaries; GPU qualification is separate |
| [desktop-verification.md](./desktop-verification.md) | Desktop dependency order and native, independent-view, fidelity, performance, and package acceptance evidence |
| [transport.md](./transport.md) | The frame seam and the five byte streams: UDS, WebSocket, QUIC, WebTransport, SSH-stdio; `phux-dial` (ADR-0007) |
| [crate-graph.md](./crate-graph.md) | Crate dependency edges, the protocol-core independence (ADR-0011), and how the crates map onto L1/L3 |
| [data-model.md](./data-model.md) | Keys and handles, resources (kinds, facets, activations, parent bindings), sessions, arrangements, and `apply` as in-process types — distinct from wire shape |
| [state-sync.md](./state-sync.md) | What happens on attach: the three Terminal bootstrap profiles, native checkpoint versus synthesized VT, StateSync (ADR-0018, ADR-0070) |
| [render-layering.md](./render-layering.md) | ratatui chrome over libghostty pane interiors (ADR-0020) |
| [predictive-echo.md](./predictive-echo.md) | Client-side prediction loop and reconciliation |
| [verification.md](./verification.md) | The test and performance quality bar: unit, integration, golden snapshots, hot-path discipline, allocation budget |
| [module-structure.md](./module-structure.md) | Per-crate module layout as it exists in tree today |

The organization model is [`../spec/L2.md`](../spec/L2.md); its commands
ride the L1 command envelope.

## What's not here

- Wire bytes — [`../spec/`](../spec/).
- TUI surfaces — [`../consumers/tui.md`](../consumers/tui.md).
- Decisions and rationale — [`../adr/`](../adr/).
- What phux is — [`../CONCEPTS.md`](../CONCEPTS.md).

Where code and target shape differ, each document says so in its single
`Status` table ([`../CONVENTIONS.md`](../CONVENTIONS.md)); product-wide gaps
live in [`../CONCEPTS.md`](../CONCEPTS.md).
