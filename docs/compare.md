# phux vs tmux, Herdr, and cmux

All four keep terminal work running while you look away, and all four can be scripted. They differ in what owns your terminals and what you can build on top. Pick by the job you have, not by a feature count.

Facts about the other three were checked against each project's own documentation on 2026-10-19: tmux 3.8, Herdr 0.9.5, and cmux 0.65.2. Newer releases may differ.

## At a glance

|                         | tmux                                  | Herdr                                     | cmux                                             | phux                                                     |
| ----------------------- | ------------------------------------- | ----------------------------------------- | ------------------------------------------------ | -------------------------------------------------------- |
| What it is              | terminal multiplexer                  | workspace app for coding agents           | native Mac terminal app                          | terminal multiplexer with a scriptable server            |
| Programs live in        | a background server                   | a background server                       | the app, unless you opt in to a tmux owner       | a background server                                      |
| Clients                 | one terminal client                   | one terminal client                       | the Mac app                                      | terminal UI and desktop app; a browser client to build on; phone apps in progress |
| Shows which agent needs you | no; add it yourself               | yes                                       | yes, as notifications                            | yes                                                      |
| Scripting               | commands and [control mode](https://github.com/tmux/tmux/wiki/Control-Mode) | [socket API](https://herdr.dev/docs/socket-api/) | [CLI and socket API](https://cmux.com/docs/api) | [commands with JSON output](./scripting.md), an MCP server, and the [protocol](./protocol.md) its own clients use |
| Other machines          | run tmux there over SSH               | over SSH, a server on each machine        | `cmux ssh` opens a remote workspace              | `phux host add`, then attach directly; a server on each machine |

## Choose by the job

| If your main job is…                                                                        | Start with | What you accept                                                                      |
| ------------------------------------------------------------------------------------------- | ---------- | ------------------------------------------------------------------------------------ |
| keeping shells alive over SSH with keys everyone already knows                              | tmux       | Agent awareness is yours to add.                                                     |
| running several coding agents in one window, with workspaces and saved SSH machines         | Herdr      | You adopt Herdr's workspace model and its terminal client.                           |
| a native Mac terminal with a built-in browser, a sidebar, and agent notifications           | cmux       | The Mac app is the experience; quitting it ends your programs unless you opt in to its tmux owner. |
| scripting terminals, attaching several kinds of client to the same panes, or reaching other machines | phux | More moving parts, and far less history than tmux.                          |

## Compared with tmux

Both start, split, detach, and reattach, and most keys match: phux uses <kbd>C-a</kbd> where tmux uses <kbd>C-b</kbd>. [Coming from tmux](./getting-started.md#coming-from-tmux) lists the differences.

tmux is a mature multiplexer with one terminal client. In phux, every client (the terminal UI, the desktop app, the browser, a script, an agent) talks to the server the same way and reads the same state, so none of them has to scrape another's screen. The server and its clients use the same terminal emulator, libghostty, so what a client draws is what the server has.

That costs moving parts. If you don't need several clients, agent state, or direct remote attach, tmux is the better-established choice.

## Compared with Herdr

Both keep real terminals in a background server, survive a client detaching, show which agent needs you, and can be driven from scripts. Both reach other machines over SSH, with a server on each machine.

Herdr is an application for agent workspaces: its server owns workspaces, tabs, and panes, and its API addresses that model. It reads most agents' state from what they draw on screen; some agents report their state to it directly ([how status works](https://herdr.dev/docs/agents/)).

phux learns an agent's state from the best source it has: the agent declaring it, the agent's host reporting it, a hook, and only then the screen ([agent states](./agents.md#agent-states)). After the first SSH setup, phux attaches to another machine's server directly, without SSH in the path ([Remote machines](./remote.md)).

Choose Herdr for one cohesive agent workspace, its range of supported agents, and its plugins. Choose phux for several independent clients, agent state reported by the agent's own host, or a protocol to build your own client on.

## Compared with cmux

cmux is a native Mac app built on libghostty: workspaces, splits, vertical tabs, a scriptable built-in browser, and agent notifications in one window ([features](https://github.com/manaflow-ai/cmux)). `cmux ssh` opens a workspace on a remote machine.

Choose cmux for that integrated terminal, browser, and notification experience. Choose phux to keep terminals running independently of any one app, attach several clients to the same panes, or give an agent its own panes to read and drive.

## What survives what

| Event                     | tmux                   | Herdr                                                                                       | cmux                                                                                                                          | phux                                                                                         |
| ------------------------- | ---------------------- | ------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------- |
| client detaches or closes | programs keep running  | programs keep running                                                                       | quitting the app ends them, unless the opt-in [tmux owner](https://github.com/manaflow-ai/cmux/blob/main/docs/local-tmux.md) holds them | programs keep running                                                                        |
| server or app restarts    | everything is gone     | layout returns with new shells; [supported agents can resume](https://herdr.dev/docs/session-state/) | layout, directories, and best-effort scrollback return with new shells; [supported agents can resume](https://github.com/manaflow-ai/cmux#session-restore) | sessions, tabs, and panes return; programs and scrollback don't ([details](./concepts.md#what-survives-what)) |

None of the four keeps running programs across a reboot.

## Measurements

On 2026-09-30, two runs compared the terminal clients of phux 0.46, tmux 3.7c, and Herdr 0.9.0 on one heavily loaded Apple M4 Pro. The rankings reversed between runs: phux's median key echo was the slowest of the three in one run and the fastest in the other. The one consistent result was that tmux's server used the least memory in both. Treat these as old versions on a noisy machine, not a speed ranking. The raw samples and commands are published for [run 1](https://phux.sh/benchmarks/2026-09-30/run1/samples.json) and [run 2](https://phux.sh/benchmarks/2026-09-30/run2/samples.json).

Key echo here is measured in terminal bytes, not from keystroke to pixel, and server memory isn't a whole application's footprint. A native app such as cmux needs its own measurement.

## Next

- [Install](./install.md) phux and take the [first-session tour](./getting-started.md).
- [Coding agents](./agents.md): connect your agent's host.
- [Remote machines](./remote.md): attach to a server on another machine.
