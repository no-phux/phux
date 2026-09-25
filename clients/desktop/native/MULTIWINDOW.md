---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-23
---

# Native multi-window verification receipt

**TL;DR.** The release/LTO addon passes a real macOS Metal test of two reactive
Solid roots with overlapping local element IDs, isolated native state, sibling
preservation, queued-event fencing, and close/reopen. Four native probe instances
are created, destroyed and dropped. This qualifies the multi-window lane, not
the separate shared-Client FFI or terminal-painting acceptance requirements.

## Inputs and ownership

Worktree: `/Users/phall/workspace/phux-desktop-host`, branch
`feat/gpuix-desktop-host`, following foundation commit
`3b29fcb6d42a60dbe06ea9e069d8e0b7157ccb30`.
The commit carrying this receipt contains only the new patch/provenance, new
`tests/native/multiwindow*` fixtures, and this receipt. Host Rust, loader,
manifests, bootstrap and shared FFI/runtime files belong to the integrating lanes.

Source pins, ordered patch hashes, lifecycle contract and complexity results
are in [patch provenance](../toolchain/patches/0002-multi-window.md).
Build inputs remain the foundation's pinned Rust 1.98.1, Bun 1.4.0,
NAPI 3.12.7 / derive 3.6.8 / build 2.4.4, and source NAPI CLI 3.10.4.
Hardware: macOS 27.0 arm64, Apple M4 Pro, Metal 4.
All source development and targets are private to this worktree.

## Qualified native behavior

`bash clients/desktop/tests/native/multiwindow.sh` exits 0 and requires the
`MULTIWINDOW_PASS` marker after every assertion. A premature AppKit exit(0)
cannot satisfy the launcher. The launcher isolates HOME/XDG and places a
temporary fixture in the pinned Solid workspace so its client-runtime preload
and local package dependencies resolve together; Bun auto-install is disabled.

The fixture constructs real `GpuixRenderer` windows and uses lower-level Solid
`createRoot(renderer)`, with a single timer pumping through either renderer:

- Alpha (380×620) and beta (460×620) have different GPUI window identities and
  the same local element IDs. Actual native titles, changed title isolation,
  distinct automation widths and independently changing signals are asserted.
- Real GPUI clicks update only their Solid root. Separate native input fields
  receive `alpha` / `beta`; pending focus before first paint and later focus/
  blur operations remain window-local.
- Pending prepaint virtual-list scroll requests differ per window. Later list
  positions 15 and 30, independent scroll offsets, and native wheel delivery
  preserve the sibling's state. Wheel coordinates use the pre-scroll viewport;
  upstream painted bounds include the scroll transform.
- Actual mouse drags select each window's text independently. Clearing alpha
  leaves beta selected. Painted search matches contain only the correct
  window's text and independently mark the second match active.
- Per-window native screenshots contain different reactive text, input values,
  selection/search washes and list rows. Both images were inspected.
- Alpha closes while beta retains text, input, selection, scroll and list state.
  A queued native click cannot execute alpha's disposed-window handler; stale
  automation/mutation/focus calls reject. Gamma reuses local IDs with a fresh
  window generation, reacts independently, and closes through the registered
  Window-menu `cmd-w` action while beta is active. Beta keeps working.
- Last-window close returns `tick() === false` to JavaScript. Delta then opens
  under the same retained application and closes cleanly. Native probe counters
  finish at `created: 4`, `destroyed: 4`, `dropped: 4`, `painted: 96` in the
  recorded run (paint count is scheduling-dependent).

The existing probe is appended via the native mutation API beneath each Solid
root: Solid's public tag whitelist currently has no custom-extension tag API.
No host API additions were needed. There is no daemon, terminal or Client in
this fixture.

## Commands and results

Paths below are relative to this worktree unless a working directory is given.
Apple environment and Rust/target overrides are scoped to each build subprocess.

