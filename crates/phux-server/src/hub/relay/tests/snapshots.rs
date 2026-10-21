//! Snapshot gating: retained snapshots, deltas, and late attaches.

use super::*;

pub(super) fn snapshot_frame(id: u32) -> FrameKind {
    FrameKind::BootstrapReady {
        terminal_id: ResourceId::local(id),
        stream_id: phux_protocol::ids::StreamId::new(1).expect("test stream id"),
        bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("test bootstrap id"),
        history_cursor: None,
    }
}

pub(super) fn begin_frame(id: u32, generation: u64) -> FrameKind {
    FrameKind::BootstrapBegin {
        terminal_id: ResourceId::local(id),
        stream_id: phux_protocol::ids::StreamId::new(1).expect("test stream id"),
        bootstrap_id: phux_protocol::ids::BootstrapId::new(generation).expect("test bootstrap id"),
        profile: phux_protocol::caps::BootstrapStreamProfile::SynthesizedVtRaw,
        cols: 80,
        rows: 24,
        base_seq: 0,
    }
}

pub(super) fn output_frame(id: u32, seq: u64, bytes: &'static [u8]) -> FrameKind {
    FrameKind::ResourceOutput {
        terminal_id: ResourceId::local(id),
        stream_id: phux_protocol::ids::StreamId::new(1).expect("test stream id"),
        bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("test bootstrap id"),
        seq,
        bytes: bytes::Bytes::from_static(bytes),
    }
}

#[tokio::test]
async fn full_mailbox_retains_the_snapshot_so_a_delta_never_overtakes_it() {
    // A snapshot refused by a full mailbox is retained, so a later delta
    // cannot reach the consumer first (L1 §9.1).
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (out_tx, mut out_rx) = mpsc::channel(2);
    subscribe(&mut session, 9, ClientId(1), out_tx.clone());
    // Saturate the mailbox: the snapshot's fan-out will be refused.
    out_tx
        .try_send(Outbound::Frame(FrameKind::Detach))
        .expect("filler one");
    out_tx
        .try_send(Outbound::Frame(FrameKind::Detach))
        .expect("filler two");

    // Snapshot arrives while saturated -> retained, nothing delivered.
    session
        .handle_inbound(&encode(&snapshot_frame(9)))
        .expect("valid satellite frame");
    // Free the mailbox.
    assert!(matches!(
        out_rx.try_recv().expect("filler one drains"),
        Outbound::Frame(FrameKind::Detach)
    ));
    assert!(matches!(
        out_rx.try_recv().expect("filler two drains"),
        Outbound::Frame(FrameKind::Detach)
    ));

    session
        .handle_inbound(&encode(&output_frame(9, 1, b"delta")))
        .expect("valid satellite frame");

    let Outbound::Frame(first) = out_rx.try_recv().expect("snapshot delivered") else {
        panic!("unexpected terminal outbound sentinel")
    };
    assert!(
        matches!(first, FrameKind::BootstrapReady { .. }),
        "the snapshot must reach the consumer before any delta, got {first:?}"
    );
    let Outbound::Frame(second) = out_rx.try_recv().expect("delta delivered") else {
        panic!("unexpected terminal outbound sentinel")
    };
    assert!(
        matches!(
            second,
            FrameKind::ResourceOutput { ref terminal_id, seq: 1, .. }
                if *terminal_id == ResourceId::satellite("devbox", 9)
        ),
        "the delta must ride after the snapshot, re-tagged, got {second:?}"
    );
}

#[tokio::test]
async fn deltas_are_suppressed_until_the_retained_snapshot_flushes_on_the_tick() {
    // A stuck snapshot suppresses deltas; the keepalive flush converges.
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (out_tx, mut out_rx) = mpsc::channel(1);
    subscribe(&mut session, 9, ClientId(1), out_tx.clone());
    out_tx
        .try_send(Outbound::Frame(FrameKind::Detach))
        .expect("filler");

    // Snapshot refused (retained), then a delta while still full.
    session
        .handle_inbound(&encode(&snapshot_frame(9)))
        .expect("valid satellite frame");
    session
        .handle_inbound(&encode(&output_frame(9, 1, b"delta")))
        .expect("valid satellite frame");

    assert!(matches!(
        out_rx.try_recv().expect("filler drains"),
        Outbound::Frame(FrameKind::Detach)
    ));
    assert!(
        out_rx.try_recv().is_err(),
        "no frame may reach the consumer while the snapshot is stuck"
    );

    session.flush_pending_snapshots();
    let Outbound::Frame(frame) = out_rx.try_recv().expect("snapshot flushed on tick") else {
        panic!("unexpected terminal outbound sentinel")
    };
    assert!(
        matches!(frame, FrameKind::BootstrapReady { .. }),
        "the tick flush delivers the retained snapshot, got {frame:?}"
    );
    assert!(
        out_rx.try_recv().is_err(),
        "the suppressed delta was dropped, not delivered before the snapshot"
    );
}

