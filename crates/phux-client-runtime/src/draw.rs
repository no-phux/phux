//! How a [`GridFrame`] paints, for any UI toolkit (feature `engine`).
//!
//! The rules every renderer needs and none of them should rediscover: which
//! colour a cell takes after defaults, theme, inverse video and selection;
//! which face; whether it shows at all; where the cursor is and how it looks;
//! which block elements are cell coverage rather than font ink. They are
//! generic over the renderer's colour type `C`, so a renderer gets exactly
//! the colours it would have computed itself.
//!
//! [`DrawList::build`] applies them to a whole frame and returns plain
//! rectangles-and-text in cell coordinates: background runs, text runs,
//! block cells, decorations and the cursor. A toolkit that can fill a
//! rectangle and draw a string at a point can paint a terminal from it.
//! Paint in this order: the default background, `backgrounds`, a
//! [`CursorPaint::solid`] cursor, `texts`, `blocks`, `decorations`, then any
//! other cursor. A faint item paints at half alpha.

use phux_client_core::grid::{
    CELL_BLINK, CELL_BOLD, CELL_FAINT, CELL_INVERSE, CELL_INVISIBLE, CELL_ITALIC, CELL_SELECTED,
    COLOR_KIND_DEFAULT, COLOR_KIND_PALETTE,
};

use crate::publication::{Cell, CellMetadata, CursorStyle, CursorWidth, GridFrame, Rgb};

pub mod boxes;
mod list;

pub use list::{BackgroundRun, BlockCell, Decoration, DrawList, TextRun};

/// A renderer's colours. `None` defaults take the frame's own.
#[derive(Clone, Copy, Debug)]
pub struct Theme<C> {
    /// Default foreground; the frame's when `None`.
    pub foreground: Option<C>,
    /// Default background; the frame's when `None`.
    pub background: Option<C>,
    /// Cursor colour; the frame's cursor colour, else the foreground.
    pub cursor: Option<C>,
    /// The 16 ANSI slots, when the theme names them.
    pub ansi: Option<[C; 16]>,
    /// Selected text.
    pub selection_foreground: C,
    /// Selected cells.
    pub selection_background: C,
    /// The renderer's colour for an RGB the frame carries.
    pub rgb: fn(Rgb) -> C,
}

impl Theme<Rgb> {
    /// The frame's own colours, with `selection` as `(foreground, background)`.
    #[must_use]
    pub fn frame_colors(selection: (Rgb, Rgb)) -> Self {
        Self {
            foreground: None,
            background: None,
            cursor: None,
            ansi: None,
            selection_foreground: selection.0,
            selection_background: selection.1,
            rgb: |color| color,
        }
    }
}

/// What the renderer currently shows: focus and the blink phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Visibility {
    /// The terminal has keyboard focus: a block cursor is solid.
    pub focused: bool,
    /// The renderer shows a cursor at all.
    pub cursor_visible: bool,
    /// The blink phase is on: blinking cells and cursors show.
    pub blink_visible: bool,
}

impl Default for Visibility {
    fn default() -> Self {
        Self {
            focused: true,
            cursor_visible: true,
            blink_visible: true,
        }
    }
}

/// One cell's colours.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellColors<C> {
    /// Glyph colour.
    pub foreground: C,
    /// Cell background.
    pub background: C,
    /// Underline colour.
    pub underline: C,
    /// SGR 2: paint glyphs and decorations at half alpha.
    pub faint: bool,
}

/// The default `(foreground, background)` for `frame`. An application's own
/// defaults (OSC 10/11) win over the theme, and DECSCNM swaps the theme's.
#[must_use]
pub fn default_colors<C: Copy>(frame: &GridFrame, theme: &Theme<C>) -> (C, C) {
    let (foreground, background) = if frame.colors.reversed {
        (theme.background, theme.foreground)
    } else {
        (theme.foreground, theme.background)
    };
    (
        if frame.colors.has_foreground {
            (theme.rgb)(frame.colors.foreground)
        } else {
            foreground.unwrap_or_else(|| (theme.rgb)(frame.colors.foreground))
        },
        if frame.colors.has_background {
            (theme.rgb)(frame.colors.background)
        } else {
            background.unwrap_or_else(|| (theme.rgb)(frame.colors.background))
        },
    )
}

/// The theme's colour for one of the 16 ANSI slots, when a theme names them.
fn ansi<C: Copy>(theme: &Theme<C>, index: usize) -> Option<C> {
    theme.ansi.as_ref()?.get(index).copied()
}

