use super::*;
use phux_protocol::wire::frame::{AgentEvent, RESOURCE_AGENT_KEY, RESOURCE_ASKED_KEY, Scope};

fn outbound(plane: &mut ControlPlane) -> Vec<FrameKind> {
    plane
        .take_outbound()
        .iter()
        .map(|bytes| decode(bytes))
        .collect()
}

fn state_id(frames: &[FrameKind]) -> u32 {
    frames
        .iter()
        .find_map(|frame| match frame {
            FrameKind::Command {
                request_id,
                command: Command::GetState { .. },
            } => Some(*request_id),
            _ => None,
        })
        .expect("topology read")
}

fn metadata_id(frames: &[FrameKind]) -> u32 {
    frames
        .iter()
        .find_map(|frame| match frame {
            FrameKind::GetMetadata { request_id, .. } => Some(*request_id),
            _ => None,
        })
        .expect("metadata read")
}

fn answer_state(plane: &mut ControlPlane, request_id: u32, snapshot: SessionSnapshot) {
    plane
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::OkWith(CommandValue::State(snapshot)),
        })
        .unwrap();
}

fn answer_metadata(plane: &mut ControlPlane, request_id: u32, value: Option<&[u8]>) {
    plane
        .feed(FrameKind::MetadataValue {
            request_id,
            value: value.map(<[u8]>::to_vec),
        })
        .unwrap();
}

fn event(plane: &mut ControlPlane, terminal: Option<ResourceId>, event: AgentEvent) {
    plane
        .feed(FrameKind::Event {
            terminal,
            event,
            stamp: None,
        })
        .unwrap();
}

fn journal_gap(plane: &mut ControlPlane) {
    event(
        plane,
        None,
        AgentEvent::JournalGap {
            first_missing: 1,
            last_missing: 10,
        },
    );
}

fn browsing() -> (ControlPlane, u32) {
    let mut plane = ControlPlane::new(ControlOptions::default());
    plane.connection_opened();
    let _ = plane.take_outbound();
    plane.feed(hello_ok(PROTOCOL_VERSION.patch)).unwrap();
    let frames = outbound(&mut plane);
    assert!(matches!(frames[0], FrameKind::SubscribeEvents { .. }));
    let request_id = state_id(&frames);
    (plane, request_id)
}

fn discovered() -> (ControlPlane, u32) {
    let (plane, agent, _) = discovered_both();
    (plane, agent)
}

/// An inventoried terminal with both roster reads outstanding:
/// `(plane, agent read, asked read)`.
fn discovered_both() -> (ControlPlane, u32, u32) {
    let (mut plane, request_id) = browsing();
    answer_state(&mut plane, request_id, snapshot());
    let frames = outbound(&mut plane);
    assert!(
        matches!(&frames[0], FrameKind::SubscribeMetadata { scope: Scope::Resource(id), key } if id == &terminal() && key == RESOURCE_AGENT_KEY)
    );
    assert!(matches!(frames[1], FrameKind::GetMetadata { .. }));
    assert!(
        matches!(&frames[2], FrameKind::SubscribeMetadata { scope: Scope::Resource(id), key } if id == &terminal() && key == RESOURCE_ASKED_KEY),
        "{frames:?}"
    );
    let agent = metadata_id(&frames);
    let asked = asked_id(&frames);
    let _ = plane.take_events();
    (plane, agent, asked)
}

fn asked_id(frames: &[FrameKind]) -> u32 {
    frames
        .iter()
        .find_map(|frame| match frame {
            FrameKind::GetMetadata {
                request_id,
                scope: Scope::Resource(id),
                key,
            } if id == &terminal() && key == RESOURCE_ASKED_KEY => Some(*request_id),
            _ => None,
        })
        .expect("asked read")
}

fn asked_levels(plane: &mut ControlPlane) -> Vec<bool> {
    plane
        .take_events()
        .into_iter()
        .filter_map(|event| match event {
            Event::AgentAskedState { terminal_id, asked } => {
                assert_eq!(terminal_id, terminal());
                Some(asked)
            }
            _ => None,
        })
        .collect()
}

fn asked_changed(plane: &mut ControlPlane, value: Option<&[u8]>) {
    plane
        .feed(FrameKind::MetadataChanged {
            scope: Scope::Resource(terminal()),
            key: RESOURCE_ASKED_KEY.to_owned(),
            value: value.map(<[u8]>::to_vec),
            actor: None,
        })
        .unwrap();
}

fn asked_event(plane: &mut ControlPlane, id: &str) {
    event(
        plane,
        Some(terminal()),
        AgentEvent::Asked {
            id: id.to_owned(),
            question: "Proceed?".to_owned(),
            suggestions: Vec::new(),
            elapsed_seconds: None,
        },
    );
}

