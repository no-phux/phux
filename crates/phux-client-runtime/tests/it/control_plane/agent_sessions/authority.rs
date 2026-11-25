//! Authoritative catalog omissions and rebind publication ordering.

use super::*;

#[test]
fn option_false_omitted_attach_withholds_until_explicit_declare() {
    let (mut plane, attach_id) = subscribing(false);
    attach_ready_to(&mut plane, attach_id, with_agent(terminal()));
    publish_agent(&mut plane);
    let cursor = agent_info(&plane).unwrap();
    replace_attached(&mut plane, two_session_snapshot(false));
    assert!(
        take_agent_records(&mut plane).is_empty(),
        "a partial ATTACHED must not release an omitted agent"
    );
    assert_eq!(
        agent_info(&plane),
        Some(cursor),
        "a partial ATTACHED must not drop the owner cursor"
    );
    replace_attached(&mut plane, with_agent(terminal()));
    assert_eq!(take_agent_records(&mut plane), [(vec![1, 2], true)]);
}

#[test]
fn option_false_authoritative_omit_forgets_the_cursor_and_does_not_resurrect() {
    for drained in [false, true] {
        let (mut plane, attach_id) = subscribing(false);
        attach_ready_to(&mut plane, attach_id, with_agent(terminal()));
        publish_agent(&mut plane);
        if drained {
            assert_eq!(take_agent_records(&mut plane), [(vec![1, 2], true)]);
        }
        replace_attached(&mut plane, two_session_snapshot(false));
        assert!(take_agent_records(&mut plane).is_empty());
        let omitted = two_session_snapshot(false);
        plane.reconcile_agent_delivery_catalog(&omitted);
        let after = plane.take_events();
        assert!(after.iter().all(|event| !matches!(
        event,
        Event::AgentSessionClosed { terminal_id, .. } | Event::TerminalClosed { terminal_id, .. }
            if *terminal_id == ResourceId::local(AGENT)
    )));
        assert!(agent_info(&plane).is_none());
        replace_attached(&mut plane, with_agent(terminal()));
        assert!(
            take_agent_records(&mut plane).is_empty(),
            "discarded rows must not resurrect on re-admit"
        );
        agent_bootstrap(&mut plane, 2, None, &[(1, "prompt")]);
        assert_eq!(take_agent_records(&mut plane), [(vec![1], true)]);
    }
}

#[test]
fn authoritative_rebind_publishes_catalog_before_held_agent_records() {
    let mut plane = resumed_with_initial_agent();
    agent_live(&mut plane, 1, &[(3, "tool_start")]).unwrap();
    replace_attached(&mut plane, two_session_snapshot(false));
    assert!(take_agent_records(&mut plane).is_empty());
    let request_id = plane.refresh_topology().unwrap();
    // ATTACHED fences the inventory queued before its admission barrier.
    plane.reply(request_id, state(two_session_snapshot(false)));
    let request_id = plane.refresh_topology().unwrap();
    let _ = plane.take_events();
    plane.reply(request_id, state(with_agent(terminal())));
    let events = plane.take_events();
    let catalog = events
        .iter()
        .position(|event| matches!(event, Event::TopologySnapshot { .. }))
        .expect("authoritative catalog");
    let records = events
        .iter()
        .position(|event| {
            matches!(
                event,
                Event::AgentPublication { publication, .. }
                    if publication.records.iter().map(|record| record.seq).eq([3])
            )
        })
        .expect("held publication");
    assert!(
        catalog < records,
        "a consumer must discover the resource before receiving its records"
    );
}