/// Background and underline colours arrive palette-resolved without their
/// index. A colour equal to one of the frame's 16 ANSI entries was almost
/// certainly that slot, so it takes the theme's colour; anything else (true
/// colour, the 240 extended slots) paints as the application sent it.
fn themed<C: Copy>(frame: &GridFrame, theme: &Theme<C>, color: Rgb) -> C {
    if theme.ansi.is_some()
        && let Some(index) = frame.colors.palette[..16]
            .iter()
            .position(|entry| *entry == color)
        && let Some(themed) = ansi(theme, index)
    {
        return themed;
    }
    (theme.rgb)(color)
}

/// One cell's colours; `defaults` is [`default_colors`] for this frame and
/// theme. Inverse swaps first, then selection replaces both.
#[must_use]
pub fn cell_colors<C: Copy>(
    cell: &Cell,
    metadata: CellMetadata,
    frame: &GridFrame,
    theme: &Theme<C>,
    defaults: (C, C),
) -> CellColors<C> {
    let mut foreground = if metadata.foreground_kind == COLOR_KIND_DEFAULT {
        defaults.0
    } else if metadata.foreground_kind == COLOR_KIND_PALETTE
        && let Some(themed) = ansi(theme, usize::from(metadata.foreground_palette_index))
    {
        themed
    } else {
        themed(
            frame,
            theme,
            rgb(cell.foreground_r, cell.foreground_g, cell.foreground_b),
        )
    };
    let mut background = if metadata.background_color_is_default {
        defaults.1
    } else {
        themed(
            frame,
            theme,
            rgb(cell.background_r, cell.background_g, cell.background_b),
        )
    };
    if cell.flags & CELL_INVERSE != 0 {
        std::mem::swap(&mut foreground, &mut background);
    }
    if cell.flags & CELL_SELECTED != 0 {
        foreground = theme.selection_foreground;
        background = theme.selection_background;
    }
    let underline = if metadata.underline_color_is_default {
        foreground
    } else {
        themed(
            frame,
            theme,
            rgb(cell.underline_r, cell.underline_g, cell.underline_b),
        )
    };
    CellColors {
        foreground,
        background,
        underline,
        faint: cell.flags & CELL_FAINT != 0,
    }
}

const fn rgb(r: u8, g: u8, b: u8) -> Rgb {
    Rgb { r, g, b }
}

/// A cell's font face.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Face {
    /// SGR 1.
    pub bold: bool,
    /// SGR 3.
    pub italic: bool,
}

impl Face {
    /// The face `cell` asks for.
    #[must_use]
    pub const fn of(cell: &Cell) -> Self {
        Self {
            bold: cell.flags & CELL_BOLD != 0,
            italic: cell.flags & CELL_ITALIC != 0,
        }
    }

    /// Bit 0 bold, bit 1 italic: an index into four faces.
    #[must_use]
    pub const fn index(self) -> usize {
        self.bold as usize | (self.italic as usize) << 1
    }
}

/// Whether `cell` paints no glyph: invisible, or blinking in its off phase.
#[must_use]
pub const fn hidden(cell: &Cell, visibility: Visibility) -> bool {
    cell.flags & CELL_INVISIBLE != 0 || (cell.flags & CELL_BLINK != 0 && !visibility.blink_visible)
}

/// Whether the cursor shows this frame.
#[must_use]
pub const fn cursor_visible(frame: &GridFrame, visibility: Visibility) -> bool {
    frame.cursor.visible
        && visibility.cursor_visible
        && (!frame.cursor.blinking || visibility.blink_visible)
}

/// The cursor's colour: the theme's, else the application's, else the
/// default foreground.
#[must_use]
pub fn cursor_color<C: Copy>(frame: &GridFrame, theme: &Theme<C>) -> C {
    theme
        .cursor
        .or_else(|| frame.colors.cursor.map(theme.rgb))
        .unwrap_or_else(|| default_colors(frame, theme).0)
}

/// The cursor's `(row, col)`. A cursor on a wide character's tail reports
/// the head.
#[must_use]
pub const fn cursor_head(frame: &GridFrame) -> (u16, u16) {
    let tail = matches!(frame.cursor.width, CursorWidth::WideTail) as u16;
    (frame.cursor.row, frame.cursor.col.saturating_sub(tail))
}

/// The painted cursor as `(row, col, width)`.
#[must_use]
pub const fn cursor_span(frame: &GridFrame) -> (u16, u16, u16) {
    let (row, col) = cursor_head(frame);
    let width = if frame.cursor.width.is_wide() { 2 } else { 1 };
    (row, col, width)
}

/// A focused, visible block cursor fills its cell, and the glyph under it
/// takes the cell's background.
#[must_use]
pub fn solid_cursor(frame: &GridFrame, visibility: Visibility) -> bool {
    cursor_visible(frame, visibility)
        && visibility.focused
        && frame.cursor.style == CursorStyle::Block
}

