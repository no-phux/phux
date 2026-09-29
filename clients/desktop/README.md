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
| ⇧⌘A              | Jump to the agent that most needs you                                       |
| ⌘B, ⌘,           | Toggle the sidebar, open Settings                                           |
| ⌘= ⌘- ⌘0         | Font size                                                                   |
| ⌘N               | New window, with its own connection and a fresh terminal                    |
| ⌘K               | Clear the screen                                                            |
| ⌘-click          | Open the link under the pointer (OSC 8 or a URL in the text)                |

### Coming from Ghostty

If you have a Ghostty config (`~/.config/ghostty/config`, or the Application
Support copy), the first launch adopts it: font family and size,
`adjust-cell-width/height` percentages, colours including the 16-colour
palette and a named `theme`, window padding, `unfocused-split-opacity`,
`split-divider-color`, and `macos-option-as-alt`. Your `keybind` lines replace
the built-in chords where an equivalent command exists, including non-Command
chords such as `ctrl+tab`, `shift+enter` or a bare `f12`. Besides splits, tabs,
fonts, scrolling and search, that covers `text:`, `esc:` and `csi:` (typed as
keys, with ESC before a key sent as Alt on it, so `shift+enter=text:\x1b\r`
gives Alt-Enter), `ignore`, `move_tab`, `new_split:left/up`,
`prompt_surface_title`, `set_font_size`, `scroll_page_lines`, `toggle_maximize`
and the search actions. A `global:` bind to `toggle_quick_terminal` becomes a
system-wide hotkey for a quick-terminal window that keeps its own terminal
between toggles. If Ghostty is still running it holds that hotkey too, so quit it
or rebind one of them. Settings > Ghostty re-applies the config, reloads it,
switches keybind import off, and lists the binds it skipped: key sequences,
bare letters, and actions with no equivalent yet (`jump_to_prompt`,
`select_all`, `reset`, `write_*_file`, clipboard actions off ⌘C/⌘V).

Drag split dividers, the sidebar edge, or tabs to rearrange; double-click a tab
to rename it and a pane header to zoom. Dropping files onto a terminal pastes
their shell-quoted paths. **Terminate Terminal Process** (palette only) is the
one action that ends a process. Layout and display preferences persist per
server incarnation under `$XDG_STATE_HOME/phux-desktop/`.

## Native framework verification

From the repository root:

```sh
just doctor desktop
just desktop-install
just desktop-source-build
just desktop-check
just desktop-framework-check
```

The framework check compares generated native declarations and Solid artifacts
against the installed packages, builds a production JSX bundle, and drives its
real native window through click and text-input events. It uses the matched
source-built addon, a background-focus window and bounded process cleanup.
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
