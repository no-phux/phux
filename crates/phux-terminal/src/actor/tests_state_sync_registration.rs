use super::*;

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
            CancellationToken::new(),
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
