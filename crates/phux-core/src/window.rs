//! [`Window`] — a session's tab-like container of panes.
//!
//! The layout is a binary split tree ([`LayoutNode`]). Each interior
//! [`LayoutNode::Split`] divides its rectangle along one axis at a `ratio`;
//! each [`LayoutNode::Leaf`] is a single pane. The tree is the auxiliary
//! structure; [`Window::slots`] remains the insertion-ordered source of
//! truth for which Terminal-kind resources are in the window.
//!
//! Spec ref: `docs/spec/L3.md` §3.2 Layout (binary subset). Pane-rect
//! tiling and directional focus live client-side in `phux-client-core`;
//! do not reintroduce parallel implementations here.

use thiserror::Error;

use crate::ids::{ResourceId, SessionId, WindowId};

/// A window: an ordered collection of layout slots belonging to a session.
/// `slots` is the source of truth; `layout` mirrors it and the
/// [`Registry`](crate::registry::Registry) keeps the two in sync.
#[derive(Debug, Clone)]
pub struct Window {
    /// The stable identifier issued by the registry.
    pub id: WindowId,
    /// The session that owns this window.
    pub session: SessionId,
    /// Terminal-kind resources occupying this window, in insertion order.
    pub slots: Vec<ResourceId>,
    /// The pane layout as a binary split tree, or `None` when no panes exist.
    pub layout: Option<LayoutNode>,
    /// The currently focused pane, if any.
    pub active: Option<ResourceId>,
}

/// A node in the binary split tree.
///
/// A `Leaf` holds a single [`ResourceId`]; a `Split` divides its rectangle
/// between two children along [`SplitDir`] at `ratio` (the left/top child
/// gets `ratio` of the parent's dimension along the split axis).
#[derive(Debug, Clone, PartialEq)]
pub enum LayoutNode {
    /// A single pane — the recursion base.
    Leaf(ResourceId),
    /// An interior node that splits its rectangle in two.
    Split {
        /// The axis the split is taken along.
        dir: SplitDir,
        /// Fraction of the parent dim given to `left`, in the open interval
        /// `(0.0, 1.0)` (ADR-0012). Enforced by [`Window::split`], not the
        /// type: hand-built trees own that check.
        ratio: f32,
        /// Left (for [`SplitDir::Horizontal`]) or top (for [`SplitDir::Vertical`]) child.
        left: Box<Self>,
        /// Right (for [`SplitDir::Horizontal`]) or bottom (for [`SplitDir::Vertical`]) child.
        right: Box<Self>,
    },
}

/// Axis along which a [`LayoutNode::Split`] divides its rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDir {
    /// Split side-by-side (a vertical bar between left and right).
    Horizontal,
    /// Split stacked (a horizontal bar between top and bottom).
    Vertical,
}

/// Errors returned by layout operations on a [`Window`].
#[derive(Debug, Clone, Copy, PartialEq, Error)]
pub enum LayoutError {
    /// The target [`ResourceId`] is not present in this window's layout.
    #[error("pane not in layout: {0:?}")]
    PaneNotInLayout(ResourceId),
    /// The requested split ratio is outside the half-open `(0.0, 1.0)` range,
    /// or is NaN.
    #[error("invalid split ratio: {0}")]
    InvalidRatio(f32),
    /// The layout has only one pane — `kill_pane` would empty the window.
    /// Callers may choose to remove the window itself instead.
    #[error("cannot kill the last pane in the layout")]
    LastPane,
}

impl LayoutNode {
    /// Return `true` if this subtree contains a [`Leaf`] for `pane`.
    ///
    /// [`Leaf`]: LayoutNode::Leaf
    #[must_use]
    pub fn contains(&self, pane: ResourceId) -> bool {
        match self {
            Self::Leaf(p) => *p == pane,
            Self::Split { left, right, .. } => left.contains(pane) || right.contains(pane),
        }
    }

    /// Collect every [`ResourceId`] in this subtree in left-to-right traversal order.
    #[must_use]
    pub fn leaves(&self) -> Vec<ResourceId> {
        let mut out = Vec::new();
        self.collect_leaves(&mut out);
        out
    }

    fn collect_leaves(&self, out: &mut Vec<ResourceId>) {
        match self {
            Self::Leaf(p) => out.push(*p),
            Self::Split { left, right, .. } => {
                left.collect_leaves(out);
                right.collect_leaves(out);
            }
        }
    }

    /// Split the [`Leaf`] for `target` into a [`Split`] whose `left` keeps
    /// `target` and whose `right` is a new [`Leaf`] for `new_pane`.
    ///
    /// Returns `Ok(())` if `target` was found and replaced, or
    /// [`LayoutError::PaneNotInLayout`] otherwise.
    ///
    /// [`Leaf`]: LayoutNode::Leaf
    /// [`Split`]: LayoutNode::Split
    fn split_at(
        &mut self,
        target: ResourceId,
        new_pane: ResourceId,
        dir: SplitDir,
        ratio: f32,
    ) -> Result<(), LayoutError> {
        match self {
            Self::Leaf(p) if *p == target => {
                *self = Self::Split {
                    dir,
                    ratio,
                    left: Box::new(Self::Leaf(target)),
                    right: Box::new(Self::Leaf(new_pane)),
                };
                Ok(())
            }
            Self::Leaf(_) => Err(LayoutError::PaneNotInLayout(target)),
            Self::Split { left, right, .. } => {
                if left.contains(target) {
                    left.split_at(target, new_pane, dir, ratio)
                } else if right.contains(target) {
                    right.split_at(target, new_pane, dir, ratio)
                } else {
                    Err(LayoutError::PaneNotInLayout(target))
                }
            }
        }
    }
}

