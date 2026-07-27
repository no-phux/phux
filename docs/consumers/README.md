---
audience: consumers, contributors, agents
stability: evolving
last-reviewed: 2026-07-09
---

# Ways to use phux

**TL;DR.** Choose the interface that fits the job: the reference TUI for a
person, the CLI, OpenCode, Pi, or MCP adapter for an agent, the browser client
for the web, or the in-tree Rust client functions when working inside this
workspace. They are peer consumers of one wire and one terminal model.

---

## Choose an interface

| You want to | Start with |
|---|---|
| Work interactively with persistent sessions and splits | [The reference TUI](./tui.md) |
| Read and drive terminals from a script or coding agent | [Agents and the CLI](./agents.md) |
| Add terminal tools and lifecycle metadata to OpenCode | [The OpenCode integration](./opencode.md) |
| Connect Pi with target persistence and lifecycle metadata | [The Pi integration](./pi.md) |
| Connect a tool client over MCP | [The MCP adapter](./mcp.md) |
| Run the terminal client in a browser | [The web client](./web.md) |
| Study the in-tree Rust free-function API | [The internal client library](./sdk.md) |

## The peer principle

No consumer is protocol-privileged. The TUI, web client, and agent surface are
peers over one wire. Rendering clients project structured views from a local
engine; the CLI and MCP adapter also consume server-derived convenience
snapshots. In neither case is structured screen state the canonical wire tier.
See
[ADR-0017](../../ADR/0017-tui-not-protocol-privileged.md) (the TUI gets no
protocol-level standing) and
[ADR-0030](../../ADR/0030-engine-delegated-wire-and-projection-consumers.md)
(structured views are projections rather than a second synchronization model).

If a consumer needs behavior the wire does not provide, the answer is to
extend the spec with an ADR, not to add a consumer-shaped hook. The reference
pattern for a consumer that wants structure is to carry its own engine and
project locally, the way the web client does.

## Files

| File | Owns |
|---|---|
| [tui.md](./tui.md) | Reference TUI, the adoption wedge: CLI, keybinds, status bar, layout, hooks, recording. |
| [web.md](./web.md) | Reference projection consumer: Rust-to-WASM browser client that carries its own engine over the WebSocket wire codec. |
| [agents.md](./agents.md) | Agent surface: the CLI verb set, public agent state, asks, workspace save/restore, and versioned JSON contracts. (See [`../../AGENTS.md`](../../AGENTS.md) for universal agent substrate instructions.) |
| [opencode.md](./opencode.md) | OpenCode package: loading, six tools, target precedence, lifecycle metadata, shared adapter boundary, and safety. |
| [pi.md](./pi.md) | Pi package: local installation, six terminal tools, target persistence, lifecycle metadata, human handoff, and current safety boundaries. |
| [mcp.md](./mcp.md) | MCP adapter: a JSON-RPC stdio tool surface over the agent verbs, `phux_ask`, and plugin workspace profile discovery. |
| [sdk.md](./sdk.md) | The workspace-internal `phux-client` free-function surface over the `phux-protocol` wire codec. |
| [recording.md](./recording.md) | Session recording: the two capture surfaces, asciicast and the self-contained GIF/APNG renderer, tuning, and the limits. |

Future consumers — a native GUI, a tmux-CC adapter — get their
own files here when they materialize. Each file's frontmatter declares its
own `stability`; a shipped surface is `stable`, a forward-looking sketch is
`evolving`. Today the consumer surfaces are real but still pre-1.0, so most
files remain marked `evolving`.
