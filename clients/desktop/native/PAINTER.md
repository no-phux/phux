---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-23
---

# Native terminal painter

**TL;DR.** `terminal::install(registry)` registers `phux-terminal`. The element
resolves the existing NAPI client registry, acquires an immutable runtime
`ViewId` publication, and shapes/paints native GPUI glyphs. This is a verified
initial painter, not closure of phux-d4x9.4; remaining acceptance is below.

## Host integration

The parent host owns its manifest, lockfile, generated bindings, and installer.
This module needs these additions to `clients/desktop/native/Cargo.toml`:

```toml
[dependencies]
phux-client-core = { path = "../../../crates/phux-client-core" }
phux-client-ffi = { path = "../../../crates/phux-client-ffi", default-features = false, features = ["napi"] }

[features]
terminal-fixtures = []
```

The existing runtime, GPUIX, `serde_json`, and NAPI dependencies suffice for the
rest. Core is needed for the authoritative cell flag/color-provenance constants;
FFI is needed to resolve the **same** connected client as the desktop commands.
There is no additional client registry, transport, listener, or event drain.

In `src/lib.rs`, export `pub mod terminal;`. Call `terminal::install(registry)`
alongside `probe::install(registry)` inside the **one**
`native_extensions::install` callback. Keep both bindings in the same loaded
addon. The exact GPUI type comes from the pinned native-extensions patch.
The existing probe module must be public (or have a test-specific dead-code
disposition) to run all-target strict Clippy: NAPI omits registration under
`cfg(test)`, leaving its private exported functions otherwise unused.

## Element contract

Give the surface an explicit bounded width and height through normal styles.
It clips its canvas and does not resize any runtime view or the shared PTY.

| Prop | Value / default |
| --- | --- |
| `clientHandle` | Existing `DesktopClient.handle` string |
| `terminalId` | Canonical binding resource ID, checked against the acquired frame |
| `viewId` | Decimal **string** encoding a real runtime-created nonzero `ViewId` |
| `font` | `{family: "Menlo", size: 14, lineHeight: 1.25}`; logical pixels, multiplier |
| `theme` | Optional `#RRGGBB` foreground/background/cursor/selectionForeground/selectionBackground |
| `focused` | `true`; false paints a hollow cursor |
| `cursorVisible` | `true`; application visibility gate |
| `blinkVisible` | `true`; external blink phase for SGR blink and blinking cursor |
| `paintRevision` | Opaque invalidation token; never compared with runtime generation |

Font/theme assignments replace their whole settings group; null resets it.
Font size is bounded to 6–96, line-height multiplier to 1–3. Invalid identity
props invalidate the surface instead of keeping an old identity. Terminal-set
default colors take precedence over theme defaults. Explicit palette/RGB cells
retain their resolved colors. Runtime selection flags determine selection paint.

The host's sole wake owner drains `takeEvents` and invalidates affected native
nodes. Local scroll/selection/settings operations must invalidate them too;
they need no socket frame. Every render reacquires the slot and fully redraws.
Missing/removed slots and stale clients clear the canvas. No cached generation
can hide removal, equal-generation replacement, skipped publications, or changes
to scale/font/theme/selection. No projection acknowledgement is emitted here.
Any later paint-confirm integration must associate the actual frame with a
captured runtime projection fence and use `acknowledge_projection_if` after
successful paint; unconditionally acknowledging during build is invalid.

## Geometry and text

GPUI shapes each complete cell grapheme with its bold/italic font and platform
fallback. Natural shaping preserves combining marks; terminal columns, not
glyph advances, determine each next origin. Wide heads occupy two columns;
both spacer variants paint no glyph. One measured, device-pixel-rounded cell
geometry determines baseline, backgrounds, decorations, cursor, clipping, and
the `Geometry::hit` / `cell_bounds` seam for later input/IME work.

Underline (single/double/curly/dotted/dashed), strike, overline, inverse, faint,
invisible, selection, cursor shape/width, and resolved true color are native.
Hyperlink metadata remains in the retained `Arc<GridFrame>` for native hit use;
this lane does not open URLs or provide mouse/keyboard/IME actions. Curly
underlines are pixel-stepped. Cursor/SGR blink has an explicit phase gate but
no timer yet. Cross-cell ligatures and procedural box-drawing are not provided.

`PaintedFrame`/`PaintedGlyph` are native observation types. Each custom element
owns its record; paint closures hold a weak reference. Teardown releases it,
including before a host ID is reused. Records are written after actual paint,
not from a prepaint string walk. GPUIX's generic text registry is intentionally
not populated: its variable-width selection is not terminal selection.

## Validation and artifacts

Build the combined addon in release/LTO with
`--features terminal-fixtures,gpuix-native/test-support`, Rust 1.98.1, the Apple
toolchain helper, the pinned private GPUIX checkout, and a private Cargo target.
The optional feature exposes `terminalFixture*` NAPI test helpers, including
painted glyphs and positions. **Do not enable it in the production addon.**
Those helpers never supply cell data to rendering. Production has no text/grid
export from this module. Regenerate parent bindings separately per host policy.

```sh
PHUX_DESKTOP_ADDON=/absolute/path/to/release-fixture.node \
  bash clients/desktop/tests/native/terminal-run.sh
```

