//! `input_activity`: live-only, unjournaled, and never echoed.

use super::*;

/// `input_activity` is live only: no journal seq, no stamp, no echo to
/// the sender, and a cursor opened at the head does not replay it.
#[test]
fn input_activity_is_unjournaled_and_not_echoed() {
    let mut state = ServerState::new();
    let sender = state.new_client_id();
    let peer = state.new_client_id();
    state.set_client_name(sender, "ada".to_owned());
    let (sender_tx, mut sender_rx) = mpsc::channel(8);
    let (peer_tx, mut peer_rx) = mpsc::channel(8);
    state.subscribe_events(sender, Some(pane(1)), sender_tx);
    state.subscribe_events(peer, Some(pane(1)), peer_tx);
    let head = state.journal_head();
    state.offer_input_activity(&pane(1), sender);
    assert_eq!(state.journal_head(), head, "the journal head is unchanged");
    assert!(
        drain_stamped(&mut sender_rx).is_empty(),
        "the sender is not echoed"
    );
    assert_eq!(
        drain_stamped(&mut peer_rx),
        vec![(
            None,
            AgentEvent::InputActivity {
                origin: "ada".to_owned(),
            }
        )]
    );
    let seq = record(&mut state, 1, AgentEvent::Bell);
    assert_eq!(seq, 1, "the next journaled event still takes seq 1");
    let (late_tx, _late_rx) = mpsc::channel(8);
    let late = state.new_client_id();
    state.subscribe_events_after(late, Some(pane(1)), seq, late_tx);
    let replayed = pump_all(&mut state, late);
    assert!(
        replayed
            .iter()
            .all(|event| !matches!(event, AgentEvent::InputActivity { .. })),
        "a cursor replay must not surface input_activity: {replayed:?}"
    );
}

/// No HELLO name yields `client:<id>`. A planted credential id is absent
/// from the encoded frame, and a full mailbox drops the event with no gap.
#[test]
fn input_activity_uses_a_fallback_label_and_drops_when_full() {
    let mut state = ServerState::new();
    let sender = state.new_client_id();
    let peer = state.new_client_id();
    state.clients.set_connection_identity(
        sender,
        crate::auth::ConnectionIdentity {
            peer: phux_protocol::policy::PeerIdentity {
                uid: 0,
                pid: None,
                exe_path: None,
                mcp_host_key: None,
                transport: phux_protocol::policy::TransportType::Quic,
                source_addr: None,
            },
            credential: Some(crate::auth::AuthenticatedCredential {
                id: "cred-NEVER-ON-WIRE".to_owned(),
                principal: "ada".to_owned(),
                scopes: Vec::new(),
                issued_at: chrono::Utc::now(),
                expires_at: None,
                generation: 1,
                registry_instance: None,
            }),
            ssh_origin: None,
            bearer: None,
        },
    );
    let (tx, mut rx) = mpsc::channel(1);
    state.subscribe_events(peer, Some(pane(4)), tx);
    let _ = record(&mut state, 4, AgentEvent::Bell);
    let head = state.journal_head();
    state.offer_input_activity(&pane(4), sender);
    assert_eq!(state.journal_head(), head);
    let got = drain_stamped(&mut rx);
    assert_eq!(got, vec![(Some(1), AgentEvent::Bell)]);
    assert!(
        pump_all(&mut state, peer).is_empty(),
        "a dropped input_activity owes no journal_gap"
    );

    let (room_tx, mut room_rx) = mpsc::channel(4);
    let room = state.new_client_id();
    state.subscribe_events(room, Some(pane(4)), room_tx);
    state.offer_input_activity(&pane(4), sender);
    let (stamp, event) = drain_stamped(&mut room_rx)
        .pop()
        .expect("the peer with room receives the fallback");
    assert!(stamp.is_none());
    let AgentEvent::InputActivity { origin } = event else {
        panic!("expected input_activity");
    };
    assert_eq!(origin, format!("client:{}", sender.0));
    let frame = FrameKind::Event {
        terminal: Some(pane(4)),
        event: AgentEvent::InputActivity { origin },
        stamp: None,
    };
    let mut bytes = bytes::BytesMut::new();
    frame.encode(&mut bytes);
    let wire = String::from_utf8_lossy(&bytes);
    assert!(!wire.contains("cred-NEVER-ON-WIRE"));
    assert!(wire.contains(&format!("client:{}", sender.0)));
}
