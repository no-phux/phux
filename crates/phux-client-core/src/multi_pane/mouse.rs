use phux_protocol::ResourceId;
use phux_protocol::input::mouse::MouseEvent;

use crate::layout::LayoutState;
use crate::layout::NodePath;
use crate::layout::Rect;
use crate::layout::SplitDir;

use super::layout::compute_layout_in;

// -----------------------------------------------------------------------------
// route_mouse_event — pure hit-test for INPUT_MOUSE routing (phux-4li.6)
// -----------------------------------------------------------------------------

/// Outcome of a click hit-test against the current multi-pane composition.
///
/// The driver consumes this to decide three independent things:
///
/// 1. Which `ResourceId` (if any) the resulting `INPUT_MOUSE` frame
///    targets — and what the pane-local coordinates are.
/// 2. Whether `LayoutState.focus` needs to swap to a different pane
///    (click-to-focus, per ADR-0019 decision 6 + DESIGN §7).
/// 3. Whether a divider repaint is required because focus changed
///    (heavy / light chrome moves with focus).
///
/// A click on a divider cell returns [`RouteDecision::Divider`] carrying
/// the controlling split's node path + axis, so the driver's drag machine
/// can adjust that split's ratio (ADR-0048). A click that falls outside
/// every pane rect *and* every divider cell (reserved chrome, degenerate
/// viewport) returns [`RouteDecision::Miss`] and is dropped.
#[derive(Debug, Clone, PartialEq)]
pub enum RouteDecision {
    /// The click hit a pane. The driver should:
    /// - swap `LayoutState.focus` to `target` iff `focus_changed`;
    /// - forward an `INPUT_MOUSE` whose payload's `(x, y)` are
    ///   replaced with `(pane_x, pane_y)` (pane-local cells);
    /// - repaint the multi-pane composition iff `focus_changed`
    ///   (so the heavy-edge chrome follows focus).
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
    /// The click hit a divider cell. The driver starts (or continues) a
    /// drag against the addressed split: `node_path` names the
    /// [`crate::layout::LayoutNode::Split`] whose `ratio` the drag tunes,
    /// and `axis` says whether the pointer's x (`Horizontal`) or y
    /// (`Vertical`) drives the ratio (ADR-0048).
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

/// Pure routing decision for an `INPUT_MOUSE` event.
///
/// Hit-tests `mouse.(x, y)` (interpreted as outer-viewport cell
/// coordinates per the parser in `attach::input` — the inbound SGR /
/// X10 / urxvt-1015 dispatchers all emit `f64` pixels with "1 cell ==
/// 1 pixel" because the client does not know cell-size at parse time)
/// against the pane `Rect`s yielded by [`compute_layout_in`].
///
/// `content` is the inset rectangle the renderer tiles panes into after
/// folding off the status bar row and any sidebar columns — it MUST be the
/// same `content_rect(viewport, has_bar, sidebar)` the paint path uses, or
/// the hit-test disagrees with what is on screen and clicks route to the
/// wrong pane (off by the status-bar row and/or the sidebar width). Clicks
/// that fall in the reserved chrome (status bar row, sidebar columns) miss
/// every inset rect and resolve to [`RouteDecision::Miss`].
///
/// Behavior matrix:
///
/// * No tree / no focus ⇒ [`RouteDecision::NoFocus`]. Caller drops.
/// * Click inside a pane's `Rect` ⇒ [`RouteDecision::Pane`] with
///   pane-local coords; `focus_changed = true` iff target ≠
///   `layout.focus`.
/// * Click on a divider cell ⇒ [`RouteDecision::Divider`] carrying the
///   split that cell controls (path + axis) for drag-to-resize.
/// * Click outside any pane and any divider (reserved chrome, degenerate
///   viewport, undersized tree) ⇒ [`RouteDecision::Miss`].
///
/// The function is pure (no allocation aside from the internal
/// [`compute_layout_in`] call's `HashMap`) and synchronous; the driver's
/// async input loop calls it once per `InputEvent::Mouse`.
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

    // Tile into the same inset content rect the renderer paints, so the
    // hit-test rects match the on-screen pane rects cell-for-cell. Single
    // pane: exactly one rect covering `content`, and every in-content click
    // lands in it.
    let multi = compute_layout_in(layout, content, viewport);
    if multi.rects.is_empty() {
        return RouteDecision::NoFocus;
    }

    // Find the pane whose Rect contains the (cell) click. Iteration
    // order is arbitrary but rects tile the viewport exactly, so at
    // most one Rect matches. f64 → u16 via floor: the parser emits
    // integer-valued f64s for cell-quantised input, so a `as` cast is
    // exact for inputs in `0..=u16::MAX`. The trailing `.min(...)`
    // clamps a stray over-edge click into the last cell of the
    // viewport so a hi-DPI host's pixel report doesn't escape the
    // pane tiling.
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

    // Not in a pane — is the cell a divider? The grab map's cells are
    // exactly the painted divider cells (built from the same segments,
    // same viewport clamp), so set-membership resolves the controlling
    // split. Inner splits paint over outer ones at a crossing; the first
    // hit whose cell set contains the click wins — for a true crossing
    // cell that ambiguity is inherent and either split is a sane grab.
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
