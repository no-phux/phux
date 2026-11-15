use super::*;

/// Two attaches make two entries; a detach removes one; a repeat detach is
/// a no-op.
#[test]
fn register_unregister_consumer_drives_lifecycle_map() {
    let bundle = TerminalActor::new(20, 5).expect("new");
    let mut actor = bundle.actor;
    assert_eq!(actor.consumer_count(), 0, "starts empty");

    let a = ClientId(1);
    let b = ClientId(2);
    let (tx_a, _rx_a) = dummy_outbound();
    let (tx_b, _rx_b) = dummy_outbound();
    actor
        .register_consumer(a, tx_a, 1, false)
        .expect("register a");
    assert_eq!(actor.consumer_count(), 1);
    actor
        .register_consumer(b, tx_b, 2, false)
        .expect("register b");
    assert_eq!(actor.consumer_count(), 2);

    actor.unregister_consumer(a);
    assert_eq!(actor.consumer_count(), 1, "one entry after first detach");
    assert!(actor.consumer_state(a).is_none(), "a removed");
    assert!(actor.consumer_state(b).is_some(), "b retained");

    // Idempotent detach: re-detaching `a` is a no-op.
    actor.unregister_consumer(a);
    assert_eq!(actor.consumer_count(), 1);

    actor.unregister_consumer(b);
    assert_eq!(actor.consumer_count(), 0, "both removed");
}

/// A fresh state-sync registration has no acks and a reference primed from
/// the live terminal; a raw one skips the priming renders entirely.
#[test]
fn register_consumer_primes_only_tick_managed_references() {
    let mut actor = TerminalActor::new_with_seed(20, 5, b"hello")
        .expect("new_with_seed")
        .actor;
    let (synced, raw) = (ClientId(7), ClientId(8));
    let (tx, _rx) = dummy_outbound();
    actor
        .register_consumer(synced, tx.clone(), 11, true)
        .expect("register");
    actor
        .register_consumer(raw, tx, 11, false)
        .expect("register raw");

    let state = actor.consumer_state(synced).expect("state present");
    assert_eq!((state.sync.last_acked_seq, state.sync.next_seq), (0, 1));
    assert_eq!(state.wire_terminal_id, 11, "wire id stored on the entry");
    // Seeded "hello" puts the cursor at (5, 0).
    let cursor = &state.sync.reference.cursor_mode;
    assert_eq!((cursor.cursor_x, cursor.cursor_y), (Some(5), Some(0)));

    let reference = &actor.consumer_state(raw).expect("raw state").sync.reference;
    assert_eq!((reference.cols, reference.rows), (0, 0), "raw: not primed");
    assert_eq!(reference.cursor_mode.cursor_x, None);
}

#[test]
fn aggregate_live_gate_preserves_first_delta_until_activation() {
    let bundle = TerminalActor::new(20, 5).expect("new");
    let mut actor = bundle.actor;
    let client = ClientId(8);
    let (outbound, mut outbound_rx) = dummy_outbound();
    let (gate_tx, gate_rx) = watch::channel(false);
    let next_seq = actor.core.seq().checked_add(1).expect("next live sequence");
    actor
        .register_consumer_generation(
            client,
            outbound,
            12,
            phux_protocol::ids::StreamId::new(9).expect("stream id"),
            phux_protocol::ids::BootstrapId::new(3).expect("bootstrap id"),
            true,
            gate_rx,
            next_seq,
        )
        .expect("register");

    actor.vt_write_for_test(b"after-cut");
    actor.tick_emit();
    assert!(
        outbound_rx.try_recv().is_err(),
        "live output must remain behind the aggregate ATTACH barrier"
    );
    let state = actor.consumer_state(client).expect("consumer state");
    assert_eq!(
        state.sync.next_seq, 1,
        "suppression must not advance sequence"
    );
    assert!(
        state.sync.needs_initial_emit,
        "suppression must not consume the diff"
    );

    gate_tx.send(true).expect("activate live output");
    actor.tick_emit();
    let Outbound::Frame(FrameKind::ResourceOutput { seq, bytes, .. }) =
        outbound_rx.try_recv().expect("first post-barrier delta")
    else {
        panic!("expected terminal output");
    };
    assert_eq!(seq, 1);
    assert!(!bytes.is_empty());
}
