# Any program can report status

A program running in a phux pane can say what it is doing by writing one
escape sequence to its terminal. phux records that and shows it in the
sidebar, the agents view, and `phux agent list`. The program needs no phux
library, no socket, and no hook.

The sequence is OSC 7501, from revision 0.4 of the
[Program Status Protocol](https://www.superlogical.com/rex/docs/build/program-status).
Mitchell Hashimoto's
[post](https://mitchellh.com/writing/program-status-osc7501) is where that
specification started. This page is how phux applies it. The rules, the
keys, and the limits are in the [program protocol](./program-protocol.md).

```sh
printf '\033]7501;state=blocked:kind=permission:app=mytool\033\\'
```

`ESC ] 7501 ;` starts the report and `ESC \` ends it (`BEL`, byte `0x07`,
also ends it). The body is `key=value` pairs separated by `:`. This report
uses three keys the protocol defines:

| Key | Here | Meaning |
| --- | --- | --- |
| `state` | `blocked` | Required. One of `idle`, `working`, `done`, `blocked`, `error`, `clear`. |
| `kind` | `permission` | With `blocked` only: `permission`, `question`, or `auth`. |
| `app` | `mytool` | A stable program name, 1 to 32 of `[A-Za-z0-9_.+-]`. |

`clear` removes records; it is not a state phux displays. Other keys
(`id`, `progress`, `title`, `msg`) are in the
[protocol](./program-protocol.md#keys). A missing or unknown `state`
discards the report. Unknown keys are ignored.

## A shell function

Write to `/dev/tty` when standard output may be redirected. `kind` is
omitted unless the program is blocked:

```sh
report() {
  state=$1
  kind=$2
  app=${3:-mytool}
  if [ -n "$kind" ]; then
    printf '\033]7501;state=%s:kind=%s:app=%s\033\\' "$state" "$kind" "$app" > /dev/tty
  else
    printf '\033]7501;state=%s:app=%s\033\\' "$state" "$app" > /dev/tty
  fi
}

report working "" mytool
report blocked permission mytool
report "done" "" mytool
```

[`examples/agents/report-status.sh`](../examples/agents/report-status.sh)
is that function as a script. With no arguments it reports `working`, then
`blocked`, waits, then `done`. Each of those reports also sends `title`,
the base64 of `deploy`:

```sh
printf '\033]7501;state=blocked:kind=permission:app=mytool:title=ZGVwbG95\033\\'
```

phux takes the agent name from `title` and the kind from `app`. The demo
sends two different words, so the pane is addressable as `%deploy`. A
report replaces its record, so every report in the sequence carries the
title.

A shell prompt mark, OSC 133 `A`, ends `working` and `blocked` records, and
so does the pane's process exiting. `idle`, `done`, and `error` stay.
[`examples/agents/05-program-status.sh`](../examples/agents/05-program-status.sh)
reads `phux agent list` while the program is still blocked:

```sh
bash examples/agents/05-program-status.sh
```

It stands up its own server on a private socket, the same way the other
scripts in [`examples/agents`](../examples/agents/README.md) do.

## Feature detection

A program can ask whether the terminal speaks the protocol. The query
creates no record:

```sh
printf '\033]7501;?\033\\' > /dev/tty
```

phux answers with the terminator the query used:

```text
OSC 7501 ; ?:states=idle,working,done,blocked,error:kinds=permission,question,auth ST
```

That byte string is what the server test expects
([`reports.rs`](../crates/phux-server/tests/agents/program_status/reports.rs)).
A probe against the same debug build, from a pane that sent the query and
then primary device attributes, read this reply first:

```text
ESC ] 7501 ; ?:states=idle,working,done,blocked,error:kinds=permission,question,auth ESC \
```

The device-attributes answer followed it. A program that does not understand
a pair in the reply ignores it.

Detection is optional: a terminal drops an OSC it does not know, so a
program may send reports without asking. The
[protocol](./program-protocol.md#feature-detection) has the rest, including
the device-attributes check.

## Wrappers

The shell function is the wrapper. Call it from a script, or from a hook
that already runs a command in the pane. None of the agent integrations in
this repository write OSC 7501, and this page does not claim that Claude
Code, Codex, OpenCode, or pi emit it on their own.

Where the repository documents a command hook, the hook can run the same
script. Claude Code's plugin registers command hooks for `SessionStart`,
`UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `PostToolUseFailure`,
`PermissionRequest`, `Notification`, `Stop`, and `SessionEnd`
([`hooks.json`](../integrations/claude/hooks/hooks.json)). The script those
hooks run talks to phux on the socket. It does not print OSC 7501, and the
[plugin readme](../integrations/claude/README.md) says the plugin never
declares a state: the server derives one from the session stream, or from
its own detector. A command hook of your own can exec
`report-status.sh`. A live agent session outranks OSC 7501 in the order
below, so a report does not replace a state that session already published.

pi's extension and the OpenCode plugin also report through an agent
session, not through this sequence. Their readmes describe that stream
([pi](../integrations/pi/README.md),
[OpenCode](../integrations/opencode-v2/README.md)) and do not document a
command hook whose job is to write a terminal sequence. Use the shell
function. This repository does not document a Codex hook or plugin that
reports state; a Codex pane with nothing stronger is read from the screen
rules, and the same shell function is what a program in that pane can run.

## What a run shows

This is the output of one run of
`examples/agents/05-program-status.sh` against a debug build of this
checkout. The program is unchanged from main `c63ec1ad6`. The pane id is
the one that server assigned. Columns are separated by tabs.

`phux agent list` is the first line. `phux agent explain @1` is the line
after the blank line, plus the `agent_record` line under it. The second
column is the program's `app`, which the agent record uses as its kind.
The address is `%deploy`: the name is the `title`, and the kind is the
`app`. `permission` stays on the program-status record, not in that
column. `mytool` has no detection manifest, so explain says there is no
rule evidence to show.

```text
@1	mytool	%deploy	blocked	1.00	the phux.agent/v1 record for this pane (ADR-0040); no detection manifest is loaded for `mytool`, so there is no rule evidence to show

@1	mytool	%deploy	blocked	1.00	the phux.agent/v1 record for this pane (ADR-0040); no detection manifest is loaded for `mytool`, so there is no rule evidence to show
  - agent_record 1.00: phux.agent/v1 record published for this pane ({"name":"deploy","kind":"mytool","state":"blocked"})
```

From the same run, `phux resource show @1` included these lines. The
command also printed the pane's process.

```text
agent: {"kind":"mytool","name":"deploy","state":"blocked"}
program_status: {"active":{"app":"mytool","id":"","kind":"permission","state":"blocked","title":"deploy"},"record_ids":[""]}
```

The [protocol](./program-protocol.md#how-phux-shows-program-status) says
which agent-record fields come from which keys. An `error` report is stored
on the agent record as `done`, with high attention.

## How phux ranks a report

phux is evidence-ranked; screen is the floor.

The order is designed strongest first. It is what the server does with the
evidence it has, as specified in
[precedence](./program-sequences.md#precedence) and implemented in
[`program_status.rs`](../crates/phux-server/src/runtime/program_status.rs)
and the detector drain in
[`agent_state.rs`](../crates/phux-server/src/runtime/connections/agent_state.rs):

1. A state declared with `phux agent set --state`. phux leaves it until it
   is cleared.
2. A live agent session fed by `phux agent emit`.
3. OSC 7501 records on the pane.
4. Hook reports from `phux agent report-state`.
5. The foreground program and the screen, matched against built-in rules.

While a pane has any OSC 7501 record and no live session, the drain does
not publish a screen-derived state, and it drops the state a hook report
would have published. A declaration or a live session is left in place:
the OSC 7501 projection returns without writing over them. This page
describes that design. A run against a real Claude Code, Codex, OpenCode,
or pi session, with and without those integrations, is not in this
repository.

An agent with no integration falls back to the built-in screen rules.
Rules ship for Claude Code, Codex, OpenCode, pi, Oh My Pi, Grok, Amp,
Cursor, Gemini CLI, Goose, and Aider. A screen that matches no rule reads
as `idle`, never `blocked`. The detector is level-triggered: it
re-derives state from the screen on each tick, and nothing matched means
idle
([`agent_detect`](../crates/phux-server/src/agent_detect/mod.rs)). The
ranks inside that detector, screen then process then hook then session
stream, are
[`EvidenceSource`](../crates/phux-server/src/agent_state.rs). OSC 7501 is
not one of those four. It publishes the agent record itself, and the drain
skips the screen while a record is present and no live session holds the
pane.

`phux agent explain` on a pane shows the evidence behind the state you
are looking at. `phux agent explain --file` tests rules against a saved
screen and does not talk to a server.
