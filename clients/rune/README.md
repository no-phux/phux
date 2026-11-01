# phux on Rune (alpha)

An alpha desktop client built on [Rune](https://github.com/anomalyco/rune), a
native GPU UI framework (wgpu, winit). It sits beside the
[GPUI desktop](../desktop/README.md) as a candidate replacement and uses the
same persistent server and shared client runtime. Nothing here replaces or
changes the GPUI desktop.

It is an alpha: expect rough edges, and keep using the GPUI desktop or the
terminal UI for daily work.

## Install

```sh
curl -fsSL https://phux.sh/install | sh -s -- --rune
```

This checks the archive's SHA-256, installs the phux CLI if you have none, and
puts **Phux Rune.app** in Applications, or on Linux the app in
`~/.local/share/phux-rune` with a menu entry and a `phux-rune` command. The
release signature is verified only with `minisign` on your `PATH`; without it
the digest comes from the unsigned feed, and `PHUX_REQUIRE_SIGNATURE=1` refuses
to go on ([Install](../../docs/install.md)). The app's own updates always
require the signature.

Or download the newest `rune-v*` release from
[Releases](https://github.com/no-phux/phux/releases?q=rune-v&expanded=true):

- **macOS (Apple silicon)**: `phux-rune-VERSION-macos-arm64.zip`. Unzip and
  move **Phux Rune.app** to Applications, then open it. From 0.1.0-alpha.3
  the app is Developer ID signed, Apple-notarized and stapled, like the GPUI
  desktop; earlier alphas were ad-hoc signed and Gatekeeper refuses them.
- **Linux (x86_64)**: `phux-rune-VERSION-linux-x86_64.tar.gz`, then run
  `phux-rune`.

Check an archive against its `.sha256` before opening it.

It needs macOS 14 on Apple silicon, or x86_64 Linux with glibc 2.39 (Ubuntu
24.04 or newer), Vulkan and `libxkbcommon-x11`.

**Wayland is not qualified.** The binary includes Wayland support and uses it
when a compositor offers it, but every automated check runs headless or on X11
under Xvfb. Nobody has tested window decorations, the clipboard or input methods
on Wayland; on a compositor with XWayland, unsetting `WAYLAND_DISPLAY` for the
app runs it on X11.

The app needs the phux CLI (`curl -fsSL https://phux.sh/install | sh`).
From 0.1.0-alpha.6 it updates itself; earlier alphas need this one download.

## What it does

The app finds `phux` (`--phux`, `PHUX_BIN`, `~/.local/bin`,
`/opt/homebrew/bin`, `/usr/local/bin`, `~/.cargo/bin`, then `PATH`), starts or
reuses the server with `phux server --ensure`, and attaches the most recent
session (or `--session NAME`). It never creates a session on its own.

- **Machines.** The switcher under the sidebar's title moves the window to
  any machine in the CLI's registry; the choice is remembered, and sessions
  keep running wherever they are. **Add a machine** takes `me@host` or a
  machine from your tailnet and runs `phux host add --install` over the ssh
  access you already have. A window shows one machine at a time. On a new
  computer the local server is empty, so sessions living elsewhere appear
  only once that machine is added.
- **Updates.** Works like the GPUI desktop's
  ([UPDATES.md](../desktop/UPDATES.md)). The app checks at launch and every six
  hours, and a corner card that never takes the keyboard offers the update.
  **Download update** fetches the archive and refuses it unless a release key
  signed its checksum and the new build can draw a frame on this computer; the
  app then installs when you quit, or now with **Restart now**, which reopens
  your session. On Linux the files are replaced as soon as the download is
  verified, so the next launch is the new version however you quit, even if a
  server update is still due. **Update server** updates the
  phux CLI through its own updater and hands the live server off without
  closing sessions; a refused handoff offers a retry, and package-managed
  installs show their own command. Settings has **Check automatically** and
  **Update phux with Phux Rune**. Updates need a copy in a writable place such
  as Applications; development builds, and windows started with `--socket`,
  `--phux`, `PHUX_BIN` or `PHUX_PROFILE`, never touch phux. Update outcomes
  go to `rune-update.log` (see **Diagnostics**).
- **Diagnostics.** Panics, startup and update outcomes are logged on this
  computer, with credentials and your home directory removed: `rune-panic.log`,
  `rune.log` and `rune-update.log` in `~/Library/Logs/Phux`, or
  `$XDG_STATE_HOME/phux-rune` (`~/.local/state/phux-rune`) on Linux. **Copy
  diagnostics** in Settings puts them on the clipboard with the app's state.
  Nothing is ever sent anywhere.
- **Inbox.** ⌘I opens one drawer of everything waiting on you, across every
  session: what agents are asking, with their suggested answers (click, or
  ⌘1–⌘9) and a reply field; actions the server is holding for approval
  (⌘↩ approves, ⌘⌫ denies; plain Return never does); finished agents and
  failed commands. Answers go through the runtime's stale-question gate, so
  a reply can't land on whatever the agent moved on to. The focused pane
  shows its agent's question as a card you can answer with a click.
- **Notifications.** When something needs you and you can't see it, the
  installed app posts a macOS notification once; clicking it goes there, and
  it is withdrawn when answered or seen. The Dock icon counts what's waiting.
- **Tabs.** The session's arrangement groups, shared with the terminal UI.
  ⌘T opens a terminal in a new tab, ⌘1–⌘9 switch, double-click renames. The
  strip appears from two tabs up; a dot marks a tab that needs you, and a
  jump from anywhere lands on the right tab.
- **Sidebar.** Every session and terminal on the server, each terminal with
  what it is doing: a declared agent's name and state (working pulses,
  blocked or asking needs you, done waits until you look), a running
  command, a long command that finished or failed while you were elsewhere,
  and a bell. Session headers count what needs you; ⌘J jumps to the next.
  ⌘B folds the sidebar.
- **Command palette.** ⌘K (or ⌘P) reaches every terminal, session and
  action by fuzzy search.
- **Splits.** The selected session's arrangement is the server-stored split
  tree the terminal UI uses, so both show the same layout. Split, move
  between panes, zoom one, drag dividers; new splits and ratios are written
  back to the server.
- **Terminal.** The runtime's draw list: colours, bold and italic,
  underlines, box and block glyphs, wide characters, cursor (with blink) and
  selection, on device-pixel cells. Each pane follows its own size. Programs
  that track the mouse get it; the wheel scrolls history, or sends arrows in
  full-screen programs. Input methods compose at the cursor.
- **Find.** ⌘F searches the focused pane's loaded history, newest first:
  ↩ and ⌘G walk back, ⇧↩ and ⌘⇧G forward, Aa toggles case, ⌘E searches
  for the selection. The match is selected and scrolled to.
- **Images and files.** Paste an image and it uploads to the server and types
  its path, for agents that read images. Files pasted or dropped on a pane
  type as quoted paths.
- **Links.** Hold ⌘ over a URL (or an OSC 8 hyperlink) to underline it and
  see where it goes in the title bar; ⌘-click opens it. URLs wrapped
  across rows count. Only `http`, `https` and `mailto` open.
- **History.** Scrolled back, a pane shows where it is in its history and a
  pill back to the live tail that counts the lines that arrived meanwhile.
- **Settings.** ⌘, picks a theme (eight built in, or the OS; arrow keys
  preview it live on your terminals), text size, notifications, Option as
  Meta, and copy on select. All of it is remembered between launches. A
  program's own OSC colours beat the theme.
- **Sessions.** Double-click a session to rename it; the palette also renames
  tabs and ends sessions.
- Closing the window detaches; the server and its programs keep running. The
  window title follows the focused terminal.

| Keys (macOS) | Action |
| --- | --- |
| ⌘K / ⌘P | Command palette |
| ⌘J | Jump to the next terminal that needs you |
| ⌘I | Inbox |
| ⌘, | Settings |
| ⌘T / ⌘N | New tab / new session |
| ⌘1–⌘9 | Switch tab (in the inbox: pick a suggested answer) |
| ⌘D / ⌘⇧D | Split right / down |
| ⌘⌥ arrows | Move between panes |
| ⌘⇧↩ | Zoom the focused pane |
| ⌘W | Close the terminal (**Confirm before closing** asks if something is running) |
| ⌘] / ⌘[ | Next / previous terminal |
| ⌘B | Fold the sidebar |
| ⌘F / ⌘G / ⌘⇧G | Find / older match / newer match |
| ⌘E | Find the selected text |
| ⌘-click | Open the link under the pointer |
| ⌘C / ⌘V | Copy the selection / paste (bracketed by the server) |
| ⌘= / ⌘- / ⌘0 | Bigger / smaller / default text |
| ⌘↑ / ⌘↓ | Top / bottom of history |
| Drag, double- and triple-click | Select by cell, word, line (Shift selects over mouse-tracking programs) |

On Linux the same chords use Ctrl-Shift (split down is Ctrl-Shift-E, and
Ctrl-click opens links);
plain Ctrl chords go to the shell. Ctrl and Alt chords follow the key's
position, so Ctrl-C is ^C on any layout.

A paste with line breaks into a program that has not turned on bracketed
paste would run each line; the window asks first (Return pastes, Esc
cancels).

## Known gaps

- One window. Tabs can't be dragged to reorder yet, and the server has no
  call to rename a single terminal.
- No Kitty images, custom themes or font choice. Find covers the history the
  window has loaded, not the server's whole scrollback.
- A question raised through a hook before the window connected has no text
  to show (the server keeps only that one is pending); its card opens the
  terminal instead. Title-sentinel asks always show.
- A URL that fills a row is assumed to continue on the next one (frames
  carry no soft-wrap flag yet); the title bar shows the joined target
  before a ⌘-click.
- No DMG yet; the ZIP is the macOS download.
- Wayland is not qualified; see [Install](#install).
- The update check draws off-screen, so a build that only fails when opening a
  real window can still install.
- On macOS, while a server update is due, quitting leaves the app as it was
  and the download is dropped. **Restart & update server** moves both. Linux
  has already replaced the files by then.

## Develop

Rune is pinned to a `dev` revision in `Cargo.toml`; this crate has its own
workspace and lock so Rune's graph stays out of the rest of phux.

```sh
cd clients/rune
cargo test
cargo run -- --socket /tmp/phux-test/s --session work
```

Point it at a disposable dev server, never the installed one: run a dev
build of `phux` with an explicit short socket under `/tmp`
(`PHUX_PROFILE=rune-test target/debug/phux --socket /tmp/phux-test/s server --ensure`)
and unset every inherited `PHUX_*` variable first.

Rune's headless tooling works on this binary, so changes can be checked
without a window:

```sh
cargo run -- --socket /tmp/phux-test/s --snap out.png --at 3 --size 1100x720
cargo run -- --socket /tmp/phux-test/s --script typing.json --snap out.png --at 5
```

Notifications need the signed bundle (`rune_mac::notify::available`); `cargo
run` can't post, and Settings says so.

`scripts/package.sh VERSION OUT_DIR` builds the release archive for the
host (ad-hoc signed; a local build may link the dev shell's libraries, so
ship only CI builds), and needs `bun` for its notices.
`just rune-check` is the gate, `just rune-deny` checks the lockfile, and both
run as the `rune` lane of `ci`, which also packages and launches the archive
with `scripts/qualify.sh ARCHIVE target/debug/phux`. On a dev machine that
check can fail on what a Nix shell linked; CI's rustup builds are the real
answer. [Releasing](../../docs/releasing.md#phux-rune) covers tags and what to
do about a bad release.

| Module | Owns |
| --- | --- |
| `launch` | command line, finding the CLI, `phux server --ensure` |
| `updates` | the feed, staging and swapping a verified build, the phux CLI |
| `diag` | local logs, the panic hook, and the report Copy diagnostics makes |
| `desk` | the runtime client and what is on screen; `desk/*`: layout and splits, tabs, frames and fit, input, mouse, find, links, palette, sessions, inbox, settings, attachments |
| `links` | OSC 8 hyperlinks and written URLs under a cell |
| `activity` | per-terminal agent state, questions, commands and bells, and what needs you |
| `inbox` | what is waiting on you, most urgent first |
| `alerts`, `dock` | system notifications, their clicks, and the Dock count (`dock` is the crate's only `unsafe`) |
| `theme` | built-in terminal palettes and the chrome drawn from them |
| `palette` | palette items and fuzzy ranking |
| `grid` | cell metrics and the draw list as Rune elements |
| `keys` | Rune keys as phux key atoms and app shortcuts |
| `ui` | window composition; `ui/*`: sidebar, tabs, panes, pane overlays (find, history), inbox, settings, palette |
