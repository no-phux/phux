//! Shared fixtures and responsibility-grouped paint regression tests.

use super::*;
use phux_client_core::grid::{
    CELL_BLINK, CELL_BOLD, CELL_FAINT, CELL_INVERSE, CELL_INVISIBLE, CELL_ITALIC, CELL_SELECTED,
};
use phux_client_runtime::publication::{
    Cursor, CursorWidth, FrameColors, GridBuffer, GridDamage, Rgb, Scrollbar,
};

mod appearance;
mod damage;
mod scrolling;

use super::damage::{
    can_patch, cursor_key, merge_rows, row_paints_alike, rows_to_rebuild, shifted_height,
    worth_scroll_search,
};
use super::scrolling::Scroll;
use fixtures::*;

mod fixtures {
    use super::*;

    pub(super) fn defaults(frame: &GridFrame, settings: &Settings) -> (Hsla, Hsla) {
        draw::default_colors(frame, &settings.theme())
    }

    pub(super) fn frame() -> GridFrame {
        GridFrame {
            terminal_id: phux_protocol::ResourceId::local(1),
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

    /// A builder over `frame` without a window: the geometry is given.
    pub(super) fn builder(frame: GridFrame, settings: Settings, geometry: Geometry) -> Builder {
        let frame = Arc::new(frame);
        let defaults = defaults(&frame, &settings);
        let fonts = Rc::new(Fonts::new(&settings.font));
        Builder {
            fonts: Rc::clone(&fonts),
            theme: settings.theme(),
            defaults,
            solid_cursor: None,
            run: None,
            pending: None,
            row_sprites: Vec::new(),
            scene: Scene {
                frame,
                bounds: geometry.bounds,
                scale: 1.,
                settings,
                geometry,
                fonts,
                raster: None,
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

    pub(super) fn row_grid(lines: &[&str]) -> GridFrame {
        let mut utf8 = Vec::new();
        let mut cells = Vec::new();
        for line in lines {
            for byte in line.as_bytes() {
                cells.push(text_cell(&mut utf8, &[*byte]));
            }
        }
        let mut frame = frame();
        frame.cols = u16::try_from(lines.first().map_or(0, |line| line.len())).unwrap_or(0);
        frame.rows = u16::try_from(lines.len()).unwrap_or(0);
        frame.buffer.metadata = vec![CellMetadata::default(); cells.len()];
        frame.buffer.cells = cells;
        frame.buffer.utf8 = utf8;
        frame
    }

    pub(super) fn cursor_at(
        mut frame: GridFrame,
        style: CursorStyle,
        row: u16,
        col: u16,
    ) -> GridFrame {
        frame.cursor.visible = true;
        frame.cursor.blinking = false;
        frame.cursor.style = style;
        frame.cursor.row = row;
        frame.cursor.col = col;
        frame
    }

    pub(super) fn painted_frame(cells: Vec<Cell>, utf8: Vec<u8>) -> GridFrame {
        let mut frame = frame();
        frame.cols = 2;
        frame.rows = 2;
        frame.buffer.metadata = vec![CellMetadata::default(); cells.len()];
        frame.buffer.cells = cells;
        frame.buffer.utf8 = utf8;
        frame
    }

    pub(super) fn text_cell(utf8: &mut Vec<u8>, text: &[u8]) -> Cell {
        let offset = u32::try_from(utf8.len()).unwrap_or(0);
        utf8.extend_from_slice(text);
        Cell {
            utf8_offset: offset,
            utf8_len: u16::try_from(text.len()).unwrap_or(0),
            ..Cell::default()
        }
    }

    pub(super) fn scrolling_geometry() -> Geometry {
        Geometry {
            bounds: Bounds::new(point(px(0.), px(0.)), size(px(100.), px(100.))),
            cell_width: px(10.),
            cell_height: px(20.),
            scale: 1.,
            ..Default::default()
        }
    }

    pub(super) fn scrolling_frame(rows: &[&str], offset: u64) -> GridFrame {
        let mut frame = frame();
        frame.cols = 2;
        frame.rows = rows.len() as u16;
        frame.cursor.visible = false;
        frame.scrollbar.offset = offset;
        for row in rows {
            let cell = text_cell(&mut frame.buffer.utf8, row.as_bytes());
            frame.buffer.cells.extend([cell, Cell::default()]);
            frame.buffer.metadata.extend([CellMetadata::default(); 2]);
        }
        frame
    }

    pub(super) fn with_fringe(mut frame: GridFrame, text: &str) -> GridFrame {
        let cell = text_cell(&mut frame.buffer.utf8, text.as_bytes());
        frame.buffer.fringe = vec![cell, Cell::default()];
        frame.buffer.fringe_metadata = vec![CellMetadata::default(); 2];
        frame
    }
}
