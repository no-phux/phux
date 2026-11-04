use std::time::Duration;

use super::{
    GAP_RESYNC_MAX_ATTEMPTS, PumpGeneration, PumpWait, STALE_OUTPUT_BUDGET, is_stale, next_event,
};

use crate::resource::{ResourceOutput, ResyncAudience, ResyncTarget};

fn pump_on(owner: u64, stream: u64) -> ResyncTarget {
    ResyncTarget {
        owner,
        stream_id: phux_protocol::ids::StreamId::new(stream).expect("non-zero stream id"),
        bootstrap_id: bootstrap(7),
    }
}

/// An addressed gap resync replaces only the fenced pump it names; a
/// reflow still replaces everyone's.
#[test]
fn an_addressed_resync_is_taken_only_by_the_fenced_pump_it_names() {
    let stale = pump_on(1, 1);
    let addressed = ResyncAudience::Only(vec![stale].into());

    let mut fenced = opened();
    fenced.fence_for_gap();
    assert!(fenced.takes_resync(&addressed, stale));
    assert!(
        !fenced.takes_resync(&addressed, pump_on(1, 2)),
        "another stream of the same client is a different pump",
    );

    let fresh = opened();
    assert!(
        !fresh.takes_resync(&addressed, pump_on(2, 1)),
        "a fresh pump must not be re-bootstrapped for someone else's gap",
    );
    assert!(
        !fresh.takes_resync(&addressed, stale),
        "a named pump whose gap an earlier resync already healed skips it",
    );

    assert!(fresh.takes_resync(&ResyncAudience::Everyone, pump_on(2, 1)));
    assert!(fenced.takes_resync(&ResyncAudience::Everyone, stale));
}

/// Owner and stream can collide across pump kinds, so a target names the
/// generation too.
#[test]
fn a_resync_for_another_generation_of_the_same_stream_is_not_taken() {
    let mut retired = opened();
    retired.retire();
    let same_stream_other_generation = ResyncTarget {
        bootstrap_id: bootstrap(8),
        ..pump_on(2, 1)
    };
    let audience = ResyncAudience::Only(vec![same_stream_other_generation].into());
    assert!(!retired.takes_resync(&audience, pump_on(2, 1)));
    assert!(retired.takes_resync(&audience, same_stream_other_generation));
}

/// A retired generation takes a resync that names it, and only that.
#[test]
fn a_retired_generation_takes_only_a_resync_that_names_it() {
    let me = pump_on(2, 1);
    let mut retired = opened();
    retired.retire();
    assert!(!retired.is_fenced(), "retirement alone sets no gap fence");
    assert!(retired.takes_resync(&ResyncAudience::Only(vec![me].into()), me));
    assert!(
        !retired.takes_resync(&ResyncAudience::Only(vec![pump_on(1, 1)].into()), me),
        "another pump's resync must not revive a retired pump",
    );
    assert!(retired.takes_resync(&ResyncAudience::Everyone, me));

    retired.republished_at(100);
    assert!(
        !retired.takes_resync(&ResyncAudience::Only(vec![me].into()), me),
        "once republished and unfenced it is an ordinary fresh pump again",
    );
}

/// A chunk that waited behind a republish is aged from the republish.
#[test]
fn chunk_age_starts_no_earlier_than_the_generation_publication() {
    let read_long_ago = std::time::Instant::now()
        .checked_sub(Duration::from_secs(2))
        .expect("monotonic clock has run for two seconds");
    let mut generation = opened();
    generation.republished_at(100);
    assert!(!is_stale(generation.chunk_age(read_long_ago)));
    assert!(
        generation.chunk_age(std::time::Instant::now()) < STALE_OUTPUT_BUDGET,
        "a chunk read after publication is aged from its own read",
    );
}

