---
audience: humans, agents
stability: evolving
last-reviewed: 2026-09-16
---

# When to use phux (and when not to)

**TL;DR.** Use phux when a person, an agent, and more than one kind of client
must share the same live terminal. Use tmux for a mature local multiplexer with
minimal machinery. Use Herdr for an integrated agent-workspace product. phux's
distinct bet is a public resource wire with peer consumers.

## The fast answer

phux is not the automatic upgrade from tmux, and it is not a reimplementation
of Herdr. Its extra protocol and client-side terminal replicas are worthwhile
when the terminal itself must be addressable: a TUI can display it, an agent can
read and drive it, Cockpit or a browser can render it, and all of them remain on
the same object.

If that sentence does not describe your work, choose the simpler or more
integrated tool that does.

## Find yourself

| You are | phux? | Why |
|---|---|---|
| A human who wants their agent to *see and drive the same terminal they do* | **Yes — this is the point** | One server, many consumers; the agent attaches to your live pane, reads its grid, and types into it. |
| A human who wants a native macOS client on those same terminals | **Yes** | Cockpit ships, independently versioned. Install is in [`INSTALL.md`](./INSTALL.md#cockpit-native-macos). |
| An agent author who wants structured, scriptable terminal control | **Yes** | `ls`/`snapshot`/`send-keys`/`run`/`wait`/`watch`/`ask`/`agent` with `--json`, plus `phux-mcp`. The CLI + JSON schema is the contract. |
| A team composing terminal-native coding agents | **Yes** | Public Codex/Claude integration fixtures, plugin workspace profiles, and MCP tools give you a phux-shaped agent bench without an in-process plugin host. |
| A tmux user who wants other programs to be peer clients | **Yes** | Attach/detach, splits, status bar, keys, and copy/navigation are the on-ramp; the resource wire is the reason to switch. |
| Someone on one SSH session who just wants splits and persistence | **No clear benefit** | tmux already does this well and phux adds no wire advantage for a single local user. |
| A fleet operator who wants to drive terminals across machines | **Yes, with a hub-and-spoke limit** | A configured hub aggregates and routes satellite Terminals addressed as `host/@N`; it does not merge remote session/window models or chain satellite routes. |
| Someone who wants their agent's own event log held next to its terminal, readable by the same tools | **Yes, in this tree** | `phux agent session open` / `close`, `phux agent emit`, `phux agent log`. `%name` resolves an AgentSession. Older brew/curl releases may not advertise it; `phux status --json` is the check. |

## Compared with tmux

The common path is deliberately unsurprising: start, split, detach, reattach.
The default prefix is `Ctrl-A`, and the [tmux translation](./coming-from.md#tmux)
lists the corresponding keys.

The difference is not a longer feature checklist. tmux is a mature multiplexer
whose server and client implement a terminal application. phux defines a
resource protocol beneath its TUI. A headless command, an agent, Cockpit, and a
browser can therefore attach to the same terminal without treating the TUI's
screen as an API. The server and rendering clients use the same terminal engine,
so phux carries terminal bytes across that boundary rather than translating
every terminal protocol into a second screen model.

That design costs more components and far less ecosystem history. If you do not
need peer clients, structured agent lifecycle, or direct remote attachment,
tmux is the better-established answer.

## Compared with Herdr

Herdr and phux both keep real PTYs in a background server, survive client
detach, recognize agent work, and provide automation. The durable boundary is
different.

Herdr is an agent-workspace application. Its server owns the workspace model
and projects panes and agent state to Herdr clients; its control API addresses
that product model. phux stops its protocol vocabulary at resources,
lifecycle, streams, events, and metadata. Terminal and AgentSession are resource
kinds; sessions, windows, splits, and the attention inbox are client policy.

Choose Herdr when its cohesive agent workspace, supported-agent breadth, and
plugin ecosystem are the product you want. Choose phux when the terminal and
agent event stream must be a public substrate for independently shaped clients,
when a harness should emit lifecycle instead of making screen detection the
source of truth, or when machines should connect directly without a product
account. The deeper [system-shape comparison](./architecture/phux-and-herdr.md)
names the exact boundaries and links to both projects' primary documentation.

## Performance

The [performance page](./performance.md) publishes dated results from the
repository's reproducible local benchmark across phux transports and Herdr. It
includes versions, method, caveats, and the reproduction command. Treat those
numbers as one controlled machine comparison, not a universal ranking.

## Gaps

[Status table in CONCEPTS](./CONCEPTS.md#status).

## Go deeper

- Translate tmux or screen keys: [`coming-from.md`](./coming-from.md)
- Compare phux and Herdr's system boundaries: [`architecture/phux-and-herdr.md`](./architecture/phux-and-herdr.md)
- Inspect measured performance: [`performance.md`](./performance.md)
- The mental model: [`docs/CONCEPTS.md`](./CONCEPTS.md)
- Driving phux from an agent: [`docs/consumers/agents.md`](./consumers/agents.md) · [`docs/consumers/mcp.md`](./consumers/mcp.md)
- Why it's built on a shared engine: [ADR-0030](adr/0030-engine-delegated-wire-and-projection-consumers.md)
- How it sits next to tmux: [ADR-0009](adr/0009-phux-vs-mux-positioning.md)
- Where it's going: [`docs/vision.md`](./vision.md)
