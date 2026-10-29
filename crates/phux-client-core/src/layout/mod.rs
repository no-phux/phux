//! Client-side mirror of the binary split-tree pane layout (ADR-0019).
//!
//! The tree is [`LayoutNode`], a consumer convention rather than a wire type
//! (ADR-0030); the operations are free functions over it. Tiling lives in [`crate::multi_pane::pane_rects`],
//! the same walk paint uses. The whole [`ArrangementDoc`] persists as the v3
//! CBOR envelope under L3 key `phux.tui.layout/v1` (docs/organization.md);
//! that codec lives in [`legacy`]. Focus fields are non-authoritative
//! (ADR-0049).

use std::borrow::Cow;

use phux_protocol::ResourceId;
use thiserror::Error;

/// Axis along which a [`LayoutNode::Split`] divides its rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDir {
    /// Split side-by-side (a vertical bar between left and right).
    Horizontal,
    /// Split stacked (a horizontal bar between top and bottom).
    Vertical,
}

/// Binary split tree of a window's panes (ADR-0012); `Split` gives its
/// left/top child `ratio` of the parent along [`SplitDir`]. It lives in L3
/// metadata (docs/organization.md), never on the wire.
#[derive(Debug, Clone, PartialEq)]
pub enum LayoutNode {
    /// A single pane — recursion base.
    Leaf(ResourceId),
    /// An interior node that splits its rectangle in two.
    Split {
        /// The axis the split is taken along.
        dir: SplitDir,
        /// Fraction given to `left`, in the closed interval `0.0..=1.0`; the
        /// endpoints are admitted because clients bank unapplied resize
        /// ratios (ADR-0048).
        ratio: f32,
        /// Left (for [`SplitDir::Horizontal`]) or top (for [`SplitDir::Vertical`]) child.
        left: Box<Self>,
        /// Right (for [`SplitDir::Horizontal`]) or bottom (for [`SplitDir::Vertical`]) child.
        right: Box<Self>,
    },
}

/// Cardinal direction for [`focus_direction`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Move focus upward.
    Up,
    /// Move focus downward.
    Down,
    /// Move focus left.
    Left,
    /// Move focus right.
    Right,
}

/// An axis-aligned rectangle in cell coordinates, origin at the outer
/// viewport's top-left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    /// Top-left x coordinate (column).
    pub x: u16,
    /// Top-left y coordinate (row).
    pub y: u16,
    /// Width in cells.
    pub w: u16,
    /// Height in cells.
    pub h: u16,
}

/// One step down the binary split tree: into the `left` or `right` child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NodeStep {
    /// Descend into the `left` child of a [`LayoutNode::Split`].
    Left,
    /// Descend into the `right` child.
    Right,
}

/// A path from the layout root to a [`LayoutNode`] (empty = root). Divider
/// cells carry the path of their split so a drag can find its `ratio`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NodePath(pub(crate) Vec<NodeStep>);

impl NodePath {
    /// The root path (empty).
    #[must_use]
    pub const fn root() -> Self {
        Self(Vec::new())
    }

    /// Push a step onto the path (descend one level).
    pub(crate) fn push(&mut self, step: NodeStep) {
        self.0.push(step);
    }

    /// Pop the last step (ascend one level).
    pub(crate) fn pop(&mut self) -> Option<NodeStep> {
        self.0.pop()
    }
}

/// Errors returned by the free-function layout operations.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum LayoutError {
    /// The target [`ResourceId`] is not present in the tree.
    #[error("pane not in layout: {0:?}")]
    PaneNotInLayout(ResourceId),
    /// The requested split ratio is outside `(0.0, 1.0)`, or is NaN.
    #[error("invalid split ratio: {0}")]
    InvalidRatio(f32),
    /// Closing this pane would leave the workspace with no panes.
    #[error("cannot close the final pane in a persisted layout")]
    LastPane,
}

/// One window's split tree plus this client's focused leaf. Focus is
/// per-client (ADR-0019); peers ignore a sender's focus (ADR-0049).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LayoutState {
    /// The binary split tree. `None` until the first pane is seeded.
    pub tree: Option<LayoutNode>,
    /// The client-local focused leaf. `None` until the first pane is
    /// seeded; reset to `None` if the tree becomes empty.
    pub focus: Option<ResourceId>,
}

impl LayoutState {
    /// Construct an empty state — no tree, no focus.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            tree: None,
            focus: None,
        }
    }

    /// Construct a state with a single leaf and matching focus.
    #[must_use]
    pub fn single(pane: ResourceId) -> Self {
        let focus = pane.clone();
        Self {
            tree: Some(LayoutNode::Leaf(pane)),
            focus: Some(focus),
        }
    }
}

/// One group of an arrangement (a TUI tab): a stable identity, a name, and
/// its pane layout.
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    /// Random 16-byte identity, minted when the group is created and kept
    /// for as long as it exists, whatever its tree holds.
    pub id: [u8; 16],
    /// Display name, shown in the tab bar.
    pub name: String,
    /// This group's pane layout and per-client focus.
    pub state: LayoutState,
}

impl Group {
    /// A new group under a freshly minted random identity.
    #[must_use]
    pub fn new(name: String, state: LayoutState) -> Self {
        Self::with_id(identity::fresh_id(), name, state)
    }

    /// A group under an identity it already has (decoded, or read from the
    /// server's arrangement).
    #[must_use]
    pub const fn with_id(id: [u8; 16], name: String, state: LayoutState) -> Self {
        Self { id, name, state }
    }
}

mod identity;
pub mod legacy;
mod projection;
pub mod wire;

pub use identity::fresh_id;

pub use legacy::{LayoutDecodeError, LayoutEncodeError};

pub use projection::{
    LAYOUT_KEY, LAYOUT_METADATA_GROUP, MAX_LAYOUT_METADATA_BYTES, layout_key,
    projection_key_session,
};

mod arrangement;
mod mutation;
mod ops;

pub use arrangement::ArrangementDoc;
pub use mutation::{LayoutMutation, MutationError, adopt_unarranged_groups, apply_mutation};
use ops::shifted_index;
pub use ops::{focus_direction, kill_pane, leaves, repair_focus, set_ratio_at, split_at};

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::float_cmp)]
mod tests;
