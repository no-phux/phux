//! Client-local copy-mode extraction and clipboard emission.
//!
//! A client-side projection (ADR-0030), not a wire tier: on commit the
//! overlay's viewport [`CopyRequest`] becomes a one-shot [`Selection`] on the
//! focused pane's own terminal, formatted and written to the host clipboard
//! via OSC 52.

use std::io::{self, Write};

use libghostty_vt::{
    Terminal as GhosttyTerminal,
    fmt::{Format, Formatter},
    screen::GridRef,
    selection::{SelectLineOptions, SelectWordOptions, Selection},
    terminal::{Point, PointCoordinate, PointSpace},
};
use phux_client_core::engine::DocumentSpace;
use phux_protocol::ids::ResourceId;

use super::pane_state::AttachKernel;
use crate::render::overlay::{CopyRequest, ScreenSelectionPoint, SearchMatch, SelectionGrab};

/// Base64 alphabet (RFC 4648 §4, standard, with `+`/`/` and `=` padding).
const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// The plain text of `req`'s selection in `terminal`.
///
/// `Rect` builds a two-corner selection (rectangular when `req.rectangle`);
/// the other grabs use libghostty's `select_*` at the overlay cursor
/// (`Output` needs OSC-133 zones). `None` when nothing is selectable or a
/// call fails (best-effort).
#[must_use]
pub fn extract_selection_text(
    terminal: &GhosttyTerminal<'_, '_>,
    req: CopyRequest,
) -> Option<String> {
    let selection = resolve_selection(terminal, req)?;
    let mut formatter = Formatter::new_with_selection(terminal, &selection).ok()?;
    formatter
        .set_format(Format::Plain)
        .ok()?
        .set_trim(true)
        .ok()?
        .set_unwrap(true)
        .ok()?;
    let bytes = formatter.format_alloc(None).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// The one-shot [`Selection`] `req`'s grab names, or `None`.
fn resolve_selection<'t>(
    terminal: &'t GhosttyTerminal<'_, '_>,
    req: CopyRequest,
) -> Option<Selection<'t>> {
    match req.grab {
        SelectionGrab::Rect => select_drag_rect(terminal, req),
        SelectionGrab::All => terminal.select_all().ok().flatten(),
        SelectionGrab::Word => select_word_at_cursor(terminal, req),
        SelectionGrab::Line | SelectionGrab::LineSemantic => select_line_at_cursor(terminal, req),
        SelectionGrab::Output => select_output_at_cursor(terminal, req),
    }
}

/// Map the overlay's inclusive `(row, col)` viewport rectangle onto two
/// [`Point::Viewport`] grid references and build the two-corner [`Selection`]
/// (rectangular when `req.rectangle`).
fn select_drag_rect<'t>(
    terminal: &'t GhosttyTerminal<'_, '_>,
    req: CopyRequest,
) -> Option<Selection<'t>> {
    // Endpoints are inclusive (see `Selection::new`); the overlay's
    // CellRange is already normalized so start <= end and both ends
    // name real cells.
    let Some(anchor) = req.mouse_anchor_screen else {
        let start = viewport_grid_ref(terminal, req.start_col, req.start_row)?;
        let end = viewport_grid_ref(terminal, req.end_col, req.end_row)?;
        return Some(Selection::new(start, end, req.rectangle));
    };
    // A pinned anchor may have scrolled out of the viewport, on either side
    // of the cursor: the two corners are the anchor's cell and the cursor.
    let anchor = screen_grid_ref(terminal, anchor)?;
    let cursor = viewport_cursor_ref(terminal, req)?;
    Some(Selection::new(anchor, cursor, req.rectangle))
}

/// Resolve a stored mouse press point against the full primary screen. This
/// is valid for both scrollback and active rows, unlike `Point::History`.
fn screen_grid_ref<'t>(
    terminal: &'t GhosttyTerminal<'_, '_>,
    point: ScreenSelectionPoint,
) -> Option<GridRef<'t>> {
    terminal
        .grid_ref(Point::Screen(PointCoordinate {
            x: point.col,
            y: point.row,
        }))
        .ok()
}

