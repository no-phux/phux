//! State-sync tests: consumer registration, `FRAME_ACK`, tick
//! emission, loss tolerance, backpressure, the RTT-adaptive cadence,
//! the detector tick, and input-snapshot publication.

use super::test_support::*;
use super::*;

/// phux-q0e.2: ATTACH allocates a per-consumer `RenderState` and
/// `register_consumer` stores it keyed by `ClientId`. Two attaches
/// land two entries; one detach removes only that entry; a second
/// detach of the same id is a no-op.
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

/// phux-q0e.2: right after `register_consumer` returns, the
/// per-consumer state has `last_acked_seq == 0` (no `FRAME_ACK`s yet
/// — wired by phux-q0e.4) and the cursor/mode capture matches the
/// live terminal. The dirty-bit reset is a best-effort FFI call
/// (phux-l0t notes the libghostty surface is unreliable on
/// repeated updates); we assert the observable contract — the
/// `ConsumerSyncState` is in place and primed against the live
/// terminal — rather than the post-reset dirty value itself, which
/// the tick driver (phux-q0e.3) will re-read on its first tick.
#[test]
fn register_consumer_initial_state_matches_terminal() {
    let bundle = TerminalActor::new_with_seed(20, 5, b"hello").expect("new_with_seed");
    let mut actor = bundle.actor;
    let client = ClientId(7);
    let (tx, _rx) = dummy_outbound();
    // A tick-managed (state-sync) consumer: priming runs, so the
    // capture must reflect the live terminal.
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
    // Seeded "hello" advances the cursor to (5, 0). The capture
    // must reflect that — proves the RenderState was actually
    // updated against the live terminal, not left blank.
    assert_eq!(state.last_cursor_mode.cursor_x, Some(5));
    assert_eq!(state.last_cursor_mode.cursor_y, Some(0));
}

#[test]
fn aggregate_live_gate_preserves_first_delta_until_activation() {
    let bundle = TerminalActor::new(20, 5).expect("new");
    let mut actor = bundle.actor;
    let client = ClientId(8);
    let (outbound, mut outbound_rx) = dummy_outbound();
    let (gate_tx, gate_rx) = watch::channel(false);
    let next_seq = actor.raw_seq.checked_add(1).expect("next live sequence");
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
    let Outbound::Frame(FrameKind::TerminalOutput { seq, bytes, .. }) =
        outbound_rx.try_recv().expect("first post-barrier delta")
    else {
        panic!("expected terminal output");
    };
    assert_eq!(seq, 1);
    assert!(!bytes.is_empty());
}

/// A raw broadcast-pump consumer (the human attach path) skips the two
/// full-grid render passes priming would cost: its reference and
/// cursor/mode capture are never read (the tick serves only tick-managed
/// consumers). So it registers with the `unprimed` placeholder, not a
/// live capture (phux-ahk register-prime gating).
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
    // Not primed: the placeholder capture, not the seeded cursor at (5, 0).
    assert_eq!(state.last_cursor_mode.cursor_x, None);
    assert_eq!(state.last_cursor_mode.cursor_y, None);
}

/// phux-q0e.2: end-to-end across the actor's `select!` loop —
/// ATTACH then DETACH over the channels handle the lifecycle on
/// the same `LocalSet` thread the `Terminal` lives on. Drives the
/// actor through `spawn_local`, so the `!Send` `RenderState`
/// stays on its owning thread.
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
        actor.on_frame_ack(client, seq);
        assert_eq!(
            actor.consumer_state(client).expect("state").last_acked_seq,
            seq,
            "in-order ack must advance last_acked_seq",
        );
    }
}

/// phux-q0e.4: older or duplicate acks (`seq <= last_acked_seq`) MUST
/// be silently dropped — they carry no new state information under
/// SPEC §12.2's cumulative-ack semantics. After ack=5 then ack=3, the
/// field must stay at 5.
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

/// phux-38k6: a `FRAME_ACK` from a raw (broadcast-pump) consumer carries a
/// pump-local seq unrelated to this per-consumer tick state, so
/// `on_frame_ack` drops it — `last_acked_seq` must NOT move. Otherwise a
/// foreign counter would skew the RTT/backpressure accounting if the
/// consumer later went state-sync.
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

