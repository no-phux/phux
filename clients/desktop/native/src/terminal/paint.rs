use std::{cell::RefCell, rc::Rc, sync::Arc, time::Instant};

use super::{GridFrame, Settings, geometry::Geometry, gpui};
use gpui::prelude::*;
use gpui::{Bounds, ContentMask, Hsla, Pixels, ShapedLine, TextAlign, fill, point, px, size};
use phux_client_core::grid::{
    CELL_BLINK, CELL_BOLD, CELL_FAINT, CELL_INVERSE, CELL_INVISIBLE, CELL_ITALIC, CELL_OVERLINE,
    CELL_SELECTED, CELL_STRIKETHROUGH, COLOR_KIND_DEFAULT,
};
use phux_client_runtime::publication::{Cell, CellMetadata, CursorStyle, CursorWidth, Rgb};

#[derive(Default)]
pub struct Observation {
    pub view_id: Option<phux_client_runtime::ViewId>,
    pub frame: Option<Arc<GridFrame>>,
    pub geometry: Geometry,
    pub glyphs: Vec<GlyphObservation>,
    pub error: Option<String>,
    pub prepare_micros: u128,
    pub paint_micros: u128,
}

pub struct GlyphObservation {
    pub row: u16,
    pub col: u16,
    pub bounds: Bounds<Pixels>,
    pub origin: gpui::Point<Pixels>,
    pub baseline: Pixels,
    pub foreground: Hsla,
}

struct Glyph {
    line: ShapedLine,
    observation: GlyphObservation,
    opacity: f32,
    layer: Option<OpacityLayer>,
}

struct OpacityLayer {
    element: gpui::AnyElement,
    result: Rc<RefCell<Option<Result<(), String>>>>,
}

#[derive(Clone, Copy)]
struct CellColors {
    foreground: Hsla,
    background: Hsla,
    underline: Hsla,
    opacity: f32,
}

pub(super) struct Prepared {
    report: Observation,
    settings: Settings,
    background: Hsla,
    backgrounds: Vec<(Bounds<Pixels>, Hsla)>,
    glyphs: Vec<Glyph>,
    decorations: Vec<(Bounds<Pixels>, Hsla)>,
    cursor: Vec<(Bounds<Pixels>, Hsla)>,
}

pub(super) fn prepare(
    frame: Result<Arc<GridFrame>, String>,
    settings: Settings,
    bounds: Bounds<Pixels>,
    window: &mut gpui::Window,
    cx: &mut gpui::App,
) -> Prepared {
    let started = Instant::now();
    let mut prepared = Prepared {
        background: settings.background.unwrap_or_else(gpui::black),
        report: Observation {
            view_id: settings.view_id,
            ..Default::default()
        },
        settings,
        backgrounds: Vec::new(),
        glyphs: Vec::new(),
        decorations: Vec::new(),
        cursor: Vec::new(),
    };
    match frame {
        Ok(frame) => prepared.prepare_frame(frame, bounds, window),
        Err(error) => prepared.report.error = Some(error),
    }
    prepared.prepare_opacity_layers(window, cx);
    prepared.report.prepare_micros = started.elapsed().as_micros();
    prepared
}

impl Prepared {
    fn prepare_frame(
        &mut self,
        frame: Arc<GridFrame>,
        bounds: Bounds<Pixels>,
        window: &gpui::Window,
    ) {
        self.report.geometry =
            Geometry::measure(&self.settings, bounds, frame.cols, frame.rows, window);
        self.background = defaults(&frame, &self.settings).1;
        for row in 0..frame.rows {
            for col in 0..frame.cols {
                self.prepare_cell(&frame, row, col, window);
            }
        }
        self.prepare_cursor(&frame);
        self.report.frame = Some(frame);
    }