/// libghostty's own word selection at the overlay cursor.
fn select_word_at_cursor<'t>(
    terminal: &'t GhosttyTerminal<'_, '_>,
    req: CopyRequest,
) -> Option<Selection<'t>> {
    let cursor = viewport_cursor_ref(terminal, req)?;
    terminal
        .select_word(SelectWordOptions::new(cursor))
        .ok()
        .flatten()
}

/// libghostty's own line selection at the overlay cursor, honoring the
/// OSC-133 semantic-prompt boundary for [`SelectionGrab::LineSemantic`].
fn select_line_at_cursor<'t>(
    terminal: &'t GhosttyTerminal<'_, '_>,
    req: CopyRequest,
) -> Option<Selection<'t>> {
    let cursor = viewport_cursor_ref(terminal, req)?;
    let opts = SelectLineOptions::new(cursor)
        .with_semantic_prompt_boundary(req.grab == SelectionGrab::LineSemantic);
    terminal.select_line(opts).ok().flatten()
}

/// libghostty's own command-output selection at the overlay cursor.
///
/// Best-effort: no OSC-133 zones -> `None` -> silent no-op copy.
fn select_output_at_cursor<'t>(
    terminal: &'t GhosttyTerminal<'_, '_>,
    req: CopyRequest,
) -> Option<Selection<'t>> {
    let cursor = viewport_cursor_ref(terminal, req)?;
    terminal.select_output(cursor).ok().flatten()
}

/// The grid reference under the overlay cursor
/// (`req.cursor_row`/`req.cursor_col`) — the anchor every engine-derived grab
/// resolves from.
fn viewport_cursor_ref<'t>(
    terminal: &'t GhosttyTerminal<'_, '_>,
    req: CopyRequest,
) -> Option<GridRef<'t>> {
    viewport_grid_ref(terminal, req.cursor_col, req.cursor_row)
}

/// An overlay `(col, row)` as a viewport grid reference (the coordinates
/// index the visible viewport the user selected from).
fn viewport_grid_ref<'t>(
    terminal: &'t GhosttyTerminal<'_, '_>,
    col: u16,
    row: u16,
) -> Option<GridRef<'t>> {
    terminal
        .grid_ref(Point::Viewport(PointCoordinate {
            x: col,
            y: u32::from(row),
        }))
        .ok()
}

/// The full-screen row shown on viewport row 0: what a copy-mode anchor is
/// pinned against so it survives scrolling.
#[must_use]
pub fn viewport_screen_top(terminal: &GhosttyTerminal<'_, '_>) -> Option<u32> {
    let cell = viewport_grid_ref(terminal, 0, 0)?;
    terminal
        .point_from_grid_ref(&cell, PointSpace::Screen)
        .ok()?
        .map(|point| point.y)
}

/// The codepoint in each of viewport row `row`'s first `cols` cells (`0`
/// for an empty or unreadable cell), for copy-mode's word motions.
#[must_use]
pub fn viewport_row_cells(terminal: &GhosttyTerminal<'_, '_>, row: u16, cols: u16) -> Vec<u32> {
    (0..cols)
        .map(|col| {
            viewport_grid_ref(terminal, col, row)
                .and_then(|cell| cell.cell().ok())
                .and_then(|cell| cell.codepoint().ok())
                .unwrap_or(0)
        })
        .collect()
}

/// The most hits one copy-mode search registers; the history cache's anchor
/// budget may cap it lower.
const MAX_SEARCH_MATCHES: usize = 4096;

/// The document (history-space) row shown on viewport row 0, the offset
/// between a [`SearchMatch`] row and a pane-local one.
#[must_use]
pub fn viewport_top(terminal: &GhosttyTerminal<'_, '_>) -> Option<u32> {
    let cell = viewport_grid_ref(terminal, 0, 0)?;
    terminal
        .point_from_grid_ref(&cell, PointSpace::History)
        .ok()?
        .map(|point| point.y)
}

