//! [`Arrangement`]: a shared layout document over resources, and the binary
//! split tree ([`LayoutNode`]) each of its groups holds.
//!
//! An arrangement is presentation, never membership: it names resources
//! through [`EntryRef`] leaves and a session's arrangement may name only
//! that session's members. Clients edit trees in their own leaf type (live
//! handles) and translate to [`EntryRef`] leaves with [`LayoutNode::map`].
//!
//! Spec ref: `docs/spec/L3.md` §3.2 Layout (binary subset). Pane-rect
//! tiling and directional focus live client-side in `phux-client-core`.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::keys::{ArrangementKey, ResourceKey, Revision, SessionKey, WorkspaceKey};

/// A node in a binary split tree whose leaves are `L`.
///
/// A `Leaf` holds one `L`; a `Split` divides its rectangle between two
/// children along [`SplitDir`] at `ratio` (the left or top child gets
/// `ratio` of the parent's dimension along the split axis).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutNode<L> {
    /// A single view: the recursion base.
    Leaf(L),
    /// An interior node that splits its rectangle in two.
    Split {
        /// The axis the split is taken along.
        dir: SplitDir,
        /// Fraction of the parent dimension given to `left`, in the open
        /// interval `(0.0, 1.0)` (ADR-0012). [`LayoutNode::split`] and
        /// [`LayoutNode::validate_ratios`] enforce it; the type does not.
        ratio: f32,
        /// Left (for [`SplitDir::Horizontal`]) or top child.
        left: Box<Self>,
        /// Right (for [`SplitDir::Horizontal`]) or bottom child.
        right: Box<Self>,
    },
}

/// Axis along which a [`LayoutNode::Split`] divides its rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitDir {
    /// Side by side (a vertical bar between left and right).
    Horizontal,
    /// Stacked (a horizontal bar between top and bottom).
    Vertical,
}

/// Errors from editing a [`LayoutNode`] tree.
#[derive(Debug, Clone, Copy, PartialEq, Error)]
pub enum LayoutError<L> {
    /// The target leaf is not in the tree.
    #[error("leaf not in layout: {0:?}")]
    PaneNotInLayout(L),
    /// A split ratio is NaN or outside the open interval `(0.0, 1.0)`.
    #[error("invalid split ratio: {0}")]
    InvalidRatio(f32),
}

/// Check that `ratio` lies in the open interval `(0.0, 1.0)`.
///
/// # Errors
///
/// [`LayoutError::InvalidRatio`] for NaN or an out-of-range value.
pub fn validate_ratio<L: std::fmt::Debug>(ratio: f32) -> Result<(), LayoutError<L>> {
    if ratio.is_nan() || ratio <= 0.0 || ratio >= 1.0 {
        Err(LayoutError::InvalidRatio(ratio))
    } else {
        Ok(())
    }
}

impl<L> LayoutNode<L> {
    /// Borrow every leaf in left-to-right (depth-first) order.
    #[must_use]
    pub fn leaves(&self) -> Vec<&L> {
        let mut out = Vec::new();
        self.collect_leaves(&mut out);
        out
    }

    fn collect_leaves<'a>(&'a self, out: &mut Vec<&'a L>) {
        match self {
            Self::Leaf(l) => out.push(l),
            Self::Split { left, right, .. } => {
                left.collect_leaves(out);
                right.collect_leaves(out);
            }
        }
    }

    /// The same tree shape with every leaf passed through `f`.
    #[must_use]
    pub fn map<M>(&self, f: &mut impl FnMut(&L) -> M) -> LayoutNode<M> {
        match self {
            Self::Leaf(l) => LayoutNode::Leaf(f(l)),
            Self::Split {
                dir,
                ratio,
                left,
                right,
            } => LayoutNode::Split {
                dir: *dir,
                ratio: *ratio,
                left: Box::new(left.map(f)),
                right: Box::new(right.map(f)),
            },
        }
    }

    /// Like [`Self::map`], stopping at the first leaf `f` rejects.
    ///
    /// # Errors
    ///
    /// The first error `f` returns.
    pub fn try_map<M, E>(
        &self,
        f: &mut impl FnMut(&L) -> Result<M, E>,
    ) -> Result<LayoutNode<M>, E> {
        Ok(match self {
            Self::Leaf(l) => LayoutNode::Leaf(f(l)?),
            Self::Split {
                dir,
                ratio,
                left,
                right,
            } => LayoutNode::Split {
                dir: *dir,
                ratio: *ratio,
                left: Box::new(left.try_map(f)?),
                right: Box::new(right.try_map(f)?),
            },
        })
    }

    /// Check every split ratio in the tree.
    ///
    /// # Errors
    ///
    /// [`LayoutError::InvalidRatio`] for the first bad ratio.
    pub fn validate_ratios(&self) -> Result<(), LayoutError<L>>
    where
        L: std::fmt::Debug,
    {
        match self {
            Self::Leaf(_) => Ok(()),
            Self::Split {
                ratio, left, right, ..
            } => {
                validate_ratio(*ratio)?;
                left.validate_ratios()?;
                right.validate_ratios()
            }
        }
    }
}

