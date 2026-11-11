use super::*;

fn no_arrangement_or_document_admission(client: &Client, closed: &[ResourceId]) {
    for frame in outbound(client) {
        if let FrameKind::Command { command, .. } = frame {
            assert!(!matches!(command, Command::ArrangementReplace { .. }));
            if let Command::AttachResource { terminal_id, .. } = command {
                assert!(
                    !closed.contains(&terminal_id),
                    "closed document was readmitted"
                );
            }
        }
    }
}

#[test]
fn document_wire_closures_skip_dead_siblings_in_either_order_before_organization_catches_up() {
    for order in [[2, 3], [3, 2]] {
        let records = [
            record(1, ResourceKind::Terminal),
            record(2, ResourceKind::AccessibilityTree),
            record(3, ResourceKind::PixelSurface),
        ];
        let (client, mut rt) = phone_fixture(&records, &doc(three()));
        rt.update(|d, _| d.focus_pane(&id(3)));
        let before = stored(&client);
        for n in order {
            close(&client, n);
        }
        assert!(client.organization().resource_by_live(&id(2)).is_some());
        let observation = client.take_activity();
        rt.update(|d, tx| {
            d.observe(&client, observation, tx);
            assert_eq!(
                d.wanted.as_ref().map(|(id, _)| id),
                Some(&id(1)),
                "only a live terminal can reach create_view"
            );
        });
        step(&mut rt);
        assert_eq!(focus(&rt), id(1));
        assert!(rt.state.wanted.is_none(), "no grace-period stall");
        rt.update(|d, _| {
            d.refresh_resources(&client);
        });
        step(&mut rt);
        assert!(!rt.state.is_document(&id(2)));
        assert!(!rt.state.is_document(&id(3)));
        assert!(rt.state.others.is_empty());
        assert_eq!(stored(&client), before);
        no_arrangement_or_document_admission(&client, &[id(2), id(3)]);
    }
}

#[test]
fn closing_every_document_cannot_reopen_one_from_the_stale_organization() {
    for order in [[1, 2], [2, 1]] {
        let records = [
            record(1, ResourceKind::AccessibilityTree),
            record(2, ResourceKind::PixelSurface),
        ];
        let (client, mut rt) = phone_fixture(&records, &doc(split(1, 2)));
        let before = stored(&client);
        for n in order {
            close(&client, n);
        }
        let observation = client.take_activity();
        rt.update(|d, tx| {
            d.observe(&client, observation, tx);
            assert!(d.wanted.is_none(), "no closed target reaches create_view");
            assert!(d.target(&client).is_none());
        });
        step(&mut rt);
        rt.update(|d, _| {
            d.refresh_resources(&client);
        });
        step(&mut rt);
        assert!(rt.state.shown.is_none());
        assert!(rt.state.wanted.is_none());
        assert!(rt.state.resources.entries.is_empty());
        assert!(rt.state.others.is_empty());
        assert_eq!(stored(&client), before);
        no_arrangement_or_document_admission(&client, &[id(1), id(2)]);
    }
}

#[test]
fn a_closed_execution_fence_allows_a_new_execution_of_the_same_key() {
    for handle in [2, 9] {
        let records = [
            record(1, ResourceKind::Terminal),
            record(2, ResourceKind::AccessibilityTree),
            record(3, ResourceKind::Terminal),
        ];
        let (client, mut rt) = phone_fixture(&records, &doc(three()));
        rt.update(|d, _| d.focus_pane(&id(2)));
        close(&client, 2);
        step(&mut rt);
        assert!(!rt.state.is_document(&id(2)));
        let mut stale = records.to_vec();
        stale[1].name = Some("metadata is not a new execution".into());
        replace_records(&client, stale);
        step(&mut rt);
        assert!(!rt.state.is_document(&id(2)));
        let mut next = records.to_vec();
        next[1] = super::restoration::replacement(handle);
        replace_records(&client, next);
        step(&mut rt);
        assert!(rt.state.is_document(&id(handle)));
        assert!(
            rt.state.resources.closed.is_empty(),
            "obsolete fence is reclaimed"
        );
        rt.update(|d, _| d.want(id(handle)));
        step(&mut rt);
        assert_eq!(focus(&rt), id(handle));
        assert!(rt.state.shown.as_ref().unwrap().view.is_none());
    }
}

#[test]
fn document_close_fences_are_reclaimed_by_inventory_and_connection_retirement() {
    let records = [
        record(1, ResourceKind::Terminal),
        record(2, ResourceKind::AccessibilityTree),
    ];
    let (client, mut rt) = phone_fixture(&records, &doc(split(1, 2)));
    close(&client, 2);
    step(&mut rt);
    assert_eq!(rt.state.resources.closed.len(), 1);
    replace_records(&client, vec![records[0].clone()]);
    step(&mut rt);
    assert!(rt.state.resources.closed.is_empty());
    replace_records(&client, records.to_vec());
    step(&mut rt);
    assert!(rt.state.is_document(&id(2)));
    close(&client, 2);
    step(&mut rt);
    assert_eq!(rt.state.resources.closed.len(), 1);
    rt.update(|d, _| d.retire_documents(&client));
    assert!(rt.state.resources.closed.is_empty());
}

#[test]
fn a_closed_pending_target_cannot_drop_a_different_live_shown_view() {
    let records = [
        record(1, ResourceKind::Terminal),
        record(2, ResourceKind::AccessibilityTree),
    ];
    let (client, mut rt) = phone_fixture(&records, &doc(split(1, 2)));
    let shown = rt.state.shown.as_ref().unwrap().view;
    rt.update(|d, _| d.want(id(2)));
    close(&client, 2);
    step(&mut rt);
    assert_eq!(focus(&rt), id(1));
    assert_eq!(
        rt.state.shown.as_ref().unwrap().view,
        shown,
        "live view is retained"
    );
    assert!(rt.state.wanted.is_none());
}
