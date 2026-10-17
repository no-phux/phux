---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-10-16
---

# Local GPUI patches

**TL;DR.** Ordered, reviewed patches over the pinned gpui-fast 0.1.9
crates, applied by `../bootstrap.py`. `../source.json` records each
patch's SHA-256. Paths are relative to `toolchain/gpui/` (one directory per
crate). None has been published upstream.

## Series

| Patch | Applies to | What it does |
|---|---|---|
| `0001-drawable-presented.patch` | `gpui-fast`, `gpui-fast-apple`, `gpui-fast-macos` | `Window::on_drawable_presented` and `Window::native_visibility`. Metal reports the exact drawable a scene was submitted on through its presented handler; the callback runs only when that drawable was presented on a still-visible window, and never for paint, `on_next_frame` or command-buffer completion. A window with gpui-fast's native composition enabled draws several surfaces, reports no drawable and drops the callback; the desktop does not enable composition. The terminal's delivery-fence recovery (`native/PRESENTATION.md`) and desktop evidence depend on it. |
| `0002-window-capture.patch` | same | `Window::render_to_image` outside `test-support`: renders the current scene into an offscreen texture at the drawable's size, so no drawable is taken from the swap chain. Used only by `--evidence` qualification captures. |
| `0004-native-glass-surfaces.patch` | `gpui-fast`, `gpui-fast-macos` | Optional compositor-backed glass and complete-window opacity. macOS wraps the existing GPUI rendering view in `NSGlassEffectView`, retaining its Metal layer and input routing. Native hierarchy changes happen outside the state mutex to allow synchronous AppKit callbacks. Other platforms retain the host's solid popover fallback. |
| `0006-bulk-glyph-sprites.patch` | `gpui-fast`, `gpui-fast-apple` | Retained glyph sprites. `Window::resolve_glyph` rasterizes a glyph into the atlas without painting it, with the same origin quantization as `paint_glyph`; `Window::paint_glyph_sprites` paints a slice of them, moved by whole device pixels, each as an ordinary sprite primitive, so retained views, scroll layers and replay treat it like any painted glyph; `Window::glyph_raster` says what they stay valid under (window, atlas generation, scale factor, subpixel inputs). `PlatformAtlas::generation` advances whenever a tile is freed, so a retained sprite never outlives its tile. The terminal painter (`native/PAINTER.md`) depends on it. |
| `0007-clearable-native-glass.patch` | `gpui-fast`, `gpui-fast-macos` | Lets a long-lived window wear and shed native glass. `Window::clear_glass_effect` moves the GPUI view back into the content view, drops the material and the forced appearance, and reapplies the recorded background appearance. `set_glass_effect` clears the window first, removes any blur in every path, and restyles an installed material instead of stacking another; a blur is not added beneath it. `Window::set_glass_tint` sets `tintColor` where AppKit has it. The main window's Liquid Glass skin and its switch to an opaque terminal depend on it. |

## Rules

- Change patches through `bootstrap.py --edit` / `--export`, never by hand.
- A patch states its reason here and is the smallest change that works.
- Upstream candidates: these are general GPUI features; propose them to Zed
  when their shape has settled.
