# Driving terminals from scripts

Every `phux` command other than `phux` and `phux attach` runs without a UI.
A script, a cron job, or a coding agent can create panes, read their
screens, type into them, and wait for things to happen, while a person
watches the same panes in the TUI or not at all.

This page covers the commands and the conventions. [Agents](./agents.md)
covers the parts specific to coding agents.

## Create

```sh
phux spawn --json -- bash                 # a pane in the current session; prints its id
phux spawn --json -c ~/src/app -- bash    # with a working directory
phux new --json -s build -c ~/src/app     # a new session, without attaching
```

The id (`@7`) is what every other command takes. Inside the pane,
`$PHUX_TERMINAL_ID` holds it and `$PHUX_SOCKET` names the server.

Panes normally vanish when their process exits. `phux spawn --retain=600`
keeps the exit status and last screen readable for ten minutes.

## Read

```sh
phux snapshot @7                 # the screen as text
phux snapshot --json @7          # with cursor, size, and wrap information
phux snapshot --tail 200 @7      # the last 200 rows including history
phux snapshot --format html @7   # rendered with colors
```

A snapshot does not attach, resize, or otherwise disturb the pane.

## Type

```sh
phux send-keys @7 'make test' Enter       # keys, tmux style; `C-c` works too
phux paste @7 < patch.diff                # paste text; does not press Enter
phux run @7 'git status'                  # run a command, get its output and exit code
```

`run` needs a shell to be reading the pane. It refuses when something else
is in the foreground, so it does not type a command into a running editor.
It returns the command's own exit code, or 125 if it timed out.

## Wait

```sh
phux wait --until 'Finished' --timeout 300 @7          # a line contains the text
phux wait --regex 'Finished|error\[' --timeout 300 @7   # or matches the pattern
phux watch --json @7                                   # stream events as they happen
phux watch --until command_finished --timeout 600 @7   # the shell reported a command ended
phux watch --until asked --timeout 600 @7              # the pane's agent asked for input
phux resource wait --timeout 60 @7                     # the pane's process exited
```

`wait` and `watch` exit 0 when the condition is met and 124 on timeout, so a
shell `||` reads naturally. `wait` polls the screen; `watch` subscribes to
the event stream and prints each event as a line (one JSON object per line
with `--json`). The event names are listed under `--until` in
[`phux watch --help`](./reference/cli.md#phux-watch).

Where the shell has OSC 133 integration, `command_started` and
`command_finished` events, and `wait --output-only`, tell command output
from the typed command. Without it, `wait` matches every line and says so.

## Organize

```sh
phux tag add @7 build          # then: phux snapshot '#build', phux kill '#build'
phux ls --json                 # every session and pane
phux kill @7 --yes             # --yes because kill asks on a terminal
phux rm @7                     # forget an ended pane
```

## Conventions

**`--json` output is versioned.** Each document carries a `schema_version`
that moves only when a key is removed, renamed, or retyped. Keys are added
without a bump, so ignore keys you don't know. The shapes come from the
binary, so check `phux <command> --help`, or run the command, rather than
expecting a schema here.

**Failures are one JSON line on stderr**, with stdout empty:

```json
{
  "error": { "code": "no_such_target", "message": "no such target: @99" },
  "exit_code": 1,
  "remedy": "run `phux ls` to see live sessions and panes",
  "schema_version": 1
}
```

`error.code` is a fixed vocabulary; branch on it, not on `message`.

**Exit codes** are the same everywhere: 0 ok, 1 failure, 2 usage error or
refused by the server, 3 partial fleet view (a linked host did not answer),
124 timeout, 125 `run` timeout. [Exit codes](./reference/exit-codes.md) is
the full table.

**Targets** are the grammar in [concepts](./concepts.md#targets). Prefer
`@N` ids in scripts; names and tags are for people and can change under
you. `.` means the focused pane, which is whatever a human is looking at,
so avoid it unless that is what you mean.

**Nothing here touches the human's view.** Reading, typing, and waiting
never attach, resize, or steal focus. Typing into a pane a person is using
is allowed, though; see [agents](./agents.md#safety) for the rules that keep
agents out of each other's and your panes.

## MCP

`phux mcp` runs the same surface as an MCP server over stdio, for hosts that
speak MCP rather than shelling out. See [agents](./agents.md#mcp).