impl<L: PartialEq> LayoutNode<L> {
    /// Whether this subtree holds a leaf equal to `target`.
    #[must_use]
    pub fn contains(&self, target: &L) -> bool {
        match self {
            Self::Leaf(l) => l == target,
            Self::Split { left, right, .. } => left.contains(target) || right.contains(target),
        }
    }
}

impl<L: PartialEq + Clone + std::fmt::Debug> LayoutNode<L> {
    /// Split the leaf for `target` into a [`LayoutNode::Split`] whose left
    /// keeps `target` and whose right is a new leaf for `new_leaf`.
    ///
    /// # Errors
    ///
    /// * [`LayoutError::InvalidRatio`] if `ratio` is NaN or outside `(0, 1)`.
    /// * [`LayoutError::PaneNotInLayout`] if `target` is not present.
    ///
    /// The tree is unchanged on error.
    pub fn split(
        &mut self,
        target: &L,
        new_leaf: L,
        dir: SplitDir,
        ratio: f32,
    ) -> Result<(), LayoutError<L>> {
        validate_ratio(ratio)?;
        if !self.contains(target) {
            return Err(LayoutError::PaneNotInLayout(target.clone()));
        }
        self.split_at(target, new_leaf, dir, ratio);
        Ok(())
    }

    fn split_at(&mut self, target: &L, new_leaf: L, dir: SplitDir, ratio: f32) {
        match self {
            Self::Leaf(l) if l == target => {
                let kept = l.clone();
                *self = Self::Split {
                    dir,
                    ratio,
                    left: Box::new(Self::Leaf(kept)),
                    right: Box::new(Self::Leaf(new_leaf)),
                };
            }
            Self::Leaf(_) => {}
            Self::Split { left, right, .. } => {
                if left.contains(target) {
                    left.split_at(target, new_leaf, dir, ratio);
                } else {
                    right.split_at(target, new_leaf, dir, ratio);
                }
            }
        }
    }
}

/// Remove the leaf for `target` from `tree`, collapsing its parent split so
/// the sibling takes the parent's place. Removing the only leaf leaves
/// `None`.
///
/// # Errors
///
/// [`LayoutError::PaneNotInLayout`] if `target` is not present; `tree` is
/// unchanged.
pub fn remove<L: PartialEq + Clone + std::fmt::Debug>(
    tree: &mut Option<LayoutNode<L>>,
    target: &L,
) -> Result<(), LayoutError<L>> {
    let found = tree.as_ref().is_some_and(|t| t.contains(target));
    if !found {
        return Err(LayoutError::PaneNotInLayout(target.clone()));
    }
    *tree = tree.take().and_then(|t| collapse(t, &|l| l == target));
    Ok(())
}

/// Remove every leaf matching `drop`, collapsing splits as [`remove`] does.
/// Returns `None` when no leaf survives.
#[must_use]
pub fn collapse<L>(node: LayoutNode<L>, drop: &impl Fn(&L) -> bool) -> Option<LayoutNode<L>> {
    match node {
        LayoutNode::Leaf(l) => (!drop(&l)).then_some(LayoutNode::Leaf(l)),
        LayoutNode::Split {
            dir,
            ratio,
            left,
            right,
        } => match (collapse(*left, drop), collapse(*right, drop)) {
            (Some(l), Some(r)) => Some(LayoutNode::Split {
                dir,
                ratio,
                left: Box::new(l),
                right: Box::new(r),
            }),
            (Some(only), None) | (None, Some(only)) => Some(only),
            (None, None) => None,
        },
    }
}

/// A leaf of a persisted arrangement: a stable entry id pointing at what the
/// entry shows. Entry ids are random 16 bytes minted by whoever creates the
/// entry.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EntryRef {
    /// Stable id of this entry within its arrangement.
    pub entry: [u8; 16],
    /// What the entry shows.
    pub target: EntryTarget,
}

impl EntryRef {
    /// An entry showing a member resource.
    #[must_use]
    pub const fn member(entry: [u8; 16], resource: ResourceKey) -> Self {
        Self {
            entry,
            target: EntryTarget::Member(resource),
        }
    }

    /// The member resource this entry shows, if it shows one.
    #[must_use]
    pub const fn resource(&self) -> Option<ResourceKey> {
        match self.target {
            EntryTarget::Member(key) => Some(key),
            EntryTarget::Satellite { .. } => None,
        }
    }
}

