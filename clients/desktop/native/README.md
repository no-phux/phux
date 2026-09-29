---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-28
---

# Single-addon desktop host

**TL;DR.** This release/LTO cdylib links GPUIX, the FFI Client registry and
the desktop exports into one NAPI addon. JavaScript commands and the native
painter resolve the same Client. The `phux-terminal` element paints runtime
views; the fixtures below are the qualification evidence, and full terminal
acceptance remains open.

## Boundaries

The pinned GPUIX source must carry the ordered patches in
[`../toolchain/patches/`](../toolchain/patches/README.md). Patch 1 exposes
GPUIX's GPUI crate and a startup-only custom-element installer; the installer
runs after built-in factories on the rendering thread and seals when the first
view/registry is created. No GPUI objects become `Send`.

The host manifest is a package-local Cargo workspace with its own lockfile, so
the GPUI/Zed graph stays out of other phux consumers. Its path dependencies are
root phux crates, so a root dependency bump can strand that lock:
`just desktop-check` runs `scripts/check-desktop-host-lock.py`; repair with
`just desktop-lock-fix` or `just desktop-lock-refresh`. The FFI dependency keeps
its C ABI disabled and owns the sole Client registry. Do not add another.

`loadDesktopHost(absoluteAddonPath)` in `loader.mjs` canonicalizes the path,
sets `NAPI_RS_NATIVE_LIBRARY_PATH`, loads the addon and installs extensions.
Call it before importing GPUIX or the Solid adapter. A second addon path is
rejected even after a failed start, and WASI-selection overrides are refused.

`phux-terminal` sizes its terminal from its own bounds when the shell marks it
`sizeOwner` (see the architecture's Geometry section), gives the keyboard back
to the window when `focused` drops, and reports `click` and `fileDrop` so the
shell can track the chosen pane and paste dropped paths. `phux-drag-region`
moves the window and zooms it on double-click, which a transparent titlebar
otherwise loses.

`nativeClientStatus(handle)` resolves a handle through the painter's accessor
without draining events. `DesktopClient.close()` returns the final event batch,
which the owner must process to keep queued and shutdown-generated outcomes.

`generated/index.d.ts` is NAPI's combined output. `build-host.sh` runs
`check-generated.mjs` after every production build; on an intentional API
change, review the diff and copy `.cache/host/index.d.ts` over it. Never edit
signatures by hand.

Build with the repo Rust pin, `scripts/lib/apple-toolchain-env.sh` scoped to
the build subprocess, and a private `CARGO_TARGET_DIR`. Native rendering is
qualified on release artifacts only.

## Fixtures

All run offscreen on Metal with isolated HOME/XDG directories and never touch
the user's daemon. None is a screen-reader, OS IME or packaging test.

| Command | Proves |
|---|---|
| `just desktop-native-build` | Release addon build plus generated-declaration check |
| `just desktop-native-test` (`tests/native/run-smoke.sh`) | Extension lifecycle: factory creation, paint, bounds, clicks, prop retention, unmount/remount, canonical-path aliasing, late-startup rejection |
| `just desktop-native-client-test` | JS-created FFI Client and native access share one registry over a real PTY |
| `just desktop-native-view-test` | Independent runtime views through the combined addon |
| `just desktop-native-fixtures-build` then `just desktop-native-painter-test` | Painter fidelity and decoded-pixel oracle; see [PAINTER.md](PAINTER.md) |
| `bash tests/native/multiwindow.sh` | Two Solid roots with overlapping element IDs: isolated input, selection, scroll, search, close/reopen and queued-event fencing (needs patch 2) |
| `bash tests/native/solid-native-elements.sh` | Solid JSX custom tags mount the native probe, update reactively and reject missing factories (needs patch 3) |
| `bash tests/feasibility/run.sh` | Integrated two-surface Solid terminal; see [its README](../tests/feasibility/README.md) |

Input has a manual fixture; see [INPUT.md](INPUT.md). Presentation acknowledgement
is covered by unit tests; see [PRESENTATION.md](PRESENTATION.md).

## Remaining acceptance

`phux-d4x9.2` stays open for per-window scoping of GPUIX's remaining
scroll/list/automation/text-paint state beyond the multi-window fixture,
Retina/scale-change evidence, and full terminal product acceptance.
