# Phux Cockpit

Phux Cockpit is a native macOS spatial runtime for terminal, web, and future
control-plane surfaces. Native Phux terminals use the same bounded tabs and
split-pane substrate as local terminal processes.

The immediate goal is an exceptional native Phux terminal for terminal and
TUI-agent work. Its stable execution and interaction primitives grow into a
native control environment for large-scale directed machine work. See
[Product Direction](docs/PRODUCT_DIRECTION.md).

## Authority boundary

Cockpit is a native command surface and projection client. Phux owns durable
work identity, execution authority, applied Run policy state, ordered evidence,
artifacts, and shared attention state. Cockpit owns product policy, windows,
tabs, splits, focus, input routing, rendering, accessibility, and other
presentation state.

The direct local provider remains an intentionally ephemeral terminal path: its
process and emulator live inside the app and end with it. Durable local work
uses a local Phux coordinator. Cockpit does not maintain a second authoritative
work database, artifact store, or process-owning daemon. See
[Durable Work Architecture](docs/DURABLE_WORK_ARCHITECTURE.md).

## Spatial runtime

- Cockpit launches with **one terminal** and one shell process, up to 32. Each
  has a stable in-process terminal ID and owns its own PTY, emulator, scrollback,
  selection, input queue, and retained-rendering namespace. Tab order and
  visible placement do not own execution.
- **A tab owns a pane tree**, not a terminal. A leaf is a terminal; a branch
  divides its rect between two children along an orientation at a fraction.
  `cmd+D` splits right and `cmd+shift+D` splits down, each creating a NEW
  shell beside the focused pane, nested arbitrarily up to 16 panes per tab.
  Closing a pane promotes its sibling into the parent's rect, so a split never
  collapses into a hole and never pulls in an unrelated tab. Splitting,
  focusing, resizing, and collapsing never restart a process.
- **One geometry.** `resolve()` over that tree is the single source of truth
  for pane rects: the painter, the hit-test widget tree, and the PTY sizing
  pump all consume the same call. They used to be three independent
  derivations, and they had already drifted.
- **Tabs** are native canvas controls with tab accessibility semantics,
  rendered from a real slice with a close affordance and a `+`. A tab is
  titled by its focused pane's shell title (OSC 0/2), then the last component
  of its working directory (OSC 7), then its mint number. Tabs can sit above
  the workspace or in a side rail without changing terminal identity, focus,
  or process state.
- **Web** is a native WebKit surface for the explicitly allowed GitHub,
  Superlogical, and Mitchell Hashimoto top-level origins, reachable by
  `cmd+shift+B` and the View menu. It is deliberately NOT a tab in the
  terminal strip: the strip shows terminals. It keeps its page process alive
  while a terminal is selected. Native bridge commands are disabled; WebKit
  subframes and page resources remain ordinary web content.
- **The workspace persists.** Tab list, pane trees, divider fractions,
  selection, focus, and each terminal's working directory are written to the
  platform state directory on a debounce and restored before the first
  terminal is created. Restoring recreates shells in the saved shape;
  `process_restoration_supported` is `false` and nothing pretends otherwise.
- **Phux terminals** published by a coordinator enter the same bounded tab
  topology as local ones. One that appears becomes a tab; it takes a visible
  pane when you select it, never by displacing a live local terminal. `cmd+W`
  closes local terminals only — a phux terminal's lifetime is not Cockpit's to
  end. Cockpit still does not run the phux TUI.

## Chrome that costs nothing

At rest Cockpit is a terminal, not an application frame around one. A single
healthy terminal gets the whole content area: no tab strip, no toolbar, no
status banner.

**Tabs live in the titlebar.** A `hidden_inset_tall` window is already carrying
~66pt of band that holds three traffic lights and nothing else, and its height
does not depend on how many tabs exist — so the strip drawn inside it takes no
content height in any state. This is the difference between chrome that is
cheap to reveal and chrome there is nothing to reveal: `content.y` does not
move, so no terminal is resized and no TUI redraws. The old separate 50pt band
cost every terminal in the tab three rows and a `SIGWINCH` on the way in, and
another on the way out. It survives only for a window the platform gave no
titlebar — fullscreen — where the room genuinely has to come from somewhere.

