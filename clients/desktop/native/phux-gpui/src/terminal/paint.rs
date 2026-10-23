//! Native terminal scenes: preparation, incremental retention, and drawing.

use std::{
    cell::RefCell,
    rc::Rc,
    sync::{Arc, LazyLock},
    time::Instant,
};

use super::{Settings, geometry::Geometry};
use gpui::prelude::*;
use gpui::{
    Bounds, ContentMask, FontFeatures, Hsla, Pixels, ShapedLine, SharedString, fill, point, px,
    size,
};
use phux_client_core::grid::{CELL_OVERLINE, CELL_STRIKETHROUGH};
pub(super) use phux_client_runtime::draw::cursor_span;
use phux_client_runtime::draw::{self, BlockRegion, Theme, block_regions, cursor_head};
use phux_client_runtime::publication::{Cell, CellMetadata, CursorStyle, GridDamage, GridFrame};

mod appearance;
mod builder;
mod damage;
mod decorations;
mod drawing;
mod model;
mod preparation;
mod preview;
mod scrolling;
mod shaping;

use appearance::{
    cell_colors, cursor_color, cursor_visible, hidden, prepare_cursor, solid_cursor, style,
};
use damage::{
    color_bytes, has_fringe, painted_cell, painted_text, patch, rows_paint_alike, solid_head,
    text_span,
};
use decorations::block_bounds;
use drawing::each_glyph;
pub(super) use drawing::shifted;
use model::{
    BackgroundRun, Builder, CellColors, Fonts, Glyph, GlyphRun, Ink, OpacityLayer, PendingRun,
    RowInk, RowQuad, RowSprites,
};
pub use model::{GlyphObservation, Observation};
pub(super) use model::{Prepared, Scene, SceneCache};
use preparation::opacity_layers;
pub(super) use preparation::prepare;
pub(super) use preview::prepare_preview;
use scrolling::{rows_after_scroll, scroll_of, shift_scene};

/// Where a laser beam lands, and its colour: the focused, visible cursor.
pub(super) fn laser_target(
    frame: &GridFrame,
    settings: &Settings,
) -> Option<(super::laser::Spot, Hsla)> {
    (settings.cursor_laser && settings.focused && cursor_visible(frame, settings))
        .then(|| (cursor_span(frame), cursor_color(frame, settings)))
}

#[cfg(test)]
mod tests;
