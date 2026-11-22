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

/// Terminal `id` closed upstream with status 0 and no stated reason.
pub(super) fn closed_frame(id: u32) -> FrameKind {
    FrameKind::ResourceClosed {
        terminal_id: ResourceId::local(id),
        exit_status: Some(0),
        reason: phux_protocol::wire::frame::CloseReason::Unknown,
        signal: None,
    }
}

#[tokio::test]
async fn full_mailbox_retains_the_snapshot_so_a_delta_never_overtakes_it() {
    // A snapshot refused by a full mailbox is retained, so a later delta
    // cannot reach the consumer first.
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
    session.feed(&snapshot_frame(9));
    // Free the mailbox.
    assert!(matches!(
        out_rx.try_recv().expect("filler one drains"),
        Outbound::Frame(FrameKind::Detach)
    ));
    assert!(matches!(
        out_rx.try_recv().expect("filler two drains"),
        Outbound::Frame(FrameKind::Detach)
    ));

    session.feed(&output_frame(9, 1, b"delta"));

    let first = next_frame(&mut out_rx, "snapshot delivered");
    assert!(
        matches!(first, FrameKind::BootstrapReady { .. }),
        "the snapshot must reach the consumer before any delta, got {first:?}"
    );
    let second = next_frame(&mut out_rx, "delta delivered");
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
    session.feed(&snapshot_frame(9));
    session.feed(&output_frame(9, 1, b"delta"));

    assert!(matches!(
        out_rx.try_recv().expect("filler drains"),
        Outbound::Frame(FrameKind::Detach)
    ));
    assert!(
        out_rx.try_recv().is_err(),
        "no frame may reach the consumer while the snapshot is stuck"
    );

    session.flush_pending_snapshots();
    let frame = next_frame(&mut out_rx, "snapshot flushed on tick");
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
    session.feed(&begin_frame(9, 1));
    // A fresher snapshot arrives (still full) and must replace it.
    session.feed(&begin_frame(9, 2));
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
    // snapshot.
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (tx_a, mut rx_a) = mpsc::channel(8);
    let (tx_b, mut rx_b) = mpsc::channel(8);

    // A attaches and its snapshot lands: A is now streaming (gate Open).
    attach(&mut session, 9, ClientId(1), tx_a);
    session.feed(&snapshot_frame(9));
    let a_snap = next_frame(&mut rx_a, "A's snapshot");
    assert!(matches!(a_snap, FrameKind::BootstrapReady { .. }));

    attach(&mut session, 9, ClientId(2), tx_b);

    session.feed(&output_frame(9, 1, b"a-stream"));
    let a_delta = next_frame(&mut rx_a, "A sees the delta");
    assert!(matches!(a_delta, FrameKind::ResourceOutput { seq: 1, .. }));
    assert!(
        rx_b.try_recv().is_err(),
        "B must not see a delta before its own snapshot (L1 §9.1)"
    );

    // B's own snapshot finally lands: it is delivered, opening B's gate.
    session.feed(&snapshot_frame(9));
    let b_first = next_frame(&mut rx_b, "B's snapshot lands");
    assert!(
        matches!(b_first, FrameKind::BootstrapReady { .. }),
        "B's first frame must be its snapshot, got {b_first:?}"
    );

    // A subsequent delta now rides after B's snapshot, in order.
    session.feed(&output_frame(9, 2, b"after"));
    let b_delta = next_frame(&mut rx_b, "B sees the post-snapshot delta");
    assert!(
        matches!(
            b_delta,
            FrameKind::ResourceOutput { ref terminal_id, seq: 2, .. }
                if *terminal_id == ResourceId::satellite("devbox", 9)
        ),
        "B's delta must follow its snapshot, re-tagged, got {b_delta:?}"
    );
}

