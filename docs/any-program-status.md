# Any program can report status

A program running in a phux pane can say what it is doing by writing one
escape sequence to its terminal. phux records that and shows it in the
sidebar, the agents view, and `phux agent list`. The program needs no phux
library, no socket, and no hook.

The sequence is OSC 7501, from revision 0.4 of the
[Program Status Protocol](https://www.superlogical.com/rex/docs/build/program-status).
Mitchell Hashimoto [introduced OSC 7501](https://mitchellh.com/writing/program-status-osc7501).
This page is how phux applies it. The rules, the keys, and the limits are in
the [program protocol](./program-protocol.md).

```sh
printf '\033]7501;state=blocked:kind=permission:app=mytool\033\\'
```

`ESC ] 7501 ;` starts the report and `ESC \` ends it (`BEL`, byte `0x07`,
also ends it). The body is `key=value` pairs separated by `:`.

| Key | Here | Meaning |
| --- | --- | --- |
| `state` | `blocked` | Required. One of `idle`, `working`, `done`, `blocked`, `error`, `clear`. |
| `kind` | `permission` | With `blocked` only: `permission`, `question`, or `auth`. |
| `app` | `mytool` | A stable program name, 1 to 32 of `[A-Za-z0-9_.+-]`. |
| `title` | `ZGVwbG95` | Base64 of `deploy`. The agent name. Optional. |

Send `title` too. phux uses it as the pane's agent name and `app` as its kind.
Without `title` the name equals the kind, and `%name` can't address the pane.
`title` is base64: `printf deploy | base64`.

`clear` removes records; it is not a state phux displays. Other keys
(`id`, `progress`, `msg`) are in the
[protocol](./program-protocol.md#keys). A missing or unknown `state`
discards the report. Unknown keys are ignored.

## A shell function

Write to `/dev/tty` when standard output may be redirected. `kind` is
omitted unless the program is blocked. `title`, the fourth argument, is
base64, the same way
[`report-status.sh`](../examples/agents/report-status.sh) appends it, and
is omitted when absent:

```sh
report() {
  state=$1
  kind=$2
  app=${3:-mytool}
  title=${4:-}
  if [ -n "$kind" ]; then
    body=$(printf 'state=%s:kind=%s:app=%s' "$state" "$kind" "$app")
  else
    body=$(printf 'state=%s:app=%s' "$state" "$app")
  fi
  if [ -n "$title" ]; then
    body=$(printf '%s:title=%s' "$body" "$title")
  fi
  printf '\033]7501;%s\033\\' "$body" > /dev/tty
}

report working "" mytool ZGVwbG95
report blocked permission mytool ZGVwbG95
report "done" "" mytool ZGVwbG95
```

[`examples/agents/report-status.sh`](../examples/agents/report-status.sh)
is that function as a script. With no arguments it reports `working`, then
`blocked`, waits, then `done`. Each of those reports also sends `title`,
the base64 of `deploy`:

```sh
printf '\033]7501;state=blocked:kind=permission:app=mytool:title=ZGVwbG95\033\\'
```

A report replaces its record, so every report in the sequence carries the
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
ESC ] 7501 ; ?:states=idle,working,done,blocked,error:kinds=permission,question,auth ESC \
```

A program that does not understand a pair in the reply ignores it.

Detection is optional: a terminal drops an OSC it does not know, so a
program may send reports without asking. The
[protocol](./program-protocol.md#feature-detection) has the rest, including
the device-attributes check.

## Wrappers

None of the agent integrations in this repository write OSC 7501. Claude
Code, pi, and OpenCode report through a live agent session, which outranks
OSC 7501, so a report won't replace their state. To report from a script or
a hook you own, run `report-status.sh`. A Codex pane with nothing stronger
is read from the screen rules.

## What a run shows

`phux agent list`

```text
@1	mytool	%deploy	blocked	1.00	the phux.agent/v1 record for this pane (ADR-0040); no detection manifest is loaded for `mytool`, so there is no rule evidence to show
```

`phux agent explain @1`

```text
@1	mytool	%deploy	blocked	1.00	the phux.agent/v1 record for this pane (ADR-0040); no detection manifest is loaded for `mytool`, so there is no rule evidence to show
  - agent_record 1.00: phux.agent/v1 record published for this pane ({"name":"deploy","kind":"mytool","state":"blocked"})
```

The address is `%deploy`: the name comes from `title`, the kind from `app`.

## How phux ranks a report

Agent state is evidence-ranked; screen is the floor.

From strongest to weakest, as specified in
[precedence](./program-sequences.md#precedence):

1. A state declared with `phux agent set --state`. phux leaves it until it
   is cleared.
2. A live agent session fed by `phux agent emit`.
3. OSC 7501 records on the pane.
4. Hook reports from `phux agent report-state`.
5. The foreground program and the screen, matched against built-in rules.

While a pane has any OSC 7501 record and no live session, the drain does
not publish a screen-derived state, and it drops the state a hook report
would have published. A declaration or a live session is left in place:
the OSC 7501 projection returns without writing over them. This is the
order the server is built to keep. It has not been measured on a live
Claude Code, Codex, OpenCode, or pi session.

An agent with no integration falls back to the built-in screen rules.
Rules ship for Claude Code, Codex, OpenCode, pi, Oh My Pi, Grok, Amp,
Cursor, Gemini CLI, Goose, and Aider. A screen that matches no rule reads
as `idle`, never `blocked`.

`phux agent explain` on a pane shows the evidence behind the state you
are looking at. `phux agent explain --file` tests rules against a saved
screen and does not talk to a server.
