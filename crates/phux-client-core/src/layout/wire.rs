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
//! [`EntryTarget::Satellite`](phux_protocol::wire::frame::EntryTarget::Satellite);
//! only member leaves need the map.

use std::collections::{HashMap, HashSet};

use phux_protocol::ResourceId;
use phux_protocol::wire::frame::{
    ArrangementDoc as WireDoc, ArrangementGroup as WireGroup, ArrangementNode as WireNode, EntryRef,
};
use thiserror::Error;

#[cfg(test)]
use super::leaves;
use super::{ArrangementDoc, Group, LayoutState};

mod handle_map;
mod nodes;
pub use handle_map::HandleMap;
use nodes::{append_leaf, node_from_wire, node_to_wire, unique_entry};

/// A member entry the client document could not place, because its resource
/// has no live handle.
#[derive(Debug, Clone, PartialEq)]
struct Unplaced {
    /// The group it sat in.
    group: [u8; 16],
    /// The entry.
    entry: EntryRef,
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
    let mut slots = Vec::new();
    let mut groups = Vec::with_capacity(doc.groups.len());
    for (index, group) in doc.groups.iter().enumerate() {
        let mut unplaced = Vec::new();
        let tree = node_from_wire(
            &group.tree,
            map,
            &mut binding.entries,
            &mut unplaced,
            &mut slots,
        );
        match tree {
            Some(tree) => {
                binding
                    .unplaced
                    .extend(unplaced.into_iter().map(|entry| Unplaced {
                        group: group.id,
                        entry,
                    }));
                let focus = Some(tree.first_leaf().clone());
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
    (
        ArrangementDoc {
            groups,
            active: 0,
            slots,
        },
        binding,
    )
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
        let mut node = node_to_wire(tree, map, binding, &doc.slots, &mut used_entries)?;
        for unplaced in binding.unplaced.iter().filter(|u| u.group == group.id) {
            let entry = unique_entry(unplaced.entry.clone(), &mut used_entries);
            node = append_leaf(node, entry);
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
    Ok(WireDoc { groups })
}

/// Every entry of a stored tree, depth-first.
#[must_use]
pub fn wire_leaves(node: &WireNode) -> Vec<EntryRef> {
    let mut out = Vec::new();
    collect_wire_leaves(node, &mut out);
    out
}

/// The exact entry for a target if the group presents it once.
/// For duplicate presentations a handle alone cannot identify the view.
#[must_use]
pub fn unique_entry_for_target(
    group: &WireGroup,
    target: &phux_protocol::wire::frame::EntryTarget,
) -> Option<[u8; 16]> {
    let leaves = wire_leaves(&group.tree);
    let mut matching = leaves
        .iter()
        .flat_map(|leaf| std::iter::once(leaf).chain(&leaf.tabs))
        .filter(|entry| &entry.target == target);
    let entry = matching.next()?.entry;
    matching.next().is_none().then_some(entry)
}

fn collect_wire_leaves(node: &WireNode, out: &mut Vec<EntryRef>) {
    match node {
        WireNode::Leaf(entry) => out.push(entry.clone()),
        WireNode::Split { first, second, .. } => {
            collect_wire_leaves(first, out);
            collect_wire_leaves(second, out);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use crate::layout::{LayoutNode, SplitDir, split_at};
    use phux_protocol::ids::{ResourceKey, SatelliteHost};
    use phux_protocol::wire::frame::{EntryTarget, SplitDirection};

    /// The keys of every stored member entry that has no live handle.
    fn unplaced_keys(binding: &EntryBinding) -> Vec<ResourceKey> {
        let mut keys: Vec<ResourceKey> = binding
            .unplaced
            .iter()
            .filter_map(|unplaced| member_key(&unplaced.entry))
            .collect();
        for hidden in &binding.hidden {
            keys.extend(
                wire_leaves(&hidden.group.tree)
                    .iter()
                    .filter_map(member_key),
            );
        }
        keys
    }

    const fn member_key(entry: &EntryRef) -> Option<ResourceKey> {
        match &entry.target {
            EntryTarget::Member(key) => Some(*key),
            _ => None,
        }
    }

    fn key(n: u8) -> ResourceKey {
        ResourceKey::new([n; 16]).unwrap()
    }

    fn entry(n: u8, target: EntryTarget) -> EntryRef {
        EntryRef::new([0xe0 | n; 16], target)
    }

    fn member(n: u8) -> WireNode {
        WireNode::Leaf(entry(n, EntryTarget::Member(key(n))))
    }

    fn split(first: WireNode, second: WireNode) -> WireNode {
        WireNode::Split {
            dir: SplitDirection::Horizontal,
            ratio: 0.4,
            first: Box::new(first),
            second: Box::new(second),
        }
    }

    fn map(pairs: &[(u8, u32)]) -> HandleMap {
        HandleMap::from_pairs(
            pairs
                .iter()
                .map(|(k, h)| (key(*k), Some(ResourceId::local(*h)))),
        )
    }

    fn satellite() -> WireNode {
        WireNode::Leaf(entry(
            9,
            EntryTarget::Satellite {
                host: SatelliteHost::new("edge"),
                id: 4,
            },
        ))
    }

    fn doc() -> WireDoc {
        WireDoc {
            groups: vec![
                WireGroup {
                    locked: false,
                    id: [1; 16],
                    name: Some("editor".to_owned()),
                    tree: split(member(1), split(member(2), satellite())),
                },
                WireGroup {
                    locked: false,
                    id: [2; 16],
                    name: None,
                    tree: member(3),
                },
            ],
        }
    }

    #[test]
    fn a_resolved_document_round_trips_byte_for_byte() {
        let stored = doc();
        let handles = map(&[(1, 11), (2, 12), (3, 13)]);
        let (client, binding) = from_protocol(&stored, &handles);
        assert_eq!(client.groups.len(), 2);
        assert_eq!(client.groups[0].id, [1; 16]);
        assert_eq!(client.groups[1].name, "2", "an unnamed group is numbered");
        assert_eq!(
            leaves(client.groups[0].state.tree.as_ref().unwrap()),
            vec![
                ResourceId::local(11),
                ResourceId::local(12),
                ResourceId::satellite("edge", 4)
            ]
        );
        let mut written = to_protocol(&client, &handles, &binding).unwrap();
        // The client named group 2 "2"; everything else is identical.
        written.groups[1].name = None;
        assert_eq!(written, stored);
    }

    #[test]
    fn unresolved_members_are_kept_and_restored_on_write() {
        let stored = doc();
        // Key 2 has no live handle; group 2's only member neither.
        let handles = map(&[(1, 11)]);
        let (client, binding) = from_protocol(&stored, &handles);
        assert_eq!(
            client.groups.len(),
            1,
            "a group with nothing to show is hidden"
        );
        assert_eq!(
            leaves(client.groups[0].state.tree.as_ref().unwrap()),
            vec![ResourceId::local(11), ResourceId::satellite("edge", 4)]
        );
        assert_eq!(binding.unplaced_count(), 2);
        assert_eq!(unplaced_keys(&binding), vec![key(2), key(3)]);

        let written = to_protocol(&client, &handles, &binding).unwrap();
        assert_eq!(written.groups.len(), 2);
        assert_eq!(written.groups[1], stored.groups[1], "hidden group returns");
        let targets: Vec<EntryTarget> = wire_leaves(&written.groups[0].tree)
            .into_iter()
            .map(|e| e.target)
            .collect();
        assert!(targets.contains(&EntryTarget::Member(key(2))));
        assert!(targets.contains(&EntryTarget::Member(key(1))));
    }

    #[test]
    fn edits_keep_entry_ids_and_mint_fresh_ones_for_new_leaves() {
        let stored = doc();
        let mut handles = map(&[(1, 11), (2, 12), (3, 13)]);
        let (mut client, binding) = from_protocol(&stored, &handles);
        handles.insert(key(4), ResourceId::local(14));
        let tree = client.groups[1].state.tree.take().unwrap();
        client.groups[1].state.tree = Some(
            split_at(
                &tree,
                &ResourceId::local(13),
                &ResourceId::local(14),
                SplitDir::Vertical,
                0.5,
            )
            .unwrap(),
        );
        let written = to_protocol(&client, &handles, &binding).unwrap();
        let entries = wire_leaves(&written.groups[1].tree);
        assert_eq!(entries[0], entry(3, EntryTarget::Member(key(3))));
        assert_eq!(entries[1].target, EntryTarget::Member(key(4)));
        assert_ne!(entries[1].entry, [0; 16]);
        let all: Vec<[u8; 16]> = written
            .groups
            .iter()
            .flat_map(|g| wire_leaves(&g.tree))
            .map(|e| e.entry)
            .collect();
        let unique: HashSet<_> = all.iter().collect();
        assert_eq!(unique.len(), all.len(), "entry ids stay unique");
    }

    #[test]
    fn an_unknown_local_handle_is_refused_and_ratios_are_clamped() {
        let client = ArrangementDoc::single(ResourceId::local(99));
        assert_eq!(
            to_protocol(&client, &HandleMap::new(), &EntryBinding::default()),
            Err(WireError::UnknownHandle(ResourceId::local(99)))
        );
        let handles = map(&[(1, 1), (2, 2)]);
        let mut client = ArrangementDoc::single(ResourceId::local(1));
        client.groups[0].state.tree = Some(LayoutNode::Split {
            dir: SplitDir::Horizontal,
            ratio: 1.0,
            left: Box::new(LayoutNode::Leaf(ResourceId::local(1))),
            right: Box::new(LayoutNode::Leaf(ResourceId::local(2))),
        });
        let written = to_protocol(&client, &handles, &EntryBinding::default()).unwrap();
        let WireNode::Split { ratio, .. } = written.groups[0].tree else {
            panic!("split")
        };
        assert!(ratio > 0.0 && ratio < 1.0);
    }

    #[test]
    fn removing_a_group_with_unplaced_entries_keeps_them_under_its_id() {
        let stored = doc();
        let handles = map(&[(1, 11), (3, 13)]);
        let (mut client, binding) = from_protocol(&stored, &handles);
        client.groups.remove(0);
        let written = to_protocol(&client, &handles, &binding).unwrap();
        let orphan = written
            .groups
            .iter()
            .find(|g| g.id == [1; 16])
            .expect("orphaned entries keep their group id");
        assert_eq!(
            wire_leaves(&orphan.tree)[0].target,
            EntryTarget::Member(key(2))
        );
    }

    #[test]
    fn handle_map_pairs_both_ways_and_replaces_stale_pairs() {
        let mut handles = map(&[(1, 11)]);
        assert_eq!(handles.key(&ResourceId::local(11)), Some(key(1)));
        handles.insert(key(1), ResourceId::local(21));
        assert_eq!(handles.key(&ResourceId::local(11)), None);
        assert_eq!(handles.handle(&key(1)), Some(&ResourceId::local(21)));
        handles.remove_key(&key(1));
        assert!(handles.is_empty());
    }
}
