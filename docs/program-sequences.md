# Program sequences and agent reports

The second half of the [program protocol](./program-protocol.md). It
covers the sequences other than OSC 7501 that phux acts on when a program
in a pane writes them, the pane's agent record, and the `phux` commands a
program can run to report state instead of writing escapes.

The key words MUST, MUST NOT, SHOULD, and MAY are used as in RFC 2119 and
RFC 8174, and only when they appear in capitals. `ESC`, `OSC`, and `ST`
mean what they mean on the [first page](./program-protocol.md#conventions).

phux renders everything a program writes. This page lists only what phux
also acts on. A sequence missing here, such as OSC 8 hyperlinks, is
rendered and does nothing more.

## Titles (OSC 0 and 2)

`OSC 0 ; text ST` and `OSC 2 ; text ST` set the pane's title. phux reads
the title after each chunk of output, and on a change:

- sends a `title_changed` event to `phux watch` and the MCP tools,
- matches it against [agent rules](./agents.md#agent-states) that look at
  the title,
- checks it for a [`phux-ask` question](#questions-the-phux-ask-title).

An empty title means no title. phux sets no length limit; the TUI cuts the
title to fit where it shows it. OSC 1 (icon name) has no effect.

A tab's label prefers, in order: a name the user gave the tab, the pane's
agent name, the program's title, the folder's name, and the tab's number. A
program's title never replaces a name the user set.

Programs cannot read the title back: `CSI 21 t` is not answered.

## Questions: the phux-ask title

A program that needs a person to answer a question sets its title to a
`phux-ask` marker:

```
title       := "phux-ask" ["[" id "]"] ":" question ["?s=" suggestions]
id          := any characters except "]"
suggestions := answer ("|" answer)*
```

- The question is everything after the `:`, up to the first `?s=`.
- Suggestions are split on `|`; empty ones are dropped. They are shown in
  order.
- `phux-ask` without a `:` is not a question.
- Setting any other title withdraws the question.

```sh
printf '\033]2;phux-ask[deploy-7]:Deploy v2.4.1 to production??s=yes|no\033\\'
```

phux raises an `asked` event carrying the id, the question, and the
suggestions, marks the pane as needing attention, and notifies the user.
`phux agent answer --id deploy-7 --choice 1 @7` pastes the first suggestion
and presses Enter, so the program reads the answer as one line on standard
input. A question without an id is shown, but `phux agent answer` refuses
it. A program SHOULD give each question a new id.

`phux ask` reports the same question without touching the title, and
outranks a `phux-ask` title while it holds. phux has no command that
withdraws a `phux ask` question; it lasts until the pane closes, so a
program that can change its title SHOULD prefer the title.

## Shell integration (OSC 133)

phux reads the semantic prompt marks a shell emits:

| Mark | phux does |
| --- | --- |
| `OSC 133 ; A ST` | Pane is at a prompt. Ends the pane's `working` and `blocked` [program status](./program-protocol.md#states) records. |
| `OSC 133 ; B ST` | Pane is at a prompt, reading input. |
| `OSC 133 ; C ST` | A command started: a `command_started` event. |
| `OSC 133 ; D [; code] ST` | A command finished: a `command_finished` event with the exit code, and the status bar's `exit` widget shows it. phux also rechecks the pane's folder. |

Extra parameters such as `aid=` are allowed and ignored. A `D` without a
code clears the last code. `phux wait --output-only`, `phux snapshot --cells`, and the
`command_started` and `command_finished` events of `phux watch` rely on
these marks; see [scripting](./scripting.md).

## Progress (OSC 9;4)

`OSC 9 ; 4 ; state ; percent ST`, with state `0` remove, `1` set, `2`
error, `3` indeterminate, `4` paused. phux keeps the last one per pane only
for agent rules that match the `osc-progress` region. It shows no progress
bar and does not turn progress into program status. A full reset sets it
back to `0`.

## Clipboard (OSC 52)

`OSC 52 ; selection ; base64 ST` asks to set the clipboard. OSC 1337 `Copy`
and kitty's OSC 5522 writes are treated the same way.

- Every selection (`c`, `p`, `s`, and the rest) means the clipboard of the
  user's machine.
- Only a write from the pane the user's TUI has focused, from live output,
  of UTF-8 `text/plain` at most 1 MiB decoded, is used. phux drops a write
  replayed from history, a clear, and anything else. Only the last write in
  one chunk of output counts.
- The user's `defaults.clipboard-write` decides: `allow` (the default) sets
  the clipboard, `ask` shows the size and asks first, `deny` drops it.
- phux passes the text on to the user's terminal as OSC 52, which may have
  its own policy.
- **Clipboard reads are never answered.** An `OSC 52 ; c ; ? ST` query gets
  no reply, so a program MUST NOT wait for one without a timeout.

What clients other than the TUI do with a clipboard write is not defined
here.

## Working folder (OSC 7)

phux does not read OSC 7. It asks the operating system for the folder of
the pane's process (on Linux and macOS), when a command finishes and when
output goes quiet. A program cannot set the pane's folder by escape.

## Notifications and the bell

phux ignores OSC 9 (other than `9;4`), OSC 777, and OSC 99 from a pane: a
program cannot raise a desktop notification through phux. Report state
instead, with [OSC 7501](./program-protocol.md) or a
[`phux-ask` question](#questions-the-phux-ask-title), and phux notifies the
user according to their `[notifications]` settings.

A BEL (`0x07`) outside a control string is a `bell` event in `phux watch`
and the MCP tools, one per chunk of output. It does not ring the user's
terminal, and does not mark the pane as needing attention. A BEL that ends
an OSC is a terminator, not a bell.

## Replies phux sends

phux answers these queries itself, without asking the user's terminal:

| Query | Reply |
| --- | --- |
| `OSC 7501 ; ? ST` | [Feature detection](./program-protocol.md#feature-detection) |
| `CSI c` (primary device attributes) | `CSI ? ... c` |
| `CSI > q` (XTVERSION) | `libghostty`. Do not detect phux by this. |
| `OSC 10 ; ? ST`, `OSC 11 ; ? ST` | The default colours of the user's terminal, as phux saw them at attach |
| `CSI 14 t`, `CSI 16 t`, `CSI 18 t` | The pane's size, from a cell size of 8 by 16 pixels when the real one is unknown |

Not answered: `OSC 52` reads, `CSI 21 t`, ENQ, and `CSI ? 996 n`.

## The agent record

Every pane can carry an agent record, `phux.agent/v1`, a JSON object:

| Field | Value |
| --- | --- |
| `name` | Required. A record without a non-blank `name` counts as no record. |
| `kind` | A slug such as `claude` or `terraform`. |
| `state` | `unknown` \| `idle` \| `working` \| `blocked` \| `done`. An unknown word reads as `unknown`. |
| `attention` | `none` \| `low` \| `normal` \| `high`. An unknown word reads as `normal`. Absent means derived: `blocked` high, `working` normal, `done` and `unknown` low, `idle` none. |
| `session` | A free-form label, such as a job or fleet name. |

The record is what `phux agent list`, `phux agent wait`, the sidebar, and
notifications read. phux writes it from the best evidence it has; a program
can write it directly with `phux agent set`.

### Precedence

From strongest to weakest:

1. A record declared with a state, through `phux agent set --state` or the
   MCP `phux_agent_set` tool. phux leaves it alone until it is cleared.
2. A live agent session fed by `phux agent emit`.
3. [OSC 7501 program status](./program-protocol.md#how-phux-shows-program-status).
4. Hook reports from `phux agent report-state`.
5. The foreground program and the screen, matched against agent rules.

While a pane has any OSC 7501 record and no live session, phux ignores
levels 4 and 5 for it.

### Commands a program can run

A program in a pane SHOULD name its own pane, `@$PHUX_TERMINAL_ID`.
Leaving the target out means the focused pane of the focused session,
which is often someone else's.

```sh
phux agent set --name deploy --kind terraform --state working "@$PHUX_TERMINAL_ID"
phux agent set --name deploy --kind terraform --state blocked --attention high "@$PHUX_TERMINAL_ID"
phux agent clear "@$PHUX_TERMINAL_ID"
phux agent show "@$PHUX_TERMINAL_ID"
```

- `phux agent set` replaces the whole record; nothing is merged. Without
  `--state` the record says `unknown`, and phux may fill in a state from
  weaker evidence. A declared state stays until `phux agent clear` or the
  pane closes, even after the program exits, so a program that declares a
  state MUST clear it before it exits. `set` and `clear` exit 0 even when the
  write does not land or cannot be confirmed (they print an error);
  confirm with `phux agent show`.
- `phux agent report-state @7 working` (also `blocked`, `done`) is for hooks.
- `phux agent session open`, then `phux agent emit --type prompt`,
  `tool_start`, `ask`, or `stop`, feeds an agent session. See
  [agents](./agents.md#agents-without-an-integration).
- `phux ask @7 --id ID --suggest yes --suggest no "Question?"` raises a
  question like the [`phux-ask` title](#questions-the-phux-ask-title).

**`report-state` and `emit` change the agent record only when phux
recognized the program** as one of the agents its rules know. For any other
program, use OSC 7501 or `phux agent set`.

Only connections on the local socket can feed an agent session; a remote
client cannot. Any client that can write the pane's metadata can write the
agent record, so the record says what a program claims, not who it is.

## Relationship to standard sequences

| Sequence | In phux |
| --- | --- |
| OSC 0, 2 | Title, `phux-ask` questions, agent rules |
| OSC 1 | No effect |
| OSC 7 | Not read |
| OSC 9 (notification) | Ignored |
| OSC 9;4 | Agent rules only |
| OSC 52 | Clipboard write, by user policy; reads not answered |
| OSC 99, OSC 777 | Ignored |
| OSC 133 | Prompt state, command events, exit codes; `A` ends active OSC 7501 records |
| OSC 7501 | [Program status](./program-protocol.md#program-status-osc-7501) |
| BEL | `bell` event |

## Revision history

| Date | Revision | Changes |
| --- | --- | --- |
| 2026-10-21 | 1 | First version. |
