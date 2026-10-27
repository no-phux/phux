---
audience: humans
stability: evolving
last-reviewed: 2026-10-23
---

# Desktop usage

**TL;DR.** **⌘P** finds work and commands. **⌘,** opens Settings.
For installation and development, see the [desktop README](README.md).

## Using it

### Command palette

Use **⌘P** for work and **⇧⌘P** for actions in a centered palette.
Search, toggle switches, and edit values directly;
[the command palette guide](PALETTE.md) covers controls and navigation.

### Settings

**⌘,** opens a separate, searchable Settings window. Changes save automatically.
See [Settings](SETTINGS.md) for categories, shortcuts, and preference scope.

### Fullscreen

Use **⌃⌘F**, **Toggle Full Screen** in the command palette, or the green
window button to enter or leave native fullscreen. On macOS, the menu bar
and Dock auto-hide so the window fills the display; moving the pointer to
the corresponding screen edge reveals them. Exiting restores the window
through the normal macOS fullscreen transition. **Maximize Window** only
zooms the window within the desktop and is separate from fullscreen.

### Performance monitor

Search the palette for **Performance Monitor**, **CPU**, **memory**, or
**telemetry** to open the live desktop/server monitor without a restart.
The [performance monitor guide](PERFORMANCE.md) covers charts, baselines,
native memory accounting, allocation controls, metric filtering and traces.

### Dev mode

**Settings > Developer** is where to debug the desktop. **Developer mode**
there (or **Toggle Developer Mode**) turns the HUD, heap accounting and dev
socket on or off live in every window, saved in
`$XDG_CONFIG_HOME/phux-desktop/developer.json`. `PHUX_DEV=1` or `--dev` forces
it on for one run. The page also shows the socket path, profiling switches,
trace capture, **Copy diagnostics**, and the log and traces folders.

- **HUD.** The top-right corner of each window shows frame rate, frame build
  time (p50, p99, max), jank (dropped frames while something animates or
  streams), CPU, memory, live and peak heap, allocations per second, and the
  server's CPU and memory, over a sparkline of the last 120 frame builds. The
  sparkline turns amber past an 8.3 ms build and red past 16.7 ms.
  **Toggle Performance HUD** in the palette shows it without Developer mode. The HUD
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
  `0600` and carries no terminal content. Turning Developer mode off closes it.

Start the server with `PHUX_DEV=1` too for its heap figures. A desktop that
starts the server passes its environment on.

### Updates

The corner update card and **Settings > Updates** manage Desktop and the local
server. See [Desktop updates](UPDATES.md) for downloads, progress, explicit
restart, server handoff, release channels, and recovery.

### Workspace

The window is a sidebar (every visible session, its terminal count and
independently expandable contents), workspace tabs and split panes. Exceptions
speak where they apply: a connection or startup chip in the titlebar, and a
**Follow live** pill on a pane scrolled back from its output. The sidebar, the
Changes panel and a session's rows slide open and shut unless **Animations** is
off or the system reduces motion. Command-key chords belong to the app;
other keys reach the terminal unless you bind them through Ghostty or enable
the optional desktop leader.

Tabs can sit on the top, bottom or sidebar. Hover for pane context, or click
the selected tab for live miniatures without resizing your terminals. See
[Tabs and previews](TAB_PEEK.md) for placement, controls and shortcuts.
Sidebar tabs sit directly below the window header, above the session catalog;
both lists scroll independently. Split tabs carry a layered mosaic of their
layout, with the focused pane lit even when the tab is inactive. Agent harness
marks and state pips stay visible in unselected sessions, including on other
machines; selecting work is not required to identify it.

