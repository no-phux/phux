# phux on Rune

The phux desktop client, built on [Rune](https://github.com/anomalyco/rune), a
native GPU UI framework (wgpu, winit), over the same persistent server and
shared client runtime as the terminal UI. The same app runs a phone form
([MOBILE.md](./MOBILE.md)).

It replaced the GPUI desktop, which was retired after Desktop 0.62.0; its
last source is the `desktop-v0.62.0` tag. It is young: expect rough edges,
and keep the terminal UI at hand.

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

- **macOS (Apple silicon)**: `phux-rune-VERSION-macos-arm64.dmg` (from
  0.1.0; [phux.sh](https://phux.sh)'s **Download for Mac** links it). Open it
  and drag **Phux Rune.app** to Applications. The
  `phux-rune-VERSION-macos-arm64.zip` beside it is the updater's archive:
  unzip it and move the app to Applications. From 0.1.0-alpha.3 the app is
  Developer ID signed, Apple-notarized and stapled; earlier alphas were ad-hoc
  signed and Gatekeeper refuses them.
- **Linux**: `phux-rune-VERSION-linux-x86_64.tar.gz`, or
  `phux-rune-VERSION-linux-aarch64.tar.gz` on arm64, then run `phux-rune`.

Check an archive against its `.sha256` before opening it.

It needs macOS 14 on Apple silicon, or x86_64 or arm64 Linux with glibc 2.39
(Ubuntu 24.04 or newer), Vulkan and `libxkbcommon-x11`.

**Wayland is checked on one compositor.** Every build is opened on a headless
sway and used there: typing (non-ASCII included), paste and copy through the
Wayland clipboard, Chinese composed through an input method (fcitx5 pinyin),
and fractional scaling (1.25 and 1.5). Not checked: other input methods and
dead keys, GNOME and KDE. There is no middle-click paste: a selection does not
set the primary selection. The clipboard uses the data-control
protocol that wlroots compositors, Hyprland and KDE offer. On a compositor
with XWayland, unsetting `WAYLAND_DISPLAY` for the app runs it on X11.

The app needs the phux CLI (`curl -fsSL https://phux.sh/install | sh`).
From 0.1.0-alpha.6 it updates itself; earlier alphas need this one download.

## What it does

The app finds `phux` (`--phux`, `PHUX_BIN`, `~/.local/bin`,
`/opt/homebrew/bin`, `/usr/local/bin`, `~/.cargo/bin`, then `PATH`), starts or
reuses the server with `phux server --ensure`, and attaches the most recent
session (or `--session NAME`). It never creates a session on its own.

- **Getting started.** An installed app's first connection walks through
  five steps in the terminal area: **Welcome**; **Look** (colour scheme,
  sidebar, themes and, on macOS, a Glass slider from solid to Liquid Glass
  behind the window chrome, all applied live); **Agents & permissions** (`phux skill
  setup --json`, and on macOS a sheet that opens each privacy pane with
  `phux server permissions --open SERVICE`, offers the server's app to drag
  in, and polls until it is granted); **Bring your setup**, shown only with
  Claude Code, Codex or OpenCode installed (`phux migrate --json --agent ID`
  opens the agent's pane beside it; key bindings move, themes and fonts
  don't); and **All set**, with three keys from the keymap. Enter
  continues; Escape goes back. A quit resumes at the same step. An older
  phux reads **Update phux to enable this**. **Get Started** in the palette
  or Settings reopens it; runs aimed at a server never open it on their own.
- **Machines.** The switcher under the sidebar's title moves the window to
  any machine in the CLI's registry; the choice is remembered, and sessions
  keep running wherever they are. **Add a machine** takes `me@host` or a
  machine from your tailnet and runs `phux host add --install` over the ssh
  access you already have. A window shows one machine at a time. On a new
  computer the local server is empty, so sessions living elsewhere appear
  only once that machine is added.
- **Updates.** Settings → **About & Updates** controls automatic checks,
  local phux management, and the phux release channel (**Follow installed**,
  **Stable**, or **Next**). Rune uses the existing CLI's updater, not a private
  install; package managers keep ownership. Downloads and server handoffs
  still need a click. [Updates and release channels](./UPDATES.md) covers
  verification, restart behavior, and the app's independent feed.
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
  installed app posts a notification once (macOS, or on Linux through
  `notify-send`); clicking it goes there, and it is withdrawn when answered
  or seen. The Dock icon counts what's waiting.
- **Tabs.** The session's arrangement groups, shared with the terminal UI.
  ⌘T opens a terminal in a new tab, ⌘1–⌘9 switch, double-click renames, and
  a drag reorders them on the server. The strip appears from two tabs up; a
  dot marks a tab that needs you, and a jump from anywhere lands on the
  right tab.
- **Sidebar.** Every session and terminal on the server, each terminal with
  what it is doing: a declared agent's name and state (working pulses,
  blocked or asking needs you, done waits until you look), a running
  command, a long command that finished or failed while you were elsewhere,
  and a bell. Session headers count what needs you; ⌘J jumps to the next.
  ⌘B folds the sidebar.
- **Command palette.** ⌘K (or ⌘P) reaches every terminal, session and
  action by fuzzy search. **Keyboard Shortcuts: Essentials** lists the keys
  worth learning first, each row runnable, with every chord one row further;
  the first time a window connects, a tip offers it. Hovering **+ Terminal**
  or **+ Session** at the foot of the sidebar shows the keys that do the same.
- **Insert Path.** ⌥; (or ⇧⌘I) opens a picker at the cursor that searches
  the focused pane's folder on the machine it runs on. Return types the path at the prompt as one shell word, relative to
  that folder and quoted only when it must be.
- **Splits.** The selected session's arrangement is the server-stored split
  tree the terminal UI uses, so both show the same layout. Each pane floats
  as its own rounded glass card, the one with the keyboard lit in the accent.
  Split, move between panes, zoom one, drag the gutters between cards; new
  splits and ratios are written back to the server.
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
  see where it goes in the title bar; ⌘-click opens it. On the phone form a
  tap on a link opens it, and a drag across one still selects. URLs wrapped
  across rows count. Only `http`, `https` and `mailto` open.
- **History.** Scrolled back, a pane shows where it is in its history and a
  pill back to the live tail that counts the lines that arrived meanwhile.
- **Settings.** The sliders button at the right of the desktop title bar
  opens Settings, even with the sidebar folded. ⌘, (Ctrl-Shift-, on Linux)
  and **Settings** in the command palette open it too. It has grouped
  Appearance, Terminal, Notifications,
  Remote Hosts, Developer, and About & Updates sections, with local navigation
  on desktop and category drilldown on phone. Search Settings matches names,
  descriptions, categories and current values across all sections, including
  useful aliases such as **font**, **zoom**, and **colours**. **Theme**
  opens the existing live-preview palette; Return keeps a choice and Escape
  restores the old theme, both returning to Settings. **Font** steps through
  the system monospace, the bundled JetBrains Mono and two the platform ships;
  `font_family` in Rune's prefs can name any installed family, and one that
  isn't installed shows the system's. Font, text size, notifications,
  Option as Meta (macOS), copy on select, and close confirmation are remembered
  immediately. A `[rune]` table in `config.toml` overrides any of them
  ([config](../../docs/config.md#rune)). Diagnostics stays available in development builds; update errors
  and protocol warnings remain visible during search. A program's own OSC
  colours beat the theme. A soft upper edge and wider lower fade show where
  more settings are hidden. Keyboard-revealed controls stay clear of the fades;
  an edge with no hidden content stays opaque.
- **Themes.** The palette switches themes by name (`nord`) and **Change
  Theme** lists them with each one shown live as the highlight moves:
  Return keeps it, Escape puts the old one back. A Ghostty config on this machine (`theme`,
  `config-file` includes, `palette`, `background`, `foreground`,
  `cursor-color`) is read at launch and offered as the **Ghostty** theme.
  On Omarchy, the **Omarchy** theme
  takes the current Omarchy theme's colours and accent, repaints within
  seconds when you switch Omarchy themes, and is the default until you pick
  another. `just rune-demo` films the themes into
  `clients/rune/demo/themes.mp4`.
- **Sessions.** ⇧⌘S switches session by name, or creates one with the name
  you typed. Double-click a session to rename it; the palette also renames
  tabs and ends sessions.
- **Typing.** What you type shows as typed while the server confirms it,
  never underlined.
- Closing the window detaches; the server and its programs keep running. The
  window title follows the focused terminal.

| Keys (macOS) | Action |
| --- | --- |
| ⌘K / ⌘P | Command palette |
| ⌘J | Jump to the next terminal that needs you |
| ⌘I | Inbox |
| ⌘, | Settings |
| ⌘T / ⌘N | New tab / new session |
| ⌘⇧S, a branch name | New git worktree and its session |
| ⌘1–⌘9 | Switch tab (in the inbox: pick a suggested answer) |
| ⌘D / ⌘⇧D | Split right / down |
| ⌘⌥ arrows | Move between panes |
| ⌘⇧↩ | Zoom the focused pane |
| ⌘W | Close the terminal (**Confirm before closing** asks if something is running) |
| ⌘] / ⌘[ | Next / previous terminal |
| ⌘B | Fold the sidebar |
| ⌘F / ⌘G / ⌘⇧G / ⌘E | Find / older / newer / the selection |
| ⌘⇧C | Copy mode: vi keys; `v` selects, `y` copies |
| ⌘-click | Open the link under it |
| ⌘C / ⌘V | Copy the selection / paste (bracketed) |
| ⌘= / ⌘- / ⌘0 | Bigger / smaller / default text |
| ⌘↑ / ⌘↓ | Top / bottom of history |
| Drag, double- and triple-click | Select by cell, word, line (Shift selects over mouse-tracking programs) |

On Linux the same chords use Ctrl-Shift (split down is Ctrl-Shift-E, copy
mode Ctrl-Shift-X, Ctrl-click opens links); plain Ctrl chords go to the shell. Ctrl and Alt chords follow the key's
position, so Ctrl-C is ^C on any layout.

In a text field, use the native editing keys: ⌘C/V/A on macOS, plain
Ctrl+C/V/A on Linux. Terminal clipboard chords never act on the terminal
behind a focused Find field.

A paste with line breaks into a program without bracketed paste would run
each line; the window asks first (Return pastes, Esc cancels).

## Known gaps

- One window, and the server has no call to rename a single terminal.
- No Kitty images; the only custom themes are Ghostty's and Omarchy's. Find covers the history the
  window has loaded, not the server's whole scrollback.
- A question raised through a hook before the window connected has no text
  to show (the server keeps only that one is pending); its card opens the
  terminal instead. Title-sentinel asks always show.
- A URL that fills a row is assumed to continue on the next one (frames
  carry no soft-wrap flag yet); the title bar shows the joined target
  before a ⌘-click.
- Screen readers get Settings, the buttons and notices, but terminal text,
  the palette's results, tabs and sidebar rows don't read well yet.
- A GIF with no loop extension loops in a typed document instead of
  playing once: `image` reads it as an infinite loop, and Rune trusts it.
- No DMG yet; the ZIP is the macOS download.
- On Wayland there is no middle-click paste, and only sway is checked; see
  [Install](#install).
- The update check draws off-screen, so a build that only fails when opening a
  real window can still install.
- On macOS, while a server update is due, quitting leaves the app as it was
  and the download is dropped. **Restart & update server** moves both. Linux
  has already replaced the files by then.

## Read-only mirrors

The palette opens AccessibilityTree and PixelSurface resources beside
terminals, and can start a page, tree or surface producer.
[MIRRORS.md](./MIRRORS.md) covers live and read-only mirrors and starting a
source.

## Develop

`--phone` opens the phone form, where the phone app is growing ahead of
Rune's phone support: [MOBILE.md](./MOBILE.md) lists what works and what
waits on upstream.

The [native iOS host](./ios/README.md) renders the same phone view inside
UIKit using upstream's embedded Metal host, with system input and layout
guides instead of drawn keys/insets. CI builds simulator and device bundles
and exercises a live terminal in an isolated simulator. This is not yet a
provisioned device/TestFlight release or full native accessibility coverage.

Rune is pinned to a `dev` revision in `Cargo.toml`; this crate has its own
workspace and lock so Rune's graph stays out of the rest of phux. The
[Rune upstream follower](../../scripts/rune-upstream/README.md) moves the pin
twice a day.

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

`--tree out.json` writes the element tree, with the accessibility tree that
screen readers and automation tools see under `"accessibility"`. Every window
exposes that tree to the OS through Rune's
[accessibility](https://github.com/anomalyco/rune/tree/dev/crates/rune#accessibility)
support, which is also how tools drive a real window now that Rune's strobe
bridge is gone. Headless time does not synchronize phux's server workers;
use `--realtime` against a live server so frames keep pace with replies,
and wait for the attached terminal before measuring an interaction. Packaged
desktop and phone qualification uses this wall-clock pacing.

The resource smoke test also reads back a genuine Rune GPU frame containing
PNG and JSON panes together. Export that frame and its native accessibility
tree without starting any server or touching installed phux:

```sh
RUNE_RESOURCE_SNAP="$PWD/target/resource-native.png" \
  cargo test --locked native_png_headless_renderer_smoke -- --nocapture
```

The test checks decoded PNG pixels, native image draw commands, JSON text,
and the Read-only mirror label; `target/resource-native.json` is the tree.

`RUNE_ONBOARDING_SNAP=DIR cargo test --locked every_step_renders` writes
each onboarding step as a PNG under `DIR`.

On macOS, notifications need the signed bundle (`rune_notify::available`);
`cargo run` can't post, and Settings says so. On Linux any build posts while
`notify-send` is on `PATH`, and withdraws through `gdbus`.

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
| `mobile`, `desk/native_input`, `ios` | direct native transport, target-bound UIKit editing, embedded Metal host and simulator qualification; desktop `main` delegates to the shared library |
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
| `onboarding`, `desk/onboarding`, `ui/onboarding` | first-run steps: the CLI's `--json` answers parsed, step state in settings, the view |
| `grid` | cell metrics and the draw list as Rune elements |
| `keys` | Rune keys as phux key atoms and app shortcuts |
| `ui` | window composition; `ui/*`: sidebar, tabs, panes, pane overlays (find, history), inbox, settings, palette |
