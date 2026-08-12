---
audience: contributors, agents
stability: scratch
last-reviewed: 2026-08-12
---

# OSC 9;4 capture: does Claude Code emit it, and does the title kill-switch suppress it?

**TL;DR.** Claude Code (captured at v2.1.227/v2.1.228) emits `ESC ] 9 ; 4 ;
<state> ; BEL` at exactly the same three points it flips its OSC 0 title:
`state=0` at idle/startup, `state=3` (indeterminate) when a turn starts,
`state=0` again when the turn ends. `CLAUDE_CODE_DISABLE_TERMINAL_TITLE=1`
suppresses the title (OSC 0) completely but does **not** suppress OSC 9;4 —
all three progress events still fire, byte-for-byte identical to the
title-enabled run. Both findings are **captured evidence**, not inference.
phux-w7z2.16 can rely on OSC 9;4 as a title-independent working/idle signal
for Claude Code; it cannot rely on it carrying a numeric percentage (the
progress field is always empty), and codex/opencode/pi remain unanswered
(attempted, inconclusive — see "What remains open").

## The question (verbatim from phux-w7z2.15)

1. Does the shipped Claude Code emit OSC 9;4 progress at all? What are the
   exact payloads on the working -> idle transition?
2. Does `CLAUDE_CODE_DISABLE_TERMINAL_TITLE=1` also suppress OSC 9;4? If it
   does, the OSC-progress region does not fill the hole `claude.toml`
   documents (the title-disabled install), and phux-w7z2.16's value case
   collapses from "the missing working backstop" to "one more corroborating
   signal."

Also worth capturing while the harness was set up (bonus, non-blocking):
whether codex, opencode, and pi emit 9;4.

## Method

Real raw-byte capture, not a screen fixture. `crates/phux-server/src/agent_detect/fixtures/claude/*.txt`
stores libghostty-vt's *rendered grid text*, which is the wrong shape for
this question — OSC 9;4 never paints a visible glyph, so a screen dump
cannot show it (and `ADR/0035-agent-asked-event.md` already records that
libghostty-vt does not surface OSC 9 / OSC 777 through its Rust API at
all, which is a separate, larger finding — see "Also found" below).

`phux rec` forwards raw PTY bytes per ADR-0013 and would also have worked,
but building the workspace was not on the critical path for a two-question
capture, so this note uses a minimal, purpose-built PTY harness instead
(`pty_capture.py`, in this directory): it opens a real pty with `pty.fork`,
execs the target CLI into it, sets a stable window size, and tees every
byte the child writes to a file for a fixed duration while injecting one
or two staged lines of input at fixed delays. A `select()`-timeout read
loop is required, not a blocking `os.read` — see "A bug that cost the
codex/opencode/pi answer" below.

