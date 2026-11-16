use super::*;
use phux_protocol::wire::frame::{
    AgentEvent, RESOURCE_AGENT_KEY, RESOURCE_ASKED_KEY, RESOURCE_NOTE_KEY, Scope,
};

fn state_id(frames: &[FrameKind]) -> u32 {
    state_read(frames).expect("topology read")
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
    let mut plane = opened(ControlOptions::default(), hello_ok(PROTOCOL_VERSION.patch));
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
    metadata_changed(plane, RESOURCE_ASKED_KEY, value);
}

/// A live `METADATA_CHANGED` of the terminal's `key`.
fn metadata_changed(plane: &mut ControlPlane, key: &str, value: Option<&[u8]>) {
    plane
        .feed(FrameKind::MetadataChanged {
            scope: Scope::Resource(terminal()),
            key: key.to_owned(),
            value: value.map(<[u8]>::to_vec),
            actor: None,
        })
        .unwrap();
}

fn title_changed(plane: &mut ControlPlane, title: &str) {
    event(
        plane,
        Some(terminal()),
        AgentEvent::TitleChanged {
            title: title.into(),
        },
    );
}

/// Answer the queued topology read with `snapshot`; returns what follows.
fn answer_queued_state(plane: &mut ControlPlane, snapshot: SessionSnapshot) -> Vec<FrameKind> {
    let request_id = state_id(&outbound(plane));
    answer_state(plane, request_id, snapshot);
    outbound(plane)
}

/// The next queued declaration read, answered with `value`, publishes it.
fn assert_reread_declares(plane: &mut ControlPlane, frames: &[FrameKind], value: Option<&[u8]>) {
    answer_metadata(plane, metadata_id(frames), value);
    assert_eq!(declarations(plane), vec![value.map(<[u8]>::to_vec)]);
}

fn refuse(plane: &mut ControlPlane, request_id: u32, code: ErrorCode, message: &str) {
    plane
        .feed(FrameKind::Error {
            request_id: Some(request_id),
            code,
            message: message.into(),
        })
        .unwrap();
}

/// `snapshot()` with a second pane, `ResourceId::local(8)`.
fn with_second_pane() -> SessionSnapshot {
    let mut current = snapshot();
    current.resources.push(ResourceInfo::new(
        ResourceId::local(8),
        WindowId::new(1),
        20,
        4,
    ));
    current
}

