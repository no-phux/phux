//! Pure layout-action helpers for the multi-pane TUI dispatcher.
//!
//! Per ADR-0019 the client interprets keybind `ResolvedAction`s into
//! [`LayoutState`] mutations; the driver adds the wire side effects
//! (`SPAWN_RESOURCE`, `SET_METADATA`). These helpers never touch the
//! connection, the clock, or the screen. Focus in the broadcast envelope is
//! non-authoritative (ADR-0049).

use std::io::{self, Write};

use phux_client::conditional_kill::BoundResource;
use phux_protocol::ResourceId;
use phux_protocol::ids::ServerInstance;
use thiserror::Error;

use super::paint::{SidebarReservation, content_rect};
use crate::layout::{
    self, Direction, LayoutError, LayoutNode, LayoutState, NodePath, Rect, SplitDir,
};
use crate::multi_pane::{pane_rects_proportional_in, split_content_span_at};

/// Errors from the pure action helpers; the driver logs and bells.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum ActionError {
    /// There is no focused pane to act on.
    #[error("no focused pane")]
    NoFocus,
    /// The layout tree is empty.
    #[error("layout tree is empty")]
    EmptyTree,
    /// The tree operation failed.
    #[error("layout error: {0}")]
    Layout(#[from] LayoutError),
    /// No split along the resize axis encloses the focused pane.
    #[error("no resizable boundary in direction")]
    NoResizableBoundary,
}

/// Split the focused leaf along `dir` at 50/50, focusing `new_pane`.
///
/// # Errors
/// [`ActionError::NoFocus`] / [`ActionError::EmptyTree`] on an empty state;
/// [`ActionError::Layout`] from [`layout::split_at`].
pub fn apply_split(
    state: &LayoutState,
    new_pane: ResourceId,
    dir: SplitDir,
) -> Result<LayoutState, ActionError> {
    let tree = state.tree.as_ref().ok_or(ActionError::EmptyTree)?;
    let focused = state.focus.as_ref().ok_or(ActionError::NoFocus)?;
    let new_tree = layout::split_at(tree, focused, &new_pane, dir, 0.5)?;
    Ok(LayoutState {
        tree: Some(new_tree),
        focus: Some(new_pane),
    })
}

/// Remove the focused leaf. The sibling is promoted and focus moves to the
/// first DFS leaf (ADR-0019); killing the last leaf empties the state.
///
/// # Errors
/// [`ActionError::NoFocus`] / [`ActionError::EmptyTree`] on an empty state;
/// [`ActionError::Layout`] when the focus is not a leaf.
pub fn apply_kill(state: &LayoutState) -> Result<LayoutState, ActionError> {
    let tree = state.tree.as_ref().ok_or(ActionError::EmptyTree)?;
    let focused = state.focus.as_ref().ok_or(ActionError::NoFocus)?;
    let new_tree = layout::kill_pane(tree, focused)?;
    Ok(new_tree.map_or(
        LayoutState {
            tree: None,
            focus: None,
        },
        |tree| {
            let focus = layout::leaves(&tree).into_iter().next();
            LayoutState {
                tree: Some(tree),
                focus,
            }
        },
    ))
}

/// Move focus to the neighbour in `dir`; `None` at the layout edge (no bell,
/// matching tmux).
#[must_use]
pub fn apply_focus(state: &LayoutState, dir: Direction) -> Option<LayoutState> {
    let tree = state.tree.as_ref()?;
    let current = state.focus.as_ref()?;
    let next = layout::focus_direction(tree, current, dir)?;
    Some(LayoutState {
        tree: state.tree.clone(),
        focus: Some(next),
    })
}

/// Minimum leaf size along the active axis, in cells (ADR-0019 decision 5).
const MIN_PANE_CELL: u16 = 2;