Both Claude Code captures use the same prompt ("count slowly from 1 to
400, one number per line, nothing else") that `crates/phux-server/src/agent_detect/fixtures/claude/working.txt`
already uses, chosen because it produces a long, tool-free generation turn
(no permission dialog, no risk of the capture stalling on approval).

### One substitution, and exactly one

Claude Code's startup banner renders the signed-in account
("`<address>`'s Organization"). In both `.rawcap` files that address is
replaced with `user@redacted.example` — chosen to be **byte-identical in
length** to the original, so every column position, wrap point and
escape-sequence offset in the capture is preserved. Nothing else in either
file is altered.

This is hygiene, not secrecy: the same address is already the workspace
`authors` field in `Cargo.toml` and ships with every published crate. A
capture committed as evidence is simply not a place to accumulate further
copies of a maintainer's personal address, where nobody would think to look
for one and no tool would think to scrub it.

The substitution lands in an SGR-coloured banner line and touches no control
sequence. The evidence this note rests on is unaffected and can be re-counted
directly: each capture still contains exactly three `ESC ] 9 ; 4` events.

Anyone reproducing this will see their own account in that position; that is
expected, and it is not evidence of anything.

The environment this ran in is itself a nested Claude Code session, which
leaks `CLAUDE_CODE_*` env vars (`CLAUDE_CODE_CHILD_SESSION`,
`CLAUDE_CODE_SESSION_ID`, etc.) into any `claude` subprocess spawned from
it and visibly changes its startup banner ("Transcript saving is off —
inherited CLAUDE_CODE_CHILD_SESSION marker", a bypass-permissions banner).
The first capture attempt did not strip these and is not used as evidence
below (it agreed with the clean captures on the state values, so it never
misled the finding, but it is not reproducible from a plain shell and is
not committed). All captures actually cited here were run with those
`CLAUDE_CODE_*` vars unset via `env -u ...`.

Date: 2026-08-12. Claude Code version at capture time: v2.1.227 (server
observed via `CLAUDE_CODE_EXECPATH`)/v2.1.228 (`claude --version`) — see
"Version note" below for why both numbers appear. The existing detection
fixtures in `crates/phux-server/src/agent_detect/fixtures/claude/` were
captured against v2.1.207 (per `crates/phux-server/rules/claude.toml`),
so this capture is roughly 20 patch versions ahead of that manifest.

## Raw evidence

Two full captures are committed alongside this note:

- [`2026-08-12-osc-9-4-claude-code/claude-title-enabled.rawcap`](./2026-08-12-osc-9-4-claude-code/claude-title-enabled.rawcap)
  (10,186 bytes) — default env, `claude`, one "count to 400" turn.
- [`2026-08-12-osc-9-4-claude-code/claude-title-disabled.rawcap`](./2026-08-12-osc-9-4-claude-code/claude-title-disabled.rawcap)
  (11,687 bytes) — `CLAUDE_CODE_DISABLE_TERMINAL_TITLE=1`, otherwise
  identical.
- [`2026-08-12-osc-9-4-claude-code/pty_capture.py`](./2026-08-12-osc-9-4-claude-code/pty_capture.py)
  — the capture harness that produced them, for exact reproduction.

Both files are marked `-text -diff` in a local `.gitattributes` so they
stay byte-exact regardless of a contributor's line-ending settings.

### `claude-title-enabled.rawcap`: every OSC 9; occurrence

```
offset=150   ESC]9;4;0;BEL     (startup)
offset=4013  ESC]9;4;3;BEL     (turn starts)
offset=10007 ESC]9;4;0;BEL     (turn ends)
```

Hex context (10 bytes before, 70 after) for each, decimal offsets as
printed by the extraction script, `xxd`-style:

**Startup — `ESC]9;4;0;BEL` immediately follows the OSC 0 title, which is
the static "not busy" glyph `U+2733` (✳):**

```
00000000: 3368 1b5b 3f31 3030 3668 1b5d 303b e29c  3h.[?1006h.]0;..
00000010: b320 436c 6175 6465 2043 6f64 6507 1b5d  . Claude Code..]
00000020: 393b 343b 303b 071b 5b3f 3230 3236 681b  9;4;0;..[?2026h.
00000030: 5b48 0d1b 5b31 421b 5b33 383b 323b 3231  [H..[1B.[38;2;21
00000040: 353b 3131 393b 3837 6de2 95ad e294 80e2  5;119;87m.......
```

**Turn start — `ESC]9;4;3;BEL` immediately follows the OSC 0 title
flipping to the animated busy glyph `U+25D0` (◐):**

```
00000000: 3036 681b 5d30 3be2 9790 2043 6c61 7564  06h.]0;... Claud
00000010: 6520 436f 6465 071b 5d39 3b34 3b33 3b07  e Code..]9;4;3;.
00000020: 1b5b 3f32 3032 3668 1b5b 3f32 356c 1b5b  .[?2026h.[?25l.[
00000030: 480d 1b5b 3134 421b 5b34 383b 323b 3535  H..[14B.[48;2;55
00000040: 3b35 353b 3535 6d1b 5b33 383b 323b 3830  ;55;55m.[38;2;80
```

**Turn end — `ESC]9;4;0;BEL` immediately follows the OSC 0 title reverting
to the static `U+2733` prefix (with the title text now the task summary,
"Count from 1 to 400"):**

```
00000000: 5d30 3be2 9cb3 2043 6f75 6e74 2066 726f  ]0;... Count fro
00000010: 6d20 3120 746f 2034 3030 071b 5d39 3b34  m 1 to 400..]9;4
00000020: 3b30 3b07 1b5b 3f32 3032 3668 1b5b 3f32  ;0;..[?2026h.[?2
00000030: 356c 1b5b 480d 1b5b 3333 421b 5b33 383b  5l.[H..[33B.[38;
00000040: 323b 3135 333b 3135 333b 3135 336d e29c  2;153;153;153m..
```

Also present in this capture, once, near the end (not the subject of this
bead, noted because it is adjacent evidence a future OSC-progress region
should not collide with): `ESC]777;notify;Claude Code;Claude is waiting
for your input BEL` — Claude Code additionally emits an iTerm2/growl-style
OSC 777 desktop notification independent of both the title and OSC 9;4.

### `claude-title-disabled.rawcap`: every OSC 9; and OSC 0/2; occurrence

```
OSC 0; (title) count: 0
OSC 2; (title) count: 0
OSC 9; count: 3
  offset=130   ESC]9;4;0;BEL   (startup)
  offset=3664  ESC]9;4;3;BEL   (turn starts)
  offset=10909 ESC]9;4;0;BEL   (turn ends)
OSC 777; count: 1  (same notify body as above)
```

Zero title escapes of any kind — `CLAUDE_CODE_DISABLE_TERMINAL_TITLE=1`
does exactly what its name says, confirmed independently as part of this
same capture. The OSC 9;4 sequence is present, at the same three states,
in the same order, with the same (empty) progress field, as the
title-enabled run. This is the direct answer to question 2.

## Answers

### Q1 — does Claude Code emit OSC 9;4, and when? **CAPTURED. High confidence.**

Yes. Across two independent clean captures (and a third, contaminated-env
capture not committed, which agreed), Claude Code emits exactly three OSC
9;4 events per interactive turn, always terminated with BEL (never `ST` /
`ESC \` in any observed instance), always with an empty progress-value
field (`;;` — never a percentage):

| Transition | Payload | Co-emitted with |
|---|---|---|
| App startup / idle | `ESC]9;4;0;BEL` | OSC 0 title set to static `✳` prefix |
| Turn starts (agent begins working) | `ESC]9;4;3;BEL` | OSC 0 title flips to animated busy glyph |
| Turn ends (agent goes idle) | `ESC]9;4;0;BEL` | OSC 0 title reverts to static `✳` prefix |

State `3` is `INDETERMINATE` in the vocabulary `docs/spec/L1.md` already
defines for `ProgressState` (line ~328). State `0` is `REMOVE`. State `1`
(`DEFAULT`, i.e. a determinate percentage) was never observed — Claude
Code does not report fractional progress through this channel for a plain
chat turn; the value slot is structurally present but always empty.

No claim is made here about longer tool-use turns, multi-file edits, or
Claude Code's `-p`/print mode — only the interactive TUI, single
freeform-generation-turn case captured above.

### Q2 — does the title kill-switch also suppress OSC 9;4? **CAPTURED. High confidence: NO, they are independent.**

`CLAUDE_CODE_DISABLE_TERMINAL_TITLE=1` suppressed 100% of OSC 0 title
escapes (0 of 0, vs. 12 in the matched title-enabled run) and 0% of OSC
9;4 escapes (3 of 3, identical sequence and ordering to the title-enabled
run). The two are gated by different code paths in the shipped CLI. OSC
777 (desktop notification) also survived title-disable, for what it is
worth as a third independent channel.

This directly answers the premise question the bead exists to gate:
phux-w7z2.16's value case does **not** collapse. OSC 9;4 is not "one more
corroborating signal" that dies with the same switch as the title — it is
a genuinely independent channel that keeps working for exactly the
installs (`CLAUDE_CODE_DISABLE_TERMINAL_TITLE=1`) that `claude.toml`
already flags as missing a working-state backstop.

## What phux-w7z2.16 can and cannot rely on

**Can rely on:**
- Claude Code emits OSC 9;4 on the working <-> idle boundary, independent
  of the title kill-switch. A region keyed on OSC 9;4 state transitions
  (`3` = working, `0` = not-working) is a real, title-independent signal
  for Claude Code specifically, captured against a real shipped build.
- The payload shape matches `docs/spec/L1.md`'s existing `ProgressState`
  enum with no changes needed (`REMOVE = 0`, `INDETERMINATE = 3` are the
  only two values Claude Code has been observed to use).
- The terminator is BEL in every observed case; a parser does not need to
  handle `ST` for this specific CLI (though a generic OSC 9;4 parser
  should still accept both, since the escape is not Claude-specific).

**Cannot rely on:**
- A numeric progress percentage. Claude Code never populates it in this
  capture; do not design UI that expects a progress bar to fill.
- Any claim about `blocked` (permission-dialog) state. This capture only
  exercised a permission-free generation turn; whether Claude Code emits
  a distinct OSC 9;4 state (e.g. `2` = ERROR) while a permission dialog is
  up is **not captured** and not answered here.
- Any claim about codex, opencode, or pi (see below — attempted,
  inconclusive).
- Whether phux's own runtime can currently *observe* OSC 9;4 at all: per
  `ADR/0035-agent-asked-event.md` (already in-tree, unrelated to this
  bead), libghostty-vt does not surface OSC 9 / OSC 777 through its Rust
  API today — only title, cwd, and bell. This capture proves the bytes
  exist on the wire; it says nothing about whether `phux-server`'s current
  engine can see them without a libghostty-vt API addition or a
  independent scanner. phux-w7z2.16 needs to re-check that separately; it
  is the most likely blocker between "the bytes exist" (this bead) and
  "phux can use them" (that one).
- Whether v2.1.207 (the version `claude.toml`'s screen rules are pinned
  to) emits the same sequence. This capture is against v2.1.227/228 —
  ~20 patch releases later. The behavior is plausible to be stable across
  that range (OSC 9;4 support is the kind of thing that ships once and
  stays), but that is inference, not evidence, and is flagged as such.

## Also found (adjacent, not asked, worth recording)

- Claude Code also emits `OSC 777;notify;Claude Code;Claude is waiting
  for your input BEL` when a turn finishes — a third channel alongside
  title and OSC 9;4, also observed to survive the title kill-switch. Not
  investigated further; noted so a future OSC-9;4 region design does not
  collide with it.
- The busy-glyph the title alternates through in this capture is `U+25D0`
  (◐, CIRCLE WITH LEFT HALF BLACK), not the `U+2802`/`U+2810` (⠂/⠐ Braille
  dots) pair `claude.toml` documents as verified against v2.1.207. Either
  the animation cycles through more than two frames and only two were
  ever screen-captured before, or the glyph set changed between v2.1.207
  and v2.1.227. The static "not busy" glyph (`U+2733`, ✳) matches
  `claude.toml` exactly. This is a version-drift observation for whoever
  next re-verifies `claude.toml`'s title rule (tracked separately as
  phux-w7z2.40's "re-capture to a checklist"); it does not affect this
  bead's answers, since the OSC 9;4 payload is identical regardless of
  which glyph frame is showing.

## A bug that cost the codex/opencode/pi answer

The first capture harness used a blocking `os.read(master_fd, ...)` in the
main loop. That works for Claude Code, which produces continuous output
(spinner repaints) while busy, so the loop returns to the top often
enough to notice "time to send staged input." It silently starves any CLI
that goes quiet after its own prompt (codex sits at a "do you want to
trust this directory?" dialog with no further redraw until it receives
input) — the loop blocks in `read()` forever, never reaching the code that
would send the keystroke that unblocks it. The harness in this directory
now uses `select.select([master_fd], [], [], 0.2)` before each read, which
fixes the deadlock; the `pty_capture.py` committed alongside this note
already has the fix.

## What remains open

- **codex, opencode, pi**: attempted, not answered. With the fixed
  harness, `codex` (v0.147.0, this environment) accepted the "trust this
  directory" dialog and echoed the composed prompt into its input line,
  proving the harness mechanics work against it, but no OSC 9;4 event was
  observed and the turn never visibly completed within a 100-second
  capture window — no error banner, no further redraw, no submission
  confirmation. This is most plausibly a submission-mechanics gap (does
  codex require a distinct keypress from a single combined `text + \r`
  write, e.g. because of how it distinguishes typed newlines from
  submission?) or an auth/network condition specific to this sandbox, not
  a documented absence of OSC 9;4 in codex. It is genuinely unresolved —
  **do not read the null result as "codex doesn't emit it."** opencode and
  pi were not attempted at all (time-boxed out of this bead's scope, which
  is Claude Code only; the bead lists the other three as "worth capturing"
  bonus content, not blocking).
- **`blocked` (permission dialog) state**: not exercised in either
  capture. Unknown whether Claude Code emits a distinct OSC 9;4 state
  while a permission prompt is up, or holds at whatever state a prior
  turn left it in.
- **v2.1.207 parity**: not re-verified; this capture is against
  v2.1.227/228 only.

### Reproduction procedure

For anyone who wants to extend this (codex/opencode/pi, the `blocked`
state, or a different Claude Code version), from a **plain, non-nested**
shell (i.e. not itself running inside a Claude Code session, to avoid the
`CLAUDE_CODE_*` env leakage this note worked around):

```bash
cd research/2026-08-12-osc-9-4-claude-code

