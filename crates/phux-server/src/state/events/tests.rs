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
fn a_filter_never_hides_lifecycle_control_or_a_source_gap() {
    let filter = EventFilter::of(vec![ResourceEventType::CwdChanged]);
    assert!(filter.admits(&AgentEvent::ResourceClosed { exit_status: None }));
    assert!(filter.admits(&AgentEvent::SourceGap { dropped: 1 }));
    assert!(!filter.admits(&AgentEvent::Idle));
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
    let legacy = state.clients_mut().new_client_id();
    let aware = state.clients_mut().new_client_id();
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

mod input_activity;
mod satellite;
mod satellite_close;
