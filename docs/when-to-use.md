---
audience: humans, agents
stability: evolving
last-reviewed: 2026-07-09
---

# When to use phux (and when not to)

**TL;DR.** phux is a terminal multiplexer whose differentiator is the wire, not
the splits: a terminal is an object other programs can attach to, inspect, and
drive. The headline case is a human and their agents sharing the *same* live
terminal with CLI/MCP control, public agent state, and external integration
packages. If you want that, phux is for you today. If you want a
battle-hardened local multiplexer with a decade of muscle memory, you mostly
do not need it yet. Find your row below.

## Find yourself

| You are | phux? | Why |
|---|---|---|
| A human who wants their agent to *see and drive the same terminal they do* | **Yes — this is the point** | One server, many consumers; the agent attaches to your live pane, reads its grid, and types into it. |
| An agent author who wants structured, scriptable terminal control | **Yes** | `ls`/`snapshot`/`send-keys`/`run`/`wait`/`watch`/`ask`/`agent` with `--json`, plus `phux-mcp`. The CLI + JSON schema is the contract. |
| A team composing terminal-native coding agents | **Yes** | Public Codex/Claude integration fixtures, plugin workspace profiles, and MCP tools give you a phux-shaped agent bench without an in-process plugin host. |
| A tmux user who wants a modern, protocol-honest multiplexer | **Yes, with eyes open** | Attach/detach, splits, status bar, keybindings, visible help hints, and copy/navigation affordances work. Expect pre-1.0 edges. |
| Someone on one SSH session who just wants splits and persistence | **Probably not yet** | tmux already does this well and phux adds no wire advantage for a single local user. Revisit when you want remoting or agents. |
| A fleet operator who wants to drive terminals across machines | **Yes, with a hub-and-spoke limit** | A configured hub aggregates and routes satellite Terminals addressed as `host/@N`; it does not merge remote session/window models or chain satellite routes. |
| Someone who wants their agent's own event log held next to its terminal, readable by the same tools | **Soon, not yet** | Agent sessions as a second resource kind (`phux agent log`) are on a branch; until they land, the terminal, `phux agent show`, and `phux watch` are what there is. |

## The honest gaps

phux is pre-1.0. The wire, the reference TUI's attach/detach/multi-pane, and
modern-protocol passthrough are real and tested, but remain pre-1.0. The
headless verbs, `phux agent`, `phux-mcp`, plugin actions, workspace
save/restore, and hub-to-satellite Terminal routing are real and tested, but
JSON/API details may still wiggle before 1.0. Federated session/window joins,
a native GUI consumer, and a typed public SDK crate remain intentionally
absent. Predictive local echo is
implemented behind the opt-in `[experimental]` config and remains off by
default. If a capability is not in the [README status list](../README.md#status),
treat it as a promise, not a feature.

## Go deeper

- The mental model: [`docs/CONCEPTS.md`](./CONCEPTS.md)
- Driving phux from an agent: [`docs/consumers/agents.md`](./consumers/agents.md) · [`docs/consumers/mcp.md`](./consumers/mcp.md)
- Why it's built on a shared engine: [ADR-0030](../ADR/0030-engine-delegated-wire-and-projection-consumers.md)
- How it sits next to tmux: [ADR-0009](../ADR/0009-phux-vs-mux-positioning.md)
- Where it's going: [`docs/vision.md`](./vision.md)
