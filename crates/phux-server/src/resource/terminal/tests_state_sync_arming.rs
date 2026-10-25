use super::*;

/// The tick timer is not armed on a pane with no consumer.
#[test]
fn the_state_tick_is_disarmed_with_nothing_to_emit() {
    let bundle = TerminalActor::new(20, 5).expect("new");
    let mut actor = bundle.actor;
    assert!(
        !actor.state_tick_armed(),
        "no consumer, no burst, no native cursor: nothing to tick for",
    );

    // A raw consumer is served by the broadcast pump, not the tick.
    let (raw_outbound, _raw_rx) = dummy_outbound();
    actor
        .register_consumer(ClientId(1), raw_outbound, 11, false)
        .expect("register raw consumer");
    assert!(
        !actor.state_tick_armed(),
        "a raw consumer is pumped, not ticked",
    );

    // A state-sync consumer is exactly what the tick exists for.
    let (sync_outbound, _sync_rx) = dummy_outbound();
    actor
        .register_consumer(ClientId(2), sync_outbound, 12, true)
        .expect("register state-sync consumer");
    assert!(
        actor.state_tick_armed(),
        "a state-sync consumer must re-arm the tick",
    );
}

/// A subscribed but caught-up pane needs no periodic render wakeup.
#[test]
fn caught_up_state_sync_disarms_and_mutation_rearms_tick() {
    let mut actor = TerminalActor::new(20, 5).expect("actor").actor;
    let (outbound, mut output) = dummy_outbound();
    actor
        .register_consumer(ClientId(1), outbound, 11, true)
        .expect("consumer");
    assert!(actor.state_tick_armed(), "initial consumer work");
    actor.tick_emit();
    while output.try_recv().is_ok() {}
    assert!(
        !actor.state_tick_armed(),
        "clean caught-up consumer must sleep"
    );
    actor.vt_write_for_test(b"wake");
    assert!(actor.state_tick_armed(), "new output rearms the timer");
    actor.tick_emit();
    assert!(output.try_recv().is_ok(), "mutation is delivered");
    assert!(!actor.state_tick_armed(), "caught up again");
}

#[test]
fn backpressure_keeps_clean_state_sync_timer_armed_until_retry() {
    let mut actor = TerminalActor::new(20, 5).expect("actor").actor;
    let (outbound, mut output) = mpsc::channel(1);
    actor
        .register_consumer(ClientId(1), outbound, 11, true)
        .expect("consumer");
    actor.vt_write_for_test(b"first");
    actor.tick_emit();
    actor.vt_write_for_test(b" second");
    actor.tick_emit();
    assert!(!actor.terminal_dirty_since_tick, "tick consumed mutation");
    assert!(
        actor.state_tick_armed(),
        "behind reference still owes a retry"
    );
    output.try_recv().expect("first frame");
    actor.tick_emit();
    assert!(
        output.try_recv().is_ok(),
        "retry delivers without another write"
    );
    assert!(!actor.state_tick_armed(), "retry caught up");
}

#[test]
fn gated_initial_consumer_keeps_timer_armed_until_live() {
    let mut actor = TerminalActor::new(20, 5).expect("actor").actor;
    let (outbound, _output) = dummy_outbound();
    actor
        .register_consumer(ClientId(1), outbound, 11, true)
        .expect("consumer");
    let (gate, live_gate) = watch::channel(false);
    actor
        .consumer_states
        .get_mut(&ClientId(1))
        .expect("state")
        .live_gate = live_gate;
    actor.tick_emit();
    assert!(
        actor.state_tick_armed(),
        "external gate changes need the initial tick obligation"
    );
    gate.send(true).expect("open gate");
    actor.tick_emit();
    assert!(!actor.state_tick_armed(), "opened and caught up");
}

/// An open output burst keeps the tick armed (it owes the `idle`).
#[test]
fn an_open_output_burst_arms_the_state_tick() {
    let bundle = TerminalActor::new(20, 5).expect("new");
    let mut actor = bundle.actor;
    let (event_tx, mut event_rx) = mpsc::channel(8);
    actor.set_event_sink(event_tx);
    assert!(!actor.state_tick_armed(), "quiet pane, disarmed");

    actor.feed_for_test(b"hello");
    assert!(
        matches!(
            event_rx.try_recv().map(|emitted| emitted.event),
            Ok(AgentEvent::Dirty)
        ),
        "the burst opened",
    );
    assert!(
        actor.state_tick_armed(),
        "an open burst owes an idle, so the tick must be armed",
    );

    // First tick sees the output flag and holds; the second settles the burst
    // and disarms again.
    actor.service_state_tick_for_test();
    assert!(actor.state_tick_armed(), "burst still open");
    actor.service_state_tick_for_test();
    assert!(
        matches!(
            event_rx.try_recv().map(|emitted| emitted.event),
            Ok(AgentEvent::Idle)
        ),
        "the settling idle still fires",
    );
    assert!(
        !actor.state_tick_armed(),
        "a settled pane disarms the tick again",
    );
}