/// Resize the focused pane: move the boundary of the enclosing split along
/// the direction's axis `amount / axis_cells` toward `dir`, so a positive
/// `amount` grows the focused pane toward `dir` when that side has a
/// neighbor, and shrinks it from the far side otherwise (tmux's
/// `resize-pane`). `Ok(None)` when a child would drop below
/// [`MIN_PANE_CELL`] (the driver bells).
///
/// # Errors
/// [`ActionError::NoFocus`] / [`ActionError::EmptyTree`] on an empty state;
/// [`ActionError::NoResizableBoundary`] when no split along the axis
/// encloses the focused leaf.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
pub(super) fn apply_resize(
    state: &LayoutState,
    dir: Direction,
    amount: i16,
    viewport: (u16, u16),
    sidebar: Option<SidebarReservation>,
) -> Result<Option<LayoutState>, ActionError> {
    let tree = state.tree.as_ref().ok_or(ActionError::EmptyTree)?;
    let focused = state.focus.as_ref().ok_or(ActionError::NoFocus)?;
    if amount == 0 {
        return Ok(Some(state.clone()));
    }
    let target_axis = match dir {
        Direction::Left | Direction::Right => SplitDir::Horizontal,
        Direction::Up | Direction::Down => SplitDir::Vertical,
    };
    let total_cells = match target_axis {
        SplitDir::Horizontal => viewport.0,
        SplitDir::Vertical => viewport.1,
    };
    if total_cells == 0 {
        return Err(ActionError::NoResizableBoundary);
    }
    let resize = AxisResize {
        focused,
        axis: target_axis,
        dir,
        cells: f32::from(amount),
    };
    let (new_tree, applied) = resize.apply(tree, f32::from(total_cells));
    if !applied {
        return Err(ActionError::NoResizableBoundary);
    }
    let candidate = LayoutState {
        tree: Some(new_tree),
        focus: Some(focused.clone()),
    };
    if violates_min_cell(&candidate, viewport, sidebar) {
        return Ok(None);
    }
    Ok(Some(candidate))
}

/// One `resize-pane`: move the divider of the split on `axis` nearest to
/// `focused` by `cells` toward `dir` (tmux's `resize-pane -L/-R/-U/-D`).
struct AxisResize<'a> {
    focused: &'a ResourceId,
    axis: SplitDir,
    dir: Direction,
    cells: f32,
}

impl AxisResize<'_> {
    /// Rebuild `node` (spanning `extent` cells on the axis) with the nearest
    /// enclosing split on the axis moved, returning `(new_tree, applied)`.
    fn apply(&self, node: &LayoutNode, extent: f32) -> (LayoutNode, bool) {
        let LayoutNode::Split {
            dir: sd,
            ratio,
            left,
            right,
        } = node
        else {
            return (node.clone(), false);
        };
        let on_axis = *sd == self.axis;
        // A child's extent on the axis: its share when this split divides
        // the axis, the whole extent when it stacks across it.
        let (left_extent, right_extent) = if on_axis {
            (extent * *ratio, extent * (1.0 - *ratio))
        } else {
            (extent, extent)
        };
        // A deeper split on the axis is the pane's own border; try it first.
        let (new_left, new_right, applied) = if tree_contains(left, self.focused) {
            let (new_left, applied) = self.apply(left, left_extent);
            (Box::new(new_left), right.clone(), applied)
        } else if tree_contains(right, self.focused) {
            let (new_right, applied) = self.apply(right, right_extent);
            (left.clone(), Box::new(new_right), applied)
        } else {
            return (node.clone(), false);
        };
        let rebuilt = move |ratio| LayoutNode::Split {
            dir: *sd,
            ratio,
            left: new_left,
            right: new_right,
        };
        if applied || !on_axis || extent <= 0.0 {
            return (rebuilt(*ratio), applied);
        }
        (
            rebuilt(clamp_ratio(*ratio + self.signed_delta(extent))),
            true,
        )
    }

    /// The ratio step for `cells` of a split spanning `extent` cells. The
    /// boundary moves toward `dir` whichever side holds focus: a right or
    /// lower pane grows toward `dir`, a left or upper one gives way. `ratio`
    /// is the left (upper) child's share.
    fn signed_delta(&self, extent: f32) -> f32 {
        let delta = self.cells / extent;
        match self.dir {
            Direction::Right | Direction::Down => delta,
            Direction::Left | Direction::Up => -delta,
        }
    }
}

fn tree_contains(node: &LayoutNode, target: &ResourceId) -> bool {
    match node {
        LayoutNode::Leaf(p) => p == target,
        LayoutNode::Split { left, right, .. } => {
            tree_contains(left, target) || tree_contains(right, target)
        }
    }
}

/// Clamp a ratio strictly inside `(0, 1)`; `split_at` rejects the bounds.
fn clamp_ratio(r: f32) -> f32 {
    const EPS: f32 = 0.001;
    r.clamp(EPS, 1.0 - EPS)
}

