---
audience: humans
stability: evolving
last-reviewed: 2026-10-18
---

# Desktop usage

**TL;DR.** Use the command palette to find work, actions, and settings.
For installation and development, see the [desktop README](README.md).

## Using it

### Command palette

The main menu beside the window controls drops a glass menu: new terminal,
agent, session, folder or window, **Open Recent**, Go To and the palette,
**View** (tab placement and sidebar grouping), pane splits, Settings and
Reconnect. Its header leads to **Switch Machine**. Session, tab and pane
actions (the **…** on a row, or a right-click) use the same glass menu.
**⌘P**, **⇧⌘P**, and **⌘,** open the same searchable panel: **Work**, **Actions**, and **Settings**. Work lists sessions
across hosts plus the active session's terminals and tabs; a typed search also
finds actions and settings. The work, terminal and tab this window shows
read **Current**, so the one in use stays distinguishable from similar names
on other hosts or tabs. Settings search opens a focused page of live controls,
with **All settings** to go back instead of a second sidebar. Actions start with five everyday commands; **All commands** expands the list.
Rows show the name and the selected command's shortcut. Typing searches the
complete catalog, including descriptions and settings. **Filter** narrows Work
by agent state; **Details** reveals the selected destination's metadata on demand. A leading `>` in
Work switches to Actions and keeps the rest of your search. Terminals and tabs you open often and recently rise among the
matches for a typed query; an empty query keeps the list in its sections.

Use **↑/↓** to choose and **Return** to run. **Escape** clears a nonempty
search, then closes the panel; clicking outside also closes it. Commands that
open a dialog move the keyboard into that dialog.

### Performance monitor

Open the command palette and search **Performance Monitor**, **CPU**, **memory**,
or **telemetry** to opt in. This is a utility, not a separate Developer menu.
It opens a native window and starts sampling immediately; neither the desktop
nor the server needs a restart. Reopening the command raises the same window.

While monitoring, the palette also offers **Stop Performance Monitoring**.
This closes the monitor and stops its sampler. Closing the window does the
same. You can enable it again immediately; each new monitor starts a fresh CPU
and metric interval.

For automatic opening at launch, the native executable accepts `--dev-telemetry`
or `PHUX_DESKTOP_DEV_TELEMETRY=1`. These are conveniences, not prerequisites for
the runtime controls.

The monitor samples the **desktop process**, not the server or terminal child
processes, once per second. CPU is measured over the actual elapsed interval
(one fully occupied core is 100%, so multiple cores can exceed 100%). Current
resident memory and lifetime peak resident memory are separate readings.
An em dash means unavailable or no interval yet, not zero.

The scrollable metric table shows the existing kernel, runtime, renderer, and
UI-thread instrumentation as interval rates, mean timings, and estimated p95.
**Pause** freezes the readings and stops sampling; **Resume** starts a fresh
interval. **Copy JSON** copies the displayed process readings, interval metrics,
and cumulative report. Sampling stops when the monitor closes; use the palette
to reopen it. This is local instrumentation: no telemetry is uploaded and no
metrics history is written by the monitor.

Without opening the monitor, there is no monitor sampling loop. The existing
cheap atomic render/runtime counters remain on. Nothing about enabling or
stopping this monitor changes the server.

### Updates

**Settings > Updates** checks the channel's release manifest
([ADR-0175](../../docs/adr/0175-one-release-manifest-per-channel.md)) at launch
and every six hours without blocking terminal input. Turn automatic checks off
or use **Check now** at any time. **Download update** downloads the bundle,
verifies its checksum and signature, and stages it beside `Phux.app` while you
keep working. **Restart to update** then quits, swaps the staged bundle in
after the app has exited, and reopens it. If the swap fails, the previous app
is moved back. Quitting normally also installs a staged update. The helper logs
to `~/Library/Logs/Phux/update.log`. The titlebar shows the same button.
Quitting the desktop detaches; it does not stop your server-owned terminals.

