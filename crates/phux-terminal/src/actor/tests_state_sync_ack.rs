use super::*;

/// A seeded 20x5 actor with `client` registered (tick-managed when
/// `state_sync`), and the mailbox to hold so the tick does not reap it.
fn seeded_with(client: ClientId, state_sync: bool) -> (TerminalActor, mpsc::Receiver<Outbound>) {
    let mut actor = TerminalActor::new_with_seed(20, 5, b"hello")
        .expect("new_with_seed")
        .actor;
    let (tx, rx) = dummy_outbound();
    actor
        .register_consumer(client, tx, 11, state_sync)
        .expect("register");
    (actor, rx)
}

fn acked(actor: &TerminalActor, client: ClientId) -> u64 {
    actor
        .consumer_state(client)
        .expect("state")
        .sync
        .last_acked_seq
}

/// phux-q0e.4: in-order acks walk `last_acked_seq` forward; older or
/// duplicate acks are dropped (cumulative).
#[test]
fn on_frame_ack_is_cumulative_and_monotonic() {
    let client = ClientId(1);
    // State-sync: its acks belong to the per-consumer tick seq space
    // (phux-38k6).
    let (mut actor, _rx) = seeded_with(client, true);
    for seq in 1..=3 {
        actor.vt_write_for_test(b"x");
        actor.tick_emit();
        actor.on_frame_ack(client, seq);
        assert_eq!(acked(&actor, client), seq, "in-order ack advances");
    }
    for _ in 0..3 {
        actor.vt_write_for_test(b"x");
        actor.tick_emit();
    }
    for (ack, want) in [(5, 5), (3, 5), (5, 5), (6, 6)] {
        actor.on_frame_ack(client, ack);
        assert_eq!(acked(&actor, client), want, "ack {ack}");
    }
}

/// Acks that must change nothing: a raw consumer's (a pump-local seq), a
/// stranger's, and one after detach (no resurrection).
#[test]
fn foreign_and_late_acks_are_dropped() {
    let raw = ClientId(1);
    let (mut actor, _rx) = seeded_with(raw, false);
    assert!(
        !actor.on_frame_ack(raw, 7),
        "a raw ack yields no RTT sample"
    );
    assert_eq!(acked(&actor, raw), 0, "a raw-pump ack does not advance");

    let stranger = ClientId(999);
    actor.on_frame_ack(stranger, 42);
    assert!(
        actor.consumer_state(stranger).is_none(),
        "no entry from a stray ack"
    );

    let client = ClientId(7);
    let (mut actor, _rx) = seeded_with(client, true);
    for _ in 0..2 {
        actor.vt_write_for_test(b"x");
        actor.tick_emit();
    }
    actor.on_frame_ack(client, 2);
    assert_eq!(acked(&actor, client), 2);
    actor.unregister_consumer(client);
    actor.on_frame_ack(client, 9);
    assert!(actor.consumer_state(client).is_none());
    assert_eq!(actor.consumer_count(), 0);
}

#[test]
fn future_ack_cannot_erase_pending_data_or_poison_later_acknowledgements() {
    let client = ClientId(1);
    let (mut actor, _rx) = seeded_with(client, true);
    actor.enable_loss_tolerance(client);
    actor.vt_write_for_test(b"one");
    actor.tick_emit();
    let next = actor.consumer_state(client).unwrap().sync.next_seq;
    assert_eq!(next, 2);
    let state = actor.consumer_state(client).unwrap();
    let pending = state.sync.emit_instants.len();
    let stream = state.sync.stream_id;
    let bootstrap = state.sync.bootstrap_id;
    let acked_rows = state.sync.acked_reference.rows_body.clone();
    assert_eq!(state.sync.pending_refs.len(), 1);
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
        assert_eq!(state.sync.last_acked_seq, 0);
        assert_eq!(state.sync.emit_instants.len(), pending);
        assert_eq!(state.sync.pending_refs.len(), 1);
        assert_eq!(state.sync.acked_reference.rows_body, acked_rows);
        assert!(state.sync.rtt.smoothed().is_none());
    }
    assert!(actor.on_generation_frame_ack(client, stream, bootstrap, 1));
    let state = actor.consumer_state(client).unwrap();
    assert_eq!(state.sync.last_acked_seq, 1);
    assert!(state.sync.emit_instants.is_empty());
    assert!(state.sync.pending_refs.is_empty());
    assert_ne!(state.sync.acked_reference.rows_body, acked_rows);
}

#[test]
fn ack_between_pty_mutation_and_tick_preserves_pending_row_changes() {
    let (mut actor, _rx) = seeded_with(ClientId(1), true);
    actor.vt_write_for_test(b"\x1b[1;1Hfirst");
    actor.tick_emit();
    actor.vt_write_for_test(b"\x1b[2;1HSECOND\x1b[1;6H");
    assert!(actor.on_frame_ack(ClientId(1), 1));
    actor.tick_emit();
    let state = actor.consumer_state(ClientId(1)).unwrap();
    assert!(
        state.sync.reference.rows_body[1]
            .windows(6)
            .any(|bytes| bytes == b"SECOND")
    );
}
