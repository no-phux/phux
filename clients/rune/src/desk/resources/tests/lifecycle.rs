//! Phone focus retirement uses live identity, not the pre-batch mirrors.
use super::*;

fn phone_fixture(records: &[ResourceRecord], doc: &ArrangementDoc) -> (Arc<Client>, Runtime<Desk>) {
    let (client, mut rt) = fixture(records, doc);
    rt.update(|d, _| {
        d.phone = Some(crate::desk::phone::Phone::default());
        d.refresh_layout(&client);
    });
    step(&mut rt);
    outbound(&client);
    (client, rt)
}

fn three() -> LayoutNode {
    LayoutNode::Split {
        dir: SplitDir::Horizontal,
        ratio: 0.5,
        left: Box::new(split(1, 2)),
        right: Box::new(LayoutNode::Leaf(id(3))),
    }
}

fn focus(rt: &Runtime<Desk>) -> ResourceId {
    rt.state.shown.as_ref().unwrap().terminal.clone()
}

fn stored(client: &Client) -> ArrangementDoc {
    client
        .organization()
        .arrangement_document(&session())
        .unwrap()
        .2
}

fn replace_records(client: &Client, records: Vec<ResourceRecord>) {
    let revision = client.organization().snapshot_revision().get() + 1;
    let request_id = client.refresh_organization().unwrap();
    let page = OrganizationPage::new(
        AuthorityId::new([1; 16]).unwrap(),
        Revision::new(revision),
        0,
    )
    .with_sessions(vec![
        SessionRecord::new(session(), "work", Revision::new(1))
            .with_arrangement(Some(arrangement()))
            .with_live(Some(SessionId::new(1))),
    ])
    .with_resources(records);
    client
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::OkWith(CommandValue::OrganizationPage(Box::new(page))),
        })
        .unwrap();
}

fn accept_hide(
    client: &Client,
    records: &[ResourceRecord],
    before: &ArrangementDoc,
    ids: &[ResourceId],
) {
    assert_eq!(&stored(client), before, "no speculative server mutation");
    let mut expected = before.clone();
    remove_document_leaves(&mut expected, ids);
    let edits: Vec<_> = outbound(client)
        .into_iter()
        .filter_map(|frame| match frame {
            FrameKind::Command {
                command: Command::ArrangementReplace { document, .. },
                ..
            } => Some(document),
            _ => None,
        })
        .collect();
    let handles = HandleMap::from_pairs(records.iter().map(|r| (r.key, r.live.clone())));
    let (_, _, _, binding) = client
        .organization()
        .arrangement_document(&session())
        .unwrap();
    assert_eq!(edits, [to_protocol(&expected, &handles, &binding).unwrap()]);
    // The connected fixture now hears the arrangement the server accepted.
    install_arrangement(client, records, &expected);
}

#[test]
fn closing_a_phone_document_group_never_reopens_a_document_from_the_hide_batch() {
    let records = [
        record(1, ResourceKind::AccessibilityTree),
        record(2, ResourceKind::PixelSurface),
        record(3, ResourceKind::AccessibilityTree),
    ];
    let mut arrangement_doc = doc(split(1, 2));
    arrangement_doc.add_group("survivor".into(), id(3));
    arrangement_doc.active = 0;
    let (client, mut rt) = phone_fixture(&records, &arrangement_doc);
    let before = stored(&client);
    rt.update(|d, _| d.close_tab(0));
    assert!(rt.state.shown.is_none());
    assert!(rt.state.wanted.is_none(), "no closing sibling is requested");
    assert!(rt.state.resources.dismissed.contains(&key(1)));
    assert!(rt.state.resources.dismissed.contains(&key(2)));
    accept_hide(&client, &records, &before, &[id(1), id(2)]);
    step(&mut rt);
    assert_eq!(focus(&rt), id(3));
    assert_eq!(rt.state.resources.entries.len(), 3, "producers stay live");
    assert!(rt.state.resources.dismissed.contains(&key(2)));
    assert_eq!(
        stored(&client).groups.len(),
        1,
        "only the hide edits the arrangement"
    );
}

#[test]
fn phone_hide_skips_excluded_predecessors_and_chooses_a_surviving_group_leaf() {
    let records = [
        record(1, ResourceKind::AccessibilityTree),
        record(2, ResourceKind::PixelSurface),
        record(3, ResourceKind::Terminal),
    ];
    let arrangement_doc = doc(three());
    let (client, mut rt) = phone_fixture(&records, &arrangement_doc);
    let before = stored(&client);
    rt.update(|d, _| d.focus_pane(&id(2)));
    rt.update(|d, _| d.hide_documents(&[id(1), id(2)]));
    assert_eq!(rt.state.wanted.as_ref().map(|(id, _)| id), Some(&id(3)));
    accept_hide(&client, &records, &before, &[id(1), id(2)]);
    step(&mut rt);
    assert_eq!(focus(&rt), id(3));
    assert!(rt.state.shown.as_ref().unwrap().view.is_some());
    assert_eq!(
        leaves(rt.state.phone.as_ref().unwrap().tree.as_ref().unwrap()),
        [id(3)]
    );
    assert!(rt.state.resources.dismissed.contains(&key(1)));
    assert!(rt.state.resources.dismissed.contains(&key(2)));
    assert_eq!(
        leaves(stored(&client).groups[0].state.tree.as_ref().unwrap()),
        [id(3)]
    );
}