#[tokio::test]
async fn a_fresher_snapshot_replaces_a_retained_one() {
    // A newer snapshot replaces a retained one.
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (out_tx, mut out_rx) = mpsc::channel(1);
    subscribe(&mut session, 9, ClientId(1), out_tx.clone());
    out_tx
        .try_send(Outbound::Frame(FrameKind::Detach))
        .expect("filler");

    // First snapshot refused and retained.
    session
        .handle_inbound(&encode(&begin_frame(9, 1)))
        .expect("valid satellite frame");
    // A fresher snapshot arrives (still full) and must replace it.
    session
        .handle_inbound(&encode(&begin_frame(9, 2)))
        .expect("valid satellite frame");
    assert!(matches!(
        out_rx.try_recv().expect("filler drains"),
        Outbound::Frame(FrameKind::Detach)
    ));
    session.flush_pending_snapshots();
    let Outbound::Frame(FrameKind::BootstrapBegin { bootstrap_id, .. }) =
        out_rx.try_recv().expect("bootstrap BEGIN flushed")
    else {
        panic!("expected a bootstrap BEGIN");
    };
    assert_eq!(
        bootstrap_id.get(),
        2,
        "the freshest retained generation must win"
    );
}

// --- second-subscriber attach ordering (phux-v45.14) ------------------

#[test]
fn a_second_attach_sees_its_snapshot_before_any_delta_of_the_ongoing_stream() {
    // B attaches while A streams: B must not see A's delta before its own
    // snapshot (L1 §9.1).
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (tx_a, mut rx_a) = mpsc::channel(8);
    let (tx_b, mut rx_b) = mpsc::channel(8);

    // A attaches and its snapshot lands: A is now streaming (gate Open).
    attach(&mut session, 9, ClientId(1), tx_a);
    session
        .handle_inbound(&encode(&snapshot_frame(9)))
        .expect("valid satellite frame");
    let Outbound::Frame(a_snap) = rx_a.try_recv().expect("A's snapshot") else {
        panic!("unexpected terminal outbound sentinel")
    };
    assert!(matches!(a_snap, FrameKind::BootstrapReady { .. }));

    attach(&mut session, 9, ClientId(2), tx_b);

    session
        .handle_inbound(&encode(&output_frame(9, 1, b"a-stream")))
        .expect("valid satellite frame");
    let Outbound::Frame(a_delta) = rx_a.try_recv().expect("A sees the delta") else {
        panic!("unexpected terminal outbound sentinel")
    };
    assert!(matches!(a_delta, FrameKind::ResourceOutput { seq: 1, .. }));
    assert!(
        rx_b.try_recv().is_err(),
        "B must not see a delta before its own snapshot (L1 §9.1)"
    );

    // B's own snapshot finally lands: it is delivered, opening B's gate.
    session
        .handle_inbound(&encode(&snapshot_frame(9)))
        .expect("valid satellite frame");
    let Outbound::Frame(b_first) = rx_b.try_recv().expect("B's snapshot lands") else {
        panic!("unexpected terminal outbound sentinel")
    };
    assert!(
        matches!(b_first, FrameKind::BootstrapReady { .. }),
        "B's first frame must be its snapshot, got {b_first:?}"
    );

    // A subsequent delta now rides after B's snapshot, in order.
    session
        .handle_inbound(&encode(&output_frame(9, 2, b"after")))
        .expect("valid satellite frame");
    let Outbound::Frame(b_delta) = rx_b.try_recv().expect("B sees the post-snapshot delta") else {
        panic!("unexpected terminal outbound sentinel")
    };
    assert!(
        matches!(
            b_delta,
            FrameKind::ResourceOutput { ref terminal_id, seq: 2, .. }
                if *terminal_id == ResourceId::satellite("devbox", 9)
        ),
        "B's delta must follow its snapshot, re-tagged, got {b_delta:?}"
    );
}

