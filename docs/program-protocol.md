# Program protocol

This page is for people who write programs that run inside a phux pane:
build tools, deploy scripts, coding agents. It specifies how such a program
tells phux what it is doing, and what phux does with that.

The main channel is the Program Status Protocol, OSC 7501. A program writes
one escape sequence to its terminal, and phux records it, ranks it, and
shows it as the pane's state in the sidebar, the agents view, notifications,
and `phux agent list`. It needs no phux library and no socket.

```sh
printf '\033]7501;state=working:app=deploy\033\\'
```

[Program sequences and agent reports](./program-sequences.md) covers the
other sequences phux reads (titles, the `phux-ask` title, OSC 133, OSC 9;4,
OSC 52, BEL), the agent record, and the commands a program can run instead
of writing escapes.

## Conventions

The key words MUST, MUST NOT, SHOULD, and MAY are used as in RFC 2119 and
RFC 8174, and only when they appear in capitals. Where this page says
"phux does", it describes the code on `main`. Where behaviour is not
defined, the page says so. Do not depend on it.

| Name | Bytes |
| --- | --- |
| `ESC` | `0x1B` |
| `OSC` | `ESC ]`, that is `0x1B 0x5D` |
| `ST` | `ESC \`, that is `0x1B 0x5C`, or `BEL`, that is `0x07` |

A **pane** is one phux terminal and its pseudo-terminal. A **program** is
anything that writes to that pseudo-terminal. A **report** is one OSC 7501
sequence. A **record** is what phux stores as a result of a report.

## What a program can tell phux

| Channel | What it carries | Specified in |
| --- | --- | --- |
| OSC 7501 | State, what the program waits on, progress, a label, a message | [Program status](#program-status-osc-7501) |
| OSC 0 and 2 | The pane's title | [Titles](./program-sequences.md#titles-osc-0-and-2) |
| `phux-ask` title | A question for a person, with suggested answers | [Questions](./program-sequences.md#questions-the-phux-ask-title) |
| OSC 133 | Prompt and command boundaries, exit codes | [Shell integration](./program-sequences.md#shell-integration-osc-133) |
| OSC 9;4 | Progress, for agent detection rules | [Progress](./program-sequences.md#progress-osc-94) |
| OSC 52 | A clipboard write | [Clipboard](./program-sequences.md#clipboard-osc-52) |
| `phux agent ...` | The pane's agent record, over the socket | [Agent record](./program-sequences.md#the-agent-record) |

## Detecting phux

phux sets two variables in every local pane's environment:

| Variable | Value |
| --- | --- |
| `PHUX_TERMINAL_ID` | The pane's number: `7` for pane `@7`. |
| `PHUX_SOCKET` | The socket of the server that owns the pane. |

A program that finds both is running in a phux pane and MAY call the `phux`
command line with target `@$PHUX_TERMINAL_ID`. phux does not set
`TERM_PROGRAM`, `PHUX`, or `PHUX_PANE`. `TERM` is `xterm-256color` unless
the user configures `defaults.term`.

A program that only writes OSC 7501 needs none of this; it can use
[feature detection](#feature-detection) or send reports without asking.

## Program status (OSC 7501)

phux implements revision 0.4 of the
[Program Status Protocol](https://www.superlogical.com/rex/docs/build/program-status),
a terminal-neutral specification. The parser is libghostty's. This section
restates the protocol as phux applies it, including where phux differs.

### Syntax

```
OSC 7501 ; pairs ST        report
OSC 7501 ; ? ST            feature detection

pairs    := pair (":" pair)*
pair     := key "=" value
key      := [a-z]+
value    := [A-Za-z0-9_.,+/=-]*
```

- The body is `key=value` pairs separated by `:`. Whitespace around a key
  or value is removed. Keys are case-sensitive: `State=idle` is an unknown
  key, and `state=IDLE` is an unknown state.
- A malformed pair (no `=`, or a byte outside the value set) is skipped,
  and the rest of the report still applies.
- Unknown keys are ignored. If a key repeats, the last value wins.
- `title` and `msg` are standard base64 of UTF-8 text. Padding is optional.
  The decoded text MUST NOT contain a control character (`U+0000` to
  `U+001F`, `U+007F`, `U+0080` to `U+009F`), so no newline and no tab.
- A report is discarded whole when it breaks a [limit](#limits), has a
  missing or unknown `state`, an invalid `id`, base64 that does not decode,
  or decoded text that is not UTF-8 or holds a control character. Nothing
  from a discarded report is applied, and phux sends no error back.
- CAN (`0x18`) or SUB (`0x1A`) inside the sequence cancels it.

### Records and ids

A pane holds a set of records, one per id.

```
id      := segment ("/" segment)*
segment := [A-Za-z0-9_.+-]{1,32}
```

- A report with no `id` addresses the **root record**. A program that only
  reports its own state uses the root record and can skip the rest of this
  section.
- **Each report replaces its record completely.** A key missing from the
  report is missing from the record afterwards, so a program that wants
  `app` or `title` on a record sends them in every report.
- An `id` made of value-set bytes that breaks the grammar (`id=`, `id=/a`,
  `id=a//b`, `id=a,b`, a segment over 32 bytes, more than 8 segments)
  discards the report.