/// phux-q0e.4: `on_frame_ack` for an unregistered client is a silent
/// no-op — no panic, no entry created. Mirrors the rest of the
/// consumer lifecycle's idempotency.
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
    actor.on_frame_ack(client, 2);
    assert_eq!(actor.consumer_state(client).unwrap().last_acked_seq, 2);

    actor.unregister_consumer(client);
    assert!(actor.consumer_state(client).is_none());

    // Late ack after detach: must not resurrect the entry.
    actor.on_frame_ack(client, 9);
    assert!(actor.consumer_state(client).is_none());
    assert_eq!(actor.consumer_count(), 0);
}

/// phux-0q8 coexistence gate: with a consumer registered but the
/// emission gate forced OFF (`consumer_tick_emits == false`),
/// `tick_emit` MUST NOT push any frame onto the consumer's outbound
/// mailbox — even with dirty seeded content. This is the invariant
/// that lets the per-consumer lifecycle run live alongside the
/// broadcast pump without double-painting the client when the gate is
/// off. Production defaults the gate OFF for human attach (phux-yeca),
/// but this test still disables it explicitly so the invariant is local.
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
    actor.raw_seq = 7;
    actor.vt_write_for_test(b"before-cut");

    let client = ClientId(1);
    let (outbound, mut outbound_rx) = dummy_outbound();
    let (live_gate_tx, live_gate) = watch::channel(false);
    let (reply, mut replied) = oneshot::channel();
    actor.handle_consumer_attach(ConsumerAttachRequest {
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
    let Outbound::Frame(FrameKind::TerminalOutput { seq, bytes, .. }) =
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

/// phux-bowo: dirty/idle settling is independent of the state-sync
/// emitter gate. A raw-only pane must produce a fresh pair for each
/// output burst even though `tick_emit` stays gated and therefore leaves
/// `terminal_dirty_since_tick` set.
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
        actor.vt_write_for_test(chunk);
        actor.source_events_from_chunk(chunk);
        assert!(
            matches!(event_rx.try_recv(), Ok(AgentEvent::Dirty)),
            "each output burst must begin with dirty",
        );

        // The first tick observes output and keeps the burst open. The
        // gated tick emits no synthesized output and does not consume its
        // state-sync dirty flag.
        actor.maybe_emit_idle();
        actor.tick_emit();
        assert!(event_rx.try_recv().is_err(), "first tick is not idle");

        // A following quiet tick settles independently of that still-set
        // state-sync flag, re-arming dirty for the next burst.
        actor.maybe_emit_idle();
        actor.tick_emit();
        assert!(
            matches!(event_rx.try_recv(), Ok(AgentEvent::Idle)),
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
    assert!(actor.event_subscribers.borrow().is_empty());

    actor.source_events_from_chunk(b"\x1b]9;4;");
    assert!(
        actor.last_progress.is_empty(),
        "split mark is not complete yet"
    );
    actor.source_events_from_chunk(b"3;\x07");
    assert_eq!(actor.last_progress, "4;3;");
    actor.source_events_from_chunk(b"\x1b]9;4;0;\x07");
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

/// phux-fseo: a consumer that negotiated `OutputMode::StateSync`
/// (`wants_state_sync == true`) is served by the tick even with the
/// global test gate OFF — the per-consumer opt-in is the production
/// path. Proves the negotiation actually reaches `tick_emit` without
/// relying on `enable_tick_emit_for_test`.
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
    let Outbound::Frame(FrameKind::TerminalOutput { seq, .. }) = frame else {
        panic!("expected a TerminalOutput frame for the state-sync consumer");
    };
    assert_eq!(seq, 1, "first tick emission stamps seq=1");
}

/// phux-fseo: with the global gate OFF and two consumers sharing one
/// pane — one `StateSync`, one `Raw` — the tick serves ONLY the
/// state-sync consumer. The raw consumer is served by the runtime's
/// broadcast pump; emitting to it here too would double-paint it.
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
            Ok(Outbound::Frame(FrameKind::TerminalOutput { .. })),
        ),
        "state-sync consumer must receive the synthesized delta",
    );
    assert!(
        raw_rx.try_recv().is_err(),
        "raw consumer must stay on the broadcast pump — tick must not double-paint it",
    );
}

// ---- phux-v45.8 / ADR-0042: loss-tolerant (advance-on-ack) state sync ----

