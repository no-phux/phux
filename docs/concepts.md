---
group: start
order: 3
---

# Concepts

The terms the rest of the docs assume.

```text
server ── session "api" ── tab 0 ── pane @1  (your shell)
   │                     │        └─ pane @2  (cargo watch)
   │                     └─ tab 1 ── pane @3  (claude)
   └──── session "notes" ─ tab 0 ── pane @4

clients: terminal UI, desktop app, phone, every `phux` command
```

## Server

The background process that owns every terminal: its program and its
screen. Each user on a machine has one. `phux` and `phux new` start it;
read-only commands such as `phux ls` report that none is running instead.
`phux status` shows its pid, uptime, and clients.

It listens only on a socket in your runtime directory
([path](./reference/files.md#socket)). Nothing else can reach it until you
set up [remote access](./remote.md).

## Sessions, tabs, and panes

- A **pane** is one terminal: a program and its screen. It closes when its
  program exits, unless `[defaults] retain-on-exit = true`.
- A **tab** holds panes, split side by side or stacked. Config actions call
  it `window` (`new-window`); targets and `--help` call it `group`.
- A **session** is a named set of tabs, usually one per project. A session
  you create stays, empty, until you end it (`phux kill`). The session bare
  `phux` starts, and one from `phux new --ephemeral`, ends with its last
  program.

`phux lock NAME:G` stops every client from closing a tab until
`phux unlock`.

`phux new NAME` is exact creation: an existing name is refused, including
with `--ephemeral`. `phux attach NAME` only selects an existing session; it
never creates one. With `defaults.attach-session`, a no-name attach asks the
host to ensure that session atomically, then selects the receipt's exact
identity. Ensuring an existing session does not start another pane. Older
servers that cannot ensure refuse without creating; an uncertain receipt
requires inspecting `phux ls`, not automatically creating again.

`phux worktree open` uses the same atomic ensure: it preserves an existing
session and its panes, or creates the worktree's persistent session with its
project identity. `--attach` selects the receipt's exact session; a missing
live pane in headless output is a committed intent to reconcile, not permission
to create again.

## Resources

Everything the server hosts is a **resource** with a **kind**: a terminal,
an agent session, or a document a producer publishes (an accessibility tree,
a pixel surface, a typed document). `phux ls --json` lists them with their
parents. [Resource kinds](./resource-kinds.md) has the rest.

## Clients

Anything that connects to the server: the terminal UI, the
[desktop app](./clients.md#desktop), phone apps, the [MCP server](./agents.md#mcp),
and every one-shot command such as `phux snapshot`. All see the same state.
Closing a client changes nothing on the server.

When several clients show one session, the smallest view sets each pane's
size by default; `[defaults] window-size = "active"` sizes it for the client
you used last. A client can lock its view size (`phux attach --lock-size`, or
Lock view size in the palette): while it is attached, other clients see its
panes letterboxed or cropped instead of resizing them.

## Hosts

A host is another machine running its own server that this machine knows
how to reach. No account or hosted service sits in between.

```sh
phux host add me@mini   # once, over SSH
phux attach mini
phux ls --all           # sessions here and on every host
```

## Targets

A target names what a command acts on. `phux help targets` prints the
full grammar (tabs, tags, agents, keys, linked machines); for example:

| Target     | Means                                   | Example                       |
| ---------- | --------------------------------------- | ----------------------------- |
| `name:G.P` | pane `P` of tab `G` (numbers from 0)    | `phux send-keys work:1.0 C-c` |
| `@N`       | a pane by id                            | `phux run @7 'cargo test'`    |
| `%name`    | the pane a named agent runs in          | `phux wait %reviewer`         |

## Names, ids, and keys

- A **name** (`work`, `work:1`) is for people and can be renamed.
- A **pane id** (`@7`, `$PHUX_TERMINAL_ID` inside the pane) names a pane
  that exists now. It survives a server restart, but can be reused after the
  pane is gone. Use ids in scripts.
- A **key** (`^3f9a2c`, shown by `phux ls -l`) is the lasting identity of a
  session or pane.

`phux relaunch` or `phux restore` starts a new run in the same pane: same
key, new process.

## What survives what

| Event                                        | Programs     | Sessions, tabs, panes | Scrollback |
| -------------------------------------------- | ------------ | --------------------- | ---------- |
| detach, close the terminal, lose SSH         | keep running | kept                  | kept       |
| a client crashes                             | keep running | kept                  | kept       |
| `phux update` or `phux upgrade`              | keep running | kept                  | kept       |
| server crash, `phux kill --server`, reboot   | end          | come back             | lost, unless [`restore.scrollback`](#kept-scrollback) |

When the server stops, its programs stop. The next server brings back
sessions, tabs, panes, and each pane's directory, with nothing running:

```sh
phux restore       # start every pane a server stop ended; resumes supported agents
phux relaunch @4   # start one pane again
phux rm @4         # forget it instead
```

`phux restore` never replays an agent's one-shot task, and never types into
a program. It starts your default shell, enabled agent integrations and
`[[restore.program]]` resumes. Any other saved command is refused, and
nothing runs, until you review its exact recipe and type yes:
`phux relaunch ^KEY --approve-raw`, with the key `phux restore` prints
([why](./organization.md#restore-and-executable-authority)). To restore automatically, and to resume other programs that
report a resume token ([Resume](./program-protocol.md#resume)):

```toml
[restore]
on-start = true

[[restore.program]]
app = "nvim"
command = ["nvim", "-S", "${PHUX_RESUME_TOKEN}"]
```

`phux service install` runs the server under launchd or systemd (with
linger on Linux), so it starts at login and after a reboot.

### Kept scrollback

```toml
[restore]
scrollback = 2000                        # lines per pane, at most 10000; 0 is off
scrollback-exclude = ["secrets", "psql"] # session, pane, or program names
```

A pane that starts again then has its last lines in its history, above a
`-- before the restart, … --` divider. A crash loses the last few seconds of
output, and only the main screen is kept, never a full-screen program's.
Read [what is on disk](./security.md#kept-scrollback) before turning it on.
Restart the server after changing it.

## Agents

phux tracks which panes run a coding agent and its state: `working`,
`blocked` (waiting for you), `idle`, `done`, or `unknown`. The sidebar and
<kbd>C-a</kbd> <kbd>q</kbd> use it. See [Coding agents](./agents.md).
