---
audience: humans, agents, contributors
stability: evolving
last-reviewed: 2026-08-02
---

# phux config reference

**TL;DR.** The complete `config.toml` surface: a section index pinned against the config schema, every scalar knob with its shipped default, and the annotated default configuration embedded in the binary that generated this page.

<!--
GENERATED FILE - do not edit. A unit test byte-compares this page
against `phux gen-reference-docs` output and fails on any drift, so
hand edits do not survive. Regenerate with `just docs-gen`.
-->

The configuration surface of `~/.config/phux/config.toml`. The loader layers your file on top of the annotated defaults shown at the bottom of this page: every key you set wins, everything you omit keeps tracking the shipped default. Scaffold a starter file with `phux config init`, validate yours with `phux config check`, and inspect the effective merged result with `phux config show`.

## Sections

| Section | Contents |
|---|---|
| `[defaults]` | Server-wide defaults: shell, `TERM`, scrollback depth, mouse tracking, spawn-time cwd policy, session naming, multi-view window sizing. |
| `[keybindings]` | Prefix chord, the prefix-table and global binding maps, and the which-key popup knobs. |
| `[status]` | Status-bar composition: widget lists for the left, center, and right slots, plus which outer-terminal row the bar reserves. |
| `[sidebar]` | The window sidebar: off by default; width in columns and the edge it docks to when enabled. |
| `[[hooks.<event>]]` | Event hooks: per event name, an array of `when` predicates each paired with an action to run on match. |
| `[[plugins]]` | Declarative plugin manifests composed into this config; each entry names a `phux-plugin.toml` path and an enabled flag. |
| `[[satellites]]` | Federation satellites a hub routes to: name, endpoint, token-file path, and certificate pin (ADR-0038). |
| `[[connector]]` | Outbound relay links this server supervises: relay endpoint, token-file path, and certificate pin (ADR-0052). |
| `[[remote]]` | Remote phux servers this machine attaches to, written by `phux host enroll` / `phux host add` and resolved by `phux attach <name>` (ADR-0055). |
| `[theme]` | Free-form color slots (`slot = "color"`) consumed by the renderer. |
| `[experimental]` | Opt-in unstable knobs; anything here may change or disappear without notice. |

## Scalar keys

Every scalar knob with its shipped default, serialized from the schema itself. Keys that are unset by default (`defaults.shell`, `defaults.spawn-on-attach`) and composite keys — widget lists, binding tables, hook and registry arrays — do not appear here; the annotated config below documents them in place.

| Key | Default |
|---|---|
| `defaults.cwd-inheritance` | `"inherit-focused"` |
| `defaults.history-limit` | `50000` |
| `defaults.mouse` | `true` |
| `defaults.session-name-template` | `"${cwd-basename}"` |
| `defaults.term` | `"xterm-256color"` |
| `defaults.window-size` | `"smallest"` |
| `experimental.predictive-echo` | `false` |
| `keybindings.prefix` | `"C-a"` |
| `keybindings.which-key` | `true` |
| `keybindings.which-key-delay-ms` | `400` |
| `sidebar.enabled` | `false` |
| `sidebar.position` | `"left"` |
| `sidebar.width` | `20` |
| `status.position` | `"bottom"` |

## The annotated default config

The base layer embedded in the binary (`crates/phux-config/src/default.toml`), verbatim. `phux config init` writes a fully-commented projection of this file, and `phux config show --default` prints it.

