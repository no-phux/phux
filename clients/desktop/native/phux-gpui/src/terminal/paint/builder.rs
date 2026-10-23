//! Walk viewport and fringe cells into retained scene primitives.

use super::*;

impl Builder {
    pub(super) fn new(
        frame: Arc<GridFrame>,
        settings: Settings,
        bounds: Bounds<Pixels>,
        window: &gpui::Window,
    ) -> Self {
        let geometry = Geometry::measure(&settings, bounds, frame.cols, frame.rows, window);
        Self::from_kept(
            frame,
            settings,
            geometry,
            (
                bounds,
                window.scale_factor(),
                crate::platform::glyph_raster(window),
            ),
            None,
        )
    }

    /// `fonts`, when given, were built from a font that paints like `settings.font`.
    pub(super) fn from_kept(
        frame: Arc<GridFrame>,
        settings: Settings,
        geometry: Geometry,
        (bounds, scale, raster): (Bounds<Pixels>, f32, Option<crate::platform::Raster>),
        fonts: Option<Rc<Fonts>>,
    ) -> Self {
        let theme = settings.theme();
        let defaults = draw::default_colors(&frame, &theme);
        let solid_cursor = solid_cursor(&frame, &settings).then(|| cursor_head(&frame));
        let fonts = fonts.unwrap_or_else(|| Rc::new(Fonts::new(&settings.font)));
        Self {
            fonts: Rc::clone(&fonts),
            theme,
            defaults,
            solid_cursor,
            run: None,
            pending: None,
            row_sprites: Vec::new(),
            scene: Scene {
                frame,
                bounds,
                scale,
                settings,
                geometry,
                fonts,
                raster,
                background: defaults.1,
                backgrounds: Vec::new(),
                inks: Vec::new(),
                sprites: Vec::new(),
                decorations: Vec::new(),
                cursor_blocks: Vec::new(),
                cursor_overlays: Vec::new(),
                glyphs: Vec::new(),
            },
        }
    }

    pub(super) fn build(mut self, window: &gpui::Window) -> Scene {
        let frame = Arc::clone(&self.scene.frame);
        for row in 0..frame.rows {
            self.paint_row(&frame, row, window);
        }
        if has_fringe(&frame) {
            self.paint_row(&frame, frame.rows, window);
        }
        self.finish()
    }

    pub(super) fn paint_rows(&mut self, rows: &[u16], window: &gpui::Window) {
        let frame = Arc::clone(&self.scene.frame);
        for row in rows {
            self.paint_row(&frame, *row, window);
        }
    }

    fn paint_row(&mut self, frame: &GridFrame, row: u16, window: &gpui::Window) {
        for col in 0..frame.cols {
            self.prepare_cell(frame, row, col, window);
        }
        self.flush_background();
        self.flush_glyph_run(window);
        if !self.row_sprites.is_empty() {
            self.scene.sprites.push(RowSprites {
                row,
                dy: px(0.),
                sprites: self.row_sprites.drain(..).collect(),
            });
        }
    }

    /// Resolve an ink's glyphs into the row's sprites, painted as `paint_glyphs`
    /// would paint them. False leaves the row's sprites as they were, and the
    /// ink then paints from its shaped line.
    pub(super) fn resolve(
        &mut self,
        line: &ShapedLine,
        bounds: Bounds<Pixels>,
        colors: &[(usize, Hsla)],
        clip_cells: bool,
        window: &gpui::Window,
    ) -> bool {
        if self.scene.raster.is_none() {
            return false;
        }
        let kept = self.row_sprites.len();
        let geometry = self.scene.geometry;
        let resolved = each_glyph(line, bounds, colors, geometry, clip_cells, |glyph| {
            let clip = glyph.clip.map_or(bounds, |cell| cell.intersect(&bounds));
            if let Some(sprite) = crate::platform::resolve_glyph(
                window,
                glyph.origin,
                glyph.font_id,
                glyph.id,
                line.font_size,
                glyph.color,
                glyph.is_emoji,
                clip,
            )
            .map_err(|error| error.to_string())?
            {
                self.row_sprites.push(sprite);
            }
            Ok(())
        });
        if resolved.is_err() {
            self.row_sprites.truncate(kept);
        }
        resolved.is_ok()
    }

    pub(super) fn finish(mut self) -> Scene {
        prepare_cursor(&mut self.scene);
        self.scene
    }

    fn prepare_cell(&mut self, frame: &GridFrame, row: u16, col: u16, window: &gpui::Window) {
        let Some((cell, metadata)) = painted_cell(frame, row, col) else {
            return;
        };
        let geometry = self.scene.geometry;
        let bounds = geometry.cell_bounds(row, col, 1);
        // The fringe row starts at the grid's bottom edge; a sub-row scroll
        // brings it into view.
        if row < frame.rows && !bounds.intersects(&geometry.bounds) {
            return;
        }
        let colors = cell_colors(cell, metadata, frame, &self.theme, self.defaults);
        self.background_cell(row, col, colors.background);
        if hidden(cell, &self.scene.settings) {
            return;
        }
        self.prepare_ink(frame, (row, col), cell, bounds, colors, window);
        // Decorations stay above procedural blocks as well as shaped text.
        self.prepare_decorations(row, cell, bounds, colors);
    }
}
