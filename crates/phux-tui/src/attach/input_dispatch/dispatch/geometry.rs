//! Viewport scrolling, copy search, and pane/overlay geometry.

use super::{
    ArrangementDoc, CopySearchRequest, CopySearchResult, DispatchCtx, FrameKind, HashMap,
    OverlayState, PaneSlot, PendingSplit, ResourceId, ScrollViewport, SidebarReservation, actions,
    content_rect,
};

/// Apply `delta` and report whether the history viewport offset changed.
pub(super) fn replica_scroll_moved(
    replica: &mut phux_client_core::engine::ghostty::GhosttyReplica,
    delta: isize,
) -> bool {
    let before = replica_viewport_offset(replica);
    if replica
        .scroll_viewport(ScrollViewport::Delta(delta))
        .is_err()
    {
        return false;
    }
    let after = replica_viewport_offset(replica);
    before.zip(after).is_some_and(|(a, b)| a != b)
}

fn replica_viewport_offset(
    replica: &phux_client_core::engine::ghostty::GhosttyReplica,
) -> Option<u64> {
    replica.terminal()?.scrollbar().ok().map(|bar| bar.offset)
}

pub(in crate::attach::input_dispatch) fn scroll_focused_pane_viewport(
    kernel: &mut crate::attach::pane_state::AttachKernel,
    panes: &mut HashMap<ResourceId, PaneSlot>,
    focused_resource: Option<&ResourceId>,
    delta: isize,
) -> bool {
    if delta == 0 {
        return false;
    }
    let Some(fid) = focused_resource else {
        return false;
    };
    let Some(slot) = panes.get_mut(fid) else {
        return false;
    };
    let Some(replica) = kernel.published_engine_mut(fid) else {
        return false;
    };
    if replica
        .scroll_viewport(ScrollViewport::Delta(delta))
        .is_err()
    {
        return false;
    }
    if delta < 0 {
        slot.viewport_scrolled = true;
    }
    true
}

/// Run a copy-mode search on the focused pane: find the hit after (or
/// before) the cursor, scroll it into view when it is off screen, and report
/// every hit with the viewport's new top row.
pub(in crate::attach::input_dispatch) fn copy_search(
    kernel: &mut crate::attach::pane_state::AttachKernel,
    panes: &mut HashMap<ResourceId, PaneSlot>,
    focused_resource: Option<&ResourceId>,
    req: &CopySearchRequest,
) -> CopySearchResult {
    use crate::attach::copy::{pick_match, search_loaded, viewport_top};
    use crate::attach::pane_state::published_terminal;

    let Some(fid) = focused_resource else {
        return CopySearchResult::NotFound;
    };
    let Some(top) = published_terminal(kernel, fid).and_then(viewport_top) else {
        return CopySearchResult::NotFound;
    };
    let matches = search_loaded(kernel, fid, &req.needle);
    let from = (
        top.saturating_add(u32::from(req.cursor_row)),
        req.cursor_col,
    );
    let Some(current) = pick_match(&matches, from, req.backward) else {
        return CopySearchResult::NotFound;
    };
    let hit = matches[current];
    let rows = u32::from(req.pane_rows.max(1));
    if hit.start_row < top || hit.end_row >= top.saturating_add(rows) {
        // Center the hit, as far as the scrollback allows.
        let want = i64::from(hit.start_row.saturating_sub(rows / 2));
        let delta = isize::try_from(want - i64::from(top)).unwrap_or_default();
        scroll_focused_pane_viewport(kernel, panes, Some(fid), delta);
    }
    let top = published_terminal(kernel, fid)
        .and_then(viewport_top)
        .unwrap_or(top);
    CopySearchResult::Found {
        matches,
        current,
        top,
    }
}

/// Snap `focused_resource`'s viewport back to the live screen if a wheel /
/// copy-mode scroll left it pinned in scrollback. Returns `true` iff the
pub(in crate::attach::input_dispatch) fn snap_scrolled_viewport(
    kernel: &mut crate::attach::pane_state::AttachKernel,
    panes: &mut HashMap<ResourceId, PaneSlot>,
    focused_resource: Option<&ResourceId>,
) -> bool {
    let Some((fid, slot)) =
        focused_resource.and_then(|fid| panes.get_mut(fid).map(|slot| (fid, slot)))
    else {
        return false;
    };
    if !slot.viewport_scrolled {
        return false;
    }
    let Some(replica) = kernel.published_engine_mut(fid) else {
        return false;
    };
    if replica.scroll_viewport(ScrollViewport::Bottom).is_err() {
        return false;
    }
    slot.viewport_scrolled = false;
    true
}

