---
audience: contributors
stability: stable
last-reviewed: 2026-07-27
---

# 0064 — Playback as a pane

**TL;DR.** `phux play` creates a Terminal whose PTY is fed from a cast, not a
viewer that paints one onto your shell. It is a real pane — attachable,
snapshotable, resizable, watchable, killable — which is the only version of
playback phux can build that nothing else already builds. Zero wire change:
the pane's process is this binary re-invoked in a hidden writer mode.

Status: Accepted
Date: 2026-07-27

## Context

[ADR-0060](./0060-self-contained-session-recording.md) shipped recording and
listed a player under Alternatives: *"Shipping a player (`phux play`).
Rejected as out of scope: `asciinema play` already exists, and a `.cast` is
the interoperable artifact precisely so we do not have to."*

That argument is still correct, and this ADR does not overturn it. It
overturns one reading of it — that "a player" is one thing. There are two, and
only one was rejected on the evidence:

1. **A shell-level viewer**: paint a cast onto the terminal the user is
   already sitting in, then exit. `asciinema play` is this, it is good, and a
   second implementation would be a surface phux has to keep honest (pause,
   seek, key handling, v3 timing) for no capability gain.
2. **A pane whose PTY is fed from a cast.** A different object: it lives in
   the multiplexer, so every verb already aimed at panes works on it. Nothing
   outside a multiplexer can produce that, and `asciinema play` running inside
   a pane is not it — that is a shell process painting a screen, gone when it
   exits, with no id, no snapshot, and no observers.

## Decision

1. **`phux play FILE [TARGET]` creates a pane.** It spawns a Terminal whose
   command is a playback writer and prints the new Terminal id. It does not
   attach and does not block for the length of the recording.
2. **The writer is this binary, re-invoked.** The launcher spawns
   `std::env::current_exe()` with a hidden `--pty-writer` flag. Inside the
   pane stdout *is* the PTY, so writing the cast's `o` bytes to stdout is
   literally "feed the PTY from the cast".
3. **Zero wire change.** `SPAWN_TERMINAL` already carries `command`, the
   server already injects `PHUX_TERMINAL_ID` + `PHUX_SOCKET` into every
   spawned pane, and `TERMINAL_RESIZE` already exists
   ([ADR-0062](./0062-headless-resize-and-window-size-policy.md)). No frame,
   no command tag, no `ServerFeature` bit, no version bump, no `docs/spec/`
   sentence.
4. **`TARGET` says where the pane goes, never what gets overwritten.** The
   playback pane is created *beside* `TARGET` (default `.`, the focused
   pane), splitting its window exactly as `phux spawn --target` does. There
   is no flag that plays into an existing pane.
5. **The pane is fitted to the recording, and holds the final frame.** Before
   the first byte, the writer resizes its own pane to the cast header's grid,
   and it honors each `r` event the same way; `--no-fit` opts out. When the
   recording ends the writer parks, so the last frame stays readable and
   `phux snapshot` cannot lose a race with process exit; `--close` opts out.
6. **The shell-level viewer stays unbuilt.** `--pty-writer` is hidden and is
   not a user surface. `asciinema play FILE` remains the answer to "play this
   cast in my terminal", and `phux play --help` says so.

## Why

**Why a pane is not "asciinema play with extra steps".** The test is what you
can do to the running thing. Against a playback pane: `snapshot` reads its
grid, `resize` sizes it, `rec` re-records it, an agent subscribes over
`ATTACH_TERMINAL` and diffs frames, a second client watches with you, `kill`
ends it. Against `asciinema play` in a shell: nothing — the process owns the
glass and the only interface is the keyboard of whoever is standing there.
Every one of those verbs works on this pane without a line of new code, which
is both the argument for the feature and the reason it is small.

**Why a spawned child and not a server-side timer.** Writing bytes into a pane
on a schedule *is* what a PTY child does. A new frame carrying "write these
bytes into that Terminal" costs a command tag, a feature bit, and a spec
change — exactly the ADR-0061 bill ADR-0060 declined to pay for a recorder —
and is strictly worse: a server-side player grows its own lifecycle (pane
kill, server upgrade, a client detaching mid-playback) that a child process
inherits for free. The existing `ROUTE_INPUT` / `APPLY_INPUT` path was checked
first and is the wrong pipe by construction: it writes to the PTY's *input*
side, so a cast's output bytes would arrive as keystrokes.

