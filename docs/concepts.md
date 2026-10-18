# Concepts

The few ideas the rest of the docs assume: one server, sessions of panes,
how commands name things, and what survives a restart.

## The server

Each user runs one phux server. It owns every pseudo-terminal, the
processes running in them, and the screen contents of each one (parsed by
[libghostty](https://github.com/ghostty-org/ghostty), the terminal emulator
library from Ghostty). Clients connect to the server, show what it has, and
send it input. Closing a client changes nothing on the server.

`phux` and `phux new` start a server if none is running; read-only
commands such as `phux ls` say there is none instead. Unless you set up
[remote access](./remote.md), the server listens only on a Unix socket in
your runtime directory ([Files](./reference/files.md) has the path). It
exits by itself once it has no running processes and no clients.

A development build uses a separate server, socket, and state directory
from an installed release, so the two never touch each other.

## Sessions, tabs, panes

A **session** is a named workspace. It holds **tabs**, and each tab holds
**panes** arranged in splits. A pane is one terminal: one pseudo-terminal,
one process (usually your shell), one screen.

A session you name with `phux new NAME` is persistent: it stays, empty,
after its last process exits, until you `phux rm` or `phux kill` it. A
session with a generated name, like the one bare `phux` creates, ends with
its last process.

The UI says "tab". Key binding actions say "window" (`new-window`), and
targets say "group". They are the same thing.

## Targets

Commands that act on something take a target:

| Target     | Means                                          | Example                       |
| ---------- | ---------------------------------------------- | ----------------------------- |
| `name`     | a session                                      | `phux snapshot work`          |
| `name:G`   | tab `G` of a session (from 0, or its name)     | `phux kill work:1`            |
| `name:G.P` | pane `P` of that tab                           | `phux send-keys work:1.0 C-c` |
| `@N`       | a pane by id                                   | `phux run @7 'cargo test'`    |
| `host/@N`  | a pane on another server this one is linked to | `phux snapshot edge/@7`       |
| `#tag`     | every pane with that tag                       | `phux kill '#build'`          |
| `%name`    | the pane a named agent runs in                 | `phux wait %reviewer`         |
| `^key`     | a session or pane by key (6+ hex digits)       | `phux kill ^3f9a2c`           |
| `.`        | the focused pane                               | `phux signal . kill`          |

`phux help targets` prints the same table. Quote `#tag` in a shell, where
`#` starts a comment.

## Clients

The terminal UI (`phux`, `phux attach`) is one client. The
[desktop app](./clients.md) is another. Every command you run from a shell
(`phux ls`, `phux snapshot`, `phux send-keys`) is a short-lived client too,
as is the [MCP server](./agents.md#mcp). They all use the same protocol and
see the same sessions, so you can watch in the UI while a script types.

Several clients can show one session at once. A pane has one size at a
time; the most recent resize sets it.

## What survives what

| Event                                               | Processes    | Sessions, tabs, panes                |
| --------------------------------------------------- | ------------ | ------------------------------------ |
| detach, close the terminal, lose the SSH connection | keep running | kept                                 |
| `phux update`, `phux upgrade`                       | keep running | kept                                 |
| server crash, `phux kill --server`, reboot          | stop         | kept, with the panes marked as ended |

The session layout is stored on disk, so it comes back after a crash or a
reboot, but the processes in it do not. Scrollback is not stored.

```sh
phux restore           # start again every pane that a restart ended
phux relaunch @4       # start one ended pane again
phux rm @4             # forget an ended pane
```

To have the server start at login and come back after a reboot:

```sh
phux service install
```

This writes a launchd agent on macOS or a systemd user unit on Linux.
`phux service uninstall` removes it.

## Agents

phux tracks which panes run a coding agent and what state each one is in:
`working`, `blocked` (waiting for you), `idle`, or `done`. An agent
integration can report its state directly; otherwise phux works it out from
the process name and the screen. See [agents](./agents.md).
