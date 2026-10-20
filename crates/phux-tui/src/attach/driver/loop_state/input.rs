//! Stdin batches and the outer-terminal resize.

mod batch;
mod dispatch;
mod resize;
mod view;

use super::{HashMap, ResourceId, SidebarReservation};
pub(super) use view::{classify_repaint, same_rects};

/// The view geometry captured before a batch, so a later reflow can diff it.
struct PriorView {
    zoomed: Option<ResourceId>,
    sidebar: Option<SidebarReservation>,
    active_window: usize,
    rects: HashMap<ResourceId, crate::layout::Rect>,
    /// Whether the floating overlay (ADR-0147) was open: it covers panes
    /// without changing their tiles, so opening or closing it is invisible
    /// to the rect diff yet leaves its box on screen unless the view clears.
    floating: bool,
    /// Whether a modal overlay (palette, picker, prompt) was up. Like the
    /// floating box it scribbles over pane cells the rect diff cannot see.
    overlay: bool,
    /// Each drawn pane's history viewport offset, so a batch that scrolled
    /// one (a wheel notch, a copy-mode page) repaints its rows even though
    /// no tile moved.
    viewports: HashMap<ResourceId, u64>,
}
