---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-23
---

# Native foundation verification receipt

**TL;DR.** The release/LTO single-addon native-extension fixture passes on
Apple-silicon Metal. The parent integration additionally verifies shared FFI
Client identity against a real PTY. Terminal painting and multi-window isolation
remain unqualified; `phux-d4x9.2` stays open for those requirements.

## Parent shared-Client integration (2026-09-23)

The commit carrying this section links NAPI-only `phux-client-ffi` directly into
the wrapper, following runtime `937660bd8`, binding `9bc2aae75` and reviewed
binding fixes `9acaed925`. It builds at `/Users/phall/workspace/phux-desktop`.
Source/OS/hardware inputs match the foundation receipt below. Final addon SHA-256:
`f3dfbe0fc5e04ec0eb542c38037b84d361213307d81e8f70d8ffaffb9b3a90bd`.

Commands passed:

- `bash clients/desktop/scripts/build-host.sh`: release/LTO build and combined
  generated declaration check. Log: `clients/desktop/.cache/host-build.log`.
- `just desktop-native-test`: both isolated fresh-process fixtures pass;
  2 created, 2 destroyed, 2 dropped, 11 actual Metal paints. Also checks the
  linked `DesktopClient` constructor and stale native lookup.
- `CARGO_TARGET_DIR="$PWD/target-desktop-validation" cargo run --locked -p
  phux-client-ffi --no-default-features --features napi --example napi_smoke --
  "$PWD/clients/desktop/.cache/host/phux-desktop-native.darwin-arm64.node"
  --production-host`: real PTY, native/JS status identity, acknowledged paste,
  refusal, attach/detach/spawn, lossless close, 18 wakes and environment cleanup.
  The 300,000-disposal test grew RSS by 606,208 bytes between 50k and 300k;
  abandoned-object GC and 100 forced Worker terminations passed.
- Apple-normalized release host Clippy, `--no-deps -- -D warnings`: passed.
- `just desktop-check`: TS7, Oxc, rule suites, 24 tests/51 assertions and
  17 source/patch preservation tests passed. `bash scripts/doctor.sh desktop`
  and workspace formatting passed.
- Parent compatibility: runtime 74 tests passed (2 ignored benchmarks), core
  341 tests plus 2 compile-fail doctests, default C ABI 236 tests and 3 connected
  integration tests passed in the private `target-desktop-validation` directory.

Independent binding review reran all 40 NAPI-only unit tests, full fixture-addon
fault/cleanup smoke and 25 concurrent Worker terminations. All three initial
findings are resolved: final shutdown receipts, original-number validation and
bounded environment cleanup. Real restart verifies a held Client lease and JS
owner share epoch 1 to 2, exactly one Unknown, and the retained delivery fence.
The combined production-host command above does not claim that fault-fixture
coverage; it has no fixture-only held-lease export.

Direct native Cargo without Apple normalization reproduced a Metal-toolchain
lookup failure from inherited SDK settings. The supported build script and
doctor use the existing Apple environment helper and passed on the same host.

## Inputs and environment

- Worktree: `/Users/phall/workspace/phux-desktop-host`,
  `feat/gpuix-desktop-host`, base `8d3fc1bacf6ce2d8f0bf89f16dfb347eecdfc0b0`.
  The commit carrying this receipt identifies the verified owned source diff.
- GPUIX `6d5e6887ad8dc6e94eb66043394f3d17a462c56a`; Zed
  `81c99f816b4a5f69d3c014774068034c24d1d7af`; bounded patch hash recorded in
  `../toolchain/patches/README.md`. Private source checkout and target caches.
- Rust 1.98.1; Bun 1.4.0; NAPI 3.12.7; napi-derive 3.6.8;
  napi-build 2.4.4; source NAPI CLI 3.10.4.
- macOS 27.0 (26A428), arm64, Apple M4 Pro 20-core GPU, Metal 4.
  Connected display: 1920×1080 at 60 Hz. Offscreen fixture/capture: 360×160
  pixels at scale 1. No daemon, endpoint, terminal, runtime view or frame exists
  in this infrastructure fixture. Host root ID 1; custom element ID 2.
- Final smoke launcher isolates HOME and XDG config/state/cache/data. Windows
  are offscreen and no user terminal is connected or mutated.

## Commands and outcomes

All successful commands below exited 0. Paths are relative to the worktree.

