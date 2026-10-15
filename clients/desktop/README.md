# phux desktop

Install the GPUI desktop client on Apple-silicon macOS 27 or later:

```sh
curl -fsSL https://phux.sh/install-desktop | sh
```

The installer selects only published `desktop-vX.Y.Z` releases,
verifies the ZIP checksum and app signature, and installs `Phux.app` plus a
`phux-desktop` launcher. It installs the [phux CLI](https://phux.sh/install)
first when none is found, but never replaces an existing CLI or stops its
server.
Run it again to update; pin or roll back to a published version with
`sh -s -- --version X.Y.Z` after the pipe. `--applications-dir` and
`--bin-dir` select alternate destinations. Releases are ad-hoc signed, **not
Apple-notarized**; after verification the installer clears quarantine.

`Phux.app` bundles the `phux-desktop` binary built from `clients/desktop/native`.
There is no JavaScript bundle and no NAPI addon. Launched from the Dock or
Spotlight, the app resolves the installed `phux` CLI
(`~/.local/bin/phux`, `/opt/homebrew/bin/phux`, `/usr/local/bin/phux`, or
`~/.cargo/bin/phux`) and starts or reuses the server. A shell-only `PATH` or
`PHUX_BIN` is not enough for a Dock launch. It opens your Intake, the session
untargeted work lands in; a server without one opens its most recent session.
The app never creates a session on its own. `PHUX_SESSION` names a session
instead and creates it, persistent, when it does not exist; `PHUX_PROFILE` and
`PHUX_SOCKET` pick the server. Startup problems are shown in the window and logged to
`~/Library/Logs/phux-desktop.log`.

The sidebar's **Intake sessions** group contains the designated Intake (unorganized
work) and imported sessions awaiting organization. Select work and choose
**Organize Selected Work…** to move it to a named session, or explicitly
**Leave unorganized (Intake)**. Moving changes membership, never execution.
Imported sessions can also be marked organized without moving their work.
The bell opens **Inbox & notifications**: agent attention alerts in Inbox,
errors and command results in Notifications. Its badge counts unread alerts,
not sessions or messages; an agent alert opens the agent's terminal.

Closing a pane/window or quitting the app detaches; it does not terminate
server-owned shells. A desktop crash is separate from a server crash:
relaunching can reattach while the server remains alive, but this release
does **not** promise process survival after a server crash or machine reboot.
Only **Terminate Terminal Process** and **Kill Session** intentionally end
terminals.

`just desktop-package` builds the app without installing it.
`just desktop-install-app` builds and installs `/Applications/Phux.app`.
Release ownership and qualification are in [Releasing](../../docs/RELEASING.md).

For development, run from this checkout instead:

```sh
just desktop-app
```

That bootstraps the parent-managed GPUI snapshot, builds the debug desktop
executable and `phux` from this tree, starts a server if one is not already
running, and opens the desktop. Set `PHUX_SOCKET` only when you want a server
other than the one this checkout just ensured. `PHUX_PROFILE=<name>` isolates
the whole instance. `PHUX_DESKTOP_BACKGROUND=1` asks the app not to steal focus.

For a disposable demo that cannot reuse the installed server:

```sh
just desktop-demo
```

This builds the CLI and desktop executable from this checkout, then owns a
private foreground server and app. Each run gets a temporary socket, HOME, and
XDG directories; inherited Phux listener/auth settings are cleared. Closing the
app or interrupting the supervisor reaps both children and removes that state.
The demo refuses installed or external artifacts and does not restart a lost
server behind the supervisor.

## Using it

### Command palette

Press **⇧⌘P** for commands or **⌘P** to find a terminal, agent or tab.
The native panel has **Commands** and **Go To** controls, host/session context,
action descriptions, and your effective keyboard shortcuts. Essentials come
first; search prioritizes command names and also matches descriptions and
keywords. In Go To, a leading `>` switches to commands and keeps the rest of
your search.

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

