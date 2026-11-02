//! Relayed satellite events: scopes, cursors, gaps, and the head.

use super::*;

#[test]
fn relayed_events_reach_only_their_satellite_scope_and_are_not_retained() {
    let satellite = WireResourceId::satellite("sat", 9);
    let mut state = ServerState::new();
    let wide = state.new_client_id();
    let scoped = state.new_client_id();
    let (wide_tx, mut wide_rx) = mpsc::channel(4);
    let (scoped_tx, mut scoped_rx) = mpsc::channel(4);
    state.subscribe_events(wide, None, wide_tx);
    state.subscribe_satellite_events(
        scoped,
        satellite.clone(),
        EventFilter::all(),
        None,
        scoped_tx,
    );
    let actor = phux_protocol::wire::frame::ActorRef::new(phux_protocol::ids::ClientId::new(3));
    let theirs = EventStamp::new(40, 1_234).with_actor(Some(actor));
    let seq = state
        .record_relayed_event(satellite.clone(), AgentEvent::Bell, Some(&theirs))
        .expect("stamped");
    assert!(drain(&mut wide_rx).is_empty(), "not a server-wide event");
    let Ok(Outbound::Frame(FrameKind::Event {
        terminal,
        stamp: Some(stamp),
        ..
    })) = scoped_rx.try_recv()
    else {
        panic!("the satellite scope receives it");
    };
    assert_eq!(terminal, Some(satellite));
    assert_eq!((stamp.seq, stamp.ts_ms), (seq, 1_234));
    assert_eq!(stamp.actor, None, "a satellite's actor does not cross");
    assert!(
        state.journal.replay_after(0, |_| true).entries.is_empty(),
        "relayed events are not retained"
    );
}

#[test]
fn a_cursor_on_a_satellite_scope_is_owed_a_gap() {
    let mut state = ServerState::new();
    let _ = record(&mut state, 1, AgentEvent::Bell);
    let client = state.new_client_id();
    let (tx, mut rx) = mpsc::channel(4);
    state.subscribe_satellite_events(
        client,
        WireResourceId::satellite("sat", 9),
        EventFilter::all(),
        Some(0),
        tx,
    );
    assert_eq!(drain(&mut rx), vec![gap(1, 1)]);
}

/// A satellite cursor's gap is covered by the head, sent or owed; a hub
/// event on another scope does not raise it.
#[test]
fn the_head_covers_a_satellite_cursors_gap_and_no_later_hub_event() {
    let mut state = ServerState::new();
    let _ = record(&mut state, 1, AgentEvent::Bell);
    let past_cursor = record(&mut state, 1, AgentEvent::Bell);
    let subscribe = |state: &mut ServerState, tx| {
        let client = state.new_client_id();
        let satellite = WireResourceId::satellite("sat", 9);
        state.subscribe_satellite_events(client, satellite, EventFilter::all(), Some(1), tx);
        client
    };
    let (tx, mut rx) = mpsc::channel(4);
    let sent = subscribe(&mut state, tx);
    let (full_tx, _full_rx) = mpsc::channel(1);
    full_tx
        .try_send(Outbound::Frame(journal_gap_frame(0, 0)))
        .expect("room for the filler");
    let owed = subscribe(&mut state, full_tx);
    let later = record(&mut state, 2, AgentEvent::Bell);

    assert_eq!(drain(&mut rx), vec![gap(2, past_cursor)]);
    for client in [sent, owed] {
        let head = state.journal_head_for(Some(client));
        assert_eq!(head, past_cursor, "covers the gap's last_missing");
        assert!(head < later, "a later hub event does not raise it");
    }
}

