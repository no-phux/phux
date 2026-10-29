//! A terminal frame as Rune elements.
//!
//! The runtime's `DrawList` already resolved the theme, palette, inverse
//! video, selection, and blink, so this module only places cells. Each row is
//! one memoized subtree keyed by its content, so a line of output rebuilds
//! one row, not the screen. Text runs are set in the system monospace face
//! with tracking that lands every glyph on a device-pixel cell, which keeps
//! long runs aligned with the backgrounds and the cursor beneath them.

use std::hash::{Hash, Hasher};

use phux_client_runtime::draw::{
    BackgroundRun, BlockCell, CursorPaint, Decoration, DecorationKind, DrawList, TextRun, Theme,
    Underline,
};
use phux_client_runtime::publication::{CursorStyle, GridFrame, Rgb};
use rune::text::advance;
use rune::{Color, El, Family, TextStyle, el, memo, text};

pub const DEFAULT_FONT_SIZE: f32 = 13.0;
pub const MIN_FONT_SIZE: f32 = 8.0;
pub const MAX_FONT_SIZE: f32 = 32.0;

/// One cell's size in points, snapped to whole device pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    pub font_size: f32,
    pub scale: f32,
    pub cell_w: f32,
    pub cell_h: f32,
    /// Added to the face's advance so it equals `cell_w`.
    tracking: f32,
}

impl Metrics {
    pub fn new(font_size: f32, scale: f32) -> Self {
        let scale = if scale > 0.0 { scale } else { 1.0 };
        let face = TextStyle::new(font_size, 400.0).family(Family::SystemMono);
        // Ten cells, so a fractional advance survives the measurement.
        let natural = advance("MMMMMMMMMM", face) / 10.0;
        let natural = if natural > 0.0 {
            natural
        } else {
            font_size * 0.6
        };
        let snap = |points: f32| ((points * scale).round().max(1.0)) / scale;
        let cell_w = snap(natural);
        Self {
            font_size,
            scale,
            cell_w,
            cell_h: snap(font_size * 1.3),
            tracking: cell_w - natural,
        }
    }

    /// The integer cell `RESIZE_TERMINAL` reports, in device pixels.
    pub fn cell_px(&self) -> (u16, u16) {
        let px = |points: f32| {
            (points * self.scale)
                .round()
                .clamp(1.0, f32::from(u16::MAX)) as u16
        };
        (px(self.cell_w), px(self.cell_h))
    }

    /// Whole cells that fit in `size` points; at least one each way.
    pub fn fit(&self, size: [f32; 2]) -> (u16, u16) {
        let cells = |points: f32, cell: f32| (points / cell).floor().clamp(1.0, 500.0) as u16;
        (cells(size[0], self.cell_w), cells(size[1], self.cell_h))
    }

    /// The cell under a point in the grid's own points, clamped to the grid.
    pub fn cell_at(&self, x: f32, y: f32, cols: u16, rows: u16) -> (u16, u16) {
        let clamp = |v: f32, cell: f32, n: u16| {
            (v / cell)
                .floor()
                .clamp(0.0, f32::from(n.saturating_sub(1))) as u16
        };
        (clamp(x, self.cell_w, cols), clamp(y, self.cell_h, rows))
    }

    fn style(&self, bold: bool, italic: bool) -> TextStyle {
        let style = TextStyle::new(self.font_size, if bold { 700.0 } else { 400.0 })
            .family(Family::SystemMono)
            .lh(self.cell_h)
            .tracking(self.tracking);
        if italic { style.italic() } else { style }
    }
}

pub fn color(rgb: Rgb) -> Color {
    let channel = |v: u8| {
        let c = f32::from(v) / 255.0;
        if c <= 0.040_45 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    Color {
        r: channel(rgb.r),
        g: channel(rgb.g),
        b: channel(rgb.b),
        a: 1.0,
    }
}

pub fn theme() -> Theme<Color> {
    Theme {
        foreground: None,
        background: None,
        cursor: None,
        ansi: None,
        selection_foreground: color(Rgb {
            r: 0xff,
            g: 0xff,
            b: 0xff,
        }),
        selection_background: color(Rgb {
            r: 0x3a,
            g: 0x5f,
            b: 0x9e,
        }),
        rgb: color,
    }
}

fn faint(color: Color, faint: bool) -> Color {
    if faint {
        color.alpha(color.a * 0.5)
    } else {
        color
    }
}

fn hash_color(color: Color, state: &mut impl Hasher) {
    for lane in [color.r, color.g, color.b, color.a] {
        lane.to_bits().hash(state);
    }
}

/// One row's share of the draw list, borrowed.
#[derive(Default)]
struct Row<'a> {
    backgrounds: Vec<&'a BackgroundRun<Color>>,
    texts: Vec<&'a TextRun<Color>>,
    blocks: Vec<&'a BlockCell<Color>>,
    decorations: Vec<&'a Decoration<Color>>,
    cursor: Option<&'a CursorPaint<Color>>,
}