**Settings > Updates** checks releases at launch and every six hours without
blocking terminal input. Turn automatic checks off or use **Check now** at any
time. **Update & restart** downloads and verifies the desktop bundle, replaces
this copy of `Phux.app` with rollback on installation failure, and reopens it.
The titlebar shows the same button when a desktop update is available. Quitting
the desktop detaches; it does not stop your server-owned terminals.

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

The window is a sidebar (agents by urgency, then every session and its panes),
workspace tabs, split panes, and a status bar. Command-key chords belong to the
app; everything else reaches the terminal.

Choose **Top** or **Side** in **Settings > Window > Tab placement**, or the
main menu's **Tab Placement** submenu. The choice is saved with your display
preferences. Side tabs have their own scrollable rail, even with the navigator
hidden; Quick Terminal keeps compact titlebar tabs. Selecting, closing,
double-clicking to rename, and dragging to reorder work in either layout.
The command palette also offers **Tabs on Top** and **Tabs on Side**.

Settings, dialogs, notifications, find controls, and dropdowns use the same
theme-aware glass treatment. **Settings > Appearance > Font family** previews
each face in the searchable dropdown, with bundled fonts pinned first.

The titlebar's host picker shows the machine this window is on — **Local**, or a
registered remote. Picking another machine points this window at it in place: it
releases the current views and opens that host's home session with its saved
layout. No new window is opened. A plain click on a host in the sidebar's
**Hosts** section does the same; its context menu's **Open Window** still opens a
separate window for a host.

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
switches keybind import off (the global hotkey too, from the next launch), and
lists the binds it skipped: key sequences,
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

## Qualification

From the repository root:

```sh
just doctor desktop
just desktop-install
just desktop-check
just desktop-framework-check
```

`desktop-check` is the CI gate: script typecheck, lint, qualification-guard
tests, and the desktop host lock comparison. It does not launch GPUI.
The opt-in GPUI journey regressions run real window close/reopen and reconnect
lifecycles against a disposable server started by a checkout-built CLI:

```sh
cargo build -p phux
PHUX_DESKTOP_JOURNEY_TEST_BIN="$PWD/target/debug/phux" \
  XDG_STATE_HOME="$PWD/clients/desktop/.cache/journey-state" \
  cargo test --locked --manifest-path clients/desktop/native/Cargo.toml \
  --lib app::session::journey_tests -- --ignored
```

`desktop-framework-check` packages `Phux.app` and runs

```sh
bun clients/desktop/scripts/smoke-app.ts --app /absolute/Phux.app --phux /absolute/phux
```

against an isolated same-checkout server. The app must write `presented.png`,
`layout.json`, and `interaction.json` from a drawable-presented frame. The
harness OCRs the PNG and proves the same shell PID survives desktop
SIGKILL/relaunch and SIGTERM/relaunch. A harness-written ok flag is not
success. It does not exercise server crashes, reboot, or restoration of
secondary windows. Missing GPU access is a blocked run.

## Script tooling

From `clients/desktop`, with repository-pinned Bun 1.4.2 and Node 24 on `PATH`:

```sh
export BUN_INSTALL_CACHE_DIR="$PWD/.cache/bun"
bun install --frozen-lockfile
bun run check:tooling
```

Bun runs the scripts. It is not the production host. `check:tooling` formats,
typechecks, and lints those scripts, then runs the vendored rule suites and
the qualification-guard tests. Fix with `bun run format` and
`bun run lint --fix`. Exact tool versions are pinned in `package.json` and
`bun.lock`. A tooling pass is not GPU evidence; see
[desktop verification](../../docs/architecture/desktop-verification.md).

### Boundaries enforced by the gate

- Type-aware unsafe operations, floating and misused promises fail.
- Chained casts, unjustified assertions and module mocks fail via the
  [selected anti-slop rules](tools/oxlint/anti-slop/README.md).
- Script tooling must not import Cockpit or the web clients.
- Negative fixtures are excluded from ordinary lint and run explicitly with
  asserted diagnostics, so an unloaded plugin cannot appear green.
- Qualification tests reject an installed CLI, a production socket, and smoke
  evidence that is only an ok flag.
