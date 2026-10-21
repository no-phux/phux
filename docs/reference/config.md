# Config settings

Every `config.toml` section and setting, with the shipped defaults.

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
| `[sidebar]` | The window sidebar: enabled by default; width defaults to 20 columns, 0 adapts to 28–40; position chooses the docking edge. |
| `[chrome]` | Responsive-chrome breakpoints: the column and row counts at which overlays go full-bleed and the sidebar yields its columns back to the panes. |
| `[notifications]` | Agent notifications the TUI posts when an agent blocks or finishes: delivery mode (terminal OSC, system, bell, off), the OSC dialect, per-event toggles, focus suppression, and sound. |
| `[[hooks.<event>]]` | Event hooks: per event name, an array of `when` predicates each paired with an action to run on match. |
| `[[plugins]]` | Declarative plugin manifests composed into this config; each entry names a `phux-plugin.toml` path and an enabled flag. |
| `[[satellites]]` | Federation satellites a hub routes to: name, endpoint, token-file path, and certificate pin. |
| `[[connector]]` | Outbound relay links this server supervises: relay endpoint, token-file path, and certificate pin. |
| `[[uplink]]` | Hubs this server dials out to as a satellite, written by `phux host join`: satellite name, hub endpoint, the hub's CA pin, and the enrolled identity's paths. |
| `[[remote]]` | Remote phux servers this machine attaches to, written by `phux host add` and resolved by `phux attach <name>`. |
| `[[projects]]` | Named projects `phux project open <name>` resolves: a checkout path and an optional out-of-repo recipe. |
| `[theme]` | Free-form color slots (`slot = "color"`) consumed by the renderer. |
| `[experimental]` | Opt-in unstable knobs; anything here may change or disappear without notice. |
| `[policy]` | The authorization posture read at server start: `local` (owner socket only) or `paired` (workload mTLS, scope ceilings enforced at dispatch). |
| `[voice]` | The server-side transcriber behind `TRANSCRIBE`: an argv that turns an uploaded clip into text for a paste. |
| `[organize]` | Who judges where panes belong for `phux organize`: `TypeSafe`'s Jev with a key, else repository matching. |
| `[limits]` | Server-enforced ceilings that are not a per-pane spawn default: the largest L3 metadata value the server stores at one key. |

## Scalar keys

Every scalar knob with its shipped default, serialized from the schema itself, plus the tri-state knobs whose shipped state is *unset* and whose unset meaning is spelled out in place of a value. Keys that are simply absent by default (`defaults.shell`, `defaults.spawn-on-attach`) and composite keys — widget lists, binding tables, hook and registry arrays — do not appear here; the annotated config below documents them in place.

| Key | Default |
|---|---|
| `chrome.compact-cols` | `64` |
| `chrome.compact-rows` | `18` |
| `chrome.min-pane-cols` | `40` |
| `defaults.agent-log-bytes` | `4194304` |
| `defaults.approval-max-pending` | `64` |
| `defaults.approval-max-pending-total` | `1024` |
| `defaults.approval-ttl-secs` | `120` |
| `defaults.clipboard-write` | `"allow"` |
| `defaults.cwd-inheritance` | `"inherit-focused"` |
| `defaults.event-journal-bytes` | `1048576` |
| `defaults.event-journal-entries` | `4096` |
| `defaults.history-bytes` | `10485760` |
| `defaults.history-limit` | `50000` |
| `defaults.mouse` | `true` |
| `defaults.retain-on-exit` | `false` |
| `defaults.retain-on-exit-max` | `256` |
| `defaults.retain-on-exit-max-secs` | `86400` |
| `defaults.retain-on-exit-secs` | `600` |
| `defaults.session-name-template` | `"${cwd-basename}"` |
| `defaults.term` | `"xterm-256color"` |
| `defaults.window-size` | `"smallest"` |
| `experimental.predictive-echo` | unset — the dial decides: on when the attach leaves the machine, off otherwise. `true` / `false` force it on every transport |
| `keybindings.prefix` | `"C-a"` |
| `keybindings.which-key` | `true` |
| `keybindings.which-key-delay-ms` | `400` |
| `limits.metadata-value-bytes` | `262144` |
| `notifications.blocked` | `true` |
| `notifications.done` | `true` |
| `notifications.mode` | `"terminal"` |
| `notifications.sound` | `false` |
| `notifications.suppress-focused` | `true` |
| `notifications.terminal-protocol` | `"auto"` |
| `organize.jev-key-command` | unset — `TYPESAFE_API_KEY` only. An argv whose trimmed stdout is the key, run when the variable is unset |
| `organize.offline` | `false` |
| `policy.mode` | unset — `local`, and a configured remote listener, relay connector, or workload authority location refuses to start the server. `local` admits the owner socket only; `paired` requires an enrolled workload certificate on every TLS connection |
| `sidebar.agents-share` | `50` |
| `sidebar.enabled` | `true` |
| `sidebar.hosts` | `true` |
| `sidebar.hosts-refresh-secs` | `10` |
| `sidebar.position` | `"left"` |
| `sidebar.width` | `20` |
| `status.position` | `"top"` |
| `voice.timeout-secs` | unset — 30. Seconds before the transcriber is killed and the request refused |
| `voice.transcriber` | unset — `TRANSCRIBE` is refused with a remedy. An argv; `{path}` is replaced by the uploaded clip's path and stdout is the transcript |

