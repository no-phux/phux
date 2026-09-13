---
audience: consumers, contributors, agents
stability: evolving
last-reviewed: 2026-09-13
---

# Ways to use phux

**TL;DR.** Pick the interface that matches the job: the TUI or Cockpit for a
person, the CLI and MCP for a script or agent, OpenCode, Pi, or Claude when
those hosts already run the work, the browser or iOS client when the glass
is not a tty, and recording when you want a cast. They are peers of one
server and one terminal model.

---

## Choose an interface

| You want to | Start with |
|---|---|
| Work interactively in a terminal | [The reference TUI](./tui.md) |
| Read and drive terminals from a script or coding agent | [Agents and the CLI](./agents.md) |
| Emit working/blocked/idle from a harness | [Harness authors](./harness.md) |
| Speak the wire from a new client | [Build a client](./build-a-client.md) |
| Connect a tool client over MCP | [The MCP adapter](./mcp.md) |
| Give OpenCode terminal tools and fleet awareness | [The OpenCode integration](./opencode.md) |
| Give Pi target persistence and fleet awareness | [The Pi integration](./pi.md) |
| Run Claude Code against the same terminals | [The Claude Code plugin](./claude.md) |
| Run the terminal client in a browser | [The web client](./web.md) |
| Use the native macOS app | [Cockpit](./cockpit.md) |
| Record a pane or an attached session | [Recording](./recording.md) |
| Pair a phone over `wss://` (contract) | [The iOS client](./ios.md) |

Every interface here is a peer of the others; the TUI has no protocol-level
standing ([ADR-0017](../adr/0017-tui-not-protocol-privileged.md)).

Gaps: [`../CONCEPTS.md`](../CONCEPTS.md#status).

## Files

| File | Owns |
|---|---|
| [tui.md](./tui.md) | Reference TUI: prefix keys, layout, chrome, copy-mode, fleet overlay. |
| [cockpit.md](./cockpit.md) | Native macOS client over `phux-client-ffi`. |
| [web.md](./web.md) | Browser client that carries its own engine over the WebSocket wire codec. |
| [ios.md](./ios.md) | Swift/UniFFI iOS client and the `phux pair` connect-link contract. |
| [agents.md](./agents.md) | Agent surface: CLI verbs, JSON contracts, asks, workspace save/restore. |
| [harness.md](./harness.md) | Producer contract: open an AgentSession, emit, never write detector state. |
| [build-a-client.md](./build-a-client.md) | Third-party client paths: CLI, MCP, phux-protocol, FFI. |
| [opencode.md](./opencode.md) | OpenCode package: tools, fleet context, target precedence, safety. |
| [pi.md](./pi.md) | Pi package: tools, fleet context, target persistence, safety. |
| [claude.md](./claude.md) | Claude Code plugin: MCP tools plus lifecycle identity. |
| [mcp.md](./mcp.md) | MCP adapter over the agent verbs. |
| [sdk.md](./sdk.md) | Workspace-internal `phux-client` free-function surface. |
| [recording.md](./recording.md) | Session recording: observer capture, interactive tee, playback as a pane. |

Each file's frontmatter declares its own `stability`. A shipped surface is
`stable`; a surface still settling is `evolving`.