```toml
# phux default configuration.
#
# This file ships with the phux binary (embedded via include_str!) and
# defines what phux looks and feels like out of the box. The loader
# layers your ~/.config/phux/config.toml ON TOP of this file: any key
# you set there overrides the matching default below; anything you omit
# keeps the default.
#
# Chord syntax (see crates/phux-config/src/keybind.rs):
#   C-a    Ctrl+a
#   M-a    Meta/Alt+a
#   S-a    Shift+a  (equivalent: a bare uppercase letter "A")
#   Tab    named key (case-sensitive)
#   F1     function key 1..24
#   |      ASCII punctuation; shifted glyphs (|, ?, ", etc.) decompose
#          to physical-key + implicit Shift on a US layout.
#
# For the full schema, see crates/phux-config/src/schema.rs and docs/consumers/tui.md.

[defaults]
# shell = unset → honor $SHELL (fallback /bin/sh).
# TERM advertised to the inner program of every server-spawned pane. The
# xterm-256color baseline is the safe universal value (phux-7vx): it gives
# 256 colours and the standard xterm keys with no kitty-keyboard
# advertisement, so ncurses TUIs like htop keep working. A per-spawn
# SPAWN_TERMINAL env override for TERM always wins over this. Set to
# "ghostty" to opt into ghostty's extended terminfo once your apps are
# known to round-trip the kitty keyboard protocol. Evidence so far
# (phux-0o8 harness, crates/phux-server/tests/kip_roundtrip.rs): the phux
# stack round-trips the kitty protocol end-to-end — nvim opts in via
# CSI-u and works, fzf/less/vim/btop are regression-free under
# TERM=ghostty — but htop, the app that broke last time, is unproven, so
# the shipped default stays conservative.
term = "xterm-256color"
history-limit = 50000
# Client-side outer-terminal mouse tracking on attach (ADR-0048): divider
# drag-to-resize and click-to-focus work without an inner program turning
# mouse mode on. false = pass-through-only (native click-drag selection).
mouse = true

# How a newly-spawned pane picks its working directory. One of:
#   "inherit-focused"     — match the focused pane's CWD (tmux behavior).
#   "home"                — always start in $HOME.
#   "session-root"        — the directory the session was created in.
#   "last-cwd-per-window" — remember the last CWD per window and reuse.
# Default "inherit-focused" requires server-side PTY working-dir tracking
# (OSC 7 from the shell, or a kernel-side query); wiring is a follow-up
# to phux-4li.1. The knob lands first so consumers can target it.
cwd-inheritance = "inherit-focused"

# What naked `phux` (or `phux attach` with no name) opens with when it
# auto-creates its default session: this command runs via `$SHELL -c`.
# `phux new` deliberately ignores this and gives an explicitly-created
# session a plain shell. Unset (= no key) means "use defaults.shell"
# (which in turn honors $SHELL, then /bin/sh).
# spawn-on-attach = "/usr/bin/zellij-like-launcher"

# Template for auto-created session names (naked `phux` / `phux attach`
# with no name). ${cwd-basename} expands to the basename of the client's
# working directory at session-create time (`:` in it becomes `_` so the
# name stays selector-safe); unknown placeholders pass through verbatim.
#
# We ship ${cwd-basename} rather than a literal "default": a bench of
# project checkouts then reads as its directories in the session picker,
# which is the only way the picker is useful with more than one session
# open. Set session-name-template = "default" for the old behavior.
session-name-template = "${cwd-basename}"

# Policy for picking one geometry when concurrent views of a single
# Terminal disagree on size (ADR-0027). A Terminal is one PTY + one grid,
# so it renders one size; a view wanting another size letterboxes rather
# than reflowing. One of:
#   "smallest" — use the smallest view's size; larger views letterbox.
#                Nothing is ever cropped (tmux's default).
#   "largest"  — use the largest view's size; smaller views may crop.
#   "latest"   — track the most recently resized view.
#   "manual"   — hold a fixed size (implies a future resize verb).
# Not yet consumed at the size-decision point: multi-view/multi-client
# geometry negotiation is a follow-up (today the server uses
# last-writer-wins per SPEC §10.5, phux-nk07). The knob lands first so
# consumers can target it.
window-size = "smallest"

[keybindings]
# Prefix: chord captured by the TUI before everything else. After hitting
# the prefix, your next keystroke is matched against `prefix-table`.
prefix = "C-a"

# Which-key popup: press the prefix and hesitate for which-key-delay-ms
# and a small panel pops up listing every prefix-table continuation
# (built from YOUR bindings, including rebinds). It is display-only: any
# key dismisses it and executes its binding exactly as if the popup were
# not there; Esc dismisses it and cancels the prefix. Set
# which-key = false to disable.
#
# 400 ms is deliberately snappier than tmux-ish 600: the popup is phux's
# primary discovery surface, so it should feel like a hint that arrives
# while you hesitate, not a timeout you wait out.
which-key = true
which-key-delay-ms = 400

[keybindings.prefix-table]
# Pane splits, tmux-style: " stacks panes (horizontal divider), % places
# them side-by-side (vertical divider).
'"' = { action = "split-pane", direction = "horizontal" }
"%" = { action = "split-pane", direction = "vertical" }
# Ergonomic aliases for the same two actions: the chord draws the divider
# it creates, so you never have to remember which of " and % is which.
"|" = { action = "split-pane", direction = "vertical" }
"-" = { action = "split-pane", direction = "horizontal" }

# Kill: x kills the focused pane, X (Shift-x) kills the whole window
# (all its panes). Soft-kill via shell exit; see phux-4li.12.
"x" = "kill-pane"
"X" = "kill-window"

# Move focus, vi-style.
"h" = { action = "focus-direction", direction = "left" }
"j" = { action = "focus-direction", direction = "down" }
"k" = { action = "focus-direction", direction = "up" }
"l" = { action = "focus-direction", direction = "right" }

# Cycle through panes in DFS order. tmux uses n/p for *windows*, so the
# pane-cycle bindings live on o / ; (phux-4li.16).
"o" = "next-pane"
";" = "previous-pane"
# One-entry, client-local MRU jump-back (repeat to toggle between two panes).
"=" = "last-pane"

# Zoom: toggle the focused pane to fill the whole window (tmux prefix-z).
"z" = "toggle-zoom"

# Sidebar: show/hide the window-list strip (mnemonic: VS Code's Ctrl-B).
"b" = "toggle-sidebar"

# Copy-mode: enter text selection mode. Arrow keys adjust selection,
# Enter copies to clipboard, Escape cancels. (phux-wave-a-copy-mode)
"[" = "copy-mode"

# Windows ("tabs"), tmux-style (phux-4li.15/.16). c opens a new window;
# n/p cycle windows; X kills the active window (see kill bindings above);
# 0-9 jump to a window by the position shown in the status bar; w opens the
# grouped window picker (below).
"c" = "new-window"
"n" = "next-window"
"p" = "previous-window"
# Tab hops to the next window too, matching tabbed-UI muscle memory.
"Tab" = "next-window"
"0" = { action = "select-window", index = 0 }
"1" = { action = "select-window", index = 1 }
"2" = { action = "select-window", index = 2 }
"3" = { action = "select-window", index = 3 }
"4" = { action = "select-window", index = 4 }
"5" = { action = "select-window", index = 5 }
"6" = { action = "select-window", index = 6 }
"7" = { action = "select-window", index = 7 }
"8" = { action = "select-window", index = 8 }
"9" = { action = "select-window", index = 9 }

# Rename the active window. With an explicit name it renames immediately,
# e.g. "," = { action = "rename-window", name = "build" }. The bare
# binding opens an interactive type-a-name prompt pre-filled with the
# current name (phux-ahv.1).
"," = "rename-window"

# Rename the current session (tmux convention: $ renames the session, ,
# renames the window). With an explicit name it renames immediately, e.g.
# "$" = { action = "rename-session", name = "notes" }. The bare binding
# opens an interactive type-a-name prompt pre-filled with the current
# session name; on commit the client sends the RENAME_SESSION command and
# optimistically updates its own status bar. The server is authoritative,
# so other attached clients pick up the new name on their next snapshot.
"$" = "rename-session"

# Command palette: a filterable list of every action, each annotated with
# its bound chord. Choosing one runs it through the same path a keybind
# takes (phux-ahv.8). Actions contributed by enabled plugins appear here
# automatically as `plugin:` rows.
#
# Space is the primary spelling — prefix-Space is one relaxed two-key
# motion and needs no Shift, which is what you want from the chord you
# reach for when you have forgotten every other chord. `:` stays bound as
# the vim-flavored alias.
"Space" = "command-palette"
":" = "command-palette"

# Navigation, mnemonic s/w (sessions / windows):
#
#   w — grouped window picker (`window-picker`, phux-4li.19). A filterable,
#       hierarchical list: every session is a header with its windows
#       nested beneath. Picking a window in the CURRENT session switches to
#       it via `select-window { index }`; picking another session switches
#       to that session (its windows then list under its own picker). See
#       the window-picker builder for why foreign windows aren't yet
#       directly selectable in one step.
#
#   s — session picker (`session-picker`, phux-4li.20). A filterable list
#       of all server sessions, built from the ATTACHED snapshot. The current
#       session appears first and is marked; choosing it simply closes the
#       picker. Choosing another row commits `switch-session { name }` through
#       the single dispatch path, which drives an in-process re-attach on the
#       same connection (phux-eb0): the client detaches from the current
#       session, re-runs the ATTACH handshake against the target, rebuilds all
#       session-scoped state, and repaints — no reconnect, no flicker.
#
#   a — alias for the session picker, kept for muscle memory (the original
#       binding before s/w landed). Same action as `s`.
"w" = "window-picker"
"s" = "session-picker"
"a" = "session-picker"

#   A — agent-fleet dashboard (`agent-fleet`, phux-foz.7). One filterable
#       overlay over the whole fleet: every pane of the attached session
#       grouped under session headers, labelled with its agent's name/kind
#       (the structured phux.agent/v1 record when declared, the OSC title
#       otherwise), a state glyph (! blocked, * working, - idle, . done,
#       ? unknown), an attention highlight for panes waiting on you, and
#       the pane's branch or cwd. Enter focuses the chosen pane; rows for
#       other sessions hop there via `switch-session` (their panes list
#       once attached). The rows refresh live as agent events arrive.
"A" = "agent-fleet"

# Attention navigation (phux-oih5.16, ADR-0049). q jumps to the next pane
# with a pending AgentEvent::Asked, cycling by window order then DFS leaf
# order; Q returns to the pane where that excursion began. These unoccupied
# lowercase/uppercase mnemonic keys are client-local and never write layout
# metadata or shared focus. The fleet dashboard above remains unchanged.
"q" = "next-attention"
"Q" = "return-from-attention"

# New session: C (Shift-c) creates a fresh session and switches this
# client to it in-process. With no name it opens a prompt; the session
# picker (s, alias a) also offers a "+ New session" row. (Lowercase c is
# new-window; capital is the session-level peer.)
"C" = "new-session"

# Detach this client from the session.
"d" = "detach"

# Supervisory control (ADR-0033, "take the wheel + kill"). Take seizes
# exclusive input over the focused pane so an agent driving it is locked
# out until you Give it back; Freeze (SIGSTOP) pauses the pane's process
# mid-step and Resume (SIGCONT) lets it run again. Destructive signals
# (kill / terminate / interrupt) are left to `phux signal` so a stray
# keystroke can't tear a process down; bind them yourself if you want them.
"W" = "take-input"
"g" = "give-input"
"f" = { action = "signal-terminal", signal = "freeze" }
"u" = { action = "signal-terminal", signal = "resume" }

# Resize the focused pane. Capital letters are equivalent to `S-<lower>`.
"H" = { action = "resize-pane", direction = "left",  amount = 5 }
"J" = { action = "resize-pane", direction = "down",  amount = 5 }
"K" = { action = "resize-pane", direction = "up",    amount = 5 }
"L" = { action = "resize-pane", direction = "right", amount = 5 }

# Show the help overlay: a centered modal listing every keybinding. Esc
# or ? again dismisses it.
"?" = "show-help"

# Live config reload (phux-foz.5): re-reads this file (full layered
# load, `extends` included) and applies keybindings, theme, and status
# bar in place; a file that fails to parse or validate keeps the old
# config and surfaces the error in a toast. Unbound by default — the
# command palette offers it as "Reload the config file"; bind it
# yourself if you iterate on config a lot:
#   "R" = "reload-config"

[keybindings.global]
# No other global bindings shipped by default. Add yours here, e.g.:
#   "M-Enter" = "detach"

# Plugin packages are declarative manifests. phux can inspect them with
# `phux plugin list --json`, validate/link/toggle/unlink them with
# `phux plugin`, project their agent-state declarations with
# `phux config agents --json`, and execute action entries with
# `phux config run PLUGIN ACTION [--json]`. Event hooks, plugin panes, and
# link handlers are declarative until their host surfaces ship. The
# manifest path may be absolute or relative to this config file.
#
# [[plugins]]
# manifest = "/path/to/plugin/phux-plugin.toml"
# enabled = true
#
# A minimal phux-plugin.toml:
#   id = "example.agent-tools"
#   name = "Agent Tools"
#   version = "0.1.0"
#   min_phux_version = "0.0.2"
#
#   [[actions]]
#   id = "summarize"
#   title = "Summarize pane"
#   contexts = ["pane"]
#   command = ["python3", "summarize.py"]
#
#   [[agents]]
#   id = "codex"
#   label = "Codex"
#   state = "blocked"      # unknown | idle | working | blocked
#   attention = "high"     # none | low | normal | high
#   contexts = ["workspace", "pane"]
#
#   [[events]]
#   id = "idle"
#   title = "Pane idle"
#   on = "pane.idle"
#   command = ["sh", "-c", "printf idle"]
#
#   [[panes]]
#   id = "board"
#   title = "Agent Board"
#   placement = "split"
#   command = ["agent-board"]
#
#   [[links]]
#   id = "ticket"
#   title = "Open ticket"
#   patterns = ["https://linear.app/*"]
#   command = ["agent-ticket", "{url}"]

# Federation satellites are remote phux servers a hub can route to. The registry
# lands before routing so operators can declare stable names and endpoints.
# The hub authenticates to a satellite as an ordinary remote consumer
# (ADR-0038): run `phux pair` on the satellite host, save the printed token
# into an owner-only file on the hub, and record that file's path plus the
# printed certificate fingerprint here. The token itself never goes in this
# file — only the path to it.
#
# [[satellites]]
# name = "devbox"
# endpoint = "quic://devbox.example:8788"
# enabled = true
# token-file = "/home/me/.local/state/phux/satellites/devbox.token"
# cert-fingerprint = "AB:CD:..."

# Outbound connectors let this server reach consumers through a self-hosted
# relay without accepting inbound traffic. Each link is supervised
# independently; the relay terminates TLS and can see plaintext phux frames.
# Keep the enrollment token in an owner-only file, never inline.
#
# [[connector]]
# relay = "relay.example:4433"
# token-file = "/home/me/.local/state/phux/connectors/relay.token"
# cert-fingerprint = "AB:CD:..."

# Remote phux servers this machine attaches TO (ADR-0055): `phux attach
# mini` resolves the endpoint, certificate pin, and token recorded here, so
# you never retype them. `phux host enroll` and `phux host add` write these
# entries for you; edit by hand only to relabel or retarget one. As with
# satellites, the token itself never goes in this file — token-file points
# at an owner-only file holding one hex token on one line. `session` picks
# the session to attach on arrival; omit it and the remote server's own
# last-attach memory decides, exactly as a local naked `phux` does.
#
# [[remote]]
# name = "mini"
# endpoint = "quic://mini.example:8788"
# token-file = "/home/me/.local/state/phux/remotes/mini.token"
# cert-fingerprint = "AB:CD:..."
# session = "main"

