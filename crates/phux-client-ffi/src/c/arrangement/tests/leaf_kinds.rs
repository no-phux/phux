//! Every leaf-capable resource kind is a catalog entry and survives a read:
//! only an `AgentSession`, which is never a leaf, is pruned.
use super::*;
use phux_protocol::ResourceKind;

/// `registry(1, false)` plus, in session 1, a surface (5), a tree (6), a
/// typed document (7), an agent session (8), and a kind this build does
/// not name (11).
fn mixed() -> SessionSnapshot {
    let mut snapshot = registry(1, false);
    let member = |id, kind| {
        ResourceInfo::new(ResourceId::local(id), WindowId::new(0), 0, 0)
            .with_kind(kind)
            .with_session(Some(SessionId::new(1)))
            .with_parent(Some(ResourceId::local(1)))
    };
    snapshot.resources.extend([
        member(5, ResourceKind::PixelSurface).with_title(Some("display".to_owned())),
        member(6, ResourceKind::AccessibilityTree),
        member(7, ResourceKind::Document).with_title(Some("notes".to_owned())),
        member(8, ResourceKind::AgentSession),
        member(11, ResourceKind::Unknown { tag: 90 }),
    ]);
    snapshot
}

/// One group splitting `ids` left to right.
fn row(ids: &[u32]) -> ArrangementDoc {
    let mut leaves = ids.iter().map(|id| ResourceId::local(*id));
    let first = leaves.next().unwrap();
    let mut tree = LayoutNode::Leaf(first.clone());
    let mut last = first;
    for id in leaves {
        tree = layout::split_at(&tree, &last, &id, layout::SplitDir::Horizontal, 0.5).unwrap();
        last = id;
    }
    ArrangementDoc {
        groups: vec![Group::new(
            "mixed".into(),
            LayoutState {
                tree: Some(tree),
                focus: Some(ResourceId::local(ids[0])),
            },
        )],
        active: 0,
    }
}

fn leaves(client: &PhuxClient) -> Vec<ResourceId> {
    client
        .inner
        .arrangement
        .topology
        .groups
        .iter()
        .filter_map(|group| group.state.tree.as_ref())
        .flat_map(layout::leaves)
        .collect()
}

fn node_kinds(client: &PhuxClient) -> Vec<(u32, u32)> {
    (0..client.inner.arrangement.nodes.len())
        .filter_map(|index| {
            let mut node = PhuxArrangementNode::default();
            // SAFETY: live client, writable initialized output.
            let result = unsafe { phux_client_arrangement_node_get(client, index, &raw mut node) };
            assert_eq!(result, PhuxClientResult::Ok);
            (node.kind == 1).then_some((node.terminal_id.id, node.resource_kind))
        })
        .collect()
}

#[test]
fn a_read_keeps_document_and_unknown_leaves_and_prunes_only_agent_sessions() {
    let mut client = harness();
    let doc = row(&[1, 5, 7, 8, 11]);
    refresh(&mut client, 1, mixed(), Some(doc.encode_cbor().unwrap()));
    assert_eq!(client.inner.arrangement.status, 2);
    assert_eq!(
        leaves(&client),
        [1, 5, 7, 11].map(ResourceId::local),
        "an agent session is never a leaf; every other kind survives the read"
    );
    assert_eq!(
        node_kinds(&client),
        [(1, 0), (5, 3), (7, 4), (11, 90)],
        "each leaf names its wire kind"
    );
    // Terminal 2 is unarranged; an unplaced tree (6) is normal for a mirror.
    assert_eq!(client.inner.arrangement.unarranged(), 1);

    let catalog: Vec<_> = (0..)
        .map_while(|index| {
            let mut entry = PhuxCatalogTerminal::default();
            // SAFETY: live client, writable initialized output.
            let result = unsafe {
                phux_client_catalog_terminal_get(&raw const *client, index, &raw mut entry)
            };
            (result == PhuxClientResult::Ok).then_some((entry.terminal_id.id, entry.resource_kind))
        })
        .collect();
    assert_eq!(
        catalog,
        [(1, 0), (2, 0), (5, 3), (6, 2), (7, 4), (9, 0), (11, 90)],
        "every kind but an agent session is a catalog entry"
    );
}

/// Publish `mutation` and confirm it against `mixed()`.
fn mutate_mixed(client: &mut PhuxClient, mutation: &PhuxArrangementMutation) {
    // SAFETY: client and mutation/spans are valid and disjoint.
    let result = unsafe { phux_client_arrangement_mutate(client, mutation) };
    assert_eq!(result, PhuxClientResult::Ok);
    let pending = client.inner.arrangement.pending.as_ref().unwrap();
    let proposed = pending.proposed.as_ref().unwrap().encode_topology_cbor();
    finish(client, mixed(), Some(proposed.unwrap()));
    assert_eq!(client.inner.arrangement.status, 2);
}

