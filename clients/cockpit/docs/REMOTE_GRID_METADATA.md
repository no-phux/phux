---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-07
---

# Remote grid metadata

**TL;DR.** `phux_client_terminal_grid_metadata` is an additive, read-only
companion to the unchanged v1 grid/cell ABI. It carries the color provenance
and cursor state needed to apply the local terminal presentation policy.

## Borrow and layout contract

1. Call `phux_client_terminal_grid` to build a dense viewport.
2. Initialize `PhuxTerminalGridMetadata.size` and `.version`, then call the
   metadata query for the same terminal. This reads the same-pass cache; it
   does not render again or invalidate the grid.
3. Copy presentation data before any mutable client call. Such a call
   invalidates metadata availability, even if it does not rebuild the grid.

The metadata repeats grid generation/revision, dimensions and cell count.
Zig validates these before changing owned state. Its row-major cell metadata
array borrows the render cache; the global palette and colors are returned by
value. Both the C grid and metadata can be destroyed after projection. No
borrowed metadata pointers are retained in the owned canvas.

The existing `PhuxTerminalGridView` and `PhuxTerminalCell` layouts are
unchanged. The new output header is checked before writing the full struct;
tests use undersized header-only storage with a canary. This adds a symbol,
not a protocol or v1 ABI version change.

On this aarch64 macOS host, a C `sizeof` probe against both the `27738342`
header and the new header reports v1 cell/grid sizes of 36/176 bytes in both.
The additive cell metadata is 4 bytes; the metadata grid is 856 bytes,
including its 256-entry RGB palette. These are measured layout sizes, not
portable hardcoded admission limits.

## Source values and presentation policy

The FFI captures the following during the same render pass:

- Active 256-color palette from `Snapshot.colors()`.
- Effective foreground/background options from `Terminal.fg_color()` and
  `bg_color()`, with `Mode::REVERSE_COLORS` applied exactly once.
- Cursor color, blink and wide-tail state from the render snapshot. Cursor
  width also checks the actual dense grid cell's native wide classification.
- Foreground kind/index and default-vs-explicit underline/background color
  from each native `Style` and cell content tag, before RGB loses provenance.

The effective color getters matter: pinned Ghostty `terminal/render.zig`
retains its previous colors when either configured default is absent. After
OSC 110/111 clears an override, that cached RGB can still name the old
override. Metadata explicitly reports absent defaults, and the projection
uses its configured SDK theme fallback, swapped under DECSCNM. It never
infers absence from a color value. Explicit palette/RGB backgrounds on blank
cells are preserved through their native content tags.

`grid_metadata.Policy` separates renderer policy from terminal state:

- The current local `Palette.resolveFgRaw` enables bold-as-bright for ANSI
  indices 0–7. The remote projection uses the actual index and live palette
  entry 8–15. RGB colors, already-bright indices and the color cube are not
  brightened. Disabling the policy preserves bold's weight flag.
- `Palette.resolveFg` applies inverse once; otherwise faint blends halfway
  toward the terminal default background, after bold-as-bright. The remote
  arithmetic uses the same normalized-channel blend and precedence.
- Default underline color becomes the SDK's `null` (follow resolved cell
  foreground), while explicit underline RGB remains explicit even when it
  equals the original foreground.
- Missing terminal defaults, cursor color and selection wash use SDK theme
  tokens. `copyClient` uses the policy stored by the Host; `copyWithMetadata`
  also accepts a configured policy. No engine-side bold setting is invented:
  neither the current core adapter nor the pinned engine exposes one.

Host publication uses `copyClient` with its stored policy; queue, input and
error-routing paths remain owned by their respective lanes.

## Configured policy and idle repaint

The shipping painter forwards `terminalTokensFrom` and `Config.cursor_color`
through `remote_color_policy.sync` before reading the remote presentation.
Host stores this policy and synchronizes new terminal publication. Changed
policy immediately recolors existing owned presentations; identical policy
calls return without walking cells. Theme preview, commit, rollback and removal
of an explicit cursor fallback therefore reach the next paint without a server
frame, render query, input command or timer.

`OwnedColors` retains the original 36-byte C cell and 4-byte provenance per
cell, plus one by-value global metadata record. No FFI pointers survive the
copy. Color projection always starts from original RGB/flags, preserving
inverse/faint precedence and explicit/default color distinctions. Glyph,
cluster, hyperlink and selection storage are not rebuilt on policy changes.
OSC overrides win until OSC 110/111/112 resets them. A frozen grid remains
recolorable after reconnect destroys its source client.

