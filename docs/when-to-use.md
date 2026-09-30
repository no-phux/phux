---
audience: humans, agents
stability: evolving
last-reviewed: 2026-09-30
---

# When to use phux (and when not to)

**TL;DR.** Use phux when people, agents, and different clients need the same
addressable terminal. Use tmux for established terminal multiplexing, Herdr for
an integrated terminal-based agent workspace, or cmux for a native macOS
terminal/browser workspace. Choose the boundary and workflow you need, not a
benchmark winner or a claim that one product replaces all the others.

## The fast answer

phux is not the automatic upgrade from tmux, and it is not a reimplementation
of Herdr. Its extra protocol and client-side terminal replicas are worthwhile
when the terminal itself must be addressable: a TUI can display it, an agent can
read and drive it, Cockpit or a browser can render it, and all of them remain on
the same object.

If that sentence does not describe your work, choose the simpler or more
integrated tool that does.

## Choose by the job

| Your main job | Start with | Tradeoff to accept |
|---|---|---|
| Keep shells alive over SSH; split, detach, and reattach with familiar tooling | **tmux** | Automation is command/control-mode oriented; agent-workspace policy is yours to compose. |
| Run several coding agents in a cohesive terminal workspace with attention state and workspace navigation | **Herdr** | Adopt Herdr's workspace/tab/pane model and its client experience. |
| Work in a native Mac terminal with a browser beside it, sidebar context, and agent notifications | **cmux** | The native app is the main experience; its GUI pipeline is not interchangeable with a headless mux server. |
| Share live terminal resources between an agent, terminal UI, browser/native clients, or machines | **phux** | More protocol/client machinery and less ecosystem history than tmux; evaluate the particular client you need. |

These are starting points, not exclusive capability boxes. tmux has a
[control mode](https://github.com/tmux/tmux/wiki/Control-Mode), Herdr has a
[socket API](https://herdr.dev/docs/socket-api/), and cmux exposes a
[CLI and socket API](https://cmux.com/docs/api). phux's distinction is the
resource boundary and independent consumers, not the claim that other products
cannot be automated.

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
| A Mac user primarily looking for a polished terminal plus an in-app browser | **Compare cmux first** | cmux directly packages that workflow; phux's shared-server model earns its complexity only if you need it. |

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

## Compared with cmux

cmux is a [native Swift/AppKit application powered by libghostty](https://github.com/manaflow-ai/cmux):
terminal workspaces, splits, vertical tabs, agent notifications, and a scriptable
in-app browser live in one Mac application. It can be driven through its CLI and
socket API; it is not merely a pretty terminal with no automation surface.
Its [SSH workflow](https://github.com/manaflow-ai/cmux#features) also means “native Mac
app” should not be read as “local terminals only.”

Choose cmux when that terminal/browser/notification experience is the product
you want. Choose phux when a long-lived terminal resource must be shared by
differently shaped clients, when your harness needs structured lifecycle and
bounded terminal observation, or when the terminal control plane should not
depend on the native application being open. phux's native clients and cmux's
native app need their own usability and rendering evaluation; a fast phux TUI
probe cannot answer which GUI feels better.

Be precise about persistence. cmux's
[session restore](https://github.com/manaflow-ai/cmux#session-restore) restores
app-owned layout, directories, and best-effort scrollback; it does not checkpoint
arbitrary process memory. Its documented
[opt-in local tmux profile](https://github.com/manaflow-ai/cmux/blob/main/docs/local-tmux.md)
keeps processes under a separate tmux owner across app quit. That is useful
persistence, not the same configuration as a default cmux terminal. Likewise,
phux and tmux detach preserve work only while their owning server and machine
remain alive; none of these claims promises live processes across reboot.

External capability descriptions were checked against official documentation
on September 30, 2026. They describe the documented product, not a guarantee that
every older installed version includes the same commands.

## Performance

The [performance page](./performance.md) separates product choice from
measurement: real phux/tmux/Herdr server-client boundaries, native cmux
requirements, repeat commands, sample-count rules, and the limitations of the
historical results. A server-only RSS row is not a complete-app memory ranking;
a PTY-byte round trip is not native GUI input-to-pixel latency.

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