#[test]
fn phone_document_replacement_restores_the_same_key_ahead_of_a_temporary_fallback() {
    for handle in [2, 9] {
        let records = [
            record(1, ResourceKind::Terminal),
            record(2, ResourceKind::AccessibilityTree),
            record(3, ResourceKind::Terminal),
        ];
        let (client, mut rt) = phone_fixture(&records, &doc(three()));
        rt.update(|d, _| d.focus_pane(&id(2)));
        publish(
            &client,
            2,
            ResourceKind::AccessibilityTree,
            br#"{"old":true}"#,
        );
        step(&mut rt);
        let before = stored(&client);
        let mut replacement = records.to_vec();
        replacement[1] = record(2, ResourceKind::AccessibilityTree)
            .with_live(Some(id(handle)))
            .with_activation(Some(phux_protocol::wire::frame::ActivationInfo::new(
                phux_protocol::ids::ActivationId::new(1),
                phux_protocol::ids::ServerInstance::new([2; 16]),
                phux_protocol::wire::frame::ExecState::Running,
            )));
        replace_records(&client, replacement);
        step(&mut rt);
        assert_eq!(focus(&rt), id(handle), "same key wins on handle {handle}");
        assert!(matches!(
            rt.state.resources.entries[&id(handle)].body,
            Body::Pending
        ));
        assert!(rt.state.resources.returning.is_none());
        assert!(
            rt.state
                .notice
                .as_ref()
                .is_none_or(|n| !n.text.contains("Mirror unavailable")),
            "immediate replacement is not unavailable"
        );
        let mut expected = before;
        expected.groups[0].state.tree = Some(LayoutNode::Split {
            dir: SplitDir::Horizontal,
            ratio: 0.5,
            left: Box::new(split(1, handle)),
            right: Box::new(LayoutNode::Leaf(id(3))),
        });
        assert_eq!(stored(&client), expected, "replacement only remaps handles");
    }
}

fn close(client: &Client, n: u32) {
    client
        .feed(FrameKind::ResourceClosed {
            terminal_id: id(n),
            exit_status: Some(0),
            signal: None,
            reason: phux_protocol::wire::frame::CloseReason::Unknown,
        })
        .unwrap();
}

#[test]
fn phone_batched_terminal_closures_skip_dead_siblings_in_either_event_order() {
    for remove_identity in [false, true] {
        for order in [[2, 3], [3, 2]] {
            let records = [
                record(1, ResourceKind::Terminal),
                record(2, ResourceKind::Terminal),
                record(3, ResourceKind::Terminal),
            ];
            let (client, mut rt) = phone_fixture(&records, &doc(three()));
            rt.update(|d, _| d.focus_pane(&id(3)));
            let before = stored(&client);
            if remove_identity {
                remove(&client, 2, ResourceKind::Terminal);
                remove(&client, 3, ResourceKind::Terminal);
            }
            for n in order {
                close(&client, n);
            }
            assert!(
                rt.state.lists_resource(&id(2)),
                "window mirror is still pre-batch"
            );
            step(&mut rt);
            assert_eq!(
                focus(&rt),
                id(1),
                "order {order:?}, removed {remove_identity}"
            );
            assert!(rt.state.wanted.is_none());
            assert!(rt.state.others.is_empty());
            assert!(!rt.state.layout_is_stale());
            if !remove_identity {
                assert_eq!(
                    stored(&client),
                    before,
                    "local fallback never edits the arrangement"
                );
            }
        }
    }
}

#[test]
fn phone_fallback_uses_the_latest_organization_even_while_topology_is_stale() {
    use phux_client_runtime::control::Event;
    let records = [
        record(1, ResourceKind::Terminal),
        record(2, ResourceKind::Terminal),
        record(3, ResourceKind::Terminal),
    ];
    let (client, mut rt) = phone_fixture(&records, &doc(three()));
    rt.update(|d, _| d.focus_pane(&id(3)));
    remove(&client, 2, ResourceKind::Terminal);
    remove(&client, 3, ResourceKind::Terminal);
    assert!(client.topology().unwrap().pane(&id(2)).is_some());
    assert!(client.organization().resource_by_live(&id(2)).is_none());
    rt.update(|d, tx| {
        d.apply(
            &client,
            Event::TerminalClosed {
                terminal_id: id(3),
                exit_status: Some(0),
                signal: None,
                reason: phux_protocol::wire::frame::CloseReason::Unknown,
            },
            tx,
        );
    });
    assert_eq!(rt.state.wanted.as_ref().map(|(id, _)| id), Some(&id(1)));
}

#[test]
fn phone_fallback_rejects_a_leaf_that_is_no_longer_a_supported_document() {
    for kind in [
        ResourceKind::AgentSession,
        ResourceKind::Document,
        ResourceKind::Unknown { tag: 99 },
    ] {
        let records = [
            record(1, ResourceKind::Terminal),
            record(2, ResourceKind::AccessibilityTree),
            record(3, ResourceKind::Terminal),
        ];
        let (client, mut rt) = phone_fixture(&records, &doc(three()));
        rt.update(|d, _| d.focus_pane(&id(3)));
        let mut replacement = records.to_vec();
        replacement[1].kind = kind;
        replace_records(&client, replacement);
        assert!(
            rt.state.is_document(&id(2)),
            "window mirror is still pre-batch"
        );
        // No observation has refreshed the document map or retained group yet.
        rt.update(|d, _| d.close_shown(&client, &[]));
        assert_eq!(rt.state.wanted.as_ref().map(|(id, _)| id), Some(&id(1)));
    }
}

mod closed;
mod restoration;