/// The cursor to paint this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorPaint<C> {
    /// Row.
    pub row: u16,
    /// Column of its first cell.
    pub col: u16,
    /// Cells it covers: 2 on a wide character.
    pub width: u16,
    /// Shape. An unfocused terminal's cursor is [`CursorStyle::BlockHollow`].
    pub style: CursorStyle,
    /// Colour.
    pub color: C,
    /// Painted under the glyphs, which take the cell's background.
    pub solid: bool,
}

/// The cursor this frame paints, if any.
#[must_use]
pub fn cursor<C: Copy>(
    frame: &GridFrame,
    theme: &Theme<C>,
    visibility: Visibility,
) -> Option<CursorPaint<C>> {
    if !cursor_visible(frame, visibility) {
        return None;
    }
    let (row, col, width) = cursor_span(frame);
    let style = if visibility.focused {
        frame.cursor.style
    } else {
        CursorStyle::BlockHollow
    };
    Some(CursorPaint {
        row,
        col,
        width,
        style,
        color: cursor_color(frame, theme),
        solid: style == CursorStyle::Block,
    })
}

/// One filled part of a cell, in eighths of its width and height from its
/// top-left corner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockRegion {
    /// Left edge.
    pub x: u8,
    /// Top edge.
    pub y: u8,
    /// Width.
    pub width: u8,
    /// Height.
    pub height: u8,
}

/// A region literal, which a `&'static` table can borrow.
macro_rules! region {
    ($x:expr, $y:expr, $width:expr, $height:expr) => {
        BlockRegion {
            x: $x,
            y: $y,
            width: $width,
            height: $height,
        }
    };
}

/// Solid Unicode blocks describe cell coverage, not font ink. Drawing them
/// from the cell edges avoids font side bearings and line-height gaps in
/// terminal UIs that use half blocks for smooth panels.
#[must_use]
pub fn block_regions(text: &[u8]) -> Option<&'static [BlockRegion]> {
    Some(match std::str::from_utf8(text).ok()? {
        "▀" => &[region!(0, 0, 8, 4)],
        "▁" => &[region!(0, 7, 8, 1)],
        "▂" => &[region!(0, 6, 8, 2)],
        "▃" => &[region!(0, 5, 8, 3)],
        "▄" => &[region!(0, 4, 8, 4)],
        "▅" => &[region!(0, 3, 8, 5)],
        "▆" => &[region!(0, 2, 8, 6)],
        "▇" => &[region!(0, 1, 8, 7)],
        "█" => &[region!(0, 0, 8, 8)],
        "▉" => &[region!(0, 0, 7, 8)],
        "▊" => &[region!(0, 0, 6, 8)],
        "▋" => &[region!(0, 0, 5, 8)],
        "▌" => &[region!(0, 0, 4, 8)],
        "▍" => &[region!(0, 0, 3, 8)],
        "▎" => &[region!(0, 0, 2, 8)],
        "▏" => &[region!(0, 0, 1, 8)],
        "▐" => &[region!(4, 0, 4, 8)],
        "▔" => &[region!(0, 0, 8, 1)],
        "▕" => &[region!(7, 0, 1, 8)],
        "▖" => &[region!(0, 4, 4, 4)],
        "▗" => &[region!(4, 4, 4, 4)],
        "▘" => &[region!(0, 0, 4, 4)],
        "▙" => &[region!(0, 0, 4, 8), region!(4, 4, 4, 4)],
        "▚" => &[region!(0, 0, 4, 4), region!(4, 4, 4, 4)],
        "▛" => &[region!(0, 0, 8, 4), region!(0, 4, 4, 4)],
        "▜" => &[region!(0, 0, 8, 4), region!(4, 4, 4, 4)],
        "▝" => &[region!(4, 0, 4, 4)],
        "▞" => &[region!(4, 0, 4, 4), region!(0, 4, 4, 4)],
        "▟" => &[region!(4, 0, 4, 8), region!(0, 4, 4, 4)],
        _ => return None,
    })
}

/// libghostty's underline styles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Underline {
    /// SGR 4.
    Single,
    /// SGR 21 / 4:2.
    Double,
    /// 4:3.
    Curly,
    /// 4:4.
    Dotted,
    /// 4:5.
    Dashed,
}

impl Underline {
    /// The style `cell` carries, if any.
    #[must_use]
    pub const fn of(cell: &Cell) -> Option<Self> {
        Some(match cell.underline {
            1 => Self::Single,
            2 => Self::Double,
            3 => Self::Curly,
            4 => Self::Dotted,
            5 => Self::Dashed,
            _ => return None,
        })
    }
}

/// A line drawn across cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecorationKind {
    /// Below the text.
    Underline(Underline),
    /// Through the text, a little below the cell's middle.
    Strikethrough,
    /// Along the cell's top edge.
    Overline,
}

#[cfg(test)]
mod tests;