## The annotated default config

The base layer embedded in the binary (`crates/phux-config/src/default.toml`), verbatim. `phux config init` writes a fully-commented projection of this file, and `phux config show --default` prints it.

```toml
# phux default configuration.
#
# These are the settings phux ships with. Your ~/.config/phux/config.toml
# is layered on top: any key you set there replaces the one below, and
# everything you leave out keeps its default. Keep your file to the
# settings you actually change.
#
#   phux config check   validate your file
#   phux config show    print the merged result
#   phux config keys    list every key binding and where it came from
#
# Key syntax:
#   C-a     Ctrl+a
#   M-a     Alt+a (also A-a)
#   S-a     Shift+a; a bare capital "A" means the same
#   Tab     a named key: Tab, BackTab, Enter, Esc, Space, Backspace,
#           Delete, Insert, Up, Down, Left, Right, Home, End, PageUp,
#           PageDown, F1 to F24 (case-sensitive)
#   |       punctuation is the key you press, so "|" is Shift+\ on a
#           US layout; on other layouts write S- explicitly
#
# Every setting is listed at https://docs.phux.sh/reference/config.

[defaults]
# shell: the program a new pane runs. Unset uses $SHELL, then /bin/sh.

# TERM for programs in new panes. xterm-256color works everywhere: 256
# colours and the standard xterm keys. Set "ghostty" for its extended
# terminfo and the kitty keyboard protocol once the programs you use
# handle it (htop is the usual holdout).
term = "xterm-256color"

# Scrollback per pane. Whichever limit is reached first trims the oldest
# lines, and on anything wider than a narrow pane the byte limit is the
# one that binds, so history-bytes decides how deep a pane goes:
#
#   history-bytes      rows kept at 80 cols   at 200 cols
#   2 MiB              ~2669                  ~1030
#   10 MiB (default)   ~14400                 ~5790
#   32 MiB             ~47500                 ~19031
#
# The cost is memory, and only once a pane has printed that much: an idle
# pane holds nothing, a flooded one about this many bytes. Multiply by
# your busy panes before raising it. 67108864 (64 MiB) is the maximum.
history-limit = 50000
history-bytes = 10485760

# How much of each agent's event log phux keeps (`phux agent log`). The
# oldest records drop first. 67108864 (64 MiB) is the maximum.
agent-log-bytes = 4194304

# How many recent events the server keeps for a watcher (`phux watch`,
# `phux wait`) that reconnects, so it can catch up on what it missed.
# Whichever limit is reached first drops the oldest. 1048576 events and
# 67108864 bytes are the maximums.
event-journal-entries = 4096
event-journal-bytes = 1048576

# Keep a pane inspectable after its program exits: its exit status, last
# screen, and scrollback stay until they expire or are closed. Off by
# default; `phux run` and `phux spawn` can ask for it per pane. A kept
# pane holds its scrollback in memory. 4096 is the maximum count.
retain-on-exit = false
retain-on-exit-secs = 600
retain-on-exit-max-secs = 86400
retain-on-exit-max = 256

# Mouse support: click a pane to focus it, drag a divider to resize.
# false leaves the mouse to your terminal's own selection.
mouse = true

# What happens when a program copies to the clipboard (OSC 52): "allow"
# copies, "ask" asks first, "deny" ignores it. Only the focused pane can
# copy, and programs can never read your clipboard.
clipboard-write = "allow"

# Where a new pane starts:
#   "inherit-focused"     the focused pane's directory (like tmux)
#   "home"                always $HOME
#   "session-root"        the directory the session was created in
#   "last-cwd-per-window" the last directory used in that tab
# Following the focused pane needs your shell to report its directory
# (OSC 7); most do.
cwd-inheritance = "inherit-focused"

# spawn-on-attach: the command a plain `phux` runs when it creates its
# first session, through `$SHELL -c`. `phux new` ignores it. Unset runs
# the shell.
# spawn-on-attach = "htop"

# Names for sessions phux creates for you. ${cwd-basename} is the
# directory you ran phux from, so a row of project checkouts reads as
# their names in the session picker; ${random-name} is a word pair such as
# drifting-cedar. Set "default" for one fixed name.
session-name-template = "${cwd-basename}"

# When two clients view one pane at different sizes, which size wins:
#   "smallest" the smallest view; bigger ones get blank margins (like tmux)
#   "largest"  the largest view; smaller ones are cropped
#   "latest"   whichever client resized last
#   "manual"   keep the current size
window-size = "smallest"

[keybindings]
# The prefix. Press it, then a key from the prefix table below.
prefix = "C-a"

# After the prefix, hesitate for which-key-delay-ms and a popup lists
# every key you can press next, rebinds included. It never eats a key:
# whatever you press runs as if the popup were not there, and Esc cancels
# the prefix.
which-key = true
which-key-delay-ms = 400

# Keys pressed after the prefix. Set a key to "none" to unbind it.
# https://docs.phux.sh/reference/keys lists these grouped as the popup
# shows them.
[keybindings.prefix-table]
# Split. " and % are tmux's keys; | and - draw the divider they make.
'"' = { action = "split-pane", direction = "horizontal" }
"%" = { action = "split-pane", direction = "vertical" }
"|" = { action = "split-pane", direction = "vertical" }
"-" = { action = "split-pane", direction = "horizontal" }

# Close the focused pane; X closes the whole tab after asking. close-view
# (no key; in the palette and context menu) hides a pane without ending
# it, and reopen-views brings hidden panes back.
"x" = "kill-pane"
"X" = "kill-window"

# Move between panes: h j k l like vim, o / ; in order, = back to the
# last one.
"h" = { action = "focus-direction", direction = "left" }
"j" = { action = "focus-direction", direction = "down" }
"k" = { action = "focus-direction", direction = "up" }
"l" = { action = "focus-direction", direction = "right" }
"o" = "next-pane"
";" = "previous-pane"
"=" = "last-pane"

# Zoom the focused pane to fill the tab, and back.
"z" = "toggle-zoom"

# Show or hide the sidebar.
"b" = "toggle-sidebar"

# Copy mode: scroll back, search, select, and copy. Arrows move, Enter
# copies, Esc leaves.
"[" = "copy-mode"

# Tabs. Actions keep tmux's word "window" for a tab. 0-9 jump to the tab
# with that number in the status bar; < and > move the tab left or right.
"c" = "new-window"
"n" = "next-window"
"p" = "previous-window"
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
"<" = { action = "move-window", delta = -1 }
">" = { action = "move-window", delta = 1 }

# Open a new tab in a directory you pick (G), or find a path and type it
# into the pane (F).
"G" = "go-to-directory"
"F" = "find-path"

# Rename the tab (,) or the session ($). Give a name to skip the prompt:
#   "," = { action = "rename-window", name = "build" }
"," = "rename-window"
"$" = "rename-session"

# The command palette: every action, searchable, with its key. Space is
# the easy one; : and ? open it too.
"Space" = "command-palette"
":" = "command-palette"
"?" = "command-palette"

# Pick a tab from every session (w), or a session (s).
"w" = "window-picker"
"s" = "session-picker"

# Agents. A opens every agent with its state and whether it needs you;
# a steps the sidebar's agent filter (all, this session's, needs you);
# q jumps to the next pane waiting for an answer and Q goes back.
"A" = "agent-fleet"
"a" = "agents-filter"
"q" = "next-attention"
"Q" = "return-from-attention"

# Settings page.
"S" = "settings"

# Move every pane into the session for its repository (`phux organize`).
# Nothing is closed, and organize-undo reverses it.
"O" = "organize"

# Write a bug report bundle (`phux report show` opens it).
"B" = "report-bug"

# rec-mark (no key) drops a marker while `phux rec` is recording:
#   "M" = { action = "rec-mark", label = "intro" }

# Sessions: C makes a new one, N and P cycle, ) goes back to the last.
"C" = "new-session"
"N" = "next-session"
"P" = "previous-session"
")" = "last-session"

# Detach. The session keeps running; `phux` brings you back.
"d" = "detach"

# Take the keyboard from an agent driving the pane (W), give it back (g),
# pause the pane's program (f) and resume it (u). Signals that end a
# program are left to `phux signal`, so a stray key cannot kill one.
"W" = "take-input"
"g" = "give-input"
"f" = { action = "signal-terminal", signal = "freeze" }
"u" = { action = "signal-terminal", signal = "resume" }

# Resize the focused pane by 5 cells.
"H" = { action = "resize-pane", direction = "left",  amount = 5 }
"J" = { action = "resize-pane", direction = "down",  amount = 5 }
"K" = { action = "resize-pane", direction = "up",    amount = 5 }
"L" = { action = "resize-pane", direction = "right", amount = 5 }

# reload-config (no key; in the palette) applies your config file without
# detaching. Bind it if you edit the file often:
#   "R" = "reload-config"

[keybindings.global]
# Keys that work without the prefix. None ship, so every key reaches the
# program in the pane until you add one here:
#   "M-Enter" = "toggle-zoom"

# Plugin packages are declarative manifests. phux can inspect them with
# `phux plugin list --json`, validate/link/toggle/unlink them with
# `phux plugin`, project their agent-state declarations with
# `phux config agents --json`, and execute action entries with
# `phux config run PLUGIN ACTION [--json]`; `phux plugin log` lists
# recent action and hook runs with their exit status and output tail.
# The server runs enabled plugins' `[[events]]` hooks, and the TUI opens
# their `[[panes]]` (every placement) and lays out their `[[sidebar]]`
# sections; link handlers are still declarative only. The manifest path
# may be absolute or relative to this config file.
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
#   placement = "split"    # split | tab | zoomed | overlay (a floating box)
#   command = ["agent-board"]
#
#   [[sidebar]]            # a fixed band between Agents and Sessions
#   id = "exits"
#   title = "Last exit"
#   format = "{window}: exit {exit}"   # agent cwd exit index state title window
#   rows = 3                           # 1-8; a pane rows when every token resolves
#
#   [[links]]
#   id = "ticket"
#   title = "Open ticket"
#   patterns = ["https://linear.app/*"]
#   command = ["agent-ticket", "{url}"]

# Federation satellites are remote phux servers a hub can route to. The registry
# lands before routing so operators can declare stable names and endpoints.
# The hub authenticates to a satellite as an ordinary remote consumer:
# run `phux pair` on the satellite host, save the printed token
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
# independently. The relay is blind: each consumer runs TLS to this server
# inside the relayed stream, so the relay forwards only ciphertext.
# Keep the enrollment token in an owner-only file, never inline.
#
# [[connector]]
# relay = "relay.example:4433"
# token-file = "/home/me/.local/state/phux/connectors/relay.token"
# cert-fingerprint = "AB:CD:..."

# Uplinks make this server a satellite of a hub that cannot dial it (a
# sandbox behind NAT): the server dials the hub and serves the
# hub's streams over that one connection, so it needs no listener. `phux
# host join CODE` (the code comes from `phux host ticket NAME` on the hub)
# enrolls a key it keeps and writes the entry; edit by hand only to disable
# one. The key stays in an owner-only file, never inline.
#
# [[uplink]]
# name = "sandbox"
# hub = "hub.example:8788"
# ca-fingerprint = "sha256:..."
# client-cert = "/home/me/.local/state/phux/uplinks/uplink.sandbox.0123456789abcdef.pem"
# client-key = "/home/me/.local/state/phux/uplinks/uplink.sandbox.0123456789abcdef.key"

