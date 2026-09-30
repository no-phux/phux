# phux desktop

Install the app on an Apple-silicon Mac:

```sh
just doctor desktop
just desktop-install-app
```

That builds `/Applications/Phux.app`: the shell and launcher compiled into one
executable, with the release native addon beside it. Launched from the Dock or
Spotlight, it uses your installed `phux` CLI (`~/.local/bin/phux`, Homebrew or
`~/.cargo/bin`) to start or reuse your server and attaches its `default`
session. `PHUX_PROFILE`, `PHUX_SOCKET` and `PHUX_SESSION` override that choice.
Startup problems are shown in the window and logged to
`~/Library/Logs/phux-desktop.log`. `just desktop-package` builds the bundle
without installing it.

For development, run from this checkout instead:

```sh
just desktop-app
```

That clones the pinned GPUIX source, applies the reviewed patches, builds the
native host, builds `phux` from this tree, starts a server if one is not
already running, and opens the desktop. Set `PHUX_SOCKET` only when you want a
server other than the one this checkout just ensured. `PHUX_PROFILE=<name>`
isolates the whole instance; `PHUX_DESKTOP_BACKGROUND=1` opens the window
behind the active app.

For a disposable demo that cannot reuse the installed server:

```sh
just desktop-demo
```

This builds the CLI and native addon from this checkout, then owns a private
foreground server and app. Each run gets a temporary socket, HOME, and XDG
directories; inherited Phux listener/auth settings are cleared. Closing the
app or interrupting the supervisor reaps both children and removes that state.
The demo refuses installed or external artifacts and does not restart a lost
server behind the supervisor.

## Using it

The window is a sidebar (agents by urgency, then every session and its panes),
a tab bar under the traffic lights, split panes, and a status bar. Command-key
chords belong to the app; everything else reaches the terminal.

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
`split-divider-color`, and `macos-option-as-alt`. Your `keybind` lines replace
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
path or open it; the palette's **Open … as a File** commands do the last. A `global:` bind to `toggle_quick_terminal` becomes a
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

Drag split dividers, the sidebar edge, or tabs to rearrange; double-click a tab
to rename it and a pane header to zoom. Dropping files onto a terminal pastes
their shell-quoted paths. **Insert Path** lists paths on the host that runs
the focused terminal (a satellite's own disk for a satellite pane), never this
Mac's; it needs a server that advertises `PATH_QUERY`. Enter types the chosen
path as one shell-quoted word and presses nothing else, and only into the pane
it opened on. **Terminate Terminal Process** (palette only) is the
one action that ends a process. Layout and display preferences persist per
server incarnation under `$XDG_STATE_HOME/phux-desktop/`.
Unavailable saved panes retain their split positions until they can attach;
closing one explicitly removes it. Replacing the daemon retires its old view
handles rather than rebinding their numeric terminal IDs. Unsupported or
damaged layout files are preserved without automatic writes, with a notice in
the window. Back up and remove the affected layout file to start a fresh one.

## Native framework verification

From the repository root:

```sh
just doctor desktop
just desktop-install
just desktop-source-build
just desktop-check
just desktop-framework-check
```

The framework check builds the checksum-verified patched source addon and a
production JSX bundle, then drives its real native window through click and
text-input events. It uses a background-focus window and bounded process
cleanup; patched declarations are not compared to the unpatched npm package.
The GPU capture is `dist/framework/window.png` under this package.
This fixture verifies the framework, not a working phux terminal application.
The required CI workflow gate runs `desktop-install` and `desktop-check` on
every change. GPU/window checks remain a separate macOS qualification step.

## TypeScript tooling

From `clients/desktop`, with repository-pinned Bun 1.4.2 and Node 24 on `PATH`:

```sh
export BUN_INSTALL_CACHE_DIR="$PWD/.cache/bun"
bun install --frozen-lockfile
bun run check:tooling
```

`check:tooling` runs formatting, strict TS7 typecheck, type-aware
warning-free Oxlint, the vendored rule suites, the workspace model tests
(`tests/model`) and adversarial CLI/JSX fixtures.
Fix with `bun run format` and `bun run lint --fix`. Exact tool and framework
versions are pinned in `package.json` and `bun.lock`; no global JS tool install
or `bunx` download is involved. A tooling pass is not native/GPU evidence; see
[native/README.md](native/README.md) for that.

Notes on the pinned toolchain:

- TS7's `tsc` is the native compiler. `skipLibCheck` skips third-party
  declaration bodies, not application uses of them; the negative native-event
  fixture must still produce TS2339. TS7 preserves JSX and emits nothing; the
  tooling test compiles JSX through GPUIX's Solid Bun plugin.
- Oxlint's `--type-aware` needs `oxlint-tsgolint`. The Solid plugin recognizes
  `@gpuix/solid` through `moduleSources`. Native JSX components declare an
  explicit `JSX.Element` return type because the pinned linter otherwise
  reports the inferred type as an error type.
- The Solid plugin's ESLint peers do not yet list TS7, so Bun reports a peer
  mismatch. RuleTester suites run under Node 24 because its raw-transfer parser
  rejects Bun.

### Boundaries enforced by the gate

- Type-aware unsafe operations, floating and misused promises fail.
- Chained casts, unjustified assertions and Jest/Vitest module mocks fail via
  the [selected anti-slop rules](tools/oxlint/anti-slop/README.md).
- Bun mock imports, DOM renderer imports and sibling-client imports fail. UI
  source cannot import Node builtins; the bridge/service boundary may use only
  `node:path`, `node:module` and `node:fs/promises`. Transport and reconnect
  authority belongs to Rust.
- Lost Solid reactivity, destructured props and unused/invalid suppressions fail.
- Negative fixtures are excluded from ordinary lint/typecheck and run explicitly
  with asserted diagnostics, so an unloaded plugin cannot appear green.

Solid owns reactive views. Add Effect (exact-pinned v4) only when a real
non-Solid async service, scoped resource or schema boundary needs it.