fn validate_ratio(ratio: f32) -> Result<(), LayoutError> {
    if ratio.is_nan() || ratio <= 0.0 || ratio >= 1.0 {
        Err(LayoutError::InvalidRatio(ratio))
    } else {
        Ok(())
    }
}

impl Window {
    /// Split the leaf for `target` into two, placing `new_pane` as the new
    /// sibling along `dir` with the given `ratio`.
    ///
    /// On success the layout grows by one [`Leaf`](LayoutNode::Leaf) and one
    /// [`Split`](LayoutNode::Split); `target` and `new_pane` are siblings.
    ///
    /// # Errors
    /// * [`LayoutError::PaneNotInLayout`] if `target` is not present.
    /// * [`LayoutError::InvalidRatio`] if `ratio` is NaN or outside `(0, 1)`.
    pub fn split(
        &mut self,
        target: ResourceId,
        new_pane: ResourceId,
        dir: SplitDir,
        ratio: f32,
    ) -> Result<(), LayoutError> {
        validate_ratio(ratio)?;
        let layout = self
            .layout
            .as_mut()
            .ok_or(LayoutError::PaneNotInLayout(target))?;
        layout.split_at(target, new_pane, dir, ratio)
    }

    /// Initialize the layout with `pane` as the sole [`Leaf`](LayoutNode::Leaf).
    ///
    /// Idempotent only when the layout is currently empty; if the window
    /// already has a layout this returns [`LayoutError::PaneNotInLayout`]
    /// (the caller should use [`Window::split`] instead).
    ///
    /// # Errors
    /// Returns [`LayoutError::PaneNotInLayout`] if the layout is already
    /// initialized — a guard against silently clobbering the tree.
    pub fn seed_layout(&mut self, pane: ResourceId) -> Result<(), LayoutError> {
        if self.layout.is_some() {
            return Err(LayoutError::PaneNotInLayout(pane));
        }
        self.layout = Some(LayoutNode::Leaf(pane));
        Ok(())
    }

    /// Remove the leaf for `target` from the layout, collapsing its parent
    /// [`Split`](LayoutNode::Split) so the remaining sibling takes its
    /// grandparent's slot.
    ///
    /// # Errors
    /// * [`LayoutError::PaneNotInLayout`] if `target` is not present.
    /// * [`LayoutError::LastPane`] if `target` is the only leaf — the caller
    ///   must remove the whole window.
    pub fn kill_pane(&mut self, target: ResourceId) -> Result<(), LayoutError> {
        let Some(layout) = self.layout.as_mut() else {
            return Err(LayoutError::PaneNotInLayout(target));
        };
        match layout {
            LayoutNode::Leaf(p) if *p == target => {
                self.layout = None;
                Err(LayoutError::LastPane)
            }
            LayoutNode::Leaf(_) => Err(LayoutError::PaneNotInLayout(target)),
            LayoutNode::Split { .. } => {
                // Replace `layout` with the collapsed subtree.
                let Some(owned) = self.layout.take() else {
                    // Unreachable: we matched Some(Split{..}) above.
                    return Err(LayoutError::PaneNotInLayout(target));
                };
                let (new_root, found) = collapse(owned, target);
                self.layout = Some(new_root);
                if found {
                    Ok(())
                } else {
                    Err(LayoutError::PaneNotInLayout(target))
                }
            }
        }
    }
}

/// Walk `node`, removing the leaf for `target`, collapsing the parent Split
/// so the sibling takes its place. Returns the rewritten tree and whether
/// `target` was found.
fn collapse(node: LayoutNode, target: ResourceId) -> (LayoutNode, bool) {
    match node {
        LayoutNode::Leaf(p) => (LayoutNode::Leaf(p), false),
        LayoutNode::Split {
            dir,
            ratio,
            left,
            right,
        } => {
            // If either direct child is the target leaf, collapse to the sibling.
            if let LayoutNode::Leaf(p) = *left
                && p == target
            {
                return (*right, true);
            }
            if let LayoutNode::Leaf(p) = *right
                && p == target
            {
                return (*left, true);
            }
            // Otherwise recurse.
            let (new_left, found_l) = collapse(*left, target);
            if found_l {
                return (
                    LayoutNode::Split {
                        dir,
                        ratio,
                        left: Box::new(new_left),
                        right,
                    },
                    true,
                );
            }
            let (new_right, found_r) = collapse(*right, target);
            (
                LayoutNode::Split {
                    dir,
                    ratio,
                    left: Box::new(new_left),
                    right: Box::new(new_right),
                },
                found_r,
            )
        }
    }
}
