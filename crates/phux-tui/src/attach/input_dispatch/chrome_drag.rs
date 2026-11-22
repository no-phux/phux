//! Pointer drags over client chrome: the sidebar edge, the two window
//! strips, and a pane's title. Pane-divider drags (ADR-0048) share the grab
//! slot ([`DragGrab`]).
//!
//! Everything here is synchronous and client-local; the one wire effect, a
//! reordered layout broadcast on release, is [`DragCommit::broadcast`]. A
//! dragged sidebar width lasts for the attach and is never written to
//! `config.toml`.

use std::collections::HashMap;

use phux_protocol::ResourceId;
use phux_protocol::input::mouse::MouseEvent;

use super::ctx::{DispatchCtx, DragGrab, WindowGrab, WindowStrip};
use super::dispatch::{drag_resize, quantize_cell};
use crate::attach::paint::{SidebarEdge, SidebarReservation, content_rect, sidebar_rect};
use crate::attach::pane_state::PaneSlot;
use crate::render::chrome::sidebar::{SidebarHit, hit_test};

/// The narrowest a dragged sidebar may get. Narrower than this, a window
/// name no longer fits beside its status glyph.
pub(super) const MIN_DRAGGED_SIDEBAR_COLS: u16 = 16;

/// What releasing a grab asks the async caller to do.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct DragCommit {
    /// The release changed what is on screen.
    pub layout_changed: bool,
    /// The release changed shared layout that other clients must see.
    pub broadcast: bool,
    /// The pane focus moves to: the one a dragged pane joined.
    pub focus: Option<ResourceId>,
}

/// Advance `grab` by one button-motion event. Returns whether the screen
/// changed. A window grab updates the insertion marker to the slot under
/// the pointer; the reorder still happens where the pointer is released.
pub(super) fn motion(ctx: &mut DispatchCtx<'_>, grab: &DragGrab, mouse: &MouseEvent) -> bool {
    match grab {
        DragGrab::Divider(divider) => drag_resize(ctx, mouse, divider),
        DragGrab::SidebarEdge => resize_sidebar(ctx, quantize_cell(mouse.x)),
        DragGrab::Window(_) => preview_window_drop(ctx, mouse),
        // ponytail: no drop marker while a pane is carried; outline the
        // target if a drop on the wrong pane turns out to be easy to make.
        DragGrab::Pane(_) => false,
    }
}

/// Commit `grab` at the release position. The caller has already cleared
/// the grab slot.
pub(super) fn release(
    ctx: &mut DispatchCtx<'_>,
    grab: &DragGrab,
    mouse: &MouseEvent,
) -> DragCommit {
    match grab {
        // The divider re-tuned the layout on every motion; the release only
        // publishes the final ratio (ADR-0048).
        DragGrab::Divider(_) => DragCommit {
            layout_changed: false,
            broadcast: true,
            focus: None,
        },
        // Width is client-local presentation; motion already applied it.
        DragGrab::SidebarEdge => DragCommit::default(),
        DragGrab::Window(window) => {
            let moved = drop_window(ctx, *window, mouse);
            DragCommit {
                // Always dirty: the insertion marker has to leave the
                // strip even when the order did not change.
                layout_changed: true,
                broadcast: moved,
                focus: None,
            }
        }
        DragGrab::Pane(pane) => {
            let joined = join_slot(ctx, pane, mouse);
            DragCommit {
                layout_changed: joined.is_some(),
                broadcast: joined.is_some(),
                focus: joined,
            }
        }
    }
}

/// The pane whose title, in the rule above it, is under the pointer: the
/// handle a pane is dragged by onto another pane's slot. Only where the
/// drop can be kept: an arrangement record on a server that keeps slot
/// tabs. Elsewhere a press there falls through to the divider.
pub(super) fn pane_title_at(
    ctx: &DispatchCtx<'_>,
    panes: &HashMap<ResourceId, PaneSlot>,
    layout: &crate::layout::LayoutState,
    content: crate::layout::Rect,
    mouse: &MouseEvent,
) -> Option<ResourceId> {
    if !ctx.slot_tabs {
        return None;
    }
    let (x, y) = (quantize_cell(mouse.x), quantize_cell(mouse.y));
    let multi = crate::attach::multi_pane::compute_layout_in(layout, content, ctx.viewport);
    multi.rects.iter().find_map(|(id, rect)| {
        if rect.y.checked_sub(1) != Some(y) {
            return None;
        }
        let label = crate::attach::pane_state::pane_label(ctx.engine_kernel, panes, id)?;
        let (start, end) = crate::render::chrome::dividers::title_span(*rect, &label, ctx.theme)?;
        (start..end).contains(&x).then(|| id.clone())
    })
}