fn declarations(plane: &mut ControlPlane) -> Vec<Option<Vec<u8>>> {
    plane
        .take_events()
        .into_iter()
        .filter_map(|event| match event {
            Event::AgentMetadata { terminal_id, value } => {
                assert_eq!(terminal_id, terminal());
                Some(value)
            }
            _ => None,
        })
        .collect()
}

#[test]
fn subscribes_before_read_and_a_live_clear_fences_the_delayed_snapshot() {
    let (mut plane, read) = discovered();
    plane
        .feed(FrameKind::MetadataChanged {
            scope: Scope::Resource(terminal()),
            key: RESOURCE_AGENT_KEY.to_owned(),
            value: None,
            actor: None,
        })
        .unwrap();
    answer_metadata(&mut plane, read, Some(b"old declaration"));
    assert_eq!(declarations(&mut plane), vec![None]);
    assert!(outbound(&mut plane).is_empty());
}

#[test]
fn a_live_declaration_fences_a_delayed_absence_and_preserves_raw_consumers() {
    let (mut plane, read) = discovered();
    plane
        .feed(FrameKind::MetadataChanged {
            scope: Scope::Resource(terminal()),
            key: RESOURCE_AGENT_KEY.to_owned(),
            value: Some(b"new".to_vec()),
            actor: None,
        })
        .unwrap();
    answer_metadata(&mut plane, read, None);
    let events = plane.take_events();
    assert_eq!(events.iter().filter(|event| matches!(event, Event::AgentMetadata { value: Some(value), .. } if value == b"new")).count(), 1);
    assert_eq!(events.iter().filter(|event| matches!(event, Event::Frame(frame) if matches!(**frame, FrameKind::MetadataChanged { .. }))).count(), 1);
}

#[test]
fn gap_bursts_coalesce_and_old_reads_cannot_publish_before_recovery() {
    let (mut plane, old_read) = discovered();
    for _ in 0..100 {
        journal_gap(&mut plane);
    }
    let frames = outbound(&mut plane);
    assert_eq!(frames.len(), 1);
    let first = state_id(&frames);
    answer_metadata(&mut plane, old_read, Some(b"stale"));
    assert!(declarations(&mut plane).is_empty());
    assert!(outbound(&mut plane).is_empty());
    answer_state(&mut plane, first, snapshot());
    let followup = state_id(&outbound(&mut plane));
    answer_state(&mut plane, followup, snapshot());
    let final_read = metadata_id(&outbound(&mut plane));
    answer_metadata(&mut plane, final_read, None);
    assert_eq!(declarations(&mut plane), vec![None]);
    assert!(outbound(&mut plane).is_empty());
}

#[test]
fn source_gap_recovers_even_without_an_attached_terminal() {
    let (mut plane, read) = discovered();
    answer_metadata(&mut plane, read, Some(b"old"));
    let _ = plane.take_events();
    event(
        &mut plane,
        Some(terminal()),
        AgentEvent::SourceGap { dropped: 12 },
    );
    let request_id = state_id(&outbound(&mut plane));
    answer_state(&mut plane, request_id, snapshot());
    let read = metadata_id(&outbound(&mut plane));
    answer_metadata(&mut plane, read, Some(b"recovered"));
    assert_eq!(declarations(&mut plane), vec![Some(b"recovered".to_vec())]);
}

#[test]
fn reconnect_resubscribes_and_does_not_correlate_old_metadata() {
    let (mut plane, old_read) = discovered();
    plane.connection_lost(None);
    plane.connection_opened();
    let _ = outbound(&mut plane);
    plane.feed(hello_ok(PROTOCOL_VERSION.patch)).unwrap();
    let request_id = state_id(&outbound(&mut plane));
    answer_state(&mut plane, request_id, snapshot());
    let frames = outbound(&mut plane);
    assert!(matches!(frames[0], FrameKind::SubscribeMetadata { .. }));
    let read = metadata_id(&frames);
    assert_ne!(old_read, read);
    answer_metadata(&mut plane, old_read, Some(b"previous connection"));
    answer_metadata(&mut plane, read, None);
    assert_eq!(declarations(&mut plane), vec![None]);
}

