//! Overlay-layer painting: the active-overlay compositor, the copy-mode
//! status strip, and the live agent-fleet refresh.

use std::collections::HashMap;
use std::io::{self, Write};

#[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
use phux_protocol::caps::BootstrapCapabilities;
use phux_protocol::ids::TerminalId;

use crate::agent_meta::AgentRecord;
use crate::attach::paint::{SidebarReservation, StatusBarPaint, content_rect, paint_full_frame};
use crate::attach::pane_state::{AttachKernel, PaneSlot, VcsIndex};
use crate::attach::render::{SelectionRect, write_cup};
use crate::layout::Workspace;
use crate::render::chrome::status_bar::StatusBarPainter;
use crate::render::overlay::OverlayState;

/// Paint the active overlay layer (called only when an overlay is active).
///
/// Copy-mode is **not** a modal overlay: it repaints the focused pane with its
/// selection reverse-videoed — the live content is otherwise untouched, so
/// nothing on screen swaps — plus a status line. Every other overlay is modal:
/// clear the screen and paint its own surface. The branch is chosen by
/// [`OverlayState::copy_selection`].
#[allow(
    clippy::too_many_arguments,
    reason = "mirrors paint_full_frame's paint context plus the overlay state"
)]
pub(super) fn paint_active_overlay<W: crate::attach::RenderSink>(
    out: &mut W,
    overlays: &OverlayState,
    workspace: &Workspace,
    panes: &mut HashMap<TerminalId, PaneSlot>,
    engine_kernel: &AttachKernel,
    focused: Option<&TerminalId>,
    // phux-x2hm: the driver's pane-zoom state. The base-frame repaints below
    // render through `Workspace::render_window` so the zoomed pane fills the
    // window; the copy-mode branch keeps using the REAL active window because
    // copy mode operates on the focused pane regardless of zoom.
    zoomed: Option<&TerminalId>,
    viewport_dims: (u16, u16),
    status_bar: Option<&mut StatusBarPainter>,
    // phux-4h5a: the sidebar reservation, so base-frame repaints under an
    // overlay keep panes inset (no reflow flicker when a modal opens).
    // `None` reservation (default) is byte-identical.
    sidebar: Option<SidebarReservation>,
    // phux-foz.10: the sidebar strip painter. The base-frame repaint under a
    // floating overlay starts with ED2 (full clear), so without the painter
    // the reserved columns stay blank and the sidebar vanishes for as long
    // as the palette / help / prompt / which-key overlay is open. Chrome
    // persists under overlays: overlays float above content, not above
    // chrome.
    sidebar_painter: Option<&mut crate::render::chrome::sidebar::SidebarPainter>,
    session_name: &str,
    theme: &crate::render::Theme,
) -> StatusBarPaint {
    // phux-foz.14: floating modals center inside the pane content rect (the
    // viewport minus the sidebar strip and status-bar row), NOT the raw
    // viewport, so a centered box never lands on the sidebar columns and
    // occludes the chrome the base-frame repaint (phux-foz.10) preserves. The
    // borrow of `status_bar` ends here (position is `Copy`), so it stays
    // available to move into `paint_full_frame` below.
    let bar_pos = status_bar.as_deref().map(StatusBarPainter::position);
    let overlay_content = {
        let cr = content_rect(viewport_dims, bar_pos, sidebar);
        ratatui::layout::Rect::new(cr.x, cr.y, cr.w, cr.h)
    };
    if let Some(sel) = overlays.copy_selection() {
        let (Some(ls), Some(fid)) = (workspace.active_window(), focused) else {
            return StatusBarPaint::NotPublished;
        };
        // Set the selection on the focused renderer for this one paint, repaint
        // the (zoom-honoring) base frame — the renderer inverts the selected
        // cells with their own styles — then clear it so ordinary renders are
        // unaffected. `ls` only gated the early-return on focus; the actual
        // paint goes through the zoomed view so the base matches the screen.
        let _ = ls;
        let base = workspace.render_window(zoomed);
        if let Some(slot) = panes.get_mut(fid) {
            slot.renderer.set_selection(Some(sel));
        }
        let painted = base
            .as_deref()
            .map_or(StatusBarPaint::NotPublished, |base| {
                paint_full_frame(
                    out,
                    base,
                    panes,
                    engine_kernel,
                    focused,
                    viewport_dims,
                    status_bar,
                    sidebar,
                    sidebar_painter,
                    session_name,
                )
            });
        if let Some(slot) = panes.get_mut(fid) {
            slot.renderer.set_selection(None);
        }
        let _ = paint_copy_mode_status(out, sel, viewport_dims, theme);
        if matches!(
            bar_pos,
            Some(crate::render::chrome::status_bar::Position::Bottom)
        ) {
            StatusBarPaint::NotPublished
        } else {
            painted
        }
    } else if let Some(clip) = overlays.active_bounds(overlay_content) {
        // Floating modal (help / prompt / command palette / pickers): keep
        // the live panes visible by repainting the base frame, then emit
        // only the modal's bounded region on top. No `\x1b[2J` — the panes
        // surround the box instead of vanishing behind a full-screen clear.
        // The base frame includes the sidebar strip (phux-foz.10): the
        // repaint's own ED2 cleared it, and chrome must persist under a
        // floating overlay.
        let painted =
            workspace
                .render_window(zoomed)
                .as_deref()
                .map_or(StatusBarPaint::NotPublished, |ls| {
                    paint_full_frame(
                        out,
                        ls,
                        panes,
                        engine_kernel,
                        focused,
                        viewport_dims,
                        status_bar,
                        sidebar,
                        sidebar_painter,
                        session_name,
                    )
                });
        let _ = overlays.paint_clipped(out, viewport_dims, overlay_content, clip, theme.shadow);
        painted
    } else {
        // Full-screen overlay (no bounded region): clear + paint.
        let _ = out.write_all(b"\x1b[2J\x1b[H");
        let _ = overlays.paint(out, viewport_dims);
        StatusBarPaint::NotPublished
    }
}

