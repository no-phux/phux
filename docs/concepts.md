# Concepts

The handful of ideas the rest of the docs assume: a server that owns your terminals, the sessions, tabs, and panes inside it, the clients that show them, how commands name things, and what survives a restart.

```text
server ── session "api" ── tab 0 ── pane @1  (your shell)
   │                     │        └─ pane @2  (cargo watch)
   │                     └─ tab 1 ── pane @3  (claude)
   └──── session "notes" ─ tab 0 ── pane @4

clients: terminal UI, desktop app, browser, phone, every `phux` command
```

## Server

The server is the background process that owns every terminal: the programs running in them and what's on each screen. Each user on a machine has one.

`phux` and `phux new` start the server if it isn't running. Commands that only read, such as `phux ls`, say there's no server instead of starting one. The server stops by itself once nothing is running in it and no client is attached.

```sh
phux status    # pid, uptime, attached clients
```

**Nobody else can reach your server unless you set that up.** It listens only on a socket in your own runtime directory ([File locations](./reference/files.md#socket) has the path). [Remote machines](./remote.md) covers letting another machine in.

A build from a phux checkout runs a separate server, with its own socket and state, so working on phux never touches the sessions of the `phux` you have installed.

## Sessions, tabs, and panes

A **pane** is one terminal: one program (usually your shell) and its screen. A **tab** holds one or more panes, split side by side or stacked. A **session** is a named workspace that holds tabs, usually one per project.

```sh
phux new api      # a session named "api"
phux ls           # every session, and which ones need you
```

**A session you name stays until you delete it.** It's still there, empty, after its last program exits, until `phux rm` or `phux kill` removes it. A session with a generated name, like the one bare `phux` creates from the current directory, ends with its last program. `phux new --ephemeral NAME` gives a named session that behavior too.

A pane closes when its program exits. To keep a finished pane's last screen and exit status around for a while, set `retain-on-exit = true` under `[defaults]` in your [config file](./config.md).

**A tab has three names.** The UI says "tab". Actions in the config file keep tmux's word "window" (`new-window`, `next-window`). Targets and `--help` text say "group". All three mean the same thing.

## Clients

A client is anything that connects to the server to show or change what's in it:

- the terminal UI (`phux`, `phux attach`)
- the [desktop app](./clients.md#desktop)
- the [browser client](./clients.md#browser), and phone apps once they ship
- every command you run from a shell, such as `phux snapshot`, which connects, does one thing, and exits
- the [MCP server](./agents.md) a coding agent uses

They all see the same sessions, so you can watch a pane in the terminal UI while a script types into it. **Closing a client changes nothing on the server.**

Several clients can show one session at once. A pane has one size at a time; by default the smallest view wins and larger views get blank margins, like tmux. The `window-size` setting under `[defaults]` changes that.

## Hosts

A host is another machine running its own phux server that you've told this machine how to reach.

```sh
phux host add me@mini     # once, over your existing SSH access
phux attach mini          # from then on
phux ls --all             # sessions here and on every host
```

There's no account and no hosted service in between. [Remote machines](./remote.md) covers setup and what each machine can see.

## Targets

Commands that act on something take a target:

| Target     | Means                                              | Example                       |
| ---------- | -------------------------------------------------- | ----------------------------- |
| `name`     | a session                                          | `phux snapshot work`          |
| `name:G`   | tab `G` of a session, counting from 0, or its name | `phux kill work:1`            |
| `name:G.P` | pane `P` of that tab                               | `phux send-keys work:1.0 C-c` |
| `@N`       | a pane by id                                       | `phux run @7 'cargo test'`    |
| `host/@N`  | a pane on a linked machine                         | `phux snapshot edge/@7`       |
| `#tag`     | every pane with that tag                           | `phux kill '#build'`          |
| `%name`    | the pane a named agent runs in                     | `phux wait %reviewer`         |
| `^key`     | a session or pane by key (6+ hex digits)           | `phux kill ^3f9a2c`           |
| `.`        | the focused pane                                   | `phux signal . kill`          |

`phux help targets` prints the same table. Quote `#tag` in a shell, where `#` can start a comment.

## Identity and execution

Names, ids, and keys answer different questions:

- A **name** (`work`, `work:1`) is for people. You can rename a session or tab, so a name can point somewhere else tomorrow.
- A **pane id** (`@7`) names a pane that exists right now. Inside a pane, `$PHUX_TERMINAL_ID` holds it. Prefer ids in scripts. An id can be reused after its pane is gone or the server restarts, so don't store one for later.
- A **key** (`^3f9a2c`) is the lasting identity of a session or pane, kept across server restarts. `phux ls -l` shows them.

Starting a pane's program again, with `phux relaunch` or `phux restore`, is a new run in the same pane: same key, new process.

## What survives what

| Event                                                | Programs in panes | Sessions, tabs, and panes | Scrollback |
| ---------------------------------------------------- | ----------------- | ------------------------- | ---------- |
| detach, close the terminal, lose the SSH connection  | keep running      | kept                      | kept       |
| a client crashes                                     | keep running      | kept                      | kept       |
| `phux update` or `phux upgrade`                      | keep running      | kept                      | kept       |
| the server crashes, or `phux kill --server`          | end               | come back                 | lost       |
| the machine reboots                                  | end               | come back                 | lost       |

**When the server stops, the programs in it stop too.** The next server brings back your sessions, tabs, and panes, with each pane's directory, but no program is running in them. Start them again yourself:

```sh
phux restore           # start every pane a server stop ended
phux relaunch @4       # start one pane again
phux rm @4             # forget a pane instead
```

`phux restore` also resumes coding agents whose integration supports it.

To have the server start at login and come back by itself after a reboot:

```sh
phux service install
```

This writes a launchd agent on macOS or a systemd user unit on Linux. `phux service uninstall` removes it. [phux vs tmux, Herdr, and cmux](./compare.md#what-survives-what) compares this with the others.

## Agents

phux notices which panes run a coding agent and keeps track of its state: `working`, `blocked` (waiting for you), `idle`, or `done`. That's what the sidebar and <kbd>C-a</kbd> <kbd>q</kbd> use to show you which agent needs you. An agent's host can report its state directly; otherwise phux works it out from the program and the screen. See [Coding agents](./agents.md).
