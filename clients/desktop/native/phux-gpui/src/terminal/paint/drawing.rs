//! Paint prepared native quads, retained sprites, and shaped glyph fallbacks.

use super::*;

impl Prepared {
    /// Cell backgrounds, a solid cursor and glyphs share one GPUI layer, so
    /// they take one draw order instead of one bounds-tree insertion each;
    /// GPUI draws a layer's quads before its sprites. Decorations, procedural
    /// blocks and the other cursors share a second layer above it.
    pub fn paint(
        mut self,
        bounds: Bounds<Pixels>,
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) -> Observation {
        let started = Instant::now();
        let Some(scene) = self.scene.take() else {
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                if let Some(fallback) = self.fallback {
                    window.paint_quad(fill(bounds, fallback));
                }
            });
            return self.finish(started);
        };
        let mut layers = std::mem::take(&mut self.layers).into_iter();
        let shift = self.shift;
        let moved = |bounds: Bounds<Pixels>| shifted(bounds, shift);
        // Rows below the viewport are the fringe, visible only while shifted.
        // The shifted grid is clipped to the rows, not the padding under them.
        let rows = scene.frame.rows;
        let shown = |row: u16| row < rows || shift != px(0.);
        let grid = if shift == px(0.) {
            bounds
        } else {
            let bottom = scene
                .geometry
                .cell_bounds(rows, 0, 1)
                .top()
                .min(bounds.bottom());
            Bounds::from_corners(bounds.origin, point(bounds.right(), bottom))
        };
        let has_images = !scene.frame.images.is_empty();
        let hosted = scene.settings.host_fills_background
            && scene.settings.background == Some(scene.background);
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            if has_images {
                window.paint_layer(bounds, |window| {
                    if !hosted {
                        window.paint_quad(fill(bounds, scene.background));
                    }
                });
                window.with_content_mask(Some(ContentMask { bounds: grid }), |window| {
                    if let Err(error) = images::paint(
                        &scene,
                        images::Layer::BelowBackground,
                        shift,
                        grid,
                        window,
                        cx,
                    ) {
                        self.report.error = Some(error);
                    }
                    window.paint_layer(bounds, |window| {
                        for quad in scene.backgrounds.iter().filter(|quad| shown(quad.row)) {
                            window.paint_quad(fill(moved(quad.bounds), quad.color));
                        }
                        images::mask_explicit_backgrounds(&scene, shift, window);
                    });
                    if let Err(error) =
                        images::paint(&scene, images::Layer::BelowText, shift, grid, window, cx)
                    {
                        self.report.error = Some(error);
                    }
                });
            }
            window.paint_layer(bounds, |window| {
                if !has_images && !hosted {
                    window.paint_quad(fill(bounds, scene.background));
                }
                window.with_content_mask(Some(ContentMask { bounds: grid }), |window| {
                    if !has_images {
                        for quad in scene.backgrounds.iter().filter(|quad| shown(quad.row)) {
                            window.paint_quad(fill(moved(quad.bounds), quad.color));
                        }
                    }
                    for (bounds, color) in &scene.cursor_blocks {
                        window.paint_quad(fill(moved(*bounds), *color));
                    }
                    for row in scene.sprites.iter().filter(|row| shown(row.row)) {
                        crate::platform::paint_glyph_sprites(
                            window,
                            &row.sprites,
                            point(px(0.), row.dy + shift),
                        );
                    }
                    let geometry = scene.geometry;
                    for ink in &scene.inks {
                        let result = match &ink.ink {
                            Ink::Glyph(glyph) if glyph.opacity < 1. => {
                                let layer = layers
                                    .next()
                                    .ok_or_else(|| "opacity layer missing".to_owned());
                                if !shown(ink.row) {
                                    continue;
                                }
                                layer.and_then(|layer| layer.paint(window, cx))
                            }
                            _ if ink.resolved || !shown(ink.row) => continue,
                            Ink::Glyph(glyph) => glyph.paint(glyph.color, geometry, shift, window),
                            Ink::Run(run) => run.paint(geometry, shift, window),
                        };
                        if let Err(error) = result {
                            self.report.error = Some(error);
                        }
                    }
                });
            });
            window.paint_layer(bounds, |window| {
                window.with_content_mask(Some(ContentMask { bounds: grid }), |window| {
                    for quad in scene.decorations.iter().filter(|quad| shown(quad.row)) {
                        window.paint_quad(match quad.bend {
                            None => fill(moved(quad.bounds), quad.color),
                            Some(bend) => bent(moved(quad.bounds), bend, quad.color),
                        });
                    }
                    for (bounds, color) in &scene.cursor_overlays {
                        window.paint_quad(fill(moved(*bounds), *color));
                    }
                });
            });
            if has_images {
                window.with_content_mask(Some(ContentMask { bounds: grid }), |window| {
                    if let Err(error) =
                        images::paint(&scene, images::Layer::AboveText, shift, grid, window, cx)
                    {
                        self.report.error = Some(error);
                    }
                });
            }
        });
        self.report.shift = shift;
        self.report.geometry = scene.geometry;
        self.report.frame = Some(Arc::clone(&scene.frame));
        if cfg!(feature = "terminal-fixtures") {
            self.report.glyphs.clone_from(&scene.glyphs);
        }
        self.finish(started)
    }

    fn finish(mut self, started: Instant) -> Observation {
        self.report.paint_micros = started.elapsed().as_micros();
        crate::perf::PAINT.record_elapsed(started);
        self.report
    }
}