# Remote phux servers this machine attaches TO: `phux attach
# mini` resolves the endpoint, certificate pin, and token recorded here, so
# you never retype them. `phux host add me@mini` writes an entry for you
# over ssh; edit by hand only to relabel or retarget one. As with
# satellites, the token itself never goes in this file — token-file points
# at an owner-only file holding one hex token on one line. `session` picks
# the session to attach on arrival; omit it and the remote server's own
# last-attach memory decides, exactly as a local naked `phux` does. `ssh`
# is the destination the entry was set up through, which an attach uses to
# restart a stopped server there; `direct` is a paired `quic://` route kept
# beside an `ssh://` endpoint until it answers, when the attach promotes it.
#
# [[remote]]
# name = "mini"
# endpoint = "quic://mini.example:8788"
# token-file = "/home/me/.local/state/phux/remotes/mini.token"
# cert-fingerprint = "AB:CD:..."
# session = "main"
# ssh = "me@mini"
#
# A server reached through a relay registers the relay as its
# endpoint, the relay's pin as cert-fingerprint, and the relay route as
# tls-server-name, the TLS server name every dial offers. `phux pair
# --relay-route ROUTE` on the server prints a link that `phux attach
# --remote NAME --code` registers this way.
#
# A routed entry also needs the server's certificate authority pinned for
# that relay pin and route in `known-authorities` beside this file:
# every dial runs end to end with the server inside the relayed stream, so the
# relay sees only ciphertext. `phux attach --remote NAME --code LINK` and
# `phux host add ... --ca-fingerprint` write that pin; a hand-written entry
# without it is refused on every dial.
#
# [[remote]]
# name = "studio"
# endpoint = "quic://relay.example:4433"
# token-file = "/home/me/.local/state/phux/remotes/studio.token"
# cert-fingerprint = "AB:CD:..."
# tls-server-name = "studio"

