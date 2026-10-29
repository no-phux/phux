# phux on Rune (alpha)

An alpha desktop client built on [Rune](https://github.com/anomalyco/rune), a
native GPU UI framework (wgpu, winit). It sits beside the
[GPUI desktop](../desktop/README.md) as a candidate replacement and uses the
same persistent server and shared client runtime. Nothing here replaces or
changes the GPUI desktop.

It is an alpha: expect rough edges, and keep using the GPUI desktop or the
terminal UI for daily work.

## Install

Download the newest `rune-v*` prerelease from
[Releases](https://github.com/no-phux/phux/releases?q=rune-v&expanded=true):

- **macOS (Apple silicon)**: `phux-rune-VERSION-macos-arm64.zip`. Unzip and
  move **Phux Rune.app** to Applications. The alpha is ad-hoc signed, not
  notarized: the first time, Control-click the app and choose **Open**.
- **Linux (x86_64)**: `phux-rune-VERSION-linux-x86_64.tar.gz`, then run
  `phux-rune`. Needs Vulkan and `libxkbcommon-x11`.

Check an archive against its `.sha256` before opening it.

The app needs the phux CLI (`curl -fsSL https://phux.sh/install | sh`). It
never installs, replaces, or upgrades the CLI or its server.

## What it does

The app finds `phux` (`--phux`, `PHUX_BIN`, `~/.local/bin`,
`/opt/homebrew/bin`, `/usr/local/bin`, `~/.cargo/bin`, then `PATH`), starts or
reuses the server with `phux server --ensure`, and attaches the most recent
session (or `--session NAME`). It never creates a session on its own.

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
- Closing the window detaches; the server and its programs keep running. The
  window title follows the focused terminal.

| Keys (macOS) | Action |
| --- | --- |
| ⌘K / ⌘P | Command palette |
| ⌘J | Jump to the next terminal that needs you |
| ⌘T / ⌘N | New terminal / new session |
| ⌘D / ⌘⇧D | Split right / down |
| ⌘⌥ arrows | Move between panes |
| ⌘⇧↩ | Zoom the focused pane |
| ⌘W | Close the terminal (asks unless its prompt is idle) |
| ⌘] / ⌘[ | Next / previous terminal |
| ⌘B | Fold the sidebar |
| ⌘C / ⌘V | Copy the selection / paste (bracketed by the server) |
| ⌘= / ⌘- / ⌘0 | Bigger / smaller / default text |
| ⌘↑ / ⌘↓ | Top / bottom of history |
| Drag, double- and triple-click | Select by cell, word, line (Shift selects over mouse-tracking programs) |

On Linux the same chords use Ctrl-Shift (split down is Ctrl-Shift-E);
plain Ctrl chords go to the shell. Ctrl and Alt chords follow the key's
position, so Ctrl-C is ^C on any layout.

A paste with line breaks into a program that has not turned on bracketed
paste would run each line; the window asks first (Return pastes, Esc
cancels).

## Known gaps

- One window; arrangement tabs (groups) other than the active one are not
  shown.
- No settings, themes, or Kitty images; no search in history yet.
- Option types layout characters on macOS (no Option-as-Meta).
- macOS builds are ad-hoc signed; no auto-update.

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

`scripts/package.sh VERSION OUT_DIR` builds the release archive for the
host. Pushing a `rune-vVERSION` tag runs
[`rune-client.yml`](../../.github/workflows/rune-client.yml), which tests,
packages macOS and Linux, and publishes a prerelease.

| Module | Owns |
| --- | --- |
| `launch` | command line, finding the CLI, `phux server --ensure` |
| `desk` | the runtime client and what is on screen; `desk/*`: layout and splits, frames and fit, input, mouse, palette, sessions |
| `activity` | per-terminal agent state, commands and bells, and what needs you |
| `palette` | palette items and fuzzy ranking |
| `grid` | cell metrics and the draw list as Rune elements |
| `keys` | Rune keys as phux key atoms and app shortcuts |
| `ui` | window composition; `ui/*`: sidebar, panes, palette overlay |
