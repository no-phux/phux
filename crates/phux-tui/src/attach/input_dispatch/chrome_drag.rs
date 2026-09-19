//! Pointer drags over client chrome: the sidebar edge and the two window
//! strips (the status bar's tabs and the sidebar's window rows).
//!
//! Pane-divider drags (ADR-0048) share the same grab slot
//! ([`DragGrab`]) and the same press / motion / release lifecycle; their
//! ratio math lives beside the divider hit-test in `dispatch.rs`.
//!
//! Everything here is synchronous and mutates only client-local state. The
//! one wire effect a drag can have, broadcasting the reordered layout on
//! release, is reported back to the async caller as
//! [`DragCommit::broadcast`]. A dragged sidebar width is runtime chrome,
//! like `toggle-sidebar`: it lasts for the attach, rides across session
//! switches, and is never written to `config.toml` (ADR-0101 decision 2).

use phux_protocol::input::mouse::MouseEvent;

use super::ctx::{DispatchCtx, DragGrab, WindowGrab, WindowStrip};
use super::dispatch::{drag_resize, quantize_cell};
use crate::attach::paint::{SidebarEdge, SidebarReservation, sidebar_rect};
use crate::render::chrome::sidebar::{SidebarHit, hit_test};

/// The narrowest a dragged sidebar may get. Narrower than this, a window
/// name no longer fits beside its status glyph.
pub(super) const MIN_DRAGGED_SIDEBAR_COLS: u16 = 16;

/// What releasing a grab asks the async caller to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct DragCommit {
    /// The release changed what is on screen.
    pub layout_changed: bool,
    /// The release changed shared layout that other clients must see.
    pub broadcast: bool,
}

/// Advance `grab` by one button-motion event. Returns whether the screen
/// changed. A window grab has nothing to show mid-drag: the reorder
/// happens where the pointer is released.
pub(super) fn motion(ctx: &mut DispatchCtx<'_>, grab: &DragGrab, mouse: &MouseEvent) -> bool {
    match grab {
        DragGrab::Divider(divider) => drag_resize(ctx, mouse, divider),
        DragGrab::SidebarEdge => resize_sidebar(ctx, quantize_cell(mouse.x)),
        DragGrab::Window(_) => false,
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
        },
        // Width is client-local presentation; motion already applied it.
        DragGrab::SidebarEdge => DragCommit::default(),
        DragGrab::Window(window) => {
            let moved = drop_window(ctx, *window, mouse);
            DragCommit {
                layout_changed: moved,
                broadcast: moved,
            }
        }
    }
}

/// Whether an outer-viewport cell is the sidebar's resize handle: the
/// painted separator rule, which sits between strip and panes only when the
/// strip docks left. A right-docked strip paints its rule on the screen
/// edge and has no pane-facing border to grab, so it has no handle. The
/// bottom corner of the rule stays the collapse chevron.
pub(super) fn on_sidebar_edge(ctx: &DispatchCtx<'_>, x: u16, y: u16) -> bool {
    let Some(res) = ctx.sidebar.filter(|res| res.edge == SidebarEdge::Left) else {
        return false;
    };
    let strip = sidebar_rect(ctx.viewport, res);
    if strip.w == 0 || y < strip.y || y >= strip.y.saturating_add(strip.h) {
        return false;
    }
    x == strip.x + strip.w - 1
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
    let Some(window) = ctx.workspace.windows.get(from).map(|w| w.id) else {
        return;
    };
    *ctx.drag = Some(DragGrab::Window(WindowGrab { window, strip }));
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

/// Move the grabbed window to the slot under the release point on the
/// strip it came from. Returns whether the order changed.
fn drop_window(ctx: &mut DispatchCtx<'_>, grab: WindowGrab, mouse: &MouseEvent) -> bool {
    let (x, y) = (quantize_cell(mouse.x), quantize_cell(mouse.y));
    let target = match grab.strip {
        WindowStrip::Tabs => tab_under(ctx, x, y),
        WindowStrip::Sidebar => sidebar_window_under(ctx, x, y),
    };
    let Some(to) = target else {
        return false;
    };
    // Re-resolve the grabbed window: it may have moved or closed mid-drag.
    let Some(from) = ctx
        .workspace
        .windows
        .iter()
        .position(|w| w.id == grab.window)
    else {
        return false;
    };
    let moved = ctx.workspace.move_window(from, to);
    if moved {
        tracing::debug!(from, to, "window drag: reordered");
    }
    moved
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