/// A gated subscriber still receives `BELL` (without opening the gate) and
/// `RESOURCE_CLOSED` (before it is reaped).
#[test]
fn a_gated_attach_still_receives_bell_and_terminal_closed() {
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (tx_b, mut rx_b) = mpsc::channel(8);
    attach(&mut session, 9, ClientId(2), tx_b);
    session.feed(&output_frame(9, 1, b"suppressed"));
    assert!(
        rx_b.try_recv().is_err(),
        "a delta is suppressed before the snapshot"
    );

    session.feed(&FrameKind::Bell {
        terminal_id: ResourceId::local(9),
    });
    let bell = FrameKind::Bell {
        terminal_id: ResourceId::satellite("devbox", 9),
    };
    assert_eq!(next_frame(&mut rx_b, "bell past the gate"), bell);
    session.feed(&output_frame(9, 2, b"still-gated"));
    assert!(
        rx_b.try_recv().is_err(),
        "the bell bypass does not open the gate"
    );

    let closed = |terminal_id| FrameKind::ResourceClosed {
        terminal_id,
        exit_status: Some(0),
        reason: phux_protocol::wire::frame::CloseReason::Unknown,
        signal: None,
    };
    session.feed(&closed(ResourceId::local(9)));
    let frame = next_frame(&mut rx_b, "close delivered past the gate");
    assert_eq!(frame, closed(ResourceId::satellite("devbox", 9)));
    session.feed(&output_frame(9, 3, b"after-close"));
    assert!(
        rx_b.try_recv().is_err(),
        "subscription must be reaped on close"
    );
}

/// Event-only subscriptions carry no snapshot, so they flow at once, and a
/// re-subscribe does not re-gate them. The hub drops the satellite's
/// journal stamp.
#[test]
fn an_event_only_subscription_flows_ungated_without_the_satellite_stamp() {
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (out_tx, mut out_rx) = mpsc::channel(8);
    subscribe(&mut session, 9, ClientId(1), out_tx.clone());
    subscribe(&mut session, 9, ClientId(1), out_tx);
    let stamp =
        phux_protocol::wire::frame::EventStamp::new(41, 1_700_000_000_000).with_actor(Some(
            phux_protocol::wire::frame::ActorRef::new(phux_protocol::ids::ClientId::new(3)),
        ));
    session.feed(&FrameKind::Event {
        terminal: Some(ResourceId::local(9)),
        event: AgentEvent::CommandStarted,
        stamp: Some(Box::new(stamp)),
    });
    let frame = next_frame(&mut out_rx, "the event flows");
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
    // attach's snapshot.
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (out_tx, mut out_rx) = mpsc::channel(8);

    // 1. Event-only subscribe: gate Open, an EVENT flows immediately.
    subscribe(&mut session, 9, ClientId(1), out_tx.clone());
    session.feed(&agent_event(9));
    assert!(
        out_rx.try_recv().is_ok(),
        "the event-only stream must be flowing before the upgrade"
    );

    // 2. UPGRADE the same client to an attach on the same terminal.
    attach(&mut session, 9, ClientId(1), out_tx);

    session.feed(&output_frame(9, 1, b"pre-snapshot"));
    assert!(
        out_rx.try_recv().is_err(),
        "the upgrade must gate deltas until its own snapshot (L1 §9.1)"
    );

    // 4. The attach's snapshot lands: delivered, re-opening the gate.
    session.feed(&snapshot_frame(9));
    let first = next_frame(&mut out_rx, "the attach snapshot lands");
    assert!(
        matches!(first, FrameKind::BootstrapReady { .. }),
        "the first post-upgrade frame must be the snapshot, got {first:?}"
    );

    // 5. A subsequent delta now rides after the snapshot, in order.
    session.feed(&output_frame(9, 2, b"post-snapshot"));
    let delta = next_frame(&mut out_rx, "post-snapshot delta rides");
    assert!(
        matches!(
            delta,
            FrameKind::ResourceOutput { ref terminal_id, seq: 2, .. }
                if *terminal_id == ResourceId::satellite("devbox", 9)
        ),
        "the delta must follow the snapshot, re-tagged, got {delta:?}"
    );
}