# Baseline: title enabled.
PTY_CAPTURE_INPUT="count slowly from 1 to 400, one number per line, nothing else" \
  PTY_CAPTURE_INPUT_DELAY=3 \
  python3 pty_capture.py /tmp/claude-title-enabled.rawcap 80 claude

# Title kill-switch.
CLAUDE_CODE_DISABLE_TERMINAL_TITLE=1 \
  PTY_CAPTURE_INPUT="count slowly from 1 to 400, one number per line, nothing else" \
  PTY_CAPTURE_INPUT_DELAY=3 \
  python3 pty_capture.py /tmp/claude-title-disabled.rawcap 80 claude

# Inspect: every OSC 9; sequence with its terminator.
python3 -c "
import re
data = open('/tmp/claude-title-enabled.rawcap', 'rb').read()
for m in re.finditer(rb'\x1b\]9;', data):
    i = m.start()
    end = data.find(b'\x07', i)
    print(data[i:end+1])
"
```

To reach a permission dialog for the `blocked`-state question, swap the
prompt for one that requires a tool call phux/the sandbox has not
pre-approved, e.g. `PTY_CAPTURE_INPUT="run \`touch /tmp/probe.txt\`"`, and
raise `PTY_CAPTURE_INPUT_DELAY` / the capture duration enough to land the
dialog inside the window.
