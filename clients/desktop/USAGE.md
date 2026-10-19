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

### Fullscreen

Use **⌃⌘F**, **Toggle Full Screen** in the command palette, or the green
window button to enter or leave native fullscreen. On macOS, the menu bar
and Dock auto-hide so the window fills the display; moving the pointer to
the corresponding screen edge reveals them. Exiting restores the window
through the normal macOS fullscreen transition. **Maximize Window** only
zooms the window within the desktop and is separate from fullscreen.

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

The monitor samples the **desktop process** once per second, and shows the
server's CPU, memory and heap every two seconds by running `phux perf
--json`. Terminal child processes are not included. **Show HUD** and
**5 s Trace** are the same commands the palette offers below. CPU is
measured over the actual elapsed interval
(one fully occupied core is 100%, so multiple cores can exceed 100%). Current
resident memory and lifetime peak resident memory are separate readings.
An em dash means unavailable or no interval yet, not zero.

**Overview**, **Memory**, and **Metrics** separate quick readings from the raw
instrumentation. Overview has hoverable Longbridge Kit CPU and app-memory charts.
App memory means macOS physical footprint (closest to Activity Monitor), or RSS
on Linux. Memory also shows native malloc blocks in use and reserved capacity
on macOS, without requiring Rust counting. Capacity is not resident RAM.
Charts retain at most 120 numeric samples, not
full reports, and restart after a pause. The content scrolls even in a short
window. Space pauses or resumes unless you are typing in a field; Escape, ⌘W
or Ctrl-W closes the monitor. ⌘F or Ctrl-F opens the metric filter; every search
word must match the metric name.

**Set baseline** compares later memory readings with the current snapshot and
restarts chart history without resetting counters. Repeat the same workload
and compare after it settles. **Release unused heap** asks macOS malloc, off
the UI thread, to return unused pages. It leaves live allocations intact and
reports the allocator's returned-byte count, which is not necessarily the
footprint reduction. It is best-effort, macOS-only, and disabled while paused.

Footprint, RSS and tracked Rust heap overlap: do not add them or subtract heap
from RSS to infer GPU memory. Native AppKit, Metal, window surfaces and
allocator-retained pages are outside the Rust allocation counter. The Memory
tab explains the measurements and gives the process-specific `vmmap -summary`
command for native attribution. Rust counting requires an isolated development
launch with `PHUX_DEV=1`, **Settings > Performance > Count Rust allocations**,
or **Count allocations** directly in the monitor's Memory tab.
The setting applies immediately across desktop windows without a restart and
lasts only for this run. It does not enable a dev socket or change the server.
Turn it off after profiling to remove counting overhead. Opening the monitor
never enables counting automatically.

The scrollable metric table shows the existing kernel, runtime, renderer, and
UI-thread instrumentation as interval rates, mean timings, and estimated p95.
**Pause** freezes the readings and releases this monitor's server polling watch;
another open HUD may still poll. **Resume** starts a fresh
interval. **Copy JSON** copies the displayed process readings, interval metrics,
and cumulative report. Sampling stops when the monitor closes; use the palette
to reopen it. This is local instrumentation: no telemetry is uploaded and no
metrics history is written by the monitor.

Without opening the monitor, there is no monitor sampling loop. The existing
cheap atomic render/runtime counters remain on. Nothing about enabling or
stopping this monitor changes the server.

### Dev mode

Launch with `PHUX_DEV=1` (or `phux-desktop --dev`) for everything at once:

- **HUD.** The top-right corner of each window shows frame rate, frame build
  time (p50, p99, max), jank (dropped frames while something animates or
  streams), CPU, memory, live and peak heap, allocations per second, and the
  server's CPU and memory, over a sparkline of the last 120 frame builds. The
  sparkline turns amber past an 8.3 ms build and red past 16.7 ms.
  **Toggle Performance HUD** in the palette shows it without dev mode. The HUD
  repaints twice a second, so a quiet window reads about 2 fps.
- **Heap accounting.** Every report gains live and peak heap and allocation
  counts.
- **Traces.** **Capture Performance Trace** records five seconds of desktop
  and server spans, plus memory and frame-rate tracks, into one file and copies
  its path. Open it in <https://ui.perfetto.dev>. Traces go to `phux-traces`
  beside the desktop log, or `PHUX_TRACE_DIR`.
- **Dev socket.** `desktop-<pid>.dev.sock` beside the server socket answers
  one line of JSON per request (`perf`, `frames`, `server`, `trace`, `hud`,
  `alloc`, `reset`, `help`). `phux perf --desktop`, `phux perf trace
  --desktop` and `phux perf hud on|off` use it, as do agents. It is created
  `0600` and carries no terminal content.

Start the server with `PHUX_DEV=1` too for its heap figures. A desktop that
starts the server passes its environment on.

### Updates

**Settings > Updates** checks the channel's release manifest
([release contract](../../docs/releasing.md)) at launch
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

The window is a sidebar (every visible session, its pane count and independently
expandable contents), workspace tabs, split panes, and a status bar that
speaks only for exceptions. Command-key chords belong to the app; everything
else reaches the terminal.

Choose **Top**, **Bottom** or **Sidebar** in **Settings > Layout & notifications > Tab placement**,
or **View** in the main menu. The choice is saved with your display
preferences. Side tabs live in a fixed-height, separately scrollable area at the
foot of the sidebar. Switching sessions never inserts tabs into the session tree;
hiding the sidebar returns them to the titlebar,
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
Click a session's name to select it; only its caret expands or collapses its
contents. Switching restores tabs and splits, not the destination's old window
display settings: the current filter, grouping, sidebar width and tab placement stay put. Selection never folds other sessions or changes the host filter.
**Settings > Layout & notifications > Sidebar machines**, or **Sidebar view >
Visible machines**, controls which machines appear. All are shown by default;
exclusions are saved, do not disconnect hosts, and do not limit Work search.
The sidebar's **All machines / Machines · filtered** control opens that menu;
**Reset** shows every machine again, even when the active host was hidden.
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
| ⌘W / ⇧⌘W         | Close pane / tab (ends the processes and removes their views)               |
| ⌥⌘ arrows, ⌘[ ⌘] | Focus the pane in a direction, cycle panes                                  |
| ⇧⌘↩              | Zoom the focused pane                                                       |
| ⌘1–⌘9, ⇧⌘[ ⇧⌘]   | Select a tab                                                                |
| ⌘F, ⌘G / ⇧⌘G     | Find in the terminal, next / previous match                                 |
| ⌘E               | Find the selected text                                                      |
| ⌘L               | Scroll back to live output                                                  |
| ⌘↑ / ⌘↓          | Jump to the previous / next shell prompt (needs OSC 133 prompt marks)       |
| ⌘C / ⌘V          | Copy the selection / paste (a copied file or image pastes as its path)      |
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
Drag to select; double-click selects a word, triple-click a line, Option-drag
a rectangle, and Shift-drag selects even in a program that takes the mouse.
A drag can leave the pane: held above or below it, the terminal scrolls and
keeps selecting, faster the farther out the pointer is. The selection stays
until you click or type in that pane. Command-C copies it as plain text, soft
wraps joined and trailing blanks trimmed (a selection over 1 MiB says so;
**Open Scrollback as a File** takes everything). Right-click, or Shift-right-click
in a program that takes the mouse, for Copy, Paste, Select All and the pane's
actions. A paste goes to the program as a paste: a shell, editor or coding
agent that turns on bracketed paste takes many lines at once and runs none of
them. Only a program that would read the lines as typed keys, where each line
break is Enter, asks first. On Linux a selection is also the primary
selection, so a middle click pastes it.
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
terminal that did not end, and why. **Close Pane** (⌘W, pane menu) ends its
process for every client; **Close Tab** (⇧⌘W, tab menu or close button) ends
all its processes. Typing `exit` removes the pane too. Closing a window or
moving a pane to another window only detaches its views and keeps work running. New sessions are persistent: they stay, empty, after their last
terminal exits. Organization session layouts and display preferences persist
under `$XDG_STATE_HOME/phux-desktop/sessions/<socket-and-session-key-hash>/`,
so renaming a session does not change its saved tabs or splits. Older servers
retain the name-scoped directory. Existing name-scoped and unscoped layouts
are read-only migration fallbacks; explicit `--layout` paths stay unchanged.
Terminal identity is still checked against the server incarnation.
Unavailable saved panes retain their split positions until they can attach;
process-close requests are refused while disconnected rather than hiding live work. Replacing the daemon retires its old view
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

See [Native agents](NATIVE_AGENTS.md) for conversation controls, ACP launches,
Attached TUI behavior, permissions and provider setup.
