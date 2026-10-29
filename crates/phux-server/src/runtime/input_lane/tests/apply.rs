use super::*;

#[tokio::test(flavor = "current_thread")]
async fn apply_input_writes_one_atomic_batch_then_acks_and_deduplicates() {
    LocalSet::new()
        .run_until(async {
            let mut fx = fixture();
            let first = fx.apply(1, &[b"one", b"two"]);
            let request = next_write(&mut fx.pane.writer_rx).await;
            assert_eq!(request.bytes.as_ref(), b"onetwo");
            complete(request, WriteCompletion::Delivered);
            assert_eq!(first.await, CommandResult::Ok);
            assert_eq!(fx.apply(1, &[b"one", b"two"]).await, CommandResult::Ok);
            assert_no_write(&mut fx.pane.writer_rx).await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn same_id_retry_joins_the_unresolved_operation() {
    LocalSet::new()
        .run_until(async {
            let mut fx = fixture();
            let first = fx.apply(24, &[b"once"]);
            let request = next_write(&mut fx.pane.writer_rx).await;
            let mut retry = fx.apply(24, &[b"once"]);
            assert!(
                tokio::time::timeout(NOTHING_FURTHER_WINDOW, &mut retry)
                    .await
                    .is_err(),
                "the retry waits for the operation's outcome"
            );
            complete(request, WriteCompletion::Delivered);
            assert_eq!(first.await, CommandResult::Ok);
            assert_eq!(retry.await, CommandResult::Ok);
            assert_no_write(&mut fx.pane.writer_rx).await;
        })
        .await;
}

/// The dedupe lookup precedes per-Terminal admission.
#[tokio::test(flavor = "current_thread")]
async fn cached_operation_is_readable_while_another_id_holds_admission() {
    LocalSet::new()
        .run_until(async {
            let mut fx = fixture();
            let first = fx.apply(25, &[b"cached"]);
            complete(
                next_write(&mut fx.pane.writer_rx).await,
                WriteCompletion::Delivered,
            );
            assert_eq!(first.await, CommandResult::Ok);

            let active = fx.apply(26, &[b"active"]);
            let active_write = next_write(&mut fx.pane.writer_rx).await;
            assert_eq!(fx.apply(25, &[b"cached"]).await, CommandResult::Ok);
            complete(active_write, WriteCompletion::Delivered);
            assert_eq!(active.await, CommandResult::Ok);
            assert_no_write(&mut fx.pane.writer_rx).await;
        })
        .await;
}

/// A writer that fails, or drops the request, after the handoff leaves
/// the delivery unknown; the retry replays that answer and never writes
/// again.
#[tokio::test(flavor = "current_thread")]
async fn writer_failure_after_handoff_is_unknown_and_deduplicated() {
    LocalSet::new()
        .run_until(async {
            let mut fx = fixture();
            for (id, fail) in [(7_u8, true), (27, false)] {
                let first = fx.apply(id, &[b"uncertain"]);
                let request = next_write(&mut fx.pane.writer_rx).await;
                if fail {
                    complete(request, WriteCompletion::Failed);
                } else {
                    drop(request);
                }
                let result = first.await;
                assert_eq!(code(&result), Some(ErrorCode::InputDeliveryUnknown));
                assert_eq!(fx.apply(id, &[b"uncertain"]).await, result);
                assert_no_write(&mut fx.pane.writer_rx).await;
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn apply_input_completion_timeout_is_cached_and_late_completion_is_harmless() {
    LocalSet::new()
        .run_until(async {
            let mut fx = fixture();
            fx.lane = spawn_input_lane_with_completion_timeout(
                fx.state.clone(),
                Duration::from_millis(10),
            )
            .expect("spawn lane");
            let first = fx.apply(10, &[b"timeout"]);
            let late = next_write(&mut fx.pane.writer_rx).await;
            let result = tokio::time::timeout(LANE_DELIVERY_DEADLINE, first)
                .await
                .expect("bounded completion wait");
            assert_eq!(code(&result), Some(ErrorCode::InputDeliveryUnknown));
            complete(late, WriteCompletion::Delivered);
            assert_eq!(fx.apply(10, &[b"timeout"]).await, result);
            assert_no_write(&mut fx.pane.writer_rx).await;
        })
        .await;
}

/// A caller that stops waiting still gets its success cached.
#[tokio::test(flavor = "current_thread")]
async fn apply_input_caches_success_after_result_waiter_is_dropped() {
    LocalSet::new()
        .run_until(async {
            let mut fx = fixture();
            drop(fx.apply(11, &[b"detached-caller"]));
            complete(
                next_write(&mut fx.pane.writer_rx).await,
                WriteCompletion::Delivered,
            );
            while fx.handle().admission.is_in_flight(&fx.pane.wire) {
                tokio::task::yield_now().await;
            }
            assert_eq!(fx.apply(11, &[b"detached-caller"]).await, CommandResult::Ok);
            assert_no_write(&mut fx.pane.writer_rx).await;
        })
        .await;
}

/// A lease refusal keeps the id bound to its payload but retryable.
#[tokio::test(flavor = "current_thread")]
async fn apply_input_honors_lease_and_retains_id_binding_after_refusal() {
    LocalSet::new()
        .run_until(async {
            let mut fx = fixture();
            fx.state
                .with_mut(|s| s.set_input_lease(fx.pane.core, fx.client_b));
            assert_eq!(
                code(&fx.apply(9, &[b"blocked"]).await),
                Some(ErrorCode::InputLeaseHeld)
            );
            assert_eq!(
                code(&fx.apply(9, &[b"different"]).await),
                Some(ErrorCode::InvalidCommand)
            );
            fx.state
                .with_mut(|s| s.release_input_lease(fx.pane.core, fx.client_b));
            let retry = fx.apply(9, &[b"blocked"]);
            let request = next_write(&mut fx.pane.writer_rx).await;
            assert_eq!(request.bytes.as_ref(), b"blocked");
            complete(request, WriteCompletion::Delivered);
            assert_eq!(retry.await, CommandResult::Ok);
        })
        .await;
}
