//! Frame metadata and dense viewport assembly from one engine snapshot.

use super::{Colors, Cursor, CursorWidth, GridBuffer, GridDamage, GridError, flatten};
use libghostty_vt::render::{CellIterator, CursorViewport, RowIterator, Snapshot};
use libghostty_vt::screen::CellWide;

pub(super) fn snapshot_header(
    snapshot: &Snapshot<'static, '_>,
    projected: bool,
) -> Result<(u16, u16, Colors, GridDamage), GridError> {
    let damage = if projected {
        GridDamage::from(snapshot.dirty()?)
    } else {
        GridDamage::Full
    };
    Ok((
        snapshot.cols()?,
        snapshot.rows()?,
        snapshot.colors()?,
        damage,
    ))
}

pub(super) fn flatten_viewport(
    rows: &mut RowIterator<'static>,
    cells: &mut CellIterator<'static>,
    snapshot: &Snapshot<'static, '_>,
    context: &flatten::CellContext<'_, '_>,
    buffer: &mut GridBuffer,
    scratch: &mut flatten::CellScratch,
) -> Result<(), GridError> {
    let expected = prepare_buffer(buffer, snapshot.cols()?, snapshot.rows()?)?;
    flatten::fill_grid_cells(rows, cells, snapshot, context, buffer, scratch)?;
    let produced = buffer.cells.len();
    if produced != expected {
        return Err(GridError::SparseViewport { produced, expected });
    }
    Ok(())
}

fn prepare_buffer(buffer: &mut GridBuffer, cols: u16, rows: u16) -> Result<usize, GridError> {
    buffer.clear();
    let expected = usize::from(cols)
        .checked_mul(usize::from(rows))
        .ok_or(GridError::Overflow("viewport cell count exceeds usize"))?;
    buffer.cells.reserve(expected);
    buffer.metadata.reserve(expected);
    Ok(expected)
}

pub(super) fn fill_fringe(
    context: &flatten::CellContext<'_, '_>,
    fringe: Option<u32>,
    cols: u16,
    scratch: &mut flatten::CellScratch,
    buffer: &mut GridBuffer,
) -> Result<(), GridError> {
    let Some(y) = fringe else {
        return Ok(());
    };
    match flatten::fill_fringe_row(context, y, cols, scratch, buffer) {
        Ok(()) => Ok(()),
        Err(GridError::Engine(libghostty_vt::Error::InvalidValue)) => {
            buffer.fringe.clear();
            buffer.fringe_metadata.clear();
            Ok(())
        }
        Err(error) => Err(error),
    }
}

/// Read cursor shape and which part of a wide glyph it occupies.
pub(super) fn read_cursor(
    snapshot: &Snapshot<'static, '_>,
    buffer: &GridBuffer,
    cols: u16,
) -> Result<Cursor, GridError> {
    let viewport = snapshot.cursor_viewport()?;
    let (col, row, width) = viewport.map_or((0, 0, CursorWidth::Narrow), |at| {
        (at.x, at.y, cursor_width(at, buffer, cols))
    });
    Ok(Cursor {
        visible: snapshot.cursor_visible()? && viewport.is_some(),
        col,
        row,
        style: snapshot.cursor_visual_style()?.into(),
        blinking: snapshot.cursor_blinking()?,
        width,
    })
}

/// The engine identifies a wide tail; the projected cell identifies its head.
fn cursor_width(at: CursorViewport, buffer: &GridBuffer, cols: u16) -> CursorWidth {
    if at.at_wide_tail {
        return CursorWidth::WideTail;
    }
    let index = usize::from(at.y) * usize::from(cols) + usize::from(at.x);
    if buffer
        .cells
        .get(index)
        .is_some_and(|cell| cell.wide == CellWide::Wide as u8)
    {
        CursorWidth::Wide
    } else {
        CursorWidth::Narrow
    }
}