The strip appears when the workspace has structure to show: a second tab, the
Web surface, or a terminal that needs attention. A split alone does not raise
it — two panes in one tab are still one tab, and the divider says so. Attention
is *unacknowledged* state, not cumulative: looking at a terminal clears it, the
way a bell already worked. The loss counters themselves stay cumulative, because
they are the evidence in each surface's accessibility label, and a diagnostic
that resets is a diagnostic that lies.

**`cmd+shift+P` summons a switcher** that floats over the grid instead of taking
room from it — type to filter by shell title, working directory, or position,
`enter` to go. It is the path that still works at thirty terminals, where a
strip has to start windowing and shrinking pills, and it is the seam this app
grows through toward directing many machines at once.

Tabs can sit in a side rail instead (`tab-placement = side`, or View > Toggle
Tab Placement) without changing terminal identity, focus, or process state. The
rail moves the cost onto the columns axis, which reflows far more cheaply than
rows.

The band is presentation, never the only path: every shortcut in the table below
reaches the model whether or not it is showing, and the menu bar carries the
same commands. The window stays draggable by its titlebar inset in every state.

All terminal executions stay live while hidden or reordered. libghostty-vt owns
terminal state, and native-sdk paints each visible pane as one packed
`cell_grid` command per row — a lattice of 20-byte cells the renderer expands,
rather than per-run rectangles and text. That is what lets a dense screen paint
in full: a distinct foreground and background on every cell merges nothing, and
the old per-run emission wanted roughly 24,000 commands for a 200x60 screen
against a 2,048 ceiling, truncating from the bottom without saying so. Where a
budget does genuinely bind, the painter reports the loss instead of quietly
dropping rows.

Terminal tabs show an attention dot when a hidden process rings the bell or
develops an operational issue, and an exception is itself enough to bring the
strip back when a lone terminal is in trouble. Splits carry no pane header at
all: the focused pane wears a hairline accent edge, the others are dimmed by a
scrim, and that is the whole indication. The scrim dims toward **black**, not
toward the window's own ground — painting the ground over panes that already
carry it composited a colour over itself, which changes nothing at any alpha,
so the dim used to draw literally nothing and a four-way split was told apart
only by a solid-versus-hollow cursor. The accent edge is what survives the case
the scrim still cannot serve: a terminal configured black, or put there by an
application's OSC 11, has no luminance left to take away.

Byte counts, I/O-loss badges and lifecycle strings are not product chrome — they
live in each surface's accessibility label, so a screen reader keeps every
detail the eye is spared.

**A shell that ends closes its pane, at any exit status**, and its sibling
reclaims the rect. Exit code is the child's answer about the last command it
ran, not a claim about whether the pane is still wanted — and `exit` inherits
that status, so gating the close on it left an ordinary session ended by an
ordinary failed command sitting behind a permanent `EXIT 1` husk that never gave
the space back. The one end that still leaves a pane standing is a spawn that
never produced a process: there is nothing to close to, and a split that
vanished on its own would read as a broken `cmd+D`. That pane says so where it
is, with its **Restart**.

The local provider is a bounded dynamic registry independent from layout. Close
frees the emulator immediately rather than holding a registry slot against a pty
exit that may never arrive, discards post-close output and generated terminal
replies, and never reuses PTY keys in-process, so stale traffic cannot cross
into a replacement terminal. Closing the last pane closes its tab, and closing
the last tab closes the window *and quits* — AppKit keeps a process alive after
its last window unless asked not to, and a running cockpit with no window is not
a closed one.

`Model.topologySnapshot()` exposes the versioned persistence boundary for the
tab list, each tab's pane tree, selection, focus, divider fractions, and
per-terminal working directories. `restoreModel()` validates or migrates that
snapshot and creates new emulator sessions and shell processes. The snapshot
intentionally contains no PID, PTY key, screen memory, or process-survival
claim. It is stored as a flat line-oriented file with an explicit terminator,
chosen over a nested format because it is read at startup from a user-editable
path a crash may have half-written, and a grammar with no nesting cannot
overflow a stack however the bytes fall. See
[Topology Snapshots](docs/TOPOLOGY_SNAPSHOTS.md).

## Configuration

