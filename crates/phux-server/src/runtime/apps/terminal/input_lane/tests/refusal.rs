use super::*;

/// Every pre-handoff refusal happens before a write: a reused id with a
/// new payload, an empty or oversized batch, an unsafe paste, encoded
/// bytes past the body limit, and a satellite target, which also binds
/// no id (L1 §9.1).
#[tokio::test(flavor = "current_thread")]
async fn apply_input_refuses_before_handoff() {
    LocalSet::new()
        .run_until(async {
            let mut fx = fixture();
            let first = fx.apply(2, &[b"original"]);
            complete(
                next_write(&mut fx.pane.writer_rx).await,
                WriteCompletion::Delivered,
            );
            assert_eq!(first.await, CommandResult::Ok);

            let handle = fx.handle();
            let wire = fx.pane.wire.clone();
            let unsafe_paste = InputEvent::Paste(PasteEvent {
                trust: PasteTrust::Untrusted,
                data: b"danger\n".to_vec(),
            });
            let too_many = std::iter::repeat_with(|| ev(b"x"))
                .take(MAX_APPLY_INPUT_EVENTS + 1)
                .collect();
            let oversized = vec![ev(&vec![b'x'; MAX_APPLY_INPUT_COMMAND_BODY + 1])];
            let satellite = phux_protocol::ResourceId::satellite("peer", 1);
            let cases = [
                (
                    2,
                    wire.clone(),
                    vec![ev(b"different")],
                    ErrorCode::InvalidCommand,
                ),
                (3, wire.clone(), vec![], ErrorCode::InvalidCommand),
                (4, wire.clone(), too_many, ErrorCode::InvalidCommand),
                (
                    5,
                    wire.clone(),
                    vec![ev(b"prefix"), unsafe_paste],
                    ErrorCode::UnsafePaste,
                ),
                (13, wire.clone(), oversized, ErrorCode::InvalidCommand),
                (
                    12,
                    satellite,
                    vec![ev(b"x")],
                    ErrorCode::UnsupportedSatelliteRoute,
                ),
            ];
            for (id, target, events, want) in cases {
                let result = handle
                    .begin_apply(&fx.state, fx.client_a, operation_id(id), target, events)
                    .await;
                assert_eq!(code(&result), Some(want), "operation {id}");
            }
            assert!(
                matches!(
                    handle
                        .cache
                        .claim_at(operation_id(12), [0xee; 32], std::time::Instant::now()),
                    CacheClaim::Owner
                ),
                "a refused satellite batch leaves its id unbound"
            );
            assert_no_write(&mut fx.pane.writer_rx).await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn apply_input_reports_mailbox_full_without_handoff() {
    let bundle = TerminalActor::new(80, 24).expect("actor");
    let facet = bundle.handle.terminal().expect("facet");
    for _ in 0..DEFAULT_INPUT_MAILBOX {
        facet
            .encoded_input
            .try_send(EncodedInputRequest::legacy(vec![b'x']))
            .expect("fill mailbox");
    }
    let state = SharedState::new();
    let (wire, client) = state.with_mut(|s| {
        let pane = s.seed_session("s").1;
        let wire = s.register_resource_handle(pane, bundle.handle.clone(), bundle.token.clone());
        (wire, s.new_client_id())
    });
    let lane = spawn_input_lane(state.clone()).expect("spawn lane");
    let result = lane
        .handle()
        .begin_apply(&state, client, operation_id(6), wire, vec![ev(b"x")])
        .await;
    assert_eq!(code(&result), Some(ErrorCode::ResourceExhausted));
}

/// phux-w7z2.60: a batch that provably never reached a PTY writer is
/// `INPUT_NOT_WRITTEN`, not unknown: a pane with no PTY, an actor gone
/// before handoff, a closed writer channel.
#[tokio::test(flavor = "current_thread")]
async fn apply_input_reports_not_written_when_no_writer_can_receive() {
    LocalSet::new()
        .run_until(async {
            for case in ["no-pty", "actor-gone", "writer-closed"] {
                let state = SharedState::new();
                let (token, wire) = if case == "writer-closed" {
                    let pane = spawn_pane(&state, "s", b"");
                    drop(pane.writer_rx);
                    (pane.token, pane.wire)
                } else {
                    let bundle = TerminalActor::new(80, 24).expect("actor");
                    let handle = bundle.handle.clone();
                    let token = bundle.token.clone();
                    if case == "no-pty" {
                        tokio::task::spawn_local(bundle.actor.run());
                    } else {
                        drop(bundle);
                    }
                    let wire = state.with_mut(|s| {
                        let pane = s.seed_session("s").1;
                        s.register_resource_handle(pane, handle, token.clone())
                    });
                    (token, wire)
                };
                let client = state.with_mut(crate::state::ServerState::new_client_id);
                let lane = spawn_input_lane(state.clone()).expect("spawn lane");
                let result = tokio::time::timeout(
                    LANE_DELIVERY_DEADLINE,
                    lane.handle().begin_apply(
                        &state,
                        client,
                        operation_id(8),
                        wire,
                        vec![ev(b"x")],
                    ),
                )
                .await
                .expect("must not hang");
                assert_eq!(code(&result), Some(ErrorCode::InputNotWritten), "{case}");
                token.cancel();
            }
        })
        .await;
}

/// A pane with no free input credit (ADR-0144) refuses `APPLY_INPUT`
/// before handoff, without waiting out the completion timeout and
/// without growing the writer queue; the id stays retryable.
#[tokio::test(flavor = "current_thread")]
async fn apply_input_to_a_saturated_pane_is_refused_before_handoff() {
    LocalSet::new()
        .run_until(async {
            let mut fx = fixture();
            let mut held: Vec<_> = std::iter::from_fn(|| fx.pane.credits.try_take()).collect();
            assert_eq!(held.len(), INPUT_CREDITS);
            // The deadline is the assertion: strictly under the timeout.
            let result = tokio::time::timeout(
                ACKNOWLEDGED_COMPLETION_TIMEOUT / 2,
                fx.apply(18, &[b"first"]),
            )
            .await
            .expect("resolves without the completion timeout");
            assert_eq!(code(&result), Some(ErrorCode::ResourceExhausted));
            assert_no_write(&mut fx.pane.writer_rx).await;

            held.pop();
            let retry = fx.apply(18, &[b"first"]);
            let request = next_write(&mut fx.pane.writer_rx).await;
            assert_eq!(request.bytes.as_ref(), b"first");
            complete(request, WriteCompletion::Delivered);
            assert_eq!(retry.await, CommandResult::Ok);
        })
        .await;
}
