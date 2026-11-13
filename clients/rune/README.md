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

The app needs the phux CLI. When none is found it offers **Install phux**, which runs the
same installer as `curl -fsSL https://phux.sh/install | sh` (CLI in `~/.local/bin`, phux
Server.app on macOS, a PATH line in the shell profile) and then connects.
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
- **Pair a phone.** Settings → **Remote Hosts**, **Pair a Phone** in the
  palette, or the card on onboarding's last step shows a QR and a copyable
  link from `phux pair --json --enroll`, with its expiry counting down and
  **New code**. It says which network the phone uses (tailnet, local
  network or relay). When remote access is off it says what turning it on
  means and waits for **Allow phones to connect** (`--enable`). **Paired
  devices** lists what `phux pair ls` says can connect; **Revoke** runs
  `phux pair revoke` so that device can't connect again.
- **Updates.** Settings → **About & Updates** controls automatic checks,
  local phux management, and the phux release channel. Downloads and server
  handoffs still need a click. [Updates and release channels](./UPDATES.md)
  covers the channels, verification, restart behavior, and the app's own feed.
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
- **Changes.** **Show Changes** in the palette opens what Git says changed
  in the focused terminal's checkout: the branch, each staged, unstaged and
  untracked file, and the picked file's diff. ↑ and ↓ pick a file, `r` or
  **Refresh** reads again, Escape closes. It only reads; nothing is staged
  from here. A diff over 2,000 lines shows its start and says so.
- **Popup.** **Popup…** in the palette runs a command you type in a floating
  terminal, in the focused terminal's folder, until it exits or you press ⌘W.
- **Quick terminal.** Turn it on in Settings › Terminal (or set
  `quick-terminal = "ctrl+backquote"`, any chord, in `[rune]`), and that
  chord drops a terminal over whatever app is in front, without switching to
  this window, and hides it again; so does clicking away. Recording a new
  chord applies at once, and a chord something else holds is refused with
  who holds it, when that's known. It holds the `quick` session on this
  computer, which the sidebar and palette leave out. `quick-terminal-size`
  and `quick-terminal-edge` size it and pick the top or bottom of the
  display. **Move to a Tab** in its palette moves the terminal into this
  window's session. While the chord is on, closing this window keeps the app
  for the chord, and the Dock brings it back
  ([config](../../docs/config.md#rune)).
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
| ⌘] / ⌘[ | Next / previous terminal, any session |
| ⌘B | Fold the sidebar |
| ⌘F / ⌘G / ⌘⇧G / ⌘E | Find / older / newer / the selection |
| ⌘⇧C | Copy mode: vi keys; `v` selects, `y` copies |
| ⌘-click | Open the link under it |
| ⌘C / ⌘V | Copy the selection / paste (bracketed) |
| ⌘= / ⌘- / ⌘0 | Bigger / smaller / default text |
| ⌘← / ⌘→ | Previous / next terminal in the session |
| ⌘↑ / ⌘↓ | Previous / next session |
| ⌘⇧↑ / ⌘⇧↓ | Previous / next prompt |
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

[DEVELOP.md](./DEVELOP.md) covers running against a dev server, the headless
tooling, packaging and the checks.
