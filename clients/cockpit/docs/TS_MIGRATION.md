# TypeScript core + `.native` markup

The [interaction seam](INTERACTION_SEAM.md) defines current modality delivery
and the split between speculative chrome and canonical state.

Status: **shipped** (2026-09-04). `src/core.ts` and `src/app.native` are
Cockpit's only app coordinator and chrome authoring path. The retired Zig
coordinator (`update.zig`, the Zig composition root and its UiApp harness) has
been deleted; there is no second app graph.

## Shape

- **Core → engine.** The core issues one versioned intent command
  (`Cmd.host("cockpit.intent")`) and one snapshot request
  (`Cmd.request("cockpit.snapshot")`) through `TsUiApp.CoreOptions.host_calls`,
  configured by the fork's narrow `native_extension` hook
  (`src/native_extension.zig`).
- **Engine → core.** `src/cockpit/native/ts_engine.zig` owns the Cockpit
  `Model` and announces changes on `protocol.event_channel_key`; channel posts
  are capped at 4096 bytes, so the core receives ordered invalidations and
  requests snapshots, never raw terminal bytes. The snapshot protocol is an
  internal lockstep seam (version 1); it bumps only if independent producers
  and consumers ever appear.
- **Terminal pixels.** Native `terminal_painter.zig` paints grids into the
  chrome display-list prefix beneath the markup tree. A `media-surface` leaf fed
  by a native RGBA producer was considered and not built: it would lose the
  incremental patch path and need a parallel accessibility surface.

## What stays native

Terminal cell grids and emulator-adjacent painting, `grid.Session` and
libghostty-vt, key encoding (it depends on live emulator modes), the lossless
outbound ring, the resize pump, providers, native search, and extension event
routing. `resolvePanesIn` / `workspaceChromeIn` remain the single geometry
derivation for painter, hit-test and PTY sizing; markup consumes it.

The `ts-chrome-parity` harness in `src/native_extension.zig` solves the
compiled `app.native` at every declared window size and density in every
reachable chrome state and runs the toolkit's layout audit on it.

## Toolchain

The SDK package's TypeScript compiler is not in the tarball pin; `build.zig`
runs `npm ci --include=dev` once per pin in
`zig-pkg/native_sdk-*/packages/core`. Node 24+ is required.

## Non-goals

Rewriting the terminal engine in TypeScript (the subset has no FFI, streams or
PTY ioctl) and any WebView frontend.