    fn prepare_cell(&mut self, frame: &GridFrame, row: u16, col: u16, window: &gpui::Window) {
        let Some(cell) = frame.cell(row, col) else {
            return;
        };
        let geometry = self.report.geometry;
        let bounds = geometry.cell_bounds(row, col, 1);
        if !bounds.intersects(&geometry.bounds) {
            return;
        }
        let index = usize::from(row) * usize::from(frame.cols) + usize::from(col);
        let metadata = frame
            .buffer
            .metadata
            .get(index)
            .copied()
            .unwrap_or_default();
        let colors = cell_colors(cell, metadata, frame, &self.settings);
        self.backgrounds.push((bounds, colors.background));
        if hidden(cell, &self.settings) {
            return;
        }
        self.prepare_decorations(cell, bounds, colors);
        // Both spacer variants contain no glyph. The head's whole grapheme is
        // shaped in one call; force_width would incorrectly advance combining marks.
        if matches!(cell.wide, 2 | 3) {
            return;
        }
        let text = String::from_utf8_lossy(frame.cell_text(row, col));
        if text.is_empty() {
            return;
        }
        let bounds = geometry.cell_bounds(row, col, if cell.wide == 1 { 2 } else { 1 });
        let foreground = self.glyph_foreground(frame, row, col, colors);
        let line = window.text_system().shape_line(
            text.into_owned().into(),
            px(self.settings.font_size),
            &[gpui::TextRun {
                len: usize::from(cell.utf8_len),
                font: cell_font(cell, &self.settings),
                color: foreground,
                ..Default::default()
            }],
            None,
        );
        self.glyphs.push(Glyph {
            line,
            opacity: colors.opacity,
            layer: None,
            observation: GlyphObservation {
                row,
                col,
                bounds,
                origin: bounds.origin,
                baseline: geometry.baseline + bounds.origin.y,
                foreground: foreground.opacity(colors.opacity),
            },
        });
    }

    fn glyph_foreground(&self, frame: &GridFrame, row: u16, col: u16, colors: CellColors) -> Hsla {
        if solid_cursor(frame, &self.settings) && cursor_head(frame) == (row, col) {
            return colors.background;
        }
        colors.foreground
    }

    fn prepare_decorations(&mut self, cell: &Cell, bounds: Bounds<Pixels>, mut colors: CellColors) {
        colors.foreground = colors.foreground.opacity(colors.opacity);
        colors.underline = colors.underline.opacity(colors.opacity);
        let thickness = px(1. / self.report.geometry.scale);
        let bottom = bounds.bottom() - thickness * 2.;
        match cell.underline {
            1 => self.rule(bounds, bottom, thickness, colors.underline),
            2 => {
                self.rule(bounds, bottom, thickness, colors.underline);
                self.rule(bounds, bottom - thickness * 2., thickness, colors.underline);
            }
            3..=5 => self.patterned_underline(
                cell.underline,
                bounds,
                bottom,
                thickness,
                colors.underline,
            ),
            _ => (),
        }
        if cell.flags & CELL_STRIKETHROUGH != 0 {
            self.rule(
                bounds,
                bounds.top() + bounds.size.height * 0.55,
                thickness,
                colors.foreground,
            );
        }
        if cell.flags & CELL_OVERLINE != 0 {
            self.rule(bounds, bounds.top(), thickness, colors.foreground);
        }
    }

    fn rule(&mut self, bounds: Bounds<Pixels>, y: Pixels, thickness: Pixels, color: Hsla) {
        self.decorations.push((
            Bounds::new(point(bounds.left(), y), size(bounds.size.width, thickness)),
            color,
        ));
    }

    fn patterned_underline(
        &mut self,
        kind: u8,
        bounds: Bounds<Pixels>,
        y: Pixels,
        thickness: Pixels,
        color: Hsla,
    ) {
        let step = match kind {
            3 => 1.,
            4 => 2.,
            _ => 4.,
        };
        let mut x = px(0.);
        let mut index = 0;
        while x < bounds.size.width {
            let offset = if kind == 3 {
                [0., 1., 2., 1.][index % 4]
            } else {
                0.
            };
            let width = if kind == 5 { thickness * 2. } else { thickness };
            self.decorations.push((
                Bounds::new(
                    point(bounds.left() + x, y - thickness * offset),
                    size(width.min(bounds.size.width - x), thickness),
                ),
                color,
            ));
            x += thickness * step;
            index += 1;
        }
    }

