use super::*;

/// With the gate on, one tick ships one delta with `seq = 1`; an unchanged
/// grid then ships nothing; a new write ships `seq = 2`.
#[test]
fn tick_emit_emits_once_when_gate_is_on() {
    let bundle = TerminalActor::new(20, 5).expect("new");
    let mut actor = bundle.actor;
    actor.enable_tick_emit_for_test();
    let client = ClientId(1);
    let (tx, mut rx) = dummy_outbound();
    // Primed against the blank terminal; the write after is the delta.
    actor
        .register_consumer(client, tx, 11, false)
        .expect("register");
    actor.vt_write_for_test(b"q0e-marker");

    actor.tick_emit();
    let frame = rx
        .try_recv()
        .expect("gate on: first tick must emit the changed grid");
    let Outbound::Frame(FrameKind::ResourceOutput {
        terminal_id,
        seq,
        bytes,
        ..
    }) = frame
    else {
        panic!("expected a ResourceOutput frame from tick_emit");
    };
    assert_eq!(seq, 1, "first tick emission stamps seq=1");
    assert_eq!(
        terminal_id.local_id(),
        Some(11),
        "tick frame carries the registered wire terminal id",
    );
    assert!(
        contains_subslice(&bytes, b"q0e-marker"),
        "tick emission must carry the seeded grid content; got {:?}",
        String::from_utf8_lossy(&bytes),
    );

    // Emit-once: no re-emission.
    actor.tick_emit();
    assert!(
        rx.try_recv().is_err(),
        "gate on: emit-once — an already-emitted change is not re-sent on the next tick",
    );

    // A fresh write produces a new single emission with the next seq.
    actor.vt_write_for_test(b" more");
    actor.tick_emit();
    let frame = rx
        .try_recv()
        .expect("gate on: a new write must emit a fresh diff");
    let Outbound::Frame(FrameKind::ResourceOutput { seq, bytes, .. }) = frame else {
        panic!("expected a ResourceOutput frame on the new write");
    };
    assert_eq!(seq, 2, "second distinct change stamps seq=2");
    assert!(
        contains_subslice(&bytes, b"more"),
        "second emission must carry the newly-written content; got {:?}",
        String::from_utf8_lossy(&bytes),
    );

    // FRAME_ACK advances last_acked_seq but does not itself trigger any
    // emission; the grid is unchanged so the next tick stays silent.
    actor.on_frame_ack(client, 2);
    actor.tick_emit();
    assert!(
        rx.try_recv().is_err(),
        "gate on: an unchanged grid stays silent after ack",
    );
    assert_eq!(
        actor.consumer_state(client).expect("state").next_seq,
        3,
        "two emissions advanced next_seq to 3; the post-ack tick was silent",
    );
    assert_eq!(
        actor.consumer_state(client).expect("state").last_acked_seq,
        2,
        "FRAME_ACK advanced last_acked_seq",
    );
}

/// Two consumers on one pane both receive one tick's delta, whatever the
/// walk order.
#[test]
fn tick_emit_serves_every_consumer_on_a_shared_pane() {
    let bundle = TerminalActor::new(20, 5).expect("new");
    let mut actor = bundle.actor;
    // Gate ON (production default).
    actor.enable_tick_emit_for_test();

    // Two consumers on the same pane, primed against the same blank
    // terminal.
    let client_a = ClientId(1);
    let client_b = ClientId(2);
    let (tx_a, mut rx_a) = dummy_outbound();
    let (tx_b, mut rx_b) = dummy_outbound();
    actor
        .register_consumer(client_a, tx_a, 11, false)
        .expect("register a");
    actor
        .register_consumer(client_b, tx_b, 11, false)
        .expect("register b");

    // One tick of new output AFTER both are primed.
    actor.vt_write_for_test(b"shared-marker");
    actor.tick_emit();

    // BOTH consumers must receive a ResourceOutput carrying the marker.
    let recv_marker = |rx: &mut mpsc::Receiver<Outbound>, who: &str| {
        let frame = rx
            .try_recv()
            .unwrap_or_else(|_| panic!("consumer {who} starved: no frame this tick"));
        let Outbound::Frame(FrameKind::ResourceOutput { seq, bytes, .. }) = frame else {
            panic!("consumer {who}: expected a ResourceOutput frame");
        };
        assert_eq!(seq, 1, "consumer {who}: first emission stamps seq=1");
        assert!(
            contains_subslice(&bytes, b"shared-marker"),
            "consumer {who}: incremental must carry the shared marker; got {:?}",
            String::from_utf8_lossy(&bytes),
        );
    };
    recv_marker(&mut rx_a, "A");
    recv_marker(&mut rx_b, "B");

    // Emit-once per consumer: with no further writes, neither consumer
    // gets a re-emission on the next tick.
    actor.tick_emit();
    assert!(
        rx_a.try_recv().is_err(),
        "consumer A: emit-once — no re-emission on an unchanged tick",
    );
    assert!(
        rx_b.try_recv().is_err(),
        "consumer B: emit-once — no re-emission on an unchanged tick",
    );

    // Detaching one does not perturb the other.
    actor.unregister_consumer(client_a);
    actor.vt_write_for_test(b" again");
    actor.tick_emit();
    let frame = rx_b.try_recv().expect("consumer B: must get the new write");
    let Outbound::Frame(FrameKind::ResourceOutput { seq, bytes, .. }) = frame else {
        panic!("consumer B: expected a ResourceOutput frame");
    };
    assert_eq!(seq, 2, "consumer B: second distinct change stamps seq=2");
    assert!(
        contains_subslice(&bytes, b"again"),
        "consumer B: must carry the second write; got {:?}",
        String::from_utf8_lossy(&bytes),
    );
    assert!(
        rx_a.try_recv().is_err(),
        "consumer A detached: must receive nothing further",
    );
}

