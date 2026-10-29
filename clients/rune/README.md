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

- The sidebar lists every session and terminal on the server; click one to
  show it. **New terminal** spawns into the selected session; **New session**
  creates and selects one.
- The terminal paints the runtime's draw list: colours, bold and italic,
  underlines, box and block glyphs, wide characters, the cursor and the
  selection. Cells snap to device pixels.
- The shown terminal follows the window's size. Closing the window detaches;
  the server and its programs keep running.

| Keys (macOS) | Action |
| --- | --- |
| ⌘C / ⌘V | Copy the selection / paste (bracketed by the server) |
| ⌘T | New terminal |
| ⌘N | New session |
| ⌘] / ⌘[ | Next / previous terminal |
| ⌘= / ⌘- / ⌘0 | Bigger / smaller / default text |
| ⌘↑ / ⌘↓ | Top / bottom of history |
| Drag, double- and triple-click | Select by cell, word, line |
| Wheel | Scroll history |

On Linux the same chords use Ctrl-Shift; plain Ctrl chords go to the shell.
Ctrl and Alt chords follow the key's position, so Ctrl-C is ^C on any layout.

A paste with line breaks into a program that has not turned on bracketed
paste would run each line; the window asks first (Return pastes, Esc
cancels).

## Known gaps

- No IME or dead keys yet: characters a key types directly work, including
  Option characters on macOS, but composed input (CJK input methods, ´ then
  e) does not.
- No splits: one terminal at a time.
- No mouse reporting to applications; the wheel scrolls history, also in
  full-screen programs.
- No settings, themes, or Kitty images.
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
| `desk` | the runtime client, the shown terminal, resize, input, selection |
| `grid` | cell metrics and the draw list as Rune elements |
| `keys` | Rune keys as phux key atoms and app shortcuts |
| `ui` | sidebar, terminal area, status line |
