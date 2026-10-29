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

/// A fresh registration has no acks and a reference primed from the live
/// terminal.
#[test]
fn register_consumer_initial_state_matches_terminal() {
    let bundle = TerminalActor::new_with_seed(20, 5, b"hello").expect("new_with_seed");
    let mut actor = bundle.actor;
    let client = ClientId(7);
    let (tx, _rx) = dummy_outbound();
    // A tick-managed (state-sync) consumer: priming runs, so the
    // reference must reflect the live terminal.
    actor
        .register_consumer(client, tx, 11, true)
        .expect("register");

    let state = actor.consumer_state(client).expect("state present");
    assert_eq!(state.last_acked_seq, 0, "no acks yet");
    assert_eq!(state.next_seq, 1, "first emission gets seq=1");
    assert_eq!(
        state.wire_terminal_id, 11,
        "wire id stored on the per-consumer entry"
    );
    // Seeded "hello" puts the cursor at (5, 0).
    assert_eq!(state.reference.cursor_mode.cursor_x, Some(5));
    assert_eq!(state.reference.cursor_mode.cursor_y, Some(0));
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
    assert_eq!(state.next_seq, 1, "suppression must not advance sequence");
    assert!(
        state.needs_initial_emit,
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

/// A raw consumer registers with an empty reference, skipping the priming
/// renders.
#[test]
fn register_raw_consumer_skips_priming() {
    let bundle = TerminalActor::new_with_seed(20, 5, b"hello").expect("new_with_seed");
    let mut actor = bundle.actor;
    let client = ClientId(7);
    let (tx, _rx) = dummy_outbound();
    actor
        .register_consumer(client, tx, 11, false)
        .expect("register");

    let state = actor.consumer_state(client).expect("state present");
    // Not primed: no geometry, not the seeded cursor at (5, 0).
    assert_eq!((state.reference.cols, state.reference.rows), (0, 0));
    assert_eq!(state.reference.cursor_mode.cursor_x, None);
}

/// Attach then detach over the actor's channels on its `LocalSet`.
#[tokio::test(flavor = "current_thread")]
async fn consumer_attach_detach_round_trip_over_channels() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let bundle = TerminalActor::new(20, 5).expect("new");
            let handle = bundle.handle.clone();
            let token = bundle.token;
            let join = tokio::task::spawn_local(bundle.actor.run());

            let client = ClientId(42);
            let (out_tx, _out_rx) = dummy_outbound();
            let (tx_a, rx_a) = oneshot::channel();
            handle
                .consumer_attach
                .send(ConsumerAttachRequest {
                    image_protocols: phux_protocol::caps::ImageProtocolSet::all(),
                    client_id: client,
                    outbound: out_tx,
                    wire_terminal_id: 99,
                    stream_id: phux_protocol::ids::StreamId::new(43).expect("test stream id"),
                    bootstrap_id: phux_protocol::ids::BootstrapId::new(1)
                        .expect("test bootstrap id"),
                    wants_state_sync: false,
                    state_sync_scrollback: None,
                    bootstrap_max_bytes: usize::MAX,
                    bootstrap_max_frames: usize::MAX,
                    bootstrap_chunk_bytes: 1,
                    loss_tolerant: false,
                    live_gate: watch::channel(true).1,
                    reply: tx_a,
                })
                .await
                .expect("send attach");
            rx_a.await.expect("attach reply").expect("attach succeeded");

            let (tx_d, rx_d) = oneshot::channel();
            handle
                .consumer_detach
                .send(ConsumerDetachRequest {
                    client_id: client,
                    reply: tx_d,
                })
                .await
                .expect("send detach");
            rx_d.await.expect("detach reply");

            token.cancel();
            tokio::time::timeout(ACTOR_EXIT_DEADLINE, join)
                .await
                .expect("actor did not exit after cancel")
                .expect("actor task panicked");
        })
        .await;
}