**Why the writer clears `OPOST` and `ECHO` on its own tty.** A cast's `o`
bytes came off a PTY *master*, so they have already been through one
terminal's output post-processing; writing them into a slave applies it
twice, and `ONLCR` rewrites every bare `\n` into `\r\n` — silently resetting
the column under a recording that meant to advance a row without returning
the carriage. `ECHO` is the same corruption from the other side: nothing here
reads stdin, so an echoed keystroke can only land in the middle of a frame.
`ISIG` is left on, which is why Ctrl-C stops a playback. Nothing restores
these, so the change is gated on `tcgetsid(stdout) == getpid()` — only the
pane's own process is its tty's session leader, and a writer run by hand from
a shell fails that test and leaves the terminal alone rather than handing
back a shell with no echo.

**Why fit-by-default.** A cast is bytes that were correct at one geometry:
played into a narrower grid, wrapped lines wrap in the wrong place and
absolute cursor addresses land elsewhere, so the result is unreadable rather
than approximate. phux is the one player that can fix that, because
`TERMINAL_RESIZE` makes a grid settable without a TTY. When the resize does
not hold — an attached client's viewport owns the size under every
`window-size` policy but `manual` — the writer says so in one line and plays
anyway, because a wrapped recording beats a refusal.

**Why holding the final frame is the default.** A pane that vanished on the
last byte would be unobservable: the artifact of a playback is the screen it
painted, and any caller that snapshots it would be racing process exit.

## Tradeoffs

- **A held pane is a parked process.** Playback panes accumulate until
  killed, exactly like a pane sitting at a shell prompt. `--close` and
  `phux kill` are the answers; the last-pane exit
  ([ADR-0003](./0003-server-process-model.md)) and the idle lifetime
  ([ADR-0063](./0063-ephemeral-server-lifetime.md)) still apply.
- **No pause, no seek, no scrubbing.** The writer only moves forward. Ctrl-C
  stops it; transport control would be the shell-level player this declines
  to build.
- **Only `o` and `r` drive a pane.** Recorded input (`i`), markers (`m`), and
  the recorded exit status (`x`) are read and ignored.
- **The launcher and the writer must agree on an argv.** Contained by a unit
  test that round-trips the built argv back through the real clap parser, and
  by both ends being the same binary — a version skew is impossible.
- **A mid-stream `r` event races the bytes after it.** The writer waits for
  the server's registry read-back, but the pane actor applies a resize on a
  different mailbox than its PTY read, so the resize and the frame after it
  can interleave. One frame, only on a recording that resizes mid-stream; the
  alternative is a wire ack that does not exist.

## Alternatives

- **Ship nothing; point at `asciinema play`.** Still the right answer for
  case 1, and `phux play --help` says so. It is not an answer for case 2:
  `asciinema play` cannot produce a snapshotable, attachable,
  agent-observable terminal, and that is the entire feature.
- **A new `PLAY_CAST` command tag behind a `ServerFeature` bit.** The
  ADR-0061-compliant way to do it server-side. Rejected: it buys durability
  nobody asked for at the cost of a feature bit and a server-side lifecycle,
  when a child process delivers the same pane for free.
- **Play into an existing pane.** Rejected: the bytes would race the pane's
  shell, and the only way to stop that racing is to kill the shell — which is
  `phux kill` plus `phux play`, spelled confusingly. A pane's process is its
  identity.
- **A separate `phux-play` binary, or a writer mode with no hidden flag.**
  Rejected: the writer's argv is an internal contract between two halves of
  one binary, and documenting it would make the shell-level player real by
  accident, compatibility promise included.

## Related

- ADR-0060 — self-contained recording; this supersedes its "shipping a player"
  rejection for the pane-shaped case only, and inherits its consumer-side,
  zero-wire-change posture.
- ADR-0061 — capabilities add, versions break; what makes "zero wire change"
  the requirement rather than the preference.
- ADR-0062 — headless resize; the frame this reuses to fit a pane.
- ADR-0050 — explicit spawn ownership; how the pane lands beside `TARGET`.
