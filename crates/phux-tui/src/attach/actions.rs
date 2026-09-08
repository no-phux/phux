//! Pure layout-action helpers for the multi-pane TUI dispatcher (phux-4li.5).
//!
//! Per ADR-0019 decisions 1, 2, and 6 the client interprets keybind
//! `ResolvedAction`s (resolved by [`phux_config::keybind::Resolver`]) into
//! mutations of a [`LayoutState`] and side-effects on the wire:
//!
//! * Local mutations: focus moves, ratio adjustments on resize, leaf
//!   removal/insertion.
//! * Wire side-effects: `SET_METADATA` to broadcast the new layout to other
//!   attached clients (ADR-0019 decision 2). The compatibility envelope still
//!   contains focus fields, but ADR-0049 makes them non-authoritative: receivers
//!   preserve client-local focus while adopting topology.
//!
//! The pure functions in this module take a `&LayoutState` and return a
//! transformed `LayoutState` (or `Option<LayoutState>` / `Result`). They
//! never touch the connection, never read the wall clock, never paint. The
//! driver wraps them with frame I/O and a repaint trigger.
//!
//! # Scope (v0.1)
//!
//! Five user-visible actions land per ADR-0019:
//!
//! | Action            | Pure helper            | Wire side-effects   |
//! |-------------------|------------------------|---------------------|
//! | `split-pane`      | [`apply_split`]        | `SPAWN` + `SET_METADATA` (deferred) |
//! | `kill-pane`       | [`apply_kill`]         | `SPAWN_KILL` + `SET_METADATA` (deferred) |
//! | `focus-direction` | [`apply_focus`]        | none (focus is per-client) |
//! | `resize-pane`     | `apply_resize`         | `SET_METADATA`              |
//! | `next-pane`       | [`apply_next_pane`]    | none (focus is per-client) |
//! | `previous-pane`   | [`apply_previous_pane`]| none (focus is per-client) |
//!
//! The two SPAWN-requiring actions (`split-pane`, `kill-pane`) are
//! implemented as pure tree operations here so they unit-test cleanly; the
//! driver-side frame I/O is partially blocked on the SPAWN frame family
//! (no `SpawnTerminal` / `KillTerminal` variants exist in
//! [`phux_protocol::wire::frame::FrameKind`] as of this commit). See the
//! TODO comments at the call sites in `attach::driver`.
//!
//! [ADR-0019]: ../../../ADR/0019-tui-multi-pane-rendering.md

use std::io::{self, Write};

use phux_protocol::TerminalId;
use thiserror::Error;

use super::paint::{SidebarReservation, content_rect};
use crate::layout::{
    self, Direction, LayoutError, LayoutNode, LayoutState, NodePath, Rect, SplitDir,
};
use crate::multi_pane::{pane_rects_proportional_in, split_content_span_at};

/// Errors returned by the pure action helpers.
///
/// These wrap [`LayoutError`] for "tree-shape" failures (target not in
/// layout, invalid ratio). The driver translates them into log lines and
/// terminal bells; they do not propagate to the user as messages because
/// the action UI is purely visual.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum ActionError {
    /// The focused pane is `None` — there is no pane to act on. The
    /// driver should log + drop the action.
    #[error("no focused pane")]
    NoFocus,
    /// The layout tree is empty (`tree.is_none()`). Most actions are
    /// meaningless here; the driver should log + drop.
    #[error("layout tree is empty")]
    EmptyTree,
    /// The tree operation failed — propagated from [`crate::layout`].
    #[error("layout error: {0}")]
    Layout(#[from] LayoutError),
    /// `resize-pane` could not find an interior split along the action's
    /// axis (e.g. attempting `resize-pane direction=left` in a layout
    /// with only vertical splits between the focused pane and the root).
    /// The driver bells.
    #[error("no resizable boundary in direction")]
    NoResizableBoundary,
}

// -----------------------------------------------------------------------------
// split-pane
// -----------------------------------------------------------------------------

/// Apply a `split-pane` action.
///
/// Splits the leaf at `state.focus` into two children along `dir` at a
/// 50/50 ratio. `new_pane` is the [`TerminalId`] for the new sibling
/// (the caller is responsible for obtaining one — see the ADR-0019
/// decision 2 SPAWN path; driver-side wiring is deferred pending the
/// SPAWN frame family).
///
/// Returns a fresh [`LayoutState`] with the new tree and `focus` set to
/// the new pane (so the next render lands focus chrome on the freshly
/// spawned pane — tmux-compatible).
///
/// # Errors
/// * [`ActionError::NoFocus`] / [`ActionError::EmptyTree`] if `focus` or
///   `tree` is `None`.
/// * [`ActionError::Layout`] propagated from
///   [`layout::split_at`] (`PaneNotInLayout`, `InvalidRatio`).
pub fn apply_split(
    state: &LayoutState,
    new_pane: TerminalId,
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

// -----------------------------------------------------------------------------
// kill-pane
// -----------------------------------------------------------------------------

/// Apply a `kill-pane` action (target = focused pane in v0.1).
///
/// Removes the focused leaf from the tree. If the tree had a single
/// leaf, returns a [`LayoutState`] with both `tree` and `focus` set to
/// `None` (the caller may treat this as "close the window"). Otherwise
/// the surviving sibling is promoted into the killed leaf's parent slot
/// (see [`layout::kill_pane`]) and `focus` is moved to the first leaf
/// of the new tree in left-to-right DFS order (ADR-0019 decision 6:
/// post-kill focus default).
///
/// # Errors
/// * [`ActionError::NoFocus`] / [`ActionError::EmptyTree`] if `focus` or
///   `tree` is `None`.
/// * [`ActionError::Layout`] propagated from [`layout::kill_pane`]
///   (`PaneNotInLayout`).
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
            // Post-kill focus default: first leaf in left-to-right DFS
            // order (ADR-0019 decision 6).
            let focus = layout::leaves(&tree).into_iter().next();
            LayoutState {
                tree: Some(tree),
                focus,
            }
        },
    ))
}