#[test]
fn tracked_pump_abort_fences_blocked_send_and_reclaims_churn() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    tokio::task::LocalSet::new().block_on(&runtime, async {
        let state = crate::state::SharedState::new();
        let client = crate::state::ClientId(1);
        let terminal = phux_core::ResourceId::default();
        let mut tasks = tokio::task::JoinSet::new();
        let (mailbox, mut receive) = tokio::sync::mpsc::channel(1);
        let (output, _) = tokio::sync::broadcast::channel::<()>(1);
        mailbox.send(0).await.unwrap();
        for round in 1..=40 {
            let subscription = output.subscribe();
            let sender = mailbox.clone();
            let (entered, entry) = tokio::sync::oneshot::channel();
            super::spawn_tracked(
                &state,
                client,
                terminal,
                Some(&mut tasks),
                None,
                async move {
                    let _subscription = subscription;
                    entered.send(()).unwrap();
                    sender.send(round).await.unwrap();
                },
            );
            entry.await.unwrap();
            assert_eq!(output.receiver_count(), 1);
            tokio::time::timeout(
                Duration::from_secs(1),
                super::stop_output(&state, client, terminal),
            )
            .await
            .unwrap();
            assert_eq!(
                output.receiver_count(),
                0,
                "detach dropped the live receiver"
            );
            assert!(tasks.len() <= 1, "completed JoinSet tasks accumulated");
            assert!(
                state
                    .with_mut(|s| s.stop_terminal_output_pumps(client, terminal))
                    .is_empty()
            );
        }
        assert_eq!(receive.recv().await, Some(0));
        assert!(
            receive.try_recv().is_err(),
            "aborted send escaped the detach fence"
        );
        while tasks.join_next().await.is_some() {}
        assert!(tasks.is_empty());
    });
}

#[test]
fn tracked_pump_can_be_detached_before_its_first_poll() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    tokio::task::LocalSet::new().block_on(&runtime, async {
        let state = crate::state::SharedState::new();
        let client = crate::state::ClientId(1);
        let terminal = phux_core::ResourceId::default();
        let (output, _) = tokio::sync::broadcast::channel::<()>(1);
        let receiver = output.subscribe();
        super::spawn_tracked(&state, client, terminal, None, None, async move {
            let _receiver = receiver;
            panic!("aborted task must never run");
        });
        tokio::time::timeout(
            Duration::from_secs(1),
            super::stop_output(&state, client, terminal),
        )
        .await
        .unwrap();
        assert_eq!(output.receiver_count(), 0);
    });
}

fn bootstrap(raw: u64) -> phux_protocol::ids::BootstrapId {
    phux_protocol::ids::BootstrapId::new(raw).expect("non-zero bootstrap id")
}

fn opened() -> PumpGeneration {
    PumpGeneration::opened_at(41, bootstrap(7))
}

/// The live gate: past the cut only, nothing while fenced (a skipped seq
/// would detach the client) or retired, and a republish re-anchors it so
/// replay entries the checkpoint covers are dropped.
#[test]
fn forwards_gates_cut_fence_and_retirement() {
    let mut generation = opened();
    assert!(!generation.forwards(41), "the cut itself is covered");
    assert!(generation.forwards(42));

    assert!(!generation.fence_for_gap(), "first gap is not a repeat");
    assert!(!generation.forwards(20_533));
    assert!(generation.fence_for_gap(), "second gap is a repeat");
    generation.republished_at(9_000);
    assert!(!generation.is_fenced());
    let admitted: Vec<u64> = [8_999_u64, 9_000, 9_001, 9_002]
        .into_iter()
        .filter(|seq| generation.forwards(*seq))
        .collect();
    assert_eq!(admitted, vec![9_001, 9_002]);

    generation.retire();
    assert!(!generation.is_active());
    assert!(!generation.forwards(9_003));
    generation.republished_at(100);
    assert!(generation.forwards(101));
}

/// Retries double up to the cap, the budget runs out, and a republish
/// restores the full budget for a later gap.
#[tokio::test(start_paused = true)]
async fn a_gap_retries_with_backoff_and_republishing_restores_the_budget() {
    let mut generation = opened();
    generation.fence_for_gap();

    let mut delays = Vec::new();
    for _ in 0..GAP_RESYNC_MAX_ATTEMPTS {
        generation.note_resync_requested();
        delays.push(generation.gap_retry_delay());
        tokio::time::advance(generation.gap_retry_delay()).await;
    }

    assert_eq!(
        delays,
        vec![
            Duration::from_millis(500),
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_secs(4),
            Duration::from_secs(4),
        ],
        "doubling backoff, capped at GAP_RESYNC_MAX_BACKOFF",
    );
    assert_eq!(generation.gap_attempts(), GAP_RESYNC_MAX_ATTEMPTS);

    generation.republished_at(9_000);
    assert_eq!(generation.gap_attempts(), 0);
    generation.fence_for_gap();
    generation.note_resync_requested();
    assert_eq!(generation.gap_retry_delay(), Duration::from_millis(500));
}

