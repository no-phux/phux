# phux desktop

A native app for Apple silicon macOS 26+ and Linux x86_64/arm64 (X11 or
Wayland, Vulkan).

Rust, GPUI, and the shared client runtime; no web view.

```sh
curl -fsSL https://phux.sh/install-desktop | sh
```

That is the phux installer with `--desktop`. It verifies the archive
against the channel manifest and the release signature, installs
`Phux.app` (macOS) or a tarball under `$XDG_DATA_HOME/phux-desktop` with a
`.desktop` entry (Linux), plus a `phux-desktop` launcher. The Mac app
bundles the phux CLI and server. The Linux desktop archive does not; that
installer adds the CLI when none is found. It never replaces an existing
CLI or stops its server. The shell installer also supports older
ad-hoc-signed releases, clearing quarantine only after verifying the
release signature.

### macOS download

Download the **macos-arm64.dmg** from the newest
[Desktop release](https://github.com/no-phux/phux/releases?q=desktop-v&expanded=true),
open it, and drag **Phux.app** onto **Applications**. Eject the image and
open Phux from Applications. Apple silicon and macOS 26.0 or later are
required. The app and DMG are Developer ID signed, Apple-notarized, and
stapled. The app bundles the phux CLI and server.
The ZIP remains available for the installer and automatic updates.

First launch installs the bundled CLI at `~/.local/bin/phux` if none exists,
then starts or reuses the server. If Terminal cannot find `phux`, choose
**Add to PATH** in Phux, then open a new Terminal window. This appends one
line to `~/.zshrc` or `~/.bash_profile`; the same control is in
**Phux Settings → Permissions**. **Retry connection** discovers a CLI installed
while the app is open; explicit `--phux` / `PHUX_BIN` paths stay pinned.

Older releases through 0.44.1 have only an ad-hoc-signed ZIP. Do not disable
Gatekeeper to install a downloaded app.

### Windows

On Windows today, the CLI and terminal UI run in WSL 2, with PowerShell
launchers. The native x64 desktop connects to that same coordinator through
`wsl.exe` and the binary stdio bridge; WSLg and a network listener are not required.
[Install the WSL CLI/server](../../docs/install.md#windows-wsl) first, then
install a Windows-capable desktop release from PowerShell:

```powershell
$installer = Join-Path $env:TEMP 'install-phux-desktop.ps1'
Invoke-WebRequest -UseBasicParsing https://raw.githubusercontent.com/no-phux/phux/main/scripts/install-desktop-windows.ps1 -OutFile $installer
& $installer
```

The installer refuses channels without a Windows artifact. It verifies
release-key-signed checksums, installs per user under `%LOCALAPPDATA%\Phux`,
and adds a Start Menu shortcut and Apps uninstall entry. Signature verification
uses `minisign.exe` on Windows or `minisign` in WSL (`sudo apt-get install minisign`).
It never replaces the WSL CLI or stops its server. Close the app and rerun
the installer to update; `-Channel next` selects preview builds, `-Version VERSION`
pins a stable version, and `-Uninstall` preserves preferences and WSL shells.

`PHUX_WSL_DISTRO` selects a distribution instead of WSL's default.
`--phux` / `PHUX_BIN` and `--socket` name **Linux** executable/socket paths.
Native file drops and clipboard images upload to WSL before their Linux paths
are pasted. Windows preferences and logs stay in `%LOCALAPPDATA%\phux-desktop`.

Windows uses `Ctrl-Shift-P/T/W/F` for palette/new/close/find, `Ctrl-Shift-C/V`
for copy/paste, `Ctrl-Shift-D` / `Ctrl-Alt-D` for splits and `Ctrl-Shift-R`
to reconnect. Plain `Ctrl-C/Z/D` stay with the shell.
The local Changes panel and external Unix developer socket are unavailable;
use terminal Git and the in-app Performance Monitor. Runtime updates are manual:
**Settings → Updates → Manual in WSL** provides a command preserving your selected
distribution and CLI. Desktop never starts a Linux updater it cannot reliably cancel.

## First launch and recovery

| Starting point                                            | Expected behavior                                                                                                                                                                                                                                                                          |
| --------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Fresh Mac, DMG only                                       | Copy to Applications and open. The bundled CLI is installed per-user and the local server starts. If Terminal cannot find `phux`, choose **Add to PATH** in Phux. It appends one line to your shell profile (~/.zshrc for zsh, ~/.bash_profile for bash). Then open a new Terminal window. |
| CLI installed first (including Homebrew)                  | Desktop uses that CLI without replacing it; starts a stopped server or reuses a healthy one.                                                                                                                                                                                               |
| CLI installed while Desktop is open                       | **Retry connection** and update checks rediscover it, even with Finder's minimal `PATH`.                                                                                                                                                                                                   |
| Previously discovered CLI moved or removed                | Automatic discovery runs again; an explicit `--phux` / `PHUX_BIN` override never silently changes.                                                                                                                                                                                         |
| Server stopped                                            | **Retry connection** starts it and reconnects to the same existing session, never recreating a deleted session.                                                                                                                                                                            |
| CLI/server update before the first connection             | Desktop resolves the local socket, starts the server if necessary, and opens its home after the update.                                                                                                                                                                                    |
| Invalid configuration or unwritable CLI install directory | The window shows the actual failure beside **Retry connection**. Repair the named file/permissions, then retry. No config, credentials or sessions are reset.                                                                                                                              |
| Offline first launch                                      | The bundled runtime needs no download. A failed release check does not prevent local terminal use.                                                                                                                                                                                         |

The CLI-only shell installer does not start a daemon by itself. Opening Desktop
or running `phux` starts the local server. Closing Desktop detaches; it does
not stop the server or its programs. No login service or network listener is
silently enabled by a DMG first launch.

## What it does

Launched from the Dock, the app finds the installed `phux` (`~/.local/bin`,
`/opt/homebrew/bin`, `/usr/local/bin`, `~/.cargo/bin`, then `PATH`). The macOS
bundle supplies it when none is installed. Desktop starts or reuses the server. It opens your intake session, the one untargeted work lands
in, or the most recent session. It never creates a session on its own.

The sidebar lists every session on this machine and on each host
registered with `phux host add`, grouped by repository, by machine, or
flat. **Add Machine…** lists the Mac and Linux machines on your Tailscale
network (signing you in to Tailscale first if needed), installs or updates
phux on the one you pick over ssh, and shows each setup step. One window shows one session; selecting another restores its layout
in place without touching the processes. Connections stay open across host
switches, retaining the last session on each host while only the selected
session is drawn. Agents that need you are marked
on the tab or terminal they are in, and the bell opens an inbox of agent
alerts and held approvals. `⌘P` searches work, commands, and individual settings;
`>` switches it to commands. `⌘,` opens a separate Settings window with a
searchable category sidebar and automatically saved controls.

Closing a pane or tab ends its processes and removes their views for every
client. Typing `exit` removes the pane too. Closing a window or moving a pane
to another window only detaches; work keeps running. **Kill Session** asks
first, then ends all its processes and deletes the session.

| Keys             | Action                                                   |
| ---------------- | -------------------------------------------------------- |
| ⌘P, ⇧⌘P          | palette: go to a terminal, agent, or tab; commands       |
| ⌘T, ⌘O           | new terminal in the focused pane's folder; open a folder |
| ⌘D, ⇧⌘D          | split right, split down                                  |
| ⌥⌘D              | another view of the focused terminal                     |
| ⌘W, ⇧⌘W          | close pane, close tab (ends the processes)               |
| ⌥⌘ arrows, ⌘[ ⌘] | focus a direction, cycle panes                           |
| ⇧⌘↩              | zoom                                                     |
| ⌘1-⌘9, ⇧⌘[ ⇧⌘]   | select a tab                                             |
| ⇧⌘\\             | peek live tab miniatures without resizing terminals      |
| ⌘F, ⌘G, ⇧⌘G, ⌘E  | find, next, previous, find selection                     |
| ⌘↑ ⌘↓            | previous / next shell prompt (needs OSC 133 marks)       |
| ⇧⌘A              | jump to the agent that most needs you                    |
| ⌘B, ⌘,           | sidebar, settings                                        |
| ⌘N, ⌥⌘N          | new window; move the focused pane to its own window      |
| ⇧⌘I              | insert a path from the focused terminal's host           |
| ⇧⌘R              | reconnect, restarting the server if it stopped           |
| ⌘-click          | open the link under the pointer                          |

On Linux, Super replaces ⌘; `Ctrl-Shift-P/T/W/F` also work, and
`Ctrl-Shift-C/V` copy and paste without intercepting the shell's `Ctrl-C`.

Programs such as Claude Code can copy text with OSC 52. Live clipboard
writes from the terminal with keyboard focus in the active window go to
this computer's clipboard, including when the terminal runs on a remote
host. Background panes, replayed history and clipboard read requests do
not change or read the clipboard.

If you have a Ghostty config, the first launch adopts fonts, colors, theme,
padding, option-as-alt, and your `keybind` lines where an equivalent command
exists. **Settings > Ghostty** re-applies it and lists the lines it could not
represent.

Dropping a file onto a pane pastes its shell-quoted path; pasting a
clipboard image saves it to a temporary file and pastes that path, which
agents read as an attachment.

**Settings > Updates** checks the desktop and phux releases at launch and
every few hours. A top-right update card offers **Download Desktop**, shows
download progress and verification, then offers **Restart now**. It also updates
the local CLI and hands its live server off when you choose **Restart & update
server**; sessions keep running, and Desktop
never restarts until you choose to. Dismiss the card to keep working and reopen
it from the titlebar. The update reopens your windows on the same hosts and
sessions, with their tabs, splits and focused panes, and brings the foreground
window back to the front. Explicit launch targets take priority over this
one-time restoration. **Update phux with Desktop** (on by
default) turns the phux half off. Linux desktop updates rerun the installer.

Logs: `~/Library/Logs/phux-desktop.log` on macOS and
`$XDG_STATE_HOME/phux-desktop/desktop.log` on Linux, redacted and rotated.
**Copy Diagnostics** in the palette copies versions, server id, host, and
recent errors, without terminal content. **Settings > Developer** gathers
these with Developer mode, profiling and traces
([dev mode](USAGE.md#dev-mode)).

## Using it

See the [desktop usage guide](USAGE.md#using-it) for navigation, tab placement,
shortcuts, and settings. Current implementation/consumer routes are in
[Clients](../../docs/clients.md).

### Command palette

See [Command palette](USAGE.md#command-palette).

### Performance monitor

See [Performance monitor](PERFORMANCE.md).

### Updates

See [Desktop updates](UPDATES.md).

### Coming from Ghostty

See [Coming from Ghostty](USAGE.md#coming-from-ghostty).

### Native agents

See [Native agents](USAGE.md#native-agents).

### Agent fleet

See [Agent fleet](AGENT_FLEET.md) for the opt-in live overview, navigation,
and animation controls.

### Backdrop shaders

See [Backdrop shaders](SHADERS.md) for the live WGSL backdrops that follow your
theme and your agents, and for writing your own.

## Development

```sh
just doctor desktop
just desktop-app       # build the app and `phux` from this tree, start a server, open the app
just desktop-demo      # same, against a disposable private server and HOME
just desktop-test      # the native crate's tests
just desktop-bootstrap-test # real CLI/server first-launch and retry journeys (also in CI)
just desktop-lint
just desktop-check     # the CI gate: script typecheck, lint, qualification guards, host lock
just desktop-package   # build the app without installing it
just desktop-dmg       # local ad-hoc DMG; not a notarized public release
```

`desktop-demo` builds the CLI and desktop executable from this checkout and
owns a private foreground server and app. Each run gets a temporary socket,
HOME and XDG directories; inherited phux listener/auth settings are cleared.
Closing the app or interrupting the supervisor reaps both children and
removes that state. It refuses installed or external artifacts and does not
restart a lost server behind the supervisor.

To film the app for a demo, launch it with `PHUX_DESKTOP_FILM=DIR`. It
writes its window to `DIR` ten times a second as PNGs named by milliseconds
since launch. These are offscreen renders, so filming needs no
screen-recording permission and works behind other windows. Then
`bun scripts/film.ts DIR out.mp4` (ffmpeg) turns them into a video at their
real timing. A release build keeps up; a debug build drops frames but keeps
the timing.

`just desktop-source` materializes the pinned, patched GPUI the crate
builds against ([toolchain](./toolchain/README.md)); `desktop-app` runs it
for you. Set `PHUX_SOCKET` only to use a server other than the one the
recipe ensures; `PHUX_PROFILE=<name>` isolates the whole instance. Never
point a development build at the installed server.

The native crate's layout is in [native/README.md](./native/README.md).
The TypeScript under `scripts/` is build, packaging, and smoke tooling, run
with the pinned Bun; it is not part of the shipped app.

See [Xcode setup](../../docs/development.md#gpui-desktop) for source builds.
DMG packaging also needs Python 3 (`venv`/`pip`) and Apple's Swift/image tools.
It installs `packaging/dmg-requirements.txt` into a disposable environment;
those dependencies do not ship in the app. [Brand provenance](../../docs/brand-assets.md).

## Qualification

Release builds are qualified before upload. `just desktop-framework-check`
packages the app and runs `scripts/smoke-app.ts` against an isolated server
built from the same checkout: the app must write `presented.png`,
`layout.json`, and `interaction.json` from a presented frame, the PNG is
OCRed, and the same shell PID must survive a desktop `SIGKILL` and
relaunch. On Linux, `scripts/smoke-linux.ts` does the same under Xvfb and
lavapipe. A hosted virtual display never confirms presentation, so a
real-GPU run (`just desktop-native-test` on an Apple silicon Mac with a
graphical login) stays manual.

Two opt-in, slower suites run real window and connection lifecycles
against a disposable server:

```sh
cargo build -p phux
PHUX_DESKTOP_JOURNEY_TEST_BIN="$PWD/target/debug/phux" \
  XDG_STATE_HOME="$PWD/clients/desktop/.cache/journey-state" \
  cargo test --locked --manifest-path clients/desktop/native/Cargo.toml \
  --lib app::session::journey_tests -- --ignored

PHUX_DESKTOP_JOURNEY_TEST_BIN="$PWD/target/debug/phux" \
  cargo test --locked --manifest-path clients/desktop/native/Cargo.toml \
  --lib connection::remote_acceptance -- --ignored
```

The second enrolls a second server as a remote host over loopback and
exercises split, rename, reconnect, a host restart, a stale pin, and a
revoked token. Neither replaces attaching to a real second machine over a
real network after a change to remote attach.
