use super::*;

/// With the emission gate off, `tick_emit` ships nothing to a raw consumer,
/// even with dirty content (no double-paint beside the pump).
#[test]
fn tick_emit_is_silent_while_gate_is_off() {
    let bundle = TerminalActor::new(20, 5).expect("new");
    let mut actor = bundle.actor;
    actor.disable_tick_emit_for_test();
    let client = ClientId(1);
    let (tx, mut rx) = dummy_outbound();
    actor
        .register_consumer(client, tx, 11, false)
        .expect("register");
    // Make the grid genuinely dirty AFTER register so a non-gated tick
    // would have something to emit — proving the gate, not an empty diff.
    actor.vt_write_for_test(b"dirty-content");

    // Several ticks: the gate must keep every one silent.
    for _ in 0..3 {
        actor.tick_emit();
    }
    assert!(
        rx.try_recv().is_err(),
        "gate off: tick_emit must not emit while the broadcast pump is the live path",
    );
    // The per-consumer entry is still live (lifecycle is active) —
    // only emission is suppressed.
    assert_eq!(actor.consumer_count(), 1);
    assert_eq!(
        actor.consumer_state(client).expect("state").next_seq,
        1,
        "no emission means the per-consumer seq never advanced",
    );
}

#[test]
fn atomic_state_sync_bootstrap_primes_exact_cut_and_sequence() {
    let bundle = TerminalActor::new(20, 5).expect("new");
    let mut actor = bundle.actor;
    actor.core.seq = 7;
    actor.vt_write_for_test(b"before-cut");

    let client = ClientId(1);
    let (outbound, mut outbound_rx) = dummy_outbound();
    let (live_gate_tx, live_gate) = watch::channel(false);
    let (reply, mut replied) = oneshot::channel();
    actor.handle_consumer_attach(ConsumerAttachRequest {
        image_protocols: phux_protocol::caps::ImageProtocolSet::all(),
        client_id: client,
        outbound,
        wire_terminal_id: 11,
        stream_id: phux_protocol::ids::StreamId::new(1).expect("stream id"),
        bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("bootstrap id"),
        wants_state_sync: true,
        state_sync_scrollback: None,
        bootstrap_max_bytes: usize::MAX,
        bootstrap_max_frames: usize::MAX,
        bootstrap_chunk_bytes: 1,
        loss_tolerant: false,
        live_gate,
        reply,
    });

    let outcome = replied
        .try_recv()
        .expect("atomic attach reply")
        .expect("atomic attach");
    let bootstrap = outcome.state_sync_bootstrap.expect("state-sync bootstrap");
    assert_eq!(bootstrap.base_seq, 7);
    assert!(
        contains_subslice(&bootstrap.snapshot.bytes, b"before-cut"),
        "snapshot must include the exact pre-registration terminal cut",
    );
    assert_eq!(actor.consumer_state(client).expect("consumer").next_seq, 8,);

    actor.tick_emit();
    assert!(
        outbound_rx.try_recv().is_err(),
        "closed aggregate gate must retain the primed reference",
    );
    live_gate_tx.send(true).expect("open aggregate gate");
    actor.tick_emit();
    assert!(
        outbound_rx.try_recv().is_err(),
        "opening the gate at the exact bootstrap cut must emit no duplicate"
    );
    assert_eq!(
        actor.consumer_state(client).expect("consumer").next_seq,
        8,
        "an empty exact-cut tick must not consume the first live sequence"
    );

    actor.vt_write_for_test(b"\r\nafter-cut");
    actor.tick_emit();
    let Outbound::Frame(FrameKind::ResourceOutput { seq, bytes, .. }) =
        outbound_rx.try_recv().expect("post-cut diff")
    else {
        panic!("expected terminal output");
    };
    assert_eq!(seq, 8, "first live diff is exactly base_seq + 1");
    assert!(
        !contains_subslice(&bytes, b"before-cut"),
        "the first diff must not duplicate snapshot content",
    );
    assert!(contains_subslice(&bytes, b"after-cut"));
    assert_eq!(
        actor.consumer_state(client).expect("consumer").next_seq,
        9,
        "one post-cut delta consumes exactly one sequence"
    );
}
