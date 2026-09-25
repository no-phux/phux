//! Bounded copy keeps both allocation and formatter traversal off unbounded paths.

use super::{
    EngineDocumentSelection, FormatOptions, GhosttyEngineError, GhosttyReplica, Selection,
    SnapshotError,
};
use crate::engine::BoundedSelectionText;
use libghostty_vt::terminal::PointSpace;

/// Maximum conservative cell range sent to the pinned formatter. Includes
/// whole endpoint rows and one extra row for wide-glyph boundary expansion.
/// The byte-buffer API rescans on overflow, so a byte limit alone cannot bound
/// formatting work. Pinned page.zig also caps grapheme suffixes at 64 scalars
/// per cell. Anchor resolution still uses Ghostty's tracked-page lookup.
const MAX_FORMAT_CELLS: u64 = 1024 * 1024;

pub(super) fn format_bounded(
    replica: &GhosttyReplica,
    selection: EngineDocumentSelection,
    max_bytes: usize,
) -> Result<BoundedSelectionText, GhosttyEngineError> {
    let terminal = replica
        .terminal()
        .ok_or(GhosttyEngineError::LiveOutputBeforeReady)?;
    let (Some(start), Some(end)) = (
        replica.anchors.get(&selection.start),
        replica.anchors.get(&selection.end),
    ) else {
        return Ok(BoundedSelectionText::Unavailable);
    };
    let (Some(start), Some(end)) = (start.snapshot(terminal)?, end.snapshot(terminal)?) else {
        return Ok(BoundedSelectionText::Unavailable);
    };
    // Tracked points resolve on their owning screen, and snapshots validate
    // only the terminal. The formatter uses the active screen, so validate
    // both snapshots against that screen before range checks or formatting.
    let (Some(start_point), Some(end_point)) = (
        terminal.point_from_grid_ref(&start, PointSpace::History)?,
        terminal.point_from_grid_ref(&end, PointSpace::History)?,
    ) else {
        return Ok(BoundedSelectionText::Unavailable);
    };
    let rows = u64::from(start_point.y.abs_diff(end_point.y)) + 2;
    if rows * u64::from(terminal.cols()?) > MAX_FORMAT_CELLS {
        return Ok(BoundedSelectionText::WorkLimitExceeded);
    }
    let selection = Selection::new(start, end, selection.rectangle);
    let mut bytes = copy_buffer(max_bytes)?;
    // Pinned selection.zig:format_buf uses a fixed writer, then a discarding
    // count pass on overflow. Preflight above bounds that second traversal too.
    let formatted = terminal.format_selection_buf(
        FormatOptions::new()
            .with_selection(&selection)
            .with_unwrap(true)
            .with_trim(true),
        &mut bytes,
    );
    finish_copy(bytes, formatted, max_bytes)
}

fn copy_buffer(max_bytes: usize) -> Result<Vec<u8>, GhosttyEngineError> {
    // Always provide a nonempty buffer: a null/empty count-only path must not
    // turn a zero-byte budget into an unbounded allocation or copy.
    let capacity = max_bytes.checked_add(1).ok_or(SnapshotError::OutOfMemory)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(capacity)
        .map_err(|_| SnapshotError::OutOfMemory)?;
    bytes.resize(capacity, 0);
    Ok(bytes)
}

fn finish_copy(
    mut bytes: Vec<u8>,
    formatted: Result<Option<usize>, SnapshotError>,
    max_bytes: usize,
) -> Result<BoundedSelectionText, GhosttyEngineError> {
    match formatted {
        Ok(Some(written)) if written <= max_bytes => {
            bytes.truncate(written);
            Ok(BoundedSelectionText::Text(bytes))
        }
        Ok(None) => Ok(BoundedSelectionText::Unavailable),
        Ok(Some(_)) | Err(SnapshotError::OutOfSpace { .. }) => {
            Ok(BoundedSelectionText::ByteLimitExceeded)
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests;