// -----------------------------------------------------------------------------
// focus-direction
// -----------------------------------------------------------------------------

/// Apply a `focus-direction` action.
///
/// Returns `Some(new_state)` when [`layout::focus_direction`] finds a
/// neighbour leaf in `dir`; `None` when there is no neighbour (the
/// focused pane is already at the corresponding edge of the layout).
/// `None` should cause the driver to log + drop (no bell — bumping into
/// the layout edge is not an error condition; matches tmux).
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

// -----------------------------------------------------------------------------
// resize-pane
// -----------------------------------------------------------------------------

/// Minimum width / height for any leaf rectangle, in cells. Below this
/// the resize is rejected (the driver bells). Per ADR-0019 decision 5
/// the floor is 2 along the active axis.
const MIN_PANE_CELL: u16 = 2;

/// Apply a `resize-pane` action.
///
/// Walks the path from the focused leaf up to the root, finds the first
/// interior [`LayoutNode::Split`] whose `dir` is perpendicular to the
/// requested direction (i.e. a Horizontal split for `Direction::Left`/
/// `Right`, a Vertical split for `Direction::Up`/`Down`), and adjusts
/// its `ratio` by `amount / total_cells_along_axis`. The sign of
/// `amount` is interpreted relative to the focused pane: positive
/// `amount` enlarges the focused pane, negative shrinks it.
///
/// Returns `Ok(None)` when the requested ratio would cause either child
/// of the resized split to fall below 2 cells along the resize axis —
/// the driver should bell and not repaint (ADR-0019 decision 5).
///
/// `viewport` is `(cols, rows)` of the outer terminal. Pure helpers
/// don't query the kernel.
///
/// # Errors
/// * [`ActionError::NoFocus`] / [`ActionError::EmptyTree`] for empty state.
/// * [`ActionError::NoResizableBoundary`] when no interior split along
///   the requested axis exists between the focused leaf and the root.
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
        // Degenerate no-op: matches "do nothing", not "bell". Return
        // the original state.
        return Ok(Some(state.clone()));
    }
    let target_axis = match dir {
        Direction::Left | Direction::Right => SplitDir::Horizontal,
        Direction::Up | Direction::Down => SplitDir::Vertical,
    };
    let total_cells = match target_axis {
        SplitDir::Horizontal => viewport.0,
        SplitDir::Vertical => viewport.1,
        _ => return Err(ActionError::NoResizableBoundary),
    };
    if total_cells == 0 {
        return Err(ActionError::NoResizableBoundary);
    }
    let delta = f32::from(amount) / f32::from(total_cells);
    // Build the new tree, recording whether the focused leaf was on the
    // low side of the chosen split — `growing_low_side` flips the sign
    // of `delta` so positive `amount` always means "the focused pane
    // grows in `dir`".
    let (new_tree, applied) = resize_along_axis(tree, focused, target_axis, dir, delta);
    if !applied {
        return Err(ActionError::NoResizableBoundary);
    }
    let candidate = LayoutState {
        tree: Some(new_tree),
        focus: Some(focused.clone()),
    };
    // ADR-0019 decision 5: bell-and-no-op when any child would drop
    // below `MIN_PANE_CELL` on the active axis.
    if violates_min_cell(&candidate, viewport, sidebar) {
        return Ok(None);
    }
    Ok(Some(candidate))
}

/// Walk the tree top-down, adjust the first split matching `axis` that
/// contains `focused`, and return `(new_tree, applied)`.
///
/// `dir` is the user-facing resize direction; it controls whether
/// `delta` is added (focused on the low/left/top side, growing toward
/// Right/Down) or subtracted (focused on the high side, growing toward
/// Left/Up). Cases:
///
/// * `dir == Right` & focused is left of the split → `+delta`
/// * `dir == Right` & focused is right of the split → `-delta`
/// * mirror for the other three directions
fn resize_along_axis(
    node: &LayoutNode,
    focused: &TerminalId,
    axis: SplitDir,
    dir: Direction,
    delta: f32,
) -> (LayoutNode, bool) {
    match node {
        LayoutNode::Leaf(p) => (LayoutNode::Leaf(p.clone()), false),
        LayoutNode::Split {
            dir: sd,
            ratio,
            left,
            right,
        } => {
            // Does this split match the resize axis AND contain the
            // focused leaf as a descendant? If yes, adjust here and
            // stop descending; if no, recurse into the matching child.
            if *sd == axis {
                let left_has = tree_contains(left, focused);
                let right_has = tree_contains(right, focused);
                if left_has || right_has {
                    let signed_delta = match (dir, left_has) {
                        (Direction::Right | Direction::Down, true)
                        | (Direction::Left | Direction::Up, false) => delta,
                        _ => -delta,
                    };
                    let new_ratio = clamp_ratio(*ratio + signed_delta);
                    return (
                        LayoutNode::Split {
                            dir: *sd,
                            ratio: new_ratio,
                            left: left.clone(),
                            right: right.clone(),
                        },
                        true,
                    );
                }
            }
            // Otherwise descend into whichever subtree contains the focus.
            if tree_contains(left, focused) {
                let (new_left, applied) = resize_along_axis(left, focused, axis, dir, delta);
                (
                    LayoutNode::Split {
                        dir: *sd,
                        ratio: *ratio,
                        left: Box::new(new_left),
                        right: right.clone(),
                    },
                    applied,
                )
            } else if tree_contains(right, focused) {
                let (new_right, applied) = resize_along_axis(right, focused, axis, dir, delta);
                (
                    LayoutNode::Split {
                        dir: *sd,
                        ratio: *ratio,
                        left: left.clone(),
                        right: Box::new(new_right),
                    },
                    applied,
                )
            } else {
                (node.clone(), false)
            }
        }
        // `LayoutNode` is `#[non_exhaustive]`; v0.1 only sees Leaf+Split.
        _ => (node.clone(), false),
    }
}