# Projects: names `phux project open NAME` resolves. Each entry is
# a checkout `path` (absolute or `~/`); phux never scans for repositories.
# The checkout's own `.phux/project.toml` recipe runs only once its exact
# bytes are approved (`phux project trust`). `recipe` points at a recipe
# kept outside the repository instead, trusted because you wrote it here.
#
# [[projects]]
# name = "phux"
# path = "~/code/phux"
#
# [[projects]]
# name = "site"
# path = "~/code/site"
# recipe = "~/.config/phux/recipes/site.toml"

[status]
# Left: the window/tab bar. The `windows` widget renders one tab per
# window from `format` (`{index}` and `{name}`); a window running an agent
# shows that agent's state glyph before its name. Every part is
# restylable — `active`/`inactive`/`index` take a style table (fg/bg color
# strings + bold/dim/italic/underline/reverse), and `separator`/`format`
# tune the layout, e.g.:
#   { kind = "windows", active = { fg = "black", bg = "green", bold = true }, inactive = { fg = "gray" }, separator = " | " }
# Right: session name + clock (the leading space in the time format keeps
# them apart).
#
# The shipped lineup: padded window tabs left, empty center, session name
# + clock right. Destinations live on the prefix, the command palette
# (`prefix Space` / `:`), and the context menu — not as a permanent hint
# strip. `help-hints` remains available if you want the teaching bar back:
#   center = [{ kind = "help-hints" }]
# The tab `format` carries a leading
# and trailing space with a one-cell separator, so the active tab's fill
# reads as a padded chip rather than a highlight hugging the glyphs — and
# because every cell of a segment (padding included) is a click target,
# the padding also widens the mouse target by two columns per tab.
#
# The bar sits on the theme's `surface`, the same bed as the sidebar, so
# the two read as one frame around the panes. The active tab is the
# theme's `accent` + bold on its `selection_bg` (the sidebar's selected-row
# colour), a chip with one cell of padding either side. Inactive tabs are
# `dim` text with no fill, so the strip is not a row of slabs. The
# `{index}` selector recedes a step further, to `border`. Session name and
# clock recede to the same dim, and the clock keeps one cell of gutter from
# the edge, matching the sidebar's. The tabs ride the theme's slots, named
# `theme.<slot>`, so a `[theme]` name, file, or override recolours the bar
# with the rest of the chrome; any widget colour may name a slot this way
# (the `[theme]` block below lists the slots).
#
# A `button` runs one argument-free action on click. The `+` that follows
# the tabs opens a new tab, where a browser puts it; `+ session` in the
# top-right corner prompts for a new session's name. Both use the inactive tabs' ink,
# so they read as part of the strip rather than as alerts.
left = [
  { kind = "windows", format = " {index} {name} ", separator = " ", active = { fg = "theme.accent", bg = "theme.selection_bg", bold = true }, inactive = { fg = "theme.dim" }, index = { fg = "theme.border" } },
  { kind = "button", label = "+", action = "new-window", style = { fg = "theme.dim", bold = true } },
]
center = []
# The right slot changes shape with the terminal, via the universal
# `min-cols` / `max-cols` options every widget accepts (they gate on the
# width of the whole bar, not on the widget's own share).
#
# Wide enough for ambient context (>64 columns): session name and clock.
# Narrower than that, those columns are worth more as an affordance: the
# sidebar is hidden, the tab strip has collapsed to the active tab, and
# nothing on screen says other work exists. So the clock and the session
# name step aside for an `agents` chip (the `switch` widget) — click it (or
# press prefix-A) for every agent, which on a small terminal opens
# full-screen.
right = [
  { kind = "session-name", min-cols = 65, style = { fg = "theme.dim" } },
  { kind = "time", format = " %a %H:%M ", min-cols = 65, style = { fg = "theme.dim" } },
  { kind = "button", label = "+ session", action = "new-session", min-cols = 65, style = { fg = "theme.dim" } },
  { kind = "switch", max-cols = 64, chip = { fg = "theme.accent", bold = true } },
]
# Which outer-terminal row the bar reserves: "top" (default) or "bottom".
# position = "top"

