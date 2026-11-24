---
group: customize
order: 4
---

# Hooks

How to run a command when something happens: a pane exits, a client
attaches, an agent changes state. Every event, its context keys, and the
environment a hook gets are in the [hooks reference](./reference/hooks.md).

```toml
# A clean exit: do nothing.
[[hooks.pane-exit]]
when   = { exit-code = 0 }
action = "noop"

# Any other exit: alert.
[[hooks.pane-exit]]
when   = { exit-code = "*" }
action = { kind = "run", command = "notify-send \"pane exited with $PHUX_EXIT_CODE\"" }
```

## Applying a change

The server reads hooks when it starts. After editing, run `phux config
check`, then `phux upgrade` to restart the server in place without closing
any pane. `worktree-created` is the exception: the CLI fires it and reads
your file each time. For a temporary listener with no config change, use
[`phux on`](./scripting.md#running-a-command-when-an-event-happens).

## Matching

Each `[[hooks.<event>]]` entry has a `when` and an `action`. Entries for one
event are tried in order and **the first match wins**: it uses up the event
whether or not its action runs anything. Put specific entries first, and use
`"noop"` to stop later ones on purpose.

Every key in `when` must hold. `"*"` matches anything, even an absent key;
a key ending in `-startswith` matches a prefix; anything else is an exact
match (`exit-code = 0` matches `"0"`). Leave `when` out to match every
firing.

```toml
[[hooks.agent-state-changed]]
when   = { agent-kind-startswith = "claude", to = "blocked" }
action = { kind = "run", command = ["say", "Claude needs you"] }
```

A string `command` runs with `/bin/sh -c`; an array runs directly. Each
context key arrives as `PHUX_<KEY>` (`exit-code` is `PHUX_EXIT_CODE`), plus
`PHUX_EVENT` and `PHUX_SOCKET`, so a plain `phux` in the hook talks to the
server that fired it.

Hooks never slow the server: at most 8 run at once, each killed after 30
seconds, and events that arrive faster than that are dropped.
[Plugin](./plugins.md) event hooks all run; first-match-wins applies only to
your file.

## Debugging

`phux config check` catches a hook that would silently do nothing: an
unknown event, a `when` key the event never has, an action other than `run`
or `noop`.

```text
hooks.pane-exit[0].when.exitcode: unknown name: unknown when key `exitcode`: this clause can never match; `pane-exit` context keys are exit-code, terminal-id (did you mean `exit-code`?)
```

`phux plugin log` shows recent hook runs as `hooks.<event>[<index>]` with
exit status and duration; `--failed` adds the end of their output. A hook
missing from the log means the server hasn't restarted since the edit, or an
earlier entry matched first.
