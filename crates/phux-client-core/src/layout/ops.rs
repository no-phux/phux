//! Pure tree operations: split, resize, kill, focus movement, and leaf walks.

use super::{
    Direction, LayoutError, LayoutNode, LayoutState, NodePath, NodeStep, ResourceId, SplitDir,
};

/// Keep `state`'s focus when it is still a leaf, else fall back to the
/// first leaf (or none for an empty tree).
pub(super) fn repair_focus(state: &mut LayoutState) {
    state.focus = state.tree.as_ref().and_then(|tree| {
        let panes = leaves(tree);
        state
            .focus
            .as_ref()
            .filter(|focus| panes.contains(focus))
            .cloned()
            .or_else(|| panes.into_iter().next())
    });
}

/// Where index `i` lands after the element at `from` moves to `to`.
pub(super) const fn shifted_index(i: usize, from: usize, to: usize) -> usize {
    if i == from {
        to
    } else if from < i && i <= to {
        i - 1
    } else if to <= i && i < from {
        i + 1
    } else {
        i
    }
}

/// Split the leaf for `target`: it becomes the `left` child and `new_pane`
/// the `right` child of a new split along `dir` at `ratio`.
///
/// # Errors
/// * [`LayoutError::PaneNotInLayout`] if `target` is not present.
/// * [`LayoutError::InvalidRatio`] if `ratio` is NaN or outside `(0, 1)`.
pub fn split_at(
    tree: &LayoutNode,
    target: &ResourceId,
    new_pane: &ResourceId,
    dir: SplitDir,
    ratio: f32,
) -> Result<LayoutNode, LayoutError> {
    let mut out = tree.clone();
    out.split(target, new_pane.clone(), dir, ratio)?;
    Ok(out)
}

/// Replace the `ratio` of the split addressed by `path` (callers clamp).
/// `None` when `path` no longer addresses a split.
#[must_use]
pub fn set_ratio_at(tree: &LayoutNode, path: &NodePath, ratio: f32) -> Option<LayoutNode> {
    set_ratio_inner(tree, &path.0, ratio)
}

fn set_ratio_inner(node: &LayoutNode, steps: &[NodeStep], ratio: f32) -> Option<LayoutNode> {
    match node {
        LayoutNode::Split {
            dir,
            ratio: r,
            left,
            right,
        } => match steps.split_first() {
            // Path ends here: this is the split to retune.
            None => Some(LayoutNode::Split {
                dir: *dir,
                ratio,
                left: left.clone(),
                right: right.clone(),
            }),
            Some((NodeStep::Left, rest)) => {
                let new_left = set_ratio_inner(left, rest, ratio)?;
                Some(LayoutNode::Split {
                    dir: *dir,
                    ratio: *r,
                    left: Box::new(new_left),
                    right: right.clone(),
                })
            }
            Some((NodeStep::Right, rest)) => {
                let new_right = set_ratio_inner(right, rest, ratio)?;
                Some(LayoutNode::Split {
                    dir: *dir,
                    ratio: *r,
                    left: left.clone(),
                    right: Box::new(new_right),
                })
            }
        },
        // Ran off a leaf (or hit a leaf at the path tip) — the path no
        // longer names a split in this tree.
        LayoutNode::Leaf(_) => None,
    }
}

/// Remove the leaf for `target`, collapsing its parent split. `Ok(None)`
/// when it was the last leaf. Trees hold each pane once; were `target`
/// duplicated, every copy would go.
///
/// # Errors
/// [`LayoutError::PaneNotInLayout`] if `target` is not present.
pub fn kill_pane(
    tree: &LayoutNode,
    target: &ResourceId,
) -> Result<Option<LayoutNode>, LayoutError> {
    let mut slot = Some(tree.clone());
    phux_core::arrangement::remove(&mut slot, target)?;
    Ok(slot)
}

