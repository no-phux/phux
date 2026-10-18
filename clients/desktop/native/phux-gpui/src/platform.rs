//! The gpui-fast APIs phux's toolchain patches add, in one place.
//!
//! With `patched-gpui` these call the patched methods. Without it the crate
//! builds on the published gpui-fast: [`Raster`] and [`Sprite`] are
//! uninhabited, so `glyph_raster` is always `None` and the painter takes its
//! existing shaped-line path, presentation receipts fall back to the next
//! frame, and macOS keeps press-and-hold.

use gpui::{Bounds, FontId, GlyphId, Hsla, Pixels, Point, Window};
#[cfg(feature = "patched-gpui")]
use std::sync::Arc;

/// A glyph rasterized into the window's sprite atlas (patch 0006).
#[cfg(feature = "patched-gpui")]
pub(crate) type Sprite = gpui::GlyphSprite;
/// What retained sprites stay valid under (patch 0006).
#[cfg(feature = "patched-gpui")]
pub(crate) type Raster = gpui::GlyphRaster;

/// No retained sprites without patch 0006.
#[cfg(not(feature = "patched-gpui"))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Sprite {}
/// No sprite atlas generation without patch 0006: never constructed.
#[cfg(not(feature = "patched-gpui"))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Raster {}

/// The window's retained-glyph inputs, or `None` when glyphs must paint from
/// their shaped lines.
#[cfg(feature = "patched-gpui")]
pub(crate) fn glyph_raster(window: &Window) -> Option<Raster> {
    window.glyph_raster()
}

#[cfg(not(feature = "patched-gpui"))]
pub(crate) fn glyph_raster(_window: &Window) -> Option<Raster> {
    None
}

/// Rasterize one glyph without painting it. Called only while
/// [`glyph_raster`] is `Some`.
#[cfg(feature = "patched-gpui")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_glyph(
    window: &Window,
    origin: Point<Pixels>,
    font_id: FontId,
    glyph_id: GlyphId,
    font_size: Pixels,
    color: Hsla,
    is_emoji: bool,
    clip: Bounds<Pixels>,
) -> gpui::Result<Option<Sprite>> {
    window.resolve_glyph(origin, font_id, glyph_id, font_size, color, is_emoji, clip)
}

#[cfg(not(feature = "patched-gpui"))]
#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_glyph(
    _window: &Window,
    _origin: Point<Pixels>,
    _font_id: FontId,
    _glyph_id: GlyphId,
    _font_size: Pixels,
    _color: Hsla,
    _is_emoji: bool,
    _clip: Bounds<Pixels>,
) -> gpui::Result<Option<Sprite>> {
    Ok(None)
}

/// Paint a row of retained sprites moved by `offset`.
#[cfg(feature = "patched-gpui")]
pub(crate) fn paint_glyph_sprites(
    window: &mut Window,
    sprites: &Arc<[Sprite]>,
    offset: Point<Pixels>,
) {
    window.paint_glyph_sprites(sprites, offset);
}

#[cfg(not(feature = "patched-gpui"))]
pub(crate) fn paint_glyph_sprites(
    _window: &mut Window,
    sprites: &std::sync::Arc<[Sprite]>,
    _offset: Point<Pixels>,
) {
    // An uninhabited slice is empty.
    debug_assert!(sprites.is_empty());
}

/// Whether the window is on screen and not occluded (patch 0001). Without
/// receipts the window's own activity is the best available signal.
#[cfg(feature = "patched-gpui")]
pub(crate) fn visible(window: &Window) -> bool {
    window.native_visibility() == gpui::NativeVisibility::Visible
}

#[cfg(not(feature = "patched-gpui"))]
pub(crate) fn visible(_window: &Window) -> bool {
    true
}

/// Run `callback` once the drawable of the scene being painted was presented
/// on a still-visible window (patch 0001). Without receipts it runs when the
/// next frame begins: that scene was submitted, not proven presented.
#[cfg(feature = "patched-gpui")]
pub(crate) fn on_presented(
    window: &Window,
    callback: impl FnOnce(&mut Window, &mut gpui::App) + 'static,
) {
    window.on_drawable_presented(callback);
}

#[cfg(not(feature = "patched-gpui"))]
pub(crate) fn on_presented(
    window: &Window,
    callback: impl FnOnce(&mut Window, &mut gpui::App) + 'static,
) {
    window.on_next_frame(callback);
}

/// The terminal's input handler. A terminal repeats a held key; patch 0005
/// turns off macOS's press-and-hold accent menu for this handler only.
#[cfg(feature = "patched-gpui")]
pub(crate) fn terminal_input_handler<V: gpui::EntityInputHandler>(
    handler: gpui::ElementInputHandler<V>,
) -> gpui::ElementInputHandler<V> {
    handler.with_apple_press_and_hold_enabled(false)
}

#[cfg(not(feature = "patched-gpui"))]
pub(crate) fn terminal_input_handler<V: gpui::EntityInputHandler>(
    handler: gpui::ElementInputHandler<V>,
) -> gpui::ElementInputHandler<V> {
    handler
}
