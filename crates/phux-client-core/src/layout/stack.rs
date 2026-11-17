//! Stacks: several panes in one slot of the tree, one shown. A stack is
//! never empty; one left with a single pane becomes that pane's leaf.

use super::{LayoutNode, ResourceId};

/// The pane a stack shows: `active`, clamped into `panes`.
#[must_use]
pub(super) fn shown(active: usize, panes: &[ResourceId]) -> Option<&ResourceId> {
    panes.get(active.min(panes.len().saturating_sub(1)))
}

/// `panes` as one slot showing `active` (clamped): `None` for none, a leaf
/// for one, a stack for more.
#[must_use]
pub fn stacked(active: usize, mut panes: Vec<ResourceId>) -> Option<LayoutNode> {
    match panes.len() {
        0 => None,
        1 => panes.pop().map(LayoutNode::Leaf),
        n => Some(LayoutNode::Stack {
            active: active.min(n - 1),
            panes,
        }),
    }
}

/// The stack `node` is, without `target`: the shown pane stays shown, and
/// when it was `target` the pane after it shows.
pub(super) fn without(node: &LayoutNode, target: &ResourceId) -> LayoutNode {
    let LayoutNode::Stack { active, panes } = node else {
        return node.clone();
    };
    let at = panes.iter().position(|p| p == target);
    let active = match at {
        Some(at) if at < *active => active - 1,
        _ => *active,
    };
    let kept = panes.iter().filter(|p| *p != target).cloned().collect();
    stacked(active, kept).unwrap_or_else(|| node.clone())
}

/// Every leaf `node` shows, in [`super::leaves`] order: a stack's hidden
/// panes are skipped.
#[must_use]
pub fn shown_leaves(node: &LayoutNode) -> Vec<ResourceId> {
    match node {
        LayoutNode::Leaf(p) => vec![p.clone()],
        LayoutNode::Split { left, right, .. } => {
            let mut out = shown_leaves(left);
            out.extend(shown_leaves(right));
            out
        }
        LayoutNode::Stack { active, panes } => shown(*active, panes).into_iter().cloned().collect(),
    }
}

/// The stack holding `pane`, as its shown index and its panes; `None` when
/// `pane` isn't stacked.
#[must_use]
pub fn stack_of<'a>(node: &'a LayoutNode, pane: &ResourceId) -> Option<(usize, &'a [ResourceId])> {
    match node {
        LayoutNode::Leaf(_) => None,
        LayoutNode::Split { left, right, .. } => {
            stack_of(left, pane).or_else(|| stack_of(right, pane))
        }
        LayoutNode::Stack { active, panes } => {
            panes.contains(pane).then_some((*active, panes.as_slice()))
        }
    }
}

/// `node` with the stack holding `pane` showing it; unchanged when `pane`
/// isn't stacked.
#[must_use]
pub fn reveal(node: &LayoutNode, pane: &ResourceId) -> LayoutNode {
    match node {
        LayoutNode::Leaf(_) => node.clone(),
        LayoutNode::Split {
            dir,
            ratio,
            left,
            right,
        } => LayoutNode::Split {
            dir: *dir,
            ratio: *ratio,
            left: Box::new(reveal(left, pane)),
            right: Box::new(reveal(right, pane)),
        },
        LayoutNode::Stack { active, panes } => LayoutNode::Stack {
            active: panes.iter().position(|p| p == pane).unwrap_or(*active),
            panes: panes.clone(),
        },
    }
}

/// `node` with `pane` stacked onto `onto`'s slot and shown; `pane` must not
/// already be in `node`. Unchanged when `onto` isn't in it.
pub(super) fn push(node: &LayoutNode, onto: &ResourceId, pane: &ResourceId) -> LayoutNode {
    match node {
        LayoutNode::Leaf(p) if p == onto => LayoutNode::Stack {
            active: 1,
            panes: vec![p.clone(), pane.clone()],
        },
        LayoutNode::Stack { panes, .. } if panes.contains(onto) => {
            let mut panes = panes.clone();
            panes.push(pane.clone());
            LayoutNode::Stack {
                active: panes.len() - 1,
                panes,
            }
        }
        LayoutNode::Leaf(_) | LayoutNode::Stack { .. } => node.clone(),
        LayoutNode::Split {
            dir,
            ratio,
            left,
            right,
        } => LayoutNode::Split {
            dir: *dir,
            ratio: *ratio,
            left: Box::new(push(left, onto, pane)),
            right: Box::new(push(right, onto, pane)),
        },
    }
}
