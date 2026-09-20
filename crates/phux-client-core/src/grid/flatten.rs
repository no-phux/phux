//! The walk from libghostty's row and cell iterators to [`Cell`] records.

use libghostty_vt::Terminal;
use libghostty_vt::render::{CellIteration, CellIterator, Colors, RowIterator, Snapshot};
use libghostty_vt::screen::{Cell as RawCell, CellContentTag};
use libghostty_vt::style::{RgbColor, Style, StyleColor};
use libghostty_vt::terminal::{Point, PointCoordinate};

use super::{
    CELL_BLINK, CELL_BOLD, CELL_FAINT, CELL_HYPERLINK, CELL_INVERSE, CELL_INVISIBLE, CELL_ITALIC,
    CELL_OVERLINE, CELL_PROTECTED, CELL_SELECTED, CELL_STRIKETHROUGH, COLOR_KIND_DEFAULT,
    COLOR_KIND_PALETTE, COLOR_KIND_RGB, Cell, CellMetadata, GridBuffer, GridError,
};

const NO_HYPERLINK: (u32, u32) = (0, 0);

/// Inputs that stay fixed for every cell of one projection.
pub(super) struct CellContext<'a, 'cb> {
    pub terminal: &'a Terminal<'static, 'cb>,
    pub colors: &'a Colors,
}

/// Reusable per-cell scratch so flattening allocates at most once per grapheme
/// cluster and once per hyperlink URI longer than the current buffer.
pub(super) struct CellScratch {
    graphemes: String,
    hyperlink: Vec<u8>,
}

impl CellScratch {
    pub(super) fn new() -> Self {
        Self {
            graphemes: String::new(),
            hyperlink: vec![0_u8; 64],
        }
    }
}

/// Walk libghostty's row and cell iterators, appending one record per
/// viewport cell in row-major order.
pub(super) fn fill_grid_cells(
    rows: &mut RowIterator<'static>,
    cells: &mut CellIterator<'static>,
    snapshot: &Snapshot<'static, '_>,
    context: &CellContext<'_, '_>,
    buffer: &mut GridBuffer,
    scratch: &mut CellScratch,
) -> Result<(), GridError> {
    let mut row_index = 0_u32;
    let mut row_iter = rows.update(snapshot)?;
    while let Some(row) = row_iter.next() {
        let mut column_index = 0_u16;
        let mut cell_iter = cells.update(row)?;
        while let Some(cell) = cell_iter.next() {
            push_flattened_cell(
                cell,
                PointCoordinate {
                    x: column_index,
                    y: row_index,
                },
                context,
                scratch,
                buffer,
            )?;
            column_index = column_index
                .checked_add(1)
                .ok_or(GridError::Overflow("render column"))?;
        }
        row_index = row_index
            .checked_add(1)
            .ok_or(GridError::Overflow("render row"))?;
    }
    Ok(())
}

/// Flatten one libghostty cell into a record, appending its text and any
/// hyperlink URI to the shared UTF-8 arena first.
fn push_flattened_cell(
    cell: &CellIteration<'static, '_>,
    at: PointCoordinate,
    context: &CellContext<'_, '_>,
    scratch: &mut CellScratch,
    buffer: &mut GridBuffer,
) -> Result<(), GridError> {
    let raw = cell.raw_cell()?;
    let style = cell.style()?;
    let content_tag = raw.content_tag()?;
    let start = buffer.utf8.len();
    append_cell_text(
        cell,
        raw,
        content_tag,
        &mut buffer.utf8,
        &mut scratch.graphemes,
    )?;
    let cell_utf8_len = buffer.utf8.len() - start;
    let has_hyperlink = raw.has_hyperlink()?;
    let (hyperlink_offset, hyperlink_len) = if has_hyperlink {
        append_hyperlink_uri(
            context.terminal,
            at,
            &mut buffer.utf8,
            &mut scratch.hyperlink,
        )?
    } else {
        NO_HYPERLINK
    };
    let colors = context.colors;
    let fg = resolve_color(style.fg_color, colors.foreground, &colors.palette);
    let bg = cell_background(raw, content_tag, style, colors)?;
    let underline_color = resolve_color(style.underline_color, fg, &colors.palette);
    let flags = cell_flags(style, cell, raw, has_hyperlink)?;
    let record = Cell {
        utf8_offset: u32::try_from(start)
            .map_err(|_| GridError::Overflow("cell UTF-8 arena offset"))?,
        utf8_len: u16::try_from(cell_utf8_len)
            .map_err(|_| GridError::Overflow("cell grapheme length"))?,
        hyperlink_offset,
        hyperlink_len,
        content_tag: content_tag as u16,
        wide: raw.wide()? as u8,
        semantic_content: raw.semantic_content()? as u8,
        flags,
        foreground_r: fg.r,
        foreground_g: fg.g,
        foreground_b: fg.b,
        background_r: bg.r,
        background_g: bg.g,
        background_b: bg.b,
        underline: style.underline as u8,
        underline_r: underline_color.r,
        underline_g: underline_color.g,
        underline_b: underline_color.b,
        reserved: 0,
    };
    buffer.metadata.push(cell_metadata(style, content_tag));
    buffer.cells.push(record);
    Ok(())
}

