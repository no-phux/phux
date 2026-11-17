use super::*;
use crate::layout::split_at;
use phux_protocol::ids::{ResourceKey, SatelliteHost};

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

/// The member keys of group `group_index` of a stored document, in
/// depth-first leaf order: what `name:N.M` addresses.
fn group_member_keys(doc: &WireDoc, group_index: usize) -> Option<Vec<EntryTarget>> {
    let group = doc.groups.get(group_index)?;
    Some(
        wire_leaves(&group.tree)
            .into_iter()
            .map(|entry| entry.target)
            .collect(),
    )
}

fn key(n: u8) -> ResourceKey {
    ResourceKey::new([n; 16]).unwrap()
}

fn entry(n: u8, target: EntryTarget) -> EntryRef {
    EntryRef {
        entry: [0xe0 | n; 16],
        target,
    }
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
        stacks: true,
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
    assert_eq!(
        group_member_keys(&doc(), 0).unwrap()[1],
        EntryTarget::Member(key(2))
    );
}
