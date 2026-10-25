use super::*;

// ---- phux-q0e.5: RTT-adaptive tick interval ----

/// The EMA seeds on the first sample and converges toward a steady RTT
/// across a handful of samples (TCP-RTO-style `α = 0.125`).
#[test]
fn rtt_estimator_seeds_then_converges() {
    let mut est = RttEstimator::default();
    assert_eq!(est.smoothed(), None, "no sample yet");

    // First sample seeds srtt directly.
    est.observe(std::time::Duration::from_millis(100));
    assert_eq!(
        est.smoothed(),
        Some(std::time::Duration::from_millis(100)),
        "first sample seeds srtt exactly",
    );

    // Feed a steady 100ms stream; srtt must stay put (no drift).
    for _ in 0..20 {
        est.observe(std::time::Duration::from_millis(100));
    }
    let srtt = est.smoothed().expect("has srtt");
    assert!(
        srtt.abs_diff(std::time::Duration::from_millis(100)) < std::time::Duration::from_millis(1),
        "steady 100ms stream keeps srtt ~100ms, got {srtt:?}",
    );

    // Step the RTT up to 300ms; the EMA moves slowly (one step folds in
    // only α of the gap), then converges over many samples.
    let before = est.smoothed().expect("has srtt");
    est.observe(std::time::Duration::from_millis(300));
    let after_one = est.smoothed().expect("has srtt");
    assert!(
        after_one > before && after_one < std::time::Duration::from_millis(150),
        "one 300ms sample nudges srtt but does not jump to it: {before:?} -> {after_one:?}",
    );
    for _ in 0..100 {
        est.observe(std::time::Duration::from_millis(300));
    }
    let converged = est.smoothed().expect("has srtt");
    assert!(
        converged.abs_diff(std::time::Duration::from_millis(300))
            < std::time::Duration::from_millis(2),
        "srtt converges toward the new 300ms RTT, got {converged:?}",
    );
}

/// The adaptive interval is `RTT/2` clamped to [20 ms, 200 ms].
#[test]
fn adaptive_interval_clamps_both_ends() {
    // Near-zero local RTT -> floor (50 Hz), strictly faster than the
    // fixed 33 Hz default this replaces.
    assert_eq!(
        adaptive_tick_interval(std::time::Duration::from_micros(10)),
        MIN_TICK_INTERVAL,
        "near-zero RTT clamps to the 20ms floor",
    );
    assert!(
        MIN_TICK_INTERVAL < DEFAULT_TICK_INTERVAL,
        "the floor must be snappier than the old fixed cadence",
    );

    // Mid-band: 80ms RTT -> 40ms tick (unclamped RTT/2).
    assert_eq!(
        adaptive_tick_interval(std::time::Duration::from_millis(80)),
        std::time::Duration::from_millis(40),
        "mid-band RTT maps to exactly RTT/2",
    );

    // Satellite-class RTT -> ceiling (5 Hz).
    assert_eq!(
        adaptive_tick_interval(std::time::Duration::from_secs(2)),
        MAX_TICK_INTERVAL,
        "huge RTT clamps to the 200ms ceiling",
    );
}

/// An estimator with no sample reports the cold-start default; once a
/// sample lands, `desired_tick_interval` tracks the clamped RTT/2.
#[test]
fn desired_interval_defaults_then_adapts() {
    let mut est = RttEstimator::default();
    assert_eq!(
        est.desired_tick_interval(),
        DEFAULT_TICK_INTERVAL,
        "no sample -> cold-start default",
    );
    est.observe(std::time::Duration::from_millis(120));
    assert_eq!(
        est.desired_tick_interval(),
        std::time::Duration::from_millis(60),
        "after a 120ms sample -> 60ms tick",
    );
}

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

/// A never-acking consumer's `emit_instants` stays capped at
/// [`MAX_EMIT_INSTANTS`], keeping the newest.
#[test]
fn emit_instants_is_capped_for_never_acking_consumer() {
    let bundle = TerminalActor::new(20, 5).expect("new");
    let mut actor = bundle.actor;
    let client = ClientId(1);
    let (tx, mut rx) = dummy_outbound();
    actor
        .register_consumer(client, tx, 11, true)
        .expect("register");

    // Distinct content per tick and a drained mailbox keep every tick
    // emitting.
    let ticks = MAX_EMIT_INSTANTS + 64;
    for i in 0..ticks {
        actor.vt_write_for_test(&[b'a' + u8::try_from(i % 26).expect("0..26 fits u8")]);
        actor.tick_emit();
        while rx.try_recv().is_ok() {}
    }

    let state = actor.consumer_state(client).expect("state present");
    assert!(
        state.emit_instants.len() <= MAX_EMIT_INSTANTS,
        "emit_instants must stay capped at {} for a never-acking consumer; got {}",
        MAX_EMIT_INSTANTS,
        state.emit_instants.len(),
    );
    let lowest = *state.emit_instants.keys().next().expect("non-empty map");
    assert!(
        lowest > 1,
        "oldest emit instants should have been evicted; lowest retained seq = {lowest}",
    );
}
