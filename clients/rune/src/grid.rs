//! A terminal frame as Rune elements.
//!
//! The runtime's `DrawList` already resolved the theme, palette, inverse
//! video, selection, and blink, so this module only places cells. A pane
//! whose frame didn't move reuses its whole subtree without walking a cell.
//! Otherwise each row is one memoized subtree named by its content, so a line
//! of output rebuilds one row wherever the others scrolled to. Text runs are
//! set in the system monospace face with tracking that lands every glyph on a
//! device-pixel cell, which keeps long runs aligned with the backgrounds and
//! the cursor beneath them; `glyphs` keeps the rest on the grid.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::time::Instant;

use phux_client_runtime::draw::{
    BackgroundRun, BlockCell, CursorPaint, Decoration, DecorationKind, DrawList, TextRun, Theme,
    Underline,
};
use phux_client_runtime::publication::{CursorStyle, GridFrame, Rgb};
use rune::text::advance;
use rune::{Color, El, Family, TextStyle, el, memo, text};

use crate::perf;
use crate::theme::Selection;

mod glyphs;

/// Inter, which replacing Rune's built-in fonts has to name again, and the
/// Nerd Font GPUI bundles, for the private-use icons no system face has.
pub const FONTS: &[&[u8]] = &[
    include_bytes!("../../desktop/native/assets/fonts/Inter.ttf"),
    include_bytes!(
        "../../desktop/native/phux-gpui/assets/fonts/JetBrainsMonoNLNerdFontMono-Regular.ttf"
    ),
];
/// The sans and mono slots. `Family::SystemMono` stacks the system's faces
/// over the mono slot, so the Nerd Font only ever fills in missing glyphs.
pub const FAMILIES: (&str, &str) = ("Inter", "ui-monospace, JetBrainsMonoNL Nerd Font Mono");

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

pub fn theme(selection: Selection) -> Theme<Color> {
    Theme {
        foreground: None,
        background: None,
        cursor: None,
        ansi: None,
        selection_foreground: selection.foreground,
        selection_background: selection.background,
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
    perf::ROWS_BUILT.incr();
    let mut out = el().w_full().h(m.cell_h);
    for run in &row.backgrounds {
        out = out.child(rect(m, run.start, run.end.saturating_sub(run.start)).fill(run.color));
    }
    // Paint order from the draw-list contract: a solid cursor under text.
    if let Some(cursor) = row.cursor.filter(|c| c.solid) {
        out = out.child(cursor_shape(m, cursor));
    }
    let mut marks = glyphs::Marks::new(m);
    for run in &row.texts {
        marks.run(&list.text[run.text.clone()], run);
    }
    out = out.children(marks.into_els());
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
pub fn paint(
    frame: &GridFrame,
    focused: bool,
    blink_on: bool,
    metrics: &Metrics,
    selection: Selection,
) -> El {
    let preparing = Instant::now();
    // A generation restarts with its presentation, and an allocation can
    // come back: together with the replica's sequence they name one frame.
    let version = (
        std::ptr::from_ref(frame) as usize,
        (frame.generation, frame.stream_id, frame.bootstrap_id),
        frame.last_seq,
        (focused, blink_on),
        (metrics.font_size.to_bits(), metrics.cell_w.to_bits()),
        selection.key(),
    );
    let mut built = false;
    let grid = memo(format!("grid-{}", frame.terminal_id), version, || {
        built = true;
        build(frame, focused, blink_on, metrics, selection)
    });
    if built {
        perf::PREPARE_PATCHED.incr();
    } else {
        perf::PREPARE_REUSED.incr();
    }
    perf::PREPARE.record_elapsed(preparing);
    grid
}

fn build(
    frame: &GridFrame,
    focused: bool,
    blink_on: bool,
    metrics: &Metrics,
    selection: Selection,
) -> El {
    let visibility = phux_client_runtime::draw::Visibility {
        focused,
        cursor_visible: true,
        blink_visible: blink_on,
    };
    let list = DrawList::build(frame, &theme(selection), visibility);
    let mut grid = rune::col()
        .size(
            f32::from(list.cols) * metrics.cell_w,
            f32::from(list.rows) * metrics.cell_h,
        )
        .fill(list.background)
        // A row that moves lands at once: output never slides or fades.
        .instant_changes();
    // Rows are named by what they show, so a scrolled row keeps its subtree;
    // equal rows are told apart by their order.
    let mut seen: HashMap<u64, u16> = HashMap::new();
    for row in &split(&list) {
        let version = row.version(&list, metrics);
        let twin = seen.entry(version).or_default();
        let id = format!("r{version:x}-{twin}");
        *twin += 1;
        grid = grid.child(memo(id, version, || row_el(&list, row, metrics)));
    }
    grid
}

/// The input method's anchor at the terminal cursor, with any composition
/// drawn over the cells it will land in. Only while the terminal has the
/// keyboard.
pub fn ime_layer(
    frame: &GridFrame,
    preedit: Option<&str>,
    metrics: &Metrics,
    ink: (Color, Color),
) -> El {
    let (fg, bg) = ink;
    let x = f32::from(frame.cursor.col) * metrics.cell_w;
    let y = f32::from(frame.cursor.row) * metrics.cell_h;
    let anchor = el()
        .key("ime-caret")
        .absolute(x, y)
        .size(metrics.cell_w, metrics.cell_h)
        .ime_cursor();
    let Some(composed) = preedit.filter(|t| !t.is_empty()) else {
        return anchor;
    };
    let cells = f32::from(text_cells(composed)) * metrics.cell_w;
    let composing = text(composed)
        .absolute(x, y)
        .font(metrics.style(false, false))
        .color(fg)
        .single_line();
    el().absolute(0.0, 0.0)
        .size(
            f32::from(frame.cols) * metrics.cell_w,
            f32::from(frame.rows) * metrics.cell_h,
        )
        .child(el().absolute(x, y).h(metrics.cell_h).w(cells).fill(bg))
        .child(composing)
        .child(
            el().absolute(x, y + metrics.cell_h - 1.5)
                .h(1.5)
                .w(cells)
                .fill(fg),
        )
        .child(anchor)
}

/// Terminal cells a composition covers: two for wide (CJK) characters.
fn text_cells(text: &str) -> u16 {
    let wide = |c: char| matches!(u32::from(c), 0x1100..=0x115F | 0x2E80..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6 | 0x1F300..=0x1FAFF | 0x20000..=0x3FFFD);
    text.chars()
        .map(|c| if wide(c) { 2u16 } else { 1 })
        .fold(0u16, u16::saturating_add)
}

/// The frame's own default background, for the area around the grid.
pub fn background(frame: &GridFrame) -> Color {
    let (_, background) =
        phux_client_runtime::draw::default_colors(frame, &theme(Selection::system()));
    background
}

#[cfg(test)]
mod budget;

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