impl GlyphRun {
    /// `force_width` already put each glyph on its column, so the run is one
    /// pass over its glyphs, clipped to its cells.
    fn paint(
        &self,
        geometry: Geometry,
        shift: Pixels,
        window: &mut gpui::Window,
    ) -> Result<(), String> {
        let bounds = shifted(self.bounds, shift);
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            paint_glyphs(&self.line, bounds, &self.colors, geometry, true, window)
        })
    }
}

impl Glyph {
    /// Paint the glyph clipped to its cells, monochrome glyphs in `color`.
    fn paint(
        &self,
        color: Hsla,
        geometry: Geometry,
        shift: Pixels,
        window: &mut gpui::Window,
    ) -> Result<(), String> {
        let bounds = shifted(self.bounds, shift);
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            paint_glyphs(
                &self.line,
                bounds,
                &[(self.line.len(), color)],
                geometry,
                false,
                window,
            )
        })
    }

    pub(super) fn opacity_layer(
        &self,
        geometry: Geometry,
        shift: Pixels,
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) -> OpacityLayer {
        let glyph = self.clone();
        let bounds = shifted(self.bounds, shift);
        let result = Rc::new(RefCell::new(None));
        let output = result.clone();
        // ShapedLine's color emoji path ignores TextRun.color. Div::opacity is
        // GPUI's public entry to the window opacity stack, used by both glyph
        // paths. The colour stays opaque so monochrome glyphs are not dimmed twice.
        let mut element = gpui::div()
            .opacity(self.opacity)
            .w(bounds.size.width)
            .h(bounds.size.height)
            .child(
                gpui::canvas(
                    |_, _, _| (),
                    move |_, (), window, _| {
                        *output.borrow_mut() =
                            Some(glyph.paint(glyph.color, geometry, shift, window));
                    },
                )
                .size_full(),
            )
            .into_any_element();
        element.prepaint_as_root(
            bounds.origin,
            bounds.size.map(gpui::AvailableSpace::Definite),
            window,
            cx,
        );
        OpacityLayer { element, result }
    }
}

/// `bounds` moved down by `shift` (up when negative).
pub(in crate::terminal) fn shifted(bounds: Bounds<Pixels>, shift: Pixels) -> Bounds<Pixels> {
    Bounds::new(point(bounds.left(), bounds.top() + shift), bounds.size)
}

impl OpacityLayer {
    fn paint(mut self, window: &mut gpui::Window, cx: &mut gpui::App) -> Result<(), String> {
        self.element.paint(window, cx);
        self.result
            .borrow_mut()
            .take()
            .ok_or("opacity layer did not paint")?
    }
}

/// One glyph of a shaped line, placed on its cell.
pub(super) struct PlacedGlyph {
    pub(super) origin: gpui::Point<Pixels>,
    pub(super) font_id: gpui::FontId,
    pub(super) id: gpui::GlyphId,
    pub(super) color: Hsla,
    pub(super) is_emoji: bool,
    /// The cell a non-ASCII run glyph is clipped to, inside the line's bounds.
    pub(super) clip: Option<Bounds<Pixels>>,
}

