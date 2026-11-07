use super::*;

pub(super) fn replacement(handle: u32) -> ResourceRecord {
    record(2, ResourceKind::AccessibilityTree)
        .with_live(Some(id(handle)))
        .with_activation(Some(phux_protocol::wire::frame::ActivationInfo::new(
            phux_protocol::ids::ActivationId::new(2),
            phux_protocol::ids::ServerInstance::new([2; 16]),
            phux_protocol::wire::frame::ExecState::Running,
        )))
}

#[test]
fn a_document_key_survives_two_organization_batches_until_a_new_execution_arrives() {
    for missing_record in [false, true] {
        for handle in [2, 9] {
            let records = [
                record(1, ResourceKind::Terminal),
                record(2, ResourceKind::AccessibilityTree),
                record(3, ResourceKind::Terminal),
            ];
            let (client, mut rt) = phone_fixture(&records, &doc(three()));
            rt.update(|d, _| d.focus_pane(&id(2)));
            let mut gap = records.to_vec();
            gap[1].live = None;
            if missing_record {
                gap.remove(1);
            }
            replace_records(&client, gap);
            step(&mut rt);
            assert_eq!(focus(&rt), id(1), "temporary fallback stays usable");
            assert!(
                client
                    .acquire_view(rt.state.shown.as_ref().unwrap().view.unwrap())
                    .is_some()
            );
            assert_eq!(rt.state.resources.returning, Some(key(2)));
            assert!(
                rt.state
                    .notice
                    .as_ref()
                    .unwrap()
                    .text
                    .contains("Mirror unavailable")
            );
            step(&mut rt);
            assert_eq!(
                rt.state.resources.returning,
                Some(key(2)),
                "polling cannot consume the key"
            );
            let mut next = records.to_vec();
            next[1] = replacement(handle);
            replace_records(&client, next);
            step(&mut rt);
            assert_eq!(focus(&rt), id(handle));
            assert!(rt.state.shown.as_ref().unwrap().view.is_none());
            assert!(rt.state.resources.returning.is_none());
        }
    }
}

#[test]
fn an_explicit_focus_request_cancels_delayed_document_restoration() {
    let records = [
        record(1, ResourceKind::Terminal),
        record(2, ResourceKind::AccessibilityTree),
        record(3, ResourceKind::Terminal),
    ];
    let (client, mut rt) = phone_fixture(&records, &doc(three()));
    rt.update(|d, _| d.focus_pane(&id(2)));
    let mut gap = records.to_vec();
    gap[1].live = None;
    replace_records(&client, gap);
    step(&mut rt);
    assert_eq!(rt.state.resources.returning, Some(key(2)));
    rt.update(|d, _| d.want(id(3)));
    step(&mut rt);
    assert!(rt.state.resources.returning.is_none());
    assert_eq!(focus(&rt), id(3));
    let mut next = records.to_vec();
    next[1] = replacement(9);
    replace_records(&client, next);
    step(&mut rt);
    assert_eq!(
        focus(&rt),
        id(3),
        "returning document cannot steal explicit focus"
    );
}

#[test]
fn the_missing_document_guard_recovers_a_shown_document_without_its_entry() {
    let records = [
        record(1, ResourceKind::Terminal),
        record(2, ResourceKind::AccessibilityTree),
        record(3, ResourceKind::Terminal),
    ];
    let (client, mut rt) = phone_fixture(&records, &doc(three()));
    rt.update(|d, _| {
        d.focus_pane(&id(2));
        d.resources.entries.remove(&id(2));
        assert!(d.restore_document(&client));
        assert!(d.shown.is_none());
        assert_eq!(d.wanted.as_ref().map(|(id, _)| id), Some(&id(1)));
    });
    step(&mut rt);
    assert_eq!(focus(&rt), id(1));
}

#[test]
fn another_automatic_document_fallback_does_not_cancel_a_waiting_key() {
    for wire_close in [false, true] {
        let records = [
            record(1, ResourceKind::PixelSurface),
            record(2, ResourceKind::AccessibilityTree),
            record(3, ResourceKind::Terminal),
        ];
        let (client, mut rt) = phone_fixture(&records, &doc(three()));
        rt.update(|d, _| d.focus_pane(&id(2)));
        let mut gap = records.to_vec();
        gap[1].live = None;
        replace_records(&client, gap.clone());
        step(&mut rt);
        assert_eq!(focus(&rt), id(1));
        if wire_close {
            close(&client, 1);
        } else {
            gap[0].live = None;
            replace_records(&client, gap);
        }
        step(&mut rt);
        assert_eq!(focus(&rt), id(3));
        assert_eq!(rt.state.resources.returning, Some(key(2)));
        let mut next = records.to_vec();
        next[1] = replacement(9);
        replace_records(&client, next);
        step(&mut rt);
        assert_eq!(focus(&rt), id(9));
    }
}