fn tree_contains(node: &LayoutNode, target: &TerminalId) -> bool {
    match node {
        LayoutNode::Leaf(p) => p == target,
        LayoutNode::Split { left, right, .. } => {
            tree_contains(left, target) || tree_contains(right, target)
        }
        _ => false,
    }
}

/// Clamp a candidate ratio strictly inside `(0.0, 1.0)`. The layout-tree
/// invariant (split-at rejects 0.0 or 1.0) protects us from degenerate
/// trees; this clamps with a small epsilon so a near-edge resize that
/// would mathematically hit the boundary stays just inside.
fn clamp_ratio(r: f32) -> f32 {
    const EPS: f32 = 0.001;
    r.clamp(EPS, 1.0 - EPS)
}

/// Check whether any leaf in `state` falls below [`MIN_PANE_CELL`] cells
/// in either axis under `viewport`. Used by [`apply_resize`] to gate
/// the bell-no-op per ADR-0019 decision 5.
///
/// phux-4h5a: tiles into the inset content rect so the underflow check sees
/// the same width panes paint into when a `sidebar` is docked. The status-bar
/// row is intentionally NOT reserved here (`bar = None`): this gate has
/// always measured against the full pane-row budget, and the
/// `content_rect(.., None, None)` form reproduces the prior
/// `pane_rects(tree, viewport)` byte-for-byte on the disabled path.
///
/// phux-foz.3: measures the *proportional* tiling
/// ([`pane_rects_proportional_in`]), not the frozen tiling paint uses.
/// The §6.2 viewport-reflow floor would otherwise pin every rect at
/// minimum and this gate would never trip, letting `resize-pane` bank
/// unbounded ratio behind a frozen divider.
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

// -----------------------------------------------------------------------------
// drag-to-resize (ADR-0048): node-targeted resize from an absolute pointer
// -----------------------------------------------------------------------------

