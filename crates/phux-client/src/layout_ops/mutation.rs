use phux_protocol::ids::ResourceId;

use crate::layout::{
    ArrangementDoc, LayoutError, LayoutNode, SplitDir, kill_pane, leaves, repair_focus, split_at,
};

use super::{LayoutMutation, LayoutOpsError};

/// Apply one mutation without doing I/O.
///
/// # Errors
///
/// Rejects missing/duplicate targets, invalid ratios, same-pane operations,
/// and closing the final pane.
pub fn apply_mutation(
    workspace: &mut ArrangementDoc,
    mutation: &LayoutMutation,
) -> Result<(), LayoutOpsError> {
    match mutation {
        LayoutMutation::Split {
            target,
            new_pane,
            dir,
            ratio,
        } => apply_split(workspace, target, new_pane, *dir, *ratio, true),
        LayoutMutation::SplitPreservingFocus {
            target,
            new_pane,
            dir,
            ratio,
        } => apply_split(workspace, target, new_pane, *dir, *ratio, false),
        LayoutMutation::Move {
            source,
            target,
            dir,
            ratio,
        } => apply_move(workspace, source, target, *dir, *ratio),
        LayoutMutation::Swap { first, second } => apply_swap(workspace, first, second),
        LayoutMutation::Close { target } => apply_close(workspace, target),
    }
}

/// The tree of `workspace.windows[index]`, blaming `blame` when the window
/// carries no tree at all.
fn window_tree<'a>(
    workspace: &'a ArrangementDoc,
    index: usize,
    blame: &ResourceId,
) -> Result<&'a LayoutNode, LayoutOpsError> {
    workspace.groups[index]
        .state
        .tree
        .as_ref()
        .ok_or_else(|| LayoutOpsError::ForeignTarget(blame.clone()))
}

/// The index of the window holding `target`, or [`LayoutOpsError::ForeignTarget`].
fn require_window(
    workspace: &ArrangementDoc,
    target: &ResourceId,
) -> Result<usize, LayoutOpsError> {
    find_window(workspace, target).ok_or_else(|| LayoutOpsError::ForeignTarget(target.clone()))
}

/// Split `target` to make room for `new_pane`, moving focus to the new pane
/// only when `focus_new_pane` is set.
fn apply_split(
    workspace: &mut ArrangementDoc,
    target: &ResourceId,
    new_pane: &ResourceId,
    dir: SplitDir,
    ratio: f32,
    focus_new_pane: bool,
) -> Result<(), LayoutOpsError> {
    if find_window(workspace, new_pane).is_some() {
        return Err(LayoutOpsError::DuplicatePane(new_pane.clone()));
    }
    let index = require_window(workspace, target)?;
    let tree = window_tree(workspace, index, target)?;
    workspace.groups[index].state.tree = Some(split_at(tree, target, new_pane, dir, ratio)?);
    if focus_new_pane {
        workspace.groups[index].state.focus = Some(new_pane.clone());
        workspace.active = index;
    }
    Ok(())
}

/// Exchange the leaf positions of `first` and `second` across every window.
fn apply_swap(
    workspace: &mut ArrangementDoc,
    first: &ResourceId,
    second: &ResourceId,
) -> Result<(), LayoutOpsError> {
    if first == second {
        return Err(LayoutOpsError::SamePane);
    }
    require_window(workspace, first)?;
    require_window(workspace, second)?;
    for window in &mut workspace.groups {
        if let Some(tree) = window.state.tree.as_ref() {
            window.state.tree = Some(swap_leaves(tree, first, second)?);
        }
    }
    // Focus follows Terminal identity, not physical leaf position.
    Ok(())
}

/// Remove `target`, refusing to close the workspace's final pane.
fn apply_close(workspace: &mut ArrangementDoc, target: &ResourceId) -> Result<(), LayoutOpsError> {
    workspace.close_pane(target).map_err(|err| match err {
        LayoutError::LastPane => LayoutOpsError::LastPane,
        LayoutError::PaneNotInLayout(id) => LayoutOpsError::ForeignTarget(id),
        err @ LayoutError::InvalidRatio(_) => LayoutOpsError::Layout(err),
    })
}