[status]
# Left: the window/tab bar. The `windows` widget renders one tab per
# window as `{index}:{name}`; by default the active tab is bold +
# reverse-video and the rest are dimmed. Every part is restylable —
# `active`/`inactive` take a style table (fg/bg color strings +
# bold/dim/italic/underline/reverse), and `separator`/`format` tune the
# layout, e.g.:
#   { kind = "windows", active = { fg = "black", bg = "green", bold = true }, inactive = { fg = "gray" }, separator = " | " }
# Right: session name + clock (the leading space in the time format keeps
# them apart).
#
# The shipped lineup: padded window tabs left, contextual help hints
# center, session name + clock right. The tab `format` carries a leading
# and trailing space and `separator` is empty, so the active tab's fill
# reads as a padded chip rather than a highlight hugging the glyphs — and
# because every cell of a segment (padding included) is a click target,
# the padding also widens the mouse target by two columns per tab.
#
# The two colors below are the theme's `accent` (#7aa2f7) over the
# tokyonight base, and the same muted blue the `dim` / `sidebar_section`
# theme slots use for recessive chrome. They are spelled out rather than
# named because widget styles are plain data — see `[theme]` in
# docs/consumers/tui.md section 4.4 for the slot list.
left = [
  { kind = "windows", format = " {index}:{name} ", separator = "", active = { fg = "#1a1b26", bg = "#7aa2f7", bold = true }, inactive = { fg = "#565f89" } },
]
center = [{ kind = "help-hints" }]
# The right slot changes shape with the terminal, via the universal
# `min-cols` / `max-cols` options every widget accepts (they gate on the
# width of the whole bar, not on the widget's own share).
#
# Wide enough for ambient context (>64 columns): session name and clock.
# Narrower than that, those columns are worth more as an affordance: the
# sidebar is hidden, the tab strip has collapsed to the active tab, and
# nothing on screen says other work exists. So the clock and the session
# name step aside for a `switch` chip — click it (or press prefix-A) for
# the fleet, which on a small terminal opens full-screen.
right = [
  { kind = "session-name", min-cols = 65 },
  { kind = "time", format = " %a %H:%M", min-cols = 65 },
  { kind = "switch", max-cols = 64 },
]
# Which outer-terminal row the bar reserves: "bottom" (default) or "top".
# position = "bottom"

