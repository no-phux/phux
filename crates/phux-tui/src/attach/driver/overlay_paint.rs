//! Overlay-layer painting: the active-overlay compositor and the copy-mode
//! status strip.

use std::collections::HashMap;
use std::io::{self, Write};

use phux_protocol::ids::ResourceId;

use crate::attach::chrome_ctx::ChromeCtx;
use crate::attach::paint::{StatusBarPaint, paint_full_frame};
use crate::attach::pane_state::{AttachKernel, PaneSlot};
use crate::attach::render::{SelectionRect, write_cup};
use crate::layout::LayoutState;
use crate::render::chrome::status_bar::Position;
use crate::render::overlay::CopyMarks;
use crate::render::overlay::OverlayState;

/// Paint the active overlay layer. Copy mode repaints the focused pane with
/// its selection inverted plus a status line; every other overlay floats over
/// a repainted base frame. [`OverlayState::copy_selection`] picks the branch.
///
/// `base` is the render window (zoom honored), `None` for an empty workspace.
/// The chrome keeps panes inset under an overlay (no reflow flicker), and its
/// sidebar painter keeps the strip visible under a modal: the base repaint
/// starts with ED2, so without the painter the sidebar vanishes.
pub(super) fn paint_active_overlay<W: crate::attach::RenderSink>(
    out: &mut W,
    overlays: &OverlayState,
    base: Option<&LayoutState>,
    panes: &mut HashMap<ResourceId, PaneSlot>,
    engine_kernel: &AttachKernel,
    focused: Option<&ResourceId>,
    chrome: &mut ChromeCtx<'_>,
) -> StatusBarPaint {
    if let Some(sel) = overlays.copy_selection() {
        return paint_copy_mode(
            out,
            sel,
            overlays,
            base,
            panes,
            engine_kernel,
            (focused, chrome),
        );
    }
    // Modals center in the pane content rect, never over the sidebar.
    let overlay_content = {
        let cr = chrome.content_layout().rect;
        ratatui::layout::Rect::new(cr.x, cr.y, cr.w, cr.h)
    };
    let viewport_dims = chrome.viewport;
    let Some(clip) = overlays.active_bounds(overlay_content) else {
        // Full-screen overlay (no bounded region): clear + paint.
        let _ = out.write_all(b"\x1b[2J\x1b[H");
        let _ = overlays.paint(out, viewport_dims);
        // The clear and the overlay replaced every pane cell.
        crate::attach::pane_state::invalidate_all_fronts(panes);
        return StatusBarPaint::NotPublished;
    };
    // Floating modal: repaint the base frame (panes and sidebar stay
    // visible), then only the modal's bounded region; no full clear.
    let painted = base.map_or(StatusBarPaint::NotPublished, |ls| {
        paint_full_frame(out, ls, panes, engine_kernel, focused, chrome)
    });
    let shadow = chrome.theme.shadow;
    let _ = overlays.paint_clipped(out, viewport_dims, overlay_content, clip, shadow);
    // The box sits over pane cells; forget their fronts (free insurance).
    crate::attach::pane_state::invalidate_all_fronts(panes);
    painted
}

