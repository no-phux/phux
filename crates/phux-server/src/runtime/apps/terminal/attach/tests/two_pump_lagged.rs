//! Two pumps sharing one stream: lag and a saturated resync mailbox.
#![allow(
    clippy::wildcard_imports,
    reason = "the attach test topics share the parent test namespace"
)]

use super::*;

/// A consumer whose mailbox fills is re-bootstrapped alone, without
/// waiting for it to drain; its neighbour sees every chunk on its
/// original generation.
#[tokio::test(flavor = "current_thread")]
async fn a_lagged_consumer_resyncs_without_republishing_its_neighbour() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (output, _seed) = tokio::sync::broadcast::channel::<ResourceOutput>(4);
            let (resize_tx, mut resize_rx) = ResizeSender::channel(8);
            let mut fresh = spawn_two_pump_consumer(1, 0, 64, &output, &resize_tx);
            let mut lagging = spawn_two_pump_consumer(2, 0, 1, &output, &resize_tx);
            let initial = two_pump_initial_generation();
            let replacement = next_bootstrap_id(initial);

            // The lagging consumer's one slot takes seq 1; seq 2 is a gap.
            let now = std::time::Instant::now();
            for seq in 1..=9 {
                output.send(live(seq, now)).expect("pumps subscribed");
                let_pumps_run().await;
            }
            let request = resync_request_from(&mut resize_rx, &mut lagging).await;
            assert_eq!(
                request.resync_for,
                Some(ResyncTarget {
                    owner: 2,
                    stream_id: two_pump_stream(),
                    bootstrap_id: two_pump_initial_generation(),
                }),
                "the resync names the lagging pump, not the pane",
            );

            assert_eq!(
                frames_seen(&mut lagging, 1).await,
                vec![Seen::Output {
                    generation: initial,
                    seq: 1
                }],
            );
            answer_gap_resync(&output, &request, 9);
            assert_eq!(
                frames_seen(&mut lagging, 4).await,
                vec![
                    Seen::Tombstone,
                    Seen::Begin {
                        generation: replacement,
                        base_seq: 9
                    },
                    Seen::Chunk,
                    Seen::Ready {
                        generation: replacement
                    },
                ],
                "the lagging consumer converges onto a fresh generation",
            );

            // Stamped after the republish so it cannot age past the budget.
            output
                .send(live(10, std::time::Instant::now()))
                .expect("pumps subscribed");

            let expected_fresh: Vec<_> = (1..=10)
                .map(|seq| Seen::Output {
                    generation: initial,
                    seq,
                })
                .collect();
            assert_eq!(frames_seen(&mut fresh, 10).await, expected_fresh);
            assert_eq!(
                frames_seen(&mut lagging, 1).await,
                vec![Seen::Output {
                    generation: replacement,
                    seq: 10
                }],
            );
            assert_quiet(&mut fresh, "the fresh consumer").await;
            assert_quiet(&mut lagging, "the lagging consumer").await;

            drop(output);
            for consumer in [fresh, lagging] {
                let fault = consumer.task.await.expect("pump task");
                assert!(fault.is_none(), "pumps stop cleanly when the pane closes");
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn saturated_resync_mailbox_blocks_until_actor_accepts_request() {
    let (tx, mut rx) = ResizeSender::channel(1);
    tx.send(ResizeRequest {
        cols: 80,
        rows: 24,
        cell_px: None,
        resync_clients: false,
        resync_only: true,
        resync_for: None,
    })
    .await
    .expect("occupy resize mailbox");

    let pump = ResyncTarget {
        owner: 7,
        stream_id: StreamId::new(3).expect("stream id"),
        bootstrap_id: BootstrapId::new(1).expect("bootstrap id"),
    };
    let mut pending = Box::pin(enqueue_output_resync(&tx, pump));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(10), &mut pending)
            .await
            .is_err(),
        "lagged pump must not resume while the resync mailbox is full"
    );
    assert!(
        rx.recv().await.expect("occupied request").resync_only,
        "first request is the existing mailbox occupant"
    );
    assert!(pending.await, "resync queues once capacity is available");
    let queued = rx.recv().await.expect("queued resync");
    assert!(queued.resync_only && queued.resync_clients);
    assert_eq!(
        queued.resync_for,
        Some(pump),
        "a gap resync names the pump that fell behind, so nobody else republishes",
    );

    drop(rx);
    assert!(
        !enqueue_output_resync(&tx, pump).await,
        "closed actor mailbox fails instead of resuming delta forwarding"
    );
}
