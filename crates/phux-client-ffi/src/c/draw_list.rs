//! The runtime's draw list (`phux_client_runtime::draw`) for C renderers.
//!
//! One call applies phux's paint rules to the terminal's current grid and
//! lends rectangles and text in cell coordinates. A renderer that can fill a
//! rectangle and draw a string paints a terminal from it without walking
//! cells, resolving colours or placing the cursor itself.

use std::{mem::size_of, ptr};

use phux_client_runtime::draw::{
    BackgroundRun, BlockCell, CursorPaint, Decoration, DecorationKind, DrawList, TextRun, Theme,
    Underline, Visibility,
};

use crate::c::error::{BridgeError, check_struct, terminal_id_in};
use crate::c::grid_metadata::PhuxGridRgb;
use crate::c::types::{ABI_VERSION, PhuxBytes, PhuxDocumentAnchor, bytes_out};
use crate::c::{PhuxClient, PhuxClientResult, PhuxResourceId, with_client_mut};

/// What the renderer shows now. Initialize `size` and `version`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhuxDrawOptions {
    pub size: usize,
    pub version: u32,
    pub selection_foreground: PhuxGridRgb,
    pub selection_background: PhuxGridRgb,
    /// Keyboard focus: a block cursor is solid, else hollow.
    pub focused: bool,
    pub cursor_visible: bool,
    /// The blink phase is on.
    pub blink_visible: bool,
}

impl Default for PhuxDrawOptions {
    fn default() -> Self {
        Self {
            size: size_of::<Self>(),
            version: ABI_VERSION,
            selection_foreground: PhuxGridRgb {
                r: 255,
                g: 255,
                b: 255,
            },
            selection_background: PhuxGridRgb {
                r: 0x38,
                g: 0x57,
                b: 0x79,
            },
            focused: true,
            cursor_visible: true,
            blink_visible: true,
        }
    }
}

/// Cells `start..end` of `row` filled with `color`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PhuxDrawBackground {
    pub row: u16,
    pub start: u16,
    pub end: u16,
    pub color: PhuxGridRgb,
}

/// Bit 0 of `PhuxDrawText::face`.
pub const PHUX_DRAW_FACE_BOLD: u8 = 1;
/// Bit 1 of `PhuxDrawText::face`.
pub const PHUX_DRAW_FACE_ITALIC: u8 = 2;

/// UTF-8 at `utf8_offset..+utf8_len` of the list's arena, drawn from `col`
/// across `cells` columns, one character per column.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PhuxDrawText {
    pub row: u16,
    pub col: u16,
    pub cells: u16,
    pub face: u8,
    pub faint: bool,
    pub color: PhuxGridRgb,
    pub utf8_offset: u32,
    pub utf8_len: u32,
}

/// One filled part of a cell, in eighths from its top-left corner.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PhuxDrawRegion {
    pub x: u8,
    pub y: u8,
    pub width: u8,
    pub height: u8,
}

/// A block element: fill `regions[..region_count]` of the cell.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PhuxDrawBlock {
    pub row: u16,
    pub col: u16,
    pub color: PhuxGridRgb,
    pub faint: bool,
    pub region_count: u8,
    pub regions: [PhuxDrawRegion; 2],
}

/// `PhuxDrawDecoration::kind`: 1..=5 are libghostty's underline styles.
pub const PHUX_DRAW_UNDERLINE_SINGLE: u8 = 1;
pub const PHUX_DRAW_UNDERLINE_DOUBLE: u8 = 2;
pub const PHUX_DRAW_UNDERLINE_CURLY: u8 = 3;
pub const PHUX_DRAW_UNDERLINE_DOTTED: u8 = 4;
pub const PHUX_DRAW_UNDERLINE_DASHED: u8 = 5;
pub const PHUX_DRAW_STRIKETHROUGH: u8 = 6;
pub const PHUX_DRAW_OVERLINE: u8 = 7;

/// A line across cells `start..end` of `row`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PhuxDrawDecoration {
    pub row: u16,
    pub start: u16,
    pub end: u16,
    pub kind: u8,
    pub faint: bool,
    pub color: PhuxGridRgb,
}

/// The cursor, when `visible`. `style` is `PhuxCursorStyle`; a `solid`
/// cursor paints under the glyphs.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PhuxDrawCursor {
    pub visible: bool,
    pub solid: bool,
    pub row: u16,
    pub col: u16,
    pub width: u16,
    pub style: u32,
    pub color: PhuxGridRgb,
}

