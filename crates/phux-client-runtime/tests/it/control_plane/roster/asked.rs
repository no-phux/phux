//! The server-owned `phux.agent.asked/v1` flag: subscribe, read, fence, retract.
use super::*;

#[test]
fn the_asked_flag_is_subscribed_before_it_is_read_and_reports_the_level() {
    let (mut plane, agent, asked) = discovered_both();
    assert_ne!(agent, asked);
    answer_metadata(&mut plane, asked, Some(b"1"));
    assert_eq!(asked_levels(&mut plane), vec![true]);
    let (mut plane, _, asked) = discovered_both();
    answer_metadata(&mut plane, asked, None);
    assert_eq!(asked_levels(&mut plane), vec![false]);
}

#[test]
fn a_live_clear_retracts_the_question_and_fences_the_delayed_read() {
    let (mut plane, _, asked) = discovered_both();
    asked_changed(&mut plane, Some(b"1"));
    asked_event(&mut plane, "q1");
    asked_changed(&mut plane, None);
    // The read was issued before both changes: its `1` must not resurrect
    // the retracted question.
    answer_metadata(&mut plane, asked, Some(b"1"));
    let events = plane.take_events();
    let order: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            Event::AgentAskedState { asked, .. } => Some(format!("level {asked}")),
            Event::AgentAsked { question_id, .. } => Some(format!("asked {question_id}")),
            _ => None,
        })
        .collect();
    assert_eq!(order, ["level true", "asked q1", "level false"]);
    // Raw consumers still see the live frames.
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, Event::Frame(frame) if matches!(&**frame, FrameKind::MetadataChanged { key, .. } if key == RESOURCE_ASKED_KEY)))
            .count(),
        2
    );
}

#[test]
fn a_question_cleared_during_a_gap_is_retracted_by_recovery() {
    let (mut plane, _, asked) = discovered_both();
    answer_metadata(&mut plane, asked, Some(b"1"));
    asked_event(&mut plane, "q1");
    let _ = plane.take_events();
    // The clear's METADATA_CHANGED was lost with the gap.
    journal_gap(&mut plane);
    let inventory = state_id(&outbound(&mut plane));
    answer_state(&mut plane, inventory, snapshot());
    let reread = asked_id(&outbound(&mut plane));
    assert_ne!(reread, asked);
    answer_metadata(&mut plane, reread, None);
    assert_eq!(asked_levels(&mut plane), vec![false]);
}

#[test]
fn reconnect_rereads_the_flag_and_ignores_the_previous_connection() {
    let (mut plane, _, old_asked) = discovered_both();
    answer_metadata(&mut plane, old_asked, Some(b"1"));
    let _ = plane.take_events();
    plane.connection_lost(None);
    plane.connection_opened();
    let _ = outbound(&mut plane);
    plane.feed(hello_ok(PROTOCOL_VERSION.patch)).unwrap();
    let request_id = state_id(&outbound(&mut plane));
    answer_state(&mut plane, request_id, snapshot());
    let asked = asked_id(&outbound(&mut plane));
    // A late reply correlated by the old connection is not this one's read.
    answer_metadata(&mut plane, old_asked, Some(b"1"));
    answer_metadata(&mut plane, asked, None);
    assert_eq!(asked_levels(&mut plane), vec![false]);
}

#[test]
fn an_ordinary_refresh_leaves_a_pending_question_alone() {
    let (mut plane, agent, asked) = discovered_both();
    answer_metadata(&mut plane, agent, None);
    answer_metadata(&mut plane, asked, Some(b"1"));
    asked_event(&mut plane, "q1");
    let _ = plane.take_events();
    assert!(refresh_after_settling(&mut plane, snapshot()).is_empty());
    assert!(
        asked_levels(&mut plane).is_empty(),
        "no level, so no retraction"
    );
}

#[test]
fn an_unexpected_flag_value_still_reads_as_asked() {
    let (mut plane, _, asked) = discovered_both();
    answer_metadata(&mut plane, asked, Some(b"yes"));
    assert_eq!(asked_levels(&mut plane), vec![true]);
}

#[test]
fn a_closed_terminal_publishes_no_late_asked_level() {
    let (mut plane, _, asked) = discovered_both();
    event(
        &mut plane,
        Some(terminal()),
        AgentEvent::ResourceClosed { exit_status: None },
    );
    answer_metadata(&mut plane, asked, Some(b"1"));
    assert!(asked_levels(&mut plane).is_empty());
}