Cockpit reads `~/.config/phux-cockpit/config` (or `$XDG_CONFIG_HOME/…`), falling
back to the macOS platform config directory. The dotfile path is searched first
on purpose: the people most likely to write one are arriving from Ghostty and
will not go looking in `~/Library/Preferences`. `PHUX_COCKPIT_CONFIG` overrides
both.

The syntax is Ghostty's — one `key = value` per line, `#` starts a whole-line
comment, and there are deliberately no trailing comments because `#` is also how
every colour begins. An unknown key or a malformed value is a diagnostic, not a
failure: one bad line costs that line, never the rest of the file. A config that
produced any diagnostic raises **one line of chrome above the terminal naming
the line numbers**, dismissed by clicking it; the startup log carries the same
diagnostics in full, with the offending key or value quoted. The band takes no
keyboard and holds no chord — a setting that did nothing is not a reason to
stand between you and a prompt.

```
theme = nord
font-size = 14
cursor-style = bar
cursor-style-blink = false
background = #090b0f
foreground = #f4f7fb
palette = 1 = #f38ba8
minimum-contrast = 3
scrollback-limit = 50000000
shell = /opt/homebrew/bin/fish
inherit-working-directory = true
tab-placement = top
```

### Minimum contrast

`minimum-contrast` is a WCAG contrast ratio, 1 to 21, that every cell's text
must clear against the background it is painted on. A cell that falls short is
repainted in pure white or pure black — whichever reads better on that
background. **`minimum-contrast = 1` turns it off.**

This is Ghostty's key, with Ghostty's range and Ghostty's algorithm, and one
difference: Ghostty defaults it to 1 and Cockpit defaults it to **3**. On
Cockpit's ground (`#090b0f`) the terminal's own ANSI black measures 1.19:1 and
a faint blue 2.60:1 — both are faithful VT output and neither is text you can
read. 3 lifts exactly those and leaves everything above them alone: ANSI bright
black (3.43:1) stays the grey a prompt meant it to be, and so does plain faint
(5.03:1). Raising it to the AA threshold of 4.5 would turn that grey the same
pure white as the text it was de-emphasising, which is why the default is not
4.5.

Box-drawing, block, Legacy Computing and Powerline glyphs are exempt, matching
Ghostty: those are shapes drawn in a foreground colour on purpose, and a
Powerline separator raised to white is a white wedge through your prompt.

### Themes and the settings surface

`theme = <name>` names one of the built-in sets — `phux-dark`, `phux-light`,
`high-contrast`, `nord`, `gruvbox-dark`, `solarized-dark`. `theme = auto`
**follows the system** instead of naming one: `phux-dark` while macOS is in
dark mode, `phux-light` while it is in light, re-adopted the moment you flip
the switch. The pair is deliberately those two and not a mix of themes — they
are the same register inverted, so crossing sunset changes brightness rather
than identity. Picking a theme in the settings surface ENDS the subscription:
the panel writes the name it chose, so the file stops saying `auto` at exactly
the moment the app stops following. A theme sets
`background`, `foreground` and `selection-background`; an explicit key for any
of those **outranks** it, wherever the two lines happen to sit in the file. It
deliberately leaves the ANSI-16 palette alone, so a terminal red stays a
terminal red — see [Decisions](docs/DECISIONS.md).

**`cmd+,`** (or **View > Settings…**) opens the settings surface. Up and down
preview a theme LIVE against whatever is on screen, `return` saves the choice
into your config file, and `esc` puts back the one you had. The panel writes
only the `theme` line: your comments, your spacing, and any key this build has
never heard of are copied through untouched.

The panel also shows the **contrast ratio** between the foreground and
background actually being painted, and flags it when it drops below the WCAG AA
minimum for body text (4.5:1). That readout is there so "I can't read my
terminal" is something you can see the answer to rather than something that
needs investigating. Every built-in theme clears the threshold; a test keeps it
that way.

The panel's own colours are fixed and never follow the theme. A settings page
you cannot read when the theme is broken is worse than none.

`shell` (or `command`) is a command LINE, not just a path, so `command = tmux
attach` keeps its argument. It runs via the login shell with `exec`, so the
program you name is the pty's own process. A value that is empty, over-long, or
carries a NUL is refused and the built-in shell stands.

`font-family` and `selection-foreground` are parsed but **cannot** be applied in
this build: the SDK selects faces from a fixed registered set rather than by
family name, and a terminal grid carries one selection colour rather than a
foreground override. Setting either raises the notice above, and logs a line,
saying so.