#[test]
fn live_topology_changes_fence_delayed_state_snapshots() {
    let (mut plane, read) = discovered();
    answer_metadata(&mut plane, read, None);
    let request_id = plane.refresh_topology().unwrap();
    let _ = outbound(&mut plane);
    event(
        &mut plane,
        Some(terminal()),
        AgentEvent::TitleChanged {
            title: "new title".into(),
        },
    );
    event(
        &mut plane,
        Some(terminal()),
        AgentEvent::CwdChanged { cwd: "/new".into() },
    );
    answer_state(&mut plane, request_id, snapshot());
    let descriptor = plane.topology().unwrap().pane(&terminal()).unwrap();
    assert_eq!(descriptor.title.as_deref(), Some("new title"));
    assert_eq!(descriptor.cwd.as_deref(), Some("/new"));
    let retry = state_id(&outbound(&mut plane));
    let mut current = snapshot();
    current.resources[0].title = Some("new title".into());
    current.resources[0].cwd = Some("/new".into());
    answer_state(&mut plane, retry, current);
    assert_eq!(
        plane.topology().unwrap().panes[0].title.as_deref(),
        Some("new title")
    );
}

#[test]
fn recovery_removes_vanished_panes_and_ignores_their_late_metadata() {
    let (mut plane, old_read) = discovered();
    journal_gap(&mut plane);
    let request_id = state_id(&outbound(&mut plane));
    let mut current = snapshot();
    current.resources.clear();
    answer_state(&mut plane, request_id, current);
    answer_metadata(&mut plane, old_read, Some(b"gone"));
    assert!(plane.topology().unwrap().panes.is_empty());
    assert!(declarations(&mut plane).is_empty());
    assert!(outbound(&mut plane).is_empty());
}

#[test]
fn unrelated_metadata_replies_remain_extension_frames() {
    let (mut plane, _) = discovered();
    answer_metadata(&mut plane, 9000, Some(b"extension"));
    assert!(
        matches!(plane.take_events().as_slice(), [Event::Frame(frame)] if matches!(**frame, FrameKind::MetadataValue { request_id: 9000, .. }))
    );
}

#[test]
fn metadata_refusal_releases_correlation_without_retracting_the_badge() {
    let (mut plane, read) = discovered();
    plane
        .feed(FrameKind::Error {
            request_id: Some(read),
            code: ErrorCode::InvalidCommand,
            message: "denied".into(),
        })
        .unwrap();
    assert!(declarations(&mut plane).is_empty());
    journal_gap(&mut plane);
    let request_id = state_id(&outbound(&mut plane));
    answer_state(&mut plane, request_id, snapshot());
    let retry = metadata_id(&outbound(&mut plane));
    answer_metadata(&mut plane, retry, None);
    assert_eq!(declarations(&mut plane), vec![None]);
}

#[test]
fn consumer_overflow_recovers_declarations_without_binding_side_requests() {
    let (mut plane, read) = discovered();
    answer_metadata(&mut plane, read, Some(b"old"));
    for _ in 0..phux_client_runtime::control::EVENT_QUEUE_CAP {
        plane
            .feed(FrameKind::Bell {
                terminal_id: terminal(),
            })
            .unwrap();
    }
    let _ = plane.take_events();
    let request_id = state_id(&outbound(&mut plane));
    answer_state(&mut plane, request_id, snapshot());
    let read = metadata_id(&outbound(&mut plane));
    answer_metadata(&mut plane, read, None);
    assert_eq!(declarations(&mut plane), vec![None]);
}

#[test]
fn metadata_error_preserves_the_recovery_requested_while_read_was_pending() {
    let (mut plane, old_read) = discovered();
    journal_gap(&mut plane);
    let inventory = state_id(&outbound(&mut plane));
    answer_state(&mut plane, inventory, snapshot());
    assert!(matches!(
        outbound(&mut plane).as_slice(),
        [
            FrameKind::SubscribeMetadata { .. },
            FrameKind::SubscribeMetadata { .. }
        ]
    ));
    plane
        .feed(FrameKind::Error {
            request_id: Some(old_read),
            code: ErrorCode::InvalidCommand,
            message: "old read failed".into(),
        })
        .unwrap();
    assert!(declarations(&mut plane).is_empty());
    let frames = outbound(&mut plane);
    assert_eq!(frames.len(), 1, "one coalesced retry, even on error");
    let retry = metadata_id(&frames);
    assert_ne!(retry, old_read);
    answer_metadata(&mut plane, retry, None);
    assert_eq!(declarations(&mut plane), vec![None]);
    assert!(outbound(&mut plane).is_empty());
}

