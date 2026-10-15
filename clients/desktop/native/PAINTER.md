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

Each time this view renders, it reacquires and revalidates the slot by
borrowing the entity's `Arc<Client>`. While the connection, publication, and
prediction fence are unchanged, that reacquire is an atomic publication read:
it does not take an owner-thread turn or wait behind another pane's output.
A new publication, reconnect, detach, or prediction fence takes the full
owner turn and validates replica membership, readiness, and sequence.
A terminal keeps the scene it last prepared and reuses it while the acquired frame is the same `Arc`, the bounds
and scale factor are unchanged, and `Settings::paints_like` holds. A new frame
with the same geometry, palette, and paint settings keeps the shaped rows
whose cells still match and reshapes the rest, including a solid block
cursor's old and new rows. The runtime reports row damage while one
presentation remains the projector's reader, and forces full damage when
switching between the default presentation and a view. For consecutive frame
generations in one replica, `GridDamage::Rows` skips comparisons of clean
rows; skipped generations and full damage compare the grid so a missed update
cannot leave stale pixels. Predicted text rows are dirty both when overlaid
and when restored. A whole-row scroll moves the kept rows and reshapes only
those that entered. That search runs when most rows would otherwise be
reshaped. The terminal is a cached GPUI view, so a chrome refresh reuses
that paint until the terminal itself is notified. The desk withholds that
notification until the session is attached, then notifies placed terminals
once. A rejected reacquire keeps the last scene while the bounds still
match, so a reconnect does not blank the pane. A missing scene, or bounds
that no longer match, clears the canvas. An ASCII run is one shaped line on
the cell grid, not one paint call per glyph.
Projection acknowledgement belongs to [PRESENTATION.md](PRESENTATION.md).

## Geometry and text

GPUI shapes text with the cell's bold or italic font and platform fallback.
Terminal columns, not glyph advances, set each origin. Wide heads occupy two
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
is that thread and runs the fixtures there on every OS. Each one
publishes a real runtime grid into `Terminal` and reads an offscreen frame
from GPUI's headless renderer (`HeadlessAppContext` and
`current_headless_renderer`, behind GPUI `test-support`). They check glyph
columns, a wide character, the block cursor, selection, theme-palette
remap against true color, and a sparse live-output repaint that leaves a
neighboring row untouched. Linux needs a Vulkan adapter, hardware or
lavapipe; macOS uses Metal. The on-screen smoke is still the packaged
qualification run.

## Headless measurements

`just desktop-perf-headless` runs the same main-thread GPUI pixel binary in
benchmark mode. No CLI, server, socket, visible window, or production profile:
an embedded runtime feeds an 80×24 terminal, and GPUI draws it offscreen.
Three JSON lines report 128 cached redraws, one-row updates, and sustained
full-width scrolling. `tick_us` covers the in-process VT feed (for changed
frames) plus the draw; `stages` separate acquisition, scene preparation,
painting and presentation handoff. `scene` counts reused/patched scenes and
shaped lines, and `process` carries CPU and peak RSS. A render count check and
screenshots outside the timed path reject an empty/no-op benchmark.

The default test profile is quick for iteration; use
`just desktop-perf-headless release` for comparable optimized runs. Compare
back-to-back runs on the same machine without concurrent builds. This measures
the terminal view, not the desk's chrome, input-to-pixel latency, compositor
presentation or production thread contention. Use `perf-bench.ts` with a
visible desktop for those; it refuses to report a locked-display run that
never drew a frame. The hosted packaged-app smoke remains the visual release
qualification.

phux-d4x9.4 remains open for Retina/scale-change GPU evidence, wider
script/box-drawing fidelity, and a graphics product decision.
