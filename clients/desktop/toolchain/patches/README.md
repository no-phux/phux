---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-10-15
---

# Local GPUI patches

**TL;DR.** Ordered, reviewed patches over the pinned `gpui-pre` 0.3.8
snapshot crates, applied by `../bootstrap.py`. `../source.json` records each
patch's SHA-256. Paths are relative to `toolchain/gpui/` (one directory per
crate). None has been published upstream.

## Series

| Patch | Applies to | What it does |
|---|---|---|
| `0001-drawable-presented.patch` | `gpui-pre`, `gpui-pre-apple`, `gpui-pre-macos` | `Window::on_drawable_presented` and `Window::native_visibility`. Metal reports the exact drawable a scene was submitted on through its presented handler; the callback runs only when that drawable was presented on a still-visible window, and never for paint, `on_next_frame` or command-buffer completion. The terminal's delivery-fence recovery (`native/PRESENTATION.md`) and desktop evidence depend on it. |
| `0002-window-capture.patch` | same | `Window::render_to_image` outside `test-support`: renders the current scene into an offscreen texture at the drawable's size, so no drawable is taken from the swap chain. Used only by `--evidence` qualification captures. |
| `0004-native-glass-surfaces.patch` | `gpui-pre`, `gpui-pre-macos` | Optional compositor-backed glass and complete-window opacity. macOS wraps the existing GPUI rendering view in `NSGlassEffectView`, retaining its Metal layer and input routing. Native hierarchy changes happen outside the state mutex to allow synchronous AppKit callbacks. Other platforms retain the host's solid popover fallback. |

## Rules

- Change patches through `bootstrap.py --edit` / `--export`, never by hand.
- A patch states its reason here and is the smallest change that works.
- Upstream candidates: these are general GPUI features; propose them to Zed
  when their shape has settled.
