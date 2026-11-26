---
group: build
order: 1
title: Program Status Protocol (OSC 7501): report status from any program
description: phux supports OSC 7501, the escape sequence a program writes to report its status: working, blocked, done, or error.
---

# Program Status Protocol (OSC 7501): report status from any program

A program in a phux pane reports its status by writing one escape sequence
to its terminal: no library, socket, or hook.

```sh
printf '\033]7501;state=blocked:kind=permission:app=mytool:title=ZGVwbG95\033\\' > /dev/tty
```

`title` is base64 (`printf deploy | base64`). phux uses it as the pane's
agent name and `app` as its kind, so the pane above is `%deploy`, `blocked`,
in the sidebar, the agents view, and `phux agent list`. Without `title` the
name is the kind, made unique (`%mytool`, `%mytool-2`). Every report
replaces its record, so send the same keys each time.
See [`report-status.sh`](../examples/agents/report-status.sh) and
[`05-program-status.sh`](../examples/agents/05-program-status.sh).

Other sequences phux reads, the agent record, and the `phux agent` commands
a program can run instead are in
[Program sequences](./program-sequences.md).

## Conventions

The key words MUST, MUST NOT, SHOULD, and MAY are used as in RFC 2119 and
RFC 8174, and only when they appear in capitals. Behaviour not defined here
is undefined; do not depend on it.

| Name | Bytes |
| --- | --- |
| `ESC` | `0x1B` |
| `OSC` | `ESC ]`, that is `0x1B 0x5D` |
| `ST` | `ESC \`, that is `0x1B 0x5C`, or `BEL`, that is `0x07` |

A **pane** is one phux terminal and its pseudo-terminal. A **program** is
anything that writes to it. A **report** is one OSC 7501 sequence. A
**record** is what phux stores as a result.

## Detecting phux

phux sets `PHUX_TERMINAL_ID` (`7` for pane `@7`) and `PHUX_SOCKET` in every
local pane. A program that finds both MAY run `phux` with target
`@$PHUX_TERMINAL_ID`. phux does not set `TERM_PROGRAM`; `TERM` is
`defaults.term`, and `COLORTERM=truecolor` is always set. A program that
only writes OSC 7501 needs none of this.

## Program status (OSC 7501)

phux implements revision 0.4 of the
[Program Status Protocol](https://www.superlogical.com/rex/docs/build/program-status),
with libghostty's parser. This section states the protocol as phux applies
it, including where phux differs.

### Syntax

```
OSC 7501 ; pairs ST        report
OSC 7501 ; ? ST            feature detection

pairs    := pair (":" pair)*
pair     := key "=" value
key      := [a-z]+
value    := [A-Za-z0-9_.,+/=-]*
```

- Whitespace around a key or value is removed. Keys and values are
  case-sensitive: `State=idle` is an unknown key, `state=IDLE` an unknown
  state.
- A malformed pair (no `=`, or a byte outside the value set) is skipped and
  the rest applies, except a malformed `id`, which discards the report.
- Unknown keys are ignored. A repeated key: the last value wins.
- `title` and `msg` are standard base64 of UTF-8, padding optional. The
  decoded text MUST NOT contain a control character (`U+0000`-`U+001F`,
  `U+007F`, `U+0080`-`U+009F`).
- A report is discarded whole, silently, when it breaks a [limit](#limits),
  has a missing or unknown `state`, an invalid `id`, base64 that does not
  decode, or decoded text that is not UTF-8 or holds a control character.
- CAN (`0x18`) or SUB (`0x1A`) inside the sequence cancels it.

### Records and ids

A pane holds a set of records, one per id.

```
id      := segment ("/" segment)*
segment := [A-Za-z0-9_.+-]{1,32}
```

- No `id` addresses the **root record**.
- **Each report replaces its record completely.** A key missing from the
  report is missing from the record.
- An `id` that breaks the grammar (`id=`, `id=/a`, `id=a//b`, `id=a,b`,
  `id=a@b`, a segment over 32 bytes, more than 8 segments) discards the
  report, even when another `id` pair is valid. It never falls back to the
  root, so a `state=clear` with a bad id cannot remove every record.
- `build/test` is a child of `build`, `build` of the root. A parent need not
  exist. The tree is used only for clearing and `app` inheritance.
- Records belong to the pane, not a screen: alternate screen switches and
  `DECSTR` keep them; `RIS` (`ESC c`) removes them all. They survive
  `phux upgrade`, replayed in order.

### States

`state` is required.

| `state` | Meaning | Kept until |
| --- | --- | --- |
| `idle` | Waiting for the user's next instruction. | Replaced, cleared, or reset. |
| `working` | Running. May carry `progress`. | Replaced, cleared, reset, the pane's process exits, or a shell prompt starts (OSC 133 `A`). |
| `done` | Finished; the result is ready. | Replaced, cleared, or reset. |
| `blocked` | Needs the user. `kind` says what, `msg` why. May carry `progress`. | Same as `working`. |
| `error` | Failed and stopped. | Same as `done`. |
| `clear` | Not a state. Removes the addressed record and its descendants; with no `id`, every record. | |

