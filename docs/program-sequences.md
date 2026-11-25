---
group: build
order: 2
---

# Program sequences and agent reports

The sequences other than [OSC 7501](./program-protocol.md) that phux acts
on, the pane's agent record, and the `phux` commands a program can run
instead of writing escapes.

Terms are as in [Conventions](./program-protocol.md#conventions). A
sequence not listed here is rendered and nothing more.

## Titles (OSC 0 and 2)

`OSC 0 ; text ST` and `OSC 2 ; text ST` set the pane's title. On a change
phux sends `title_changed` to `phux watch` and MCP, runs title
[agent rules](./agents.md#agent-states), and checks for a
[`phux-ask` question](#questions-the-phux-ask-title). Empty means no title;
there is no length limit. A name the user gave a tab always wins over the
title. OSC 1 does nothing, and `CSI 21 t` is not answered.

## Questions: the phux-ask title

```
title       := "phux-ask" ["[" id "]"] ":" question ["?s=" suggestions]
id          := any characters except "]"
suggestions := answer ("|" answer)*
```

- The question runs from the `:` to the first `?s=`. Suggestions split on
  `|`; empty ones are dropped; order is kept.
- `phux-ask` without `:` is not a question. Any other title withdraws it.

```sh
printf '\033]2;phux-ask[deploy-7]:Deploy v2.4.1 to production??s=yes|no\033\\'
```

phux raises an `asked` event and notifies. `phux agent answer --id
deploy-7 --choice 1 @7` pastes the answer plus Enter; it refuses a question
without an id. A program SHOULD use a new id per question. `phux ask` raises
the same question without a title, outranks it, and lasts until `phux agent
answer` answers it or the pane closes, so a program that can set its title
SHOULD prefer the title.

## Shell integration (OSC 133)

| Mark | phux does |
| --- | --- |
| `OSC 133 ; A ST` | At a prompt. Ends `working` and `blocked` [program status](./program-protocol.md#states) records. |
| `OSC 133 ; B ST` | At a prompt, reading input. |
| `OSC 133 ; C ST` | `command_started` event. |
| `OSC 133 ; D [; code] ST` | `command_finished` with the exit code (shown by the `exit` widget); rechecks the pane's folder. No code clears the last one. |

Extra parameters (`aid=`) are ignored. `phux wait --output-only`,
`phux snapshot --cells`, and the command events need these marks. Load them
with `phux config shell-integration SHELL`, or set
`defaults.shell-integration = true` to inject them into every new pane's
default shell ([config reference](./reference/config.md)). bash before 4.4
marks prompts and exit codes but not command starts.

## Progress (OSC 9;4)

`OSC 9 ; 4 ; state ; percent ST` (state `0` remove, `1` set, `2` error, `3`
indeterminate, `4` paused). phux keeps the last one per pane only for agent
rules matching the `osc-progress` region. A full reset sets it to `0`.

## Clipboard (OSC 52)

**Writes.** `OSC 52 ; selection ; base64 ST`, OSC 1337 `Copy`, and OSC 5522
writes set the user's clipboard, whatever the selection letter. Only the
last write per output chunk counts, and only from live output of the
focused pane, as UTF-8 `text/plain` up to 1 MiB decoded; replays and clears
are dropped. `defaults.clipboard-write` is `allow`, `ask`, or `deny`. The
TUI forwards the write as OSC 52 to the user's terminal; Rune sets its own
clipboard.

**Reads.** `OSC 52 ; c ; ? ST` gets no reply by default, so a program MUST
NOT wait for one without a timeout.

`defaults.clipboard-read` is `deny`, `ask`, or `allow`. Only live output
of the focused pane in a focused window is considered, at most one prompt
per pane per 3 seconds. Only selection `c` (and `p` where a primary
selection exists) is answered. The controlling client sends
`CLIPBOARD_REPLY`; the server writes `OSC 52 ; c ; base64 ST` to the asking
pane only, from a client allowed to type there, and audits it as
`clipboard.read` without the text ([Security](./security.md)). Peers
without `CLIPBOARD_READ` never answer.

## Working folder (OSC 7)

Not read. phux asks the OS for the pane process's folder when a command
finishes and when output goes quiet.

## Pane icon (OSC 1337 SetUserVar)

```sh
printf '\033]1337;SetUserVar=phux_icon=%s\007' "$(printf '🦀' | base64)"
printf '\033]1337;SetUserVar=phux_icon=\007'   # remove
```

The value is base64 of 1 to 32 bytes of UTF-8 with no space, control
character, or direction override. Empty removes it; an invalid value or
another variable changes nothing. It lasts until changed or the server
restarts, is published as `icon` in `phux.pane-occupant/v1`, and wins over
[`[[icons]]`](./clients.md#pane-icons) config.

## Notifications and the bell

OSC 9 (other than `9;4`), OSC 777, and OSC 99 are ignored. With
`notifications.from-programs = "notes"`, OSC 9 and 777 become the pane's
[note](./scripting.md#notes). Report state instead and phux notifies per
`[notifications]`.

A BEL outside a control string is one `bell` event per output chunk. It
does not ring the user's terminal or mark the pane.

## Focus reports (DEC 1004)

After `CSI ? 1004 h`, a pane gets `CSI I` when it becomes the input pane
(never while off screen) and `CSI O` when it stops, including on detach and
window-focus changes.

## Replies phux sends

| Query | Reply |
| --- | --- |
| `OSC 7501 ; ? ST` | [Feature detection](./program-protocol.md#feature-detection) |
| `CSI c` | `CSI ? ... c` |
| `CSI > q` | `libghostty`. Do not detect phux by this. |
| `OSC 10 ; ? ST`, `OSC 11 ; ? ST` | The user's terminal colours as last seen |
| `CSI 14 t`, `CSI 16 t`, `CSI 18 t` | Pane size; cell size 8x16 when unknown |

Not answered: `CSI 21 t`, ENQ, `CSI ? 996 n`. OSC 52 reads follow
[Clipboard](#clipboard-osc-52).

## The agent record

`phux.agent/v1` is a JSON object on each pane:

| Field | Value |
| --- | --- |
| `name` | Required; blank means no record. A `name` equal to `kind` is made unique (`claude`, `claude-2`). |
| `kind` | A slug such as `claude`. |
| `state` | `unknown` \| `idle` \| `working` \| `blocked` \| `done`. Unknown words read `unknown`. |
| `attention` | `none` \| `low` \| `normal` \| `high`. Unknown reads `normal`. Absent: `blocked` high, `working` normal, `done` and `unknown` low, `idle` none. |
| `session` | Free-form label. |

phux writes it from the best evidence; `phux agent set` writes it
directly. It records what a program claims, not who it is.

### Precedence

1. A state declared by `phux agent set --state` or MCP `phux_agent_set`,
   kept until cleared.
2. A live agent session fed by `phux agent emit`.
3. [OSC 7501 program status](./program-protocol.md#how-phux-shows-program-status).
4. Hook reports from `phux agent report-state`.
5. The foreground program and screen, matched against agent rules.

While a pane has an OSC 7501 record and no live session, levels 4 and 5 are
ignored.

### Commands a program can run

Target your own pane, `@$PHUX_TERMINAL_ID`; no target means the focused
pane, often someone else's.

```sh
phux agent set --name deploy --kind terraform --state working "@$PHUX_TERMINAL_ID"
phux agent clear "@$PHUX_TERMINAL_ID"
```

- `set` replaces the whole record (last writer wins) but keeps `kind`
  when `--kind` is omitted. Without `--state`, weaker evidence may fill the
  state. A declared state outlives the program, so a program MUST clear it
  before exiting. `set` and `clear` read the record back and exit 1
  (`agent_record_unconfirmed`) when the write did not land.
- `phux agent report-state` is for hooks; `phux agent session open` plus
  `phux agent emit` feeds a session
  ([agents](./agents.md#agents-without-an-integration)); `phux ask` raises a
  question.
- **`report-state` and `emit` change the record only for programs phux's
  rules recognize.** Others use OSC 7501 or `phux agent set`.
- Only a local connection (this machine's socket or localhost) can feed an
  agent session; remote peers are refused.