#[test]
fn a_gated_attach_still_receives_terminal_closed_before_being_reaped() {
    // A gated subscriber still receives RESOURCE_CLOSED.
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (tx_b, mut rx_b) = mpsc::channel(8);
    // B attaches: gate is AwaitingFirst, no snapshot delivered yet.
    attach(&mut session, 9, ClientId(2), tx_b);

    // A normal delta is still suppressed while gated...
    session
        .handle_inbound(&encode(&output_frame(9, 1, b"suppressed")))
        .expect("valid satellite frame");
    assert!(
        rx_b.try_recv().is_err(),
        "a content delta is suppressed before the snapshot"
    );

    // ...but the terminal closing must reach B before it is reaped.
    session
        .handle_inbound(&encode(&FrameKind::ResourceClosed {
            terminal_id: ResourceId::local(9),
            exit_status: Some(0),
            reason: phux_protocol::wire::frame::CloseReason::Unknown,
            signal: None,
        }))
        .expect("valid satellite frame");
    let Outbound::Frame(frame) = rx_b.try_recv().expect("close delivered past the gate") else {
        panic!("unexpected terminal outbound sentinel")
    };
    assert_eq!(
        frame,
        FrameKind::ResourceClosed {
            terminal_id: ResourceId::satellite("devbox", 9),
            exit_status: Some(0),
            reason: phux_protocol::wire::frame::CloseReason::Unknown,
            signal: None,
        },
        "a gated subscriber must still see RESOURCE_CLOSED"
    );
    // And the subscription is reaped: no further fan-out for the id.
    session
        .handle_inbound(&encode(&output_frame(9, 2, b"after-close")))
        .expect("valid satellite frame");
    assert!(
        rx_b.try_recv().is_err(),
        "subscription must be reaped on close"
    );
}

#[test]
fn an_event_only_subscription_is_not_gated_by_the_snapshot() {
    // Event-only subscriptions carry no snapshot and flow at once.
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (out_tx, mut out_rx) = mpsc::channel(8);
    subscribe(&mut session, 9, ClientId(1), out_tx);
    session
        .handle_inbound(&encode(&FrameKind::Event {
            terminal: Some(ResourceId::local(9)),
            event: AgentEvent::CommandStarted,
            stamp: None,
        }))
        .expect("valid satellite frame");
    let Outbound::Frame(frame) = out_rx.try_recv().expect("event flows without a snapshot") else {
        panic!("unexpected terminal outbound sentinel")
    };
    assert_eq!(
        frame,
        FrameKind::Event {
            terminal: Some(ResourceId::satellite("devbox", 9)),
            event: AgentEvent::CommandStarted,
            stamp: None,
        }
    );
}

/// L1 §7.3: the hub drops the satellite's journal stamp.
#[test]
fn a_satellite_event_stamp_does_not_cross_the_hub() {
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (out_tx, mut out_rx) = mpsc::channel(8);
    subscribe(&mut session, 9, ClientId(1), out_tx);
    let stamp =
        phux_protocol::wire::frame::EventStamp::new(41, 1_700_000_000_000).with_actor(Some(
            phux_protocol::wire::frame::ActorRef::new(phux_protocol::ids::ClientId::new(3)),
        ));
    session
        .handle_inbound(&encode(&FrameKind::Event {
            terminal: Some(ResourceId::local(9)),
            event: AgentEvent::CommandStarted,
            stamp: Some(Box::new(stamp)),
        }))
        .expect("valid satellite frame");
    let Outbound::Frame(frame) = out_rx.try_recv().expect("the event flows") else {
        panic!("unexpected terminal outbound sentinel")
    };
    assert_eq!(
        frame,
        FrameKind::Event {
            terminal: Some(ResourceId::satellite("devbox", 9)),
            event: AgentEvent::CommandStarted,
            stamp: None,
        }
    );
}