/// Spawn a terminal that the server answers as `ResourceId::local(8)`.
fn spawn_second_pane(plane: &mut ControlPlane) {
    let spawn = plane.spawn_terminal(SpawnRequest::default());
    let _ = outbound(plane);
    plane
        .feed(FrameKind::ResourceSpawned {
            request_id: spawn,
            result: SpawnResult::Ok(ResourceId::local(8)),
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
    metadata_changed(&mut plane, RESOURCE_AGENT_KEY, None);
    answer_metadata(&mut plane, read, Some(b"old declaration"));
    assert_eq!(declarations(&mut plane), vec![None]);
    assert!(outbound(&mut plane).is_empty());
}

#[test]
fn a_live_declaration_fences_a_delayed_absence_and_preserves_raw_consumers() {
    let (mut plane, read) = discovered();
    metadata_changed(&mut plane, RESOURCE_AGENT_KEY, Some(b"new"));
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
    let frames = outbound(&mut plane);
    assert_reread_declares(&mut plane, &frames, None);
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
    let frames = answer_queued_state(&mut plane, snapshot());
    assert_reread_declares(&mut plane, &frames, Some(b"recovered"));
}

#[test]
fn reconnect_resubscribes_and_does_not_correlate_old_metadata() {
    let (mut plane, old_read) = discovered();
    plane.connection_lost(None);
    plane.connection_opened();
    let _ = outbound(&mut plane);
    plane.feed(hello_ok(PROTOCOL_VERSION.patch)).unwrap();
    let frames = answer_queued_state(&mut plane, snapshot());
    assert!(matches!(frames[0], FrameKind::SubscribeMetadata { .. }));
    let read = metadata_id(&frames);
    assert_ne!(old_read, read);
    answer_metadata(&mut plane, old_read, Some(b"previous connection"));
    answer_metadata(&mut plane, read, None);
    assert_eq!(declarations(&mut plane), vec![None]);
}

#[test]
fn live_label_changes_overlay_a_delayed_state_snapshot_without_a_retry() {
    let (mut plane, read) = discovered();
    answer_metadata(&mut plane, read, None);
    let request_id = plane.refresh_topology().unwrap();
    let _ = outbound(&mut plane);
    title_changed(&mut plane, "new title");
    event(
        &mut plane,
        Some(terminal()),
        AgentEvent::CwdChanged { cwd: "/new".into() },
    );
    answer_state(&mut plane, request_id, snapshot());
    let descriptor = plane.topology().unwrap().pane(&terminal()).unwrap();
    assert_eq!(descriptor.title.as_deref(), Some("new title"));
    assert_eq!(descriptor.cwd.as_deref(), Some("/new"));
    assert!(
        outbound(&mut plane).is_empty(),
        "a label change outdates the label, not the inventory"
    );
}

/// phux-qkaii: the snapshot's title is the user-set one, so a refresh after
/// the shell retitled the pane used to blank the label other clients saw.
#[test]
fn a_refresh_keeps_the_shell_title_the_snapshot_cannot_carry() {
    let (mut plane, read) = discovered();
    answer_metadata(&mut plane, read, None);
    title_changed(&mut plane, "opencode");
    let _ = refresh_after_settling(&mut plane, snapshot());
    let descriptor = plane.topology().unwrap().pane(&terminal()).unwrap();
    assert_eq!(descriptor.title.as_deref(), Some("opencode"));
}

#[test]
fn an_animating_title_cannot_starve_a_spawn_out_of_the_inventory() {
    let (mut plane, read) = discovered();
    answer_metadata(&mut plane, read, None);
    spawn_second_pane(&mut plane);
    let inventory = state_id(&outbound(&mut plane));
    for frame in ["⠋", "⠙", "⠹"] {
        title_changed(&mut plane, frame);
    }
    answer_state(&mut plane, inventory, with_second_pane());
    let topology = plane.topology().unwrap();
    assert!(topology.pane(&ResourceId::local(8)).is_some());
    assert_eq!(
        topology.pane(&terminal()).unwrap().title.as_deref(),
        Some("⠹")
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
    refuse(&mut plane, read, ErrorCode::InvalidCommand, "denied");
    assert!(declarations(&mut plane).is_empty());
    journal_gap(&mut plane);
    let frames = answer_queued_state(&mut plane, snapshot());
    assert_reread_declares(&mut plane, &frames, None);
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
    let frames = answer_queued_state(&mut plane, snapshot());
    assert_reread_declares(&mut plane, &frames, None);
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
            FrameKind::SubscribeMetadata { .. },
            FrameKind::SubscribeMetadata { .. },
            FrameKind::SubscribeMetadata { .. }
        ]
    ));
    refuse(
        &mut plane,
        old_read,
        ErrorCode::InvalidCommand,
        "old read failed",
    );
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
    spawn_second_pane(&mut plane);
    assert!(
        outbound(&mut plane).is_empty(),
        "one inventory read in flight"
    );
    answer_state(&mut plane, old_inventory, snapshot());
    let fresh = state_id(&outbound(&mut plane));
    answer_state(&mut plane, fresh, with_second_pane());
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
    let frames = refresh_after_settling(&mut plane, with_second_pane());
    assert_eq!(frames.len(), 8, "{frames:?}");
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
                FrameKind::SubscribeMetadata { .. },
                FrameKind::SubscribeMetadata { .. },
            ]
        ),
        "the asked, occupant and note reads from before the detach are still in flight: {frames:?}"
    );
}

#[test]
fn a_failed_read_is_retried_by_the_next_inventory() {
    let (mut plane, read) = discovered();
    refuse(&mut plane, read, ErrorCode::InvalidCommand, "busy");
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
    refuse(
        &mut plane,
        read,
        ErrorCode::PermissionDenied,
        "policy denied",
    );
    assert!(
        plane.take_events().is_empty(),
        "the grant's answer to the runtime's own read is not a consumer error"
    );
    for _ in 0..3 {
        journal_gap(&mut plane);
        assert!(
            answer_queued_state(&mut plane, snapshot()).is_empty(),
            "a grant does not change on a live connection, so neither does its refusal"
        );
    }
}

mod asked;
mod listing;
mod occupant;
mod satellite;

#[test]
fn a_note_read_is_the_baseline_and_only_a_live_change_is_news() {
    let (mut plane, request_id) = browsing();
    answer_state(&mut plane, request_id, snapshot());
    let read = outbound(&mut plane)
        .iter()
        .find_map(|frame| match frame {
            FrameKind::GetMetadata {
                request_id, key, ..
            } if key == RESOURCE_NOTE_KEY => Some(*request_id),
            _ => None,
        })
        .expect("a note read");
    let _ = plane.take_events();
    let note = br#"{"level":"warn","title":"deploy 90%"}"#;
    answer_metadata(&mut plane, read, Some(&note[..]));
    metadata_changed(&mut plane, RESOURCE_NOTE_KEY, Some(note));
    let notes: Vec<(bool, Option<String>)> = plane
        .take_events()
        .into_iter()
        .filter_map(|event| match event {
            Event::Note { live, note, .. } => Some((live, note.map(|note| note.title))),
            _ => None,
        })
        .collect();
    let title = Some("deploy 90%".to_owned());
    assert_eq!(notes, vec![(false, title.clone()), (true, title)]);
}
