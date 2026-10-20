# phux vs tmux, Herdr, and cmux

All four keep terminal work running while you look away, and all four can
be scripted. They differ in what owns the terminal and what you build on.
Pick by the job, not by a feature count.

External facts below were checked against each project's own documentation
on 2026-10-19: tmux 3.8, Herdr 0.9.5, cmux 0.65.2. Newer releases may differ.

## Choose by the job

| Your main job | Start with | Tradeoff to accept |
| --- | --- | --- |
| Keep shells alive over SSH; split, detach and reattach with tooling everyone knows | tmux | Automation goes through commands and control mode; agent awareness is yours to add. |
| Run several coding agents with attention state, workspaces and saved SSH machines in one window | Herdr | You adopt Herdr's workspace, tab and pane model and its terminal client. |
| Work in a native Mac terminal with an in-app browser, a sidebar and agent notifications | cmux | The Mac app is the experience; processes live in it unless you opt in to its tmux or zellij owner. |
| Program terminals through a public protocol, attach several kinds of client to the same panes, or reach other machines over QUIC | phux | More moving parts and far less ecosystem history than tmux. |

Each has an automation surface: tmux
[control mode](https://github.com/tmux/tmux/wiki/Control-Mode), the Herdr
[socket API](https://herdr.dev/docs/socket-api/), and the cmux
[CLI and socket API](https://cmux.com/docs/api). phux's is the same
[protocol](./protocol.md) its own clients use, plus the
[scripting commands](./scripting.md) and an MCP server.

## Compared with tmux

Both start, split, detach and reattach, and the keys mostly match:
phux uses `Ctrl-A` where tmux uses `Ctrl-B`. The
[translation table](./getting-started.md#coming-from-tmux) lists the
differences.

tmux is a mature multiplexer with one terminal client and control mode.
phux puts a resource protocol under its clients: the terminal UI, the
desktop app, the browser and agents all read the same server state instead
of scraping the TUI's screen. Server and clients share one terminal
emulator, libghostty, and exchange terminal bytes rather than translating
them into a second screen model. See [clients](./clients.md).

That design costs components. If you do not need several clients, agent
state, or direct remote attach, tmux is the better-established choice.

## Compared with Herdr

Both keep real PTYs in a background server, survive client detach,
show which agent needs you, and can be driven from scripts. Both reach other
machines over SSH, where each machine runs its own server.

Herdr is an agent-workspace application. Its server owns workspaces, tabs and
panes, and its API addresses that model. It reads most agents' state from
what they draw on screen; some agents report state to it directly
([how status works](https://herdr.dev/docs/agents/)).

phux keeps its protocol at resources, lifecycle, streams and events; splits,
tabs and the attention inbox are each client's presentation. Agent state
comes from the strongest source available: a declaration, the agent host's
own lifecycle stream, a hook, then the screen as the fallback
([what phux knows about an agent](./agents.md#what-phux-knows-about-an-agent)).
Remote attach starts over SSH, then runs directly over pinned QUIC
([remote machines](./remote.md)).

Choose Herdr for a cohesive agent workspace, its supported-agent breadth and
its plugins. Choose phux for independent clients, harness-reported agent
state, or a protocol to build your own client on.

## Compared with cmux

cmux is a native Swift/AppKit Mac app built on libghostty: workspaces,
splits, vertical tabs, a scriptable in-app browser and agent notifications in
one window ([features](https://github.com/manaflow-ai/cmux)). `cmux ssh`
opens a workspace on a remote machine.

Choose cmux for that integrated terminal, browser and notification
experience. Choose phux to control terminals independently of any one app,
attach several clients to the same panes, or give a harness structured agent
state and bounded terminal reads.

## What survives what

| Event | tmux | Herdr | cmux | phux |
| --- | --- | --- | --- | --- |
| Client detaches or closes | processes keep running | processes keep running | app quit ends them, unless the opt-in [tmux owner](https://github.com/manaflow-ai/cmux/blob/main/docs/local-tmux.md) holds them | processes keep running |
| Server or app restarts | everything is gone | layout returns as new shells; [supported agents can resume](https://herdr.dev/docs/session-state/) | layout, directories and best-effort scrollback return as new shells; [supported agents can resume](https://github.com/manaflow-ai/cmux#session-restore) | session, tab and pane records return; processes and scrollback do not ([details](./concepts.md#what-survives-what)) |

None of the four keeps live processes across a reboot.

## Measurements

On 2026-09-30, two runs compared phux 0.46, tmux 3.7c and Herdr 0.9.0
terminal clients on one heavily loaded Apple M4 Pro. Rankings reversed
between runs: phux's key-echo median was the slowest of the three in one run
and the fastest in the other. The one consistent result was that tmux used
the least server memory in both. These are old versions on a noisy host, not
a speed ranking; the raw samples and commands are published for
[run 1](https://phux.sh/benchmarks/2026-09-30/run1/samples.json) and
[run 2](https://phux.sh/benchmarks/2026-09-30/run2/samples.json).

PTY-byte echo is not keystroke-to-pixel latency, and server memory is not a
whole application's footprint. A native GUI such as cmux needs its own
measurement.

## Next

- [Install](./install.md) phux and try the
  [first session](./getting-started.md): start work, detach, and reattach
  from another client.
- [Coding agents](./agents.md): connect your agent host.
- [Remote machines](./remote.md): attach to a server on another host.