/// Whether any leaf falls below [`MIN_PANE_CELL`] in the sidebar-inset
/// content rect. Uses the proportional tiling, not the frozen paint tiling,
/// whose reflow floor would pin rects at minimum and never trip this gate.
fn violates_min_cell(
    state: &LayoutState,
    viewport: (u16, u16),
    sidebar: Option<SidebarReservation>,
) -> bool {
    let Some(tree) = state.tree.as_ref() else {
        return false;
    };
    let rects = pane_rects_proportional_in(tree, content_rect(viewport, None, sidebar));
    rects
        .values()
        .any(|r: &Rect| r.w < MIN_PANE_CELL || r.h < MIN_PANE_CELL)
}

/// ADR-0048 drag: set the split at `node_path` so its divider sits under the
/// absolute `pointer` (x for a `Horizontal` split, y for `Vertical`). The
/// content rect must match the hit test's, so `bar` and `sidebar` inset it.
/// `Ok(None)` at the [`MIN_PANE_CELL`] floor, so a drag stalls instead of
/// collapsing a pane.
///
/// # Errors
/// [`ActionError::EmptyTree`] on an empty state;
/// [`ActionError::NoResizableBoundary`] when `node_path` no longer names a
/// split (a stale grab) or its budget is zero.
#[allow(clippy::cast_precision_loss)]
pub(super) fn apply_divider_resize(
    state: &LayoutState,
    node_path: &NodePath,
    axis: SplitDir,
    pointer: (u16, u16),
    viewport: (u16, u16),
    bar: Option<crate::render::chrome::status_bar::Position>,
    sidebar: Option<SidebarReservation>,
) -> Result<Option<LayoutState>, ActionError> {
    let tree = state.tree.as_ref().ok_or(ActionError::EmptyTree)?;
    let content = content_rect(viewport, bar, sidebar);
    let (start, content_len) =
        split_content_span_at(tree, content, node_path).ok_or(ActionError::NoResizableBoundary)?;
    let p = match axis {
        SplitDir::Horizontal => pointer.0,
        SplitDir::Vertical => pointer.1,
    };
    let low = p.saturating_sub(start).min(content_len);
    let ratio = clamp_ratio(f32::from(low) / f32::from(content_len));
    let new_tree =
        layout::set_ratio_at(tree, node_path, ratio).ok_or(ActionError::NoResizableBoundary)?;
    let candidate = LayoutState {
        tree: Some(new_tree),
        focus: state.focus.clone(),
    };
    if violates_min_cell(&candidate, viewport, sidebar) {
        return Ok(None);
    }
    Ok(Some(candidate))
}

/// Focus the next leaf in DFS order, wrapping; `None` with fewer than two.
#[must_use]
pub fn apply_next_pane(state: &LayoutState) -> Option<LayoutState> {
    cycle(state, 1)
}

/// Focus the previous leaf in DFS order, wrapping; `None` with fewer than two.
#[must_use]
pub fn apply_previous_pane(state: &LayoutState) -> Option<LayoutState> {
    cycle(state, -1)
}

fn cycle(state: &LayoutState, step: i32) -> Option<LayoutState> {
    let tree = state.tree.as_ref()?;
    let current = state.focus.as_ref()?;
    let leaves = layout::leaves(tree);
    if leaves.len() < 2 {
        return None;
    }
    let idx = leaves.iter().position(|p| p == current)?;
    let len = i32::try_from(leaves.len()).ok()?;
    let next_idx = ((i32::try_from(idx).ok()? + step).rem_euclid(len)) as usize;
    let next = leaves.get(next_idx)?.clone();
    Some(LayoutState {
        tree: state.tree.clone(),
        focus: Some(next),
    })
}

/// Write a BEL and flush.
///
/// # Errors
/// Forwards any `io::Error` from `out`.
pub fn write_bell<W: Write>(out: &mut W) -> io::Result<()> {
    out.write_all(b"\x07")?;
    out.flush()
}

/// An in-flight `split-pane`, parked by request id until its
/// `RESOURCE_SPAWNED` reply.
#[derive(Debug, Clone)]
pub(super) struct PendingSplit {
    /// The leaf the chord targeted; the split anchors here even if focus
    /// moved meanwhile.
    pub focused_at_request: ResourceId,
    /// Axis along which to split.
    pub dir: SplitDir,
    /// Zoom the new pane instead of un-zooming (`placement = "zoomed"`).
    pub zoom_on_spawn: bool,
    /// The host the new pane was asked of.
    pub host: SplitHost,
    /// Once a satellite spawn answered: the pane whose `ATTACH_RESOURCE`
    /// decides the split.
    pub adopt: Option<SpawnedPane>,
    /// An existing pane (`host/@N` or `@N`) to attach and place; a refusal
    /// leaves it alone since this client did not spawn it.
    pub open_existing: Option<ResourceId>,
}