/// Copy mode: repaint the base frame with the focused pane's selection
/// inverted and its other visible search hits underlined, then the copy-mode
/// status strip over the bottom row.
fn paint_copy_mode<W: crate::attach::RenderSink>(
    out: &mut W,
    sel: SelectionRect,
    overlays: &OverlayState,
    base: Option<&LayoutState>,
    panes: &mut HashMap<ResourceId, PaneSlot>,
    engine_kernel: &AttachKernel,
    (focused, chrome): (Option<&ResourceId>, &mut ChromeCtx<'_>),
) -> StatusBarPaint {
    let (Some(base), Some(fid)) = (base, focused) else {
        return StatusBarPaint::NotPublished;
    };
    // Set the marks for this one zoom-honoring paint, then clear them.
    if let Some(slot) = panes.get_mut(fid) {
        let matches = visible_search_hits(overlays, engine_kernel, fid);
        slot.renderer.set_copy_marks(CopyMarks {
            selection: Some(sel),
            matches,
        });
    }
    let painted = paint_full_frame(out, base, panes, engine_kernel, focused, chrome);
    if let Some(slot) = panes.get_mut(fid) {
        slot.renderer.set_copy_marks(CopyMarks::default());
    }
    let search = overlays.copy_search_status();
    let content = chrome.content_layout().rect;
    let _ = paint_copy_mode_status(
        out,
        sel,
        search.as_deref(),
        content,
        chrome.viewport,
        chrome.theme,
    );
    // The strip lands on the bottom viewport row, which is a
    // pane row under a top-docked bar or no bar at all.
    crate::attach::pane_state::invalidate_all_fronts(panes);
    if matches!(chrome.bar(), Some(Position::Bottom)) {
        StatusBarPaint::NotPublished
    } else {
        painted
    }
}

/// The copy-mode search hits on screen in `fid`'s pane, as pane-local
/// rectangles, minus the current one (the selection already inverts it).
fn visible_search_hits(
    overlays: &OverlayState,
    engine_kernel: &AttachKernel,
    fid: &ResourceId,
) -> Vec<SelectionRect> {
    let Some(view) = overlays.copy_search_view() else {
        return Vec::new();
    };
    let Some(terminal) = crate::attach::pane_state::published_terminal(engine_kernel, fid) else {
        return Vec::new();
    };
    let (Some(top), Ok(cols), Ok(rows)) = (
        crate::attach::copy::viewport_top(terminal),
        terminal.cols(),
        terminal.rows(),
    ) else {
        return Vec::new();
    };
    view.matches
        .iter()
        .enumerate()
        .filter(|(index, _)| Some(*index) != view.current)
        .filter_map(|(_, hit)| hit.viewport_rect(top, rows, cols))
        .collect()
}

/// The copy-mode keys, right-aligned on the strip when they fit.
const COPY_MODE_KEYS: &str = "⏎ copy · w word · v line · / find · tab block · esc";

/// Emit the copy-mode status strip over the bottom row of `content` (the
/// pane columns; the sidebar keeps its own rows), then hide the hardware
/// cursor (the reverse-video selection is the position indicator).
/// `search` is the search line being typed or the last search's position.
pub(super) fn paint_copy_mode_status<W: Write>(
    out: &mut W,
    sel: SelectionRect,
    search: Option<&str>,
    content: crate::layout::Rect,
    viewport_dims: (u16, u16),
    theme: &crate::render::Theme,
) -> io::Result<()> {
    let (cols, rows) = viewport_dims;
    if rows == 0 || cols == 0 || content.w == 0 {
        return Ok(());
    }
    let span_rows = u32::from(sel.end_row - sel.start_row + 1);
    let cell_count = if sel.rectangle {
        // Block selection: span_rows * band_cols, the band taking min/max of
        // the column bounds (tuple-normalized corners can have start_col >
        // end_col on an up-left drag).
        let band_cols =
            u32::from(sel.start_col.max(sel.end_col) - sel.start_col.min(sel.end_col)) + 1;
        span_rows * band_cols
    } else {
        // Linear: bounding-box arithmetic, saturating on reversed columns.
        span_rows * (u32::from(sel.end_col.saturating_sub(sel.start_col)) + 1)
    };
    // Block vs linear, cycled by `Tab` (ADR-0045).
    let geom = if sel.rectangle { "block" } else { "linear" };
    let status = search.map_or_else(
        || format!(" copy-mode · {geom} · {cell_count} "),
        |search| format!(" copy-mode · {geom} · {cell_count} · {search} "),
    );
    let width = usize::from(content.w.min(cols.saturating_sub(content.x)));
    let status = crate::render::clip_text(&status, width);
    let status_w = crate::render::display_width(&status);
    // The key legend, when the status leaves room for it and a gap.
    let keys = format!(" {COPY_MODE_KEYS} ");
    let keys_w = crate::render::display_width(&keys);
    let legend = (status_w + keys_w + 2 <= width).then_some(keys);
    let fill = width - status_w - legend.as_ref().map_or(0, |_| keys_w);
    write_cup(out, rows - 1, content.x)?;
    // Selection strip from the theme (`selection_bg`/`selection_fg`), padded
    // to the pane columns only; then reset + hide the cursor.
    out.write_all(b"\x1b[0m")?;
    crate::render::write_sgr_color(out, theme.selection_bg, false)?;
    crate::render::write_sgr_color(out, theme.selection_fg, true)?;
    out.write_all(status.as_bytes())?;
    out.write_all(" ".repeat(fill).as_bytes())?;
    if let Some(keys) = legend {
        out.write_all(keys.as_bytes())?;
    }
    out.write_all(b"\x1b[0m\x1b[?25l")?;
    out.flush()
}