- **An `id` containing a byte outside the value set is skipped as a
  malformed pair**, so the report addresses the root record. With
  `state=clear` that removes every record. This differs from the
  specification, which ignores such a report. Programs MUST send ids that
  match the grammar.
- `/` makes `build/test` a child of `build`, and `build` a child of the
  root. A parent does not have to exist. The relationship is used for
  clearing and for `app` inheritance, and for nothing else.
- Records belong to the pane, not to a screen. Switching to or from the
  alternate screen and a soft reset (`DECSTR`) leave them alone. A full
  reset (`RIS`, `ESC c`) removes every record.
- Records survive `phux upgrade`: the new server replays them in their
  original order.

### States

`state` is required.

| `state` | Meaning | phux keeps it until |
| --- | --- | --- |
| `idle` | At rest, waiting for the user's next instruction. | Replaced, cleared, or reset. |
| `working` | Running. May carry `progress`. | Replaced, cleared, reset, the pane's process exits, or a shell prompt starts. |
| `done` | Finished; the result is ready to look at. | Replaced, cleared, or reset. |
| `blocked` | Cannot continue until the user acts. `kind` says what, `msg` says why. May carry `progress`. | Same as `working`. |
| `error` | Failed and stopped. | Same as `done`. |
| `clear` | Not a state. Removes the addressed record and every record beneath it; with no `id`, every record. | |

A shell prompt starting means OSC 133 `A`. There is no heartbeat: a record
never expires because time passed. phux keeps `idle` across a prompt and a
process exit; the specification allows either. phux never drops `done` or
`error` on its own, not even when the user looks at the pane.

When the user interrupts or cancels the program, report `idle`. A program
that exits as soon as it finishes SHOULD report `done` or `error` first,
so the user finds a record afterwards.

### Keys