/// Dirty/idle settling works on a raw-only pane with the emitter gated.
#[test]
fn raw_only_consumer_gets_repeatable_dirty_idle_cycles() {
    let bundle = TerminalActor::new(20, 5).expect("new");
    let mut actor = bundle.actor;
    let raw_client = ClientId(1);
    let (outbound, mut output_rx) = dummy_outbound();
    actor
        .register_consumer(raw_client, outbound, 11, false)
        .expect("register raw consumer");
    let (event_tx, mut event_rx) = mpsc::channel(8);
    actor.set_event_sink(event_tx);

    for chunk in [b"first".as_slice(), b"\r\nsecond".as_slice()] {
        actor.feed_for_test(chunk);
        assert!(
            matches!(
                event_rx.try_recv().map(|emitted| emitted.event),
                Ok(AgentEvent::Dirty)
            ),
            "each output burst must begin with dirty",
        );

        // The first tick sees output and keeps the burst open.
        actor.maybe_emit_idle();
        actor.tick_emit();
        assert!(event_rx.try_recv().is_err(), "first tick is not idle");

        // A following quiet tick settles independently of that still-set
        // state-sync flag, re-arming dirty for the next burst.
        actor.maybe_emit_idle();
        actor.tick_emit();
        assert!(
            matches!(
                event_rx.try_recv().map(|emitted| emitted.event),
                Ok(AgentEvent::Idle)
            ),
            "quiet tick must close the burst with idle",
        );
    }

    assert!(
        output_rx.try_recv().is_err(),
        "raw consumer must remain on the byte-faithful broadcast path",
    );
    assert!(
        actor.terminal_dirty_since_tick,
        "gated tick must not weaken state-sync dirty bookkeeping",
    );
}

#[test]
fn osc_progress_is_mirrored_without_event_listeners() {
    let bundle = TerminalActor::new(20, 5).expect("new");
    let mut actor = bundle.actor;
    assert!(actor.event_sink.is_none());

    actor.feed_for_test(b"\x1b]9;4;");
    assert!(
        actor.last_progress.is_empty(),
        "split mark is not complete yet"
    );
    actor.feed_for_test(b"3;\x07");
    assert_eq!(actor.last_progress, "4;3;");
    actor.feed_for_test(b"\x1b]9;4;0;\x07");
    assert_eq!(actor.last_progress, "4;0;");
}

/// phux-yeca: production defaults the synthesized tick emitter OFF so
/// human TUI attach stays on the immediate raw PTY broadcast path.
#[test]
fn tick_emit_gate_defaults_off_for_human_attach() {
    let bundle = TerminalActor::new(20, 5).expect("new");
    let mut actor = bundle.actor;
    let client = ClientId(1);
    let (tx, mut rx) = dummy_outbound();
    actor
        .register_consumer(client, tx, 11, false)
        .expect("register");
    actor.vt_write_for_test(b"dirty-content");

    actor.tick_emit();

    assert!(
        rx.try_recv().is_err(),
        "default human attach path must not wait for synthesized tick output",
    );
}

/// A `StateSync` consumer is served with the global test gate off.
#[test]
fn tick_emit_serves_negotiated_state_sync_consumer_with_gate_off() {
    let bundle = TerminalActor::new(20, 5).expect("new");
    let mut actor = bundle.actor;
    // NB: global gate left at its production default (OFF).
    let client = ClientId(1);
    let (tx, mut rx) = dummy_outbound();
    actor
        .register_consumer(client, tx, 11, true)
        .expect("register");
    actor.vt_write_for_test(b"state-sync-marker");

    actor.tick_emit();

    let frame = rx
        .try_recv()
        .expect("state-sync consumer must be served by the tick even with the gate off");
    let Outbound::Frame(FrameKind::ResourceOutput { seq, .. }) = frame else {
        panic!("expected a ResourceOutput frame for the state-sync consumer");
    };
    assert_eq!(seq, 1, "first tick emission stamps seq=1");
}

/// With the gate off, only the `StateSync` consumer of two is served.
#[test]
fn tick_emit_mixed_mode_serves_only_state_sync_consumer() {
    let bundle = TerminalActor::new(20, 5).expect("new");
    let mut actor = bundle.actor;
    let sync_client = ClientId(1);
    let raw_client = ClientId(2);
    let (sync_tx, mut sync_rx) = dummy_outbound();
    let (raw_tx, mut raw_rx) = dummy_outbound();
    actor
        .register_consumer(sync_client, sync_tx, 11, true)
        .expect("register state-sync");
    actor
        .register_consumer(raw_client, raw_tx, 12, false)
        .expect("register raw");
    actor.vt_write_for_test(b"shared-pane-write");

    actor.tick_emit();

    assert!(
        matches!(
            sync_rx.try_recv(),
            Ok(Outbound::Frame(FrameKind::ResourceOutput { .. })),
        ),
        "state-sync consumer must receive the synthesized delta",
    );
    assert!(
        raw_rx.try_recv().is_err(),
        "raw consumer must stay on the broadcast pump — tick must not double-paint it",
    );
}
