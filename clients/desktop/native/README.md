---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-23
---

# Single-addon host foundation

**TL;DR.** This release/LTO cdylib links GPUIX, the FFI Client registry and
desktop exports into one addon. Real-PTY tests verify that JavaScript commands
and native access resolve the same Client. Its `phux-host-probe` verifies native
extension lifecycle. The linked `phux-terminal` element paints runtime views;
full terminal and multi-window acceptance remain open.

## Boundaries

Apply `../toolchain/patches/0001-native-extensions.patch` to the pinned source
before building. The patch exposes GPUIX's exact GPUI crate and startup-only
custom-element registration. It introduces no dependency on phux. The installer
runs after built-in factories, on the rendering thread. No GPUI objects become
`Send`, and no NAPI registration entrypoint is invoked manually.
The native boundary seals when the first view/registry is created. Constructing
an uninitialized production `GpuixRenderer` is permitted before installation;
its `init()` creates the view. The test renderer creates its view immediately.

The host manifest is a package-local Cargo workspace with its own lockfile.
This keeps the desktop-only GPUI/Zed graph out of other phux consumers. Its
runtime and NAPI-only FFI path dependencies use the existing engine pin. The
FFI dependency keeps its C ABI disabled here and owns the sole Client registry.
`napi`/`napi-derive` match
GPUIX exactly; `napi-build` configures the cdylib link. `serde_json` is required
by GPUIX's native custom-prop trait. These are shared graph dependencies, not
new application services.

`loadDesktopHost(absoluteAddonPath)` in `loader.mjs` canonicalizes the path,
sets `NAPI_RS_NATIVE_LIBRARY_PATH`, loads the addon, and explicitly installs
extensions. Call it before dynamically importing GPUIX or the Solid adapter.
Every binding loader must require the same canonical `.node` file. A second
path is rejected even after initialization failure; retrying failed startup
cannot return an uninitialized cached binding.
Inherited WASI-selection overrides are rejected before loading native code.

The combined binding exports `DesktopClient`. `nativeClientStatus(handle)`
resolves its handle through the same native accessor the painter uses, without
draining events or installing a second listener. A stale handle fails after
close. `DesktopClient.close()` returns the final event batch, which the owning
application must process to retain queued and shutdown-generated input outcomes.

Run `just desktop-native-build`, `just desktop-native-test`, and
`just desktop-native-client-test` for the release artifact, GPU extension
lifecycle and real-PTY Client integration respectively. The production-host
smoke omits fault fixtures requiring the FFI test addon's `NativeClientLease`;
the full FFI smoke exercises those deterministic close/restart faults separately.

`just desktop-native-fixtures-build` builds a separate release addon under
`.cache/host-fixtures` with terminal observation exports. Run
`just desktop-native-painter-test` against it for the isolated PTY/Metal fixture.
Production builds under `.cache/host` omit `terminal-fixtures`; their checked-in
declarations never include terminal cell observations. See [PAINTER.md](PAINTER.md)
for surface props, measured geometry and the remaining rendering acceptance.

The generated GPUIX ESM loader exposes GPUIX names. The desktop loader returns
the combined binary's exports. Constructor identity in the GPU smoke checks
that those surfaces resolve to the same NAPI objects. The real-PTY Client smoke
separately proves native access resolves the JS-created FFI Client. Both are
linked in `src/lib.rs`; commands and painter use that registry. Do not add
another Client map.

## Validation

Use the repo Rust pin, scope `scripts/lib/apple-toolchain-env.sh` to the build
subprocess, and give each worktree its own `CARGO_TARGET_DIR`. Build only
release artifacts for native rendering. `napi.json` sets the wrapper basename.

Run the committed native fixtures in separate fresh Bun processes with isolated
HOME/XDG directories. The launcher defaults `PHUX_DESKTOP_ADDON` to the release
wrapper path under `.cache/host`:

```sh
bash clients/desktop/tests/native/run-smoke.sh
```

The first requires the matched generated GPUIX `index.js`. It exercises the
real Metal-backed test renderer: native factory creation, paint callbacks,
painted text, bounds, event dispatch, prop updates retaining the instance,
unmount/destroy/drop, and remount. Its capture is
`clients/desktop/.cache/extension-smoke.png`. The second proves startup closes
permanently after the first registry and loader failures stay failures.

Native test windows are offscreen. This is real GPU extension evidence, not a
production terminal, screen-reader/IME test, or multi-window qualification.

`generated/index.d.ts` is NAPI's combined output, including dependency exports.
After every native build run `bun clients/desktop/native/check-generated.mjs`.
On an intentional API change, review the generated diff and copy
`.cache/host/index.d.ts` to `native/generated/index.d.ts`; never edit signatures
by hand. The FFI crate's build script tracks NAPI's metadata-directory and force-
build environment variables. Without those invalidation hooks, changing the
embedding host's features can omit cached FFI declarations from generated output
even though the binary still exports them. Fixture-to-production builds are
verified with the same private Cargo target to exercise this cache transition.

## Remaining acceptance

`phux-d4x9.2` cannot be closed on this foundation alone. Required next evidence:

- One application/platform pump with renderer-owned window handles and a
  window registry, per-window close, and generation-fenced queued callbacks.
- Scope `renderer.rs` scroll/list/pending-focus state, `automation.rs` bounds,
  `text/paint.rs` selection start regions/painted text/highlights, and
  `text/search.rs` ordinals by window. Host-ID uniqueness alone does not isolate
  these per-frame resets and retained state.
- Two independently changing Solid `createRoot(renderer)` trees, correct
  focus/scroll/selection/menu/automation behavior and close/reopen teardown.
  Upstream's high-level render singleton and test-renderer singleton cannot
  stand in for that proof.