When an update changes the wire protocol, the Updates page says so, because
the phux CLI and server must move too. With **Manage phux updates** on, the
button becomes **Update phux & restart**. It updates and hands off the server
first, then restarts into the new desktop.

Choose **Stable** (the latest published desktop release) or **Next** (verified
main builds). You can switch back to Stable even from a newer Next build.
Desktop and runtime channels are independent.

**Manage phux updates** is off by default. When enabled, **Update phux & hand
off server** uses the installed CLI's checksum-verified updater and graceful
handoff, then reconnects the windows. It never silently closes agent sessions
to force a handoff. Homebrew, Cargo and Nix installations show their native
update command instead of replacing package-managed files. A refused handoff
offers **Retry server handoff** without downloading the runtime again.
Turning management
off leaves the CLI and running server untouched; you regain control of updates.
Only this Mac's runtime is managed, not remote hosts.

Update preferences live in `$XDG_CONFIG_HOME/phux-desktop/updates.json` (default
`~/.config/phux-desktop/updates.json`). Bare development executables and launches
with explicit CLI/socket/profile overrides cannot install updates.

The window is a sidebar (every session, the current one open on its tabs and
background terminals), workspace tabs, split panes, and a status bar that
speaks only for exceptions. Command-key chords belong to the app; everything
else reaches the terminal.

Choose **Top**, **Bottom** or **Sidebar** in **Settings > Layout & notifications > Tab placement**,
or **View** in the main menu. The choice is saved with your display
preferences. Side tabs live in the sidebar under their session, so there is one
column of navigation, not two; hiding the sidebar returns them to the titlebar,
and Quick Terminal keeps compact titlebar tabs. Bottom tabs sit below your work,
including in Plain Terminal mode; a plain terminal still hides a lone tab.
If side panels leave too little room for usable bottom tabs, they temporarily
return to the titlebar; widening the window restores your Bottom preference.
Selecting, closing,
double-clicking to rename, right-clicking for tab actions, and dragging to
reorder work in every layout. The command palette also offers **Tabs on Top**,
**Tabs on Bottom**, **Tabs in Sidebar**, and the three **Group Sessions** views.

Settings, dialogs, notifications, find controls, and dropdowns use the same
theme-aware glass treatment. **Settings > Appearance > Font family** previews
each face in the searchable dropdown, with bundled fonts pinned first.

Hosts are context on work, not window owners. The sidebar and Work search keep
all registered hosts' sessions available while the selected session changes.
Offline inventory stays visible and cannot be opened until it is refreshed.
**Settings > Hosts & connections** (also **Machines…** from the main menu's
**Switch Machine** page, the File menu and the palette) lists This Mac and every saved
machine with its state, opens, retries, disconnects, adds or forgets machines, and
refreshes inventory.
Catalog discovery never attaches terminal streams or starts work; selecting an
empty session keeps it empty. Tabs and splits still belong to the selected
session, not to a new cross-server Workspace record.

| Keys             | Action                                                                      |
| ---------------- | --------------------------------------------------------------------------- |
| ⇧⌘P / ⌘P         | Command palette / go to a terminal, agent or tab (`>` switches to commands) |
| ⌘T, ⌘O           | New terminal in the focused pane's folder; open a folder                    |
| ⌘D / ⇧⌘D         | Split right / down (a new terminal)                                         |
| ⌥⌘D              | Another view of the focused terminal                                        |
| ⌘W / ⇧⌘W         | Close pane / tab (detaches; the process keeps running)                      |
| ⌥⌘ arrows, ⌘[ ⌘] | Focus the pane in a direction, cycle panes                                  |
| ⇧⌘↩              | Zoom the focused pane                                                       |
| ⌘1–⌘9, ⇧⌘[ ⇧⌘]   | Select a tab                                                                |
| ⌘F, ⌘G / ⇧⌘G     | Find in the terminal, next / previous match                                 |
| ⌘E               | Find the selected text                                                      |
| ⌘L               | Scroll back to live output                                                  |
| ⌘↑ / ⌘↓          | Jump to the previous / next shell prompt (needs OSC 133 prompt marks)       |
| ⌘A               | Select everything, scrollback included                                      |
| ⇧⌘A              | Jump to the agent that most needs you                                       |
| ⌘B, ⌘,           | Toggle the sidebar, open Settings                                           |
| ⌘= ⌘- ⌘0         | Font size                                                                   |
| ⌘N               | New window, with its own connection and a fresh terminal                    |
| ⌥⌘N              | Move the focused pane into a new window of its own                          |
| ⌘K               | Clear the screen                                                            |
| ⇧⌘I              | Insert Path: browse or search paths on the focused terminal's host          |
| ⇧⌘R              | Reconnect, first restarting the server if it has stopped                    |
| ⌘-click          | Open the link under the pointer (OSC 8 or a URL in the text)                |