/// Set the split addressed by `node_path` so its divider sits under the
/// pointer at absolute outer-viewport cell `pointer` (ADR-0048 drag).
///
/// Unlike [`apply_resize`] — which walks up from the focused leaf and
/// nudges a split by a relative `amount` — this targets a specific split
/// (the one the grabbed divider controls) and sets its `ratio` from an
/// *absolute* position, so the divider tracks the cursor. `axis` selects
/// which pointer coordinate drives the ratio: a `Horizontal` split reads
/// `pointer.0` (x), a `Vertical` split reads `pointer.1` (y).
///
/// Reuses the keyboard-resize floor: returns `Ok(None)` (driver bells /
/// no-ops, no repaint) when the new ratio would push either child below
/// [`MIN_PANE_CELL`] cells, so a drag stalls at the floor instead of
/// collapsing a pane. The committed ratio is `clamp_ratio`-bounded just
/// like the keybind path, so the two resize routes cannot diverge.
///
/// # Errors
/// * [`ActionError::EmptyTree`] when `state.tree` is `None`.
/// * [`ActionError::NoResizableBoundary`] when `node_path` no longer
///   addresses a `Split` in the current tree (a stale grab after the
///   layout changed mid-drag) or its content budget is zero.
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
    // The content rect mouse routing tiled into. This MUST match the
    // hit-test's `content_rect(viewport, bar, sidebar)`: the pointer
    // arrives in that same inset cell space, so the divider span and the
    // pointer share an origin and the divider tracks the cursor exactly. A
    // status bar shortens the content on its axis, so for a Vertical split
    // (horizontal divider, y-driven) `bar` shifts the budget; an
    // absolute drag that ignored it would mis-track when a bar is docked.
    let content = content_rect(viewport, bar, sidebar);
    let (start, content_len) =
        split_content_span_at(tree, content, node_path).ok_or(ActionError::NoResizableBoundary)?;
    // Position along the split's axis. The divider should land under the
    // pointer: left/top size = pointer - span start, clamped into the span.
    let p = match axis {
        SplitDir::Horizontal => pointer.0,
        SplitDir::Vertical => pointer.1,
        _ => return Err(ActionError::NoResizableBoundary),
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

// -----------------------------------------------------------------------------
// next-pane / previous-pane
// -----------------------------------------------------------------------------

/// Apply a `next-pane` action: cycle focus to the next leaf in DFS
/// order, wrapping at the end. Returns `None` when the layout has zero
/// or one leaves (nothing to cycle to); driver should log + drop.
#[must_use]
pub fn apply_next_pane(state: &LayoutState) -> Option<LayoutState> {
    cycle(state, 1)
}

/// Apply a `previous-pane` action: cycle focus to the previous leaf in
/// DFS order, wrapping at the start. Same `None` semantics as
/// [`apply_next_pane`].
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

// -----------------------------------------------------------------------------
// Driver-side helper: emit a terminal bell.
// -----------------------------------------------------------------------------

/// Write a BEL (`\x07`) to `out` and flush.
///
/// Used by the driver on `apply_resize` bell-no-op and on actions that
/// find no work to do (e.g. `focus-direction` at the edge) where
/// ADR-0019 prescribes a bell instead of silent drop.
///
/// # Errors
/// Forwards any `io::Error` from `out`.
pub fn write_bell<W: Write>(out: &mut W) -> io::Result<()> {
    out.write_all(b"\x07")?;
    out.flush()
}

// -----------------------------------------------------------------------------
// Pending-split bookkeeping + spawned/closed seams (phux-4li.12)
// -----------------------------------------------------------------------------

/// Parked state for an in-flight `split-pane` action (phux-4li.12).
///
/// `run_action` emits a `SPAWN_TERMINAL` request and parks one of these
/// keyed by the request id. When the matching `TERMINAL_SPAWNED { Ok }`
/// reply arrives, the driver applies [`crate::attach::actions::apply_split`] against
/// the focused leaf captured here, splitting along the recorded
/// direction. If a sibling action mutated focus between request and
/// reply, the captured `focused_at_request` keeps the split anchored
/// to the leaf the user actually targeted.
#[derive(Debug, Clone)]
pub(super) struct PendingSplit {
    /// Leaf the user was focused on when they pressed the chord; the
    /// split is applied against this id, not the live focus (which may
    /// have moved). Empty layouts can't request a split so this is
    /// always populated.
    pub focused_at_request: TerminalId,
    /// Axis along which to split.
    pub dir: SplitDir,
    /// phux-r82.7: `true` ⇒ after the split applies, zoom the freshly
    /// spawned pane to fill the window instead of the default un-zoom
    /// (`placement = "zoomed"` plugin panes). Built-in `split-pane`
    /// always parks `false` (a split un-zooms, tmux parity).
    pub zoom_on_spawn: bool,
}

/// A `new-window` action that emitted a `SPAWN_TERMINAL` and is awaiting
/// its `TERMINAL_SPAWNED` reply (phux-4li.15). The reply handler adds a
/// new window named `name` holding the spawned pane as its sole leaf.
/// Parked separately from [`PendingSplit`] (keyed by the same
/// `request_id` space) so the reply knows whether it's growing the
/// active window or opening a new one.
#[derive(Debug, Clone)]
pub(super) struct PendingWindow {
    /// Name for the window the spawned pane will seed.
    pub name: String,
}

/// Pure seam for the `TerminalSpawned { Ok }` handler (phux-4li.12).
///
/// Applies a parked [`PendingSplit`] against `state`. The driver side
/// then takes the returned new state, replaces its `layout_state`, and
/// emits `SET_METADATA` + a repaint. Extracted out of
/// `handle_server_frame` so the layout-mutation contract is unit
/// testable without driving an async loop.
///
/// If `pending.focused_at_request` no longer exists in the tree (it
/// was killed between the user pressing the chord and the spawn reply
/// landing) the split is anchored at the current focus instead. If
/// there is no current focus either, returns `Err(NoFocus)` and the
/// driver bells + drops the spawned terminal id.
///
/// # Errors
/// Propagates [`ActionError`] from [`apply_split`].
pub(super) fn apply_spawned_ok(
    state: &LayoutState,
    new_id: TerminalId,
    pending: &PendingSplit,
) -> Result<LayoutState, ActionError> {
    // Anchor the split against the leaf the user targeted; if it's
    // gone, fall back to live focus.
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
    // apply_split splits the *focused* leaf. Build a transient state
    // with focus moved to the anchor, then call apply_split.
    let anchored = LayoutState {
        tree: state.tree.clone(),
        focus: Some(anchor),
    };
    apply_split(&anchored, new_id, pending.dir)
}

/// A local `TerminalId` that is not a leaf of `state` — a stand-in for the
/// id the server has not allocated yet.
///
/// Geometry does not depend on which id a leaf carries, only on the shape of
/// the tree, so tiling a provisional split against a placeholder yields the
/// rect the real leaf will get. The id only has to be distinct from every
/// live leaf, or `split_at` would attach the new node to the wrong place.
fn unused_leaf_id(state: &LayoutState) -> TerminalId {
    let leaves = state
        .tree
        .as_ref()
        .map(crate::layout::leaves)
        .unwrap_or_default();
    // A window with u32::MAX distinct panes cannot be tiled into a u16
    // viewport, so this loop terminates long before it exhausts the space.
    let mut candidate = u32::MAX;
    loop {
        let id = TerminalId::local(candidate);
        if !leaves.contains(&id) {
            return id;
        }
        candidate = candidate.wrapping_sub(1);
    }
}

/// The `(cols, rows)` the pane a parked [`PendingSplit`] is waiting on will
/// occupy once its reply lands (phux-a5xj).
///
/// This is the same computation the post-reply reflow performs — the split
/// applied by [`apply_spawned_ok`], tiled by
/// [`crate::multi_pane::pane_rects_in`] into the same `content` rect — run
/// one round trip early, against a placeholder id. Sending the answer as
/// `SPAWN_TERMINAL.initial_size` means the server builds the pane's grid,
/// PTY, and bootstrap generation at the client's real geometry, and the
/// `TERMINAL_RESIZE` the reflow emits next is a no-op rather than a
/// tombstone over a checkpoint that was just captured.
///
/// `None` when the split cannot be predicted — an empty tree, an anchor that
/// is not in the layout — in which case the caller omits the field and the
/// server's default plus the follow-up resize behave exactly as before.
pub(super) fn predicted_spawn_dims(
    state: &LayoutState,
    pending: &PendingSplit,
    content: Rect,
) -> Option<(u16, u16)> {
    // phux-r82.7: a `zoom_on_spawn` split zooms the new pane the instant it
    // lands, and `Workspace::render_window` tiles a zoomed pane as a lone
    // full-content leaf — so the split's own geometry never reaches the PTY.
    if pending.zoom_on_spawn {
        return Some((content.w, content.h));
    }
    let placeholder = unused_leaf_id(state);
    let next = apply_spawned_ok(state, placeholder.clone(), pending).ok()?;
    let rects = crate::multi_pane::pane_rects_in(next.tree.as_ref()?, content);
    rects.get(&placeholder).map(|rect| (rect.w, rect.h))
}

/// Pure seam for the `TerminalClosed` handler (phux-4li.12).
///
/// Folds `dying` out of `state`, using [`apply_kill`] under
/// the hood. Because `apply_kill` operates on `state.focus`, this
/// helper first sets focus to `dying`, then applies the kill — the
/// post-kill focus policy (first DFS leaf) lives inside `apply_kill`
/// and is preserved.
///
/// Returns `Ok(new_state)` when the fold succeeded, `Err(_)` when the
/// dying terminal wasn't a leaf in the tree (treat as a no-op — the
/// caller drops the `PaneSlot` either way).
///
/// # Errors
/// Propagates [`ActionError`] from [`apply_kill`].
pub(super) fn apply_terminal_closed(
    state: &LayoutState,
    dying: &TerminalId,
) -> Result<LayoutState, ActionError> {
    let anchored = LayoutState {
        tree: state.tree.clone(),
        focus: Some(dying.clone()),
    };
    apply_kill(&anchored)
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::*;
    use crate::layout::{LayoutNode, SplitDir, split_at};

    fn t(id: u32) -> TerminalId {
        TerminalId::local(id)
    }

    fn two_pane_h() -> LayoutState {
        // (1 | 2), focus on 1.
        let tree = split_at(
            &LayoutNode::Leaf(t(1)),
            &t(1),
            &t(2),
            SplitDir::Horizontal,
            0.5,
        )
        .unwrap();
        LayoutState {
            tree: Some(tree),
            focus: Some(t(1)),
        }
    }

    fn two_pane_v() -> LayoutState {
        // (1 / 2), focus on 1 — a Vertical split (horizontal divider).
        let tree = split_at(
            &LayoutNode::Leaf(t(1)),
            &t(1),
            &t(2),
            SplitDir::Vertical,
            0.5,
        )
        .unwrap();
        LayoutState {
            tree: Some(tree),
            focus: Some(t(1)),
        }
    }

    fn three_pane_mixed() -> LayoutState {
        // ((1 | 2) / 3), focus on 2.
        let t1 = split_at(
            &LayoutNode::Leaf(t(1)),
            &t(1),
            &t(2),
            SplitDir::Horizontal,
            0.5,
        )
        .unwrap();
        let t2 = split_at(&t1, &t(2), &t(3), SplitDir::Vertical, 0.5).unwrap();
        LayoutState {
            tree: Some(t2),
            focus: Some(t(2)),
        }
    }

    // ---------- apply_split ----------

    #[test]
    fn split_promotes_new_pane_to_focus() {
        let state = LayoutState::single(t(1));
        let out = apply_split(&state, t(2), SplitDir::Horizontal).unwrap();
        assert_eq!(out.focus, Some(t(2)));
        let leaves = layout::leaves(out.tree.as_ref().unwrap());
        assert_eq!(leaves, vec![t(1), t(2)]);
    }

    #[test]
    fn split_rejects_empty_state() {
        let state = LayoutState::default();
        let err = apply_split(&state, t(2), SplitDir::Horizontal).unwrap_err();
        assert!(matches!(err, ActionError::EmptyTree));
    }

    #[test]
    fn split_rejects_no_focus() {
        let state = LayoutState {
            tree: Some(LayoutNode::Leaf(t(1))),
            focus: None,
        };
        let err = apply_split(&state, t(2), SplitDir::Horizontal).unwrap_err();
        assert!(matches!(err, ActionError::NoFocus));
    }

    // ---------- apply_kill ----------

    #[test]
    fn kill_last_pane_empties_state() {
        let state = LayoutState::single(t(1));
        let out = apply_kill(&state).unwrap();
        assert!(out.tree.is_none());
        assert!(out.focus.is_none());
    }

    #[test]
    fn kill_collapses_split_and_picks_first_leaf() {
        let state = two_pane_h(); // (1|2), focus 1
        let out = apply_kill(&state).unwrap();
        // After killing 1, tree collapses to leaf(2); focus → 2.
        assert!(matches!(out.tree.as_ref().unwrap(), LayoutNode::Leaf(p) if *p == t(2)));
        assert_eq!(out.focus, Some(t(2)));
    }

    #[test]
    fn kill_picks_first_leaf_in_dfs_order_after_collapse() {
        let state = three_pane_mixed(); // ((1|2)/3), focus 2
        let out = apply_kill(&state).unwrap();
        // After killing 2, the (1|2) split collapses to leaf(1); root
        // becomes (1/3). First leaf is 1.
        assert_eq!(out.focus, Some(t(1)));
        let mut leaves: Vec<_> = layout::leaves(out.tree.as_ref().unwrap());
        leaves.sort_by_key(|id| id.local_id().unwrap_or_default());
        assert_eq!(leaves, vec![t(1), t(3)]);
    }

    // ---------- apply_focus ----------

    #[test]
    fn focus_moves_right_across_horizontal_split() {
        let state = two_pane_h(); // focus on 1
        let out = apply_focus(&state, Direction::Right).expect("has neighbour");
        assert_eq!(out.focus, Some(t(2)));
    }

    #[test]
    fn focus_returns_none_at_edge() {
        let state = two_pane_h(); // focus on 1
        assert!(apply_focus(&state, Direction::Up).is_none());
        assert!(apply_focus(&state, Direction::Left).is_none());
    }

    #[test]
    fn focus_empty_state_returns_none() {
        let state = LayoutState::default();
        assert!(apply_focus(&state, Direction::Right).is_none());
    }

    // ---------- apply_resize ----------

    #[test]
    fn resize_grow_focused_right() {
        let state = two_pane_h(); // (1|2), focus 1, ratio 0.5
        // viewport 80x24, amount=8 → delta = 8/80 = 0.1; new ratio 0.6.
        let out = apply_resize(&state, Direction::Right, 8, (80, 24), None)
            .unwrap()
            .unwrap();
        let LayoutNode::Split { ratio, .. } = out.tree.as_ref().unwrap() else {
            panic!("expected split");
        };
        assert!((ratio - 0.6).abs() < 1e-4, "got ratio {ratio}");
    }

    #[test]
    fn resize_shrink_focused_left() {
        let state = two_pane_h(); // (1|2), focus 1, ratio 0.5
        let out = apply_resize(&state, Direction::Left, 8, (80, 24), None)
            .unwrap()
            .unwrap();
        let LayoutNode::Split { ratio, .. } = out.tree.as_ref().unwrap() else {
            panic!("expected split");
        };
        assert!((ratio - 0.4).abs() < 1e-4);
    }

    #[test]
    fn resize_bell_when_child_would_drop_below_two_cells() {
        // 2-pane H-split at 0.5 in an 80-wide viewport. Pushing the
        // divider 80 cells to the left would put the focused leaf
        // (left side) at 0 cells — well below the 2-cell floor.
        let state = two_pane_h();
        let out = apply_resize(&state, Direction::Left, 80, (80, 24), None).unwrap();
        assert!(out.is_none(), "expected bell-no-op, got {out:?}");
    }

    #[test]
    fn resize_zero_amount_is_no_change() {
        let state = two_pane_h();
        let out = apply_resize(&state, Direction::Right, 0, (80, 24), None)
            .unwrap()
            .unwrap();
        assert_eq!(out, state);
    }

    #[test]
    fn resize_no_matching_axis_returns_error() {
        // Single pane: no interior split to adjust at all.
        let state = LayoutState::single(t(1));
        let err = apply_resize(&state, Direction::Right, 5, (80, 24), None).unwrap_err();
        assert!(matches!(err, ActionError::NoResizableBoundary));
    }

    #[test]
    fn resize_perpendicular_direction_unmatched_returns_error() {
        // (1|2) only has a Horizontal split (vertical divider). A
        // Direction::Up resize wants a Vertical split (horizontal
        // divider); none exists → NoResizableBoundary.
        let state = two_pane_h();
        let err = apply_resize(&state, Direction::Up, 5, (80, 24), None).unwrap_err();
        assert!(matches!(err, ActionError::NoResizableBoundary));
    }

    // ---------- apply_divider_resize (ADR-0048 drag) ----------

    #[test]
    fn divider_resize_sets_ratio_from_absolute_pointer() {
        // (1 | 2) at 0.5 in an 80x24 viewport (no chrome). The root
        // Horizontal split's content budget is 80 - 1 = 79 cells. Drop the
        // pointer at column 32: left size = 32, ratio = 32/79 ≈ 0.405.
        let state = two_pane_h();
        let path = NodePath::root();
        let out = apply_divider_resize(
            &state,
            &path,
            SplitDir::Horizontal,
            (32, 10),
            (80, 24),
            None,
            None,
        )
        .unwrap()
        .unwrap();
        let LayoutNode::Split { ratio, .. } = out.tree.as_ref().unwrap() else {
            panic!("expected split");
        };
        assert!((ratio - 32.0 / 79.0).abs() < 1e-3, "got ratio {ratio}");
    }

    #[test]
    fn divider_resize_tracks_the_pointer_both_ways() {
        // Dragging the divider right then left moves the ratio up then down.
        let state = two_pane_h();
        let path = NodePath::root();
        let right = apply_divider_resize(
            &state,
            &path,
            SplitDir::Horizontal,
            (60, 5),
            (80, 24),
            None,
            None,
        )
        .unwrap()
        .unwrap();
        let left = apply_divider_resize(
            &state,
            &path,
            SplitDir::Horizontal,
            (20, 5),
            (80, 24),
            None,
            None,
        )
        .unwrap()
        .unwrap();
        let r = |s: &LayoutState| {
            let LayoutNode::Split { ratio, .. } = s.tree.as_ref().unwrap() else {
                panic!("split");
            };
            *ratio
        };
        assert!(r(&right) > 0.5, "drag right grows left pane: {}", r(&right));
        assert!(r(&left) < 0.5, "drag left shrinks left pane: {}", r(&left));
    }

    #[test]
    fn divider_resize_respects_min_cell_floor() {
        // Pointer at column 0 would put the left pane at 0 cells — below
        // the 2-cell floor — so the resize bell-no-ops (Ok(None)).
        let state = two_pane_h();
        let path = NodePath::root();
        let out = apply_divider_resize(
            &state,
            &path,
            SplitDir::Horizontal,
            (0, 5),
            (80, 24),
            None,
            None,
        )
        .unwrap();
        assert!(out.is_none(), "expected min-cell no-op, got {out:?}");
    }

    #[test]
    fn divider_resize_stale_path_errors() {
        // A single-pane layout has no split; the root path addresses no
        // Split, so the resize errors (stale grab after the layout changed).
        let state = LayoutState::single(t(1));
        let path = NodePath::root();
        let err = apply_divider_resize(
            &state,
            &path,
            SplitDir::Horizontal,
            (40, 5),
            (80, 24),
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(err, ActionError::NoResizableBoundary));
    }

    #[test]
    fn divider_resize_vertical_split_honours_status_bar_budget() {
        // A Vertical split (horizontal divider, y-driven) over a 80x24
        // viewport with a status bar reserved: the bar takes a row and
        // the pane-grid rail takes another, so the content is 22 rows
        // starting at y = 1 and the split's budget is 22 - 1 = 21. The
        // SAME pointer y maps to a different ratio with vs without the
        // bar, proving the drag tracks the painted divider rather than
        // the full-viewport budget.
        let state = two_pane_v();
        let path = NodePath::root();
        let with_bar = apply_divider_resize(
            &state,
            &path,
            SplitDir::Vertical,
            (5, 11),
            (80, 24),
            Some(crate::render::chrome::status_bar::Position::Bottom),
            None,
        )
        .unwrap()
        .unwrap();
        let no_bar = apply_divider_resize(
            &state,
            &path,
            SplitDir::Vertical,
            (5, 11),
            (80, 24),
            None,
            None,
        )
        .unwrap()
        .unwrap();
        let r = |s: &LayoutState| {
            let LayoutNode::Split { ratio, .. } = s.tree.as_ref().unwrap() else {
                panic!("split");
            };
            *ratio
        };
        // y=11 is row 10 of the content (the rail owns row 0). Over a
        // 21-cell budget (bar) that is a larger ratio than over a
        // 22-cell budget (no bar): the shorter content makes the same
        // row sit proportionally lower.
        assert!(
            (r(&with_bar) - 10.0 / 21.0).abs() < 1e-3,
            "bar: {}",
            r(&with_bar)
        );
        assert!(
            (r(&no_bar) - 10.0 / 22.0).abs() < 1e-3,
            "no bar: {}",
            r(&no_bar)
        );
        assert!(r(&with_bar) > r(&no_bar), "bar shortens the budget");
    }

    // ---------- next-pane / previous-pane ----------

    #[test]
    fn next_pane_cycles_dfs() {
        // ((1|2)/3), focus 1 → next 2 → next 3 → wrap to 1.
        let mut state = three_pane_mixed();
        state.focus = Some(t(1));
        let s2 = apply_next_pane(&state).unwrap();
        assert_eq!(s2.focus, Some(t(2)));
        let s3 = apply_next_pane(&s2).unwrap();
        assert_eq!(s3.focus, Some(t(3)));
        let s1 = apply_next_pane(&s3).unwrap();
        assert_eq!(s1.focus, Some(t(1)));
    }

    #[test]
    fn previous_pane_cycles_reverse_dfs() {
        let mut state = three_pane_mixed();
        state.focus = Some(t(1));
        let s3 = apply_previous_pane(&state).unwrap();
        assert_eq!(s3.focus, Some(t(3)));
        let s2 = apply_previous_pane(&s3).unwrap();
        assert_eq!(s2.focus, Some(t(2)));
    }

    #[test]
    fn next_pane_returns_none_on_single_leaf() {
        let state = LayoutState::single(t(1));
        assert!(apply_next_pane(&state).is_none());
        assert!(apply_previous_pane(&state).is_none());
    }

    // ---------- write_bell ----------

    #[test]
    fn write_bell_emits_bel() {
        let mut buf = Vec::new();
        write_bell(&mut buf).unwrap();
        assert_eq!(&buf, b"\x07");
    }

    // ---------------------------------------------------------------------
    // phux-4li.12: pure-seam tests for split-pane / kill-pane wiring.
    //
    // The driver's async main_loop is hard to test in isolation because
    // it wires together a tokio select! across signals, sockets, and
    // libghostty. Instead we extract `apply_spawned_ok` and
    // `apply_terminal_closed` as pure functions and test those — the
    // async dispatcher's job is mechanical (allocate id, send frame,
    // park intent) and is covered indirectly by the round-trip integ
    // tests in phux-server.
    // ---------------------------------------------------------------------

    // ---------------------------------------------------------------------
    // phux-a5xj — spawn-time geometry prediction
    // ---------------------------------------------------------------------

    /// The property the whole fix rests on: what the client predicts at
    /// SPAWN time is byte-for-byte what its own post-reply reflow computes.
    ///
    /// If these ever diverge, the server builds the pane at one size, the
    /// reflow immediately resizes it to another, and the bootstrap-then-
    /// tombstone waste the bead is about comes straight back — silently.
    /// Exercised across several tree shapes, both split axes, and a
    /// sidebar/status-bar inset content rect.
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
            two_pane_h(),
            two_pane_v(),
            three_pane_mixed(),
        ] {
            for dir in [SplitDir::Horizontal, SplitDir::Vertical] {
                let pending = PendingSplit {
                    focused_at_request: state.focus.clone().expect("focus"),
                    dir,
                    zoom_on_spawn: false,
                };
                let predicted =
                    predicted_spawn_dims(&state, &pending, content).expect("split is predictable");
                // What actually happens one round trip later: the server
                // allocates an id, `apply_spawned_ok` folds it in, and the
                // driver tiles the result into the same content rect.
                let landed = apply_spawned_ok(&state, t(77), &pending).expect("split applies");
                let actual =
                    crate::multi_pane::pane_rects_in(landed.tree.as_ref().expect("tree"), content)
                        [&t(77)];
                assert_eq!(
                    predicted,
                    (actual.w, actual.h),
                    "prediction diverged from the reflow for {dir:?} on {:?}",
                    crate::layout::leaves(state.tree.as_ref().expect("tree")),
                );
            }
        }
    }

    /// A `zoom_on_spawn` split (ADR-0019 `placement = "zoomed"` plugin
    /// panes) is zoomed the instant it lands, and a zoomed pane renders as a
    /// lone full-content leaf — so the split's own tile never reaches the
    /// PTY and the prediction is the content rect itself.
    #[test]
    fn predicted_spawn_dims_for_a_zoomed_spawn_are_the_whole_content_rect() {
        let content = Rect {
            x: 0,
            y: 0,
            w: 100,
            h: 30,
        };
        let state = three_pane_mixed();
        let pending = PendingSplit {
            focused_at_request: state.focus.clone().expect("focus"),
            dir: SplitDir::Horizontal,
            zoom_on_spawn: true,
        };
        assert_eq!(
            predicted_spawn_dims(&state, &pending, content),
            Some((100, 30)),
        );
    }

    /// An empty workspace has no leaf to split against, so there is nothing
    /// to predict. The caller must then omit the wire field rather than
    /// invent a size — the server's default plus the follow-up resize is the
    /// correct degradation.
    #[test]
    fn predicted_spawn_dims_are_none_without_a_tree_to_split() {
        let pending = PendingSplit {
            focused_at_request: t(1),
            dir: SplitDir::Horizontal,
            zoom_on_spawn: false,
        };
        assert_eq!(
            predicted_spawn_dims(
                &LayoutState::default(),
                &pending,
                Rect {
                    x: 0,
                    y: 0,
                    w: 80,
                    h: 24,
                },
            ),
            None,
        );
    }

    /// The placeholder must never collide with a live leaf, or the split
    /// would attach to the wrong node and the prediction would be a lie.
    #[test]
    fn unused_leaf_id_avoids_live_leaves() {
        let state = LayoutState {
            tree: Some(
                split_at(
                    &LayoutNode::Leaf(t(u32::MAX)),
                    &t(u32::MAX),
                    &t(u32::MAX - 1),
                    SplitDir::Horizontal,
                    0.5,
                )
                .expect("split"),
            ),
            focus: Some(t(u32::MAX)),
        };
        let id = unused_leaf_id(&state);
        assert!(!crate::layout::leaves(state.tree.as_ref().expect("tree")).contains(&id));
    }

    #[test]
    fn apply_spawned_ok_splits_anchored_to_focused_at_request() {
        // Single pane focused on 1; pending split adds pane 2.
        let state = LayoutState::single(t(1));
        let pending = PendingSplit {
            focused_at_request: t(1),
            dir: SplitDir::Horizontal,
            zoom_on_spawn: false,
        };
        let new_state = apply_spawned_ok(&state, t(2), &pending).expect("split applies");
        // apply_split sets focus to the freshly added pane.
        assert_eq!(new_state.focus, Some(t(2)));
        let leaves = crate::layout::leaves(new_state.tree.as_ref().expect("tree"));
        assert_eq!(leaves, vec![t(1), t(2)]);
    }

    #[test]
    fn apply_spawned_ok_anchors_against_request_even_when_focus_moved() {
        // ((1|2)/3), focus moved to 3 by the time the spawn reply lands,
        // but the user's chord targeted pane 2 — verify the split lands
        // adjacent to 2 (not to the live focus).
        let t1 = split_at(
            &LayoutNode::Leaf(t(1)),
            &t(1),
            &t(2),
            SplitDir::Horizontal,
            0.5,
        )
        .expect("split 1+2");
        let tree = split_at(&t1, &t(2), &t(3), SplitDir::Vertical, 0.5).expect("split 2+3");
        let state = LayoutState {
            tree: Some(tree),
            focus: Some(t(3)),
        };
        let pending = PendingSplit {
            focused_at_request: t(2),
            dir: SplitDir::Horizontal,
            zoom_on_spawn: false,
        };
        let new_state =
            apply_spawned_ok(&state, t(99), &pending).expect("split applies against request");
        let leaves = crate::layout::leaves(new_state.tree.as_ref().expect("tree"));
        // 99 should be sibling-adjacent to 2, leaves contains all 4.
        assert!(leaves.contains(&t(99)), "new pane not in tree: {leaves:?}");
        assert!(leaves.contains(&t(2)), "anchor pane gone: {leaves:?}");
        assert!(leaves.contains(&t(1)));
        assert!(leaves.contains(&t(3)));
        assert_eq!(new_state.focus, Some(t(99)));
    }

    #[test]
    fn apply_spawned_ok_falls_back_to_live_focus_when_anchor_gone() {
        // Pane 1 in tree, focus on 1, pending intent named pane 42 (no
        // longer exists). Expect split anchored to 1 (live focus).
        let state = LayoutState::single(t(1));
        let pending = PendingSplit {
            focused_at_request: t(42),
            dir: SplitDir::Vertical,
            zoom_on_spawn: false,
        };
        let new_state = apply_spawned_ok(&state, t(2), &pending).expect("split applies");
        let leaves = crate::layout::leaves(new_state.tree.as_ref().expect("tree"));
        assert_eq!(leaves, vec![t(1), t(2)]);
    }

    #[test]
    fn apply_terminal_closed_folds_out_known_leaf() {
        // (1|2), kill 1 → tree collapses to leaf(2), focus = 2.
        let tree = split_at(
            &LayoutNode::Leaf(t(1)),
            &t(1),
            &t(2),
            SplitDir::Horizontal,
            0.5,
        )
        .expect("split");
        let state = LayoutState {
            tree: Some(tree),
            focus: Some(t(2)),
        };
        let new_state = apply_terminal_closed(&state, &t(1)).expect("fold succeeds");
        assert!(matches!(
            new_state.tree.as_ref().expect("tree"),
            LayoutNode::Leaf(p) if *p == t(2)
        ));
        // apply_kill sets focus to the first DFS leaf in the surviving
        // tree (here the only remaining leaf, 2).
        assert_eq!(new_state.focus, Some(t(2)));
    }

    #[test]
    fn apply_terminal_closed_emptied_state_when_last_leaf_dies() {
        let state = LayoutState::single(t(1));
        let new_state = apply_terminal_closed(&state, &t(1)).expect("fold succeeds");
        assert!(new_state.tree.is_none());
        assert!(new_state.focus.is_none());
    }

    #[test]
    fn apply_terminal_closed_rejects_unknown_leaf() {
        let state = LayoutState::single(t(1));
        let err = apply_terminal_closed(&state, &t(99)).unwrap_err();
        // PaneNotInLayout — driver bubbles a debug log + drops PaneSlot.
        assert!(
            matches!(err, ActionError::Layout(_)),
            "expected Layout error, got {err:?}"
        );
    }

    /// Invariant: any sequence of (split, close) operations preserves
    /// `leaves = (splits - closes + 1)` so long as the tree is
    /// non-empty after each step. Not a true proptest (we drive the
    /// pure helpers directly with deterministic ids), but exercises
    /// the same algebra phux-4li.5's per-action tests guarantee.
    #[test]
    #[allow(clippy::cast_possible_wrap, reason = "leaf counts are tiny")]
    fn split_close_sequence_preserves_leaf_count() {
        let mut state = LayoutState::single(t(1));
        let mut splits: i64 = 0;
        let mut closes: i64 = 0;

        // Three splits → 4 leaves.
        for (next_id, dir) in (2_u32..).zip([
            SplitDir::Horizontal,
            SplitDir::Vertical,
            SplitDir::Horizontal,
        ]) {
            let pending = PendingSplit {
                focused_at_request: state.focus.clone().expect("focus"),
                dir,
                zoom_on_spawn: false,
            };
            state = apply_spawned_ok(&state, t(next_id), &pending).expect("split");
            splits += 1;
            let leaf_count = crate::layout::leaves(state.tree.as_ref().expect("tree")).len() as i64;
            assert_eq!(leaf_count, splits - closes + 1);
        }

        // Two closes → 2 leaves.
        for _ in 0..2 {
            let dying = crate::layout::leaves(state.tree.as_ref().expect("tree"))[0].clone();
            state = apply_terminal_closed(&state, &dying).expect("close");
            closes += 1;
            let leaf_count = crate::layout::leaves(state.tree.as_ref().expect("tree")).len() as i64;
            assert_eq!(leaf_count, splits - closes + 1);
        }
    }
}