/// Borrowed draw list. Paint `background`, `backgrounds`, a solid cursor,
/// `texts`, `blocks`, `decorations`, then any other cursor; a faint item
/// paints at half alpha. Initialize `size` and `version`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxDrawList {
    pub size: usize,
    pub version: u32,
    pub cols: u16,
    pub rows: u16,
    pub background: PhuxGridRgb,
    pub backgrounds: *const PhuxDrawBackground,
    pub background_count: usize,
    pub texts: *const PhuxDrawText,
    pub text_count: usize,
    pub blocks: *const PhuxDrawBlock,
    pub block_count: usize,
    pub decorations: *const PhuxDrawDecoration,
    pub decoration_count: usize,
    pub cursor: PhuxDrawCursor,
    pub utf8: PhuxBytes,
}

impl Default for PhuxDrawList {
    fn default() -> Self {
        Self {
            size: size_of::<Self>(),
            version: ABI_VERSION,
            cols: 0,
            rows: 0,
            background: PhuxGridRgb::default(),
            backgrounds: ptr::null(),
            background_count: 0,
            texts: ptr::null(),
            text_count: 0,
            blocks: ptr::null(),
            block_count: 0,
            decorations: ptr::null(),
            decoration_count: 0,
            cursor: PhuxDrawCursor::default(),
            utf8: PhuxBytes::default(),
        }
    }
}

/// The C records of one draw list, owned by the terminal's render cache.
#[derive(Default)]
pub(crate) struct DrawCache {
    background: PhuxGridRgb,
    cols: u16,
    rows: u16,
    backgrounds: Vec<PhuxDrawBackground>,
    texts: Vec<PhuxDrawText>,
    blocks: Vec<PhuxDrawBlock>,
    decorations: Vec<PhuxDrawDecoration>,
    cursor: PhuxDrawCursor,
    utf8: String,
}

impl DrawCache {
    fn from_list(list: DrawList<PhuxGridRgb>) -> Self {
        Self {
            background: list.background,
            cols: list.cols,
            rows: list.rows,
            backgrounds: list.backgrounds.iter().map(background).collect(),
            texts: list.texts.iter().map(text).collect(),
            blocks: list.blocks.iter().map(block).collect(),
            decorations: list.decorations.iter().map(decoration).collect(),
            cursor: list.cursor.map_or_else(PhuxDrawCursor::default, cursor),
            utf8: list.text,
        }
    }

    fn view(&self) -> PhuxDrawList {
        PhuxDrawList {
            cols: self.cols,
            rows: self.rows,
            background: self.background,
            backgrounds: slice_ptr(&self.backgrounds),
            background_count: self.backgrounds.len(),
            texts: slice_ptr(&self.texts),
            text_count: self.texts.len(),
            blocks: slice_ptr(&self.blocks),
            block_count: self.blocks.len(),
            decorations: slice_ptr(&self.decorations),
            decoration_count: self.decorations.len(),
            cursor: self.cursor,
            utf8: bytes_out(self.utf8.as_bytes()),
            ..PhuxDrawList::default()
        }
    }
}

const fn slice_ptr<T>(items: &[T]) -> *const T {
    if items.is_empty() {
        ptr::null()
    } else {
        items.as_ptr()
    }
}

const fn background(run: &BackgroundRun<PhuxGridRgb>) -> PhuxDrawBackground {
    PhuxDrawBackground {
        row: run.row,
        start: run.start,
        end: run.end,
        color: run.color,
    }
}

fn text(run: &TextRun<PhuxGridRgb>) -> PhuxDrawText {
    let bold = if run.face.bold {
        PHUX_DRAW_FACE_BOLD
    } else {
        0
    };
    let italic = if run.face.italic {
        PHUX_DRAW_FACE_ITALIC
    } else {
        0
    };
    PhuxDrawText {
        row: run.row,
        col: run.col,
        cells: run.cells,
        face: bold | italic,
        faint: run.faint,
        color: run.color,
        // The arena is one frame's text, far below 4 GiB.
        utf8_offset: u32::try_from(run.text.start).unwrap_or(u32::MAX),
        utf8_len: u32::try_from(run.text.len()).unwrap_or(0),
    }
}

fn block(cell: &BlockCell<PhuxGridRgb>) -> PhuxDrawBlock {
    let mut regions = [PhuxDrawRegion::default(); 2];
    for (slot, region) in regions.iter_mut().zip(cell.regions) {
        *slot = PhuxDrawRegion {
            x: region.x,
            y: region.y,
            width: region.width,
            height: region.height,
        };
    }
    PhuxDrawBlock {
        row: cell.row,
        col: cell.col,
        color: cell.color,
        faint: cell.faint,
        region_count: u8::try_from(cell.regions.len().min(2)).unwrap_or(2),
        regions,
    }
}

