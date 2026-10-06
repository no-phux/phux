---
audience: contributors
stability: stable
last-reviewed: 2026-10-06
---

# 0146 — The TUI pushes kitty keyboard disambiguate on the host

**TL;DR.** While attached, the TUI pushes kitty keyboard flags `1`
(disambiguate) on the host terminal (`CSI > 1 u`) and pops them (`CSI < u`)
in the one shared terminal reset. Modified keys then reach phux as `CSI u`
with their modifiers intact, so the server re-encodes Cmd+Return, Ctrl+I, or
Ctrl+Backspace exactly as native Ghostty would. Hosts without the protocol
ignore the push and keep the legacy decoder.

Status: Accepted
Date: 2026-10-06

## Context

Input reaches the server as structured key events (ADR-0006, ADR-0008), and
the server encodes them against the pane's own keyboard modes. That pipeline
is only as faithful as what the TUI can read from the host. phux never asked
the host for the kitty protocol, so the host sent legacy bytes. Those bytes
cannot carry some keys: Ghostty sends Cmd+Return as `CSI 27;9;13~` and
Shift+Tab as `CSI Z`, Ctrl+Backspace and Ctrl+H are both `0x08`, and Ctrl+I
and Tab are both `0x09`. An app inside a pane that enabled kitty flags (Claude
Code, for example) therefore saw a different key than it sees outside phux.
The parser already decoded `CSI u`. Nothing asked the host to send it.

## Decision

- `write_enter_alt_screen` pushes `CSI > 1 u` right after `?1049h`.
  `write_terminal_reset` pops it with `CSI < u` right before `?1049l`, and
  only while the alt screen is active. Every teardown routes through that
  reset: guard Drop, signal exit, panic hook, detach exit, and the
  switch-host hand-off. The crash handler's `RESTORE_SEQ` already popped.
- The TUI pushes flag `1` only, without report-events, alternates,
  report-all, or associated text.
- The TUI does not query support with `CSI ? u`. It relies on hosts that lack
  the protocol ignoring the push, as the kitty spec requires.
- Chord matching ignores Caps Lock and Num Lock, which a kitty host reports on
  `CSI u` keys and a legacy host never sent.

## Why

- **Flag 1 is enough, as measured.** One test encoded about 400 key and
  modifier combinations with libghostty's encoder as the host, ran them
  through phux's parser, and re-encoded them for a pane. With flag 1, every
  combination came out byte-identical to native Ghostty for panes using
  kitty flags 0 or 1. Panes using report-all (`31`) differed in 6 cases. In
  legacy mode, 420 of the 1188 checks differed.
- **Higher flags cost more than they buy.** Report-all (`8`) turns every
  plain keystroke into `CSI u`, reports bare modifier presses that would
  cancel a pending prefix chord, and needs associated text (`16`) to carry
  typed characters. It would only help the rare pane using report-all.
- **No query round trip.** A query would add a reply wait to attach startup
  and a timeout path for hosts that never answer. When a host ignores the
  push, the TUI simply stays on the legacy path, which keeps working (CSI 27
  modifyOtherKeys, `CSI Z`, BS as Ctrl+Backspace).
- **One reset path.** The push and pop live with the other host modes
  (bracketed paste, focus, mouse) and need no new state. The kitty stack is
  per screen, so leaving the alt screen restores the host's main-screen
  flags even if a pop were lost.

## Tradeoffs

- Panes using report-all or associated-text flags still miss information for
  text keys: lock state, keypad versus digit row, and which modifiers were
  consumed. Under flag 1 the host sends those keys as plain text.
- The TUI cannot tell whether the host took the push. Behavior is correct
  either way, but logs cannot say which path a host used.
- An old terminal that misparses `CSI > 1 u` as `CSI u` (cursor restore) only
  moves the cursor before the first full paint.

## Alternatives

- **modifyOtherKeys (`CSI > 4;2 m`).** It is the xterm form, which Ghostty
  also supports, but it never distinguishes Esc from Alt-prefixed keys or
  reports Super uniformly, and the parser's kitty path is already complete.
- **Mirror each pane's kitty flags onto the host.** Host flags would change
  with focus, and a keystroke racing a focus change would be decoded in the
  wrong mode.
- **Push report-all plus associated text (`1|8|16`).** This would be exact for
  report-all panes, at the costs listed under Why.
