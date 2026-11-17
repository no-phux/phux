//! [`Arrangement`]: a shared layout document over resources, and the binary
//! split tree ([`LayoutNode`]) each of its groups holds. A slot of the tree
//! may hold a stack of entries, one shown.
//!
//! An arrangement is presentation, never membership: it names resources
//! through [`EntryRef`] leaves and a session's arrangement may name only
//! that session's members. Clients edit trees in their own leaf type (live
//! handles) and translate to [`EntryRef`] leaves with [`LayoutNode::map`].
//!
//! Spec ref: `docs/organization.md` Layout (binary subset). Pane-rect
//! tiling and directional focus live client-side in `phux-client-core`.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::keys::{ArrangementKey, FreshKeys, ResourceKey, Revision, SessionKey, WorkspaceKey};

mod tree;
pub use tree::*;

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
    /// Locked against close: no close of its last member, and no
    /// replacement that drops it, until it is unlocked. Only
    /// `ArrangementLock` changes it; a session kill still ends it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub locked: bool,
}

/// One unnamed group showing `members` side by side, every leaf an even
/// share, with ids from `fresh`; `None` for no members. A session created
/// with members starts with it.
pub fn first_group(members: &[ResourceKey], fresh: &mut dyn FreshKeys) -> Option<Group> {
    let mut leaves = members
        .iter()
        .map(|member| EntryRef::member(fresh.fresh(), *member));
    let mut tree = LayoutNode::Leaf(leaves.next()?);
    for (placed, leaf) in (1_u16..).zip(leaves) {
        // The `placed` leaves already in `tree` keep `placed / (placed + 1)`.
        let ratio = f32::from(placed) / (f32::from(placed) + 1.0);
        tree = LayoutNode::Split {
            dir: SplitDir::Horizontal,
            ratio,
            left: Box::new(tree),
            right: Box::new(LayoutNode::Leaf(leaf)),
        };
    }
    Some(Group {
        id: fresh.fresh(),
        name: None,
        tree,
        locked: false,
    })
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
    /// A stack has fewer than two entries or shows one past its end.
    #[error("invalid stack")]
    InvalidStack,
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
        self.groups = std::mem::take(&mut self.groups)
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
                let Group {
                    id,
                    name,
                    tree,
                    locked,
                } = g;
                collapse(tree, &|e: &EntryRef| e.resource() == Some(resource)).map(|tree| Group {
                    id,
                    name,
                    tree,
                    locked,
                })
            })
            .collect();
        changed || before != self.groups.len()
    }
}

/// The locked tab closing `closing` would empty, by its label.
///
/// That is the first locked group of their sessions' arrangements whose
/// every member entry is in `closing`; `None` when no locked tab would go.
#[must_use]
pub fn locked_tab_closed_by(org: &crate::org::Org, closing: &[ResourceKey]) -> Option<String> {
    let sessions: HashSet<SessionKey> = closing
        .iter()
        .filter_map(|key| org.resource(key).map(|r| r.session))
        .collect();
    sessions.into_iter().find_map(|session| {
        let key = org.session(&session)?.arrangement?;
        let groups = &org.arrangement(&key)?.groups;
        groups.iter().enumerate().find_map(|(index, group)| {
            let members: Vec<_> = group
                .tree
                .leaves()
                .into_iter()
                .filter_map(EntryRef::resource)
                .collect();
            let emptied = !members.is_empty() && members.iter().all(|m| closing.contains(m));
            (group.locked && emptied).then(|| tab_label(group, index))
        })
    })
}

/// How a refusal names a group: its name, else its position.
#[must_use]
pub fn tab_label(group: &Group, index: usize) -> String {
    group
        .name
        .as_ref()
        .map_or_else(|| format!("tab {index}"), |name| format!("tab {name:?}"))
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
            LayoutError::InvalidStack => ArrangementError::InvalidStack,
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