/// A stale cursor on a closed pane with a full ring reports the eviction;
/// the head counts it only while a replay is pending below it.
#[test]
fn a_later_eviction_does_not_raise_the_head_past_the_gap_sent() {
    let mut state = ServerState::new();
    state.set_event_journal_bounds(2, 1 << 20);
    for _ in 0..3 {
        let _ = record(&mut state, 2, AgentEvent::Bell);
    }
    let closed = pane(7);
    let sent = state.new_client_id();
    let (tx, mut rx) = mpsc::channel(4);
    state.subscribe_events_after(sent, Some(closed.clone()), 0, tx);
    assert_eq!(drain(&mut rx), vec![gap(1, 1)]);
    let owed = state.new_client_id();
    let (full_tx, _full_rx) = mpsc::channel(1);
    full_tx
        .try_send(Outbound::Frame(journal_gap_frame(0, 0)))
        .expect("room for the filler");
    state.subscribe_events_after(owed, Some(closed), 0, full_tx);
    let _ = record(&mut state, 2, AgentEvent::Bell);

    assert_eq!(
        state.journal_head_for(Some(sent)),
        1,
        "the gap it was sent, not the eviction since"
    );
    assert_eq!(
        state.journal_head_for(Some(owed)),
        2,
        "a replay pending below the eviction will report it"
    );
}

/// A `ROLE_CHANGED` withheld from a cursor-less subscription does not
/// raise its head (ADR-0127).
#[test]
fn a_withheld_role_changed_does_not_raise_a_legacy_connections_head() {
    let mut state = ServerState::new();
    let legacy = state.new_client_id();
    let (tx, _rx) = mpsc::channel(8);
    state.subscribe_events(legacy, None, tx);
    let aware = state.new_client_id();
    let (aware_tx, _aware_rx) = mpsc::channel(8);
    state.subscribe_events_after(aware, None, u64::MAX, aware_tx);
    let seen = record(&mut state, 1, AgentEvent::Bell);
    let role_changed = record(
        &mut state,
        1,
        AgentEvent::TerminalControl {
            lifecycle: phux_protocol::wire::frame::ResourceLifecycle::Running,
            exit_status: None,
            input_holder: None,
            action: ControlAction::RoleChanged,
            actor: None,
        },
    );

    assert_eq!(
        state.journal_head_for(Some(legacy)),
        seen,
        "the withheld ROLE_CHANGED must not raise a legacy head"
    );
    assert_eq!(state.journal_head_for(Some(aware)), role_changed);
}

#[test]
fn an_interrupted_satellite_scope_is_owed_a_gap_and_stops_receiving() {
    let satellite = WireResourceId::satellite("sat", 9);
    let mut state = ServerState::new();
    let client = state.new_client_id();
    let (tx, mut rx) = mpsc::channel(4);
    state.subscribe_satellite_events(client, satellite.clone(), EventFilter::all(), None, tx);
    state.interrupt_satellite_scope(client, &satellite);
    assert!(
        matches!(drain(&mut rx).as_slice(), [AgentEvent::JournalGap { .. }]),
        "the interrupted stream is reported"
    );
    let _ = state.record_relayed_event(satellite, AgentEvent::Bell, None);
    assert!(drain(&mut rx).is_empty(), "the scope is gone");
}

/// A void cursor merged into a pending replay reports `journal_gap{1,
/// head}` and nothing at or below it follows.
#[test]
fn a_void_cursor_merged_into_a_pending_replay_never_redelivers_below_its_gap() {
    let mut state = ServerState::new();
    for _ in 0..10 {
        let _ = record(&mut state, 1, AgentEvent::Bell);
    }
    let client = state.new_client_id();
    let (tx, mut rx) = mpsc::channel(2);
    state.subscribe_events_after(client, None, 0, tx.clone());
    let _first = drain_stamped(&mut rx);
    state.subscribe_events_after(client, Some(pane(1)), 999, tx);
    let mut rest = drain_stamped(&mut rx);
    rest.extend(pump_stamped(&mut state, client));
    let gap_last = rest
        .iter()
        .find_map(|(_, event)| match event {
            AgentEvent::JournalGap { last_missing, .. } => Some(*last_missing),
            _ => None,
        })
        .expect("the void cursor is reported");
    let after_gap: Vec<u64> = rest
        .iter()
        .skip_while(|(_, event)| !matches!(event, AgentEvent::JournalGap { .. }))
        .filter_map(|(seq, _)| *seq)
        .collect();
    assert!(
        after_gap.iter().all(|seq| *seq > gap_last),
        "gap last={gap_last}, then {after_gap:?}"
    );
}

