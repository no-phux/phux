use super::*;
use crate::state::journal::EventRecord;
use crate::state::{Decision, ServerState};
use phux_protocol::wire::frame::{ApprovalOutcome, Command, ControlAction, EventStamp};

fn pane(id: u32) -> WireResourceId {
    WireResourceId::local(id)
}

fn drain(rx: &mut mpsc::Receiver<Outbound>) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Ok(Outbound::Frame(FrameKind::Event { event, .. })) = rx.try_recv() {
        events.push(event);
    }
    events
}

/// Everything the pump would send to an eager reader.
fn pump_all(state: &mut ServerState, client: ClientId) -> Vec<AgentEvent> {
    pump_stamped(state, client)
        .into_iter()
        .map(|(_, event)| event)
        .collect()
}

/// The pump's frames with each `seq`.
fn pump_stamped(state: &mut ServerState, client: ClientId) -> Vec<(Option<u64>, AgentEvent)> {
    let epoch = epoch(state, client);
    let mut events = Vec::new();
    while let PumpStep::Frame(FrameKind::Event { event, stamp, .. }) =
        state.next_owed_event_frame(client, epoch)
    {
        events.push((stamp.map(|stamp| stamp.seq), event));
    }
    events
}

fn drain_stamped(rx: &mut mpsc::Receiver<Outbound>) -> Vec<(Option<u64>, AgentEvent)> {
    let mut events = Vec::new();
    while let Ok(Outbound::Frame(FrameKind::Event { event, stamp, .. })) = rx.try_recv() {
        events.push((stamp.map(|stamp| stamp.seq), event));
    }
    events
}

fn epoch(state: &ServerState, client: ClientId) -> u64 {
    state
        .clients
        .event_subscriptions
        .get(&client)
        .map_or(0, |sub| sub.epoch)
}

fn record(state: &mut ServerState, terminal: u32, event: AgentEvent) -> u64 {
    state
        .record_and_fanout(EventRecord::new(Some(pane(terminal)), event))
        .expect("stamped")
}

fn gap(first_missing: u64, last_missing: u64) -> AgentEvent {
    AgentEvent::JournalGap {
        first_missing,
        last_missing,
    }
}

#[test]
fn a_client_subscribed_both_ways_receives_each_event_once() {
    let mut state = ServerState::new();
    let client = state.new_client_id();
    let (tx, mut rx) = mpsc::channel(16);
    state.subscribe_events(client, Some(pane(1)), tx.clone());
    state.subscribe_resource_events(client, pane(1), EventFilter::all(), tx.clone());
    state.subscribe_events(client, None, tx);
    let _ = record(&mut state, 1, AgentEvent::Bell);
    assert_eq!(drain(&mut rx), vec![AgentEvent::Bell]);
}

#[test]
fn resubscribing_a_scope_replaces_its_filter() {
    let mut state = ServerState::new();
    let client = state.new_client_id();
    let (tx, mut rx) = mpsc::channel(16);
    let cwd_only = EventFilter::of(vec![ResourceEventType::CwdChanged]);
    state.subscribe_resource_events(client, pane(1), cwd_only, tx.clone());
    let _ = record(&mut state, 1, AgentEvent::Bell);
    assert!(drain(&mut rx).is_empty(), "a bell is not a cwd change");
    state.subscribe_resource_events(client, pane(1), EventFilter::all(), tx);
    let _ = record(&mut state, 1, AgentEvent::Bell);
    assert_eq!(drain(&mut rx), vec![AgentEvent::Bell]);
}

#[test]
fn subscribe_events_resets_a_resource_events_filter() {
    let mut state = ServerState::new();
    let client = state.new_client_id();
    let (tx, mut rx) = mpsc::channel(16);
    let cwd_only = EventFilter::of(vec![ResourceEventType::CwdChanged]);
    state.subscribe_resource_events(client, pane(1), cwd_only, tx.clone());
    state.subscribe_events(client, Some(pane(1)), tx);
    let _ = record(&mut state, 1, AgentEvent::Bell);
    assert_eq!(drain(&mut rx), vec![AgentEvent::Bell], "latest wins");
}

#[test]
fn a_filter_never_hides_lifecycle_control_or_a_source_gap() {
    let filter = EventFilter::of(vec![ResourceEventType::CwdChanged]);
    assert!(filter.admits(&AgentEvent::ResourceClosed { exit_status: None }));
    assert!(filter.admits(&AgentEvent::SourceGap { dropped: 1 }));
    assert!(!filter.admits(&AgentEvent::Idle));
}

#[test]
fn a_full_mailbox_owes_a_gap_the_pump_delivers_without_a_later_event() {
    let mut state = ServerState::new();
    let client = state.new_client_id();
    let (tx, mut rx) = mpsc::channel(1);
    state.subscribe_events(client, None, tx);
    let _ = record(&mut state, 1, AgentEvent::Dirty);
    let lost_a = record(&mut state, 1, AgentEvent::Idle);
    let lost_b = record(&mut state, 1, AgentEvent::Bell);
    assert_eq!(
        drain(&mut rx),
        vec![AgentEvent::Dirty],
        "only the first fit"
    );
    assert_eq!(pump_all(&mut state, client), vec![gap(lost_a, lost_b)]);
    let epoch = epoch(&state, client);
    assert!(matches!(
        state.next_owed_event_frame(client, epoch),
        PumpStep::Idle
    ));
}