pub(in crate::attach::input_dispatch) fn focused_pane_rect(
    ctx: &DispatchCtx<'_>,
    focused_resource: Option<&ResourceId>,
) -> crate::layout::Rect {
    super::focused_pane_rect_for(
        ctx.workspace,
        ctx.zoomed.as_ref(),
        focused_resource,
        ctx.viewport,
        ctx.bar,
        ctx.sidebar,
    )
}

/// `SPAWN_RESOURCE.initial_size`: the tile `predict` says the new leaf will
/// occupy, or `None` (field absent) when unsupported or degenerate.
pub(in crate::attach::input_dispatch) fn spawn_initial_size(
    ctx: &DispatchCtx<'_>,
    predict: impl FnOnce(crate::layout::Rect) -> Option<(u16, u16)>,
) -> Option<(u16, u16)> {
    if !ctx.spawn_initial_size_supported {
        return None;
    }
    let content = content_rect(ctx.viewport, ctx.bar, ctx.sidebar);
    // A zero axis means there is nothing to render into; the server reads a
    // zero as "unknown" anyway, so do not spend a field on it.
    predict(content).filter(|&(cols, rows)| cols > 0 && rows > 0)
}

/// [`spawn_initial_size`] for a `split-pane`: tile the split this client is
/// about to ask for and read the new leaf's rect out of it.
pub(in crate::attach::input_dispatch) fn predicted_split_size(
    ctx: &DispatchCtx<'_>,
    pending: &PendingSplit,
) -> Option<(u16, u16)> {
    let active = ctx.workspace.active_group()?.clone();
    spawn_initial_size(ctx, |content| {
        actions::predicted_spawn_dims(&active, pending, content)
    })
}

/// Stamp `size` onto an already-built `SPAWN_RESOURCE` frame — the plugin-pane
/// path builds the frame from its manifest entry before it knows which
/// placement (and therefore which tile) it is about to park.
pub(in crate::attach::input_dispatch) const fn set_spawn_initial_size(
    frame: &mut FrameKind,
    size: Option<(u16, u16)>,
) {
    if let FrameKind::SpawnResource { initial_size, .. } = frame {
        *initial_size = size;
    }
}

pub(in crate::attach) fn focused_pane_rect_for(
    workspace: &ArrangementDoc,
    zoomed: Option<&ResourceId>,
    focused_resource: Option<&ResourceId>,
    viewport: (u16, u16),
    bar: Option<crate::render::chrome::status_bar::Position>,
    sidebar: Option<SidebarReservation>,
) -> crate::layout::Rect {
    let content = content_rect(viewport, bar, sidebar);
    let Some(fid) = focused_resource else {
        return content;
    };
    workspace
        .render_group(zoomed)
        .and_then(|layout| {
            crate::multi_pane::compute_layout_in(&layout, content, viewport)
                .rects
                .get(fid)
                .copied()
        })
        .unwrap_or(content)
}

/// Hand every overlay the focused pane's current rect. The choke point for
/// rect changes without SIGWINCH (a peer's layout, a spawn/close reflow);
/// copy mode is the overlay that cares.
pub(in crate::attach) fn sync_overlays_to_focused_pane(
    overlays: &mut OverlayState,
    workspace: &ArrangementDoc,
    zoomed: Option<&ResourceId>,
    focused_resource: Option<&ResourceId>,
    viewport: (u16, u16),
    bar: Option<crate::render::chrome::status_bar::Position>,
    sidebar: Option<SidebarReservation>,
) {
    if !overlays.is_active() {
        return;
    }
    let pane =
        super::focused_pane_rect_for(workspace, zoomed, focused_resource, viewport, bar, sidebar);
    overlays.on_viewport_resize(pane.w, pane.h);
}
