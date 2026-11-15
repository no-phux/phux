use super::*;

/// A tick-emitting actor with `ClientId(1)` registered as a raw consumer,
/// primed against the blank terminal (the gate is on before it registers).
fn emitting_actor() -> (TerminalActor, ClientId, mpsc::Receiver<Outbound>) {
    let mut actor = TerminalActor::new(20, 5).expect("new").actor;
    actor.enable_tick_emit_for_test();
    let client = ClientId(1);
    let (tx, rx) = dummy_outbound();
    actor
        .register_consumer(client, tx, 11, false)
        .expect("register");
    (actor, client, rx)
}

/// The next queued `RESOURCE_OUTPUT`: its `seq`, and whether its bytes
/// carry `marker`.
fn next_output(rx: &mut mpsc::Receiver<Outbound>, marker: &[u8]) -> (u64, bool) {
    match rx.try_recv() {
        Ok(Outbound::Frame(FrameKind::ResourceOutput { seq, bytes, .. })) => {
            (seq, contains_subslice(&bytes, marker))
        }
        other => panic!("expected a ResourceOutput frame, got {other:?}"),
    }
}

/// With the gate on, one tick ships one delta with `seq = 1`; an unchanged
/// grid then ships nothing; a new write ships `seq = 2`; an ack advances
/// `last_acked_seq` without emitting.
#[test]
fn tick_emit_emits_once_when_gate_is_on() {
    let (mut actor, client, mut rx) = emitting_actor();
    actor.vt_write_for_test(b"q0e-marker");

    actor.tick_emit();
    let frame = rx
        .try_recv()
        .expect("first tick must emit the changed grid");
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
    assert_eq!(terminal_id.local_id(), Some(11), "the registered wire id");
    assert!(contains_subslice(&bytes, b"q0e-marker"), "{bytes:?}");

    actor.tick_emit();
    assert!(rx.try_recv().is_err(), "emit-once: no re-send next tick");

    actor.vt_write_for_test(b" more");
    actor.tick_emit();
    assert_eq!(next_output(&mut rx, b"more"), (2, true));

    actor.on_frame_ack(client, 2);
    actor.tick_emit();
    assert!(
        rx.try_recv().is_err(),
        "an unchanged grid stays silent after ack"
    );
    let sync = &actor.consumer_state(client).expect("state").sync;
    assert_eq!((sync.next_seq, sync.last_acked_seq), (3, 2));
}

/// Two consumers on one pane both receive one tick's delta, whatever the
/// walk order, and detaching one does not perturb the other.
#[test]
fn tick_emit_serves_every_consumer_on_a_shared_pane() {
    let (mut actor, client_a, mut rx_a) = emitting_actor();
    let client_b = ClientId(2);
    let (tx_b, mut rx_b) = dummy_outbound();
    actor
        .register_consumer(client_b, tx_b, 11, false)
        .expect("register b");

    actor.vt_write_for_test(b"shared-marker");
    actor.tick_emit();
    for rx in [&mut rx_a, &mut rx_b] {
        assert_eq!(next_output(rx, b"shared-marker"), (1, true));
    }
    actor.tick_emit();
    assert!(
        rx_a.try_recv().is_err() && rx_b.try_recv().is_err(),
        "emit-once"
    );

    actor.unregister_consumer(client_a);
    actor.vt_write_for_test(b" again");
    actor.tick_emit();
    assert_eq!(next_output(&mut rx_b, b"again"), (2, true));
    assert!(rx_a.try_recv().is_err(), "a detached consumer gets nothing");
}

/// phux-4l0: an idle tick (no write since the last tick, no consumer
/// awaiting its first emission) short-circuits and emits nothing.
#[test]
fn idle_tick_short_circuits_and_emits_nothing() {
    let (mut actor, _client, mut rx) = emitting_actor();
    // The first tick walks the fresh consumer (empty diff).
    actor.tick_emit();
    while rx.try_recv().is_ok() {}

    actor.vt_write_for_test(b"hello");
    actor.tick_emit();
    assert!(next_output(&mut rx, b"hello").1);
    for _ in 0..5 {
        actor.tick_emit();
        assert!(rx.try_recv().is_err(), "idle tick must emit nothing");
    }
    assert_eq!(actor.consumer_count(), 1, "consumer entry intact");
}

/// A consumer registered after the last write is still walked on a clean
/// terminal (`needs_initial_emit`).
#[test]
fn new_consumer_served_even_when_terminal_clean() {
    let (mut actor, _client_a, mut rx_a) = emitting_actor();
    actor.vt_write_for_test(b"first");
    actor.tick_emit();
    while rx_a.try_recv().is_ok() {}
    actor.tick_emit();
    assert!(rx_a.try_recv().is_err(), "A steady-state: nothing");

    let client_b = ClientId(2);
    let (tx_b, mut rx_b) = dummy_outbound();
    actor
        .register_consumer(client_b, tx_b, 11, false)
        .expect("reg b");
    let awaiting = |actor: &TerminalActor| {
        let state = actor.consumer_state(client_b).expect("b present");
        state.sync.needs_initial_emit
    };
    assert!(awaiting(&actor), "B should be awaiting its first emission");
    actor.tick_emit();
    assert!(
        !awaiting(&actor),
        "B's first pass ran despite the Clean terminal"
    );
    assert!(rx_b.try_recv().is_err(), "B primed ⇒ empty first pass");

    actor.vt_write_for_test(b" again");
    actor.tick_emit();
    for rx in [&mut rx_a, &mut rx_b] {
        assert!(next_output(rx, b"again").1, "every consumer gets the write");
    }
}

/// A consumer whose receiver dropped without a detach is reaped by the
/// tick, whether or not its first (empty) pass already ran.
#[test]
fn tick_emit_reaps_consumer_with_closed_mailbox() {
    for primed in [false, true] {
        let (mut actor, _client, rx) = emitting_actor();
        if primed {
            actor.tick_emit();
        }
        assert_eq!(actor.consumer_count(), 1);
        drop(rx);
        actor.vt_write_for_test(b"content");
        actor.tick_emit();
        assert_eq!(actor.consumer_count(), 0, "primed={primed}: reaped");
        actor.vt_write_for_test(b"more");
        actor.tick_emit();
        assert_eq!(actor.consumer_count(), 0, "stays reaped");
    }
}
