//! Conversion between the handle-space [`ArrangementDoc`] a client edits and
//! the key-space arrangement document the server stores
//! ([`phux_protocol::wire::frame::ArrangementDoc`]).
//!
//! Clients edit arrangements whose leaves are live handles
//! ([`ResourceId`]); the server stores leaves that name resource keys, or
//! satellite resources by host and satellite-local id. A [`HandleMap`]
//! translates between the two. Reading an arrangement ([`from_protocol`])
//! yields the client document plus an [`EntryBinding`], which remembers each
//! leaf's entry id and every member entry that has no live handle (a resource
//! that is not executing here). Writing it back ([`to_protocol`]) reuses
//! those entry ids and restores the unresolved entries, so a round trip
//! through a client never drops presentation of work it could not show.
//!
//! Satellite leaves pass through unchanged as
//! [`EntryTarget::Satellite`]; only member leaves need the map.

use std::collections::{HashMap, HashSet};

use phux_protocol::ResourceId;
use phux_protocol::wire::frame::{
    ArrangementDoc as WireDoc, ArrangementGroup as WireGroup, ArrangementNode as WireNode,
    EntryRef, EntryTarget, SplitDirection,
};
use thiserror::Error;

use super::{ArrangementDoc, Group, LayoutNode, LayoutState, SplitDir, identity, leaves};

mod handle_map;
pub use handle_map::HandleMap;

/// Smallest ratio a written split carries; the wire requires the open
/// interval `(0, 1)`, while a client may bank the closed endpoints.
const MIN_WIRE_RATIO: f32 = 0.001;

/// A member entry the client document could not place, because its resource
/// has no live handle.
#[derive(Debug, Clone, PartialEq)]
struct Unplaced {
    /// The group it sat in.
    group: [u8; 16],
    /// The entry.
    entry: EntryRef,
    /// A placed entry of the stack it sat in, to rejoin.
    stack: Option<[u8; 16]>,
}

/// A whole group the client document could not show: none of its entries has
/// a live handle.
#[derive(Debug, Clone, PartialEq)]
struct HiddenGroup {
    /// Its index in the stored document.
    index: usize,
    /// The group as stored.
    group: WireGroup,
}

/// What [`from_protocol`] remembers so [`to_protocol`] can write the document
/// back without losing entry identities or entries the client cannot show.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EntryBinding {
    entries: HashMap<ResourceId, [u8; 16]>,
    unplaced: Vec<Unplaced>,
    hidden: Vec<HiddenGroup>,
}

impl EntryBinding {
    /// The entry id the leaf for `handle` was read with.
    #[must_use]
    pub fn entry(&self, handle: &ResourceId) -> Option<[u8; 16]> {
        self.entries.get(handle).copied()
    }

    /// How many stored member entries have no live handle, including those
    /// in hidden groups.
    #[must_use]
    pub fn unplaced_count(&self) -> usize {
        let hidden: usize = self
            .hidden
            .iter()
            .map(|hidden| wire_leaves(&hidden.group.tree).len())
            .sum();
        self.unplaced.len() + hidden
    }
}

/// Why a client document could not be written as a stored arrangement.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WireError {
    /// A leaf names a local handle the map does not know: a terminal that is
    /// not a member record of this server.
    #[error("layout leaf {0:?} has no resource key")]
    UnknownHandle(ResourceId),
}

/// Read a stored arrangement as a client document.
///
/// Member leaves become their live handles; satellite leaves become
/// satellite handles. A member leaf with no live handle is left out of the
/// tree (its split collapses onto the sibling) and remembered in the
/// returned [`EntryBinding`]; a group with no placeable leaf is left out
/// whole and remembered the same way. A group without a name is named by its
/// 1-based position. Focus starts on each group's first leaf and the first
/// group is active.
#[must_use]
pub fn from_protocol(doc: &WireDoc, map: &HandleMap) -> (ArrangementDoc, EntryBinding) {
    let mut binding = EntryBinding::default();
    let mut groups = Vec::with_capacity(doc.groups.len());
    for (index, group) in doc.groups.iter().enumerate() {
        let mut unplaced = Vec::new();
        let tree = node_from_wire(&group.tree, map, &mut binding.entries, &mut unplaced);
        match tree {
            Some(tree) => {
                binding
                    .unplaced
                    .extend(unplaced.into_iter().map(|(entry, stack)| Unplaced {
                        group: group.id,
                        entry,
                        stack,
                    }));
                let focus = leaves(&tree).into_iter().next();
                let name = group
                    .name
                    .clone()
                    .unwrap_or_else(|| (index + 1).to_string());
                let state = LayoutState {
                    tree: Some(tree),
                    focus,
                };
                groups.push(Group::with_id(group.id, name, state).with_locked(group.locked));
            }
            None => binding.hidden.push(HiddenGroup {
                index,
                group: group.clone(),
            }),
        }
    }
    (ArrangementDoc { groups, active: 0 }, binding)
}