const fn decoration(line: &Decoration<PhuxGridRgb>) -> PhuxDrawDecoration {
    let kind = match line.kind {
        DecorationKind::Underline(Underline::Single) => PHUX_DRAW_UNDERLINE_SINGLE,
        DecorationKind::Underline(Underline::Double) => PHUX_DRAW_UNDERLINE_DOUBLE,
        DecorationKind::Underline(Underline::Curly) => PHUX_DRAW_UNDERLINE_CURLY,
        DecorationKind::Underline(Underline::Dotted) => PHUX_DRAW_UNDERLINE_DOTTED,
        DecorationKind::Underline(Underline::Dashed) => PHUX_DRAW_UNDERLINE_DASHED,
        DecorationKind::Strikethrough => PHUX_DRAW_STRIKETHROUGH,
        DecorationKind::Overline => PHUX_DRAW_OVERLINE,
    };
    PhuxDrawDecoration {
        row: line.row,
        start: line.start,
        end: line.end,
        kind,
        faint: line.faint,
        color: line.color,
    }
}

const fn cursor(cursor: CursorPaint<PhuxGridRgb>) -> PhuxDrawCursor {
    PhuxDrawCursor {
        visible: true,
        solid: cursor.solid,
        row: cursor.row,
        col: cursor.col,
        width: cursor.width,
        style: cursor.style as u32,
        color: cursor.color,
    }
}

/// The frame's own colours: the replica already carries the client theme
/// (`phux_client_set_terminal_theme`), so only selection comes from here.
fn theme(options: &PhuxDrawOptions) -> Theme<PhuxGridRgb> {
    Theme {
        foreground: None,
        background: None,
        cursor: None,
        ansi: None,
        selection_foreground: options.selection_foreground,
        selection_background: options.selection_background,
        rgb: PhuxGridRgb::from,
    }
}

/// Builds the terminal's current grid and lends its draw list.
///
/// `options` may be null for the defaults: focused, cursor and blink phase
/// visible, the desktop's selection colours. Pointers in `out_list` remain
/// valid until the next mutable call using `client`, like
/// `phux_client_terminal_grid`'s, which this call also refreshes.
///
/// # Safety
///
/// `client` must be a live client on its owning thread with exclusive access
/// for the call. Non-null `terminal_id` and its host span must be readable.
/// Non-null `options` must be readable for its size and version, and for the
/// whole struct when those pass validation. Non-null `out_list` must be
/// readable/writable for its size and version, and for the whole struct when
/// those pass validation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_terminal_draw_list(
    client: *mut PhuxClient,
    terminal_id: *const PhuxResourceId,
    options: *const PhuxDrawOptions,
    out_list: *mut PhuxDrawList,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        if out_list.is_null() {
            return Err(BridgeError::invalid("out_list is null"));
        }
        // SAFETY: the caller promises readable size/version fields, even for
        // undersized output storage. No reference to the full struct is made.
        let (size, version) = unsafe {
            (
                ptr::addr_of!((*out_list).size).read(),
                ptr::addr_of!((*out_list).version).read(),
            )
        };
        check_struct(size, size_of::<PhuxDrawList>(), version)?;
        // SAFETY: validated full-sized output storage.
        unsafe { out_list.write(PhuxDrawList::default()) };
        let options = if options.is_null() {
            PhuxDrawOptions::default()
        } else {
            // SAFETY: the caller promises readable size/version fields.
            let (size, version) = unsafe { ((*options).size, (*options).version) };
            check_struct(size, size_of::<PhuxDrawOptions>(), version)?;
            // SAFETY: validated as a whole struct above.
            unsafe { *options }
        };
        // SAFETY: forwards the caller's terminal-id span contract.
        let terminal_id = unsafe { terminal_id_in(terminal_id) }?;
        client.build_grid(&terminal_id)?;
        // build_grid tracks a top anchor for the grid view's caller. This
        // caller never sees it, so release it now instead of leaking one per
        // call; the cached view then carries no anchor.
        let anchor = client
            .render
            .get(&terminal_id)
            .map(|cache| cache.view.top_anchor)
            .filter(|anchor| anchor.opaque_id != 0);
        if let Some(anchor) = anchor {
            client.release_anchor(&terminal_id, anchor)?;
        }
        let cache = client
            .render
            .get_mut(&terminal_id)
            .ok_or_else(|| BridgeError::state("terminal has no published grid"))?;
        cache.view.top_anchor = PhuxDocumentAnchor::default();
        let visibility = Visibility {
            focused: options.focused,
            cursor_visible: options.cursor_visible,
            blink_visible: options.blink_visible,
        };
        let list = DrawList::build(&cache.frame, &theme(&options), visibility);
        let draw = cache.draw.insert(DrawCache::from_list(list));
        // SAFETY: output is validated above; the view borrows cache-owned storage.
        unsafe { out_list.write(draw.view()) };
        Ok(())
    })
}

#[cfg(test)]
mod tests;
