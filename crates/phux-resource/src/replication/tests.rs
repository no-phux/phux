use std::time::Duration;

use phux_protocol::ids::{BootstrapId, StreamId};

use super::{
    AckRejection, ConsumerSync, DEFAULT_TICK_INTERVAL, MAX_EMIT_INSTANTS, MAX_TICK_INTERVAL,
    MIN_TICK_INTERVAL, RETRANSMIT_DEFAULT, RETRANSMIT_MAX, RETRANSMIT_MIN, RttEstimator,
    adaptive_tick_interval, retransmit_timeout,
};

/// The reference is a counter: the version of state the consumer holds.
fn consumer(next_seq: u64) -> ConsumerSync<u32> {
    ConsumerSync::new(stream(1), bootstrap(1), next_seq, 0, 0)
}

fn stream(id: u64) -> StreamId {
    StreamId::new(id).expect("stream id")
}

fn bootstrap(id: u64) -> BootstrapId {
    BootstrapId::new(id).expect("bootstrap id")
}

/// Ship one frame whose loss-tolerant snapshot is `version`.
fn emit(sync: &mut ConsumerSync<u32>, version: u32) -> u64 {
    let seq = sync.allocate_seq();
    sync.record_emit(seq, || version);
    seq
}

#[test]
fn new_consumer_waits_for_its_first_walk() {
    let sync = consumer(7);
    assert_eq!(sync.next_seq, 7);
    assert_eq!(sync.last_acked_seq, 0);
    assert!(sync.needs_initial_emit && !sync.behind && !sync.loss_tolerant);
    assert!(sync.rtt.smoothed().is_none());
    assert!(sync.must_walk_when_clean());
}

#[test]
fn generation_must_match_stream_and_bootstrap() {
    let sync = consumer(1);
    assert!(sync.matches_generation(stream(1), bootstrap(1)));
    assert!(!sync.matches_generation(stream(2), bootstrap(1)));
    assert!(!sync.matches_generation(stream(1), bootstrap(2)));
}

#[test]
fn allocate_seq_is_sequential_and_wraps() {
    let mut sync = consumer(1);
    assert_eq!(sync.allocate_seq(), 1);
    assert_eq!(sync.allocate_seq(), 2);
    assert_eq!(sync.next_seq, 3);
    let mut last = consumer(u64::MAX);
    assert_eq!(last.allocate_seq(), u64::MAX);
    assert_eq!(last.next_seq, 0);
}

#[test]
fn acks_advance_monotonically_and_rejections_change_nothing() {
    let mut sync = consumer(1);
    for version in 1..=3 {
        emit(&mut sync, version);
    }
    assert!(sync.ack(2).is_ok());
    assert_eq!(sync.last_acked_seq, 2);

    let in_flight = sync.emit_instants.len();
    assert_eq!(
        sync.ack(2),
        Err(AckRejection::Stale { last_acked_seq: 2 }),
        "duplicate"
    );
    assert_eq!(
        sync.ack(1),
        Err(AckRejection::Stale { last_acked_seq: 2 }),
        "older"
    );
    assert_eq!(
        sync.ack(4),
        Err(AckRejection::BeyondEmitted { next_seq: 4 }),
        "never emitted"
    );
    assert_eq!(sync.last_acked_seq, 2);
    assert_eq!(sync.emit_instants.len(), in_flight);

    assert!(sync.ack(3).is_ok());
    assert_eq!(sync.last_acked_seq, 3);
}

#[test]
fn zero_is_never_acked() {
    let sync = consumer(1);
    assert_eq!(
        sync.admit_ack(0),
        Err(AckRejection::Stale { last_acked_seq: 0 })
    );
}

#[tokio::test(start_paused = true)]
async fn ack_samples_rtt_from_newest_covered_emit() {
    let mut sync = consumer(1);
    emit(&mut sync, 1);
    tokio::time::advance(Duration::from_millis(30)).await;
    emit(&mut sync, 2);
    tokio::time::advance(Duration::from_millis(10)).await;
    emit(&mut sync, 3);

    assert_eq!(sync.ack(2), Ok(Some(Duration::from_millis(10))));
    assert_eq!(sync.rtt.smoothed(), Some(Duration::from_millis(10)));
    assert_eq!(
        sync.emit_instants.keys().copied().collect::<Vec<_>>(),
        vec![3],
        "covered instants are pruned, in-flight ones kept",
    );
}

#[test]
fn ack_without_a_covered_emit_folds_no_sample() {
    // The bootstrap cut starts at 5; seqs below it were never timed here.
    let mut sync = consumer(5);
    emit(&mut sync, 1);
    assert_eq!(sync.ack(4), Ok(None));
    assert_eq!(sync.last_acked_seq, 4);
    assert!(sync.rtt.smoothed().is_none());
    assert_eq!(sync.emit_instants.len(), 1, "seq 5 is still in flight");
}

#[test]
fn emit_instants_are_capped_keeping_the_newest() {
    let mut sync = consumer(1);
    let extra = 64;
    for version in 0..(MAX_EMIT_INSTANTS + extra) {
        emit(&mut sync, u32::try_from(version).expect("small"));
    }
    assert_eq!(sync.emit_instants.len(), MAX_EMIT_INSTANTS);
    assert_eq!(
        sync.emit_instants.keys().next().copied(),
        Some(u64::try_from(extra).expect("small") + 1),
    );
}

