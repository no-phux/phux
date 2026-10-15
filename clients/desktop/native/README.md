---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-10-15
---

# phux-desktop (native crate)

**TL;DR.** One Rust crate, one executable: `phux-desktop`, a GPUI + GPUI Kit
application over the shared client runtime (ADR-0165). It has its own Cargo
workspace and lock so the GPUI graph never enters other phux consumers.
`just desktop-source` once, then `just desktop-test` and `just desktop-app`.

## Layout

| Module | Owns |
|---|---|
| `launch` | CLI options, installed-CLI and server resolution, Kit init, first window |
| `diag` | log file, `perf.jsonl` sampler, `--evidence`/`--smoke` captures, window metrics |
| `app` | windows, menus, commands, chrome, overlays, the per-window `Desk` |
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

- **Native performance window:** runtime controls and metric semantics are in
  [Performance monitor](../README.md#performance-monitor).
  The view owns its sampler and cancellable task; `telemetry::Sampler` itself
  does no background work. `diag::report()` exposes the existing typed report.
- **Log:** `~/Library/Logs/phux-desktop.log` and stderr
  (`PHUX_DESKTOP_LOG`, `PHUX_DESKTOP_LOG_LEVEL=debug`).
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
installed app; see `docs/operations.md`.
