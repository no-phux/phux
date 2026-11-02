use super::*;

/// A pane reaped by a failed publication already has its `pane_closed`;
/// an event its drain reads afterwards is not journaled.
#[test]
fn a_reaped_pane_journals_nothing_after_its_close() {
    let mut s = crate::state::ServerState::new();
    let (_session, pane) = s.seed_session("drain");
    let wire = s.intern_resource_wire(pane);
    // The seed's organization events are journaled too.
    let seeded = s.journal_head();
    journal_drained_event(&mut s, &wire, AgentEvent::Bell.into(), 0);
    let live = s.journal_head();
    assert_eq!(live, seeded + 1, "a live pane's event is journaled");
    let _ = reap_pane_journaling_close(&mut s, pane);
    let closed = s.journal_head();
    assert!(closed > live, "the close, and the record's removal");
    journal_drained_event(&mut s, &wire, AgentEvent::Bell.into(), 2);
    assert_eq!(s.journal_head(), closed, "nothing follows pane_closed");
}