/// Emit the copy-mode status strip over the bottom viewport row, then hide the
/// hardware cursor (the reverse-video selection is the position indicator).
pub(super) fn paint_copy_mode_status<W: Write>(
    out: &mut W,
    sel: SelectionRect,
    viewport_dims: (u16, u16),
    theme: &crate::render::Theme,
) -> io::Result<()> {
    let (cols, rows) = viewport_dims;
    if rows == 0 || cols == 0 {
        return Ok(());
    }
    let span_rows = u32::from(sel.end_row - sel.start_row + 1);
    let cell_count = if sel.rectangle {
        // Block (rectangle) selection: a columnar band on every spanned row, so
        // the count is span_rows * band_cols. The overlay only tuple-normalizes
        // the corners by `(row, col)`, which does NOT order the columns, so the
        // band width takes the min/max of the two column bounds — a plain
        // `end_col - start_col` would underflow whenever the drag runs up-left
        // (cursor column left of the anchor's on a lower row).
        let band_cols =
            u32::from(sel.start_col.max(sel.end_col) - sel.start_col.min(sel.end_col)) + 1;
        span_rows * band_cols
    } else {
        // Linear (text-flow) selection: the historical bounding-box arithmetic.
        // `saturating_sub` keeps the value identical for the ordered common
        // case while refusing to underflow on a multi-row drag whose corners
        // tuple-normalize to `start_col > end_col`.
        span_rows * (u32::from(sel.end_col.saturating_sub(sel.start_col)) + 1)
    };
    // Surface the active geometry from the one bit the renderer carries: block
    // (columnar band) vs linear (text-flow, incl. whole-line Line mode). `Tab`
    // cycles it (ADR-0045).
    let geom = if sel.rectangle { "block" } else { "linear" };
    let status = format!(
        " copy-mode | {geom} | {cell_count} cell(s) | arrows/PgUp/PgDn scroll | Tab mode | Enter copy | Esc "
    );
    write_cup(out, rows - 1, 0)?;
    // Selection strip from the theme (`selection_bg`/`selection_fg`). `\x1b[K`
    // fills the rest of the row with the strip bg; then reset + hide the cursor.
    out.write_all(b"\x1b[0m")?;
    crate::render::write_sgr_color(out, theme.selection_bg, false)?;
    crate::render::write_sgr_color(out, theme.selection_fg, true)?;
    let visible: String = status.chars().take(cols as usize).collect();
    out.write_all(visible.as_bytes())?;
    out.write_all(b"\x1b[K\x1b[0m\x1b[?25l")?;
    out.flush()
}

/// phux-jpqd: rebuild and repaint the agent-fleet dashboard in place when it
/// is the active live overlay. Extracted from `main_loop`'s per-frame fleet
/// refresh so the foreign-topology intercepts (layout + agent-record GET
/// replies, which `continue` past the general frame handler) can trigger the
/// same push refresh. A no-op unless a live fleet list is on the overlay
/// stack ([`OverlayState::refresh_items`] returns `false`).
#[allow(
    clippy::too_many_arguments,
    reason = "the fleet projection reads workspace/session/agent state and the overlay repaint context — all main_loop locals threaded by reference, same shape as the paint helpers"
)]
pub(super) fn refresh_fleet_if_open<W: crate::attach::RenderSink>(
    out: &mut W,
    overlays: &mut OverlayState,
    workspace: &Workspace,
    panes: &mut HashMap<TerminalId, PaneSlot>,
    engine_kernel: &AttachKernel,
    focused_pane: Option<&TerminalId>,
    zoomed: Option<&TerminalId>,
    viewport_dims: (u16, u16),
    status_bar: Option<&mut StatusBarPainter>,
    sidebar: Option<SidebarReservation>,
    sidebar_painter: &mut crate::render::chrome::sidebar::SidebarPainter,
    session_name: &str,
    theme: &crate::render::Theme,
    sessions: &[phux_protocol::wire::info::SessionInfo],
    focused_session: Option<phux_protocol::ids::SessionId>,
    agent_meta: &HashMap<TerminalId, AgentRecord>,
    vcs: &mut VcsIndex,
    foreign_layouts: &HashMap<phux_protocol::ids::SessionId, Workspace>,
    foreign_agents: &HashMap<TerminalId, AgentRecord>,
) -> StatusBarPaint {
    if !overlays.is_active() {
        return StatusBarPaint::NotPublished;
    }
    let meta = crate::attach::fleet::collect_pane_meta(panes, vcs);
    let items = crate::attach::fleet::fleet_items(
        workspace,
        sessions,
        focused_session,
        agent_meta,
        &meta,
        foreign_layouts,
        foreign_agents,
    );
    if overlays.refresh_items(crate::attach::fleet::FLEET_LIVE_KEY, &items) {
        paint_active_overlay(
            out,
            overlays,
            workspace,
            panes,
            engine_kernel,
            focused_pane,
            zoomed,
            viewport_dims,
            status_bar,
            sidebar,
            Some(sidebar_painter),
            session_name,
            theme,
        )
    } else {
        StatusBarPaint::NotPublished
    }
}