```sh
source scripts/lib/apple-toolchain-env.sh
export RUSTUP_TOOLCHAIN=1.98.1
export CARGO_TARGET_DIR="$PWD/clients/desktop/.cache/host-target"
clients/desktop/toolchain/gpuix/packages/native/node_modules/.bin/napi build \
  --manifest-path clients/desktop/native/Cargo.toml \
  --config-path clients/desktop/native/napi.json \
  --package-json-path clients/desktop/toolchain/gpuix/packages/native/package.json \
  --output-dir clients/desktop/.cache/multiwindow-host \
  --platform --esm --js index.mjs --release -- --locked
```

The release profile enables LTO. This build passed. Wrapper `cargo clippy` with
the same manifest, `--release --locked --no-deps -- -D warnings`, also passed.

| Check | Actual result |
|---|---|
| `bash scripts/doctor.sh desktop` | Pass, zero prerequisite problems |
| Native multi-window launcher above | Pass, all assertions and terminal marker |
| Existing `tests/native/run-smoke.sh`, selecting the new addon with `PHUX_DESKTOP_ADDON` | Pass, extension and fresh-process late-install fixtures |
| Source `cargo test --manifest-path clients/desktop/toolchain/gpuix/packages/native/Cargo.toml --release --locked --lib`, private `.cache/source-target` | 248 passed, before and after focused extractions |
| Source native `napi build --platform --esm --js index.js --release --features test-support -- --locked`, same scoped environment/target | Pass; generated declarations include four new macOS methods |
| `bun run build:js` in source `packages/native`; `bun run build` in source `packages/solid` | Pass |
| `bunx --no-install vitest run js/__tests__/multi-window.test.ts js/__tests__/host-runtime.test.ts` in source native package | 11 passed, including close/replacement delivery fences |
| `bun test src/__tests__/renderer.test.tsx src/__tests__/controls.test.tsx` in source Solid package | 11 passed, 31 assertions, real native test renderer |
| Scoped Rustfmt (`--edition 2021 --config skip_children=true --check`) on touched Rust files | Pass |
| Oxfmt 0.70.0 with `.cache/oxfmt-host.json` on new MJS/TSX fixtures | Pass |
| Ordered clean patch application, byte comparison, reverse check | Pass |

The broader source native JS package test run was also attempted: 13 passed;
its browser-bundle test fails because the private native-only checkout has no
generated `wasm/gpuix-web.js` or `.wasm`. Source-level Clippy is not clean:
32 diagnostics cover existing lint patterns/dead code/type complexity, including
unneeded returns in platform-gated functions. This lane does not claim full
upstream CI, a clean source Clippy gate, browser validation, or Linux support.

## Artifacts and integration handoff

Artifacts remain under `clients/desktop/.cache/`:
`multiwindow-build.log`, `multiwindow-source-build.log`,
`multiwindow-rust-tests.log`, `multiwindow-js-tests.log`,
`multiwindow-solid-tests.log`, `multiwindow-clippy.log`,
`multiwindow-source-clippy.log`, `multiwindow-extension-smoke.log`,
`multiwindow-native-receipt.log`, and the two native screenshots.

| Artifact | SHA-256 |
|---|---|
| `multiwindow-host/phux-desktop-native.darwin-arm64.node` | `a1bc08cf14a2769fb3082f20da1e254793121db64575717a1c7ee1e12c48d85f` |
| `multiwindow-host/index.d.ts` | `e06ca9befa0a1427e59a9ed18caffdcd7ab27c614f37e8fc0481073917989cac` |

The parent must apply patch 2 after patch 1 in its reproducible source bootstrap,
regenerate the **combined host declarations** after FFI integration, and review
the patch before landing. A fresh-context critic request was rejected by the
harness: `Subagent depth limit reached (1)`. Writer review is complete; parent
review is still required. No Beads state or remote repository was changed.
The separate shared Client registry identity/stale-Client-handle and terminal
painting requirements remain with their assigned lanes.