/// Pick up `pane` by its title.
pub(super) fn begin_pane_drag(ctx: &mut DispatchCtx<'_>, pane: ResourceId) {
    tracing::debug!(?pane, "pane drag: grabbed");
    *ctx.drag = Some(DragGrab::Pane(pane));
}

/// Make `pane` the last tab of the slot of the pane under the release
/// point. That pane, or `None` when the drop lands on no other pane.
fn join_slot(
    ctx: &mut DispatchCtx<'_>,
    pane: &ResourceId,
    mouse: &MouseEvent,
) -> Option<ResourceId> {
    use crate::attach::multi_pane::{RouteDecision, route_mouse_event};
    let content = content_rect(ctx.viewport, ctx.bar, ctx.sidebar);
    let target = match route_mouse_event(
        &*ctx.workspace.render_group(ctx.zoomed.as_ref())?,
        content,
        ctx.viewport,
        mouse,
    ) {
        RouteDecision::Pane { target, .. } if target != *pane => target,
        _ => return None,
    };
    match ctx.workspace.add_tab(pane, &target) {
        Ok(()) => {
            tracing::debug!(?pane, ?target, "pane drag: joined slot");
            Some(target)
        }
        Err(err) => {
            tracing::debug!(?pane, ?target, error = %err, "pane drag: refused");
            None
        }
    }
}

/// Whether an outer-viewport cell is the sidebar's resize handle: the
/// separator rule, which always sits on the pane-facing side. Its bottom
/// cell stays the collapse chevron.
pub(super) fn on_sidebar_edge(ctx: &DispatchCtx<'_>, x: u16, y: u16) -> bool {
    let Some(res) = ctx.sidebar else {
        return false;
    };
    let strip = sidebar_rect(ctx.viewport, res);
    if strip.w == 0 || y < strip.y || y >= strip.y.saturating_add(strip.h) {
        return false;
    }
    x == strip.x + ctx.sidebar_targets.counts.rule.column(strip.w)
        && hit_test(strip, ctx.sidebar_targets.counts, x, y) != Some(SidebarHit::Collapse)
}

/// Pick up the sidebar edge.
pub(super) fn begin_sidebar_resize(ctx: &mut DispatchCtx<'_>) {
    *ctx.drag = Some(DragGrab::SidebarEdge);
    tracing::debug!("sidebar drag: grabbed edge");
}

/// Pick up the window at `from` from `strip`. Refused before the layout
/// read completes: a reorder then would be overwritten by the stored layout.
pub(super) fn begin_window_drag(ctx: &mut DispatchCtx<'_>, from: usize, strip: WindowStrip) {
    if !ctx.layout_read_complete {
        return;
    }
    let Some(window) = ctx.workspace.groups.get(from).map(|w| w.id) else {
        return;
    };
    *ctx.drag = Some(DragGrab::Window(WindowGrab {
        window,
        strip,
        drop_at: Some(from),
    }));
    tracing::debug!(from, ?strip, "window drag: grabbed");
}

/// Resize the sidebar so its pane-facing edge sits at column `x`. Updates
/// the driver's width and this batch's reservation together, so later
/// events in the same batch hit-test against the new geometry.
fn resize_sidebar(ctx: &mut DispatchCtx<'_>, x: u16) -> bool {
    let Some(res) = ctx.sidebar else {
        return false;
    };
    let floors = WidthFloors {
        strip: MIN_DRAGGED_SIDEBAR_COLS.min(res.width),
        panes: ctx.chrome.min_pane_cols,
    };
    let Some(width) = dragged_sidebar_width(ctx.viewport.0, res.edge, x, floors) else {
        return false;
    };
    if width == res.width {
        return false;
    }
    *ctx.sidebar_width = width;
    ctx.sidebar = Some(SidebarReservation {
        edge: res.edge,
        width,
    });
    true
}