## Install

Install with Homebrew:

```sh
brew install --cask phall1/tap/phux-cockpit
```

The cask places **Phux Cockpit** in Applications. Releases without configured
Developer ID credentials are ad-hoc signed; the cask clears the quarantine
attribute and reports that fact in its caveat.

## Keybindings

| Key | Action |
|---|---|
| `cmd+1` ... `cmd+5` | Select the tab at that position |
| `cmd+T` | Create and select a tab, up to 32 terminals |
| `cmd+W` | Close the focused pane, then its tab, then the window |
| `cmd+shift+left` / `cmd+shift+right` | Move the selected tab |
| `cmd+shift+[` / `cmd+shift+]` | Select previous or next tab |
| `cmd+D` | Split right, creating a new shell beside the focused pane |
| `cmd+shift+D` | Split down, creating a new shell below the focused pane |
| `cmd+[` / `cmd+]` | Focus previous or next pane |
| `cmd+option+arrows` | Move keyboard focus to the pane in that direction |
| `cmd+=` / `cmd+-` / `cmd+0` | Increase, decrease, or reset the terminal font size |
| `cmd+A` | Select the whole scrollback |
| `cmd+click` | Open the URL under the pointer |
| `cmd+K` | Clear the screen and scrollback |
| `cmd+shift+P` | Go to terminal — the summoned switcher (type to filter, arrows or `ctrl+N`/`ctrl+P` to move, `enter` to go, `esc` to dismiss) |
| `cmd+,` | Settings — themes with a live preview and a WCAG contrast readout (arrows or `ctrl+N`/`ctrl+P` to preview, `return` to save, `esc` to cancel) |
| `cmd+shift+B` | Show the Web surface |
| `cmd+shift+space` | Enter or leave keyboard selection mode |
| Arrow keys | Move the selection caret |
| `shift` + arrow keys | Extend the selection |
| `B` | Toggle block selection while selecting |
| `enter` | Copy and leave selection mode |
| `esc` | Cancel selection mode |
| `cmd+C` | Copy the active selection |
| `cmd+V` | Safely paste the system clipboard into the selected terminal |
| `cmd+arrow-up` / `cmd+arrow-down` | Scroll one history line (`shift` scrolls a page) |
| `cmd+home` / `cmd+end` | Jump to the top or bottom of history |
| `cmd+R` | Restart the focused terminal after its process exits |
| `cmd+M` | Minimize the focused window |
| Pinch | Size the terminal type, the way `cmd+=` and `cmd+-` do |

Clicking a tab switches surfaces without stopping hidden execution. Clicking a
split pane moves input ownership to it. The divider supports pointer dragging,
arrow-key adjustment, Home, and End. Trackpad and wheel input route only to the
terminal under the pointer. Dragging a selection beyond the top or bottom edge
autoscrolls through history. Right-click or control-click opens native Copy and
Paste actions while Cockpit owns pointer selection or the process has ended.
While a live TUI enables mouse reporting, it exclusively owns secondary click,
so the native menu is intentionally unavailable; Shift-drag selection remains
copyable with `cmd+C`. `cmd+click` opens a URL under the pointer, and works
even while a TUI owns mouse reporting — a program that prints links should not
have to give up mouse input for them to be clickable. It is deliberately a
heuristic that fails toward "not a link": only `http`, `https` and `mailto` are
recognised, so a printed `file:` or `javascript:` path is never something the
OS can be asked to open. A `cmd+click` on ordinary text is an ordinary click. A copied range remains highlighted until typing or
another selection clears it.

**Tabs drag.** Pick one up and carry it along the strip: the reorder happens as
the pointer moves, so the tab under the cursor is the tab that will be there
when you let go — there is no landing animation to disagree with. Escape puts it
back where you picked it up. A click still selects; only a gesture past the
runtime's own drag slop reorders. The menu command and `cmd+shift+arrow` are
unchanged.

**Right-clicking a tab** opens its own menu — New Terminal, Move Left, Move
Right, Close, Close Others. Every verb acts on the tab under the pointer rather
than on the selected one, which is the whole reason the menu exists; the ends of
the strip disable Move rather than hiding it, so the menu never changes shape.