    fn prepare_cursor(&mut self, frame: &GridFrame) {
        if !cursor_visible(frame, &self.settings) {
            return;
        }
        let geometry = self.report.geometry;
        let (row, col) = cursor_head(frame);
        let bounds =
            geometry.cell_bounds(row, col, if frame.cursor.width.is_wide() { 2 } else { 1 });
        let color = self
            .settings
            .cursor
            .or_else(|| frame.colors.cursor.map(rgb))
            .unwrap_or_else(|| defaults(frame, &self.settings).0);
        let thickness = px(1. / geometry.scale);
        let style = if self.settings.focused {
            frame.cursor.style
        } else {
            CursorStyle::BlockHollow
        };
        match style {
            CursorStyle::Block => self.backgrounds.push((bounds, color)),
            CursorStyle::Bar => self.cursor.push((
                Bounds::new(bounds.origin, size(thickness * 2., bounds.size.height)),
                color,
            )),
            CursorStyle::Underline => self.cursor.push((
                Bounds::new(
                    point(bounds.left(), bounds.bottom() - thickness * 2.),
                    size(bounds.size.width, thickness * 2.),
                ),
                color,
            )),
            CursorStyle::BlockHollow => self
                .cursor
                .extend(outline(bounds, thickness).map(|bounds| (bounds, color))),
        }
    }

    pub fn paint(
        mut self,
        bounds: Bounds<Pixels>,
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) -> Observation {
        let started = Instant::now();
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            window.paint_quad(fill(bounds, self.background));
            for (bounds, color) in self.backgrounds {
                window.paint_quad(fill(bounds, color));
            }
            for mut glyph in self.glyphs {
                let cell_bounds = glyph.observation.bounds;
                let baseline =
                    (self.report.geometry.cell_height - glyph.line.ascent - glyph.line.descent)
                        / 2.
                        + glyph.line.ascent;
                let origin =
                    cell_bounds.origin + point(px(0.), self.report.geometry.baseline - baseline);
                glyph.observation.origin = origin;
                glyph.observation.baseline = origin.y + baseline;
                let result = glyph.paint(self.report.geometry, window, cx);
                match result {
                    Ok(()) => self.report.glyphs.push(glyph.observation),
                    Err(error) => self.report.error = Some(error.to_string()),
                }
            }
            for (bounds, color) in self.decorations.into_iter().chain(self.cursor) {
                window.paint_quad(fill(bounds, color));
            }
        });
        self.report.paint_micros = started.elapsed().as_micros();
        self.report
    }

    fn prepare_opacity_layers(&mut self, window: &mut gpui::Window, cx: &mut gpui::App) {
        for glyph in &mut self.glyphs {
            if glyph.opacity < 1. {
                glyph.prepare_opacity_layer(self.report.geometry, window, cx);
            }
        }
    }
}