impl Row<'_> {
    fn version(&self, list: &DrawList<Color>, metrics: &Metrics) -> u64 {
        let mut state = std::collections::hash_map::DefaultHasher::new();
        metrics.cell_w.to_bits().hash(&mut state);
        metrics.cell_h.to_bits().hash(&mut state);
        metrics.font_size.to_bits().hash(&mut state);
        for run in &self.backgrounds {
            (run.start, run.end).hash(&mut state);
            hash_color(run.color, &mut state);
        }
        for run in &self.texts {
            (
                run.col,
                run.cells,
                run.face.bold,
                run.face.italic,
                run.faint,
            )
                .hash(&mut state);
            list.text[run.text.clone()].hash(&mut state);
            hash_color(run.color, &mut state);
        }
        for block in &self.blocks {
            (block.col, block.regions.as_ptr() as usize, block.faint).hash(&mut state);
            hash_color(block.color, &mut state);
        }
        for line in &self.decorations {
            (line.start, line.end, format!("{:?}", line.kind), line.faint).hash(&mut state);
            hash_color(line.color, &mut state);
        }
        if let Some(cursor) = self.cursor {
            (cursor.col, cursor.width, cursor.style as u8, cursor.solid).hash(&mut state);
            hash_color(cursor.color, &mut state);
        }
        state.finish()
    }
}

fn split(list: &DrawList<Color>) -> Vec<Row<'_>> {
    let mut rows: Vec<Row<'_>> = (0..list.rows).map(|_| Row::default()).collect();
    for run in &list.backgrounds {
        if let Some(r) = rows.get_mut(usize::from(run.row)) {
            r.backgrounds.push(run);
        }
    }
    for run in &list.texts {
        if let Some(r) = rows.get_mut(usize::from(run.row)) {
            r.texts.push(run);
        }
    }
    for block in &list.blocks {
        if let Some(r) = rows.get_mut(usize::from(block.row)) {
            r.blocks.push(block);
        }
    }
    for line in &list.decorations {
        if let Some(r) = rows.get_mut(usize::from(line.row)) {
            r.decorations.push(line);
        }
    }
    if let Some(cursor) = &list.cursor
        && let Some(r) = rows.get_mut(usize::from(cursor.row))
    {
        r.cursor = Some(cursor);
    }
    rows
}

fn rect(m: &Metrics, col: u16, cells: u16) -> El {
    el().absolute(f32::from(col) * m.cell_w, 0.0)
        .size(f32::from(cells) * m.cell_w, m.cell_h)
}

fn cursor_shape(m: &Metrics, cursor: &CursorPaint<Color>) -> El {
    let width = f32::from(cursor.width.max(1)) * m.cell_w;
    let x = f32::from(cursor.col) * m.cell_w;
    let thin = (2.0 / m.scale).max(1.0 / m.scale);
    let shape = match (cursor.solid, cursor.style) {
        (true, _) | (false, CursorStyle::Block) => el()
            .absolute(x, 0.0)
            .size(width, m.cell_h)
            .fill(cursor.color),
        (false, CursorStyle::Bar) => el()
            .absolute(x, 0.0)
            .size(thin, m.cell_h)
            .fill(cursor.color),
        (false, CursorStyle::Underline) => el()
            .absolute(x, m.cell_h - thin)
            .size(width, thin)
            .fill(cursor.color),
        (false, CursorStyle::BlockHollow) => el()
            .absolute(x, 0.0)
            .size(width, m.cell_h)
            .border(1.0 / m.scale, cursor.color),
    };
    shape.key("cursor")
}

fn decoration(m: &Metrics, line: &Decoration<Color>) -> El {
    let px = 1.0 / m.scale;
    let thick = (m.font_size / 13.0).round().max(1.0) * px;
    let y = match line.kind {
        DecorationKind::Underline(_) => m.cell_h - thick * 2.0,
        DecorationKind::Strikethrough => m.cell_h * 0.55,
        DecorationKind::Overline => 0.0,
    };
    let height = match line.kind {
        DecorationKind::Underline(Underline::Double) => thick * 3.0,
        _ => thick,
    };
    let base = rect(m, line.start, line.end.saturating_sub(line.start))
        .absolute(f32::from(line.start) * m.cell_w, y);
    let base = base.h(height);
    match line.kind {
        DecorationKind::Underline(Underline::Double) => base
            .child(el().w_full().h(thick).fill(faint(line.color, line.faint)))
            .child(
                el().absolute(0.0, thick * 2.0)
                    .w_full()
                    .h(thick)
                    .fill(faint(line.color, line.faint)),
            ),
        DecorationKind::Underline(Underline::Dotted | Underline::Dashed) => {
            base.fill(faint(line.color, line.faint).alpha(0.7))
        }
        _ => base.fill(faint(line.color, line.faint)),
    }
}