/// Write a client document as a stored arrangement.
///
/// Each leaf keeps the entry id it was read with; a new leaf gets a fresh
/// random one. Entries the client could not place are restored: an unplaced
/// member entry rejoins its group (split below the group's tree), or, if the
/// client removed that group, a group under the same id at the end; a hidden
/// group returns at its stored position. Ratios outside `(0, 1)` are clamped
/// into it.
///
/// # Errors
///
/// [`WireError::UnknownHandle`] when a local leaf has no key in `map`.
pub fn to_protocol(
    doc: &ArrangementDoc,
    map: &HandleMap,
    binding: &EntryBinding,
) -> Result<WireDoc, WireError> {
    let mut used_entries = HashSet::new();
    let mut groups = Vec::with_capacity(doc.groups.len() + binding.hidden.len());
    for group in &doc.groups {
        let Some(tree) = group.state.tree.as_ref() else {
            continue;
        };
        let mut node = node_to_wire(tree, map, binding, &mut used_entries)?;
        for unplaced in binding.unplaced.iter().filter(|u| u.group == group.id) {
            let entry = unique_entry(unplaced.entry.clone(), &mut used_entries);
            node = match unplaced.stack {
                Some(anchor) => restack(node, anchor, entry),
                None => append_leaf(node, entry),
            };
        }
        groups.push(WireGroup {
            id: group.id,
            name: (!group.name.is_empty()).then(|| group.name.clone()),
            tree: node,
            locked: group.locked,
        });
    }
    let mut orphans: Vec<WireGroup> = Vec::new();
    for unplaced in &binding.unplaced {
        if groups.iter().any(|g| g.id == unplaced.group) {
            continue;
        }
        let entry = unique_entry(unplaced.entry.clone(), &mut used_entries);
        match orphans.iter_mut().find(|g| g.id == unplaced.group) {
            Some(orphan) => {
                let tree = std::mem::replace(&mut orphan.tree, WireNode::Leaf(entry.clone()));
                orphan.tree = append_leaf(tree, entry);
            }
            None => orphans.push(WireGroup {
                id: unplaced.group,
                name: None,
                tree: WireNode::Leaf(entry),
                locked: false,
            }),
        }
    }
    groups.extend(orphans);
    for hidden in &binding.hidden {
        if groups.iter().any(|g| g.id == hidden.group.id) {
            continue;
        }
        let at = hidden.index.min(groups.len());
        groups.insert(at, hidden.group.clone());
    }
    Ok(WireDoc {
        groups,
        stacks: true,
    })
}

fn node_from_wire(
    node: &WireNode,
    map: &HandleMap,
    entries: &mut HashMap<ResourceId, [u8; 16]>,
    unplaced: &mut Vec<(EntryRef, Option<[u8; 16]>)>,
) -> Option<LayoutNode> {
    match node {
        WireNode::Leaf(entry) => {
            let handle = handle_from_wire(entry, map, entries);
            if handle.is_none() {
                unplaced.push((entry.clone(), None));
            }
            handle.map(LayoutNode::Leaf)
        }
        WireNode::Stack {
            active,
            entries: stacked,
        } => {
            let mut panes = Vec::new();
            let mut shown = 0;
            let mut missing = Vec::new();
            for (at, entry) in stacked.iter().enumerate() {
                match handle_from_wire(entry, map, entries) {
                    Some(handle) => {
                        if at <= usize::from(*active) {
                            shown = panes.len();
                        }
                        panes.push(handle);
                    }
                    None => missing.push(entry.clone()),
                }
            }
            // What can't be shown rejoins the stack on write-back, beside a
            // placed entry of it, or below the group when none was placed.
            let anchor = panes.first().and_then(|p| entries.get(p)).copied();
            unplaced.extend(missing.into_iter().map(|entry| (entry, anchor)));
            super::stacked(shown, panes)
        }
        WireNode::Split {
            dir,
            ratio,
            first,
            second,
        } => {
            let first = node_from_wire(first, map, entries, unplaced);
            let second = node_from_wire(second, map, entries, unplaced);
            match (first, second) {
                (Some(left), Some(right)) => Some(LayoutNode::Split {
                    dir: split_from_wire(*dir),
                    ratio: *ratio,
                    left: Box::new(left),
                    right: Box::new(right),
                }),
                (Some(only), None) | (None, Some(only)) => Some(only),
                (None, None) => None,
            }
        }
    }
}

/// The live handle a stored entry shows, recorded in `entries`; `None` for
/// one with no live handle, of a kind this build doesn't know, or naming a
/// handle an earlier entry already shows.
fn handle_from_wire(
    entry: &EntryRef,
    map: &HandleMap,
    entries: &mut HashMap<ResourceId, [u8; 16]>,
) -> Option<ResourceId> {
    let handle = match &entry.target {
        EntryTarget::Member(key) => map.handle(key).cloned(),
        EntryTarget::Satellite { host, id } => Some(ResourceId::Satellite {
            host: host.clone(),
            id: *id,
        }),
        // A target kind this build does not know cannot be shown; keep it
        // for the write-back.
        _ => None,
    }?;
    // A second entry for one handle cannot be shown twice.
    if entries.contains_key(&handle) {
        return None;
    }
    entries.insert(handle.clone(), entry.entry);
    Some(handle)
}