#[test]
fn a_cursor_replay_is_queued_in_the_installing_lock_and_goes_live_at_once() {
    let mut state = ServerState::new();
    let _ = record(&mut state, 1, AgentEvent::Bell);
    let _ = record(&mut state, 2, AgentEvent::Bell);
    let client = state.new_client_id();
    let (tx, mut rx) = mpsc::channel(16);
    state.subscribe_events_after(client, Some(pane(1)), 0, tx);
    assert_eq!(
        drain(&mut rx),
        vec![AgentEvent::Bell],
        "only pane 1's event"
    );
    let _ = record(&mut state, 1, AgentEvent::Dirty);
    assert_eq!(drain(&mut rx), vec![AgentEvent::Dirty], "live at once");
}

#[test]
fn a_replay_larger_than_the_mailbox_is_pulled_in_order_as_room_frees() {
    let mut state = ServerState::new();
    for _ in 0..5 {
        let _ = record(&mut state, 1, AgentEvent::Bell);
    }
    let client = state.new_client_id();
    let (tx, mut rx) = mpsc::channel(2);
    state.subscribe_events_after(client, None, 0, tx);
    assert_eq!(drain(&mut rx).len(), 2, "what fits is queued at once");
    // Live events during the replay wait in the ring, behind it.
    let _ = record(&mut state, 1, AgentEvent::Idle);
    assert!(
        drain(&mut rx).is_empty(),
        "no live event overtakes the replay"
    );
    assert_eq!(
        pump_all(&mut state, client),
        vec![
            AgentEvent::Bell,
            AgentEvent::Bell,
            AgentEvent::Bell,
            AgentEvent::Idle
        ],
        "the rest of the replay, then what arrived during it; no gap"
    );
    let _ = record(&mut state, 1, AgentEvent::Dirty);
    assert_eq!(
        drain(&mut rx),
        vec![AgentEvent::Dirty],
        "live after catch-up"
    );
}

#[test]
fn a_second_cursor_scope_never_redelivers_what_the_first_gave() {
    let mut state = ServerState::new();
    for terminal in [1, 2, 1] {
        let _ = record(&mut state, terminal, AgentEvent::Bell);
    }
    let client = state.new_client_id();
    let (tx, mut rx) = mpsc::channel(16);
    state.subscribe_events_after(client, None, 0, tx.clone());
    assert_eq!(drain(&mut rx).len(), 3);
    state.subscribe_events_after(client, Some(pane(1)), 0, tx);
    let second = drain(&mut rx);
    assert!(
        second
            .iter()
            .all(|event| matches!(event, AgentEvent::JournalGap { .. })),
        "at most a gap below what was already given, never a duplicate: {second:?}"
    );
}

#[test]
fn expired_reads_as_released_to_a_subscription_without_a_cursor() {
    let expired = AgentEvent::TerminalControl {
        lifecycle: phux_protocol::wire::frame::ResourceLifecycle::Running,
        exit_status: None,
        input_holder: None,
        action: ControlAction::Expired,
        actor: None,
    };
    let mut state = ServerState::new();
    let legacy = state.new_client_id();
    let aware = state.new_client_id();
    let (legacy_tx, mut legacy_rx) = mpsc::channel(4);
    let (aware_tx, mut aware_rx) = mpsc::channel(4);
    state.subscribe_events(legacy, None, legacy_tx);
    state.subscribe_events_after(aware, None, u64::MAX, aware_tx);
    let _ = record(&mut state, 1, expired.clone());
    assert!(matches!(
        drain(&mut legacy_rx).as_slice(),
        [AgentEvent::TerminalControl {
            action: ControlAction::Released,
            ..
        }]
    ));
    assert_eq!(drain(&mut aware_rx), vec![expired]);
}

#[test]
fn events_carry_the_actors_hello_name() {
    let mut state = ServerState::new();
    let client = state.new_client_id();
    state.set_client_name(client, "orchestrator".to_owned());
    let (tx, mut rx) = mpsc::channel(4);
    state.subscribe_events(client, None, tx);
    let _ = state.record_and_fanout(
        EventRecord::new(Some(pane(1)), AgentEvent::Bell).with_actor(Some(client)),
    );
    let Ok(Outbound::Frame(FrameKind::Event {
        stamp: Some(stamp), ..
    })) = rx.try_recv()
    else {
        panic!("a stamped event");
    };
    let actor = stamp.actor.expect("attributed");
    assert_eq!(actor.client_name.as_deref(), Some("orchestrator"));
    assert!(stamp.ts_ms > 0);
}

mod input_activity;
mod satellite;
mod satellite_close;