/// A pane this client spawned on a satellite, with the instance token the
/// reply bound it to (ADR-0109); `None` means it can never be killed
/// conditionally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SpawnedPane {
    /// The pane, as the spawn reply named it (satellite-tagged).
    pub id: ResourceId,
    /// The satellite's instance token the spawn was bound to, if any.
    pub instance: Option<ServerInstance>,
}

impl SpawnedPane {
    /// A pane whose spawn reply carried no instance token.
    #[cfg(test)]
    pub(super) const fn unbound(id: ResourceId) -> Self {
        Self { id, instance: None }
    }

    /// The pane as a conditional kill names it, when its spawn was bound.
    pub(super) fn bound(&self) -> Option<BoundResource> {
        let instance = self.instance?;
        Some(BoundResource {
            id: self.id.clone(),
            instance,
        })
    }
}

/// Which host a parked split's pane is spawned on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) enum SplitHost {
    /// The attached server's own host: a split of a local pane.
    #[default]
    Attached,
    /// A satellite, through the attached hub: a split of a pane there.
    Satellite(phux_protocol::ids::SatelliteHost),
    /// This host in place of the satellite: the hub lacks host-aware spawns.
    AttachedInsteadOf(phux_protocol::ids::SatelliteHost),
}

/// A window or split parked on a satellite pane's `ATTACH_RESOURCE`.
#[derive(Debug, Clone)]
pub(super) enum ParkedAdopt {
    /// A window adopting its pane ([`PendingWindow::adopt`]).
    Window(PendingWindow),
    /// A split adopting its pane ([`PendingSplit::adopt`]).
    Split(PendingSplit),
}

impl ParkedAdopt {
    /// The satellite pane whose attach decides this window or split.
    pub(super) fn pane(&self) -> Option<&ResourceId> {
        match self {
            Self::Window(window) => window.adopt.as_ref().map(Adopt::pane),
            Self::Split(split) => split
                .adopt
                .as_ref()
                .map(|spawned| &spawned.id)
                .or(split.open_existing.as_ref()),
        }
    }

    /// The pane this client spawned for it, if it did.
    pub(super) const fn spawned_pane(&self) -> Option<&SpawnedPane> {
        match self {
            Self::Window(window) => window.spawned_pane(),
            Self::Split(split) => split.adopt.as_ref(),
        }
    }
}

impl PendingWindow {
    /// The satellite pane this client spawned for this window, once the
    /// spawn answered.
    pub(super) const fn spawned_pane(&self) -> Option<&SpawnedPane> {
        match &self.adopt {
            Some(Adopt::Spawned(pane)) => Some(pane),
            _ => None,
        }
    }
}

/// An in-flight `new-window`, parked by request id; its reply opens a window
/// named `name` on the spawned pane.
#[derive(Debug, Clone)]
pub(super) struct PendingWindow {
    /// Name for the window the spawned pane will seed.
    pub name: String,
    /// `Some` when the window waits on a satellite pane's `ATTACH_RESOURCE`
    /// reply instead of a spawn; it opens only if that attach succeeds.
    pub adopt: Option<Adopt>,
}

/// The satellite pane a parked window attaches before it opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Adopt {
    /// A satellite session's existing pane; a refused attach leaves it alone.
    Existing(ResourceId),
    /// A pane this client just spawned; a refused attach kills it.
    Spawned(SpawnedPane),
}

impl Adopt {
    /// The pane, whichever way it came.
    pub(super) const fn pane(&self) -> &ResourceId {
        match self {
            Self::Existing(pane) | Self::Spawned(SpawnedPane { id: pane, .. }) => pane,
        }
    }
}

/// Apply a parked split's `RESOURCE_SPAWNED { Ok }`, anchored at
/// `focused_at_request` when it is still a leaf, else at the live focus.
///
/// # Errors
/// [`ActionError::NoFocus`] when neither anchor exists; otherwise from
/// [`apply_split`].
pub(super) fn apply_spawned_ok(
    state: &LayoutState,
    new_id: ResourceId,
    pending: &PendingSplit,
) -> Result<LayoutState, ActionError> {
    let leaves = state
        .tree
        .as_ref()
        .map(crate::layout::leaves)
        .unwrap_or_default();
    let anchor = if leaves.contains(&pending.focused_at_request) {
        pending.focused_at_request.clone()
    } else {
        state.focus.clone().ok_or(ActionError::NoFocus)?
    };
    let anchored = LayoutState {
        tree: state.tree.clone(),
        focus: Some(anchor),
    };
    apply_split(&anchored, new_id, pending.dir)
}