The runner generates an ignored standalone server harness, starts an isolated
real phux server with one Python VT-producing PTY, and launches the combined
addon in a fresh Bun process with an isolated HOME/XDG. Two native elements
show distinct runtime views. Assertions cover native-painted Unicode positions
(`A界é🙂Z`), true color/inverse, invisible text, hyperlinks, scroll/selection
isolation, font/theme changes, clipping, output, alternate/main screen without
selection, removed slots, stale clients, numeric identity refusal, teardown and
host-ID reuse. `terminal-pty.py` supplies dense color and decoration fixtures.

Artifacts land in `clients/desktop/.cache/terminal-painter/`:
`fidelity.png`, `two-views.png`, `font-clipping.png`, `initial.json`, and
`timings.json`. The first measured 60×16 frame paints 484 graphemes per surface.
Thirty warm two-surface full redraws measured roughly 2.50 ms median / 2.58 ms
p95 flush, including both surfaces; left-surface preparation was 154/163 µs
median/p95 and paint submission 548/561 µs. These are local scale-1 measurements,
not GPU completion latency or a cross-machine performance budget.

Seven Rust unit tests cover color provenance/inverse/selection/faint, blinking,
wide-tail cursor geometry, fractional-scale hit geometry, bounded/reset font
settings, and lossless view identity. Run `cargo test --lib` and
`cargo clippy --all-targets --all-features -- -D warnings` against the integrated
host manifest. Production-only `cargo clippy --lib --no-default-features --
-D warnings` is also required.

Initial Lizard measurement covered 61 new Rust functions: mean CCN 2.4, maximum 8 (fixture
selection); production maximum 7 (`prepare_cursor`, `cell_colors`). There was
no prior painter-function baseline. No function crosses the skill's 10-point
refactor threshold.

## Pixel oracle and review corrections

Independent parent review found that GPUI's color-emoji path ignores
`TextRun.color`, so foreground alpha alone failed to dim emoji. Faint glyphs
now paint inside GPUI's public `Div::opacity` scope; the glyph's text color
stays opaque to avoid dimming monochrome text twice. Only faint glyphs need
these transient native elements. Decoration alpha is applied once separately.

The real-PTY runner also decodes native screenshots through `png = "=0.18.1"`
in its isolated test harness. This existing GPUI dependency is test-only here;
there is no new production dependency or image-query API. Region checks use
device/font geometry, tolerant color counts, relative glyph energy, and sibling
raster comparisons rather than platform-specific golden font bytes. They test
true color/inverse, background-only cells, seven decorations, all four cursor
shapes plus hidden cursor, color emoji/mono dimming, wide-cell spacing,
combining/composed equivalence, Greek/math fallback presence, partial glyphs,
and a magenta sentinel outside both surface clipping edges.

The old addon fails the pixel oracle with faint-emoji intensity ratio **1.0**.
The fixed addon passes with emoji **0.5010** and mono **0.5017**. Two temporary
native renderer mutations were also tested and restored: omitting decorations
fails the expected-band assertion; disabling surface clipping fails the outside
sentinel assertion. Thus these checks detect missing/overflowing raster output
even when native text/position observations remain valid.

Additional artifacts are `pixels-{block,hollow,hidden,bar,underline,clipped}.png`,
`pixel-geometry.json`, and `pixel-receipt.json`. Every run removes the previous
receipt first; only an entirely passing decoded-pixel run writes a new receipt.
Seven native unit tests and strict Clippy cover the fix; the standalone pixel
harness also passes all-target strict Clippy. No runtime code changed here.

Follow-up complexity (Lizard): `Prepared::paint` **5 → 5**, `cell_colors`
**7 → 7**; new opacity preparation/paint helpers are **1–3** and the new
pixel-oracle functions are **1–3**. All remain below CCN 10.

## Open acceptance and graphics disposition

The selected-view alternate-screen regression is retained as an explicit mode:
set `PHUX_TERMINAL_ALT_WITH_SELECTION=1` on the runner. On this branch's original runtime
revision, the selected right view remains on its main-screen generation while
the left advances to alternate (final run: right 6, left 8; counts vary with
publication batching). Native acquisition confirms those
publication generations, so repainting cannot fix it. `failure.json` and
`failure.png` record the discrepancy; phux-d4x9.17 has the reproduction.

The integrating parent subsequently reported an independent **Metal/real-PTY
PASS** with selection retained after runtime fix `bb5f28db` (parent integration
`08f88bcc0`), using `PHUX_TERMINAL_ALT_WITH_SELECTION=1 just
desktop-native-painter-test`. Its receipt is the parent worktree's
`clients/desktop/.cache/painter-alt-validation.log`. Those runtime commits are
not duplicated in this painter-only branch; its original failure is historical
evidence, not a claim that the parent runtime remains broken.

`GridFrame` supplies cells, text/hyperlink spans, cursor, colors, damage, and
scrollbar facts. It contains **no image bytes or image placements**. Kitty/sixel
graphics are therefore **unsupported by this painter**. Engine feature flags
do not imply a native graphics renderer; no substitute support is claimed.

phux-d4x9.4 remains open for real Retina/scale-change
GPU evidence (the offscreen renderer runs at scale 1), targeted resize and
replica/bootstrap replacement integration, wider fallback/shaping/script and
box-drawing fidelity, and graphics product disposition. Parent independent
review prompted the fixes above; the parent owns final review of this follow-up.
Input/IME and blink scheduling
belong to the separate input/application integration lanes.
