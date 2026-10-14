---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-10-14
---

# Native terminal painter

**TL;DR.** `Terminal` is a `gpui::Render` entity. `Terminal::new` takes
`Arc<Client>`, a runtime `ViewId` and `ResourceId`, and paints that
publication with native GPUI glyphs. It does not look up a string registry.

## Entity contract

```rust
Terminal::new(client, view, terminal, settings, window, cx) -> Self
Terminal::configure(&mut self, settings, window, cx)
Terminal::request(&mut self, action, cx) // once, on this entity
Terminal::focus_handle(&self) -> &FocusHandle
```

`Settings` is ordinary public fields: font, size, line height, cell scales,
theme colours, whether the host already fills the default background
(`host_fills_background`), cursor and blink visibility, focus, Option-as-Alt, size
ownership and non-Command `app_chords`. `configure` and `new` clamp font size
to 6–96, line height to 1–3 and cell scales to 0.5–2. Identity is not a
setting. A replaced view or client is a new entity.

The shell subscribes to `TerminalEvent::Activated` (primary click). File
drops belong to the pane card around the view, not to the view. Command-click
opens an allowlisted link.
Non-Command `app_chords` bubble to the window; Command chords are platform
keystrokes and are not sent to the PTY.

Give the view an explicit bounded width and height. It clips its canvas.
Only the size owner proposes a PTY size, after the painted capacity has held
still.

Every render reacquires and revalidates the slot by borrowing the entity's
`Arc<Client>`. A terminal keeps the scene it last prepared and reuses it while
the acquired frame is the same `Arc`, the bounds and scale factor are
unchanged, and `Settings::paints_like` holds. A new frame with the same
geometry, palette, and paint settings keeps the shaped rows whose cells still
match and reshapes the rest, including a solid block cursor's old and new
rows. A frame that is that screen scrolled by whole rows moves the kept rows
and reshapes only the rows that entered. That search runs when most rows
would otherwise be reshaped. Missing slots and a dropped client clear the canvas. Projection
acknowledgement belongs to [PRESENTATION.md](PRESENTATION.md).

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

`terminal-fixtures` records every glyph while preparing. `Terminal::painted`
reads that record. Production builds record none. **Never enable the feature
in a shipping binary.** Rust unit tests cover scene reuse, colour provenance,
hidden cells and cursor shape.

`tests/pixels.rs` is the pixel oracle, a `harness = false` test binary.
Cargo's default harness runs each `#[test]` on a worker thread, and GPUI's
macOS platform panics ("Mac platform not created on main thread") unless
`MacPlatform` is created on the process main thread. This binary's `main`
is that thread and runs the five fixtures there on every OS. Each one
publishes a real runtime grid into `Terminal` and reads an offscreen frame
from GPUI's headless renderer (`HeadlessAppContext` and
`current_headless_renderer`, behind GPUI `test-support`). They check glyph
columns, a wide character, the block cursor, selection, and theme-palette
remap against true color. Linux needs a Vulkan adapter, hardware or
lavapipe; macOS uses Metal. The on-screen smoke is still the packaged
qualification run.

phux-d4x9.4 remains open for Retina/scale-change GPU evidence, wider
script/box-drawing fidelity, and a graphics product decision.
