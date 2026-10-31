use super::*;

/// phux-w7z2.58: admission is per Terminal and the completion wait is
/// off the lane thread. With A's batch unresolved, another pane still
/// admits `APPLY_INPUT` and `ROUTE_INPUT` (fleet fan-out used to collide,
/// and one stalled writer used to freeze every pane's input).
#[tokio::test(flavor = "current_thread")]
async fn owned_receipts_admit_other_input_before_first_completion() {
    LocalSet::new()
        .run_until(async {
            let mut fx = fixture();
            let mut other = spawn_pane(&fx.state, "t", b"");
            let handle = fx.handle();

            let first_receipt = fx.apply(24, &[b"held"]);
            let first = next_write(&mut fx.pane.writer_rx).await;

            let second_receipt = handle.begin_apply(
                &fx.state,
                fx.client_a,
                operation_id(25),
                other.wire.clone(),
                vec![ev(b"other")],
            );
            complete(
                next_write(&mut other.writer_rx).await,
                WriteCompletion::Delivered,
            );
            assert_eq!(second_receipt.await, CommandResult::Ok);

            let routed = tokio::time::timeout(
                ACKNOWLEDGED_COMPLETION_TIMEOUT / 2,
                handle.begin_route(
                    &fx.state,
                    fx.client_a,
                    other.wire.clone(),
                    ev(b"control"),
                    None,
                ),
            )
            .await
            .expect("ROUTE_INPUT must not wait on another pane's completion");
            assert_eq!(routed, CommandResult::Ok);
            assert_eq!(
                next_write(&mut other.writer_rx).await.bytes.as_ref(),
                b"control"
            );

            complete(first, WriteCompletion::Delivered);
            assert_eq!(first_receipt.await, CommandResult::Ok);
            other.token.cancel();
        })
        .await;
}

/// Same-pane ordering comes from the pane's FIFO mailbox: input routed
/// after an unresolved batch still reaches the writer behind it.
#[tokio::test(flavor = "current_thread")]
async fn later_input_to_the_same_pane_stays_behind_an_unresolved_batch() {
    LocalSet::new()
        .run_until(async {
            let mut fx = fixture();
            let batch = fx.apply(23, &[b"batch"]);
            let first = next_write(&mut fx.pane.writer_rx).await;
            assert_eq!(first.bytes.as_ref(), b"batch");
            assert_eq!(fx.route(fx.client_a, b"after").await, CommandResult::Ok);
            assert_eq!(
                next_write(&mut fx.pane.writer_rx).await.bytes.as_ref(),
                b"after"
            );
            complete(first, WriteCompletion::Delivered);
            assert_eq!(batch.await, CommandResult::Ok);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn acknowledged_admission_allows_only_one_operation_per_terminal_until_completion() {
    LocalSet::new()
        .run_until(async {
            let mut fx = fixture();
            let first = fx.apply(14, &[b"first"]);
            let first_request = next_write(&mut fx.pane.writer_rx).await;
            let second = tokio::time::timeout(LANE_DELIVERY_DEADLINE, fx.apply(15, &[b"second"]))
                .await
                .expect("second admission returns immediately");
            assert_eq!(code(&second), Some(ErrorCode::ResourceExhausted));
            complete(first_request, WriteCompletion::Delivered);
            assert_eq!(first.await, CommandResult::Ok);

            let third = fx.apply(16, &[b"third"]);
            let third_request = next_write(&mut fx.pane.writer_rx).await;
            assert_eq!(third_request.bytes.as_ref(), b"third");
            complete(third_request, WriteCompletion::Delivered);
            assert_eq!(third.await, CommandResult::Ok);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn acknowledged_queue_full_rejects_without_leaking_reservation() {
    let (tx, _rx) = mpsc::channel(1);
    let admission = Arc::new(AcknowledgedAdmission::default());
    let handle = InputLaneHandle {
        tx,
        admission: Arc::clone(&admission),
        cache: SharedOperationCache::default(),
    };
    let terminal = phux_protocol::ResourceId::local(1);
    handle
        .route(RoutedInput::attached(
            ClientId(1),
            terminal.clone(),
            paste(b"legacy"),
            "INPUT_PASTE",
        ))
        .await;
    let state = SharedState::new();
    let result = handle
        .begin_apply(
            &state,
            ClientId(1),
            operation_id(17),
            terminal.clone(),
            vec![ev(b"ack")],
        )
        .await;
    assert_eq!(code(&result), Some(ErrorCode::ResourceExhausted));
    assert!(!admission.is_in_flight(&terminal));
}