#[test]
fn subscribe_then_attach_upgrade_gates_deltas_until_the_attach_snapshot() {
    // An event subscriber upgrading to an attach is re-gated until the
    // attach's snapshot (L1 §9.1).
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (out_tx, mut out_rx) = mpsc::channel(8);

    // 1. Event-only subscribe: gate Open, an EVENT flows immediately.
    subscribe(&mut session, 9, ClientId(1), out_tx.clone());
    session
        .handle_inbound(&encode(&FrameKind::Event {
            terminal: Some(ResourceId::local(9)),
            event: AgentEvent::CommandStarted,
            stamp: None,
        }))
        .expect("valid satellite frame");
    assert!(
        out_rx.try_recv().is_ok(),
        "the event-only stream must be flowing before the upgrade"
    );

    // 2. UPGRADE the same client to an attach on the same terminal.
    attach(&mut session, 9, ClientId(1), out_tx);

    session
        .handle_inbound(&encode(&output_frame(9, 1, b"pre-snapshot")))
        .expect("valid satellite frame");
    assert!(
        out_rx.try_recv().is_err(),
        "the upgrade must gate deltas until its own snapshot (L1 §9.1)"
    );

    // 4. The attach's snapshot lands: delivered, re-opening the gate.
    session
        .handle_inbound(&encode(&snapshot_frame(9)))
        .expect("valid satellite frame");
    let Outbound::Frame(first) = out_rx.try_recv().expect("the attach snapshot lands") else {
        panic!("unexpected terminal outbound sentinel")
    };
    assert!(
        matches!(first, FrameKind::BootstrapReady { .. }),
        "the first post-upgrade frame must be the snapshot, got {first:?}"
    );

    // 5. A subsequent delta now rides after the snapshot, in order.
    session
        .handle_inbound(&encode(&output_frame(9, 2, b"post-snapshot")))
        .expect("valid satellite frame");
    let Outbound::Frame(delta) = out_rx.try_recv().expect("post-snapshot delta rides") else {
        panic!("unexpected terminal outbound sentinel")
    };
    assert!(
        matches!(
            delta,
            FrameKind::ResourceOutput { ref terminal_id, seq: 2, .. }
                if *terminal_id == ResourceId::satellite("devbox", 9)
        ),
        "the delta must follow the snapshot, re-tagged, got {delta:?}"
    );
}

#[test]
fn an_event_only_re_subscribe_does_not_re_gate_a_flowing_stream() {
    // An event-only re-subscribe leaves an open stream flowing.
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (out_tx, mut out_rx) = mpsc::channel(8);
    subscribe(&mut session, 9, ClientId(1), out_tx.clone());
    // A second event-only subscribe for the same client (idempotent).
    subscribe(&mut session, 9, ClientId(1), out_tx);
    session
        .handle_inbound(&encode(&FrameKind::Event {
            terminal: Some(ResourceId::local(9)),
            event: AgentEvent::CommandStarted,
            stamp: None,
        }))
        .expect("valid satellite frame");
    assert!(
        out_rx.try_recv().is_ok(),
        "an event-only re-subscribe must not re-gate the flowing stream"
    );
}

#[test]
fn a_gated_attach_still_receives_a_bell_past_the_gate() {
    // A bell passes an AwaitingFirst gate.
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (tx, mut rx) = mpsc::channel(8);
    // Attach: gate is AwaitingFirst, no snapshot delivered yet.
    attach(&mut session, 9, ClientId(2), tx);

    // A content delta is suppressed while gated...
    session
        .handle_inbound(&encode(&output_frame(9, 1, b"suppressed")))
        .expect("valid satellite frame");
    assert!(
        rx.try_recv().is_err(),
        "a content delta is suppressed before the snapshot"
    );

    // ...but a bell rings through, re-tagged, past the gate.
    session
        .handle_inbound(&encode(&FrameKind::Bell {
            terminal_id: ResourceId::local(9),
        }))
        .expect("valid satellite frame");
    let Outbound::Frame(frame) = rx.try_recv().expect("bell delivered past the gate") else {
        panic!("unexpected terminal outbound sentinel")
    };
    assert_eq!(
        frame,
        FrameKind::Bell {
            terminal_id: ResourceId::satellite("devbox", 9),
        },
        "a gated subscriber must still see a BELL"
    );

    // ...without opening it.
    session
        .handle_inbound(&encode(&output_frame(9, 2, b"still-gated")))
        .expect("valid satellite frame");
    assert!(
        rx.try_recv().is_err(),
        "the bell bypass must not open the snapshot gate"
    );
}

// --- session: lifecycle teardown --------------------------------------