/// Visit `line`'s inked glyphs from the left of `bounds`, every glyph on the
/// row baseline. `colors` gives each span's (end byte, colour); colour emoji
/// ignore it. Spaces have no ink and are skipped. With `clip_cells`, each
/// glyph is one cell of a run and a non-ASCII glyph (box drawing, symbols)
/// is clipped to its own cell, as if it were shaped alone.
pub(super) fn each_glyph(
    line: &ShapedLine,
    bounds: Bounds<Pixels>,
    colors: &[(usize, Hsla)],
    geometry: Geometry,
    clip_cells: bool,
    mut visit: impl FnMut(PlacedGlyph) -> Result<(), String>,
) -> Result<(), String> {
    let baseline = bounds.top() + geometry.baseline;
    let text = line.text.as_bytes();
    let mut spans = colors.iter();
    let mut span = spans.next();
    for run in &line.runs {
        for glyph in &run.glyphs {
            if text.get(glyph.index) == Some(&b' ') {
                continue;
            }
            while let Some(&(end, _)) = span
                && glyph.index >= end
            {
                span = spans.next();
            }
            // A run is one glyph per cell. GPUI's forced width only snaps a
            // glyph that drifted over a pixel, so a face whose advance is not
            // the cell width would sawtooth; every glyph starts on its column.
            let column = (f32::from(glyph.position.x) / f32::from(geometry.cell_width)).round();
            let x = if clip_cells {
                geometry.cell_width * column
            } else {
                glyph.position.x
            };
            let clip = (clip_cells && text.get(glyph.index).is_some_and(|byte| *byte >= 0x80))
                .then(|| {
                    Bounds::new(
                        point(bounds.left() + x, bounds.top()),
                        size(geometry.cell_width, bounds.size.height),
                    )
                });
            visit(PlacedGlyph {
                origin: point(bounds.left() + x, baseline + glyph.position.y),
                font_id: run.font_id,
                id: glyph.id,
                color: span.map_or_else(gpui::black, |&(_, color)| color),
                is_emoji: glyph.is_emoji,
                clip,
            })?;
        }
    }
    Ok(())
}

/// Paint `line` glyph by glyph, as [`each_glyph`] places it, inside the
/// current content mask.
fn paint_glyphs(
    line: &ShapedLine,
    bounds: Bounds<Pixels>,
    colors: &[(usize, Hsla)],
    geometry: Geometry,
    clip_cells: bool,
    window: &mut gpui::Window,
) -> Result<(), String> {
    each_glyph(line, bounds, colors, geometry, clip_cells, |glyph| {
        let paint = |window: &mut gpui::Window| {
            if glyph.is_emoji {
                window.paint_emoji(glyph.origin, glyph.font_id, glyph.id, line.font_size)
            } else {
                window.paint_glyph(
                    glyph.origin,
                    glyph.font_id,
                    glyph.id,
                    line.font_size,
                    glyph.color,
                )
            }
        };
        match glyph.clip {
            Some(cell) => window.with_content_mask(Some(ContentMask { bounds: cell }), paint),
            None => paint(window),
        }
        .map_err(|error| error.to_string())
    })
}

/// A rounded box-drawing corner: the two borders meeting at `bend.corner`.
fn bent(bounds: Bounds<Pixels>, bend: super::boxes::Bend, color: Hsla) -> gpui::PaintQuad {
    let (zero, line, radius) = (px(0.), bend.thickness, bend.radius);
    let (top, bottom) = if bend.corner < 2 {
        (line, zero)
    } else {
        (zero, line)
    };
    let (left, right) = if matches!(bend.corner, 0 | 3) {
        (line, zero)
    } else {
        (zero, line)
    };
    let mut corners = gpui::Corners::all(zero);
    match bend.corner {
        0 => corners.top_left = radius,
        1 => corners.top_right = radius,
        2 => corners.bottom_right = radius,
        _ => corners.bottom_left = radius,
    }
    gpui::quad(
        bounds,
        corners,
        gpui::transparent_black(),
        gpui::Edges {
            top,
            right,
            bottom,
            left,
        },
        color,
        gpui::BorderStyle::Solid,
    )
}
