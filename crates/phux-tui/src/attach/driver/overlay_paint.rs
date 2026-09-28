//! Overlay-layer painting: the active-overlay compositor, the copy-mode
//! status strip, and the live agent-fleet refresh.

use std::collections::HashMap;
use std::io::{self, Write};

use phux_protocol::ids::ResourceId;

use crate::attach::paint::{SidebarReservation, StatusBarPaint, content_rect, paint_full_frame};
use crate::attach::pane_state::{AttachKernel, PaneSlot, VcsIndex};
use crate::attach::render::{SelectionRect, write_cup};
use crate::layout::Workspace;
use crate::render::chrome::status_bar::StatusBarPainter;
use crate::render::overlay::OverlayState;
use phux_client::agent_meta::AgentRecord;

/// Paint the active overlay layer. Copy mode repaints the focused pane with
/// its selection inverted plus a status line; every other overlay floats over
/// a repainted base frame. [`OverlayState::copy_selection`] picks the branch.
#[allow(
    clippy::too_many_arguments,
    reason = "mirrors paint_full_frame's paint context plus the overlay state"
)]
pub(super) fn paint_active_overlay<W: crate::attach::RenderSink>(
    out: &mut W,
    overlays: &OverlayState,
    workspace: &Workspace,
    panes: &mut HashMap<ResourceId, PaneSlot>,
    engine_kernel: &AttachKernel,
    focused: Option<&ResourceId>,
    // Base frames honor zoom; the copy-mode branch uses the real active
    // window because copy mode works on the focused pane regardless.
    zoomed: Option<&ResourceId>,
    viewport_dims: (u16, u16),
    status_bar: Option<&mut StatusBarPainter>,
    // Keeps panes inset under an overlay (no reflow flicker).
    sidebar: Option<SidebarReservation>,
    // The base repaint starts with ED2, so without the painter the sidebar
    // vanishes while a modal is open. Chrome persists under overlays.
    sidebar_painter: Option<&mut crate::render::chrome::sidebar::SidebarPainter>,
    session_name: &str,
    theme: &crate::render::Theme,
) -> StatusBarPaint {
    // Modals center in the pane content rect, never over the sidebar.
    let bar_pos = status_bar.as_deref().map(StatusBarPainter::position);
    let overlay_content = {
        let cr = content_rect(viewport_dims, bar_pos, sidebar);
        ratatui::layout::Rect::new(cr.x, cr.y, cr.w, cr.h)
    };
    if let Some(sel) = overlays.copy_selection() {
        let (Some(ls), Some(fid)) = (workspace.active_window(), focused) else {
            return StatusBarPaint::NotPublished;
        };
        // Set the selection for this one zoom-honoring paint, then clear it.
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
                    theme,
                )
            });
        if let Some(slot) = panes.get_mut(fid) {
            slot.renderer.set_selection(None);
        }
        let _ = paint_copy_mode_status(out, sel, viewport_dims, theme);
        // The strip lands on the bottom viewport row, which is a
        // pane row under a top-docked bar or no bar at all.
        crate::attach::pane_state::invalidate_all_fronts(panes);
        if matches!(
            bar_pos,
            Some(crate::render::chrome::status_bar::Position::Bottom)
        ) {
            StatusBarPaint::NotPublished
        } else {
            painted
        }
    } else if let Some(clip) = overlays.active_bounds(overlay_content) {
        // Floating modal: repaint the base frame (panes and sidebar stay
        // visible), then only the modal's bounded region; no full clear.
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
                        theme,
                    )
                });
        let _ = overlays.paint_clipped(out, viewport_dims, overlay_content, clip, theme.shadow);
        // The box sits over pane cells; forget their fronts (free insurance).
        crate::attach::pane_state::invalidate_all_fronts(panes);
        painted
    } else {
        // Full-screen overlay (no bounded region): clear + paint.
        let _ = out.write_all(b"\x1b[2J\x1b[H");
        let _ = overlays.paint(out, viewport_dims);
        // The clear and the overlay replaced every pane cell.
        crate::attach::pane_state::invalidate_all_fronts(panes);
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
    let status = format!(" copy-mode · {geom} · {cell_count} ");
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

/// Rebuild and repaint an open live session picker when a fresh host
/// inventory lands; a no-op otherwise.
#[allow(
    clippy::too_many_arguments,
    reason = "the picker projection reads session/host state and the overlay repaint context — main_loop locals threaded by reference, same shape as refresh_fleet_if_open"
)]
pub(super) fn refresh_session_picker_if_open<W: crate::attach::RenderSink>(
    out: &mut W,
    overlays: &mut OverlayState,
    workspace: &Workspace,
    panes: &mut HashMap<ResourceId, PaneSlot>,
    engine_kernel: &AttachKernel,
    focused_resource: Option<&ResourceId>,
    zoomed: Option<&ResourceId>,
    viewport_dims: (u16, u16),
    status_bar: Option<&mut StatusBarPainter>,
    sidebar: Option<SidebarReservation>,
    sidebar_painter: &mut crate::render::chrome::sidebar::SidebarPainter,
    session_name: &str,
    theme: &crate::render::Theme,
    sessions: &[phux_protocol::wire::info::SessionInfo],
    focused_session: Option<phux_protocol::ids::SessionId>,
    hosts: &[phux_protocol::wire::info::HostInventory],
) -> StatusBarPaint {
    if !overlays.is_active() {
        return StatusBarPaint::NotPublished;
    }
    let items = crate::attach::input_dispatch::session_picker_rows(
        sessions,
        focused_session,
        hosts,
        workspace,
    );
    if overlays.refresh_items(
        crate::attach::input_dispatch::SESSION_PICKER_LIVE_KEY,
        &items,
    ) {
        paint_active_overlay(
            out,
            overlays,
            workspace,
            panes,
            engine_kernel,
            focused_resource,
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

/// Rebuild and repaint an open live agent-fleet dashboard; a no-op
/// otherwise.
#[allow(
    clippy::too_many_arguments,
    reason = "the fleet projection reads workspace/session/agent state and the overlay repaint context — all main_loop locals threaded by reference, same shape as the paint helpers"
)]
pub(super) fn refresh_fleet_if_open<W: crate::attach::RenderSink>(
    out: &mut W,
    overlays: &mut OverlayState,
    workspace: &Workspace,
    panes: &mut HashMap<ResourceId, PaneSlot>,
    engine_kernel: &AttachKernel,
    focused_resource: Option<&ResourceId>,
    zoomed: Option<&ResourceId>,
    viewport_dims: (u16, u16),
    status_bar: Option<&mut StatusBarPainter>,
    sidebar: Option<SidebarReservation>,
    sidebar_painter: &mut crate::render::chrome::sidebar::SidebarPainter,
    session_name: &str,
    theme: &crate::render::Theme,
    sessions: &[phux_protocol::wire::info::SessionInfo],
    focused_session: Option<phux_protocol::ids::SessionId>,
    agent_meta: &HashMap<ResourceId, AgentRecord>,
    vcs: &mut VcsIndex,
    foreign_layouts: &HashMap<phux_protocol::ids::SessionId, Workspace>,
    foreign_agents: &HashMap<ResourceId, AgentRecord>,
    foreign_attention: &std::collections::HashSet<ResourceId>,
) -> StatusBarPaint {
    if !overlays.is_active() {
        return StatusBarPaint::NotPublished;
    }
    let meta = crate::attach::fleet::collect_pane_meta(
        panes,
        vcs,
        &crate::attach::agent_rows::agent_session_rows(engine_kernel),
    );
    let mut items = crate::attach::fleet::fleet_items(
        workspace,
        sessions,
        focused_session,
        agent_meta,
        &meta,
        foreign_layouts,
        foreign_agents,
    );
    items.extend(crate::attach::fleet::satellite_agent_items(
        foreign_agents,
        foreign_attention,
        workspace,
    ));
    if overlays.refresh_items(crate::attach::fleet::FLEET_LIVE_KEY, &items) {
        paint_active_overlay(
            out,
            overlays,
            workspace,
            panes,
            engine_kernel,
            focused_resource,
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