impl Glyph {
    fn prepare_opacity_layer(
        &mut self,
        geometry: Geometry,
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) {
        let line = self.line.clone();
        let bounds = self.observation.bounds;
        let result = Rc::new(RefCell::new(None));
        let output = result.clone();
        // ShapedLine's color emoji path ignores TextRun.color. Div::opacity is
        // GPUI's public entry to the window opacity stack, used by both glyph
        // paths. TextRun stays opaque so monochrome glyphs are not dimmed twice.
        let mut element = gpui::div()
            .opacity(self.opacity)
            .w(bounds.size.width)
            .h(bounds.size.height)
            .child(
                gpui::canvas(
                    |_, _, _| (),
                    move |_, (), window, cx| {
                        *output.borrow_mut() =
                            Some(paint_line(&line, bounds, geometry, window, cx));
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
        self.layer = Some(OpacityLayer { element, result });
    }

    fn paint(
        &mut self,
        geometry: Geometry,
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) -> Result<(), String> {
        if let Some(layer) = &mut self.layer {
            layer.element.paint(window, cx);
            return layer
                .result
                .borrow_mut()
                .take()
                .ok_or("opacity layer did not paint")?;
        }
        paint_line(&self.line, self.observation.bounds, geometry, window, cx)
    }
}

fn paint_line(
    line: &ShapedLine,
    bounds: Bounds<Pixels>,
    geometry: Geometry,
    window: &mut gpui::Window,
    cx: &mut gpui::App,
) -> Result<(), String> {
    let baseline = (geometry.cell_height - line.ascent - line.descent) / 2. + line.ascent;
    let origin = bounds.origin + point(px(0.), geometry.baseline - baseline);
    window
        .with_content_mask(Some(ContentMask { bounds }), |window| {
            line.paint(
                origin,
                geometry.cell_height,
                TextAlign::Left,
                None,
                window,
                cx,
            )
        })
        .map_err(|error| error.to_string())
}

fn rgb(color: Rgb) -> Hsla {
    gpui::rgb(u32::from(color.r) << 16 | u32::from(color.g) << 8 | u32::from(color.b)).into()
}

fn defaults(frame: &GridFrame, settings: &Settings) -> (Hsla, Hsla) {
    let (foreground, background) = if frame.colors.reversed {
        (settings.background, settings.foreground)
    } else {
        (settings.foreground, settings.background)
    };
    (
        if frame.colors.has_foreground {
            rgb(frame.colors.foreground)
        } else {
            foreground.unwrap_or_else(|| rgb(frame.colors.foreground))
        },
        if frame.colors.has_background {
            rgb(frame.colors.background)
        } else {
            background.unwrap_or_else(|| rgb(frame.colors.background))
        },
    )
}

fn cell_colors(
    cell: &Cell,
    metadata: CellMetadata,
    frame: &GridFrame,
    settings: &Settings,
) -> CellColors {
    let defaults = defaults(frame, settings);
    let mut foreground = if metadata.foreground_kind == COLOR_KIND_DEFAULT {
        defaults.0
    } else {
        rgb(Rgb {
            r: cell.foreground_r,
            g: cell.foreground_g,
            b: cell.foreground_b,
        })
    };
    let mut background = if metadata.background_color_is_default {
        defaults.1
    } else {
        rgb(Rgb {
            r: cell.background_r,
            g: cell.background_g,
            b: cell.background_b,
        })
    };
    if cell.flags & CELL_INVERSE != 0 {
        std::mem::swap(&mut foreground, &mut background);
    }
    if cell.flags & CELL_SELECTED != 0 {
        foreground = settings.selection_foreground;
        background = settings.selection_background;
    }
    let opacity = if cell.flags & CELL_FAINT != 0 {
        0.5
    } else {
        1.
    };
    let underline = if metadata.underline_color_is_default {
        foreground
    } else {
        rgb(Rgb {
            r: cell.underline_r,
            g: cell.underline_g,
            b: cell.underline_b,
        })
    };
    CellColors {
        foreground,
        background,
        underline,
        opacity,
    }
}

fn cell_font(cell: &Cell, settings: &Settings) -> gpui::Font {
    let mut font = settings.font.clone();
    if cell.flags & CELL_BOLD != 0 {
        font = font.bold();
    }
    if cell.flags & CELL_ITALIC != 0 {
        font = font.italic();
    }
    font
}

fn hidden(cell: &Cell, settings: &Settings) -> bool {
    cell.flags & CELL_INVISIBLE != 0 || (cell.flags & CELL_BLINK != 0 && !settings.blink_visible)
}

fn cursor_visible(frame: &GridFrame, settings: &Settings) -> bool {
    frame.cursor.visible
        && settings.cursor_visible
        && (!frame.cursor.blinking || settings.blink_visible)
}

fn solid_cursor(frame: &GridFrame, settings: &Settings) -> bool {
    cursor_visible(frame, settings) && settings.focused && frame.cursor.style == CursorStyle::Block
}

fn cursor_head(frame: &GridFrame) -> (u16, u16) {
    (
        frame.cursor.row,
        frame
            .cursor
            .col
            .saturating_sub(u16::from(frame.cursor.width == CursorWidth::WideTail)),
    )
}

fn outline(bounds: Bounds<Pixels>, thickness: Pixels) -> [Bounds<Pixels>; 4] {
    [
        Bounds::new(bounds.origin, size(bounds.size.width, thickness)),
        Bounds::new(
            point(bounds.left(), bounds.bottom() - thickness),
            size(bounds.size.width, thickness),
        ),
        Bounds::new(bounds.origin, size(thickness, bounds.size.height)),
        Bounds::new(
            point(bounds.right() - thickness, bounds.top()),
            size(thickness, bounds.size.height),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_client_runtime::publication::{
        Cursor, FrameColors, GridBuffer, GridDamage, Scrollbar,
    };

    fn frame() -> GridFrame {
        GridFrame {
            terminal_id: phux_client_ffi::projection::id::parse("local:1").expect("fixture id"),
            generation: 1,
            stream_id: 1,
            bootstrap_id: 1,
            last_seq: 0,
            cols: 10,
            rows: 2,
            cursor: Cursor::default(),
            scrollbar: Scrollbar::default(),
            colors: FrameColors::default(),
            damage: GridDamage::Full,
            buffer: GridBuffer::default(),
        }
    }

    #[test]
    fn inverse_swaps_true_color_before_selection_and_faint() {
        let frame = frame();
        let settings = Settings::default();
        let mut cell = Cell {
            foreground_r: 255,
            background_b: 255,
            flags: CELL_INVERSE,
            ..Default::default()
        };
        let metadata = CellMetadata {
            foreground_kind: 2,
            ..Default::default()
        };
        let colors = cell_colors(&cell, metadata, &frame, &settings);
        assert_eq!(colors.foreground, gpui::rgb(0x0000ff).into());
        assert_eq!(colors.background, gpui::rgb(0xff0000).into());
        cell.flags |= CELL_SELECTED | CELL_FAINT;
        let colors = cell_colors(&cell, metadata, &frame, &settings);
        assert_eq!(colors.background, settings.selection_background);
        assert_eq!(
            colors.foreground.a, 1.,
            "glyph color must not double-dim window opacity"
        );
        assert_eq!(colors.opacity, 0.5);
    }

    #[test]
    fn theme_uses_provenance_and_respects_terminal_default_override() {
        let mut frame = frame();
        let settings = Settings {
            foreground: Some(gpui::rgb(0xabcdef).into()),
            background: Some(gpui::rgb(0x123456).into()),
            ..Default::default()
        };
        let cell = Cell::default();
        let metadata = CellMetadata {
            background_color_is_default: true,
            underline_color_is_default: true,
            ..Default::default()
        };
        let colors = cell_colors(&cell, metadata, &frame, &settings);
        assert_eq!(colors.foreground, settings.foreground.expect("foreground"));
        assert_eq!(colors.underline, colors.foreground);
        frame.colors.reversed = true;
        let colors = cell_colors(&cell, metadata, &frame, &settings);
        assert_eq!(colors.foreground, settings.background.expect("background"));
        frame.colors.has_foreground = true;
        frame.colors.foreground = Rgb { r: 255, g: 0, b: 0 };
        assert_eq!(
            cell_colors(&cell, metadata, &frame, &settings).foreground,
            gpui::rgb(0xff0000).into()
        );
    }

    #[test]
    fn hidden_and_blinking_cells_do_not_produce_glyphs() {
        let mut settings = Settings::default();
        assert!(hidden(
            &Cell {
                flags: CELL_INVISIBLE,
                ..Default::default()
            },
            &settings
        ));
        let blinking = Cell {
            flags: CELL_BLINK,
            ..Default::default()
        };
        assert!(!hidden(&blinking, &settings));
        settings.blink_visible = false;
        assert!(hidden(&blinking, &settings));
    }

    #[test]
    fn wide_tail_cursor_uses_head_and_unfocused_cursor_is_not_solid() {
        let mut frame = frame();
        frame.cursor = Cursor {
            visible: true,
            col: 4,
            row: 1,
            width: CursorWidth::WideTail,
            style: CursorStyle::Block,
            blinking: false,
        };
        assert_eq!(cursor_head(&frame), (1, 3));
        assert!(solid_cursor(&frame, &Settings::default()));
        assert!(!solid_cursor(
            &frame,
            &Settings {
                focused: false,
                ..Default::default()
            }
        ));
    }
}