`color_policy_tests.zig` drives canonical frames through Bridge, provider and
Host publication, including OSC reset/reverse and frozen reconnect. The separate
`remote_theme_tests.zig` calls the shipping painter twice around a config change
with no frames. Both assert explicit RGB remains explicit. The named tests
were proved red by restoring `copyClient`'s default-only policy and by removing
the painter hookup, respectively. The historical painter guard recorded the
equivalent missing forwarding in its small helper to survive painter refactors.

Independent review confirmed ownership, lifetime, publication and recoloring
semantics. Its requested invariant comment/assert was added: captured source
colors and owned canvas cells are committed from the same admitted count,
without intervening fallible work.
A final fresh review of immutable snapshot `d68918d8` reported no actionable
findings, including the actual shipping painter test and the frozen-client path.

Manual CC for this follow-up, relative to `72db6128`:

| Function | Before | After |
|---|---:|---:|
| `CanvasStore.copyClient` / internal `copy` / `deinit` | 1 / 5 / 1 | 1 / 5 / 1 |
| Canvas / Host / provider `setColorPolicy` | — | 4 / 3 / 1 |
| `OwnedColors.capture` / `recolor` | — | 3 / 3 |
| `OwnedColors.reserve` / `deinit` | — | 1 / 1 |
| `remote_color_policy.sync` | — | 2 |
| `Host.publishDirty` | 16 | 16 |
| `paintWindow` | 26 | 26 |

The existing Host/painter functions receive branch-free hookup statements;
their separate refactoring remains with the parent integration lane.

## Evidence scope

The Rust fixture generator encodes canonical bootstrap/output frames. Zig
feeds these through the real C client and tests OSC overrides/reset, palette
provenance, inverse/default underline versus equal explicit RGB, faint over
an explicit background, bold policy off/on, cursor blink and primary/tail
coverage, DECSCNM, stale queries and mismatched metadata admission.

These are structural tests against source-confirmed local palette/cursor
semantics. They make no AppKit/CoreText raster or animation claim. Live host
comparison remains a separate serial acceptance step.

### Historical RED evidence

The former `scripts/guards/remote-grid-metadata.guard` recorded the named
metadata test failing when publication returned to the legacy v1-only copy.
The moved `remote-cell-styles` and `remote-cursor-hollow` guards were also
re-run against their new helper locations. All breaks were restored after
proof. These are historical results; the permanent ledger is retired. See
the current [mutation testing policy](../../../docs/TESTING_MUTATIONS.md).

## Review and complexity

Independent review checked Rust/C layout, header-only output validation,
same-pass invalidation and each canonical fixture cell. Its actionable
findings were fixed: cell records now finish every fallible conversion before
either parallel vector is extended, and reset/reverse tests assert individual
default-colored cells as well as global grid colors. Explicit palette/RGB
background-only cells are also covered. Duplicate immutable cursor reads and
the bounded by-value palette copy were informational, not correctness defects.
A final independent review of immutable snapshot `1f8a7934` reported no
actionable findings after rechecking those fixes and the pinned engine source.

No project Zig/Rust CC gate is configured. Manual source counts use one plus
written `if`, loops, `catch`/`orelse`, boolean decisions and non-default switch
arms (excluding Rust closure delimiters). Compared with `27738342`:

| Function | Before | After |
|---|---:|---:|
| `CanvasStore.copyBorrowed` | 7 | 1 |
| `copyClient` / `copyWithMetadata` / internal `copy` | — | 1 / 1 / 5 |
| `copyAppearance` / `cursor` | — | 5 / 5 |
| `cell` / `project` / `foreground` | 3 / — / — | 1 / 7 / 6 |
| `resolveColors` | 3 | 3 |
| metadata `validate` / `validateShape` / `validateCursor` | — | 6 / 6 / 6 |
| metadata `foreground` / `background` / `sameGeneration` | — | 3 / 3 / 4 |
| `Client.reset_borrows` | 1 | 2 |
| `render_grid_view` / `push_flattened_cell` | 2 / 2 | 2 / 2 |
| Rust metadata `publish` / `cell_metadata` | — | 3 / 4 |
| `DefaultColors.read` / metadata C query | — | 2 / 2 |

Other touched constructors, scalar conversions, copy accessors and fixture
generation helpers have CC 1; the metadata read helper has CC 2. The existing
cell shape validation remains the highest helper at CC 8.
