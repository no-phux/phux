# phux-desktop (native crate)

One Rust crate, one executable: `phux-desktop`, a GPUI and GPUI Kit
application over the shared client runtime. It has its own Cargo workspace
and lock so the GPUI dependency graph never enters the other phux crates.
`just desktop-source` once, then `just desktop-test` and `just desktop-app`.

## Layout

| Module | Owns |
|---|---|
| `launch` | CLI options, installed-CLI and server resolution, Kit init, first window |
| `diag` | log file, `perf.jsonl` sampler, `--evidence`/`--smoke` captures, window metrics |
| `app` | windows, menus, commands, chrome, overlays, the per-window `Desk` |
| `app/ui` | the component library: rows, badges, buttons, attention marks, menu anchors, the one glass menu; never reads a `Desk` |
| `app/view` | chrome composed from `ui`: titlebar, the one navigator, tabs (one renderer, two placements), panes, status |
| `app/menus` | every glass menu's contents (main, view, session, tab, pane), built from the desk |
| `app/overlays/launcher` | native command/navigation presentation over Kit's query and selection state |
| `workspace` | tabs, split trees, focus, size ownership, restore, layout persistence |
| `settings` | display preferences, Ghostty config, themes, key chords |
| `connection` | one runtime `Client` per window and its sole event/path-answer drain |
| `terminal`, `input`, `presentation` | grid painter, input handler, drawable-presented fence receipts |
| `perf` | always-on painter/key-to-paint metrics (`phux_perf` statics) |
| `telemetry` | on-demand process CPU intervals and native current/peak resident memory |
| `fonts`, `hotkeys`, `chrome` | bundled Paper Mono, global quick-terminal chords, titlebar drag |

`workspace`, `settings` and `connection` hold no GPUI types, so their rules
test without a window. Deeper contracts: [PAINTER.md](PAINTER.md),
[INPUT.md](INPUT.md), [PRESENTATION.md](PRESENTATION.md).

## ACP and attached conversations

**New ACP Agent** lists configured ACP profiles and executing-host prerequisites.
Explicit launch delegates to `phux launch --acp INTEGRATION --json --session SESSION`;
the GUI never owns a provider worker. That server-owned ACP session is also the
session mobile controls. Prompt/interrupt/answer use the shared typed control
broker and dedicated operation receipts, never terminal keys. ACP permissions and
supported provider extension questions use Kit controls with exact callback
identities and no automatic approval or recommendation selection.

Existing extension/hook streams remain **Attached TUI**: their composer and Stop
still operate the terminal, and approvals stay there. Passive stream discovery
never takes focus. Missing harness/adapter prerequisites are actionable UI;
installation and authentication remain operator responsibilities.


## Command line

```text
phux-desktop [--socket PATH] [--session NAME] [--phux PATH] [--layout PATH]
             [--fresh] [--terminal ID] [--quick] [--dev-telemetry]
             [--evidence DIR] [--smoke]
```

Without `--phux`, the CLI is `PHUX_BIN`, else the first of
`~/.local/bin`, `/opt/homebrew/bin`, `/usr/local/bin`, `~/.cargo/bin`, else
`which phux`. Without `--socket`, `PHUX_SOCKET`, else `phux server --ensure`
and its reported socket. Nothing is ever installed, replaced or upgraded. A
resolution failure still opens the window, with the reason shown.

## Observing it

- **Performance monitor:** search the palette for **Performance Monitor** to
  open a window that samples the desktop process (not the server) once a
  second: CPU over the elapsed interval, current and peak resident memory,
  and the kernel, runtime, painter, and UI-thread metrics as rates, means,
  and estimated p95. **Copy JSON** copies them. Nothing is sampled until the
  window opens, and nothing is uploaded. `--dev-telemetry` or
  `PHUX_DESKTOP_DEV_TELEMETRY=1` opens it at launch. The view owns its
  sampler and cancellable task; `telemetry::Sampler` itself does no
  background work. `diag::report()` exposes the typed report.
- **Log:** `~/Library/Logs/phux-desktop.log` and stderr
  (`PHUX_DESKTOP_LOG`, `PHUX_DESKTOP_LOG_LEVEL=debug`). Lines are redacted
  (URL passwords, labeled tokens and secrets including `PREFIX_TOKEN=` keys
  and `--token` flags, long opaque runs) before they are written, startup
  errors on stderr included. The file rotates to one `.1` backup at 8 MiB; a
  rotation that cannot move the file keeps appending rather than truncate.
- **Titles:** terminal-set titles, names and directories lose control and
  bidi override characters and are bounded before any tab, sidebar or window
  title shows them (`desk::display_title`).
- **Copy Diagnostics** (palette, Utilities): a JSON receipt of app and runtime
  versions, server id, connection epoch, host (`local` or the registry name)
  and the last 20 redacted warnings. It has a closed set of fields and carries
  no terminal content.
- **Metrics:** `diag::report_json()` is one `PerfReport` with kernel
  (`kernel.*`), runtime (`runtime.*`), painter (`desktop.acquire`,
  `desktop.prepare`, `desktop.paint`, `desktop.present`, `desktop.key_to_paint`)
  and window rows (`desktop.frame`, `desktop.drain*`, `desktop.presented`).
  Add a metric as a `phux_perf` static in the module that measures it and
  list it in that module's `TABLE`.
- **`--evidence DIR`:** appends `perf.jsonl` once a second and writes
  `presented.png`, `layout.json` and `interaction.json` for the first frame
  whose exact drawable GPUI reports for a visible window with a painted
  terminal. `presented` says whether the display confirmed it; a virtual
  display (hosted CI) never does, and `submitted` is then the claim. Windows never take
  focus in this mode. `--smoke` exits 0 after writing. The files are the proof;
  `scripts/smoke-app.ts` checks them.

## Develop

```sh
just desktop-source   # materialize the pinned, patched GPUI (toolchain/)
just desktop-test     # cargo test for this crate
just desktop-lint     # fmt --check + clippy -D warnings for this crate
just desktop-app      # debug build against a same-checkout CLI
```

Never point a development build at the production socket or replace the
installed app.