/// Every hit for `needle` in `terminal_id`'s loaded history, oldest first,
/// through the kernel's document search (the engine's case policy and wrap
/// handling). The engine anchors are released before returning: copy-mode
/// keeps plain document rows, valid until history is trimmed.
pub(crate) fn search_loaded(
    kernel: &mut AttachKernel,
    terminal_id: &ResourceId,
    needle: &str,
) -> Vec<SearchMatch> {
    let Ok(found) = kernel.search_loaded_history(terminal_id, needle, MAX_SEARCH_MATCHES) else {
        return Vec::new();
    };
    let point = |kernel: &AttachKernel, anchor| {
        kernel
            .document_anchor_point(terminal_id, anchor, DocumentSpace::History)
            .ok()
            .flatten()
    };
    let hits = found
        .matches
        .iter()
        .filter_map(|hit| {
            let (start, end) = (point(kernel, hit.start)?, point(kernel, hit.end)?);
            Some(SearchMatch {
                start_row: start.y,
                start_col: start.x,
                end_row: end.y,
                end_col: end.x,
            })
        })
        .collect();
    for hit in found.matches {
        let _ = kernel.release_document_anchor(terminal_id, hit.start);
        let _ = kernel.release_document_anchor(terminal_id, hit.end);
    }
    hits
}

/// The hit a search from document cell `from` lands on: the first one after
/// it, or (`backward`) the last one before it, wrapping around the ends.
#[must_use]
pub fn pick_match(matches: &[SearchMatch], from: (u32, u16), backward: bool) -> Option<usize> {
    let start = |hit: &SearchMatch| (hit.start_row, hit.start_col);
    let found = if backward {
        matches.iter().rposition(|hit| start(hit) < from)
    } else {
        matches.iter().position(|hit| start(hit) > from)
    };
    let wrapped = if backward {
        matches.len().checked_sub(1)
    } else {
        (!matches.is_empty()).then_some(0)
    };
    found.or(wrapped)
}

/// An OSC 52 "set clipboard" sequence: `ESC ] 52 ; c ; <base64> BEL`.
/// Whether the host honors it is up to the host.
#[must_use]
pub fn osc52_set_clipboard(text: &str) -> Vec<u8> {
    let encoded = base64_encode(text.as_bytes());
    let mut out = Vec::with_capacity(encoded.len() + 8);
    out.extend_from_slice(b"\x1b]52;c;");
    out.extend_from_slice(encoded.as_bytes());
    out.push(0x07); // BEL terminator
    out
}

/// The copy-mode extraction bridge (ADR-0045).
///
/// Resolves `req` against the focused pane's own engine and, when the
/// selection is non-empty, writes an OSC 52 clipboard sequence to `out`.
/// Nothing goes on the wire. Best-effort.
pub fn copy_to_host_clipboard<W: Write>(
    out: &mut W,
    terminal: &GhosttyTerminal<'_, '_>,
    req: CopyRequest,
) -> io::Result<()> {
    let Some(text) = extract_selection_text(terminal, req) else {
        return Ok(());
    };
    if text.is_empty() {
        return Ok(());
    }
    write_host_clipboard(out, &text)
}

/// Set the host clipboard to `text` with OSC 52 on the outer terminal.
pub fn write_host_clipboard<W: Write>(out: &mut W, text: &str) -> io::Result<()> {
    out.write_all(&osc52_set_clipboard(text))?;
    out.flush()
}

/// Standard padded base64 (RFC 4648), hand-rolled to avoid a dependency on a
/// cold path.
pub(in crate::attach) fn base64_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);

        out.push(char::from(B64[usize::from(b0 >> 2)]));
        out.push(char::from(B64[usize::from(((b0 & 0b11) << 4) | (b1 >> 4))]));
        if chunk.len() > 1 {
            out.push(char::from(
                B64[usize::from(((b1 & 0b1111) << 2) | (b2 >> 6))],
            ));
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(char::from(B64[usize::from(b2 & 0b11_1111)]));
        } else {
            out.push('=');
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests;