`bash scripts/doctor.sh desktop`: zero prerequisite problems, including Metal.

Release host build, run twice with identical declaration output and addon hash:

```sh
source scripts/lib/apple-toolchain-env.sh
export RUSTUP_TOOLCHAIN=1.98.1
export CARGO_TARGET_DIR="$PWD/clients/desktop/.cache/host-target"
clients/desktop/toolchain/gpuix/packages/native/node_modules/.bin/napi build \
  --manifest-path clients/desktop/native/Cargo.toml \
  --config-path clients/desktop/native/napi.json \
  --package-json-path clients/desktop/toolchain/gpuix/packages/native/package.json \
  --output-dir clients/desktop/.cache/host --platform --esm --js index.mjs \
  --release -- --locked
```

The environment changes above were scoped to a build subprocess. The host
Cargo release profile enables LTO. Its release clippy check passed with
`cargo clippy --manifest-path clients/desktop/native/Cargo.toml --release
--locked --no-deps -- -D warnings`. Rustfmt and the four handwritten MJS files
also passed (Oxfmt 0.70.0 with an empty temporary config to avoid the package's
intentional `native/**` ignore). The dependency build emits inherited Zed
Cocoa/dead-code warnings; this is not a warning-free upstream build claim.

The source GPUIX addon/ESM loader was also built with the source lockfile,
release mode, the same scoped Apple/Rust environment, and a separate
`.cache/source-target`. With that target, both commands passed:

```sh
cargo test --manifest-path clients/desktop/toolchain/gpuix/packages/native/Cargo.toml \
  --release --locked --lib native_extensions
cargo test --manifest-path clients/desktop/toolchain/gpuix/packages/native/Cargo.toml \
  --release --locked --lib custom_elements::tests
```

Each ran two tests: startup single-install/late-install rejection, then existing
prop synchronization and element-type replacement teardown. Four unit tests
passed in total. Testing a dependency through the host workspace was rejected
by Cargo because it is not a workspace member; the source-manifest commands
above are the resolved validation path.

`bash clients/desktop/tests/native/run-smoke.sh`: both fresh-process fixtures
passed. The Metal fixture observed two native instances created, destroyed and
dropped, with eleven paint callbacks. It asserted actual painted text, native
bounds, click event delivery, retained native state on prop update, balanced
teardown/remount, canonical-path aliasing, second-addon-path rejection, and
identical GPUIX/desktop renderer constructors. The late-startup fixture rejected
bootstrap after renderer construction and rejected both loader retry attempts.

`bun clients/desktop/native/check-generated.mjs`: combined declarations match
fresh NAPI generation, including GPUIX and application exports. `cargo tree -i
napi` and `cargo tree -i gpui` confirm a single version/source for each in the
host graph. `git apply --reverse --check` confirms the recorded patch matches
the private source changes; `git apply --check` also passed against a temporary
clean checkout of the pinned baseline. The scoped root documentation gate
passed (239 files, zero violations), as did shell syntax and staged whitespace
checks. Patch context whitespace is preserved as unified-diff syntax.

## Artifacts

All raw build/test logs and the GPU capture remain in `clients/desktop/.cache/`:
`host-build.log`, `host-build-repeat.log`, `source-build.log`, `host-clippy.log`,
`extension-unit.log`, `registry-unit.log`, `host-smoke.log`, and
`extension-smoke.png`. The capture was inspected: it shows native extension
text on the expected blue native surface, not a blank window.

- Addon: `.cache/host/phux-desktop-native.darwin-arm64.node`
- Addon SHA-256: `f0d3036778388de30eb39e45b6660919ca6de6b3e1791d35c516c9ea2c776ee6`
- Host lockfile SHA-256: `7e6a5a3841afe3f7e2d8ce52977d2cad3fac947e86e2129529fba25a45a93efa`
- Generated declarations SHA-256: `42074e944e90a37669d39e66720f3e32b6678cbb823841db7243e1789d48b6a4`

## Review and remaining scope

The writer reviewed the load-bearing code and final diff. An attempted direct
fresh-context critic was rejected by the harness subagent-depth limit (1).
The integrating parent must perform that independent review. No Beads state,
shared FFI/runtime source, root manifest, source bootstrap, or remote repository
was changed by this lane. See `README.md` for the exact remaining FFI and
multi-window seams; this is a scoped foundation pass, not full desktop CI.
