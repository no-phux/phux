use phux_protocol::ResourceId;
use phux_protocol::input::mouse::MouseEvent;

use crate::layout::LayoutState;
use crate::layout::NodePath;
use crate::layout::Rect;
use crate::layout::SplitDir;

use super::layout::compute_layout_in;

/// Outcome of a click hit-test against the multi-pane composition.
#[derive(Debug, Clone, PartialEq)]
pub enum RouteDecision {
    /// The click hit a pane: forward it with pane-local coordinates and move
    /// focus iff `focus_changed`.
    Pane {
        /// The pane the mouse event addresses.
        target: ResourceId,
        /// Pane-local 0-indexed cell x (treated as f64 pixels per
        /// SPEC §9.2.1 — the cell-quantising client contract).
        pane_x: f64,
        /// Pane-local 0-indexed cell y.
        pane_y: f64,
        /// `true` iff this click moves focus.
        focus_changed: bool,
    },
    /// The click hit the divider of the split at `node_path`; `axis` picks
    /// the pointer coordinate that drives its ratio (ADR-0048).
    Divider {
        /// Path from the layout root to the controlling split.
        node_path: NodePath,
        /// The split's axis.
        axis: SplitDir,
    },
    /// The click fell outside every pane rect AND every divider cell
    /// (reserved chrome, degenerate viewport, undersized tree). The
    /// driver drops the event entirely.
    Miss,
    /// No tree to hit-test against (fresh `LayoutState::default()`),
    /// or focus is unset. Caller falls back to "no input goes anywhere
    /// until ATTACHED seeds focus".
    NoFocus,
}

/// Route a mouse event (outer-viewport cells) to a pane, a divider, or
/// nothing.
///
/// `content` must be the same inset rect the paint path tiles into, or
/// clicks route off by the status bar / sidebar. No tree gives
/// [`RouteDecision::NoFocus`]; reserved chrome gives
/// [`RouteDecision::Miss`].
#[must_use]
pub fn route_mouse_event(
    layout: &LayoutState,
    content: Rect,
    viewport: (u16, u16),
    mouse: &MouseEvent,
) -> RouteDecision {
    // No tree means no panes to address yet — the driver dropped the
    // event already by the time `dispatch_input_events` runs, but the
    // helper stays defensive for direct callers.
    if layout.tree.is_none() {
        return RouteDecision::NoFocus;
    }

    let multi = compute_layout_in(layout, content, viewport);
    if multi.rects.is_empty() {
        return RouteDecision::NoFocus;
    }

    // Rects tile exactly, so at most one matches; over-edge clicks clamp
    // into the last cell.
    let cell_x = clamp_cell(mouse.x).min(viewport.0.saturating_sub(1));
    let cell_y = clamp_cell(mouse.y).min(viewport.1.saturating_sub(1));

    let mut hit: Option<(ResourceId, Rect)> = None;
    for (id, rect) in &multi.rects {
        if rect_contains(*rect, cell_x, cell_y) {
            hit = Some((id.clone(), *rect));
            break;
        }
    }

    if let Some((target, rect)) = hit {
        let focus_changed = layout.focus.as_ref() != Some(&target);
        // Translate to pane-local. `rect.x <= cell_x` is guaranteed by
        // `rect_contains`, so the subtraction never underflows.
        #[allow(clippy::cast_lossless, reason = "u16 → f64 is exact for our range")]
        let pane_x = f64::from(cell_x - rect.x);
        #[allow(clippy::cast_lossless, reason = "u16 → f64 is exact for our range")]
        let pane_y = f64::from(cell_y - rect.y);
        return RouteDecision::Pane {
            target,
            pane_x,
            pane_y,
            focus_changed,
        };
    }

    // Otherwise a divider cell names its split; at a crossing either split
    // is a sane grab.
    for h in &multi.divider_hits {
        if h.cells.contains(&(cell_x, cell_y)) {
            return RouteDecision::Divider {
                node_path: h.node_path.clone(),
                axis: h.axis,
            };
        }
    }

    // Reserved chrome, divider gap with no split (degenerate), or outside
    // every rect entirely.
    RouteDecision::Miss
}

/// Clamp an f64 cell position to `u16`. Pixel-precision input that
/// exceeds the viewport falls into the edge cells rather than wrapping
/// or panicking. Negative input is clamped at 0.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "input is the result of cell-quantising the SGR/X10 mouse stream; saturate to keep malformed peers from breaking the routing path"
)]
fn clamp_cell(p: f64) -> u16 {
    if p.is_nan() || p < 0.0 {
        0
    } else if p >= f64::from(u16::MAX) {
        u16::MAX
    } else {
        p as u16
    }
}

/// Half-open rectangle membership test: `[x, x+w) × [y, y+h)`. Mirrors
/// the convention `compute_layout` uses when tiling the viewport.
const fn rect_contains(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && y >= r.y && x < r.x.saturating_add(r.w) && y < r.y.saturating_add(r.h)
}
