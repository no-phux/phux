use super::*;

// ---- phux-q0e.5: RTT-adaptive tick interval ----
//
// The estimator, clamping, and the emit-instant cap are unit-tested where
// they live (`phux_resource::replication`); these pin the actor's wiring.

/// Through the actor, a slow ack backs the cadence off and a fast one pins
/// the floor (paused time).
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn actor_cadence_backs_off_on_high_rtt_and_floors_on_low() {
    let bundle = TerminalActor::new(80, 24).expect("new");
    let mut actor = bundle.actor;
    actor.enable_tick_emit_for_test();

    // Slow peer: a write + tick emits seq=1 and stamps its emit instant.
    let slow = ClientId(1);
    let (tx_slow, _rx_slow) = mpsc::channel::<Outbound>(16);
    actor
        .register_consumer(slow, tx_slow, 11, false)
        .expect("register slow");
    assert_eq!(
        actor.adaptive_tick_interval_for_test(),
        DEFAULT_TICK_INTERVAL,
        "no sample yet -> cold-start cadence",
    );

    actor.vt_write_for_test(b"hello\r\n");
    actor.tick_emit();
    // Simulate a 400ms round trip before the client acks seq=1.
    tokio::time::advance(std::time::Duration::from_millis(400)).await;
    assert!(
        actor.on_frame_ack_for_test(slow, 1),
        "ack of an emitted seq produces an RTT sample",
    );
    // 400ms RTT -> 200ms RTT/2 -> clamps to the 200ms ceiling.
    assert_eq!(
        actor.adaptive_tick_interval_for_test(),
        MAX_TICK_INTERVAL,
        "high-RTT consumer backs the cadence off to the ceiling",
    );

    // The shared cadence is the minimum over consumers.
    let fast = ClientId(2);
    let (tx_fast, _rx_fast) = mpsc::channel::<Outbound>(16);
    actor
        .register_consumer(fast, tx_fast, 12, false)
        .expect("register fast");
    actor.vt_write_for_test(b"world\r\n");
    actor.tick_emit();
    // Near-instant ack: advance time by a sub-millisecond sliver.
    tokio::time::advance(std::time::Duration::from_micros(50)).await;
    // Both consumers were emitted to on the tick above; the fast peer's
    // first emitted seq is 1.
    assert!(
        actor.on_frame_ack_for_test(fast, 1),
        "fast peer ack produces a sample",
    );
    assert_eq!(
        actor.adaptive_tick_interval_for_test(),
        MIN_TICK_INTERVAL,
        "the fastest consumer pins the shared cadence to the floor",
    );

    // Losing all sampled peers reverts to the default.
    actor.unregister_consumer(slow);
    assert_eq!(
        actor.adaptive_tick_interval_for_test(),
        MIN_TICK_INTERVAL,
        "fast peer still present -> still at the floor",
    );
    actor.unregister_consumer(fast);
    assert_eq!(
        actor.adaptive_tick_interval_for_test(),
        DEFAULT_TICK_INTERVAL,
        "no consumers left -> cold-start default",
    );
}

/// An ack with no emit instant yields no sample.
#[test]
fn ack_without_emit_instant_produces_no_sample() {
    let bundle = TerminalActor::new(80, 24).expect("new");
    let mut actor = bundle.actor;
    actor.enable_tick_emit_for_test();

    let client = ClientId(1);
    let (tx, _rx) = dummy_outbound();
    actor
        .register_consumer(client, tx, 11, false)
        .expect("register");

    // No tick_emit ran, so no emit instant was stamped. An ack here
    // advances last_acked_seq but cannot time a round trip.
    assert!(
        !actor.on_frame_ack_for_test(client, 5),
        "ack with no matching emit instant produces no RTT sample",
    );
    assert_eq!(
        actor.adaptive_tick_interval_for_test(),
        DEFAULT_TICK_INTERVAL,
        "cadence stays at the default without a sample",
    );
}
