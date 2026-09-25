---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-23
---

# Desktop source toolchain

**TL;DR.** The desktop builds GPUIX's native host and Solid adapter from one
verified source revision. `source.json` pins that revision, its Zed dependency,
the matching package version, and both dependency lockfiles. The generated
checkout is local build state; the source manifest is the reproducible input.

## Bootstrap

From the repository root, run `just desktop-source`. It clones the source and
initializes its pinned Zed submodule, then verifies the revisions, versions and
lockfile checksums before a frozen Bun install. Existing mismatched checkouts
are rejected and preserved for inspection, never reset or overwritten.

The ordered `patches` hashes in `source.json` identify the reviewed host
extension patches. A pristine checkout receives them after source verification.
Repeated bootstrap compares the complete patched tree using a temporary Git
index, including new files, while preserving the real index. Unrecorded edits,
staged changes, and unrelated untracked source fail rather than being overwritten.
An exact earlier prefix of the pinned patch series can advance to the full
series. Each patch is checked in the temporary index before source is changed;
partially applied or independently edited patches remain rejected.

`just desktop-source-build` builds the release native addon, both native JS
loaders, native JS declarations, and the Solid adapter. Cargo uses `--locked`.
The source's existing JavaScript compiler builds the third-party packages; the
desktop application's separate checks use native TypeScript 7.

The build explicitly uses phux's `rust-toolchain.toml` pin rather than the
upstream checkout's older compiler. This does not change phux's compiler or
Ghostty dependency. Linking the desktop's native host to phux still needs the
shared-host compatibility gate in `phux-d4x9.2` and `phux-d4x9.3`.

## macOS prerequisites

GPUI additionally needs Apple's Metal compiler, which is separate from the
native phux prerequisite check. With Xcode selected, install it using
`xcodebuild -downloadComponent MetalToolchain`, then verify
`xcrun metal --version` actually succeeds. A successful download alone does
not prove the compiler is usable in the current environment.

An inherited Nix `DEVELOPER_DIR` can make Apple's download command appear to
succeed while its cache refresh invokes Nix's `xcrun`. For that case, run:

```sh
env -u SDKROOT DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer \
  PATH=/usr/bin:/bin:/usr/sbin:/sbin \
  /usr/bin/xcodebuild -downloadComponent MetalToolchain
```

The build and desktop doctor reuse `scripts/lib/apple-toolchain-env.sh` to
select Apple's tools inside their subprocesses, including when launched from
Nix. This keeps Nix's linker wrappers away from the Xcode 27 SDK. A fully
Nix-provided Metal build is not claimed: Apple's Xcode and Metal component are
host prerequisites.

## Provenance and redistribution

Source: <https://github.com/remorses/gpuix>. The pinned GPUIX packages declare
Apache-2.0. Preserve the checkout's `LICENSE` and `THIRD_PARTY_NOTICES.md`,
including the MIT-attributed Comet components. The Zed checkout and every
resolved dependency retain their own notices. The packaging task must collect
the licenses for the actual linked dependency graph; the GPUIX package license
alone is not a complete distribution inventory.

Any local framework patch must be committed under this directory with its
upstream revision, rationale and verification. Never rely on an unrecorded
edit inside the ignored checkout. Source acquisition and build success do not
prove multi-window isolation, terminal integration, or release readiness;
those remain explicit desktop task acceptance gates.