/// A loss-tolerant consumer's reference advances on `FRAME_ACK`, not on
/// emit: after emitting a delta the acked reference is unchanged and the
/// frame is retained in `pending_refs`; the matching ack advances the
/// reference and prunes the pending snapshot.
#[test]
fn loss_tolerant_reference_advances_only_on_ack() {
    let bundle = TerminalActor::new(20, 5).expect("new");
    let mut actor = bundle.actor;
    let client = ClientId(1);
    let (tx, mut rx) = dummy_outbound();
    actor
        .register_consumer(client, tx, 11, true)
        .expect("register");
    actor.enable_loss_tolerance_for_test(client);

    actor.vt_write_for_test(b"hello");
    actor.tick_emit();

    let frames = drain_outputs(&mut rx);
    assert_eq!(frames.len(), 1, "one delta shipped for the new content");
    let seq = frames[0].0;
    {
        let state = actor.consumer_state(client).expect("state");
        assert!(
            state.pending_refs.contains_key(&seq),
            "the emitted frame's grid snapshot is retained until acked",
        );
        assert_eq!(
            state.last_acked_seq, 0,
            "no ack yet: the acked reference has not advanced",
        );
    }

    // Idle tick with no new content and no elapsed time: the loss-tolerant
    // gate must NOT re-ship the same cumulative delta (no flood).
    actor.tick_emit();
    assert!(
        drain_outputs(&mut rx).is_empty(),
        "an un-acked delta must not re-ship every idle tick",
    );

    // Ack it: the reference advances and the pending snapshot is pruned.
    actor.on_frame_ack(client, seq);
    {
        let state = actor.consumer_state(client).expect("state");
        assert!(
            state.pending_refs.is_empty(),
            "ack prunes the covered pending snapshot",
        );
        assert_eq!(state.last_acked_seq, seq);
    }
    // Post-ack idle tick: nothing to send (acked == live).
    actor.tick_emit();
    assert!(
        drain_outputs(&mut rx).is_empty(),
        "post-ack idle tick is silent"
    );
}

/// The core v45.8 property: a dropped/un-acked frame self-heals. Emit
/// delta 1 (simulated dropped — never applied to the mirror, never acked),
/// then emit delta 2 for later content. Because delta 2 is re-diffed
/// against the last-ACKED reference (still empty), it re-includes delta 1's
/// rows, so applying ONLY delta 2 to the mirror converges it to canonical.
#[test]
fn loss_tolerant_dropped_frame_rediffs_against_acked_and_converges() {
    let bundle = TerminalActor::new(20, 5).expect("new");
    let mut actor = bundle.actor;
    let client = ClientId(1);
    let (tx, mut rx) = dummy_outbound();
    actor
        .register_consumer(client, tx, 11, true)
        .expect("register");
    actor.enable_loss_tolerance_for_test(client);

    // Mirror starts at the same point the acked reference was primed to
    // (an empty grid) — what the consumer's TERMINAL_SNAPSHOT establishes.
    let mut mirror = GhosttyTerminal::new(TerminalOptions {
        cols: 20,
        rows: 5,
        max_scrollback: 100,
    })
    .expect("mirror");

    // Content A → delta 1. SIMULATE A DROP: do not apply it, do not ack.
    actor.vt_write_for_test(b"AAAA");
    actor.tick_emit();
    let dropped = drain_outputs(&mut rx);
    assert_eq!(dropped.len(), 1, "delta 1 emitted");

    // Content B (a second row) → delta 2. Re-diffed against acked (empty).
    actor.vt_write_for_test(b"\r\nBBBB");
    actor.tick_emit();
    let delivered = drain_outputs(&mut rx);
    assert_eq!(delivered.len(), 1, "delta 2 emitted");

    // Apply ONLY delta 2 (delta 1 was lost).
    mirror.vt_write(&delivered[0].1);

    let canonical_grid = render_viewport(&actor.terminal.borrow());
    let mirror_grid = render_viewport(&mirror);
    assert_eq!(
        canonical_grid, mirror_grid,
        "applying only the post-drop cumulative delta must converge the \
             mirror to canonical (self-heal);\ncanonical = {canonical_grid:?}\n\
             mirror    = {mirror_grid:?}",
    );
    assert_eq!(mirror_grid[0], "AAAA");
    assert_eq!(mirror_grid[1], "BBBB");
}