### Coming from Ghostty

If you have a Ghostty config (`~/.config/ghostty/config`, or the Application
Support copy), the first launch adopts it: font family and size,
`adjust-cell-width/height` percentages, colours including the 16-colour
palette and a named `theme`, window padding, `unfocused-split-opacity`,
`split-divider-color`, and `macos-option-as-alt`. Settings also exposes
**Terminal glass**, which lowers the default terminal background opacity over
the native blurred window background. Your `keybind` lines replace
the built-in chords where an equivalent command exists, including non-Command
chords such as `ctrl+tab`, `shift+enter` or a bare `f12`. Besides splits, tabs,
fonts, scrolling and search, that covers `text:` and `esc:` (typed as keys:
each control byte as its Control chord, `\n` as Ctrl-J, and ESC before a key
as Alt on it, so `shift+enter=text:\x1b\r` gives Alt-Enter; raw escape
sequences such as `csi:` are not sent), `ignore`, `move_tab`, `new_split:left/up`,
`prompt_surface_title`, `set_font_size`, `scroll_page_lines`, `toggle_maximize`,
the search actions, `jump_to_prompt` (it needs your shell to mark prompts with
OSC 133, as Ghostty's shell integration does), `select_all`, and
`copy_to_clipboard`, `paste_from_clipboard` and `paste_from_selection` on any
chord (the selection clipboard is the pane's own selection; copies are plain
text). `write_screen_file`, `write_scrollback_file` and `write_selection_file`
write plain text to a private file under `$TMPDIR`, then copy or paste its
path or open it; the palette's **Open Scrollback as a File** does the last for scrollback. A `global:` bind to `toggle_quick_terminal` becomes a
system-wide hotkey for a quick-terminal window that keeps its own terminal
between toggles. If Ghostty is still running it holds that hotkey too, so quit it
or rebind one of them. Settings > Ghostty re-applies the config, reloads it,
switches keybind import off (the global hotkey too, from the next launch),
lists malformed lines, and valid lines whose values it cannot represent, by
file and line (the rest still apply),
and counts the binds it skipped: key sequences,
bare letters, keys the window never reports (such as keypad keys), and
actions with no equivalent yet. Keys may be named as Ghostty names them, as
W3C codes (`KeyK`, `Digit1`, `ArrowUp`) or with a `physical:` prefix. `reset` is one: the
terminal's state belongs to the server, and resetting only this window's copy
would leave input encoding and every other client on the old state, so it
waits for a protocol request. `vt` and `html` copy and write formats are
another.

The window is one sheet of glass over the desktop blur; panes float on it as
cards with no header. Move the pointer to a pane's top edge for its controls
(split, zoom, close; double-click the capsule to zoom). Drag the gap between
panes, the sidebar edge, or tabs to rearrange; double-click a tab to rename it.
Splits grow in and closed panes fold away; the PTY is resized once, when the
motion settles. Cycling panes, directional navigation, and revealing a
terminal keep the zoom on the pane receiving keyboard input.
Dropping files onto a pane pastes their shell-quoted paths, and Command-V of a
copied file pastes its path rather than its name. Command-V of a clipboard
image (a screenshot, a copied picture) saves it under the temporary directory
and pastes that path, which Claude Code, Codex and similar agents read as an
image attachment. These are this Mac's paths, also for a remote pane. **Insert Path**
lists paths on the host that runs
the focused terminal (a satellite's own disk for a satellite pane), never this
Mac's; it needs a server that advertises `PATH_QUERY`. Enter types the chosen
path as one shell-quoted word and presses nothing else, and only into the pane
it opened on. **Terminate Terminal Process** (palette only) ends one process.
**Kill Session** (the palette, or a session's menu in the sidebar) asks first,
then ends every terminal in the session and deletes it; a notification reports any
terminal that did not end, and why. Closing a pane, tab or window never ends
work. New sessions are persistent: they stay, empty, after their last
terminal exits. Organization session layouts and display preferences persist
under `$XDG_STATE_HOME/phux-desktop/sessions/<socket-and-session-key-hash>/`,
so renaming a session does not change its saved tabs or splits. Older servers
retain the name-scoped directory. Existing name-scoped and unscoped layouts
are read-only migration fallbacks; explicit `--layout` paths stay unchanged.
Terminal identity is still checked against the server incarnation.
Unavailable saved panes retain their split positions until they can attach;
closing one explicitly removes it. Replacing the daemon retires its old view
handles rather than rebinding their numeric terminal IDs. Unsupported or
damaged layout files are preserved without automatic writes, with a notice in
the window. Back up and remove the affected layout file to start a fresh one.

Every message the window has for you lands behind the titlebar bell, with
Inbox and Notifications each newest first; clicking an agent entry jumps to its pane. Agent
alerts and errors also show briefly as a single card in the window and count
on the bell until you open the list. While Phux is in the background they go
to macOS Notification Center too, where a click brings the pane forward; this
needs the installed `Phux.app`, not a bare debug executable. Confirmations
and other routine results go only to the list. An agent's entry lasts only
while the agent stays in the state it announced.

Agent badges and agent notifications retire when a terminal closes or its
connection is replaced, including reconnects to the same daemon. Native events
and negotiation snapshots are separate reads: if the identity advances during a
drain, labels from that ambiguous batch are omitted until another metadata
update or reconnect rather than attributed to a new connection.

### Native agents

**New ACP Agent** lists configured ACP-capable integrations and their host
prerequisites. Launch creates one server-owned host and one headless provider
session; desktop and mobile observe and control that same AgentSession.
Closing the desktop detaches without stopping the ACP worker. A graceful phux
handoff preserves the provider session, activation, and structured transcript.
The desktop keeps that SDK owner's local draft, pending receipts, and question
edits when the handoff recreates layout placements. Controls stay disabled until
the fresh connection confirms the same parent, AgentSession, and activation.

The conversation labels **ACP** and **Attached TUI** are deliberate.
ACP Send, Stop, and provider dialogs use typed operations against the
exact activation, current turn, and question. Permission choices, confirmations,
selection, text/editor input, multi-question answers, and supported scalar JSON
forms never become terminal keys or automatic approvals. The first human
response across viewers reserves the request. Queued/submitted is not provider
consumption; an uncertain reply is shown without automatically sending again.
Attached TUI conversations keep their existing terminal-backed controls and
**Open Terminal** handoff for terminal-only approvals.

Native profiles cover OMP, Pi, Claude Code, OpenCode, Codex, Amp, Grok, Gemini
CLI, Cursor Agent, and Goose; Aider is terminal-only. Operators install and
authenticate the native ACP executable or maintained adapter explicitly.
The picker explains unavailable prerequisites and never installs or accepts
terms for you. For remote sessions, launch on the executing host; the desktop
does not create a local SDK to impersonate remote work. Setup and exact receipt
semantics are in [the agent CLI guide](../../docs/consumers/agents.md#acp-owned-native-sessions).

Project panes declared with `integration:` can prefer their configured native
backend. Arbitrary `command:` or `exec:` panes remain terminals, even when their
argv resembles an agent CLI. SDK archive recipes resume known provider sessions
with fresh destination bindings, not archived controller secrets or source IDs.
