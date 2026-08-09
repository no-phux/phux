---
audience: humans, agents, contributors
stability: evolving
last-reviewed: 2026-08-02
---

# phux status-bar widgets reference

**TL;DR.** Every status-bar widget kind the binary registers, with the exact options and defaults each factory accepts, plus the universal `style` table and the `min-cols` / `max-cols` responsive-visibility bounds. Rendered from the same spec consts the factories validate options against, so a kind or option is listed here exactly when the binary accepts it.

<!--
GENERATED FILE - do not edit. A unit test byte-compares this page
against `phux gen-reference-docs` output and fails on any drift, so
hand edits do not survive. Regenerate with `just docs-gen`.
-->

A `[[status.widgets]]` entry (or a plugin's `[[widgets]]` contribution) names one of the kinds below plus kind-specific options. The set is closed twice over: an unknown `kind` fails the bar build, and every factory rejects an option outside its documented set with a did-you-mean suggestion — `phux config check` surfaces both as located findings. Each section here renders from the same spec const the factory validates against, so these options are exactly the ones the binary accepts.

## `cwd`

The focused pane's live working directory, fed by the server's `cwd_changed` agent events (kernel-queried PTY-child cwd). A `$HOME` prefix collapses to `~`; renders nothing until the cwd is known.

Options:

- `format` — string, default `"{cwd}"` — render template; every `{cwd}` occurrence is replaced with the (home-collapsed, truncated) directory.
- `truncate` — integer `> 0`, optional — maximum displayed characters of the directory itself (format literals not counted); truncation keeps the path's trailing end.

## `exec`

The first output line of a user-supplied command, run by the client on an interval as a bounded child process (10s hard cap, `kill_on_drop`) — render never blocks on it; the bar shows the last completed run, and a failed or timed-out run keeps the last good output.

Options:

- `command` — string or non-empty string array, required — a string runs via `/bin/sh -c` (so `~` and `$VAR` expand); an array is an argv run directly.
- `interval` — duration string (`"500ms"`, `"30s"`, `"2m"`, `"1h"`) or positive integer seconds, default `"5s"` — run cadence, floored to 1s.
- `parse-ansi` (also spelled `parse_ansi`) — bool, default `true` — interpret SGR escape sequences in the output into per-cell styles; when `false` (and for every non-SGR escape either way) escapes are stripped.

## `exit`

The focused pane's last command exit code, fed by the OSC-133 `D`-mark (`command_finished.exit_code`), so it requires shell integration. Renders nothing until a command finishes with a reported code.

Options:

- `format` — string, default `"{code}"` — render template; every `{code}` occurrence is replaced with the decimal exit code.

## `help-hints`

Dim, prefix-aware affordance hints (`<prefix>  Space palette · ? help · [ copy`), rendered with the configured prefix chord. Drops hints from the right as the bar narrows, and disappears entirely rather than showing a fragment.

No kind-specific options.

## `session-name`

The current session's name, optionally truncated, templated via `format`, and prefixed.

Options:

- `format` — string, default `"{name}"` — render template; every `{name}` occurrence is replaced with the (truncated) session name.
- `prefix` — string, optional — literal text prepended verbatim to the formatted output.
- `max-len` (also spelled `max_len`) — integer `> 0`, optional — truncate the session name itself to this many characters (prefix and format literals not counted); no ellipsis.

## `switch`

A clickable chip that opens the agent-fleet switcher (the same overlay `prefix A` opens). Every cell of the chip, padding included, is a click target. Pair it with `max-cols` to surface it only on terminals too narrow for the sidebar and the full tab strip.

Options:

- `label` — string, default `"switch"` — the chip's text. Rendered with one space of padding on each side.
- `chip` — style table, default bold reverse-video — the chip's style. Reverse video by default so the affordance reads as a button on any palette.

## `text`

A literal string, rendered verbatim. The building block for separators, labels, and fixed decoration in a custom bar.

Options:

- `value` — string, REQUIRED — the literal text to render. May be empty, which renders nothing; there is no default, because a `text` widget with no `value` is always a mistake rather than a request for a blank.

## `time`

The wall clock, strftime-formatted, rendered in the local time zone and repainted every second.

Options:

- `format` — string, default `"%H:%M"` — strftime spec, validated at build time (an invalid directive fails `phux config check` and the bar build).

## `windows`

The tmux-style tab bar: one segment per window, the active one in the `active` style and the rest in `inactive`, joined by `separator`. A zoomed active window gets a ` Z` marker, a window waiting on a human answer a ` !` marker, and every tab is a click target committing `select-window` for its index — in any slot, top or bottom bar.

Options:

- `active` — style table, default bold reverse-video — style of the active window's segment.
- `inactive` — style table, default dim — style of inactive windows' segments.
- `separator` — string, default `" "` — literal text between segments.
- `format` — string, default `"{index}:{name}"` — per-segment template; `{index}` (0-based position, the `select-window` selector) and `{name}` (the editable label) are substituted.

## The universal `style` option

Every kind additionally accepts a `style` table with optional `fg`, `bg` (color strings: names, `#rrggbb`, or palette indices) and the boolean attributes `bold`, `dim`, `italic`, `underline`, `reverse`. The registry applies it uniformly before the factory runs, so no widget can opt out. Precedence: cells the widget styles itself keep their own style — `windows`' `active`/`inactive` segments, `exec`'s SGR-parsed output, `help-hints`' dim base all win — and only cells the widget left plain inherit the widget-level `style`. A `style` value that is not a table, or a table with an unknown field, fails the bar build and is flagged by `phux config check`.

## The universal `min-cols` / `max-cols` options

Every kind also accepts `min-cols` and `max-cols`: integer bounds on the width of the **whole status row** (not the widget's own share) outside which the widget renders nothing at all. A hidden widget costs no width, so the widgets that remain get the columns it would have taken.

Use them to make one lineup change shape with the terminal rather than shrink inside it. The honest answer to a narrow window is often not "show this smaller" but "do not show this": a clock is worth four columns at 120 and worth none at 45. The shipped `[status]` block uses exactly this to trade the session name and clock for a `switch` chip below 65 columns.

```toml
right = [
  { kind = "session-name", min-cols = 65 },
  { kind = "time", format = " %a %H:%M", min-cols = 65 },
  { kind = "switch", max-cols = 64 },
]
```

Both bounds are inclusive and either may be given alone. A `min-cols` above `max-cols` describes a widget that could never render and fails the bar build, as does a non-integer value; `phux config check` flags both.