/// After a `FRAME_ACK`, subsequent deltas are diffed against the newly
/// advanced acked reference (incremental, not cumulative-from-empty), and
/// the mirror — brought current by the acked frames then the new one —
/// still converges. Exercises the steady-state ack loop.
#[test]
fn loss_tolerant_incremental_after_ack_converges() {
    let bundle = TerminalActor::new(20, 5).expect("new");
    let mut actor = bundle.actor;
    let client = ClientId(1);
    let (tx, mut rx) = dummy_outbound();
    actor
        .register_consumer(client, tx, 11, true)
        .expect("register");
    actor.enable_loss_tolerance_for_test(client);
    let mut mirror = GhosttyTerminal::new(TerminalOptions {
        cols: 20,
        rows: 5,
        max_scrollback: 100,
    })
    .expect("mirror");

    // Round 1: content, deliver + ack.
    actor.vt_write_for_test(b"row-one");
    actor.tick_emit();
    let f1 = drain_outputs(&mut rx);
    assert_eq!(f1.len(), 1);
    mirror.vt_write(&f1[0].1);
    actor.on_frame_ack(client, f1[0].0);

    // Round 2: more content, delivered + acked; diffed against the acked
    // reference from round 1.
    actor.vt_write_for_test(b"\r\nrow-two");
    actor.tick_emit();
    let f2 = drain_outputs(&mut rx);
    assert_eq!(f2.len(), 1);
    mirror.vt_write(&f2[0].1);
    actor.on_frame_ack(client, f2[0].0);

    let canonical_grid = render_viewport(&actor.terminal.borrow());
    let mirror_grid = render_viewport(&mirror);
    assert_eq!(
        canonical_grid, mirror_grid,
        "steady-state ack loop converges"
    );
    assert_eq!(mirror_grid[0], "row-one");
    assert_eq!(mirror_grid[1], "row-two");
}

/// A retransmit heals a lost final frame on an otherwise idle terminal.
/// Emit content (dropped, un-acked), backdate the emit clock past the
/// retransmit timeout, then run an idle tick: the consumer retransmits a
/// cumulative delta (re-diffed against the acked reference) that converges
/// the mirror even though no new content arrived.
#[test]
fn loss_tolerant_retransmits_lost_frame_when_idle() {
    let bundle = TerminalActor::new(20, 5).expect("new");
    let mut actor = bundle.actor;
    let client = ClientId(1);
    let (tx, mut rx) = dummy_outbound();
    actor
        .register_consumer(client, tx, 11, true)
        .expect("register");
    actor.enable_loss_tolerance_for_test(client);
    let mut mirror = GhosttyTerminal::new(TerminalOptions {
        cols: 20,
        rows: 5,
        max_scrollback: 100,
    })
    .expect("mirror");

    // Content → delta. Simulate a drop: discard it, never ack.
    actor.vt_write_for_test(b"lonely");
    actor.tick_emit();
    let dropped = drain_outputs(&mut rx);
    assert_eq!(dropped.len(), 1, "initial delta emitted");

    // No new content. Backdate the emit instant past the retransmit RTO
    // so the next idle tick re-ships (suspected loss).
    actor.backdate_emit_instants_for_test(client, std::time::Duration::from_secs(2));
    actor.tick_emit();
    let retransmit = drain_outputs(&mut rx);
    assert_eq!(
        retransmit.len(),
        1,
        "an idle terminal with an un-acked frame must retransmit after the RTO",
    );

    // The retransmit (re-diffed against the acked reference) converges the
    // mirror that never saw the original frame.
    mirror.vt_write(&retransmit[0].1);
    let canonical_grid = render_viewport(&actor.terminal.borrow());
    let mirror_grid = render_viewport(&mirror);
    assert_eq!(
        canonical_grid, mirror_grid,
        "retransmit heals the lost frame on an idle terminal",
    );
    assert_eq!(mirror_grid[0], "lonely");
}

/// The emit-once (non-loss-tolerant) default is untouched: a state-sync
/// consumer that did NOT opt into loss-tolerance keeps no `pending_refs`
/// and advances its reference on emit (`on_frame_ack` does not evict a
/// pending snapshot because there is none).
#[test]
fn emit_once_default_keeps_no_pending_refs() {
    let bundle = TerminalActor::new(20, 5).expect("new");
    let mut actor = bundle.actor;
    let client = ClientId(1);
    let (tx, mut rx) = dummy_outbound();
    actor
        .register_consumer(client, tx, 11, true)
        .expect("register");
    // NB: loss-tolerance NOT enabled.
    actor.vt_write_for_test(b"content");
    actor.tick_emit();
    let frames = drain_outputs(&mut rx);
    assert_eq!(frames.len(), 1);
    let state = actor.consumer_state(client).expect("state");
    assert!(!state.loss_tolerant, "default consumer is emit-once");
    assert!(
        state.pending_refs.is_empty(),
        "emit-once path allocates no pending reference snapshots",
    );
}

