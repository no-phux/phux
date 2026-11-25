---
group: automate
order: 1
---

# Shell scripting

Every `phux` command except `phux` and `phux attach` runs without a UI, so a
script can create panes, type into them, read their screens, and wait for
output while a person watches, or nobody does.

[Using phux as an agent](./agent-quickstart.md) is the shortest install-and-drive
path. Every flag is in the [CLI reference](./reference/cli.md).

## Targets

A target names a session, tab, or pane: `work`, `work:1.0`, `@7`, `'#build'`
([grammar](./concepts.md#targets), `phux help targets`). **Use `@N` in
scripts.** Names and tags can change under you, and `.` is whatever a person
is looking at. Inside a pane, `$PHUX_TERMINAL_ID` is its number and
`$PHUX_SOCKET` its server.

## Creating panes and sessions

`phux new --json -s NAME` creates a session without attaching; `phux spawn`
adds a pane to the most recently active session. Both print the new pane's
`terminal_id` with `--json`. This creates a shell pane and drives only it:

```sh
(
  set -eu
  session="phux-demo-$$"
  created=$(phux new --json -s "$session" -- sh)
  pane="@$(printf '%s\n' "$created" | jq -er .terminal_id)"
  phux run --timeout 10 "$pane" 'printf "hello from phux\n"'
  phux snapshot "$pane"
  printf 'The shell is still running. From a terminal: phux attach %s\n' "$session"
)
```

```sh
phux spawn --target @7 --split horizontal --ratio 0.6 -c ~/src/api -- htop
phux spawn --retain=600 -- make test
```

- `phux new` starts a server if none is running; `phux spawn` does not and
  exits 1 with `no_server`.
- `phux new --json` fails if the session exists. Pick a fresh name, or
  `phux kill --yes NAME` first. A command that cannot start exits 1 with
  `spawn_failed` and leaves the session holding the failed launch.
- `--split horizontal` stacks (new pane below), `vertical` places side by
  side; both need `--target`.
- `--retain=SECS` keeps an exited pane readable by `phux resource show` and
  `phux resource wait`. Write it with `=` so a following command is not read
  as the number.
- If a placed spawn's reply is lost, read its receipt with
  `phux terminal operation` using the session and key from the error, rather
  than spawning again.

## Typing into a pane

Flags go before the target; every argument after it is a key. A named key
(`Enter`, `Tab`, `Escape`, `Up`, `C-c`, `M-x`) is pressed; anything else is
literal text.

```sh
phux send-keys @7 'make test' Enter
git diff | phux paste @7     # one paste, multi-line intact; does not submit
phux send-keys @7 Enter
```

## Running a command

`phux run` types a command into a pane's shell, waits, prints its output, and
exits with the command's status:

```sh
if phux run --timeout 300 @7 'cargo test'; then echo "tests passed"; fi
```

**`run` only types into sh, bash, zsh or fish.** With anything else in the
foreground it exits 2, because the bytes would be keystrokes (`--force`
skips the check). `--timeout` (default 600) covers the whole run; a timeout
exits 125 and neither stops the command nor takes back what was typed.

## Reading a screen

`phux snapshot` never attaches, resizes, or moves focus, so it is safe to
poll a pane someone is using.

```sh
phux snapshot @7                         # text
phux snapshot --json --tail 200 --unwrap @7
phux snapshot --format html @7           # with colors
phux snapshot --out frame.png @13        # a pixel surface or document
```

On a document resource, `snapshot` prints its content by kind; a pixel
surface needs `--out FILE`. Printed document text has control characters
replaced by visible signs so it cannot drive your terminal; `--out` and
`--json` keep exact bytes.

## Waiting for output

`phux wait` polls a pane's screen. It exits 0 on a match and 124 on timeout,
still printing the last screen so you can see why.

```sh
phux wait --until 'Finished' --timeout 300 @7
phux wait --regex 'test result: (ok|FAILED)' --output-only --timeout 300 @7
phux wait --idle 750 @7
```

`--until` also matches the command you just typed. `--output-only` skips
lines the shell marked as input, which needs OSC 133 shell integration.
`--idle MS` waits for the screen to hold still and is the default without a
pattern. On an accessibility tree or text document, `wait` follows the
resource instead. Terminal-only verbs refuse other resource kinds with
`wrong_resource_kind` (exit 2) before sending anything.

## Watching events

`phux watch` streams a pane's events, one per line, or JSON lines with
`--json`. `--until EVENT` exits 0 at the first match; the event names are in
the [CLI reference](./reference/cli.md#phux-watch).

```sh
phux watch --until command_finished --timeout 600 @7
phux watch --until asked --timeout 600 @7    # the agent asked a question
phux resource wait --timeout 60 @7           # until the process exits
```

A timeout exits 124; a stream the server closes first exits 1. To resume,
pass the cursor printed last on stderr to `--after`. `resource wait` exits 0
on exit, 1 when the pane is gone, 124 on timeout.

## Running a command when an event happens

`phux on` runs a local argv for each matching live journal event, with that
event's `phux watch --json` line on stdin:

```sh
phux on command_finished --target @7 -- jq -r '"\(.terminal) exited \(.exit_code)"'
```

No shell expansion. Handlers run one at a time, with only `PATH` inherited
and a 30-second limit. It is a live subscription, not a queue: no history,
no `--after`, no reconnect; a gap, disconnect, or handler failure exits
nonzero. Run only handlers you trust and treat event fields as data. For
server-side rules, use [hooks](./hooks.md).

## Organizing panes

```sh
phux tag add @7 build    # target it as '#build' (quote it: # starts a shell comment)
phux ls --json
phux kill --yes @7       # end a pane
phux rm --yes @7         # forget a pane whose process ended
```

### Child panes

A pane spawned from inside another pane is its child: `phux spawn` records
the pane it runs in (`$PHUX_TERMINAL_ID`) as the new pane's parent, and so
does the MCP `phux_spawn` tool. `--parent @N` names another, `--parent none`
none. `phux ls --json` lists it as the terminal's `parent`, and Rune's
sidebar folds children under their parent with a count. The parent is stored
as `phux.parent/v1` pane metadata; closing the parent closes nothing, and
its children keep naming it.

### Unseen marks

The server marks a pane when something happens in it that nobody saw: it
printed (`output`), rang its bell (`bell`), or its agent went from working or
blocked to done (`finished`). `phux ls --json` lists the strongest as the
terminal's `unseen`, and omits it once someone has looked:

```sh
phux ls --json | jq -r '.resources[] | select(.unseen == "finished") | .id'
phux seen @7             # or a session, group or #tag
```

A look clears the mark on every client. Rune reports the panes in front of
you, and a pane in some client's view collects no mark. The terminal UI does
not report yet, so what only it shows stays marked. Marks survive `phux
upgrade`, not a server restart. They are the `phux.unseen/v1` pane metadata;
a program that shows panes reports with `phux.in-view/v1` (`true` or `false`)
after attaching.

**Destructive verbs ask.** `kill`, `rm`, `detach`, `approve` and
`signal interrupt|terminate|kill` ask on a terminal; without one they exit 2
(`confirmation_required`) unless you pass `--yes`. Every state-changing verb
takes `--json`.

## Notes

`phux notify` tells the person something without writing into a pane:

```sh
phux notify --level warn "deploy 90%" "us-east still rolling"
```

Rune and the terminal UI show it as a toast; an out-of-sight `warn` or
`error` also raises a notification, per `[notifications]` in
[config](./reference/config.md). Each note replaces the pane's last one and
is stored as `phux.note/v1` pane metadata. Panes' OSC 9 and 777 are ignored
unless `[notifications] from-programs = "notes"`
([program protocol](./program-protocol.md#relationship-to-other-sequences)).

## JSON output and errors

Every `--json` document has a `schema_version` that changes only when a key
is removed, renamed, or retyped; ignore keys you don't know. On failure,
stdout is empty and stderr carries one object:

```json
{"error":{"code":"no_such_target","message":"no such target: @99"},"exit_code":1,"remedy":"run `phux ls` to see live sessions and panes","schema_version":1}
```

Branch on `error.code`, never `message`; `phux --capabilities --json` lists
every code under `error_codes`. A timed-out wait reports `timed_out`.

A mutation failure may carry `error.recovery`:

```json
{"operation_id":"12345678123456781234567812345678","scope":{"kind":"organization_authority"},"effect_certainty":"unknown","next_action":"reconcile"}
```

- `effect_certainty` is `not_applied`, `applied`, or `unknown`. It describes
  effects, not task success: delivered input may not have been read.
- `next_action` is `retry_same_operation`, `retry_new_operation`, or
  `reconcile`. **Never resend input whose delivery is unknown**; read the
  pane first.
- `scope.kind` (`work_store`, `organization_authority`,
  `input_server_incarnation`, `mcp_adapter`) says which domain the ID
  belongs to; equal IDs in different scopes are unrelated. An omitted
  `scope.id` is unknown, not inferable.

Do not parse identities out of messages. Mail has its own
[recovery rules](./agents.md#mail-and-path-claims).

## Exit codes

[Exit codes](./reference/exit-codes.md) lists them. `run` exits with the
command's status. `phux agent answer` exiting 3 (`answer_delivery_unknown`)
means part of the answer may have arrived: read the pane, never resend.

## Durable terminal creation

For integrations that must survive lost replies and restarts, the server's
public workflow creates, places and starts a terminal in one committed
intention, addressed by durable keys instead of `@N`. Ask the connected
server for its contracts:

```sh
phux api list
phux api describe terminal.create
```

| Intention | CLI | MCP |
| --- | --- | --- |
| Create, place, start | `phux terminal create --request-json JSON` | `phux_terminal_create` |
| Recover a receipt, no effects | `phux terminal operation` | `phux_terminal_operation` |
| Reconcile a resource | `phux resource show --json ^RESOURCE_KEY` | `phux_resource_show` |
| Read or wait for one execution | `phux resource execution --request-json JSON` | `phux_execution_result` |
| Read current questions | `phux api call question.list` | `phux_agent_questions` |

For the simple case, `phux terminal run` launches argv in a new terminal and
waits for it:

```sh
phux terminal run --session work -- make test
```

It exits with the child's status (128 + signal if signaled), 125 when
`--timeout` expires (the child keeps running), 126 on a confirmed launch
failure, and 1 when the outcome is unknown. The JSON `status` tells these
apart from a child exiting with the same number.

A full request, from the session's durable key in `phux ls --json`:

```sh
session=$(phux ls --json | jq -r '.sessions[] | select(.name == "work") | .key')
key=$(openssl rand -hex 16)
jq -n --arg session "$session" --arg key "$key" \
  '{session: $session, idempotency_key: $key, argv: ["/bin/sleep", "30"]}' |
  phux api call terminal.create --request-json -
```

- Argv runs directly in a **new** process: never shell source, never typed
  into an existing terminal. Empty argv starts the default shell.
- An explicit `cwd` must be absolute and exist on the server; it never falls
  back to another directory.
- Membership, placement and launch intent commit together. A refusal starts
  no process; a failed launch leaves a committed `failed` execution, with
  nothing to roll back.
- **Success means intent committed**, not that the process ran or the task
  finished. Save `resource`, `execution_ref` and the key from the receipt;
  `terminal_id` is a current handle only, never a durable address.
- Pass requests on stdin (`-`) so argv and environment stay out of the
  process list. Supported is not authorized: discovery needs global
  `INVENTORY`, placed creation `CREATE` and `ORGANIZE` on the session, and
  lookup `INVENTORY` there.

### Lost replies and retry scope

After a timeout or unreadable reply the outcome is unknown. Read the receipt
before anything else; the lookup never launches or edits:

```sh
phux terminal operation --session "$session" --idempotency-key "$key"
```

Resending the identical request with the same key replays the original
receipt without effects; the same key with another payload is refused.
Receipts survive reconnects and restarts on the same authority for 24 hours
or until 10,000 newer ones evict them. After that, lookup reports
`receipt_not_retained` (outcome `unknown`), and reusing the key executes
again. There is no exactly-once guarantee beyond retained receipts:
reconcile with `phux resource show --json "^$resource"`, which works after
exit, restart and receipt eviction, and follows the resource's latest
activation.

### Exact execution results

```sh
phux resource execution --request-json "$(jq -c '{execution: .execution_ref, timeout_secs: 30}' receipt.json)"
```

Without `timeout_secs` it reads once; with 1 to 3600 it waits while the
execution is active. `observed` carries the state and outcome (exit, signal,
failed launch, lost). `superseded` means a relaunch replaced it;
`unavailable` means the evidence is gone. Neither means the process never
ran. A timeout exits 124; a settled read exits 0, with the process status in
`observation.outcome`.

**After a restart, `failed` or `completed` with `unobserved` does not prove
no process started.** The launch intent committed; durable evidence of the
process did not. Reconcile external effects before retrying them. While
`execution.persistence_pending` is set, the state shown is the last
committed one, not proof the process is still running.
