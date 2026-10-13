---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-10-13
---

# Native desktop terminal surface

**TL;DR.** This crate's terminal, input, presentation, fonts, hotkeys and
titlebar drag are GPUI entities and plain Rust types. They take
`Arc<phux_client_runtime::Client>`. They do not export NAPI and do not look
up a string client registry. The process entry, manifest and `lib.rs` graph
belong to the integrating binary.

## Surface

`terminal::Terminal` implements `gpui::Render`. Construct it with the
connection's client, a runtime `ViewId`, the pane's `ResourceId` and
`terminal::Settings`. `configure` replaces font, theme, visibility, input
policy, size ownership and app chords. `request` runs one `HostAction`
(`Copy`, `Paste`, `CopyText`, `Open`) immediately on that entity and does not
retain it. `focus_handle` is stable across an input rebind.
`TerminalEvent::{Activated, PathsDropped}` tells the shell which pane was
clicked and which paths were dropped.

`chrome::DragRegion` moves the window and zooms it on double-click, which a
transparent titlebar otherwise loses. `fonts::install` registers Paper Mono
once before the first measure. `hotkeys::GlobalHotkeys` registers system-wide
chords through `global-hotkey` (Carbon hotkeys; no Accessibility permission)
and `take_pressed` drains presses. `perf::desktop_perf_json` appends the
desktop rows to the kernel and runtime report.

The paint path borrows the entity's `Arc<Client>`. Pending presentation
receipts hold `Weak<Client>` and drop any upgraded lease off the UI thread.
See [PAINTER.md](PAINTER.md), [INPUT.md](INPUT.md) and
[PRESENTATION.md](PRESENTATION.md).

## What this crate does not own

The integrating `lib.rs` must stop installing GPUIX custom elements and NAPI
exports, publish `terminal`, `chrome`, `fonts`, `hotkeys` and `perf`, and drop
the deleted probe module. Packaging, the shipping binary and on-screen smoke
are outside this slice.

## Remaining acceptance

Retina/scale-change GPU evidence and a visible-window presentation harness
(hidden, minimized, offscreen, failed draw) are still open. Rust tests cover
admission, identity replacement, scene reuse and the ticket boundary. They are
not a screen-reader, OS IME or packaging test.