Records never expire with time, and phux never drops `done` or `error` on
its own. After a user interrupt, report `idle`. A program that exits on
completion SHOULD report `done` or `error` first.

### Keys

| Key | Value | Notes |
| --- | --- | --- |
| `state` | `idle` \| `working` \| `done` \| `blocked` \| `error` \| `clear` | Required. |
| `id` | see [Records and ids](#records-and-ids) | Absent: the root record. |
| `kind` | `permission` \| `question` \| `auth` | `blocked` only. Unknown is absent. |
| `progress` | integer `0` to `100` | `working` or `blocked` only. Leading zeros allowed; anything else (`101`, `-1`, `4.5`) is absent. Absent is indeterminate. |
| `app` | `[A-Za-z0-9_.+-]{1,32}` | Stable program id such as `cargo`. Invalid is absent. A record without `app` inherits its nearest ancestor's when read. |
| `title` | base64 UTF-8 | Short label; the active record's is the pane's agent name. Empty is absent. |
| `msg` | base64 UTF-8 | One line of status. Shown, never interpreted. Empty is absent. |
| `resume` | `[A-Za-z0-9_.+,/=-]{1,256}`, not starting with `-` | phux extension; see [Resume](#resume). |

### Limits

Limits count bytes. A report that breaks one is discarded whole, even when a
later pair would replace the offending one.

| Item | Limit |
| --- | --- |
| Whole sequence, `OSC` through `ST` | 4096 with `ESC \`; 4095 with `BEL` |
| Key | 16, including unknown keys |
| `msg` | 2732 encoded, 2048 decoded |
| `title` | 256 encoded, 192 decoded |
| `app` | 32 |
| `id` | 128 total, 32 per segment, 8 segments |
| Records per pane | 256; the 257th evicts the least recently updated. |

### Feature detection

```
program → OSC 7501 ; ? ST
phux    → OSC 7501 ; ?:states=idle,working,done,blocked,error:kinds=permission,question,auth ST
```

phux replies itself, with the query's terminator, in byte order with its
other replies: a program that sends `CSI c` right after the query and gets
the DA reply first is on a terminal without the protocol. A query creates no
record. Detection is optional. A program MUST ignore reply pairs it does not
understand.

### How phux shows program status

Records rank by state (`blocked`, `error`, `done`, `working`, `idle`), then
most recent update. The first is the **active record**, projected onto the
pane's [agent record](./program-sequences.md#the-agent-record):

| Agent record field | From |
| --- | --- |
| `name` | Active `title` with invisible formatting removed, else `app`, else `Program`. |
| `kind` | Active `app`, else `program`. |
| `state` | Active `state`; `error` becomes `done`. |
| `attention` | `high` for `error`; otherwise derived from `state`. |

A declared `phux agent set --state` and a live agent session outrank OSC
7501; while a pane has any record, phux ignores its screen rules and
`phux agent report-state` ([Precedence](./program-sequences.md#precedence)).
When the last record goes, the projection is removed, or set to `unknown` if
the name or kind was declared.

phux chrome shows only the active record. `phux resource show [--json] @7`
shows every record. They are server-owned, client-unwritable metadata:
`phux.program-status/v1` is `{"record_ids": [...], "active": {...}}`, and
`phux.program-status.record/v1/<id>` (root id empty) is `{"id", "state",
"kind", "progress", "app", "title", "msg"}`, absent keys omitted, `app`
resolved.

### Resume (legacy)

`resume` is a legacy phux extension of OSC 7501. A foreground-job check
ignores the shell, a background job, and a grandchild, and OSC 133 `A` does
not clear a stored token. `cat`, `ssh`, or `less` can still plant one while
that program is the foreground job, and an empty `resume=` from the shell
can still withdraw it. Prefer [OSC 7577](#program-resume-osc-7577). `resume`
is an opaque token that lets `phux restore` relaunch the pane with a command
the user configured for that `app`:

```sh
printf '\033]7501;state=working:app=notes:resume=%s\033\\' "$id" > /dev/tty
```

```toml
[[restore.program]]
app = "notes"
command = ["notes", "${PHUX_RESUME_TOKEN}"]
```

- A token counts only when the same report carries a valid `app` and the
  foreground job, read at that moment, may set it. That job's process-group
  leader must be a direct child of the pane's shell: its parent is the pane
  process, and it is not the shell. A pane with no shell accepts a token
  only while its own process is foreground. A report that fails this check
  is ignored and the previous token kept, as is an invalid token. `resume=`
  withdraws the token; a report without `resume` changes nothing. Where
  those process queries have no answer (anything other than Linux and
  macOS), the check fails closed and a token is not recorded.
- phux reads `app` and `resume` apart from the record: the token is taken
  even from a discarded report, belongs to no `id`, and a pane holds one.
  While cat, ssh, or less is the foreground job, its output can still set
  the token. OSC 7577 is what will narrow that; it is not in this rule yet.
- phux forgets the token when the report withdraws it, when the job that
  set it exits (a stopped job is still alive and keeps it), and when the
  pane's process exits with a status while the server runs. OSC 133 `A`
  does not forget it, including when the prompt and the token arrive in
  one burst. A signal or an unknown pane exit keeps it, and a server
  shutdown does not record a forget. It survives server stop, upgrade,
  kill, and reboot when it was not forgotten.
- At relaunch, a matching entry runs `command` in the pane's recorded
  directory instead of the recorded command, with no shell: each element
  exactly `${PHUX_RESUME_TOKEN}` becomes the token. No entry: the pane
  starts as before. An agent integration's own resume takes precedence.
- Config load refuses an entry unless `command` starts with a program name
  and contains `${PHUX_RESUME_TOKEN}` as a whole element. Each `app` once.

### Security

Everything in a report is untrusted, and any process that can write to the
pane can send one; records describe the pane, not a process.

- Control characters are refused; `title` and `msg` are never markup.
  Invisible formatting is stripped from the agent name and human output;
  the stored record and `--json` keep the raw text.
- phux writes back only the feature-detection reply, which holds nothing
  from any report.
- Anyone who can read the pane's metadata, including admitted remote
  clients, can read records. Keep secrets out of `msg` and `title`.
- A `resume` token is not a secret: it is ordinary pane output, stored
  unprotected on disk, and passed in argv.
- A token is recorded only when the foreground job is a direct child of the
  pane shell, or when the pane's own process is that program (no shell).
  The shell in the foreground, a background job, and a grandchild are
  ignored, and OSC 133 `A` does not clear a stored token. cat, ssh, and
  less can still set it while they are the foreground job. When
  `program-resume` is on, a trusted OSC 7577 record is what a relaunch runs
  instead. A `[[restore.program]]` command MUST treat the token as
  untrusted (it may hold `/` and `..`): look it up among things it created,
  never open it as a path or evaluate it as code (`sh -c`). phux guarantees
  only its shape.

### Examples

```sh
printf '\033]7501;state=working:id=us-east:progress=40\033\\'
printf '\033]7501;state=blocked:kind=auth:app=brew:msg=UGFzc3dvcmQgcmVxdWlyZWQ=\033\\'  # "Password required"
printf '\033]7501;state=clear:id=us-east\033\\'   # one subtree
printf '\033]7501;state=clear\033\\'              # every record
```

### Relationship to other sequences

- **OSC 9;4** feeds agent rules only and never becomes a record. A program
  MAY send both.
- **OSC 9, 777, 99** are ignored by default; use
  [`phux notify`](./scripting.md#notes).
- **OSC 133 `A`** ends `working` and `blocked` records.
- **OSC 0 and 2** set the pane title; OSC 7501 `title` labels a record
  without touching it.

## Program resume (OSC 7577)

OSC 7577 is how a program reports the command that should bring it back after
a crash, `phux kill --server`, or a reboot. It does not replace a hot upgrade:
`phux upgrade` keeps the process. The report is a hint. phux stores it and
runs it only when every check below passes.

A report is `OSC 7577 ;` pairs `ST`. The body ends with `mac=`, a SHA-256 of
the bytes before that key, a colon, and the pane's `OSC7577_TOKEN`. phux checks
the MAC and `time` (within 60 seconds of its clock, and not earlier than the
last report it accepted for that pid) before it decodes anything else. A
failure there is ignored. A report that passes both and then breaks the
grammar is rejected, and that owner's record is removed.

phux sets `OSC7577_TOKEN` in the environment of the pane's first process when
`program-resume` is on, and nowhere else: not in metadata, `--json`, or logs.
Each pane mints its own token, and a new one each time that pane starts. The
token is carried across `phux upgrade` so a later report still verifies.

phux stores a record when the MAC, the time, and the grammar pass. The record
is trusted only when the reporter is a same-uid direct child of the pane's
shell. Any other parent is stored as untrusted. A record is cleared
only by `clear=1` or when its owning process exits. OSC 133 `A`, RIS, DECSTR,
and the alternate screen do not clear it.

```toml
[restore]
program-resume = true

[[restore.trust]]
command = "nvim"
```

`program-resume` is off by default. `[[restore.trust]]` has no built-in rules.
A record runs only when it is trusted, its first argument is listed, its
working directory still exists, it has not already been tried, and phux is not
running as root. The command is the reported argv with no shell, as the same
user. An untrusted record or a missing directory starts a shell instead.
`tried` is saved before the command starts, so a crash during resume does not
retry it. An agent integration's own resume is first, then a trusted OSC 7577
record, then a legacy `resume=` entry, then the shell.

`phux ls -l` and `phux restore --dry-run` show the command with controls and
bidirectional overrides escaped.

One correction to the spec's advice on command execution: phux passes the
reported argv directly, with no shell and without running shell startup files.
That is what keeps an argument such as `$(reboot)` as text.
