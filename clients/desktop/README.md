# Phux Desktop

A native app for Apple silicon macOS 27+ and Linux x86_64/arm64 (X11 or
Wayland, Vulkan). Rust, GPUI, and the shared client runtime; no web view.

```sh
curl -fsSL https://phux.sh/install-desktop | sh
```

That is the phux installer with `--desktop`. It verifies the archive
against the channel manifest and the release signature, installs
`Phux.app` (macOS) or a tarball under `$XDG_DATA_HOME/phux-desktop` with a
`.desktop` entry (Linux), plus a `phux-desktop` launcher, and installs the
phux CLI if none is found. It never replaces an existing CLI or stops its
server. macOS releases are ad-hoc signed, not notarized; the installer
clears quarantine after verifying.

## What it does

Launched from the Dock, the app finds the installed `phux` (`~/.local/bin`,
`/opt/homebrew/bin`, `/usr/local/bin`, `~/.cargo/bin`) and starts or reuses
the server. It opens your intake session, the one untargeted work lands
in, or the most recent session. It never creates a session on its own.

The sidebar lists every session on this machine and on each host
registered with `phux host add`, grouped by repository, by machine, or
flat. One window shows one session; selecting another restores its layout
in place without touching the processes. Connections stay open across host
switches, retaining the last session on each host while only the selected
session is drawn. Agents that need you are marked
on the tab or terminal they are in, and the bell opens an inbox of agent
alerts and held approvals. `⌘P` opens one palette over sessions, actions,
and settings; `>` switches it to commands.

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
it from the titlebar. **Update phux with Desktop** (on by
default) turns the phux half off. Linux desktop updates rerun the installer.

Logs: `~/Library/Logs/phux-desktop.log` on macOS,
`$XDG_STATE_HOME/phux-desktop/desktop.log` on Linux, redacted and rotated.
**Copy Diagnostics** in the palette copies versions, server id, host, and
recent errors, without terminal content.

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

## Development

```sh
just doctor desktop
just desktop-app       # build the app and `phux` from this tree, start a server, open the app
just desktop-demo      # same, against a disposable private server and HOME
just desktop-test      # the native crate's tests
just desktop-lint
just desktop-check     # the CI gate: script typecheck, lint, qualification guards, host lock
just desktop-package   # build the app without installing it
```

`desktop-demo` builds the CLI and desktop executable from this checkout and
owns a private foreground server and app. Each run gets a temporary socket,
HOME and XDG directories; inherited Phux listener/auth settings are cleared.
Closing the app or interrupting the supervisor reaps both children and
removes that state. It refuses installed or external artifacts and does not
restart a lost server behind the supervisor.

`just desktop-source` materializes the pinned, patched GPUI the crate
builds against ([toolchain](./toolchain/README.md)); `desktop-app` runs it
for you. Set `PHUX_SOCKET` only to use a server other than the one the
recipe ensures; `PHUX_PROFILE=<name>` isolates the whole instance. Never
point a development build at the installed server.

The native crate's layout is in [native/README.md](./native/README.md).
The TypeScript under `scripts/` is build, packaging, and smoke tooling, run
with the pinned Bun; it is not part of the shipped app.

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
