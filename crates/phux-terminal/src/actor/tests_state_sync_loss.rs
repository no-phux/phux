use super::*;

// ---- phux-v45.8 / ADR-0042: loss-tolerant (advance-on-ack) state sync ----

/// A loss-tolerant reference advances on ack, not emit.
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
            state.sync.pending_refs.contains_key(&seq),
            "the emitted frame's grid snapshot is retained until acked",
        );
        assert_eq!(
            state.sync.last_acked_seq, 0,
            "no ack yet: the acked reference has not advanced",
        );
    }

    assert!(
        actor.state_tick_armed(),
        "unacknowledged frames keep retransmission armed"
    );

    // Idle tick with no new content and no elapsed time: the loss-tolerant
    // gate must NOT re-ship the same cumulative delta (no flood).
    actor.tick_emit();
    assert!(
        drain_outputs(&mut rx).is_empty(),
        "an un-acked delta must not re-ship every idle tick",
    );

    // Ack it: the reference advances and the pending snapshot is pruned.
    actor.on_frame_ack(client, seq);
    assert!(
        !actor.state_tick_armed(),
        "acknowledged clean consumer can sleep"
    );
    {
        let state = actor.consumer_state(client).expect("state");
        assert!(
            state.sync.pending_refs.is_empty(),
            "ack prunes the covered pending snapshot",
        );
        assert_eq!(state.sync.last_acked_seq, seq);
    }
    // Post-ack idle tick: nothing to send (acked == live).
    actor.tick_emit();
    assert!(
        drain_outputs(&mut rx).is_empty(),
        "post-ack idle tick is silent"
    );
}

/// A dropped, un-acked frame self-heals: the next delta is diffed against
/// the acked reference and alone converges the mirror.
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
    let mut mirror = {
        let mut terminal = GhosttyTerminal::new(20, 5).expect("mirror");
        terminal
            .set_scrollback_max_lines(Some(100))
            .expect("mirror");
        terminal
    };

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

    let canonical_grid = render_viewport(
        actor
            .terminal
            .borrow()
            .try_terminal()
            .expect("no capture in flight"),
    );
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

/// After an ack, deltas diff against the advanced reference and the mirror
/// still converges.
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
    let mut mirror = {
        let mut terminal = GhosttyTerminal::new(20, 5).expect("mirror");
        terminal
            .set_scrollback_max_lines(Some(100))
            .expect("mirror");
        terminal
    };

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

    let canonical_grid = render_viewport(
        actor
            .terminal
            .borrow()
            .try_terminal()
            .expect("no capture in flight"),
    );
    let mirror_grid = render_viewport(&mirror);
    assert_eq!(
        canonical_grid, mirror_grid,
        "steady-state ack loop converges"
    );
    assert_eq!(mirror_grid[0], "row-one");
    assert_eq!(mirror_grid[1], "row-two");
}

/// A retransmit heals a lost final frame on an idle terminal.
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
    let mut mirror = {
        let mut terminal = GhosttyTerminal::new(20, 5).expect("mirror");
        terminal
            .set_scrollback_max_lines(Some(100))
            .expect("mirror");
        terminal
    };

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
    let canonical_grid = render_viewport(
        actor
            .terminal
            .borrow()
            .try_terminal()
            .expect("no capture in flight"),
    );
    let mirror_grid = render_viewport(&mirror);
    assert_eq!(
        canonical_grid, mirror_grid,
        "retransmit heals the lost frame on an idle terminal",
    );
    assert_eq!(mirror_grid[0], "lonely");
}

/// Without loss tolerance there are no pending refs; emit-once as before.
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
    assert!(!state.sync.loss_tolerant, "default consumer is emit-once");
    assert!(
        state.sync.pending_refs.is_empty(),
        "emit-once path allocates no pending reference snapshots",
    );
}
