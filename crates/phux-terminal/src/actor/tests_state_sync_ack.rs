use super::*;

/// phux-q0e.4: `on_frame_ack` advances `last_acked_seq` monotonically
/// for in-order acks. Three acks (1, 2, 3) walk the field forward.
#[test]
fn on_frame_ack_advances_last_acked_seq_in_order() {
    let bundle = TerminalActor::new_with_seed(20, 5, b"hello").expect("new_with_seed");
    let mut actor = bundle.actor;
    let client = ClientId(1);
    let (tx, _rx) = dummy_outbound();
    // State-sync consumer: its acks belong to the per-consumer tick seq
    // space, so `on_frame_ack` folds them in (phux-38k6).
    actor
        .register_consumer(client, tx, 11, true)
        .expect("register");

    for seq in 1..=3 {
        actor.vt_write_for_test(b"x");
        actor.tick_emit();
        actor.on_frame_ack(client, seq);
        assert_eq!(
            actor.consumer_state(client).expect("state").last_acked_seq,
            seq,
            "in-order ack must advance last_acked_seq",
        );
    }
}

/// Older or duplicate acks are dropped (cumulative, SPEC §12.2).
#[test]
fn on_frame_ack_older_or_duplicate_is_dropped() {
    let bundle = TerminalActor::new_with_seed(20, 5, b"hello").expect("new_with_seed");
    let mut actor = bundle.actor;
    let client = ClientId(1);
    let (tx, _rx) = dummy_outbound();
    // State-sync consumer so its acks are processed (phux-38k6).
    actor
        .register_consumer(client, tx, 11, true)
        .expect("register");

    for _ in 0..6 {
        actor.vt_write_for_test(b"x");
        actor.tick_emit();
    }
    actor.on_frame_ack(client, 5);
    assert_eq!(actor.consumer_state(client).unwrap().last_acked_seq, 5);

    // Older ack.
    actor.on_frame_ack(client, 3);
    assert_eq!(
        actor.consumer_state(client).unwrap().last_acked_seq,
        5,
        "older ack must NOT regress last_acked_seq",
    );

    // Duplicate ack.
    actor.on_frame_ack(client, 5);
    assert_eq!(
        actor.consumer_state(client).unwrap().last_acked_seq,
        5,
        "duplicate ack must NOT touch last_acked_seq",
    );

    // Higher ack still progresses.
    actor.on_frame_ack(client, 6);
    assert_eq!(actor.consumer_state(client).unwrap().last_acked_seq, 6);
}

/// A raw consumer's ack (a pump-local seq) does not move `last_acked_seq`.
#[test]
fn on_frame_ack_for_raw_consumer_is_dropped() {
    let bundle = TerminalActor::new_with_seed(20, 5, b"hello").expect("new_with_seed");
    let mut actor = bundle.actor;
    // Global gate OFF (production human-attach default) and a raw consumer.
    let client = ClientId(1);
    let (tx, _rx) = dummy_outbound();
    actor
        .register_consumer(client, tx, 11, false)
        .expect("register");

    let folded = actor.on_frame_ack(client, 7);
    assert!(!folded, "raw-consumer ack produces no RTT sample");
    assert_eq!(
        actor.consumer_state(client).expect("state").last_acked_seq,
        0,
        "raw-pump ack must not advance the per-consumer last_acked_seq",
    );
}

/// An ack for an unregistered client is a no-op.
#[test]
fn on_frame_ack_for_unregistered_consumer_is_noop() {
    let bundle = TerminalActor::new_with_seed(20, 5, b"hello").expect("new_with_seed");
    let mut actor = bundle.actor;

    let stranger = ClientId(999);
    assert_eq!(actor.consumer_count(), 0);
    actor.on_frame_ack(stranger, 42);
    assert_eq!(actor.consumer_count(), 0, "no entry created by stray ack");
    assert!(actor.consumer_state(stranger).is_none());
}

