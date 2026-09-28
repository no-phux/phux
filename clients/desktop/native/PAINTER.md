---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-27
---

# Native terminal painter

**TL;DR.** `terminal::install(registry)` registers `phux-terminal`. The element
resolves the existing NAPI client registry, acquires an immutable runtime
`ViewId` publication, and shapes and paints native GPUI glyphs. It is a
verified initial painter, not closure of phux-d4x9.4.

## Element contract

Give the surface an explicit bounded width and height. It clips its canvas and
never resizes a runtime view or the shared PTY.

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

Font/theme assignments replace their whole group; null resets it. Font size is
bounded to 6-96 and line height to 1-3. Invalid identity props invalidate the
surface. Terminal-set default colors beat theme defaults; explicit palette/RGB
cells keep their resolved colors; runtime selection flags drive selection paint.

The host's sole wake owner drains `takeEvents` and invalidates affected nodes;
local scroll/selection/settings changes must invalidate too. Every render
reacquires the slot and fully redraws, so no cached generation can hide
removal, replacement or setting changes. Missing slots and stale clients clear
the canvas. Projection acknowledgement belongs to [PRESENTATION.md](PRESENTATION.md).

## Geometry and text

GPUI shapes each cell grapheme with its bold/italic font and platform fallback;
terminal columns, not glyph advances, set each origin. Wide heads occupy two
columns and spacers paint nothing. One device-pixel-rounded cell geometry drives
baseline, backgrounds, decorations, cursor, clipping and `Geometry::hit` /
`cell_bounds` for input.

Five underline styles, strike, overline, inverse, faint, invisible, selection,
cursor shape/width and true color are native. Faint glyphs paint inside a
`Div::opacity` scope because GPUI's color-emoji path ignores `TextRun.color`.
Hyperlink metadata stays in the retained `GridFrame`. Cross-cell ligatures,
procedural box drawing, blink timers, and Kitty/sixel graphics (`GridFrame`
carries no image data) are not provided.

## Fixture

The `terminal-fixtures` feature exposes `terminalFixture*` NAPI observation
helpers; **never enable it in the production addon**. Build and run with
`just desktop-native-fixtures-build` and `just desktop-native-painter-test`.
The runner starts an isolated real server with a Python VT-producing PTY and
two native elements on distinct views. It asserts painted Unicode positions,
color, invisible text, hyperlinks, scroll/selection isolation, font/theme
changes, clipping, alternate screen, removed slots, stale clients and teardown.
A decoded-pixel oracle then checks decorations, cursor shapes, emoji dimming,
wide cells and an out-of-bounds clipping sentinel; only a fully passing run
writes `.cache/terminal-painter/pixel-receipt.json`. Set
`PHUX_TERMINAL_ALT_WITH_SELECTION=1` to exercise alternate screen with a
retained selection.

phux-d4x9.4 remains open for Retina/scale-change GPU evidence (the offscreen
renderer runs at scale 1), resize and bootstrap replacement integration, wider
script/box-drawing fidelity, and a graphics product decision.