#[test]
fn a_spawn_receipt_fences_an_older_inventory_even_before_its_broadcast() {
    let (mut plane, read) = discovered();
    answer_metadata(&mut plane, read, None);
    let old_inventory = plane.refresh_topology().unwrap();
    let spawn = plane.spawn_terminal(SpawnRequest::default());
    let _ = outbound(&mut plane);
    plane
        .feed(FrameKind::ResourceSpawned {
            request_id: spawn,
            result: SpawnResult::Ok(ResourceId::local(8)),
        })
        .unwrap();
    assert!(
        outbound(&mut plane).is_empty(),
        "one inventory read in flight"
    );
    answer_state(&mut plane, old_inventory, snapshot());
    let fresh = state_id(&outbound(&mut plane));
    let mut current = snapshot();
    current.resources.push(ResourceInfo::new(
        ResourceId::local(8),
        WindowId::new(1),
        20,
        4,
    ));
    answer_state(&mut plane, fresh, current);
    assert_eq!(plane.topology().unwrap().panes.len(), 2);
}

#[test]
fn repeated_inventory_refreshes_coalesce_and_leave_a_known_terminal_alone() {
    let (mut plane, read) = discovered();
    let request_id = plane.refresh_topology().unwrap();
    for _ in 0..100 {
        assert_eq!(plane.refresh_topology(), Some(request_id));
    }
    assert_eq!(outbound(&mut plane).len(), 1);
    answer_state(&mut plane, request_id, snapshot());
    assert!(
        outbound(&mut plane).is_empty(),
        "a live subscription already carries every later change"
    );
    answer_metadata(&mut plane, read, Some(b"current"));
    assert_eq!(declarations(&mut plane), vec![Some(b"current".to_vec())]);
}

fn refresh_after_settling(plane: &mut ControlPlane, snapshot: SessionSnapshot) -> Vec<FrameKind> {
    let request_id = plane.refresh_topology().unwrap();
    let _ = outbound(plane);
    answer_state(plane, request_id, snapshot);
    outbound(plane)
}

#[test]
fn an_inventory_subscribes_and_reads_only_terminals_new_to_it() {
    let (mut plane, read) = discovered();
    answer_metadata(&mut plane, read, None);
    let mut current = snapshot();
    current.resources.push(ResourceInfo::new(
        ResourceId::local(8),
        WindowId::new(1),
        20,
        4,
    ));
    let frames = refresh_after_settling(&mut plane, current);
    assert_eq!(frames.len(), 4, "{frames:?}");
    for pair in frames.chunks(2) {
        assert!(
            matches!(
                pair,
                [
                    FrameKind::SubscribeMetadata { scope: Scope::Resource(subscribed), key: subscribed_key },
                    FrameKind::GetMetadata { scope: Scope::Resource(read), key: read_key, .. },
                ] if *subscribed == ResourceId::local(8) && *read == ResourceId::local(8) && subscribed_key == read_key
            ),
            "{frames:?}"
        );
    }
}

#[test]
fn a_detach_ends_the_subscriptions_so_the_next_inventory_renews_them() {
    let (mut plane, read) = discovered();
    answer_metadata(&mut plane, read, None);
    plane
        .feed(FrameKind::Detached {
            reason: Some(DetachReason::Requested),
            message: String::new(),
        })
        .unwrap();
    let frames = refresh_after_settling(&mut plane, snapshot());
    assert!(
        matches!(
            frames.as_slice(),
            [
                FrameKind::SubscribeMetadata { .. },
                FrameKind::GetMetadata { .. },
                FrameKind::SubscribeMetadata { .. },
            ]
        ),
        "the asked read from before the detach is still in flight: {frames:?}"
    );
}

#[test]
fn a_failed_read_is_retried_by_the_next_inventory() {
    let (mut plane, read) = discovered();
    plane
        .feed(FrameKind::Error {
            request_id: Some(read),
            code: ErrorCode::InvalidCommand,
            message: "busy".into(),
        })
        .unwrap();
    let frames = refresh_after_settling(&mut plane, snapshot());
    let retry = metadata_id(&frames);
    assert_ne!(retry, read);
    answer_metadata(&mut plane, retry, Some(b"declared"));
    assert_eq!(declarations(&mut plane), vec![Some(b"declared".to_vec())]);
    assert!(refresh_after_settling(&mut plane, snapshot()).is_empty());
}

#[test]
fn a_denied_read_is_a_stable_answer_that_recovery_does_not_repeat() {
    let (mut plane, read) = discovered();
    plane
        .feed(FrameKind::Error {
            request_id: Some(read),
            code: ErrorCode::PermissionDenied,
            message: "policy denied".into(),
        })
        .unwrap();
    assert!(
        plane.take_events().is_empty(),
        "the grant's answer to the runtime's own read is not a consumer error"
    );
    for _ in 0..3 {
        journal_gap(&mut plane);
        let request_id = state_id(&outbound(&mut plane));
        answer_state(&mut plane, request_id, snapshot());
        assert!(
            outbound(&mut plane).is_empty(),
            "a grant does not change on a live connection, so neither does its refusal"
        );
    }
}

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