/// phux-0q8 / phux-q0e.3 / phux-3uv / phux-ia4: with the gate ON for
/// a SINGLE consumer, `tick_emit` diffs the
/// dirty seeded grid against the consumer's reference and ships exactly
/// one `TerminalOutput` carrying the content, stamping `seq = 1`.
///
/// Emit-once (phux-ia4): the consumer's reference advances on emit, so
/// a second tick with no further writes is SILENT — the change is
/// delivered exactly once, not re-emitted every tick. A subsequent
/// write produces a fresh single emission (`seq = 2`).
#[test]
fn tick_emit_emits_once_when_gate_is_on() {
    let bundle = TerminalActor::new(20, 5).expect("new");
    let mut actor = bundle.actor;
    actor.enable_tick_emit_for_test();
    let client = ClientId(1);
    let (tx, mut rx) = dummy_outbound();
    // Register against the (blank) terminal: the reference is primed
    // so deltas are measured "from now." Writing AFTER register is what
    // makes the next tick produce a diff.
    actor
        .register_consumer(client, tx, 11, false)
        .expect("register");
    actor.vt_write_for_test(b"q0e-marker");

    actor.tick_emit();
    let frame = rx
        .try_recv()
        .expect("gate on: first tick must emit the changed grid");
    let Outbound::Frame(FrameKind::TerminalOutput {
        terminal_id,
        seq,
        bytes,
        ..
    }) = frame
    else {
        panic!("expected a TerminalOutput frame from tick_emit");
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

    // Emit-once: with no further writes, the reference now matches the
    // live grid, so the next tick is silent — NO re-emission of the
    // already-delivered change.
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
    let Outbound::Frame(FrameKind::TerminalOutput { seq, bytes, .. }) = frame else {
        panic!("expected a TerminalOutput frame on the new write");
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

/// phux-ia4 regression: TWO consumers sharing one pane. A single tick
/// of new output MUST deliver the incremental to BOTH consumers — not
/// just the first one walked.
///
/// This is the exact starvation the ticket is about. Under the old
/// per-consumer-`RenderState` dirty model, the first consumer's
/// `RenderState::update` consumed the shared `Terminal` dirty bits, so
/// the second consumer that tick observed `Dirty::Clean` and emitted
/// nothing. The per-consumer reference grid removes that coupling: each
/// consumer diffs against its own last-synced rows, so both receive the
/// change in the same tick regardless of walk order.
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

    // BOTH consumers must receive a TerminalOutput carrying the marker.
    let recv_marker = |rx: &mut mpsc::Receiver<Outbound>, who: &str| {
        let frame = rx
            .try_recv()
            .unwrap_or_else(|_| panic!("consumer {who} starved: no frame this tick"));
        let Outbound::Frame(FrameKind::TerminalOutput { seq, bytes, .. }) = frame else {
            panic!("consumer {who}: expected a TerminalOutput frame");
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

    // Per-consumer independence: a consumer that detaches does not
    // perturb the other. A fresh write reaches the survivor exactly
    // once.
    actor.unregister_consumer(client_a);
    actor.vt_write_for_test(b" again");
    actor.tick_emit();
    let frame = rx_b.try_recv().expect("consumer B: must get the new write");
    let Outbound::Frame(FrameKind::TerminalOutput { seq, bytes, .. }) = frame else {
        panic!("consumer B: expected a TerminalOutput frame");
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

    // First tick: the consumer needs its initial pass, so it is walked
    // (returns empty here — primed against a blank terminal) and the
    // dirty flag set at construction is consumed.
    actor.tick_emit();
    // Drain whatever the first tick produced (expected: nothing, since
    // the reference was primed to the same blank state).
    while rx.try_recv().is_ok() {}

    // Now write, tick, drain: the consumer receives the write.
    actor.vt_write_for_test(b"hello");
    actor.tick_emit();
    let got = rx.try_recv().expect("write must reach the consumer");
    let Outbound::Frame(FrameKind::TerminalOutput { bytes, .. }) = got else {
        panic!("expected TerminalOutput");
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

/// phux-4l0: a consumer registered AFTER the last write sits on a
/// terminal that is `Clean` since the previous tick, yet has never had
/// a synthesis pass. The `needs_initial_emit` carve-out must keep the
/// short-circuit from starving it: the next tick must still walk it
/// (here the write predates the attach, so it is already primed and
/// the body is empty — the point is the entry is serviced, not
/// skipped, preserving the phux-ia4 multi-consumer guarantee).
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

    // Consumer B attaches with NO intervening write. The terminal is
    // Clean, but B has needs_initial_emit set, so the short-circuit
    // must NOT fire — B must be walked. (Primed to current state, so
    // the body is empty, but the entry is serviced and the flag
    // cleared.)
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
        let Outbound::Frame(FrameKind::TerminalOutput { bytes, .. }) = frame else {
            panic!("{who}: expected TerminalOutput");
        };
        assert!(
            contains_subslice(&bytes, b"again"),
            "{who} must carry the new write",
        );
    }
}

/// phux-ddg: a consumer whose outbound receiver has been dropped (a
/// detach whose `ConsumerDetachRequest` never reached the actor — full
/// mailbox) must be reaped by `tick_emit` rather than re-rendered every
/// tick forever. The tick is self-healing: a `Closed` mailbox removes
/// the entry.
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

    // Simulate the dropped-detach leak: the client's receiver goes
    // away (disconnect) but the detach request was lost, so the
    // per-consumer entry is still present.
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

/// phux-ddg: a consumer with a closed mailbox is reaped even when the
/// diff body is empty (idle dead consumer). Without this, an idle but
/// dead consumer would never hit the `try_send` Closed arm and would
/// linger until pane teardown.
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

    // Receiver drops (disconnect). A write keeps the per-consumer loop
    // running this tick; whether the diff body is empty or not, the
    // `is_closed()` probe on the empty-body path and the `Closed` arm
    // on the send path both reap the entry.
    drop(rx);
    actor.vt_write_for_test(b"x");
    actor.tick_emit();
    assert_eq!(
        actor.consumer_count(),
        0,
        "dead consumer reaped even though it never acked",
    );
}

/// Drain every `TerminalOutput` currently queued on `rx`, returning the
/// concatenated payload bytes and the ordered list of `seq`s.
fn drain_terminal_output(rx: &mut mpsc::Receiver<Outbound>) -> (Vec<u8>, Vec<u64>) {
    let mut bytes = Vec::new();
    let mut seqs = Vec::new();
    while let Ok(frame) = rx.try_recv() {
        if let Outbound::Frame(FrameKind::TerminalOutput {
            seq, bytes: body, ..
        }) = frame
        {
            seqs.push(seq);
            bytes.extend_from_slice(&body);
        }
    }
    (bytes, seqs)
}

/// wave-hunt/server-lifecycle: a consumer whose outbound mailbox fills up
/// under sustained output MUST NOT lose grid content. Once the client
/// drains, every written marker must still be reconstructable from the
/// delivered stream.
///
/// Pre-fix this failed: `tick_emit` synthesized the delta (which commits
/// the per-consumer reference to the just-rendered grid, emit-once) and
/// THEN dropped the frame on a `Full` mailbox. The reference had already
/// advanced past the dropped delta, so the next tick diffed against a
/// reference that already included the dropped content and never
/// re-emitted it — silent permanent content loss / mirror divergence.
/// The fix reserves the outbound permit BEFORE synthesizing, so a full
/// mailbox skips the consumer without advancing its reference.
#[test]
fn backpressured_consumer_loses_no_content_after_draining() {
    // More rounds than the mailbox holds so the tick's send hits `Full`.
    const ROUNDS: usize = 12;
    let bundle = TerminalActor::new(80, 24).expect("new");
    let mut actor = bundle.actor;
    actor.enable_tick_emit_for_test();

    let client = ClientId(1);
    // Tiny mailbox so a few ticks saturate it — same shape as the
    // production `DEFAULT_CLIENT_MAILBOX` pressure, smaller and faster.
    let (tx, mut rx) = mpsc::channel::<Outbound>(2);
    actor
        .register_consumer(client, tx, 11, false)
        .expect("register");

    // Write a distinct marker on its own line and tick to emit it,
    // WITHOUT draining the receiver. Every marker must survive.
    let markers: Vec<String> = (0..ROUNDS).map(|i| format!("MARK{i:03}=")).collect();
    for marker in &markers {
        actor.vt_write_for_test(format!("{marker}\r\n").as_bytes());
        actor.tick_emit();
    }

    // Drain, then keep ticking + draining so any content held back under
    // backpressure flows once there is room.
    let mut delivered = Vec::new();
    let (chunk, _seqs) = drain_terminal_output(&mut rx);
    delivered.extend_from_slice(&chunk);
    for _ in 0..ROUNDS * 2 {
        actor.tick_emit();
        let (chunk, _seqs) = drain_terminal_output(&mut rx);
        delivered.extend_from_slice(&chunk);
    }

    for marker in &markers {
        assert!(
            contains_subslice(&delivered, marker.as_bytes()),
            "marker {marker:?} never reached the consumer: content was lost \
                 under mailbox backpressure (reference advanced past a dropped \
                 frame). delivered={:?}",
            String::from_utf8_lossy(&delivered),
        );
    }
}

/// wave-hunt/server-lifecycle: the per-consumer monotonic `seq` must have
/// no gaps in the delivered stream. A frame that is NOT shipped must NOT
/// consume a `seq`. Pre-fix, `tick_emit` incremented `next_seq` and then
/// dropped the frame on `Full`, burning a seq for a frame the consumer
/// never saw — the client would observe a hole in the otherwise
/// contiguous reliable-transport stream (SPEC §12.2) and could not
/// distinguish loss from reorder.
#[test]
fn backpressured_consumer_sees_contiguous_seq_stream() {
    const ROUNDS: usize = 10;
    let bundle = TerminalActor::new(80, 24).expect("new");
    let mut actor = bundle.actor;
    actor.enable_tick_emit_for_test();

    let client = ClientId(1);
    let (tx, mut rx) = mpsc::channel::<Outbound>(2);
    actor
        .register_consumer(client, tx, 11, false)
        .expect("register");

    for i in 0..ROUNDS {
        actor.vt_write_for_test(format!("seqmark{i:03}\r\n").as_bytes());
        actor.tick_emit();
    }

    let mut all_seqs = Vec::new();
    let (_b, seqs) = drain_terminal_output(&mut rx);
    all_seqs.extend(seqs);
    for _ in 0..ROUNDS * 2 {
        actor.tick_emit();
        let (_b, seqs) = drain_terminal_output(&mut rx);
        all_seqs.extend(seqs);
    }

    assert!(
        !all_seqs.is_empty(),
        "expected at least one delivered frame"
    );
    for (idx, seq) in all_seqs.iter().enumerate() {
        let expected = u64::try_from(idx).expect("fits") + 1;
        assert_eq!(
            *seq, expected,
            "delivered seq stream must be contiguous from 1 with no gaps; \
                 a dropped-but-seq-burned frame leaves a hole. got={all_seqs:?}",
        );
    }
}

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

/// The adaptive interval is `RTT/2` clamped to [20ms, 200ms]: a near-zero
/// RTT clamps to the 20ms floor (snappier than the 30ms default), and a
/// huge RTT clamps to the 200ms ceiling.
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

/// End-to-end through the actor: a `FRAME_ACK` measured against a large
/// simulated transit time backs the shared cadence off toward the 200ms
/// ceiling; a near-zero transit time pins it to the 20ms floor. Uses
/// paused tokio time so the emit->ack gap is exact and deterministic.
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

    // Fast peer joins; the shared cadence is the MINIMUM desired, so the
    // near-zero-RTT peer pulls it back down to the floor regardless of
    // the slow peer.
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

    // The slow peer leaving must not regress the floor (fast peer still
    // present), and dropping the fast peer reverts to the cold-start
    // default (no samples left to consult).
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

/// An ack that matches no recorded emit instant (e.g. the consumer never
/// had a frame shipped) yields no RTT sample and leaves the cadence at
/// the default — the round-trip machinery is inert without an emission.
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

/// phux-ahk: a state-sync consumer that never sends `FRAME_ACK` must not
/// grow `emit_instants` without bound. Ack-pruning never runs for it, so
/// the per-tick insert is bounded only by the defensive
/// [`MAX_EMIT_INSTANTS`] cap (oldest-evicted). Drive many more emitting
/// ticks than the cap, never acking, and assert the map stays capped and
/// retains the newest (highest-`seq`) samples rather than the stale ones.
#[test]
fn emit_instants_is_capped_for_never_acking_consumer() {
    let bundle = TerminalActor::new(20, 5).expect("new");
    let mut actor = bundle.actor;
    let client = ClientId(1);
    let (tx, mut rx) = dummy_outbound();
    actor
        .register_consumer(client, tx, 11, true)
        .expect("register");

    // Far more emitting ticks than the cap. Distinct content each tick
    // keeps the grid dirty so the diff is non-empty and the tick actually
    // emits (and inserts). Drain the mailbox each tick so the send keeps
    // succeeding — a full mailbox would backpressure and skip the insert,
    // hiding the growth this test pins.
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
    // Eviction drops the oldest seqs, so emission must actually have run
    // past the cap (otherwise this test proves nothing) and the lowest
    // retained key is well above the first seq.
    let lowest = *state.emit_instants.keys().next().expect("non-empty map");
    assert!(
        lowest > 1,
        "oldest emit instants should have been evicted; lowest retained seq = {lowest}",
    );
}

// --- agent-detector dirty-flag accounting (ADR-0046) -------------------

/// `agent_dirty_since_detect` is the ONLY record that the grid changed
/// since the detector last looked. `detect_tick` must consume it only on a
/// tick that actually scanned.
///
/// While no agent is identified, `wants_screen` is unconditionally false —
/// there is nothing to derive against. So a `detect_tick` in that window
/// performs no scan, and eating the flag there discards every grid mutation
/// the agent made before we noticed it existed: the permission dialog it
/// painted, and then went silent behind, is exactly such a mutation. The
/// detector then derives `idle` from a screen it never read and latches
/// there, because `wants_screen` sees `current == Some(Idle)` and never
/// asks for the scan that would correct it.
#[test]
fn detect_tick_keeps_the_dirty_flag_when_it_performs_no_scan() {
    let bundle = TerminalActor::new(20, 5).expect("new");
    let mut actor = bundle.actor;
    actor.agent_detect = Some(AgentDetector::new(
        crate::agent_detect::rules::global(),
        std::time::Instant::now(),
    ));

    // The agent paints. No agent is identified yet (no PTY, so identity
    // never resolves), so this tick cannot scan.
    actor.vt_write_for_test(b"a permission dialog");
    assert!(actor.agent_dirty_since_detect, "the grid changed");

    assert!(actor.detect_tick().is_some(), "the detector ran");
    assert!(
        actor.agent_dirty_since_detect,
        "a tick that performed no scan must not consume the evidence that a scan is owed",
    );
}

/// The converse, so the flag is not simply never cleared: once an agent IS
/// identified, the scan runs and consumes the flag — which is what keeps
/// the steady state cheap.
#[test]
fn detect_tick_consumes_the_dirty_flag_when_it_scans() {
    let bundle = TerminalActor::new(20, 5).expect("new");
    let mut actor = bundle.actor;
    let now = std::time::Instant::now();
    let mut detector = AgentDetector::new(crate::agent_detect::rules::global(), now);
    detector.force_identity("claude", now);
    actor.agent_detect = Some(detector);

    actor.vt_write_for_test(b"some output");
    assert!(actor.agent_dirty_since_detect);

    assert!(actor.detect_tick().is_some(), "the detector ran");
    assert!(
        !actor.agent_dirty_since_detect,
        "a scan consumes the flag; otherwise every tick re-projects the grid forever",
    );
}

#[test]
fn input_snapshot_publishes_after_seed_output_and_resize() {
    let seeded = TerminalActor::new_with_seed(80, 24, b"\x1b[?2004h").expect("seeded");
    assert!(seeded.handle.input_snapshot.borrow().bracketed_paste);

    let mut actor = seeded.actor;
    let mut snapshots = seeded.handle.input_snapshot;
    snapshots.borrow_and_update();
    actor.vt_write_for_test(b"\x1b[?1004h");
    assert!(snapshots.has_changed().expect("publisher alive"));
    assert!(snapshots.borrow_and_update().focus_reporting);

    actor.handle_resize(101, 39, Some((11, 19)));
    assert!(snapshots.has_changed().expect("publisher alive"));
    let resized = *snapshots.borrow_and_update();
    assert_eq!((resized.cols, resized.rows), (101, 39));
    assert_eq!(resized.cell_px, (11, 19));
}