**Dropping files** from Finder onto a pane types their paths into that pane's
shell — quoted, space-separated, and delivered through the same bracketed-paste
encoder `cmd+V` uses, so a filename containing a newline arrives as data rather
than as a command. The pointer decides the pane, and focus follows the drop.

**A bell that rings while Cockpit is in the background posts a notification**
naming the terminal. In the foreground it stays a dot in the tab strip: a banner
for the pane you are typing in is how notifications get turned off wholesale.

**The menu-bar extra** (`PX`) carries the open terminal count, turns its title
warning-toned when one of them wants something, and lists every terminal in the
active window with a row that goes straight to it — raising the window on the
way, since a menu-bar pick happens while Cockpit is behind whatever you were
actually looking at.

## Requirements

- Apple silicon Mac running macOS 11 or later
- Zig 0.16.0 and Xcode Command Line Tools for source builds
- Internet access on the first source build to fetch pinned dependencies

native-sdk is pinned to
[`phall1/native@34cc9d55`](https://github.com/phall1/native/commit/34cc9d5571599d5ea4feafc9260f36575e67e77b),
the fork's upstream-v0.9.5 Cockpit lineage: terminal interaction, viewport, and
font seams, the packed `cell_grid` canvas command with its AppKit decoder and
wire format v7, macOS glyph smoothing, bounded cell-grid draw-resource caching,
per-window `ChromeContext` on `build_window` and `web_panes`, `fx.openUrl`, the
`native_extension` hook that keeps the TypeScript-core graph's engine native,
axis-aware native split dividers, scoped post-present display-list refresh
batching, and symlink-safe whole-file writes that retain file-access
confinement.
The pin is a tarball SHA rather than a branch, so a push to the fork can never
break a checkout of Cockpit — see [docs/SDK_PIN.md](docs/SDK_PIN.md) for how the
fork and this repo stay in contract, and what to run before moving the pin.
libghostty-vt is pinned
to Ghostty commit `7aa9591746ffa4d2eee458960c76554352832595`, the existing
Zig 0.16-compatible checkpoint.

## Phux FFI provenance

Production packages compile and statically link `phux-client-ffi` from the
same Phux checkout that contains `clients/cockpit`. The C ABI remains the
architectural seam; repository composition no longer requires a second clone
or a cross-repository commit lock. `./scripts/verify-phux-ffi.py` derives the
Git commit, workspace version, ABI version, and Cargo profile from that checkout
and writes those exact values into the packaged provenance resource.

## Build and test

```sh
./scripts/dev-run.sh     # build this checkout and RUN it, beside your installed app
zig build
zig build test -Dplatform=null --summary all
```

`./scripts/dev-run.sh` is the one to reach for when the question is "does my
change look right in the actual app". `zig build run` starts a bare binary that
shares the installed app's identity and state file; the script does not. See
[Running a local build beside the installed app](#running-a-local-build-beside-the-installed-app).

### Reading the result of `zig build test`

Two things have to be true for a test run to mean anything: it has to have
passed, and it has to have compiled the code you changed. `zig build test`
now answers both, in that order.

**Did it pass?** The exit code, and only the exit code.

```sh
zig build test > /tmp/t.log 2>&1; echo "exit=$?"
```

Do not judge from a log line. A green run used to end with

```
failed command: ./.zig-cache/o/<hash>/test --cache-dir=./.zig-cache ... --listen=-
```

which read as a failure and was not one. That line came from tests calling
`std.debug.print` on the happy path: Zig 0.16's build runner prints a
step-failure report — including `failed command:` — for any step whose captured
stderr is non-empty, whether or not the step failed. Those prints are now behind
`-Dmeasure=true` (see `src/tests/measured.zig`), so a passing run is quiet.

**What did it compile?** The last thing the run prints is a verdict:

```
------------------------------------------------------------------
zig build test: PASS
  phux provider: COMPILED AND TESTED (transport, host, provider, pointer)
  source root:   /Users/you/workspace/phux/clients/cockpit
  phux provider: COMPILED AND TESTED (transport, host, provider, pointer, extension)
    ffi include: /Users/you/workspace/phux/crates/phux-client-ffi/include
    ffi lib:     /Users/you/workspace/phux/target/ffi-release
    found via:   Phux monorepo checkout
  app graph:     local terminal provider (-Dphux-enabled defaults to false)
------------------------------------------------------------------
```

The verdict is a build step that depends on every test step, so it prints only
when all of them succeeded. **No verdict means the run was not green.**

The named modules are the ones rooted as test artifacts. `extension.zig` is
compiled but not rooted: its tests have never run anywhere, and rooting them
hangs the build. See phux-cockpit-iwf and the comment on
`phux_test_module_names` in `build.zig`.

`src/providers/phux/` needs the phux client FFI, so `zig build test` looks for
`phux/client.h` and `libphux_client_ffi.a` in three places, in order:

1. `-Dphux-client-ffi-include-dir` / `-Dphux-client-ffi-lib-dir`
2. `$PHUX_CLIENT_FFI_INCLUDE_DIR` / `$PHUX_CLIENT_FFI_LIB_DIR`
3. the enclosing Phux checkout: `../../crates/phux-client-ffi/include` and
   `../../target/ffi-release`

If it finds them, the provider is compiled and its tests run — regardless of
`-Dphux-enabled`. If it does not, everything else still runs and passes, and the
verdict says `PASS, INCOMPLETE` and names what was left out. Without that line, a
signature change to `PhuxProvider.search` or `Host.search` passes a local run
without ever being compiled.

Being in the build graph is not the same as being compiled: Zig analyzes only
what is referenced, and nothing in Cockpit calls `PhuxProvider.search`. Each
rooted module therefore ends with

```zig
test "every declaration in this module is compiled, not merely reachable" {
    @import("phux_ref").refAllDeclsRecursive(@This());
}
```

which is what makes the verdict's "COMPILED" claim true.

From the Phux repository root, build the FFI and run the complete Cockpit gate:

```sh
just cockpit-test
```

`-Dphux-enabled=true` adds the Phux provider to the **app** graph. It requires
the FFI and fails loudly rather than building an app with no Phux path:

```sh
zig build test -Dphux-enabled=true
```

At launch, the Phux provider attaches the running server's current session. It
never creates a server session as a startup side effect. `PHUX_SESSION=name`
selects an existing session by name; the terminal switcher then exposes the
server's complete attached-session catalog by stable server ID. If no server
session can be attached, Cockpit keeps the direct local terminal usable, marks
it as ephemeral with `PHUX OFFLINE`, and offers one explicit retry after a Phux
session is started or created.

Tabs start at the top by default. Set `PHUX_COCKPIT_TABS=side` (or `sidebar`)
to start with the side rail; the in-app placement control switches the current
workspace without restarting or resizing through intermediate states.

The application is macOS-only. The null platform is used only for deterministic
tests; it does not exercise Metal presentation.

### Running a local build beside the installed app

```sh
./scripts/dev-run.sh
```

One command: package this checkout, launch it, and leave no way to mistake it
for `/Applications/Phux Cockpit.app`. Ctrl-C quits it.

This exists because of a specific, expensive failure. Three days of rendering
bug reports were filed against the installed app — 0.7.1, built 2026-08-09 —
while `main` was 0.8.0. Nothing merged in those three days had ever been in
front of the person reporting the bugs. Both apps are called "Phux Cockpit",
both put the same icon in the same Dock, and a plain `zig build` binary is the
installed app's identical twin to macOS.

The dev run differs in four ways, each closing a different hole:

| | installed | `dev-run.sh` |
|---|---|---|
| bundle id | `dev.phux.cockpit` | `dev.phux.cockpit.dev` |
| process name | `phux-cockpit` | `phux-cockpit-dev` |
| Dock and menu | Phux Cockpit | Phux Cockpit (dev) |
| config | `~/.config/phux-cockpit/config` | `.dev-run/config` |
| workspace layout | `~/Library/Application Support/…/workspace.state` | `.dev-run/dev.phux.cockpit/workspace.state` |
| automation dropbox | the CWD you launched from | `.dev-run/.zig-cache/…` |

The process name is the one that matters to tooling rather than to you.
`pgrep -x phux-cockpit` and System Events' `process "phux-cockpit"` both target
by NAME, so two instances sharing it means any automation script may drive the
wrong app and never say so (phux-cockpit-2ml.10).

Useful flags — `--debug` for a faster Debug build, `--automation` to build with
`-Dautomation=true` and print how to drive it, `--config PATH` to use a specific
config file, `--fresh` for a clean dev home, `--detach` to launch and exit.
`./scripts/dev-run.sh --help` lists them all.

**Your config is not shared with the dev run,** which reverses what this section
used to say. The settings surface writes a theme choice back into whatever
config file the app was given, so a dev build pointed at your real config is a
dev build that can edit it — the same class of clash as the shared layout file,
one step further from being noticed. `.dev-run/config` starts empty; copy what
you want into it, or pass `--config ~/.config/phux-cockpit/config` if you would
rather have your own settings and accept that a dev build can rewrite them.

A packaged build and a Debug build still write different state file names
(`workspace.state` vs `workspace-dev.state`, keyed off the optimize mode in
`src/cockpit/session_state.zig`). That split predates this script and still
protects a plain `zig build run`; `dev-run.sh` does not rely on it.

To prove any of the above rather than believe it:

```sh
./scripts/dev-isolation-check.sh
```

It runs the installed app and a dev build at the same time and asserts each
process name resolves to exactly the pid it launched, that LaunchServices
registers two different bundle ids, that System Events can name each instance,
and that a config read and a layout write land in the dev home and not in
yours. Every assertion has a negative control — the identity comparison must
first report a clash for the *unstaged* bundle, and the isolation arm is paired
with a run that removes only `PHUX_COCKPIT_CONFIG`/`PHUX_COCKPIT_STATE` and
must then reach the "real" config. It is serial-only: quit any running Cockpit
first.

Two paths are still shared, and the check does not claim otherwise:
`~/Library/Application Support/dev.phux.cockpit/State/windows.zon` and
`~/Library/Logs/dev.phux.cockpit/native-sdk.jsonl`. The SDK keys those off the
bundle id compiled in from `app.zon`, which the plist rewrite cannot reach.
Nothing reads `windows.zon` back — `restore_state` is `false` — so the cost
today is a shared log file.

Without the script, the seams are ordinary environment variables. Both name a
FILE, and both win over every search path, for writes as well as reads:

```sh
PHUX_COCKPIT_STATE=/tmp/cockpit-scratch.state \
PHUX_COCKPIT_CONFIG=/tmp/cockpit-scratch-config \
  ./zig-out/bin/phux-cockpit
```

Leaving them unset is not a harmless default: the config lookup reads
`$XDG_CONFIG_HOME` before `$HOME`, so a run "isolated" by overriding `HOME`
still finds your real config file.

Add `-Dautomation=true` to drive the running app: commands are written as
`command-<n>.txt` files into `.zig-cache/native-sdk-automation/` **relative to
the app's working directory**, and screenshots and a full widget snapshot
appear beside them. That path has no environment override, so two instances
launched from the same directory share one dropbox — which is why `dev-run.sh`
launches from the dev home.

## Package

Create an arm64 app, ZIP, DMG, and `SHA256SUMS` under `zig-out/release`:

```sh
cd ../..
just cockpit-ffi
clients/cockpit/scripts/package-macos.sh
```

Release, CI, and local packaging all require the source and output paths to
belong to this same Phux checkout. The packaging script verifies the checkout,
header ABI, workspace version, Cargo profile, and artifact directories before
building; it does not produce an unattested local-terminal-only package.

Verify a packaged or installed bundle and run a process-lifecycle soak with:

```sh
VERSION="$(tr -d '\n' < version.txt)"
./scripts/verify-macos-app.sh \
  --app "zig-out/release/Phux Cockpit.app" \
  --version "$VERSION" \
  --signature-mode adhoc \
  --quarantine absent
./scripts/soak-macos-app.sh \
  --app "zig-out/release/Phux Cockpit.app" \
  --cycles 10 \
  --artifacts zig-out/soak
```

Local packaging ad-hoc signs the app. The hosted release accepts either no
Apple credentials or a Developer ID identity with notarization credentials;
partial credentials fail closed:

```sh
MACOS_SIGNING_IDENTITY="Developer ID Application: Example (TEAMID)" \
APPLE_NOTARY_KEY_PATH="$HOME/private/AuthKey_KEYID.p8" \
APPLE_NOTARY_KEY_ID="KEYID" \
APPLE_NOTARY_ISSUER_ID="ISSUER-UUID" \
./scripts/package-macos.sh
```

`MACOS_ENTITLEMENTS` may point to an entitlements plist when one is required.
The packaging script validates the bundle identifier, display name, version,
executable, arm64 architecture, and code signature before producing archives.
The root Release Please workflow maintains Cockpit as an independent component
inside the shared draft version PR. Merging that PR creates a
`cockpit-vX.Y.Z` tag and draft GitHub release; the **Release Cockpit** workflow
builds, verifies, and attaches the macOS artifacts before publishing the release and
regenerating `Casks/phux-cockpit.rb` in
[`phall1/homebrew-tap`](https://github.com/phall1/homebrew-tap). The tap's
scheduled updater independently repairs a missed release update.
A failed artifact pass can be resumed by manually dispatching the **Release
Cockpit** workflow against the existing draft tag.

## Limitations

- Native Phux terminal identity and lifetime remain coordinator-owned; Cockpit
  projects published terminals but does not fake remote close. Tabs containing
  remote terminals are not persisted or restored, so their placement is not
  durable across launches.
- Layout persistence restores the SHAPE of a workspace, never its processes.
  Tabs, pane trees, divider fractions, selection, focus, and working
  directories come back; scrollback and running programs do not.
  `process_restoration_supported` is explicitly `false` and the snapshot
  carries no PID.
- Working-directory inheritance and shell-titled tabs depend on the shell
  emitting OSC 7 and OSC 0/2. A shell without integration falls back to `$HOME`
  and to numbered tabs — correct, but less useful, and not something Cockpit
  can fix from its side.
- `bold` and `italic` are carried into every cell and every renderer but cannot
  change a glyph until companion mono faces are registered; the bundled face is
  the explicit no-ligature JetBrains Mono NL, so ligatures are structurally
  unavailable regardless of renderer support.
- Windows own independent workspaces. Tabs and panes cannot yet be moved or
  detached between windows, even though every window has the same terminal,
  chrome, lifecycle, fullscreen, and restoration behavior.
- Web allowlists top-level navigation; it is not a content firewall for
  subframes or page resources. native-sdk v0.9.0 does not expose page title,
  committed-navigation, load-state, or native back/forward events to Zig, so
  Cockpit does not pretend to own general browser history.
- Headless tests prove terminal and UI behavior but cannot prove live Metal
  presentation.
- Releases without configured Developer ID credentials are ad-hoc signed rather
  than Apple-notarized. Homebrew performs the same explicit quarantine removal
  used by other apps in the tap.

## Project background

Cockpit began as a standalone spike on 2026-07-27 asking whether phux could have
several independent terminal panes beside ordinary native widgets in one GPU
surface without making the native layer understand terminals.

**Verdict: GO, with caveats.** [`FINDINGS.md`](FINDINGS.md) records what was
built, what broke, what it cost, and how each claim was established. The spike
forked native-sdk's `examples/terminal` at `a7509a7`, grew it into a two-pane
cockpit, and was then ported to the framework's first-party
`canvas.terminal_grid` painter. The old forked painter remains in history at
`git show d4ccb84^:src/box.zig`.

The rendering seam remains deliberately small (`src/terminal/grid.zig`):

```zig
pub fn feed(session: *Session, bytes: []const u8) void {
    session.stream.nextSlice(bytes);
}
```

The rendering spike is complete and closed out. All four steps of `FINDINGS.md`
section 8 were finished, migrated here as PR #7, and developed further since;
Cockpit is the shipping product, not a validation fixture. Its standalone Git
history was imported under `clients/cockpit` when it joined the Phux monorepo;
the spike
branch was removed on 2026-08-09 and its history preserved at the annotated tag
`spike/first-party-terminal-grid` — `git show spike/first-party-terminal-grid`
for the audit that established nothing was left behind.

The section 7 `phux-client-ffi` sketch is historical. The implemented stable C
ABI now links Cockpit to `phux-client-core` without exposing Rust internals.
Workspace snapshots, shared layout projection, and any thinner VT-byte handoff
belong in explicit follow-up changes rather than the repository migration.

## License

Apache-2.0. See [`LICENSE`](LICENSE). Bundled dependency licenses and notices
are recorded in [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md) and shipped
inside every application bundle.
