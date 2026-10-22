# Hooks

A hook runs a command when something happens: a pane exits, a client
attaches, an agent changes state. Use hooks to send a notification,
write a log line, or kick off a script, without anything polling.

```toml
[[hooks.pane-exit]]
when   = { exit-code = "*" }
action = { kind = "run", command = "notify-send \"pane exited with $PHUX_EXIT_CODE\"" }
```

Hooks live in your [configuration file](./config.md). The server reads
them when it starts, so after editing them run `phux config check`, then
`phux upgrade` to restart the server in place without closing any pane.

## Writing a hook

Each hook is one `[[hooks.<event>]]` entry with two keys:

- `when`: what the event must look like for the hook to match.
- `action`: what to do when it matches.

An event can have several entries. They are tried in order, and **the
first one that matches wins**: it uses up the event, whether or not its
action runs anything. Put specific entries before general ones:

```toml
# A clean exit: do nothing.
[[hooks.pane-exit]]
when   = { exit-code = 0 }
action = "noop"

# Any other exit: alert.
[[hooks.pane-exit]]
when   = { exit-code = "*" }
action = { kind = "run", command = ["say", "a pane failed"] }
```

## Actions

| Action | Does |
| --- | --- |
| `{ kind = "run", command = "..." }` | Runs the string with `/bin/sh -c`, so pipes and `$VARS` work. |
| `{ kind = "run", command = ["prog", "arg"] }` | Runs the program directly with those arguments. |
| `"noop"` | Matches and does nothing, on purpose. Use it to stop later entries from running. |

Any other action never runs on the server, and `phux config check` flags
it, because a match would still use up the event.

## Matching with `when`

Every key in `when` must hold. Each key names a piece of the event's
context:

| Value | Matches |
| --- | --- |
| `"*"` | Anything, even when the key is absent |
| `"text"` | Exactly that value |
| `0`, `true` | That value as text, so `exit-code = 0` matches `"0"` |

A key ending in `-startswith` matches the start of the value:

```toml
[[hooks.agent-state-changed]]
when   = { agent-kind-startswith = "claude", to = "blocked" }
action = { kind = "run", command = "notify-send 'Claude needs you'" }
```

Leave `when` out to match every firing. A key that is absent from a
particular firing makes the clause fail, except for `"*"`.

## Events

| Event | Fires when | Context keys |
| --- | --- | --- |
| `after-new-pane` | A pane is created, before its program prints anything | `session`, `terminal-id` |
| `pane-exit` | A pane's program exits | `exit-code` (absent when a signal killed it), `terminal-id` |
| `focus-changed` | A client focuses a pane | `client-id`, `terminal-id` |
| `client-attached` | A client finishes attaching | `client-id`, `session` |
| `client-detached` | A client detaches or drops | `client-id`, `session` |
| `agent-state-changed` | An agent's state changes, from any source (detection, agent events, OSC 7501, `phux agent set` / `clear`) | `agent-kind`, `agent-name`, `from`, `to`, `terminal-id` |
| `worktree-created` | `phux worktree new` creates a checkout, before its session starts | `branch`, `repo`, `session`, `worktree` |

`worktree-created` is the one event the CLI fires rather than the server:
it waits for each hook to finish, and it reads your file each time, so
edits apply at once.

The [hooks reference](./reference/hooks.md) has the full detail of when
each event fires and which keys may be absent.

## What the command gets

Each context key arrives as an environment variable: `exit-code` is
`PHUX_EXIT_CODE`, `terminal-id` is `PHUX_TERMINAL_ID`, and so on. Every
hook also gets:

- `PHUX_EVENT`: the event name.
- `PHUX_SOCKET`: the server that fired it, so a plain `phux` command in
  the hook talks to that server.

```toml
[[hooks.pane-exit]]
when   = { exit-code = "*" }
action = { kind = "run", command = "echo \"$PHUX_EVENT $PHUX_TERMINAL_ID $PHUX_EXIT_CODE\" >> ~/phux-exits.log" }
```

**A hook never slows the server down.** Commands run in the background,
at most 8 at once, each with a 30-second limit after which it is killed.
If events arrive faster than hooks can run, the excess events are
dropped.

Plugins can declare hooks too. Every enabled plugin hook for an event
runs; first-match-wins applies only to the entries in your file. See
[Plugins](./plugins.md).

## Debugging

`phux config check` catches the mistakes that would make a hook silently
do nothing:

```text
$ phux config check
hooks.pane-exit[0].when.exitcode: unknown name: unknown when key `exitcode`: this clause can never match; `pane-exit` context keys are exit-code, terminal-id (did you mean `exit-code`?)
hooks.pane-exit[1].action: dead action: action `message` never executes server-side (only `run` does; `noop` is the deliberate no-op); a match still consumes the event
hooks.pane-exited: unknown name: unknown hook event `pane-exited` (did you mean `pane-exit`?); these entries will never fire
3 problems
```

A plugin's hooks live in its manifest, so `phux plugin validate` checks
those: an `[[events]]` entry whose `on` is not a hook event fails, with
the nearest event suggested. The server skips such an entry with a
warning.

`phux plugin log` shows the last runs of every hook and plugin action,
with exit status and duration. Failed runs show the end of their output:

```text
$ phux plugin log --failed
2026-10-21 22:33:56  hook    hooks.pane-exit[0]  exit 127  9ms
    /bin/sh: notify-send: command not found
```

Hooks from your file are named by their entry, `hooks.<event>[<index>]`,
counting from 0. If a hook never appears in the log, check that the
server was restarted after the edit, and that no earlier entry for the
same event matches first.