# Colours for phux's own chrome. Every slot below shows its shipped
# value, commented out, so you can see the whole palette at once and
# override one slot without hunting for its name. Values accept named colors ("cyan"), hex
# ("#7aa2f7") and ANSI indices ("12").
#
# Two keys select a whole palette underneath the slots: `name` is an
# installed theme (`phux theme list`, `phux theme install URL`) and
# `file` is any Omarchy-schema colors.toml, e.g. Omarchy's own current
# theme. Slots set here still override the selected theme.
#
# [theme]
# name            = "tokyo-night"
# file            = "~/.local/state/omarchy/current/theme/colors.toml"
# accent          = "#bef264"  # modal titles, query caret, active focus
# title           = "#bef264"  # section/window titles (tracks accent)
# chord           = "#86efac"  # keybinding chords in help + which-key
# action          = "reset"    # action labels — inherit your terminal fg
# dim             = "#9aa4b2"  # the recessive register: hints, sub-lines
# border          = "#7c8696"  # rules and modal borders
# section_header  = "#9aa4b2"  # headings inside help / pickers
# error           = "#f87171"  # error and alarm text
# text            = "#f4f7fb"  # body copy ON a filled panel (see surface)
# surface         = "#171b23"  # sidebar + modal fill; "reset" = transparent
# shadow          = "reset"    # floating-modal drop shadow; reset = off
# selection_fg    = "#f4f7fb"  # selected list row / copy-mode strip fg
# selection_bg    = "#293628"  # selected list row / copy-mode strip bg
# attention       = "#fde047"  # "needs you": asked-marker, blocked agents
# sidebar_section = "#9aa4b2"  # the sidebar's section headers
# agent_idle      = "#9aa4b2"  # agent lifecycle: nothing needs you
# agent_working   = "#86efac"  # agent lifecycle: live progress
# agent_blocked   = "#fde047"  # agent lifecycle: waiting on a human
# agent_done      = "#bef264"  # agent lifecycle: finished
# divider         = "#7c8696"  # pane rules off the focused frame
# divider_focus   = "#bef264"  # the focused pane's own rules (also bold)
# pane_title      = "#9aa4b2"  # an unfocused pane's label on its top rule
# pane_title_focus = "#bef264" # the focused pane's label (also bold)
#
# Every slot above that paints text or a rule clears 4.5:1 (WCAG AA)
# against `surface`, in three ordered rungs: rules, recessive text,
# then lime focus plus BOLD. Retinting one slot is fine; dropping a
# rung below the floor makes chrome that some people cannot see at all.

