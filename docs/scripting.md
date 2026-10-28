# Shell scripting

Every `phux` command except `phux` and `phux attach` runs without a UI, so a
shell script, a cron job, or a coding agent can create panes, type into them,
read their screens, and wait for output. A person can watch the same panes in
the terminal UI while the script runs, or nobody needs to.

```sh
#!/bin/sh
phux new --json -s build -c ~/src/app >/dev/null
phux run build 'cargo build' && echo "build passed"
phux snapshot build
```

[Using phux as an agent](./agent-quickstart.md) is the install-and-drive loop.
[Coding agents](./agents.md) covers what is specific to agents.

## Targets

A target names the session, tab, or pane a command acts on: `work`,
`work:1.0`, `@7`, `'#build'`, and so on. [Targets](./concepts.md#targets)
has the whole grammar, and `phux help targets` prints it.

**Use `@N` in scripts.** Names and tags are for people and can change under
you. `.` is whatever a person is looking at right now, so use it only when
that is what you mean.

## Keeping track of ids

Commands that create a pane print its id. With `--json` they print a
document you can read with [`jq`](https://jqlang.org):

```text
$ phux spawn --json -- bash
{
  "replayed": false,
  "satellite": null,
  "schema_version": 1,
  "terminal_id": 7
}
```

```sh
pane="@$(phux spawn --json -- bash | jq -r .terminal_id)"
phux send-keys "$pane" 'make test' Enter
```

| Command | Creates | Id is at |
| --- | --- | --- |
| `phux spawn --json` | A pane in the most recently active session | `.terminal_id` |
| `phux new --json -s NAME` | A session with one pane, without attaching | `.terminal_id` |

Inside a pane, `$PHUX_TERMINAL_ID` holds that pane's number (`7` for `@7`)
and `$PHUX_SOCKET` names its server.

## Creating panes and sessions

`phux new --json -s NAME` creates a session without attaching to it.
`phux spawn` adds a pane:

```sh
phux new --json -s api -c ~/src/api -- nvim
phux spawn --target @7 --split horizontal --ratio 0.6 -c ~/src/api -- htop
phux spawn --retain=600 -- make test
```

| Option | Does |
| --- | --- |
| `-- COMMAND` | Runs a command instead of your shell. |
| `-c DIR` | Sets the working directory. |
| `--target PANE` | Places the new pane beside an existing one. |
| `--split horizontal` / `vertical` | Beside the target (the default), or below it. Needs `--target`. |
| `--ratio F` | The fraction of the split the target keeps. Needs `--target`. |
| `--retain=SECS` | Keeps the exit status and last screen readable after the process exits. |
| `--idempotency-key HEX32` | Makes a retry return the first pane instead of spawning another. |

A pane normally disappears when its process exits. `--retain` keeps it
around for `phux resource show` and `phux resource wait` until the time runs
out or you `phux kill` it. Write `--retain=SECS` when a command follows, so
the number is not read as the command.

`phux new` starts a server if none is running; without `--json` it also
attaches. `phux new --json` fails if the session already exists, so a
script that runs twice should pick a fresh name or end the old session
first with `phux kill --yes NAME`. `phux spawn` never starts a server: with none running it
exits 1 with `no_server`.

## Typing into a pane

`phux send-keys` presses keys, tmux style. Each argument is a named key
(`Enter`, `Tab`, `Escape`, `Up`, `C-c`, `M-x`) or literal text:

```sh
phux send-keys @7 'make test' Enter
phux send-keys @7 C-c
```

`phux paste` delivers text as one paste, so multi-line text arrives intact
and editors don't auto-indent it. It reads stdin when you leave out the text.
A paste inserts and does not submit, so follow it with `Enter`:

```sh
git diff | phux paste @7
phux send-keys @7 Enter
```

Flags go before the target. Everything after the target is a key to send.

## Running a command

`phux run` types a command into a pane's shell, waits for it to finish, and
prints its output. It exits with the command's own status:

```text
$ phux run @7 'git status --short'
 M src/main.rs
exit=0 (155ms)
```

```sh
if phux run --timeout 300 @7 'cargo test'; then
  echo "tests passed"
fi
```

**`run` only types into a shell.** When something else is in the foreground,
such as `vim` or `less`, it refuses with exit 2, because there the same bytes
would be keystrokes. `--force` skips that check. It assumes a POSIX shell
(sh, bash, zsh).

`--timeout` covers the whole run, connecting included, and defaults to 600
seconds. A run that times out exits 125. Giving up does not stop the command
or take back what was typed. Flags must come before the target.

## Reading a screen

`phux snapshot` prints what is on a pane's screen. It does not attach,
resize, or disturb the pane, so it is safe to poll a pane someone is using.

```sh
phux snapshot @7                 # the screen as text
phux snapshot --json @7          # lines, cursor, size, and title
phux snapshot --tail 200 @7      # the last 200 rows, history included
phux snapshot --format html @7   # with colors, as HTML
```

| Option | Does |
| --- | --- |
| `--tail N` | Returns the last N rows, history then screen. |
| `--scrollback N` | Adds N rows of history above the screen. |
| `--unwrap` | Joins lines the terminal wrapped. |
| `--format html` / `vt` | Keeps colors and styling, as HTML or terminal escape sequences. |
| `--json` | Prints a document instead of text. |

## Waiting for output

`phux wait` polls a pane's screen until a line matches. It exits 0 on a
match and 124 when `--timeout` runs out:

```sh
phux wait --until 'Finished' --timeout 300 @7
phux wait --regex 'test result: (ok|FAILED)' --output-only --timeout 300 @7
phux wait --idle 750 @7
```

`--until` matches any line, including the command you just typed. Match text
that appears only in output, or pass `--output-only`, which ignores lines the
shell marked as your input. That needs a shell with OSC 133 integration;
without it, `wait` matches every line and says so on stderr.

`--idle MS` waits until the screen holds still. It is the default when you
give no pattern.

## Watching events

`phux watch` prints a pane's events as they happen, one per line, or one JSON
object per line with `--json`:

```text
$ phux watch @7
@7	unknown
@7	dirty
@7	command_started
@7	command_finished exit=0
@7	idle
```

With `--until EVENT`, it exits 0 at the first matching event:

```sh
phux watch --until command_finished --timeout 600 @7
phux watch --until asked --timeout 600 @7        # the pane's agent asked a question
```

The events are `agent_state`, `asked`, `bell`, `command_finished`,
`command_started`, `cwd_changed`, `dirty`, `idle`, `journal_gap`,
`pane_closed`, `pane_spawned`, `source_gap`, `terminal_control`,
`title_changed`, and `unknown`. `command_started` and `command_finished`
need a shell with OSC 133 integration. A timeout exits 124. If the server
closes the stream before the event arrives, `watch` exits 1.

To resume after a disconnect, pass the cursor `watch` printed last on stderr
to `--after`.

`phux resource wait` waits for a pane's process to exit:

```sh
phux resource wait --timeout 60 @7
```

It exits 0 when the process exited, 1 when the pane is gone, and 124 on
timeout.

## Organizing panes

```sh
phux tag add @7 build          # then target it as '#build'
phux ls --json                 # every session and pane
phux kill --yes @7             # end a pane
phux rm --yes @7               # forget a pane whose process ended
```

**`kill` and `rm` ask first.** On a terminal they ask for confirmation. With
no terminal they refuse with exit 2 and send nothing, so a script passes
`--yes` to say what it means. Quote `#tag` targets, because `#` starts a
comment in the shell.

## JSON output and errors

`--json` output is versioned. Each document has a `schema_version` that
changes only when a key is removed, renamed, or retyped. New keys appear
without a bump, so ignore keys you don't know. Run the command, or read its
`--help`, to see the shape.

On failure, stdout is empty and stderr carries one JSON object:

```json
{"error":{"code":"no_such_target","message":"no such target: @99"},"exit_code":1,"remedy":"run `phux ls` to see live sessions and panes","schema_version":1}
```

Branch on `error.code`, which comes from a fixed list, not on `message`.
`phux --capabilities --json` prints that list under `error_codes`, each
code with what it means.

A bounded `run`, `wait`, `watch`, `resource wait`, or `agent wait` that
runs out of time reports `timed_out` with exit 124 (125 for `run`). `wait`
still prints the pane's last screen on stdout, so you can see why the
condition never held. A timed-out `run` carries `error.recovery`:
`not_applied` means nothing reached the pane; otherwise the command may
still be running, so read the pane instead of sending it again.

A bounded `run`, `wait`, `watch`, `resource wait`, or `agent wait` that
runs out of time reports `timed_out` with exit 124 (125 for `run`). `wait`
still prints the pane's last screen on stdout, so you can see why the
condition never held. A timed-out `run` carries `error.recovery`:
`not_applied` means nothing reached the pane; otherwise the command may
still be running, so read the pane instead of sending it again.

Mutation failures can also carry additive `error.recovery` facts:

```json
{"operation_id":"12345678123456781234567812345678","scope":{"kind":"organization_authority"},"effect_certainty":"unknown","next_action":"reconcile"}
```

The scope distinguishes `work_store`, `organization_authority`,
`input_server_incarnation` and `mcp_adapter`; matching ID strings across
these domains do not identify the same operation. `scope.id` is included
only when an actual authority or executing incarnation is available. An
omitted identity is unknown, not permission to infer one from a socket,
pane number or later connection. Some existing source errors do not retain
that context.

`effect_certainty` is `not_applied`, `applied`, or `unknown`. These describe
the operation's effects, not task success: acknowledged input may have
reached the tty without an agent consuming or completing the turn.
`next_action` distinguishes `retry_same_operation`, `retry_new_operation`
and `reconcile`; normal authorization and retention limits still apply.
Unknown input delivery is reconcile-only—never silently resend it.

Legacy outer codes remain stable. For example, session creation still uses
`session_create_failed`, while `error.cause_code` preserves the underlying
classification and recovery metadata retains a known original operation
ID. Automation must not extract identities from diagnostic prose. See
[mail recovery](./agents.md#mail-and-path-claims) for the distinct durable
mail and adapter-ledger rules.

## Exit codes

Every command uses the same codes:

| Code | Means |
| --- | --- |
| `0` | Success. |
| `1` | Failure: no server, no such target, or the command failed. |
| `2` | Usage error, or the server refused the request. |
| `3` | A linked machine didn't answer, so the target couldn't be resolved. Retry later. |
| `124` | `wait`, `watch`, or `resource wait` timed out. |
| `125` | `run` timed out. Otherwise `run` exits with the command's own status. |

[Exit codes](./reference/exit-codes.md) has the full reference.

## Not touching the person's view

Reading, typing, and waiting never attach, resize, or move focus. Typing
into a pane someone is using is still allowed, so a script can collide with
a person. `phux take` gives you exclusive input to a pane, and
[agent safety](./agents.md#safety) covers the rest.

## Example: run tests beside a pane

This script runs a command in a new pane beside the current one and prints
the output if it fails:

```sh
#!/bin/sh
# tests.sh
cmd="${1:-make test}"

pane="@$(phux spawn --json --target "@$PHUX_TERMINAL_ID" -- sh | jq -r .terminal_id)"

if ! phux run --timeout 600 "$pane" "$cmd"; then
  phux snapshot --tail 50 "$pane"
  exit 1
fi
phux kill --yes "$pane"
```

```sh
./tests.sh 'cargo test'
```

## MCP

`phux mcp` serves the same commands as MCP tools over stdio, for programs
that speak MCP instead of running commands. See [MCP](./agents.md#mcp).

## Debugging

- `phux ls` and `phux ls -l` show what the server holds.
- `phux snapshot --json @N` shows exactly what a `wait` would match.
- `phux status` shows whether a server is running and which socket it uses.
- [Troubleshooting](./troubleshooting.md) covers failures that aren't yours.