fn apply_move(
    workspace: &mut ArrangementDoc,
    source: &ResourceId,
    target: &ResourceId,
    dir: SplitDir,
    ratio: f32,
) -> Result<(), LayoutOpsError> {
    if source == target {
        return Err(LayoutOpsError::SamePane);
    }
    let source_index = require_window(workspace, source)?;
    let target_index = require_window(workspace, target)?;
    // Validate the destination and ratio before collapsing the source so the
    // public pure helper is transactional on ordinary validation errors.
    let target_tree = window_tree(workspace, target_index, target)?;
    let _ = split_at(target_tree, target, source, dir, ratio)?;

    if source_index == target_index {
        move_within_window(workspace, source_index, source, target, dir, ratio)
    } else {
        move_across_windows(
            workspace,
            (source_index, target_index),
            source,
            target,
            dir,
            ratio,
        )
    }
}

/// Re-place `source` next to `target` inside the single window holding both:
/// collapse the source leaf first, then split the collapsed tree.
fn move_within_window(
    workspace: &mut ArrangementDoc,
    index: usize,
    source: &ResourceId,
    target: &ResourceId,
    dir: SplitDir,
    ratio: f32,
) -> Result<(), LayoutOpsError> {
    let tree = window_tree(workspace, index, source)?;
    let collapsed = kill_pane(tree, source)?.ok_or(LayoutOpsError::LastPane)?;
    let moved = split_at(&collapsed, target, source, dir, ratio)?;
    workspace.groups[index].state.tree = Some(moved);
    workspace.groups[index].state.focus = Some(source.clone());
    workspace.active = index;
    Ok(())
}

/// Move `source` out of its window and into `target`'s, repairing the vacated
/// window's focus and pruning it if it emptied.
fn move_across_windows(
    workspace: &mut ArrangementDoc,
    (source_index, target_index): (usize, usize),
    source: &ResourceId,
    target: &ResourceId,
    dir: SplitDir,
    ratio: f32,
) -> Result<(), LayoutOpsError> {
    let source_tree = window_tree(workspace, source_index, source)?;
    workspace.groups[source_index].state.tree = kill_pane(source_tree, source)?;
    repair_focus(&mut workspace.groups[source_index].state);

    let target_tree = window_tree(workspace, target_index, target)?;
    workspace.groups[target_index].state.tree =
        Some(split_at(target_tree, target, source, dir, ratio)?);
    workspace.groups[target_index].state.focus = Some(source.clone());
    workspace.active = target_index;
    workspace.prune_empty_groups();
    Ok(())
}

/// Append each group of `fallback` none of whose panes `workspace` arranges
/// yet. Leaves `active` alone unless `workspace` had no groups.
pub(super) fn adopt_unarranged_groups(workspace: &mut ArrangementDoc, fallback: &ArrangementDoc) {
    let was_empty = workspace.groups.is_empty();
    for group in &fallback.groups {
        let Some(tree) = group.state.tree.as_ref() else {
            continue;
        };
        if leaves(tree)
            .iter()
            .all(|pane| find_window(workspace, pane).is_none())
        {
            workspace.groups.push(group.clone());
        }
    }
    if was_empty {
        workspace.active = fallback
            .active
            .min(workspace.groups.len().saturating_sub(1));
    }
}

fn find_window(workspace: &ArrangementDoc, target: &ResourceId) -> Option<usize> {
    workspace.groups.iter().position(|window| {
        window
            .state
            .tree
            .as_ref()
            .is_some_and(|tree| leaves(tree).contains(target))
    })
}

fn swap_leaves(
    node: &LayoutNode,
    first: &ResourceId,
    second: &ResourceId,
) -> Result<LayoutNode, LayoutOpsError> {
    match node {
        LayoutNode::Leaf(id) if id == first => Ok(LayoutNode::Leaf(second.clone())),
        LayoutNode::Leaf(id) if id == second => Ok(LayoutNode::Leaf(first.clone())),
        LayoutNode::Leaf(id) => Ok(LayoutNode::Leaf(id.clone())),
        LayoutNode::Split {
            dir,
            ratio,
            left,
            right,
        } => Ok(LayoutNode::Split {
            dir: *dir,
            ratio: *ratio,
            left: Box::new(swap_leaves(left, first, second)?),
            right: Box::new(swap_leaves(right, first, second)?),
        }),
    }
}