/// Append a cell's text to the shared UTF-8 arena. Background-only cells carry
/// no codepoint and contribute nothing to it.
fn append_cell_text(
    cell: &CellIteration<'static, '_>,
    raw: RawCell,
    content_tag: CellContentTag,
    utf8: &mut Vec<u8>,
    graphemes: &mut String,
) -> Result<(), GridError> {
    match content_tag {
        CellContentTag::Codepoint => {
            let cp = raw.codepoint()?;
            if cp != 0 {
                let ch = char::from_u32(cp).ok_or(GridError::InvalidCodepoint(cp))?;
                let mut encoded = [0_u8; 4];
                utf8.extend_from_slice(ch.encode_utf8(&mut encoded).as_bytes());
            }
        }
        CellContentTag::CodepointGrapheme => {
            graphemes.clear();
            cell.graphemes_utf8(graphemes)?;
            utf8.extend_from_slice(graphemes.as_bytes());
        }
        CellContentTag::BgColorPalette | CellContentTag::BgColorRgb => {}
    }
    Ok(())
}

/// Copy a cell's hyperlink URI into the shared UTF-8 arena, growing the scratch
/// buffer until libghostty reports the whole URI fits, and return its
/// (offset, length) within that arena.
fn append_hyperlink_uri(
    terminal: &Terminal<'static, '_>,
    at: PointCoordinate,
    utf8: &mut Vec<u8>,
    scratch: &mut Vec<u8>,
) -> Result<(u32, u32), GridError> {
    let reference = terminal.grid_ref(Point::Viewport(at))?;
    let len = loop {
        match reference.hyperlink_uri(scratch) {
            Ok(len) => break len,
            Err(libghostty_vt::Error::OutOfSpace { required }) if required > scratch.len() => {
                scratch.resize(required, 0);
            }
            Err(error) => return Err(error.into()),
        }
    };
    let offset =
        u32::try_from(utf8.len()).map_err(|_| GridError::Overflow("cell UTF-8 arena offset"))?;
    utf8.extend_from_slice(&scratch[..len]);
    Ok((
        offset,
        u32::try_from(len).map_err(|_| GridError::Overflow("hyperlink URI length"))?,
    ))
}

/// Resolve a cell's background: an explicit palette or RGB background content
/// tag overrides whatever the cell's style asked for.
fn cell_background(
    raw: RawCell,
    content_tag: CellContentTag,
    style: Style,
    colors: &Colors,
) -> Result<RgbColor, GridError> {
    Ok(match content_tag {
        CellContentTag::BgColorPalette => colors.palette[usize::from(raw.bg_color_palette()?.0)],
        CellContentTag::BgColorRgb => raw.bg_color_rgb()?,
        CellContentTag::Codepoint | CellContentTag::CodepointGrapheme => {
            resolve_color(style.bg_color, colors.background, &colors.palette)
        }
    })
}

fn resolve_color(color: StyleColor, fallback: RgbColor, palette: &[RgbColor; 256]) -> RgbColor {
    match color {
        StyleColor::None => fallback,
        StyleColor::Palette(index) => palette[usize::from(index.0)],
        StyleColor::Rgb(rgb) => rgb,
    }
}

/// Fold a cell's SGR attributes together with its selection, protection and
/// hyperlink state into the flag word.
fn cell_flags(
    style: Style,
    cell: &CellIteration<'static, '_>,
    raw: RawCell,
    has_hyperlink: bool,
) -> Result<u32, GridError> {
    let mut flags = style_flags(style);
    if cell.is_selected()? {
        flags |= CELL_SELECTED;
    }
    if raw.is_protected()? {
        flags |= CELL_PROTECTED;
    }
    if has_hyperlink {
        flags |= CELL_HYPERLINK;
    }
    Ok(flags)
}

/// The SGR attribute bits of the flag word.
const fn style_flags(style: Style) -> u32 {
    let mut flags = 0;
    if style.bold {
        flags |= CELL_BOLD;
    }
    if style.italic {
        flags |= CELL_ITALIC;
    }
    if style.faint {
        flags |= CELL_FAINT;
    }
    if style.blink {
        flags |= CELL_BLINK;
    }
    if style.inverse {
        flags |= CELL_INVERSE;
    }
    if style.invisible {
        flags |= CELL_INVISIBLE;
    }
    if style.strikethrough {
        flags |= CELL_STRIKETHROUGH;
    }
    if style.overline {
        flags |= CELL_OVERLINE;
    }
    flags
}

/// The provenance record for one cell's style and content tag.
const fn cell_metadata(style: Style, content: CellContentTag) -> CellMetadata {
    let (foreground_kind, foreground_palette_index) = match style.fg_color {
        StyleColor::None => (COLOR_KIND_DEFAULT, 0),
        StyleColor::Palette(index) => (COLOR_KIND_PALETTE, index.0),
        StyleColor::Rgb(_) => (COLOR_KIND_RGB, 0),
    };
    CellMetadata {
        foreground_kind,
        foreground_palette_index,
        underline_color_is_default: matches!(style.underline_color, StyleColor::None),
        background_color_is_default: default_background(style, content),
    }
}

const fn default_background(style: Style, content: CellContentTag) -> bool {
    if matches!(
        content,
        CellContentTag::BgColorPalette | CellContentTag::BgColorRgb
    ) {
        return false;
    }
    matches!(style.bg_color, StyleColor::None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn underline_color_falls_back_to_resolved_cell_foreground() {
        let palette = [RgbColor { r: 0, g: 0, b: 0 }; 256];
        let cell_foreground = RgbColor {
            r: 0x12,
            g: 0x34,
            b: 0x56,
        };
        assert_eq!(
            resolve_color(StyleColor::None, cell_foreground, &palette),
            cell_foreground
        );
        let explicit = RgbColor {
            r: 0x65,
            g: 0x43,
            b: 0x21,
        };
        assert_eq!(
            resolve_color(StyleColor::Rgb(explicit), cell_foreground, &palette),
            explicit
        );
    }
}
