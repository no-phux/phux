---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-23
---

# macOS multi-window patch provenance

**TL;DR.** `0002-multi-window.patch` gives each macOS renderer its own GPUI
window and window-scoped caches under one embedded application. Apply it after
the native-extension patch. The real-native fixture qualifies independent Solid
roots, overlapping element IDs, input/selection/scroll isolation, close/reopen,
and teardown. This is a local patch, not an upstream publication.

## Ordered inputs

GPUIX baseline: `6d5e6887ad8dc6e94eb66043394f3d17a462c56a`, package 0.10.0.
Zed gitlink: `81c99f816b4a5f69d3c014774068034c24d1d7af`; no Zed changes.
Neither source lockfile changes.

| Order | Patch | SHA-256 |
|---|---|---|
| 1 | `0001-native-extensions.patch` | `2a9579715e980524ef2b8d363d24b035fbc1617b13ec4d6e7d43c8360de9a81a` |
| 2 | `0002-multi-window.patch` | `d346c3923e556dd135158f0998363811f8a8311ae2262d3518dab070062dbaf4` |

The patch includes upstream README API documentation, a changeset, generated
native declarations, and shared-runtime delivery-fence tests. Its baseline is
the pinned source **plus patch 1**, not the unmodified upstream checkout.

The integrating bootstrap must verify immutable source/lockfile inputs first,
then apply the patches in order. Preserve unexpected source edits. From the
verified source checkout:

```sh
git apply --check /absolute/path/to/0001-native-extensions.patch
git apply /absolute/path/to/0001-native-extensions.patch
git apply --check /absolute/path/to/0002-multi-window.patch
git apply /absolute/path/to/0002-multi-window.patch
```

Both patches applied successfully to an independent Git repository populated
from `git archive` of the exact baseline. Every patch-2 output file was compared
byte-for-byte with the developed source. Reverse-check of patch 2 also passed
against the developed checkout. Initializing the temporary Git repository is
important: `git apply` in an untracked directory inside another repository can
silently skip paths outside that directory's prefix.

## Ownership and lifecycle

- `mac_host` holds one application and platform. GPUI's existing window registry
  supplies typed, generation-bearing handles; there is no second window map.
- Each `GpuixRenderer` retains its own handle and liveness flag. All macOS
  window operations, input dispatch, screenshots and automation route through
  that handle. Input dispatch does not lease the root view, preserving GPUI's
  reentrant input-handler requirements.
- `WindowLocal` keys scroll handles, virtual-list states/pending scrolls,
  automation bounds, painted text, selection regions/layouts, highlight washes
  and search ordinals by the actual GPUI window identity. Paint closures name
  their window explicitly. There is no ambient “last painted window.”
- Pending focus, retained lists, custom instances and selection state belong to
  `GpuixView`. GPUI's view-release observer fences callbacks and clears native
  tree/caches, subscriptions, animation/selection tasks and custom instances.
- Native emission checks liveness. Shared JS dispatch additionally fences
  already-queued napi callbacks and scheduled callbacks after root replacement.
  Raw callback users must use the shared dispatch or their own liveness check.
- GPUI `QuitMode::Explicit` gives shutdown control to the JS host. `tick()`
  returns false with no windows, and a new renderer can reopen without creating
  another application. AppKit's explicit Quit action remains explicit quit.
- Non-macOS uses the previous single-window namespace and lifecycle. Linux,
  Windows and browser multi-window behavior is not claimed or qualified.

The text-selection adaptation retains its Comet attribution in the source.
The referenced Comet render source and pinned GPUI window/application/platform
implementations were read before changing paint and lifecycle ownership.

## Complexity

Rust measured with `uvx lizard -l rust` against the pre-follow-up snapshot and
final source. No project-specific complexity threshold exists. Lizard's
TypeScript parser truncated optional-chain bodies, so the TS numbers below use
the TypeScript compiler AST: decision statements, conditional expressions and
logical operators, excluding nested functions and optional-chain access.

| Function | Before CCN | After CCN |
|---|---:|---:|
| `init_macos` | 11 | 3 |
| extracted `initialize_application` / `open_window` | new | 2 / 5 |
| `build_virtual_list` | 14 | 7 |
| extracted `focus_virtual_row` / `focused_virtual_row` | new | 2 / 4 |
| `register_down_listener` | 12 | 7 |
| extracted `apply_text_press` | new | 5 |
| `registry_point` | 10 | 10 |
| shared JS `dispatch` | 12 | 5 |
| extracted `windowEventHandler` | new | 4 |
| scheduled JS `invoke` | 1 | 3 |
| `flushMutations` | 2 | 3 |
| Solid `syncSelection` / `detach` | 3 / 2 | 4 / 3 |

Raw measurements remain in `.cache/multiwindow-complexity-{before,after}.txt`
and `.cache/multiwindow-typescript-complexity.tsv` relative to the desktop
directory. Relevant native and JS tests verify the extracted behavior; see the
[validation receipt](../../native/MULTIWINDOW.md) for exact commands and limits.
