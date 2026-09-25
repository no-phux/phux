---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-23
---

# Bounded GPUIX host patch

**TL;DR.** `0001-native-extensions.patch` exposes GPUIX's existing native custom
element infrastructure to a statically linked host. It is a local, reviewable
patch; it has not been published or submitted upstream.

## Baseline and rationale

- GPUIX: `6d5e6887ad8dc6e94eb66043394f3d17a462c56a`, package 0.10.0.
- Zed gitlink: `81c99f816b4a5f69d3c014774068034c24d1d7af` (unchanged).
- Patch SHA-256: `42d56bb564ea8e87d0c0a872a918c05d597881fd0a1cd69a04287ad9aa69132d`.

The existing custom-element traits are private, and their render signature
names a crate-private view type. Loading another GPUI addon would duplicate
state and GPUI types. A narrow public facade exposes the traits, existing
surface instrumentation, opaque view, callback alias, and exact GPUI crate.
One explicit startup installer creates factories on the rendering thread after
built-ins. Creating any registry seals installation; duplicate and late
installation return errors. The installer runs outside the startup lock.
The boundary is view/registry creation, not the inert production renderer
constructor; its `init()` creates that registry. The test renderer constructor
creates a view immediately. The first review corrected the documentation to
match that deliberate initialization boundary.

The patch includes its README API contract and changeset. No GPUI behavior or
Zed source is modified. Public visibility does not expose the view's fields or
make GPU objects `Send`. Applications still own stable named element IDs,
text-paint routing, element lifetime, and explicit bootstrap order.

## Application integration

The parent source-bootstrap lane must verify the immutable source and lockfile
digests **before** applying this patch, then account for the expected patch
state on subsequent runs. The source lockfiles are unchanged. From a clean
pinned checkout:

```sh
git apply --check /absolute/path/to/0001-native-extensions.patch
git apply /absolute/path/to/0001-native-extensions.patch
```

For an already-patched checkout, `git apply --reverse --check` verifies this
patch's expected content. Preserve unexpected edits; do not reset the source
tree to make the check pass. Build the desktop wrapper from `../../native`,
and ensure the GPUIX and desktop loaders choose that same canonical `.node`.

## Scope and verification

The release/LTO wrapper's GPU smoke proves the native extension factory is
called, stable native instance retention across prop changes, painted text,
native bounds and click dispatch, and balanced destruction/drop on unmount and
remount. Generated-loader constructor identity proves GPUIX and host exports
come from the same loaded binary. See `../../native/README.md` for fixtures and
remaining integration requirements.

This patch does not implement terminal painting, the optional FFI registry,
or multi-window isolation. In particular, existing application/window,
scroll/list, automation, and text-paint singletons remain. A second blank window
would not satisfy the desktop contract.

Complexity measured with `uvx lizard -l rust` against the baseline and patch:

| Touched function | Before CCN | After CCN |
|---|---:|---:|
| `custom_surface` (visibility only) | 3 | 3 |
| `CustomElementRegistry::new` | 1 | 1 |
| `CustomElementRegistry::with_defaults` | 1 | 1 |
| `Installation::install` | new | 3 |
| public `install` / `seal_installation` | new | 1 each |
| `install_into` | new | 2 |

Handwritten host Rust functions are CCN 1 except `Probe::set_prop` (2).
`loadDesktopHost` is CCN 5. No touched function exceeds the skill's default
watch threshold. Existing registry synchronization/event branching is unchanged.