# The sidebar (prefix-b toggles it), ON by default.
#
#   Agents      every agent, in stable order, under a chip row that
#               filters it in one click: `all`, `this` (this session's),
#               or `need` (blocked or asking, or finished and not yet
#               looked at); an `@` chip steps through the machines when
#               agents run on more than one. `prefix a` steps the same
#               filter, and the choice is kept across attaches.
#   Sessions    session names, with the current session's tabs nested
#               below and a host line per machine when there is more
#               than one.
#
# Clicking an agent row navigates to its tab, switching sessions if needed.
# Session names and host rows switch to that session with its host qualifier.
# The Agents and Sessions headings open their full management views. Agents
# overflow opens the agents dashboard; Sessions overflow opens the picker.
# The footer keeps a single New tab row; Commands and Settings live on
# the palette and the context menu.
#
# Width 20 (the default) fits a badge, a name, and a right-hand tag and
# leaves the terminal its columns. A positive width reserves exactly that
# many columns; 0 adapts to one quarter of the viewport, bounded to 28–40
# columns, for longer names on a wide terminal. It docks on the `position` edge ("left" or "right")
# and the panes tile into the remaining area. `prefix-b` turns it off, and
# that choice now sticks across session switches for the life of the
# attach.
#
# The strip yields on a terminal too narrow to afford it: below
# resolved width + 40 columns it is not reserved at all and the panes get the
# whole viewport back, because a strip that leaves 30 columns of actual
# work is costing you the very panes it exists to help you move between.
# At those widths `prefix-b` rings the bell instead of toggling nothing,
# and the agents dashboard (`prefix A`, or the status bar's `agents` chip)
# is the navigation surface — it opens full-screen there.
#
# Agents on top and Sessions below share the body at a fixed line:
# `agents-share` is the Agents panel's percent of the body, half (50) by
# default, whatever the agent population. Gaining or resolving an agent never
# moves Sessions; an empty Agents panel keeps its header and a quiet
# placeholder. Set `agents-share = 0` to drop the panel and give Sessions the
# whole strip.
#
# [sidebar]
# enabled = true
# width = 20
# position = "left"
# agents-share = 50