Hosts are context on work, not window owners. The sidebar and Work search keep
all registered hosts' sessions available while the selected session changes.
Click a session's name to select it; only its caret expands or collapses its
contents. The terminal you are viewing is highlighted under it.
Sessions start folded to one line each: name, terminal count, and a
dot while something in it needs you. Option-click a caret to open or fold
every session, or use **Expand All Sessions** and **Collapse All Sessions** in
**Sidebar view** or the palette. Empty sessions sink below the ones holding
work. A machine's name appears on a session only when the same session name
runs on another machine, and terminals sharing a label show their id (`@3`). Switching restores tabs and splits, not the destination's old window
display settings: the current filter, grouping, sidebar width and tab placement
stay put. Selection never opens or folds a session or changes the host filter.
**Settings > Interface > Sidebar machines**, or **Sidebar view >
Visible machines**, controls which machines appear. All are shown by default;
exclusions are saved, do not disconnect hosts, and do not limit Work search.
The sidebar's **All machines / Machines · filtered** control opens that menu;
**Reset** shows every machine again, even when the active host was hidden.
Offline inventory stays visible and cannot be opened until it is refreshed.
A session's **Link to Project…** action files it under a named project, kept
on that session's own server. Projects meet by name across machines: link a
session on the mini and one on this Mac to the same name, or to a
repository's name, and they are listed together in the sidebar's project
grouping and in **Switch Session**. **Unlink** returns it to its repository.
**Settings > Machines** (also **Machines…** from the main menu's
**Switch Session** page, the File menu and the palette) lists This Mac and every saved
machine with its state, opens, retries, disconnects, adds or forgets machines, and
refreshes inventory.
Catalog discovery never attaches terminal streams or starts work; selecting an
empty session keeps it empty. Tabs and splits still belong to the selected
session, not to a new cross-server Workspace record. Every terminal in the
selected session gets a tab without a click: one started elsewhere (an agent,
`phux spawn`) joins the tab list behind the current tab. A terminal moved to
another window, or a capture started in the background, stays out.

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
| ⌥⌘G / ⌥⌘R        | Toggle [Changes](../../docs/desktop-changes.md) / [Reviews](../../docs/desktop-reviews.md) |
| ⌘-click          | Open the link under the pointer (OSC 8 or a URL in the text)                |

### Leader navigation

**Settings > Keyboard & shortcuts > Desktop leader** adds a prefix alongside
the existing shortcuts. It defaults to **Off**; choose **Ctrl+A**, **Ctrl+B**,
**Ctrl+Space**, or enter a custom chord such as `C-a` or `ctrl+alt+a` and press
Enter. This is a desktop preference, independent of your TUI configuration.

Press the leader, release it, then:

| Key after the leader         | Action                                                |
| ---------------------------- | ----------------------------------------------------- |
| `h j k l` or arrows          | Focus left / down / up / right                        |
| `H J K L`                    | Move the pane divider left / down / up / right        |
| `\|` or `%` / `-` or `"`     | Split right / down                                    |
| `o` / `;`                    | Next / previous pane                                  |
| `z`                          | Zoom / restore pane                                   |
| `c`, `n` / `p`               | New tab, next / previous tab                          |
| `Tab` / `Shift+Tab`, `1`–`9` | Cycle tabs, select by position (`9` selects the last) |
| `b`, `w`, `s`                | Toggle sidebar, Go To, session menu                   |
| `Space`, `:` or `?`          | Command palette                                       |
| `,`, `S`, `q`                | Rename tab, Settings, agent needing attention         |

Hesitate for 400 ms to see the shortcut list. **Escape** cancels; pressing
the leader twice sends its literal key to the terminal. An unbound suffix
keeps its normal meaning; only the leader is swallowed. Text fields and
conversations keep their normal input. Changing focus, clicking, or switching
away from the window cancels a pending leader.

### Coming from Ghostty

If you have a Ghostty config (`~/.config/ghostty/config`, or the Application
Support copy), the first launch adopts it: font family and size,
`adjust-cell-width/height` percentages, colours including the 16-colour
palette and a named `theme`, window padding, `unfocused-split-opacity`,
`split-divider-color`, and `macos-option-as-alt`. Settings also exposes
**Appearance > Glassy panes** and **Pane opacity**, which lower the default terminal background opacity over
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
capsule: who runs there, the terminal/conversation switch (or, for an agent
without one yet, **Set up conversation**), then split, zoom and the pane's
menu (double-click the capsule to zoom). A zoomed pane shows the panes it
hides as card edges stacked behind its top; click them to restore the split.
A split tab carries a small map of its panes: the focused one lit, an agent's
in its state's colour, and the ones a zoom hides faded. Drag the gap between
panes, the sidebar edge, or tabs to rearrange; double-click a tab to rename it.
A new split or tab is full size on its first frame. Closed panes fold away;
the PTY is resized once, when that fold settles. Cycling panes, directional
navigation, and revealing a terminal keep the zoom on the pane receiving
keyboard input.
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
selection, so a middle click pastes it. A program's own copy (OSC 52, as
OpenCode, Claude Code, Neovim and tmux send it, also from a remote host)
lands on the clipboard while its pane has keyboard focus in the active
window; a background pane or window never copies.
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
Use **Show details** on a notification to read its complete message without
leaving the list. Clicking a general error opens its full explanation there;
clicking an agent alert still takes you to its terminal. Approval decisions
remain explicit, with their controls below the message.

Agent badges and agent notifications retire when a terminal closes or its
connection is replaced, including reconnects to the same daemon. Native events
and negotiation snapshots are separate reads: if the identity advances during a
drain, labels from that ambiguous batch are omitted until another metadata
update or reconnect rather than attributed to a new connection.

### Native agents

See [Native agents](NATIVE_AGENTS.md) for conversation controls, ACP launches,
Attached TUI behavior, permissions and provider setup.
