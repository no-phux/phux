---
group: start
order: 4
---

# phux vs tmux, Herdr, and cmux

How phux differs from three tools that also keep terminal work running and
can be scripted. Facts about the others were checked against their own docs
on 2026-10-19 (tmux 3.8, Herdr 0.9.5, cmux 0.65.2).

## At a glance

|                     | tmux                    | Herdr                           | cmux                                 | phux                                    |
| ------------------- | ----------------------- | ------------------------------- | ------------------------------------ | --------------------------------------- |
| What it is          | terminal multiplexer    | workspace app for coding agents | native Mac terminal app              | terminal multiplexer with a scriptable server |
| Programs live in    | a background server     | a background server             | the app, unless you opt in to a tmux owner | a background server               |
| Clients             | one terminal client     | one terminal client             | the Mac app                          | terminal UI, desktop app, phones (testing), scripts |
| Agent state         | add it yourself         | yes, mostly from the screen     | notifications                        | yes, best evidence first ([states](./agents.md#agent-states)) |
| Scripting           | commands, [control mode](https://github.com/tmux/tmux/wiki/Control-Mode) | [socket API](https://herdr.dev/docs/socket-api/) | [CLI and socket API](https://cmux.com/docs/api) | [JSON CLI](./scripting.md), MCP, [protocol](./protocol.md) |
| Other machines      | run tmux there over SSH | SSH, a server on each machine   | `cmux ssh`                           | `phux host add`, then attach directly   |

## Choose by the job

| Main job                                                        | Pick  | Trade-off                                              |
| --------------------------------------------------------------- | ----- | ------------------------------------------------------ |
| shells that survive SSH, with keys everyone knows               | tmux  | agent awareness is yours to add                        |
| one cohesive workspace for many coding agents                   | Herdr | you adopt its workspace model and client               |
| a native Mac terminal with a built-in browser and notifications | cmux  | quitting the app ends programs unless you opt in to its tmux owner |
| several clients on the same panes, scripting, other machines    | phux  | more moving parts, far less history than tmux          |

[Coming from tmux](./getting-started.md#coming-from-tmux) lists the key
differences.

## What survives what

| Event                     | tmux             | Herdr                                     | cmux                                              | phux                                                       |
| ------------------------- | ---------------- | ----------------------------------------- | ------------------------------------------------- | ---------------------------------------------------------- |
| client detaches or closes | programs run on  | programs run on                           | quitting ends them, unless the [tmux owner](https://github.com/manaflow-ai/cmux/blob/main/docs/local-tmux.md) holds them | programs run on |
| server or app restarts    | everything gone  | layout, new shells; [some agents resume](https://herdr.dev/docs/session-state/) | layout, directories, some scrollback, new shells; [some agents resume](https://github.com/manaflow-ai/cmux#session-restore) | sessions, tabs, panes return, scrollback if you turn it on; programs don't ([details](./concepts.md#what-survives-what)) |

None of the four keeps programs running across a reboot.

## Measurements

On 2026-09-30, two runs compared the terminal clients of phux 0.46, tmux 3.7c, and Herdr 0.9.0 on one heavily loaded Apple M4 Pro. The rankings reversed between runs: phux's median key echo was the slowest of the three in one run and the fastest in the other. The one consistent result was that tmux's server used the least memory in both. Treat these as old versions on a noisy machine, not a speed ranking. The raw samples and commands are published for [run 1](https://phux.sh/benchmarks/2026-09-30/run1/samples.json) and [run 2](https://phux.sh/benchmarks/2026-09-30/run2/samples.json).

Key echo here is measured in terminal bytes, not from keystroke to pixel, and server memory isn't a whole application's footprint. A native app such as cmux needs its own measurement.