# Responsive-chrome breakpoints. The chrome adapts to small terminals
# around three thresholds; these move them without moving the behaviour.
# Shipped values shown, each derived from content rather than from a
# round number.
#
# `compact-cols` / `compact-rows`: at or below these a viewport is
# column- or row-starved, and overlays go full-bleed on that axis
# instead of floating. The axes are judged independently, because a
# short wide terminal and a narrow tall one want opposite things. Raise
# them for full-bleed pickers on a roomier terminal; lower them to keep
# floating modals on a small one.
#
# `min-pane-cols`: the narrowest pane area worth tiling into. The
# `[sidebar]` strip is not reserved below `[sidebar] width` + this, so
# lowering it keeps the strip on a narrower terminal.
#
# All three are plain counts with no reserved values: 0 disables a
# threshold, a very large value pins the opposite, and both are
# legitimate configurations rather than errors.
#
# [chrome]
# compact-cols = 64
# compact-rows = 18
# min-pane-cols = 40

# Agent notifications. The TUI
# posts one when an agent becomes blocked or asks for you, and when an
# agent that was working finishes. "Finished" fires once per run: looking
# at the pane before the notification goes out cancels it, and an agent
# in the pane you are looking at stays quiet while the terminal window
# has focus (suppress-focused). Bursts inside a second coalesce into one.
#
# `mode`: "terminal" writes an OSC notification to the outer terminal,
# so it reaches your desktop over SSH too; "system" uses the desktop
# notifier of the machine the TUI runs on (osascript on macOS,
# notify-send elsewhere); "bell" rings the terminal bell; "off".
#
# `terminal-protocol`: "auto" picks OSC 99 for kitty, OSC 777 for
# Ghostty, WezTerm, and foot, and OSC 9 otherwise. Over SSH the
# environment may not name your terminal, so pin "osc9", "osc777", or
# "osc99" if auto guesses wrong. Inside tmux the sequence is wrapped for
# passthrough (tmux needs `set -g allow-passthrough on`).
#
# [notifications]
# mode = "terminal"
# terminal-protocol = "auto"
# blocked = true
# done = true
# suppress-focused = true
# sound = false

# Authorization posture. Read once
# at server start; a change needs a restart.
#
# Unset is `local`, except that a configured remote listener, relay
# connector, or workload authority location (`PHUX_WORKLOAD_CA`, ...)
# refuses to start the server and names `paired` as the remedy: nothing
# ever grants a remote connection the owner's authority.
# The overlay listener is auto-bound only under `paired`.
#
# `local`: the owner's Unix socket only. A configured remote listener
# (`--listen`, `--quic`, `--webtransport`, their environment variables,
# or a `[[connector]]`) refuses to start the server.
#
# `paired`: the owner's Unix socket keeps full authority; every TLS
# connection must present a workload certificate enrolled with
# `phux workload add-key` and holds only its registry ceiling.
#
# [policy]
# mode = "paired"
```