#[test]
fn a_mutation_places_a_document_leaf_and_refuses_an_agent_session() {
    let mut client = harness();
    refresh(&mut client, 1, mixed(), Some(tabs_bytes(&[1])));
    let mut split = edit(&client, 2, 2);
    split.terminal_id = terminal_id_out(&ResourceId::local(1));
    split.new_terminal_id = terminal_id_out(&ResourceId::local(5));
    split.direction = 2;
    split.ratio = 0.5;
    mutate_mixed(&mut client, &split);
    let mut add = edit(&client, 3, 1);
    add.terminal_id = terminal_id_out(&ResourceId::local(6));
    add.name = bytes_out(b"tree");
    mutate_mixed(&mut client, &add);
    assert_eq!(leaves(&client), [1, 5, 6].map(ResourceId::local));
    assert_eq!(node_kinds(&client), [(1, 0), (5, 3), (6, 2)]);

    let queued = client.inner.outgoing.len();
    let mut agent = edit(&client, 4, 1);
    agent.terminal_id = terminal_id_out(&ResourceId::local(8));
    // SAFETY: live disjoint client and input; an agent session is no leaf.
    let result = unsafe { phux_client_arrangement_mutate(&raw mut *client, &raw const agent) };
    assert_eq!(result, PhuxClientResult::InvalidState);
    assert_eq!(client.inner.outgoing.len(), queued, "nothing queued");
}

/// `terminals` Terminals in session 1, each with `others_each` documents.
fn fleet(terminals: u32, others_each: u32) -> SessionSnapshot {
    let mut snapshot = registry(1, false);
    snapshot.resources.clear();
    for pane in 0..terminals {
        let id = 1 + pane * (others_each + 1);
        snapshot.resources.push(ResourceInfo::new(
            ResourceId::local(id),
            WindowId::new(10),
            80,
            24,
        ));
        for other in 1..=others_each {
            let kind = if other % 2 == 0 {
                ResourceKind::PixelSurface
            } else {
                ResourceKind::AccessibilityTree
            };
            snapshot.resources.push(
                ResourceInfo::new(ResourceId::local(id + other), WindowId::new(0), 0, 0)
                    .with_kind(kind)
                    .with_session(Some(SessionId::new(1))),
            );
        }
    }
    snapshot
}

#[test]
fn terminals_and_other_leaf_kinds_are_bounded_separately() {
    // A hundred mirrored panes, each with a tree and a surface, still list.
    let catalog = Catalog::from_snapshot(fleet(100, 2), 1).unwrap();
    assert_eq!(catalog.terminals.len(), 300);
    assert_eq!(session_summaries(fleet(100, 2)).unwrap().len(), 2);
    // Each bound is reached on its own and refuses one past it.
    let full = fleet(256, 4);
    assert_eq!(
        Catalog::from_snapshot(full, 1).unwrap().terminals.len(),
        1280
    );
    assert!(Catalog::from_snapshot(fleet(257, 0), 1).is_err());
    assert!(session_summaries(fleet(257, 0)).is_err());
    let mut over = fleet(256, 4);
    over.resources.push(
        ResourceInfo::new(ResourceId::local(9999), WindowId::new(0), 0, 0)
            .with_kind(ResourceKind::Document)
            .with_session(Some(SessionId::new(1))),
    );
    assert!(Catalog::from_snapshot(over, 1).is_err());
}

#[test]
fn only_a_terminal_with_bad_text_or_identity_fails_the_catalog() {
    let long = format!("{}\0tail", "é".repeat(3000));
    let mut snapshot = mixed();
    snapshot.resources[4] = snapshot.resources[4].clone().with_title(Some(long));
    snapshot.resources.push(
        ResourceInfo::new(ResourceId::local(0), WindowId::new(0), 0, 0)
            .with_kind(ResourceKind::Unknown { tag: 90 }),
    );
    let catalog = Catalog::from_snapshot(snapshot, 1).unwrap();
    let tree = catalog
        .terminals
        .iter()
        .find(|entry| entry.id == ResourceId::local(6))
        .unwrap();
    assert_eq!(tree.title, "é".repeat(2048), "cut at a UTF-8 boundary");
    assert!(
        !catalog
            .terminals
            .iter()
            .any(|e| e.id == ResourceId::local(0)),
        "an identity no leaf could carry is left out"
    );

    let mut bad = mixed();
    bad.resources[0] = bad.resources[0].clone().with_title(Some("a\0b".into()));
    assert!(
        Catalog::from_snapshot(bad, 1).is_err(),
        "terminals stay strict"
    );
}

#[test]
fn a_leaf_the_catalog_does_not_list_is_never_reported_as_a_terminal() {
    let (nodes, _) = Catalog::default()
        .flatten(&ArrangementDoc::single(ResourceId::local(1)))
        .unwrap();
    assert_eq!(nodes[0].resource_kind, PHUX_ARRANGEMENT_LEAF_KIND_UNLISTED);
}