fn node_to_wire(
    node: &LayoutNode,
    map: &HandleMap,
    binding: &EntryBinding,
    used: &mut HashSet<[u8; 16]>,
) -> Result<WireNode, WireError> {
    match node {
        LayoutNode::Leaf(handle) => Ok(WireNode::Leaf(entry_to_wire(handle, map, binding, used)?)),
        LayoutNode::Stack { active, panes } => Ok(WireNode::Stack {
            active: u16::try_from(*active).unwrap_or(0),
            entries: panes
                .iter()
                .map(|handle| entry_to_wire(handle, map, binding, used))
                .collect::<Result<_, _>>()?,
        }),
        LayoutNode::Split {
            dir,
            ratio,
            left,
            right,
        } => Ok(WireNode::Split {
            dir: split_to_wire(*dir),
            ratio: wire_ratio(*ratio),
            first: Box::new(node_to_wire(left, map, binding, used)?),
            second: Box::new(node_to_wire(right, map, binding, used)?),
        }),
    }
}

fn entry_to_wire(
    handle: &ResourceId,
    map: &HandleMap,
    binding: &EntryBinding,
    used: &mut HashSet<[u8; 16]>,
) -> Result<EntryRef, WireError> {
    let target = match handle {
        ResourceId::Local { .. } => EntryTarget::Member(
            map.key(handle)
                .ok_or_else(|| WireError::UnknownHandle(handle.clone()))?,
        ),
        ResourceId::Satellite { host, id } => EntryTarget::Satellite {
            host: host.clone(),
            id: *id,
        },
    };
    let entry = EntryRef {
        entry: binding.entry(handle).unwrap_or_else(identity::fresh_id),
        target,
    };
    Ok(unique_entry(entry, used))
}

/// `tree` with `entry` added to the slot holding `anchor`, behind what it
/// shows; split below the tree when `anchor` is gone.
fn restack(tree: WireNode, anchor: [u8; 16], entry: EntryRef) -> WireNode {
    fn go(node: &mut WireNode, anchor: [u8; 16], entry: &EntryRef) -> bool {
        match node {
            WireNode::Leaf(leaf) if leaf.entry == anchor => {
                *node = WireNode::Stack {
                    active: 0,
                    entries: vec![leaf.clone(), entry.clone()],
                };
                true
            }
            WireNode::Stack { entries, .. } if entries.iter().any(|e| e.entry == anchor) => {
                entries.push(entry.clone());
                true
            }
            WireNode::Split { first, second, .. } => {
                go(first, anchor, entry) || go(second, anchor, entry)
            }
            _ => false,
        }
    }
    let mut tree = tree;
    if go(&mut tree, anchor, &entry) {
        tree
    } else {
        append_leaf(tree, entry)
    }
}

/// `entry` with an id no earlier entry of this document used.
fn unique_entry(mut entry: EntryRef, used: &mut HashSet<[u8; 16]>) -> EntryRef {
    while entry.entry == [0; 16] || !used.insert(entry.entry) {
        entry.entry = identity::fresh_id();
    }
    entry
}

/// `tree` with `entry` split in below it.
fn append_leaf(tree: WireNode, entry: EntryRef) -> WireNode {
    WireNode::Split {
        dir: SplitDirection::Vertical,
        ratio: 0.5,
        first: Box::new(tree),
        second: Box::new(WireNode::Leaf(entry)),
    }
}

fn wire_ratio(ratio: f32) -> f32 {
    if ratio.is_nan() {
        0.5
    } else {
        ratio.clamp(MIN_WIRE_RATIO, 1.0 - MIN_WIRE_RATIO)
    }
}

const fn split_from_wire(dir: SplitDirection) -> SplitDir {
    match dir {
        SplitDirection::Horizontal => SplitDir::Horizontal,
        SplitDirection::Vertical => SplitDir::Vertical,
    }
}

const fn split_to_wire(dir: SplitDir) -> SplitDirection {
    match dir {
        SplitDir::Horizontal => SplitDirection::Horizontal,
        SplitDir::Vertical => SplitDirection::Vertical,
    }
}

/// Every entry of a stored tree, depth-first.
#[must_use]
pub fn wire_leaves(node: &WireNode) -> Vec<EntryRef> {
    let mut out = Vec::new();
    collect_wire_leaves(node, &mut out);
    out
}

fn collect_wire_leaves(node: &WireNode, out: &mut Vec<EntryRef>) {
    match node {
        WireNode::Leaf(entry) => out.push(entry.clone()),
        WireNode::Stack { entries, .. } => out.extend(entries.iter().cloned()),
        WireNode::Split { first, second, .. } => {
            collect_wire_leaves(first, out);
            collect_wire_leaves(second, out);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]
mod tests;
