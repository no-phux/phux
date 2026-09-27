---
audience: consumers, contributors, agents
stability: evolving
last-reviewed: 2026-09-26
---

# Ways to use phux

**TL;DR.** Pick the interface that matches the job: the TUI or Cockpit for a
person, the CLI and MCP for a script or agent, OpenCode, Pi, or Claude when
those hosts already run the work, the browser client when the glass
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
| Give OpenCode a phux-owned terminal | [The OpenCode plugin](./opencode-v2.md) |
| Give Pi target persistence and fleet awareness | [The Pi integration](./pi.md) |
| Run Claude Code against the same terminals | [The Claude Code plugin](./claude.md) |
| Run the terminal client in a browser | [The web client](./web.md) |
| Use the native macOS app | [Cockpit](./cockpit.md) |
| Understand the accepted GPUIX Solid desktop contract | [Desktop](./desktop.md) (not yet release-verified) |
| Record a pane or an attached session | [Recording](./recording.md) |
| Understand the in-tree Rust client library | [`phux-client`](./sdk.md) |
| Use phux on [iOS](./ios.md) or [Android](./android.md) | Coming soon |

Every interface here is a peer of the others; the TUI has no protocol-level
standing ([ADR-0017](../adr/0017-tui-not-protocol-privileged.md)).

Gaps: [`../CONCEPTS.md`](../CONCEPTS.md#status).

Each file's frontmatter declares its own `stability`.