/// A local id that is not a leaf of `state`, standing in for the id the
/// server has not allocated yet (geometry depends only on tree shape).
fn unused_leaf_id(state: &LayoutState) -> ResourceId {
    let leaves = state
        .tree
        .as_ref()
        .map(crate::layout::leaves)
        .unwrap_or_default();
    let mut candidate = u32::MAX;
    loop {
        let id = ResourceId::local(candidate);
        if !leaves.contains(&id) {
            return id;
        }
        candidate = candidate.wrapping_sub(1);
    }
}

/// The `(cols, rows)` the pane a parked split waits on will get: the
/// post-reply reflow's computation run one round trip early, sent as
/// `SPAWN_RESOURCE.initial_size` so the server builds the pane at its real
/// size. `None` when unpredictable (the caller omits the field).
pub(super) fn predicted_spawn_dims(
    state: &LayoutState,
    pending: &PendingSplit,
    content: Rect,
) -> Option<(u16, u16)> {
    // A zoomed spawn renders as a lone full-content leaf.
    if pending.zoom_on_spawn {
        return Some((content.w, content.h));
    }
    let placeholder = unused_leaf_id(state);
    let next = apply_spawned_ok(state, placeholder.clone(), pending).ok()?;
    let rects = crate::multi_pane::pane_rects_in(next.tree.as_ref()?, content);
    rects.get(&placeholder).map(|rect| (rect.w, rect.h))
}