#[test]
fn emit_once_keeps_no_snapshots() {
    let mut sync = consumer(1);
    let seq = sync.allocate_seq();
    sync.record_emit(seq, || panic!("emit-once must not snapshot"));
    assert!(sync.pending_refs.is_empty());
    assert!(sync.ack(seq).is_ok());
    assert_eq!(
        sync.acked_reference, 0,
        "emit-once never moves the acked ref"
    );
}

#[test]
fn loss_tolerant_ack_advances_to_highest_covered_snapshot() {
    let mut sync = consumer(1);
    sync.loss_tolerant = true;
    for version in [10, 20, 30] {
        emit(&mut sync, version);
    }
    assert_eq!(sync.acked_reference, 0, "emit alone does not advance it");

    assert!(sync.ack(2).is_ok());
    assert_eq!(sync.acked_reference, 20);
    assert_eq!(
        sync.pending_refs.keys().copied().collect::<Vec<_>>(),
        vec![3]
    );
    assert!(sync.must_walk_when_clean(), "seq 3 is still un-acked");

    assert!(sync.ack(3).is_ok());
    assert_eq!(sync.acked_reference, 30);
    assert!(sync.pending_refs.is_empty());
}

#[test]
fn loss_tolerant_pending_refs_are_capped() {
    let mut sync = consumer(1);
    sync.loss_tolerant = true;
    for version in 0..=MAX_EMIT_INSTANTS {
        emit(&mut sync, u32::try_from(version).expect("small"));
    }
    assert_eq!(sync.pending_refs.len(), MAX_EMIT_INSTANTS);
    assert_eq!(sync.pending_refs.keys().next().copied(), Some(2));
}

#[tokio::test(start_paused = true)]
async fn loss_tolerant_holds_until_new_content_flush_or_retransmit() {
    let mut sync = consumer(1);
    sync.loss_tolerant = true;
    emit(&mut sync, 1);

    assert!(sync.holds_delta(false, false), "un-acked, nothing new");
    assert!(!sync.holds_delta(true, false), "new content ships");
    assert!(
        !sync.holds_delta(false, true),
        "a backpressured delta ships"
    );

    tokio::time::advance(RETRANSMIT_DEFAULT.saturating_sub(Duration::from_millis(1))).await;
    assert!(!sync.retransmit_due(tokio::time::Instant::now()));
    tokio::time::advance(Duration::from_millis(1)).await;
    assert!(sync.retransmit_due(tokio::time::Instant::now()));
    assert!(!sync.holds_delta(false, false), "a due retransmit ships");

    assert!(sync.ack(1).is_ok());
    assert!(!sync.retransmit_due(tokio::time::Instant::now()));
}

#[test]
fn emit_once_never_holds_a_delta() {
    let mut sync = consumer(1);
    emit(&mut sync, 1);
    assert!(!sync.holds_delta(false, false));
}

#[test]
fn turn_flags_track_initial_walk_and_backpressure() {
    let mut sync = consumer(1);
    assert!(!sync.begin_turn(), "not behind yet");
    assert!(!sync.needs_initial_emit);
    assert!(!sync.must_walk_when_clean());

    sync.mark_behind();
    assert!(sync.must_walk_when_clean());
    assert!(sync.begin_turn(), "owed a backpressured delta");
    sync.mark_served();
    assert!(!sync.must_walk_when_clean());
}

#[test]
fn rtt_estimator_seeds_then_blends() {
    let mut est = RttEstimator::default();
    assert_eq!(est.smoothed(), None);
    assert_eq!(est.desired_tick_interval(), DEFAULT_TICK_INTERVAL);

    est.observe(Duration::from_millis(100));
    assert_eq!(est.smoothed(), Some(Duration::from_millis(100)));
    est.observe(Duration::from_millis(300));
    let srtt = est.smoothed().expect("srtt");
    assert!(
        srtt > Duration::from_millis(100) && srtt < Duration::from_millis(150),
        "{srtt:?}"
    );
}

#[test]
fn cadence_and_retransmit_are_clamped() {
    assert_eq!(
        adaptive_tick_interval(Duration::from_micros(10)),
        MIN_TICK_INTERVAL
    );
    assert_eq!(
        adaptive_tick_interval(Duration::from_millis(80)),
        Duration::from_millis(40)
    );
    assert_eq!(
        adaptive_tick_interval(Duration::from_secs(2)),
        MAX_TICK_INTERVAL
    );

    assert_eq!(retransmit_timeout(None), RETRANSMIT_DEFAULT);
    assert_eq!(
        retransmit_timeout(Some(Duration::from_millis(1))),
        RETRANSMIT_MIN
    );
    assert_eq!(
        retransmit_timeout(Some(Duration::from_millis(100))),
        Duration::from_millis(300)
    );
    assert_eq!(
        retransmit_timeout(Some(Duration::from_secs(5))),
        RETRANSMIT_MAX
    );
}