/// The narrowest a drag may leave the strip and the panes.
#[derive(Debug, Clone, Copy)]
pub(super) struct WidthFloors {
    /// Strip floor: [`MIN_DRAGGED_SIDEBAR_COLS`], or the current width when
    /// a config already set it narrower, so grabbing never snaps it wider.
    pub strip: u16,
    /// `[chrome] min_pane_cols`.
    pub panes: u16,
}

/// The sidebar width that puts its pane-facing edge at column `x`, clamped
/// so neither the strip nor the panes drop below their floors. `None` when
/// the terminal is too narrow to honor both floors at once.
pub(super) fn dragged_sidebar_width(
    cols: u16,
    edge: SidebarEdge,
    x: u16,
    floors: WidthFloors,
) -> Option<u16> {
    let max = cols.checked_sub(floors.panes)?;
    if max < floors.strip {
        return None;
    }
    let raw = match edge {
        SidebarEdge::Left => x.saturating_add(1),
        SidebarEdge::Right => cols.saturating_sub(x),
    };
    Some(raw.clamp(floors.strip, max))
}

/// Point the live insertion marker at the slot under the pointer on the
/// strip the window was picked up from. Off that strip the marker hides
/// (the drop would be a no-op). Returns whether the painted index changed.
fn preview_window_drop(ctx: &mut DispatchCtx<'_>, mouse: &MouseEvent) -> bool {
    let Some(strip) = ctx.drag.as_ref().and_then(|grab| match grab {
        DragGrab::Window(window) => Some(window.strip),
        _ => None,
    }) else {
        return false;
    };
    let drop_at = window_under(ctx, strip, mouse);
    let Some(DragGrab::Window(live)) = ctx.drag.as_mut() else {
        return false;
    };
    if live.drop_at == drop_at {
        return false;
    }
    live.drop_at = drop_at;
    true
}

/// Move the grabbed window to the slot under the release point on the
/// strip it came from. Returns whether the order changed.
fn drop_window(ctx: &mut DispatchCtx<'_>, grab: WindowGrab, mouse: &MouseEvent) -> bool {
    let target = window_under(ctx, grab.strip, mouse);
    let Some(to) = target else {
        return false;
    };
    // Re-resolve the grabbed window: it may have moved or closed mid-drag.
    let Some(from) = ctx
        .workspace
        .groups
        .iter()
        .position(|w| w.id == grab.window)
    else {
        return false;
    };
    let moved = ctx.workspace.move_group(from, to);
    if moved {
        tracing::debug!(from, to, "window drag: reordered");
    }
    moved
}

/// The window slot under the pointer on `strip`, or `None` off that strip.
fn window_under(ctx: &DispatchCtx<'_>, strip: WindowStrip, mouse: &MouseEvent) -> Option<usize> {
    let (x, y) = (quantize_cell(mouse.x), quantize_cell(mouse.y));
    match strip {
        WindowStrip::Tabs => tab_under(ctx, x, y),
        WindowStrip::Sidebar => sidebar_window_under(ctx, x, y),
    }
}

/// The window tab under `(x, y)`, when the point is on the status bar row.
fn tab_under(ctx: &DispatchCtx<'_>, x: u16, y: u16) -> Option<usize> {
    if bar_row(ctx)? != y {
        return None;
    }
    ctx.status_bar?.window_hit_at(x)
}

/// The sidebar window row under `(x, y)`.
fn sidebar_window_under(ctx: &DispatchCtx<'_>, x: u16, y: u16) -> Option<usize> {
    let strip = sidebar_rect(ctx.viewport, ctx.sidebar?);
    match hit_test(strip, ctx.sidebar_targets.counts, x, y)? {
        SidebarHit::Window(index) => Some(index),
        _ => None,
    }
}

/// The outer-viewport row the status bar occupies, or `None` without one.
pub(super) fn bar_row(ctx: &DispatchCtx<'_>) -> Option<u16> {
    use crate::render::chrome::status_bar::Position;
    if ctx.viewport.1 == 0 {
        return None;
    }
    Some(match ctx.bar? {
        Position::Bottom => ctx.viewport.1 - 1,
        Position::Top => 0,
    })
}