/// phux-q0e.4: register, ack, then detach, then re-ack — the re-ack
/// after detach is a no-op (no panic, no resurrection of the entry).
#[test]
fn on_frame_ack_after_detach_is_noop() {
    let bundle = TerminalActor::new_with_seed(20, 5, b"hello").expect("new_with_seed");
    let mut actor = bundle.actor;
    let client = ClientId(7);
    let (tx, _rx) = dummy_outbound();
    // State-sync so the pre-detach ack is folded in (phux-38k6); the
    // point of the test is that a *post*-detach ack does not resurrect.
    actor
        .register_consumer(client, tx, 11, true)
        .expect("register");
    for _ in 0..2 {
        actor.vt_write_for_test(b"x");
        actor.tick_emit();
    }
    actor.on_frame_ack(client, 2);
    assert_eq!(actor.consumer_state(client).unwrap().last_acked_seq, 2);

    actor.unregister_consumer(client);
    assert!(actor.consumer_state(client).is_none());

    // Late ack after detach: must not resurrect the entry.
    actor.on_frame_ack(client, 9);
    assert!(actor.consumer_state(client).is_none());
    assert_eq!(actor.consumer_count(), 0);
}

#[test]
fn future_ack_cannot_erase_pending_data_or_poison_later_acknowledgements() {
    let mut actor = TerminalActor::new_with_seed(20, 5, b"hello").unwrap().actor;
    let client = ClientId(1);
    let (tx, _rx) = dummy_outbound();
    actor.register_consumer(client, tx, 11, true).unwrap();
    actor.enable_loss_tolerance(client);
    actor.vt_write_for_test(b"one");
    actor.tick_emit();
    let next = actor.consumer_state(client).unwrap().next_seq;
    assert_eq!(next, 2);
    let state = actor.consumer_state(client).unwrap();
    let pending = state.emit_instants.len();
    let stream = state.stream_id;
    let bootstrap = state.bootstrap_id;
    let acked_rows = state.acked_reference.rows_body.clone();
    assert_eq!(state.pending_refs.len(), 1);
    for (stream_id, bootstrap_id, seq) in [
        (stream, bootstrap, next),
        (stream, bootstrap, u64::MAX),
        (
            phux_protocol::ids::StreamId::new(999).unwrap(),
            bootstrap,
            1,
        ),
        (
            stream,
            phux_protocol::ids::BootstrapId::new(999).unwrap(),
            1,
        ),
    ] {
        assert!(!actor.on_generation_frame_ack(client, stream_id, bootstrap_id, seq));
        let state = actor.consumer_state(client).unwrap();
        assert_eq!(state.last_acked_seq, 0);
        assert_eq!(state.emit_instants.len(), pending);
        assert_eq!(state.pending_refs.len(), 1);
        assert_eq!(state.acked_reference.rows_body, acked_rows);
        assert!(state.rtt.smoothed().is_none());
    }
    assert!(actor.on_generation_frame_ack(client, stream, bootstrap, 1));
    let state = actor.consumer_state(client).unwrap();
    assert_eq!(state.last_acked_seq, 1);
    assert!(state.emit_instants.is_empty());
    assert!(state.pending_refs.is_empty());
    assert_ne!(state.acked_reference.rows_body, acked_rows);
}

#[test]
fn ack_between_pty_mutation_and_tick_preserves_pending_row_changes() {
    let mut actor = TerminalActor::new_with_seed(20, 5, b"hello").unwrap().actor;
    let (tx, _rx) = dummy_outbound();
    actor.register_consumer(ClientId(1), tx, 11, true).unwrap();
    actor.vt_write_for_test(b"\x1b[1;1Hfirst");
    actor.tick_emit();
    actor.vt_write_for_test(b"\x1b[2;1HSECOND\x1b[1;6H");
    assert!(actor.on_frame_ack(ClientId(1), 1));
    actor.tick_emit();
    let state = actor.consumer_state(ClientId(1)).unwrap();
    assert!(
        state.reference.rows_body[1]
            .windows(6)
            .any(|bytes| bytes == b"SECOND")
    );
}
