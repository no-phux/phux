---
audience: humans, agents
stability: evolving
last-reviewed: 2026-09-12
---

# When to use phux (and when not to)

**TL;DR.** Decide whether phux fits you today. It is a multiplexer whose
terminals are objects other programs attach to, inspect, and drive. The
headline case is a human and their agents sharing the same live terminal. A
battle-hardened local multiplexer with a decade of muscle memory is still
tmux. Find your row below.

## Find yourself

| You are | phux? | Why |
|---|---|---|
| A human who wants their agent to *see and drive the same terminal they do* | **Yes — this is the point** | One server, many consumers; the agent attaches to your live pane, reads its grid, and types into it. |
| A human who wants a native macOS client on those same terminals | **Yes** | Cockpit ships, independently versioned. Install is in [`INSTALL.md`](./INSTALL.md#cockpit-native-macos). |
| An agent author who wants structured, scriptable terminal control | **Yes** | `ls`/`snapshot`/`send-keys`/`run`/`wait`/`watch`/`ask`/`agent` with `--json`, plus `phux-mcp`. The CLI + JSON schema is the contract. |
| A team composing terminal-native coding agents | **Yes** | Public Codex/Claude integration fixtures, plugin workspace profiles, and MCP tools give you a phux-shaped agent bench without an in-process plugin host. |
| A tmux user who wants a modern, protocol-honest multiplexer | **Yes** | Attach/detach, splits, status bar, keybindings, visible help hints, and copy/navigation affordances work. |
| Someone on one SSH session who just wants splits and persistence | **Probably not yet** | tmux already does this well and phux adds no wire advantage for a single local user. Revisit when you want remoting or agents. |
| A fleet operator who wants to drive terminals across machines | **Yes, with a hub-and-spoke limit** | A configured hub aggregates and routes satellite Terminals addressed as `host/@N`; it does not merge remote session/window models or chain satellite routes. |
| Someone who wants their agent's own event log held next to its terminal, readable by the same tools | **Yes, in this tree** | `phux agent session open` / `close`, `phux agent emit`, `phux agent log`. `%name` resolves an AgentSession. Older brew/curl releases may not advertise it; `phux status --json` is the check. |

## Gaps

[Status table in CONCEPTS](./CONCEPTS.md#status).

## Go deeper

- Coming from tmux, screen, or the old phux distro: [`docs/coming-from.md`](./coming-from.md)
- The mental model: [`docs/CONCEPTS.md`](./CONCEPTS.md)
- Driving phux from an agent: [`docs/consumers/agents.md`](./consumers/agents.md) · [`docs/consumers/mcp.md`](./consumers/mcp.md)
- Why it's built on a shared engine: [ADR-0030](adr/0030-engine-delegated-wire-and-projection-consumers.md)
- How it sits next to tmux: [ADR-0009](adr/0009-phux-vs-mux-positioning.md)
- Where it's going: [`docs/vision.md`](./vision.md)
