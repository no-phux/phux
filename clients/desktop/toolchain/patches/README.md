# Local GPUI patches

Ordered, reviewed patches over the pinned gpui-fast 0.1.9
crates, applied by `just desktop-source` (`tools/src/desktop/`). `../source.json` records each
patch's SHA-256. Paths are relative to `toolchain/gpui/` (one directory per
crate). None has been published upstream.

## Series

| Patch | Applies to | What it does |
|---|---|---|
| `0001-drawable-presented.patch` | `gpui-fast`, `gpui-fast-apple`, `gpui-fast-macos` | `Window::on_drawable_presented` and `Window::native_visibility`. Metal reports the exact drawable a scene was submitted on through its presented handler; the callback runs only when that drawable was presented on a still-visible window, and never for paint, `on_next_frame` or command-buffer completion. A window with gpui-fast's native composition enabled draws several surfaces, reports no drawable and drops the callback; the desktop does not enable composition. The terminal's delivery-fence recovery (`native/phux-gpui/PRESENTATION.md`) and desktop evidence depend on it. |
| `0002-window-capture.patch` | same | `Window::render_to_image` outside `test-support`: renders the current scene into an offscreen texture at the drawable's size, so no drawable is taken from the swap chain. Used only by `--evidence` qualification captures. |
| `0004-native-glass-surfaces.patch` | `gpui-fast`, `gpui-fast-macos` | Optional compositor-backed glass and complete-window opacity. macOS wraps the existing GPUI rendering view in `NSGlassEffectView`, retaining its Metal layer and input routing. Native hierarchy changes happen outside the state mutex to allow synchronous AppKit callbacks. Other platforms retain the host's solid popover fallback. |
| `0006-bulk-glyph-sprites.patch` | `gpui-fast`, `gpui-fast-apple` | Retained glyph sprites. `Window::resolve_glyph` rasterizes a glyph into the atlas without painting it, with the same origin quantization as `paint_glyph`; `Window::paint_glyph_sprites` paints a slice of them, moved by whole device pixels, as one scene operation inside a layer, which retained views replay whole and gpui-fast's scroll-layer, overlay, tile and rounded-clip passes expand to primitives; `Window::glyph_raster` says what they stay valid under (window, atlas generation, scale factor, subpixel inputs). `PlatformAtlas::generation` advances whenever a tile is freed, so a retained sprite never outlives its tile. Sprites of one draw order sort by texture, the only key batching uses, and keep their paint order within it, which keeps a layer's sprites one presorted run. The terminal painter (`native/phux-gpui/PAINTER.md`) depends on it. |
| `0007-clearable-native-glass.patch` | `gpui-fast`, `gpui-fast-macos` | Lets a long-lived window wear and shed native glass. `Window::clear_glass_effect` moves the GPUI view back into the content view, drops the material and the forced appearance, and reapplies the recorded background appearance. `set_glass_effect` clears the window first, removes any blur in every path, and restyles an installed material instead of stacking another; a blur is not added beneath it. `Window::set_glass_tint` sets `tintColor` where AppKit has it. The main window's Liquid Glass skin and its switch to an opaque terminal depend on it. |
| `0011-native-fullscreen-presentation.patch` | `gpui-fast-macos` | The native fullscreen delegate requests menu-bar and Dock auto-hide, even when the system preference keeps the menu bar visible. It preserves AppKit's other presentation options and native Space/restore behavior, without changing windowed presentation or application-wide preferences. |
| `0012-rounded-glass-sheds-frame-shadow.patch` | `gpui-fast`, `gpui-fast-macos` | A rounded glass material is the window's silhouette, so `set_glass_effect` with a nonzero corner radius turns off the window's system shadow and `clear_glass_effect` restores it. The shadow and outline AppKit draws for GPUI's titled panel follow the frame's own corners and size, and showed as a second, mismatched rounded surface behind every glass menu. Glass at radius zero (the main window) keeps its shadow. |
| `0013-arena-memory-retention.patch` | `gpui-fast` | Reclaims transient element/event arena peaks after five seconds continuously below a quarter of capacity, keeping twice recent usage. Reclamation runs after values are dropped at a safe outermost clear; larger draws restart the grace period to preserve uneven multiwindow cadences. |

## Rules

- Change patches through `desktop-source --edit` / `--export`, never by hand.
- A patch states its reason here and is the smallest change that works.
- Upstream candidates: these are general GPUI features; propose them to Zed
  when their shape has settled.

`0020-runtime-application-icon.patch` exposes `App::set_app_icon` with AppKit
and Windows implementations. Appearance changes the running app icon without
modifying the signed application bundle. Windows retains its owned HICON until
all current windows have replaced it and applies it to newly opened windows.
The Linux host owns its user-local desktop-entry icon.