/// A refused re-subscribe restores the established scope and filter.
#[test]
fn a_refused_resubscribe_restores_the_established_satellite_scope() {
    let satellite = WireResourceId::satellite("sat", 9);
    let mut state = ServerState::new();
    let client = state.new_client_id();
    let (tx, mut rx) = mpsc::channel(8);
    let _established = state.subscribe_satellite_events(
        client,
        satellite.clone(),
        EventFilter::all(),
        None,
        tx.clone(),
    );
    let refused = state.subscribe_satellite_events(
        client,
        satellite.clone(),
        EventFilter::of(vec![ResourceEventType::CwdChanged]),
        None,
        tx,
    );
    state.restore_satellite_scope(client, refused);
    let _ = state.record_relayed_event(satellite, AgentEvent::Bell, None);
    assert_eq!(
        drain(&mut rx),
        vec![AgentEvent::Bell],
        "the established, unfiltered scope still delivers"
    );
}

#[test]
fn a_refused_first_subscribe_leaves_no_satellite_scope() {
    let satellite = WireResourceId::satellite("sat", 9);
    let mut state = ServerState::new();
    let client = state.new_client_id();
    let (tx, mut rx) = mpsc::channel(8);
    let refused =
        state.subscribe_satellite_events(client, satellite.clone(), EventFilter::all(), None, tx);
    state.restore_satellite_scope(client, refused);
    let _ = state.record_relayed_event(satellite, AgentEvent::Bell, None);
    assert!(drain(&mut rx).is_empty());
    assert!(!state.clients.event_subscriptions.contains_key(&client));
}

/// A pump that outlived its subscription is told it is gone.
#[test]
fn a_pump_that_outlived_its_subscription_is_told_it_is_gone() {
    let mut state = ServerState::new();
    let client = state.new_client_id();
    let (tx, _rx) = mpsc::channel(8);
    state.subscribe_events(client, Some(pane(1)), tx.clone());
    let old = state.claim_event_pump(client).expect("first pump");
    state.unsubscribe_resource_events(client, &pane(1));
    state.subscribe_events(client, Some(pane(1)), tx);
    let new = state
        .claim_event_pump(client)
        .expect("the new subscription has its own pump");
    assert!(!Arc::ptr_eq(&old.wake, &new.wake));
    assert!(matches!(
        state.next_owed_event_frame(client, old.epoch),
        PumpStep::Gone
    ));
    assert!(matches!(
        state.next_owed_event_frame(client, new.epoch),
        PumpStep::Idle
    ));
}

/// A gap owed during a pending replay precedes later `seq`s.
#[test]
fn a_gap_owed_during_a_replay_arrives_before_later_seqs() {
    let satellite = WireResourceId::satellite("sat", 9);
    let mut state = ServerState::new();
    for _ in 0..4 {
        let _ = record(&mut state, 1, AgentEvent::Bell);
    }
    let client = state.new_client_id();
    let (tx, mut rx) = mpsc::channel(1);
    let _ = state.subscribe_satellite_events(
        client,
        satellite.clone(),
        EventFilter::all(),
        None,
        tx.clone(),
    );
    state.subscribe_events_after(client, None, 0, tx);
    let relayed = state
        .record_relayed_event(satellite, AgentEvent::Bell, None)
        .expect("stamped");
    let later = record(&mut state, 1, AgentEvent::Idle);
    let mut stream = drain_stamped(&mut rx);
    stream.extend(pump_stamped(&mut state, client));
    let seqs: Vec<Option<u64>> = stream.iter().map(|(seq, _)| *seq).collect();
    assert_eq!(
        seqs,
        vec![Some(1), Some(2), Some(3), Some(4), None, Some(later)],
        "{stream:?}"
    );
    assert_eq!(stream[4].1, gap(relayed, relayed));
}
