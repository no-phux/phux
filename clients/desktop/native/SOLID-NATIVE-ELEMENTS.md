---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-23
---

# Solid native-element verification receipt

**TL;DR.** A real Solid JSX custom tag now mounts the installed native probe,
paints on Metal, receives native clicks, updates a native prop reactively,
unmounts and remounts. Native instances finish at two created/destroyed/dropped.
Missing native factories fail clearly, and JS schema registration before native
installation proves registration does not seal native startup.

## Inputs and commands

The lane remains in `/Users/phall/workspace/phux-desktop-host`, following
`78cefd3d665f7fbccab9477f24530116cb3b0b77`. Source pins, ordered patch context,
exports, application loader wiring and complexity are in
[patch 3 provenance](../toolchain/patches/0003-solid-native-elements.md).
Rust 1.98.1, Bun 1.4.0, NAPI 3.12.7 / derive 3.6.8 / build 2.4.4 and source CLI
3.10.4 remain unchanged. Platform: macOS arm64, Apple M4 Pro, Metal 4.

Release/LTO build passed with the same scoped environment as the prior receipt:

```sh
source scripts/lib/apple-toolchain-env.sh
export RUSTUP_TOOLCHAIN=1.98.1
export CARGO_TARGET_DIR="$PWD/clients/desktop/.cache/host-target"
clients/desktop/toolchain/gpuix/packages/native/node_modules/.bin/napi build \
  --manifest-path clients/desktop/native/Cargo.toml \
  --config-path clients/desktop/native/napi.json \
  --package-json-path clients/desktop/toolchain/gpuix/packages/native/package.json \
  --output-dir clients/desktop/.cache/solid-elements-host \
  --platform --esm --js index.mjs --release -- --locked
```

`bash clients/desktop/tests/native/solid-native-elements.sh` passes and requires
the terminal `SOLID_NATIVE_ELEMENTS_PASS` marker. The launcher typechecks the
fixture's application-level JSX augmentation before running it, uses the pinned
Solid client preload/workspace with auto-install disabled, and isolates HOME/XDG.
All nodes are created by Solid JSX; there are no injected mutation nodes or
JavaScript terminal-grid simulations. The native probe remains an infrastructure
fixture, not a claim of terminal functionality.

Assertions cover registration before native bootstrap; identical addon
constructors; harmless factory queries with zero created instances; native
text/bounds/clicks; retained instance on reactive prop update; disposal through
Solid `Show`; fresh native remount; root unmount; missing-factory rejection; and
host-controlled last-window shutdown. Recorded counters: `created: 2`,
`destroyed: 2`, `dropped: 2`, `painted: 6` (paint count is scheduling-dependent).
The screenshot was inspected and shows the updated native label on its surface.

Additional checks passed:

- Source native `bun run build:js`; source Solid and React `bun run build`.
- Source native `bunx --no-install vitest run
  js/__tests__/custom-element-types.test.ts js/__tests__/host-runtime.test.ts`:
  11 tests passed. Coverage includes all built-ins, malformed/unregistered names,
  shared registration, duplicate registration and module reloads.
- Source Solid `bun test src/__tests__/renderer.test.tsx
  src/__tests__/controls.test.tsx`: 11 tests passed, 31 assertions.
- Source `cargo test --manifest-path
  clients/desktop/toolchain/gpuix/packages/native/Cargo.toml --release --locked
  --lib` with the pinned Apple/Rust environment and `.cache/source-target`:
  248 tests passed.
- Scoped Rustfmt, fixture Oxfmt, launcher shell syntax, clean ordered patch
  application and reverse-check. The source `GpuixRenderer` declaration was
  compared exactly with the newly generated combined addon declaration.

No full upstream CI or additional-platform qualification is claimed. Existing
source Clippy/browser-artifact limitations remain as recorded for patch 2.
Independent nested review is unavailable at this lane's harness depth; the
integrating parent owns fresh review.

## Artifacts

Under `clients/desktop/.cache/`: `solid-elements-build.log`,
`solid-elements-rust-tests.log`, `solid-elements-solid-tests.log`,
`solid-native-elements-final.log`, `solid-native-elements-receipt.log`,
`solid-native-elements.png`, and `solid-elements-typescript-complexity.tsv`.

| Artifact | SHA-256 |
|---|---|
| `solid-elements-host/phux-desktop-native.darwin-arm64.node` | `bce92fe54b1a74dac113a5c5cced6c06c5257884a4f1a66a05bf473f7a336c99` |
| `solid-elements-host/index.d.ts` | `c44c52bf77a11cd9dccf11d6e285605328f851bb4396545f5e5d116c6701574c` |

The separately assigned stdin automation-routing correction follows in patch 4;
the custom-element fixture intentionally uses the raw renderer and shared root
dispatch to isolate this patch's acceptance.