/// phux-4l0: an idle tick (no write since the last tick, no consumer
/// awaiting its first emission) short-circuits and emits nothing.
#[test]
fn idle_tick_short_circuits_and_emits_nothing() {
    let bundle = TerminalActor::new(20, 5).expect("new");
    let mut actor = bundle.actor;
    actor.enable_tick_emit_for_test();

    let client = ClientId(1);
    let (tx, mut rx) = dummy_outbound();
    actor
        .register_consumer(client, tx, 11, false)
        .expect("register");

    // First tick walks the fresh consumer (empty diff) and consumes the flag.
    actor.tick_emit();
    // Drain whatever the first tick produced (expected: nothing, since
    // the reference was primed to the same blank state).
    while rx.try_recv().is_ok() {}

    // Now write, tick, drain: the consumer receives the write.
    actor.vt_write_for_test(b"hello");
    actor.tick_emit();
    let got = rx.try_recv().expect("write must reach the consumer");
    let Outbound::Frame(FrameKind::ResourceOutput { bytes, .. }) = got else {
        panic!("expected ResourceOutput");
    };
    assert!(contains_subslice(&bytes, b"hello"));

    // Many idle ticks (no further writes): the short-circuit must keep
    // each one silent and must not perturb the consumer entry.
    for _ in 0..5 {
        actor.tick_emit();
        assert!(
            rx.try_recv().is_err(),
            "idle tick must emit nothing (short-circuit)",
        );
    }
    assert_eq!(actor.consumer_count(), 1, "consumer entry intact");
}

/// A consumer registered after the last write is still walked on a clean
/// terminal (`needs_initial_emit`).
#[test]
fn new_consumer_served_even_when_terminal_clean() {
    let bundle = TerminalActor::new(20, 5).expect("new");
    let mut actor = bundle.actor;
    actor.enable_tick_emit_for_test();

    // Consumer A attaches, a write lands, a tick delivers it, then a
    // steady-state tick clears the dirty flag.
    let client_a = ClientId(1);
    let (tx_a, mut rx_a) = dummy_outbound();
    actor
        .register_consumer(client_a, tx_a, 11, false)
        .expect("reg a");
    actor.vt_write_for_test(b"first");
    actor.tick_emit();
    while rx_a.try_recv().is_ok() {}
    // Steady-state tick: terminal now Clean since last tick.
    actor.tick_emit();
    assert!(rx_a.try_recv().is_err(), "A steady-state: nothing");

    let client_b = ClientId(2);
    let (tx_b, mut rx_b) = dummy_outbound();
    actor
        .register_consumer(client_b, tx_b, 11, false)
        .expect("reg b");
    assert!(
        actor
            .consumer_state(client_b)
            .expect("b present")
            .needs_initial_emit,
        "B should be awaiting its first emission",
    );
    actor.tick_emit();
    // B primed to current state ⇒ empty body, but the pass ran:
    // needs_initial_emit is now cleared.
    assert!(
        !actor
            .consumer_state(client_b)
            .expect("b present")
            .needs_initial_emit,
        "B's first pass must have run despite the Clean terminal",
    );
    assert!(rx_b.try_recv().is_err(), "B primed ⇒ empty first pass");

    // A fresh write after both are primed reaches BOTH.
    actor.vt_write_for_test(b" again");
    actor.tick_emit();
    let frame_a = rx_a.try_recv().expect("A gets the new write");
    let frame_b = rx_b.try_recv().expect("B gets the new write");
    for (who, frame) in [("A", frame_a), ("B", frame_b)] {
        let Outbound::Frame(FrameKind::ResourceOutput { bytes, .. }) = frame else {
            panic!("{who}: expected ResourceOutput");
        };
        assert!(
            contains_subslice(&bytes, b"again"),
            "{who} must carry the new write",
        );
    }
}

/// A consumer whose receiver dropped without a detach is reaped by the tick.
#[test]
fn tick_emit_reaps_consumer_with_closed_mailbox() {
    let bundle = TerminalActor::new(20, 5).expect("new");
    let mut actor = bundle.actor;
    actor.enable_tick_emit_for_test();

    let client = ClientId(1);
    let (tx, rx) = dummy_outbound();
    actor
        .register_consumer(client, tx, 11, false)
        .expect("register");
    assert_eq!(actor.consumer_count(), 1);

    drop(rx);

    // A write makes the tick try to emit to the dead consumer; the
    // send fails Closed and the entry is reaped.
    actor.vt_write_for_test(b"content");
    actor.tick_emit();
    assert_eq!(
        actor.consumer_count(),
        0,
        "closed-mailbox consumer must be reaped by the tick",
    );

    // Subsequent ticks are stable no-ops.
    actor.vt_write_for_test(b"more");
    actor.tick_emit();
    assert_eq!(actor.consumer_count(), 0, "stays reaped");
}

/// A dead consumer is reaped even with an empty diff.
#[test]
fn tick_emit_reaps_idle_consumer_with_closed_mailbox() {
    let bundle = TerminalActor::new(20, 5).expect("new");
    let mut actor = bundle.actor;
    actor.enable_tick_emit_for_test();

    let client = ClientId(1);
    let (tx, rx) = dummy_outbound();
    actor
        .register_consumer(client, tx, 11, false)
        .expect("register");

    // Prime past the initial-emit pass with one tick (empty body).
    actor.tick_emit();
    assert_eq!(actor.consumer_count(), 1);

    drop(rx);
    actor.vt_write_for_test(b"x");
    actor.tick_emit();
    assert_eq!(
        actor.consumer_count(),
        0,
        "dead consumer reaped even though it never acked",
    );
}