| Key | Value | Notes |
| --- | --- | --- |
| `state` | `idle` \| `working` \| `done` \| `blocked` \| `error` \| `clear` | Required. |
| `id` | path, see [Records and ids](#records-and-ids) | Absent means the root record. |
| `kind` | `permission` \| `question` \| `auth` | With `blocked` only, ignored otherwise. An unknown value is treated as absent. |
| `progress` | integer `0` to `100` | With `working` or `blocked` only. Leading zeros are allowed. Anything else, including `101`, `-1`, and `4.5`, is treated as absent. Absent means indeterminate. |
| `app` | `[A-Za-z0-9_.+-]{1,32}` | A stable program identifier such as `cargo` or `terraform`. A value outside the set is treated as absent. A record without `app` takes it from its nearest ancestor that has one, when read. |
| `title` | base64 UTF-8 | A short label. phux uses the active record's title as the pane's agent name. Empty is absent. |
| `msg` | base64 UTF-8 | One line: what the record is doing, waiting for, or finished. phux shows it but reads no meaning into it. Empty is absent. |

### Limits

Limits count bytes, not characters. A report that breaks any of them is
discarded whole, even when a later pair with the same key would replace
the offending one.

| Item | Limit |
| --- | --- |
| Whole sequence, `OSC` through `ST` | 4096 bytes when ended with `ESC \`; 4095 with `BEL` |
| Key | 16 bytes, including unknown keys |
| `msg` | 2732 bytes encoded, 2048 decoded |
| `title` | 256 bytes encoded, 192 decoded |
| `app` | 32 bytes |
| `id` | 128 bytes, 32 per segment, 8 segments |
| Records per pane | 256. A report that would create the 257th removes the least recently updated record. |

### Feature detection

```
program → OSC 7501 ; ? ST
phux    → OSC 7501 ; ?:states=idle,working,done,blocked,error:kinds=permission,question,auth ST
```

phux answers the query itself, with the terminator the query used, and in
byte order with its other replies. A program that sends primary device
attributes (`CSI c`) right after the query gets the OSC 7501 reply first;
if the DA reply comes first, the terminal does not support the protocol.
A query creates no record.

Detection is optional. Terminals ignore unknown OSCs, so a program MAY send
reports without asking. A program MUST ignore pairs in the reply that it
does not understand.

### How phux shows program status

phux ranks a pane's records by state, `blocked` first, then `error`,
`done`, `working`, `idle`, and by most recent update within a state. The
first is the **active record**. It becomes the pane's agent record, which
drives the sidebar, the agents view, `phux agent list`, `phux agent wait`,
and notifications:

| Agent record field | Comes from |
| --- | --- |
| `name` | The active record's `title`, with invisible formatting removed; else its `app`; else `Program`. |
| `kind` | The active record's `app`, else `program`. |
| `state` | The active record's `state`, except that `error` becomes `done`. |
| `attention` | `high` for `error`. Otherwise derived from `state`: `blocked` high, `working` normal, `done` low, `idle` none. |

phux applies this only while nothing stronger holds the pane. A record
someone declared with `phux agent set --state`, and a live agent session,
both outrank OSC 7501. OSC 7501 outranks everything else: while a pane has
any record, phux does not read its screen for agent rules and ignores
`phux agent report-state` for it. [Precedence](./program-sequences.md#precedence)
has the whole order. When the last record goes, the projected state is
removed, or set to `unknown` if someone declared the pane's name or kind.

phux does not yet show `msg`, `kind`, `progress`, or records other than the
active one in its chrome. Read all of them with:

```sh
phux resource show @7           # program_status: the active record and every id
phux resource show --json @7
```

Records are stored as server-owned metadata on the pane:
`phux.program-status/v1` holds `{"record_ids": [...], "active": {...}}`,
and `phux.program-status.record/v1/<id>` holds each record (the root's id
is the empty string). A record is `{"id", "state", "kind", "progress",
"app", "title", "msg"}`, with absent keys omitted and `app` resolved
through inheritance. No client can write either key.

### Security

Everything in a report is untrusted input from a program that already
controls the pane.

- Any process that can write to the pane's terminal can send reports, and
  phux cannot tell which one did. Records describe the pane, not a process.
- phux refuses text with control characters, never treats `title` or `msg`
  as markup, and removes text direction overrides and other invisible
  formatting characters from the agent name and from human
  `phux resource show` output. The stored record and `--json` keep the
  text as sent.
- The only bytes phux writes back are the feature detection reply, which
  contains nothing from any report. There is no sequence for reading
  records back.
- Everything phux shows from a record names the pane it came from.
- Notifications for agent state are batched within one second and sent at
  most once every ten seconds per pane and kind, however fast a program
  changes state.
- Any client that can read the pane's metadata can read its records,
  including remote clients the server admits. Do not put secrets in `msg`
  or `title`.

### Examples

Shell, with base64 values decoded in a trailing comment. Write to
`/dev/tty` when standard output may be redirected.

```sh
# One program, root record
printf '\033]7501;state=working:app=brew:msg=SW5zdGFsbGluZyB1cGRhdGVz\033\\'   # Installing updates
printf '\033]7501;state=blocked:kind=auth:app=brew:msg=UGFzc3dvcmQgcmVxdWlyZWQ=\033\\'   # Password required
printf '\033]7501;state=done:app=brew:msg=VXBncmFkZWQgMTIgcGFja2FnZXM=\033\\'   # Upgraded 12 packages

# Several records, then clear one subtree and the rest
printf '\033]7501;state=working:app=deploy\033\\'
printf '\033]7501;state=working:id=us-east:progress=40\033\\'
printf '\033]7501;state=blocked:kind=permission:id=eu-west:msg=QXBwcm92ZSBwcm9kdWN0aW9uPw==\033\\'   # Approve production?
printf '\033]7501;state=clear:id=eu-west\033\\'
printf '\033]7501;state=clear\033\\'
```

A function that encodes the message:

```sh
status() {
  printf '\033]7501;state=%s:app=sync:msg=%s\033\\' "$1" \
    "$(printf '%s' "$2" | base64 | tr -d '\n')" > /dev/tty
}
status working "Syncing photos"
rsync -a ~/Photos backup:/photos && status done "Photos synced" || status error "rsync failed"
```

Ask for support, then fall back after a short wait:

```sh
printf '\033]7501;?\033\\\033[c' > /dev/tty
```

Read the reply from the terminal in raw mode; an answer starting
`ESC ] 7501 ; ?` means supported.

### Relationship to other sequences

- **OSC 9;4 (progress).** phux reads it for agent detection rules but does
  not turn it into OSC 7501 records, so it never overwrites a real report.
  A program MAY send both.
- **OSC 9, OSC 777, OSC 99 (desktop notifications).** phux ignores these
  from a pane. It sends its own notifications to your terminal when a
  pane's state changes, so a program reports state and lets phux decide.
- **OSC 133 (shell integration).** Emitted by the shell. OSC 133 `A` ends a
  pane's `working` and `blocked` records.
- **OSC 0 and 2 (title).** Free-form. An OSC 7501 `title` labels a record
  without touching the pane's title.

## Revision history

| Date | Revision | Changes |
| --- | --- | --- |
| 2026-10-21 | 1 | First version. Describes phux's implementation of Program Status Protocol 0.4. |