/// What an arrangement entry shows.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryTarget {
    /// A resource on this authority. Session arrangements may name only
    /// members of their session.
    Member(ResourceKey),
    /// A satellite terminal reached through this hub, addressed by its host
    /// and the satellite's live handle. Presentation only: it is not checked
    /// against membership, renders as a placeholder when it cannot be
    /// resolved, and never authorizes attachment.
    Satellite {
        /// Satellite host name.
        host: String,
        /// The satellite's live terminal handle.
        id: u32,
    },
}

/// One group of an arrangement (a tab, in today's clients) holding one
/// split tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Group {
    /// Random 16-byte id, minted by whoever creates the group.
    pub id: [u8; 16],
    /// Optional human label.
    #[serde(default)]
    pub name: Option<String>,
    /// The group's layout.
    pub tree: LayoutNode<EntryRef>,
}

/// What an arrangement belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArrangementOwner {
    /// The shared arrangement of one session; it may name only members.
    Session(SessionKey),
    /// A workspace's arrangement (stage 6).
    Workspace(WorkspaceKey),
}

/// A shared layout document with stable entry ids pointing at resources.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Arrangement {
    /// Persistent key.
    pub key: ArrangementKey,
    /// The session or workspace it belongs to.
    pub owner: ArrangementOwner,
    /// Groups in presentation order.
    pub groups: Vec<Group>,
    /// Bumps on every committed change.
    pub revision: Revision,
}

/// Why an arrangement document is invalid.
#[derive(Debug, Clone, Copy, PartialEq, Error)]
pub enum ArrangementError {
    /// An entry names a resource outside the allowed member set.
    #[error("entry names a non-member resource {0}")]
    NotAMember(ResourceKey),
    /// Two entries share an entry id.
    #[error("duplicate entry id")]
    DuplicateEntry([u8; 16]),
    /// Two groups share a group id.
    #[error("duplicate group id")]
    DuplicateGroup([u8; 16]),
    /// A split ratio is NaN or outside `(0.0, 1.0)`.
    #[error("invalid split ratio: {0}")]
    InvalidRatio(f32),
}

impl Arrangement {
    /// Check the document: unique group and entry ids, valid ratios, and
    /// every member entry naming a resource in `members`. Satellite entries
    /// are not membership-checked.
    ///
    /// # Errors
    ///
    /// The first [`ArrangementError`] found.
    pub fn validate<'a>(
        &self,
        members: impl IntoIterator<Item = &'a ResourceKey>,
    ) -> Result<(), ArrangementError> {
        validate_groups(&self.groups, members)
    }

    /// Every member resource any entry names, in group then depth-first
    /// order. Satellite entries are skipped.
    #[must_use]
    pub fn resources(&self) -> Vec<ResourceKey> {
        self.groups
            .iter()
            .flat_map(|g| g.tree.leaves().into_iter().filter_map(EntryRef::resource))
            .collect()
    }

    /// Drop every entry naming `resource`, collapsing splits and dropping
    /// groups left empty. Returns whether anything changed.
    pub fn prune(&mut self, resource: ResourceKey) -> bool {
        let before = self.groups.len();
        let mut changed = false;
        self.groups =
            std::mem::take(&mut self.groups)
                .into_iter()
                .filter_map(|g| {
                    if !g
                        .tree
                        .leaves()
                        .iter()
                        .any(|e| e.resource() == Some(resource))
                    {
                        return Some(g);
                    }
                    changed = true;
                    let Group { id, name, tree } = g;
                    collapse(tree, &|e: &EntryRef| e.resource() == Some(resource))
                        .map(|tree| Group { id, name, tree })
                })
                .collect();
        changed || before != self.groups.len()
    }
}

/// [`Arrangement::validate`] over a bare group list.
///
/// # Errors
///
/// The first [`ArrangementError`] found.
pub fn validate_groups<'a>(
    groups: &[Group],
    members: impl IntoIterator<Item = &'a ResourceKey>,
) -> Result<(), ArrangementError> {
    let members: HashSet<&ResourceKey> = members.into_iter().collect();
    let mut group_ids = HashSet::new();
    let mut entry_ids = HashSet::new();
    for group in groups {
        if !group_ids.insert(group.id) {
            return Err(ArrangementError::DuplicateGroup(group.id));
        }
        group.tree.validate_ratios().map_err(|e| match e {
            LayoutError::InvalidRatio(r) => ArrangementError::InvalidRatio(r),
            LayoutError::PaneNotInLayout(_) => ArrangementError::InvalidRatio(f32::NAN),
        })?;
        for leaf in group.tree.leaves() {
            if !entry_ids.insert(leaf.entry) {
                return Err(ArrangementError::DuplicateEntry(leaf.entry));
            }
            if let Some(resource) = leaf.resource()
                && !members.contains(&resource)
            {
                return Err(ArrangementError::NotAMember(resource));
            }
        }
    }
    Ok(())
}