/// The neighbour of `current` in direction `dir`, if any.
#[must_use]
pub fn focus_direction(
    tree: &LayoutNode,
    current: &ResourceId,
    dir: Direction,
) -> Option<ResourceId> {
    let mut path: Vec<(SplitDir, ChildSide)> = Vec::new();
    if !record_path(tree, current, &mut path) {
        return None;
    }
    for i in (0..path.len()).rev() {
        let (split_dir, came_from) = path[i];
        if matches_to_sibling(split_dir, dir, came_from) {
            let sibling = sibling_at_depth(tree, &path, i)?;
            return Some(descend_to_leaf(sibling, dir, &path[i + 1..]));
        }
    }
    None
}

/// Whether `node` holds a leaf for `target`.
#[must_use]
pub fn contains(node: &LayoutNode, target: &ResourceId) -> bool {
    node.contains(target)
}

/// Every leaf of `node` in left-to-right depth-first order.
#[must_use]
pub fn leaves(node: &LayoutNode) -> Vec<ResourceId> {
    node.leaves().into_iter().cloned().collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ChildSide {
    Left,
    Right,
}

fn record_path(
    node: &LayoutNode,
    target: &ResourceId,
    out: &mut Vec<(SplitDir, ChildSide)>,
) -> bool {
    match node {
        LayoutNode::Leaf(p) => p == target,
        LayoutNode::Split {
            dir: sd,
            left,
            right,
            ..
        } => {
            out.push((*sd, ChildSide::Left));
            if record_path(left, target, out) {
                return true;
            }
            out.pop();
            out.push((*sd, ChildSide::Right));
            if record_path(right, target, out) {
                return true;
            }
            out.pop();
            false
        }
    }
}

const fn matches_to_sibling(split: SplitDir, dir: Direction, came_from: ChildSide) -> bool {
    matches!(
        (split, dir, came_from),
        (SplitDir::Horizontal, Direction::Right, ChildSide::Left)
            | (SplitDir::Horizontal, Direction::Left, ChildSide::Right)
            | (SplitDir::Vertical, Direction::Down, ChildSide::Left)
            | (SplitDir::Vertical, Direction::Up, ChildSide::Right)
    )
}

fn sibling_at_depth<'a>(
    root: &'a LayoutNode,
    path: &[(SplitDir, ChildSide)],
    depth: usize,
) -> Option<&'a LayoutNode> {
    let mut cur = root;
    for (_, side) in &path[..depth] {
        let LayoutNode::Split { left, right, .. } = cur else {
            return None;
        };
        cur = match side {
            ChildSide::Left => left,
            ChildSide::Right => right,
        };
    }
    let LayoutNode::Split { left, right, .. } = cur else {
        return None;
    };
    let (_, came_from) = path[depth];
    Some(match came_from {
        ChildSide::Left => right,
        ChildSide::Right => left,
    })
}

fn descend_to_leaf(
    node: &LayoutNode,
    dir: Direction,
    suffix: &[(SplitDir, ChildSide)],
) -> ResourceId {
    let perp = perpendicular_axis(dir);
    let hints: Vec<ChildSide> = suffix
        .iter()
        .filter_map(|(sd, side)| if *sd == perp { Some(*side) } else { None })
        .collect();
    let mut hint_idx = 0;
    let mut cur = node;
    loop {
        match cur {
            LayoutNode::Leaf(p) => return p.clone(),
            LayoutNode::Split {
                dir: sd,
                left,
                right,
                ..
            } => {
                if axis_parallel(*sd, dir) {
                    cur = match dir {
                        Direction::Right | Direction::Down => left,
                        Direction::Left | Direction::Up => right,
                    };
                } else {
                    let side = hints.get(hint_idx).copied().unwrap_or(ChildSide::Left);
                    hint_idx += 1;
                    cur = match side {
                        ChildSide::Left => left,
                        ChildSide::Right => right,
                    };
                }
            }
        }
    }
}

const fn axis_parallel(split: SplitDir, dir: Direction) -> bool {
    matches!(
        (split, dir),
        (SplitDir::Horizontal, Direction::Left | Direction::Right)
            | (SplitDir::Vertical, Direction::Up | Direction::Down)
    )
}

const fn perpendicular_axis(dir: Direction) -> SplitDir {
    match dir {
        Direction::Left | Direction::Right => SplitDir::Vertical,
        Direction::Up | Direction::Down => SplitDir::Horizontal,
    }
}