# Chrome + overlay palette. Every slot below is set to its shipped value
# in code (crates/phux-client/src/render/theme.rs); the block is commented
# here so you can see the whole system at once and override one slot
# without hunting for the name. Values accept named colors ("cyan"), hex
# ("#7aa2f7") and ANSI indices ("12"). See docs/consumers/tui.md section
# 4.4 for what each slot paints.
#
# [theme]
# accent          = "#7aa2f7"  # modal titles, query caret, active tab fill
# title           = "#7aa2f7"  # section/window titles (tracks accent)
# chord           = "#9ece6a"  # keybinding chords in help + which-key
# action          = "reset"    # action labels — inherit your terminal fg
# dim             = "#565f89"  # the recessive register: hints, sub-lines
# border          = "#3b4261"  # rules and modal borders
# section_header  = "#e0af68"  # headings inside help / pickers
# error           = "#f7768e"  # error and alarm text
# surface         = "reset"    # modal interior fill; reset = transparent
# shadow          = "#16161e"  # floating-modal drop shadow
# selection_fg    = "#c0caf5"  # selected list row / copy-mode strip fg
# selection_bg    = "#33467c"  # selected list row / copy-mode strip bg
# attention       = "#ff9e64"  # "needs you": asked-marker, blocked agents
# sidebar_section = "#565f89"  # the sidebar's `spaces` / `agents` headers
# agent_idle      = "#565f89"  # agent lifecycle: nothing needs you
# agent_working   = "#9ece6a"  # agent lifecycle: live progress
# agent_blocked   = "#ff9e64"  # agent lifecycle: waiting on a human
# agent_done      = "#7dcfff"  # agent lifecycle: finished

# The window sidebar (prefix-b toggles it): a vertical strip listing the
# session's windows as tabs, each labelled by its OSC title (falling back
# to the window name), the focused one highlighted. Off by default; when
# enabled it reserves `width` columns on the `position` edge ("left" or
# "right") and the panes tile into the remaining area.
#
# The strip yields on a terminal too narrow to afford it: below
# `width` + 40 columns it is not reserved at all and the panes get the
# whole viewport back, because a strip that leaves 30 columns of actual
# work is costing you the very panes it exists to help you move between.
# At those widths `prefix-b` rings the bell instead of toggling nothing,
# and the fleet switcher (`prefix A`, or the status bar's `switch` chip)
# is the navigation surface — it opens full-screen there.
#
# [sidebar]
# enabled = false
# width = 20
# position = "left"
```