/// Fold a closed Terminal out of `state` via [`apply_kill`] (first-DFS-leaf
/// focus).
///
/// # Errors
/// [`ActionError::Layout`] when `dying` is not a leaf (the caller drops the
/// slot either way).
pub(super) fn apply_terminal_closed(
    state: &LayoutState,
    dying: &ResourceId,
) -> Result<LayoutState, ActionError> {
    let anchored = LayoutState {
        tree: state.tree.clone(),
        focus: Some(dying.clone()),
    };
    apply_kill(&anchored)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::*;
    use crate::layout::split_at;

    fn t(id: u32) -> ResourceId {
        ResourceId::local(id)
    }

    fn two_pane(dir: SplitDir) -> LayoutState {
        let tree = split_at(&LayoutNode::Leaf(t(1)), &t(1), &t(2), dir, 0.5).unwrap();
        LayoutState {
            tree: Some(tree),
            focus: Some(t(1)),
        }
    }

    /// ((1 | 2) / 3), focus on 2.
    fn three_pane_mixed() -> LayoutState {
        let t1 = two_pane(SplitDir::Horizontal).tree.unwrap();
        let t2 = split_at(&t1, &t(2), &t(3), SplitDir::Vertical, 0.5).unwrap();
        LayoutState {
            tree: Some(t2),
            focus: Some(t(2)),
        }
    }

    fn leaves(state: &LayoutState) -> Vec<ResourceId> {
        layout::leaves(state.tree.as_ref().expect("tree"))
    }

    fn ratio(state: &LayoutState) -> f32 {
        let LayoutNode::Split { ratio, .. } = state.tree.as_ref().unwrap() else {
            panic!("expected split");
        };
        *ratio
    }

    fn split_of(focused: ResourceId, dir: SplitDir, zoom_on_spawn: bool) -> PendingSplit {
        PendingSplit {
            focused_at_request: focused,
            dir,
            zoom_on_spawn,
            host: SplitHost::Attached,
            adopt: None,
            open_existing: None,
        }
    }

    #[test]
    fn split_kill_and_focus_edges() {
        let out = apply_split(&LayoutState::single(t(1)), t(2), SplitDir::Horizontal).unwrap();
        assert_eq!(out.focus, Some(t(2)));
        assert_eq!(leaves(&out), vec![t(1), t(2)]);
        assert!(matches!(
            apply_split(&LayoutState::default(), t(2), SplitDir::Horizontal),
            Err(ActionError::EmptyTree)
        ));
        let unfocused = LayoutState {
            tree: Some(LayoutNode::Leaf(t(1))),
            focus: None,
        };
        assert!(matches!(
            apply_split(&unfocused, t(2), SplitDir::Horizontal),
            Err(ActionError::NoFocus)
        ));

        let out = apply_kill(&LayoutState::single(t(1))).unwrap();
        assert!(out.tree.is_none() && out.focus.is_none());
        let out = apply_kill(&two_pane(SplitDir::Horizontal)).unwrap();
        assert!(matches!(out.tree.as_ref().unwrap(), LayoutNode::Leaf(p) if *p == t(2)));
        assert_eq!(out.focus, Some(t(2)));
        let out = apply_kill(&three_pane_mixed()).unwrap();
        assert_eq!(out.focus, Some(t(1)), "first DFS leaf after the collapse");
        assert_eq!(leaves(&out), vec![t(1), t(3)]);

        let state = two_pane(SplitDir::Horizontal);
        assert_eq!(
            apply_focus(&state, Direction::Right).unwrap().focus,
            Some(t(2))
        );
        assert!(apply_focus(&state, Direction::Up).is_none());
        assert!(apply_focus(&state, Direction::Left).is_none());
        assert!(apply_focus(&LayoutState::default(), Direction::Right).is_none());
    }

    #[test]
    fn resize_moves_the_enclosing_split_and_respects_the_floor() {
        let state = two_pane(SplitDir::Horizontal);
        // 8 cells of 80 is a 0.1 ratio step.
        let grown = apply_resize(&state, Direction::Right, 8, (80, 24), None)
            .unwrap()
            .unwrap();
        assert!((ratio(&grown) - 0.6).abs() < 1e-4);
        let shrunk = apply_resize(&state, Direction::Left, 8, (80, 24), None)
            .unwrap()
            .unwrap();
        assert!((ratio(&shrunk) - 0.4).abs() < 1e-4);
        assert_eq!(
            apply_resize(&state, Direction::Right, 0, (80, 24), None).unwrap(),
            Some(state.clone())
        );
        assert!(
            apply_resize(&state, Direction::Left, 80, (80, 24), None)
                .unwrap()
                .is_none(),
            "below the 2-cell floor is a bell-no-op"
        );
        for (state, dir) in [
            (LayoutState::single(t(1)), Direction::Right),
            (two_pane(SplitDir::Horizontal), Direction::Up),
        ] {
            assert!(matches!(
                apply_resize(&state, dir, 5, (80, 24), None),
                Err(ActionError::NoResizableBoundary)
            ));
        }
    }

    /// With the right pane focused, `C-a H` ("grow the focused pane to the
    /// left") shrank it: the boundary moved right. The boundary follows the
    /// key's direction whichever side holds focus.
    #[test]
    fn resize_moves_the_boundary_toward_the_key_from_either_side() {
        let mut right_focused = two_pane(SplitDir::Horizontal);
        right_focused.focus = Some(t(2));
        let left = apply_resize(&right_focused, Direction::Left, 8, (80, 24), None)
            .unwrap()
            .unwrap();
        assert!((ratio(&left) - 0.4).abs() < 1e-4, "{}", ratio(&left));
        let right = apply_resize(&right_focused, Direction::Right, 8, (80, 24), None)
            .unwrap()
            .unwrap();
        assert!((ratio(&right) - 0.6).abs() < 1e-4, "{}", ratio(&right));

        let mut lower_focused = two_pane(SplitDir::Vertical);
        lower_focused.focus = Some(t(2));
        let up = apply_resize(&lower_focused, Direction::Up, 6, (80, 24), None)
            .unwrap()
            .unwrap();
        assert!(ratio(&up) < 0.5, "{}", ratio(&up));
    }

    /// In `(1 | (2 | 3))` the `2|3` divider is pane 3's own border, but
    /// `resize-pane` moved the outer `1|23` one. As tmux does, the nearest
    /// enclosing split on the axis moves, by cells of that split's extent.
    #[test]
    fn resize_moves_the_nearest_split_on_the_axis() {
        let outer = two_pane(SplitDir::Horizontal).tree.unwrap();
        let nested = split_at(&outer, &t(2), &t(3), SplitDir::Horizontal, 0.5).unwrap();
        let inner_ratio = |state: &LayoutState| {
            let Some(LayoutNode::Split { right, .. }) = state.tree.as_ref() else {
                panic!("expected split");
            };
            let LayoutNode::Split { ratio, .. } = right.as_ref() else {
                panic!("expected nested split");
            };
            *ratio
        };
        for focus in [t(2), t(3)] {
            let state = LayoutState {
                tree: Some(nested.clone()),
                focus: Some(focus.clone()),
            };
            // 4 cells of the inner split's ~40-cell extent is ~0.1.
            let moved = apply_resize(&state, Direction::Left, 4, (80, 24), None)
                .unwrap()
                .unwrap();
            assert!((ratio(&moved) - 0.5).abs() < 1e-4, "{focus:?}: outer moved");
            assert!(
                (inner_ratio(&moved) - 0.4).abs() < 0.01,
                "{focus:?}: inner at {}",
                inner_ratio(&moved)
            );
        }
        // Pane 1's nearest horizontal split is the outer one.
        let state = LayoutState {
            tree: Some(nested),
            focus: Some(t(1)),
        };
        let moved = apply_resize(&state, Direction::Right, 8, (80, 24), None)
            .unwrap()
            .unwrap();
        assert!((ratio(&moved) - 0.6).abs() < 1e-4);
        assert!((inner_ratio(&moved) - 0.5).abs() < 1e-4);
    }

    /// A vertical resize from inside a horizontal split still finds the
    /// enclosing vertical split, however deep the pane sits.
    #[test]
    fn resize_skips_splits_on_the_other_axis() {
        let state = three_pane_mixed();
        let moved = apply_resize(&state, Direction::Down, 3, (80, 24), None)
            .unwrap()
            .unwrap();
        let LayoutNode::Split { dir, ratio, .. } = moved.tree.as_ref().unwrap() else {
            panic!("expected split");
        };
        assert_eq!(*dir, SplitDir::Horizontal);
        assert!(
            (*ratio - 0.5).abs() < 1e-4,
            "the horizontal root is untouched"
        );
    }

    #[test]
    fn divider_resize_tracks_the_absolute_pointer() {
        let drag = |state: &LayoutState, axis, pointer, bar| {
            apply_divider_resize(state, &NodePath::root(), axis, pointer, (80, 24), bar, None)
        };
        let h = two_pane(SplitDir::Horizontal);
        // The root split's budget is 80 - 1 = 79 cells.
        let at_32 = drag(&h, SplitDir::Horizontal, (32, 10), None)
            .unwrap()
            .unwrap();
        assert!((ratio(&at_32) - 32.0 / 79.0).abs() < 1e-3);
        assert!(
            ratio(
                &drag(&h, SplitDir::Horizontal, (60, 5), None)
                    .unwrap()
                    .unwrap()
            ) > 0.5
        );
        assert!(
            ratio(
                &drag(&h, SplitDir::Horizontal, (20, 5), None)
                    .unwrap()
                    .unwrap()
            ) < 0.5
        );
        assert!(
            drag(&h, SplitDir::Horizontal, (0, 5), None)
                .unwrap()
                .is_none(),
            "floor"
        );
        assert!(matches!(
            drag(
                &LayoutState::single(t(1)),
                SplitDir::Horizontal,
                (40, 5),
                None
            ),
            Err(ActionError::NoResizableBoundary)
        ));

        // A docked bar shortens a Vertical split's budget (21 rows vs 22 after
        // the rail), so the same pointer row maps to a larger ratio: the drag
        // tracks the painted divider.
        let v = two_pane(SplitDir::Vertical);
        let bar = Some(crate::render::chrome::status_bar::Position::Bottom);
        let with_bar = ratio(&drag(&v, SplitDir::Vertical, (5, 11), bar).unwrap().unwrap());
        let no_bar = ratio(
            &drag(&v, SplitDir::Vertical, (5, 11), None)
                .unwrap()
                .unwrap(),
        );
        assert!((with_bar - 10.0 / 21.0).abs() < 1e-3, "bar: {with_bar}");
        assert!((no_bar - 10.0 / 22.0).abs() < 1e-3, "no bar: {no_bar}");
    }

    #[test]
    fn pane_cycling_wraps_in_dfs_order() {
        let mut state = three_pane_mixed();
        state.focus = Some(t(1));
        let mut forward = Vec::new();
        let mut s = state.clone();
        for _ in 0..3 {
            s = apply_next_pane(&s).unwrap();
            forward.push(s.focus.clone().unwrap());
        }
        assert_eq!(forward, vec![t(2), t(3), t(1)]);
        let back = apply_previous_pane(&state).unwrap();
        assert_eq!(back.focus, Some(t(3)));
        assert!(apply_next_pane(&LayoutState::single(t(1))).is_none());
        assert!(apply_previous_pane(&LayoutState::single(t(1))).is_none());
    }

    /// What the client predicts at SPAWN time must equal its own post-reply
    /// reflow, or the server builds the pane at one size and the reflow
    /// immediately resizes it (bootstrap-then-tombstone waste).
    #[test]
    fn predicted_spawn_dims_match_the_post_reply_reflow() {
        let content = Rect {
            x: 3,
            y: 1,
            w: 117,
            h: 39,
        };
        for state in [
            LayoutState::single(t(1)),
            two_pane(SplitDir::Horizontal),
            two_pane(SplitDir::Vertical),
            three_pane_mixed(),
        ] {
            for dir in [SplitDir::Horizontal, SplitDir::Vertical] {
                let pending = split_of(state.focus.clone().unwrap(), dir, false);
                let predicted = predicted_spawn_dims(&state, &pending, content).unwrap();
                let landed = apply_spawned_ok(&state, t(77), &pending).unwrap();
                let actual =
                    crate::multi_pane::pane_rects_in(landed.tree.as_ref().unwrap(), content)
                        [&t(77)];
                assert_eq!(
                    predicted,
                    (actual.w, actual.h),
                    "{dir:?} on {:?}",
                    leaves(&state)
                );
            }
        }
        // A zoomed spawn is the whole content rect; no tree, no prediction.
        let zoomed = split_of(t(2), SplitDir::Horizontal, true);
        assert_eq!(
            predicted_spawn_dims(&three_pane_mixed(), &zoomed, content),
            Some((117, 39))
        );
        let plain = split_of(t(1), SplitDir::Horizontal, false);
        assert_eq!(
            predicted_spawn_dims(&LayoutState::default(), &plain, content),
            None
        );
    }

    #[test]
    fn unused_leaf_id_avoids_live_leaves() {
        let tree = split_at(
            &LayoutNode::Leaf(t(u32::MAX)),
            &t(u32::MAX),
            &t(u32::MAX - 1),
            SplitDir::Horizontal,
            0.5,
        )
        .unwrap();
        let state = LayoutState {
            tree: Some(tree),
            focus: Some(t(u32::MAX)),
        };
        assert!(!leaves(&state).contains(&unused_leaf_id(&state)));
    }

    #[test]
    fn apply_spawned_ok_anchors_on_the_requested_leaf_or_live_focus() {
        let out = apply_spawned_ok(
            &LayoutState::single(t(1)),
            t(2),
            &split_of(t(1), SplitDir::Horizontal, false),
        )
        .unwrap();
        assert_eq!(out.focus, Some(t(2)));
        assert_eq!(leaves(&out), vec![t(1), t(2)]);

        // Focus moved to 3 before the reply; the chord targeted 2.
        let mut state = three_pane_mixed();
        state.focus = Some(t(3));
        let out =
            apply_spawned_ok(&state, t(99), &split_of(t(2), SplitDir::Horizontal, false)).unwrap();
        assert_eq!(leaves(&out), vec![t(1), t(2), t(99), t(3)]);
        assert_eq!(out.focus, Some(t(99)));

        // The requested leaf is gone: anchor at the live focus.
        let out = apply_spawned_ok(
            &LayoutState::single(t(1)),
            t(2),
            &split_of(t(42), SplitDir::Vertical, false),
        )
        .unwrap();
        assert_eq!(leaves(&out), vec![t(1), t(2)]);
    }

    #[test]
    fn apply_terminal_closed_folds_known_leaves_only() {
        let mut state = two_pane(SplitDir::Horizontal);
        state.focus = Some(t(2));
        let out = apply_terminal_closed(&state, &t(1)).unwrap();
        assert!(matches!(out.tree.as_ref().unwrap(), LayoutNode::Leaf(p) if *p == t(2)));
        assert_eq!(out.focus, Some(t(2)));
        let out = apply_terminal_closed(&LayoutState::single(t(1)), &t(1)).unwrap();
        assert!(out.tree.is_none() && out.focus.is_none());
        assert!(matches!(
            apply_terminal_closed(&LayoutState::single(t(1)), &t(99)),
            Err(ActionError::Layout(_))
        ));
    }

    /// Any split/close sequence keeps `leaves == splits - closes + 1`.
    #[test]
    fn split_close_sequence_preserves_leaf_count() {
        let mut state = LayoutState::single(t(1));
        let mut expected = 1;
        for (next_id, dir) in (2_u32..).zip([
            SplitDir::Horizontal,
            SplitDir::Vertical,
            SplitDir::Horizontal,
        ]) {
            let pending = split_of(state.focus.clone().unwrap(), dir, false);
            state = apply_spawned_ok(&state, t(next_id), &pending).unwrap();
            expected += 1;
            assert_eq!(leaves(&state).len(), expected);
        }
        for _ in 0..2 {
            let dying = leaves(&state)[0].clone();
            state = apply_terminal_closed(&state, &dying).unwrap();
            expected -= 1;
            assert_eq!(leaves(&state).len(), expected);
        }
    }
}
