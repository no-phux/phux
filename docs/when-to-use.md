---
audience: humans, agents
stability: evolving
last-reviewed: 2026-09-30
---

# When to use phux (and when not to)

**TL;DR.** Choose phux for programmable terminals, independent clients, and
direct remote access. Choose tmux for established multiplexing, Herdr for an
integrated agent workspace, or cmux for a native Mac terminal/browser workspace.
Compare the workflow you need; performance measurements answer narrower questions.

## The fast answer

phux keeps terminals running independently of their clients and exposes them
through public control and event interfaces. Use it to automate terminal work,
connect machines, or build clients with their own layouts and workflows.
For a conventional multiplexer or an integrated workspace, consider the
alternatives below.

## Choose by the job

| Your main job | Start with | Tradeoff to accept |
|---|---|---|
| Keep shells alive over SSH; split, detach, and reattach with familiar tooling | tmux | Automation is command/control-mode oriented; agent-workspace policy is yours to compose. |
| Run several coding agents with attention state and workspace navigation | Herdr | Adopt Herdr's workspace/tab/pane model and client experience. |
| Work in a native Mac terminal with a browser, sidebar context, and agent notifications | cmux | The native app is the main experience, not a headless mux server. |
| Program terminals through public interfaces, use independent clients, or connect machines directly | phux | More protocol/client machinery and less ecosystem history than tmux; evaluate the client you need. |

All four support automation. tmux has a
[control mode](https://github.com/tmux/tmux/wiki/Control-Mode), Herdr has a
[socket API](https://herdr.dev/docs/socket-api/), and cmux has a
[CLI and socket API](https://cmux.com/docs/api). phux differs in its resource
model and independent clients.

## Check the workflow, not just the feature list

Try the [quickstart](./QUICKSTART.md), then your intended client or
[agent integration](./consumers/getting-started.md). Start work, inspect it
from another client, detach, and reconnect. Check platform requirements and
server capabilities: repository docs may describe features absent from an
older release.

For remote fleets, phux's hub-and-spoke routing addresses satellite terminals
as `host/@N`. It does not merge remote session/window models or chain satellite
routes. See [remote access](./remote-access.md) before choosing a deployment.

## Compared with tmux

Both tools support start, split, detach, and reattach. phux's default prefix is
`Ctrl-A`; the [tmux translation](./coming-from.md#tmux) lists corresponding keys.

tmux is a mature multiplexer with a terminal client and control mode. phux
defines a resource protocol beneath its clients. Headless commands, agents,
Cockpit, and browsers use it without treating the TUI's screen as an API.
The server and rendering clients use the same terminal engine, exchanging
terminal bytes rather than translating them into a second screen model.

That design adds components. If you do not need independent clients,
structured agent lifecycle, or direct remote attachment, tmux offers a
better-established ecosystem.

## Compared with Herdr

Both systems keep real PTYs in a background server, survive client detach,
recognize agent work, and provide automation.

Herdr is an agent-workspace application. Its server owns the workspace model
and projects panes and agent state to Herdr clients; its control API addresses
that product model. phux stops its protocol vocabulary at resources,
lifecycle, streams, events, and metadata. Terminal and AgentSession are resource
kinds; sessions, windows, splits, and the attention inbox are client policy.

Choose Herdr for its cohesive agent workspace, supported-agent breadth, and
plugin ecosystem. Choose phux for independent clients, harness-emitted
lifecycle records rather than screen detection, or direct machine connections
without a product account. The [architecture comparison](./architecture/phux-and-herdr.md)
details these boundaries and links to both projects' documentation.

## Compared with cmux

cmux is a [native Swift/AppKit application powered by libghostty](https://github.com/manaflow-ai/cmux):
terminal workspaces, splits, vertical tabs, agent notifications, and a scriptable
in-app browser live in one Mac application. Its CLI and socket API support
automation, and its [SSH workflow](https://github.com/manaflow-ai/cmux#features)
supports remote terminals.

Choose cmux for that integrated terminal/browser/notification experience.
Choose phux to control terminals independently of a native app, use multiple
clients, or give a harness structured lifecycle and bounded terminal observation.
Evaluate each native interface directly; a phux TUI benchmark cannot tell you
which GUI feels better.

cmux's [session restore](https://github.com/manaflow-ai/cmux#session-restore)
restores layout, directories, and best-effort scrollback, not arbitrary process
memory. Its [opt-in local tmux profile](https://github.com/manaflow-ai/cmux/blob/main/docs/local-tmux.md)
keeps processes under tmux across app quit; this is not the default cmux
configuration. phux and tmux detach preserve work while their server and
machine remain alive. None promises live processes across reboot.

External capabilities were checked against official documentation on
September 30, 2026; older installed versions may differ.

## Performance

The [performance page](./performance.md) documents measurements,
repeat commands, sample-count rules, and historical limits. It compares real
phux/tmux/Herdr server-client boundaries and reports native cmux observations
separately. Server-only RSS is not whole-app memory; PTY-byte round trips are
not GUI input-to-pixel latency.

## Gaps

Read the [current capability and persistence limits](./CONCEPTS.md#status).

## Go deeper

- [Translate tmux or screen keybindings](./coming-from.md).
- [Understand terminals, sessions, and the resource model](./CONCEPTS.md).
- [Run a coding agent](./consumers/getting-started.md), or use the
  [automation reference](./consumers/agents.md) and [MCP adapter](./consumers/mcp.md).
- Why it's built on a shared engine: [ADR-0030](adr/0030-engine-delegated-wire-and-projection-consumers.md)
- How it sits next to tmux: [ADR-0009](adr/0009-phux-vs-mux-positioning.md)
- [Read the project direction](./vision.md).