/// A fenced pump that is never answered ends, rather than retrying for
/// the life of the process.
#[tokio::test(start_paused = true)]
async fn a_fenced_pump_gives_up_once_its_budget_is_spent() {
    let (tx, mut rx) = tokio::sync::broadcast::channel::<ResourceOutput>(8);
    let mut generation = opened();
    generation.fence_for_gap();

    let mut retries = 0_u32;
    loop {
        match next_event(&generation, &mut rx).await {
            PumpWait::RetryResync => {
                retries += 1;
                generation.note_resync_requested();
                assert!(retries < 32, "the fence must not retry forever");
            }
            PumpWait::GapUnrecoverable => break,
            PumpWait::Event(_) | PumpWait::FinalDrain => panic!("nothing was ever broadcast"),
        }
    }
    assert_eq!(
        retries, GAP_RESYNC_MAX_ATTEMPTS,
        "one request per attempt, then the pump gives up",
    );
    drop(tx);
}

/// An actor that answers late still unfences the pump.
#[tokio::test(start_paused = true)]
async fn a_late_resync_still_unfences_the_pump() {
    let (tx, mut rx) = tokio::sync::broadcast::channel::<ResourceOutput>(8);
    let mut generation = opened();
    generation.fence_for_gap();
    generation.note_resync_requested();

    // Two backoff windows late — well past the first deadline, well
    // inside the budget.
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1_600)).await;
        let _ = tx.send(ResourceOutput::Resync {
            cols: 80,
            rows: 24,
            reason: crate::resource::ResyncReason::OutboundGap,
            audience: ResyncAudience::Everyone,
            base_seq: 9_000,
            bytes: bytes::Bytes::new(),
        });
    });

    loop {
        match next_event(&generation, &mut rx).await {
            PumpWait::RetryResync => generation.note_resync_requested(),
            PumpWait::Event(Ok(ResourceOutput::Resync { base_seq, .. })) => {
                generation.republished_at(base_seq);
                break;
            }
            other => panic!(
                "expected the late resync, got {}",
                match other {
                    PumpWait::GapUnrecoverable => "give-up",
                    _ => "another event",
                }
            ),
        }
    }
    assert!(!generation.is_fenced());
    assert!(generation.forwards(9_001));
}

#[tokio::test(start_paused = true)]
async fn unrelated_ready_events_do_not_postpone_the_retry_deadline() {
    let (tx, mut rx) = tokio::sync::broadcast::channel::<ResourceOutput>(8);
    let mut generation = opened();
    generation.fence_for_gap();
    generation.note_resync_requested();
    for _ in 0..4 {
        tokio::time::advance(Duration::from_millis(100)).await;
        tx.send(unrelated_resync()).unwrap();
        assert!(matches!(
            next_event(&generation, &mut rx).await,
            PumpWait::Event(_)
        ));
        assert!(generation.fence_for_gap());
        // A repeated Lagged handler must not spend another attempt or
        // move the deadline even if it redundantly notes a request.
        generation.note_resync_requested();
        assert_eq!(generation.gap_attempts(), 1);
    }
    tokio::time::advance(Duration::from_millis(100)).await;
    tx.send(unrelated_resync()).unwrap();
    assert!(matches!(
        next_event(&generation, &mut rx).await,
        PumpWait::RetryResync
    ));
}

#[tokio::test(start_paused = true)]
async fn final_request_has_a_response_window_before_failure() {
    let (tx, mut rx) = tokio::sync::broadcast::channel::<ResourceOutput>(8);
    let mut generation = opened();
    generation.fence_for_gap();
    for attempt in 0..GAP_RESYNC_MAX_ATTEMPTS {
        generation.note_resync_requested();
        if attempt + 1 < GAP_RESYNC_MAX_ATTEMPTS {
            tokio::time::advance(generation.gap_retry_delay()).await;
        }
    }
    tokio::time::advance(Duration::from_secs(3)).await;
    generation.note_resync_requested();
    assert_eq!(generation.gap_attempts(), GAP_RESYNC_MAX_ATTEMPTS);
    tx.send(unrelated_resync()).unwrap();
    assert!(matches!(
        next_event(&generation, &mut rx).await,
        PumpWait::Event(Ok(_))
    ));
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(matches!(
        next_event(&generation, &mut rx).await,
        PumpWait::GapUnrecoverable
    ));
}

fn unrelated_resync() -> ResourceOutput {
    ResourceOutput::Resync {
        cols: 80,
        rows: 24,
        reason: crate::resource::ResyncReason::OutboundGap,
        audience: ResyncAudience::Only(vec![pump_on(99, 99)].into()),
        base_seq: 9_000,
        bytes: bytes::Bytes::new(),
    }
}
