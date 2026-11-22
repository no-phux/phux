//! The server-owned `phux.agent.asked/v1` flag: subscribe, read, fence, retract.
use super::*;

#[test]
fn the_asked_flag_is_subscribed_before_it_is_read_and_reports_the_level() {
    let (mut plane, agent, asked) = discovered_both();
    assert_ne!(agent, asked);
    // Any present value reads as asked, not only `1`.
    answer_metadata(&mut plane, asked, Some(b"yes"));
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