fn block(m: &Metrics, cell: &BlockCell<Color>) -> El {
    let mut host = rect(m, cell.col, 1);
    for region in cell.regions {
        let eighth = |v: u8, cell: f32| f32::from(v) * cell / 8.0;
        host = host.child(
            el().absolute(eighth(region.x, m.cell_w), eighth(region.y, m.cell_h))
                .size(
                    eighth(region.width, m.cell_w),
                    eighth(region.height, m.cell_h),
                )
                .fill(faint(cell.color, cell.faint)),
        );
    }
    host
}

fn row_el(list: &DrawList<Color>, row: &Row<'_>, m: &Metrics) -> El {
    let mut out = el().w_full().h(m.cell_h);
    for run in &row.backgrounds {
        out = out.child(rect(m, run.start, run.end.saturating_sub(run.start)).fill(run.color));
    }
    // Paint order from the draw-list contract: a solid cursor under text.
    if let Some(cursor) = row.cursor.filter(|c| c.solid) {
        out = out.child(cursor_shape(m, cursor));
    }
    for run in &row.texts {
        let content = &list.text[run.text.clone()];
        out = out.child(
            text(content)
                .absolute(f32::from(run.col) * m.cell_w, 0.0)
                .font(m.style(run.face.bold, run.face.italic))
                .color(faint(run.color, run.faint))
                .single_line(),
        );
    }
    for cell in &row.blocks {
        out = out.child(block(m, cell));
    }
    for line in &row.decorations {
        out = out.child(decoration(m, line));
    }
    if let Some(cursor) = row.cursor.filter(|c| !c.solid) {
        out = out.child(cursor_shape(m, cursor));
    }
    out
}

/// The whole grid, sized to its cells, over the frame's default background.
pub fn paint(frame: &GridFrame, focused: bool, metrics: &Metrics) -> El {
    let visibility = phux_client_runtime::draw::Visibility {
        focused,
        ..Default::default()
    };
    let list = DrawList::build(frame, &theme(), visibility);
    let mut grid = rune::col()
        .size(
            f32::from(list.cols) * metrics.cell_w,
            f32::from(list.rows) * metrics.cell_h,
        )
        .fill(list.background);
    for (index, row) in split(&list).iter().enumerate() {
        let version = row.version(&list, metrics);
        let id = format!("row{index}");
        grid = grid.child(memo(id, version, || row_el(&list, row, metrics)));
    }
    grid
}

/// The frame's own default background, for the area around the grid.
pub fn background(frame: &GridFrame) -> Color {
    let (_, background) = phux_client_runtime::draw::default_colors(frame, &theme());
    background
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_land_on_device_pixels() {
        for scale in [1.0, 2.0, 3.0] {
            let m = Metrics::new(DEFAULT_FONT_SIZE, scale);
            let px = m.cell_w * scale;
            assert!(
                (px - px.round()).abs() < 1e-3,
                "{px} device px wide at {scale}x"
            );
            let px = m.cell_h * scale;
            assert!(
                (px - px.round()).abs() < 1e-3,
                "{px} device px tall at {scale}x"
            );
        }
    }

    #[test]
    fn fitting_floors_and_never_reaches_zero() {
        let m = Metrics::new(DEFAULT_FONT_SIZE, 2.0);
        let (cols, rows) = m.fit([m.cell_w * 80.5, m.cell_h * 24.9]);
        assert_eq!((cols, rows), (80, 24));
        assert_eq!(m.fit([0.0, 0.0]), (1, 1));
    }

    #[test]
    fn hit_testing_clamps_to_the_grid() {
        let m = Metrics::new(DEFAULT_FONT_SIZE, 2.0);
        assert_eq!(m.cell_at(-5.0, -5.0, 80, 24), (0, 0));
        assert_eq!(m.cell_at(m.cell_w * 3.5, m.cell_h * 2.5, 80, 24), (3, 2));
        assert_eq!(m.cell_at(1e6, 1e6, 80, 24), (79, 23));
    }

    #[test]
    fn colors_decode_srgb() {
        let white = color(Rgb {
            r: 255,
            g: 255,
            b: 255,
        });
        assert!((white.r - 1.0).abs() < 1e-6);
        let mid = color(Rgb {
            r: 128,
            g: 128,
            b: 128,
        });
        assert!(mid.r > 0.2 && mid.r < 0.23, "linear {}", mid.r);
    }
}
