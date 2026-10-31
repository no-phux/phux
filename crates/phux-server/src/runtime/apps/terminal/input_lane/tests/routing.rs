use super::*;

/// `INPUT_*` and `ROUTE_INPUT` share one lane FIFO, so a mixed stream
/// reaches the PTY writer in the order routed.
#[tokio::test(flavor = "current_thread")]
async fn attached_and_route_input_share_one_fifo() {
    LocalSet::new()
        .run_until(async {
            let mut fx = fixture();
            fx.attached(fx.client_a, b"a");
            let routed = fx.route(fx.client_a, b"b");
            fx.attached(fx.client_a, b"c");
            assert_eq!(routed.await, CommandResult::Ok);
            for expected in [&b"a"[..], b"b", b"c"] {
                assert_eq!(next_write(&mut fx.pane.writer_rx).await.bytes, expected);
            }
        })
        .await;
}

/// Lease exclusion (ADR-0033) holds on the lane: with B holding the
/// wheel, A's attached input is dropped and A's `ROUTE_INPUT` keeps its
/// typed lease error, while B's input is delivered.
#[tokio::test(flavor = "current_thread")]
async fn lane_honors_input_lease() {
    LocalSet::new()
        .run_until(async {
            let mut fx = fixture();
            fx.state
                .with_mut(|s| s.set_input_lease(fx.pane.core, fx.client_b));
            fx.attached(fx.client_a, b"a");
            let refused = fx.route(fx.client_a, b"r").await;
            assert_eq!(code(&refused), Some(ErrorCode::InputLeaseHeld));
            fx.attached(fx.client_b, b"B");
            assert_eq!(
                next_write(&mut fx.pane.writer_rx).await.bytes.as_ref(),
                b"B"
            );
            assert_no_write(&mut fx.pane.writer_rx).await;
        })
        .await;
}

/// `ROUTE_INPUT` completes while the runtime thread is synchronously
/// busy: only the dedicated lane thread can have routed it.
#[tokio::test(flavor = "current_thread")]
async fn route_input_routes_while_main_runtime_is_not_polling() {
    LocalSet::new()
        .run_until(async {
            let fx = fixture();
            // Enqueue under the state lock so the lane cannot finish
            // before this task stops polling.
            let mut receipt = fx.state.with(|_| {
                let mut receipt = fx.route(fx.client_a, b"k");
                assert!((&mut receipt).now_or_never().is_none());
                receipt
            });
            let deadline = std::time::Instant::now() + LANE_DELIVERY_DEADLINE;
            let routed = loop {
                if let Some(result) = (&mut receipt).now_or_never() {
                    break result;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "lane did not route while the runtime was blocked"
                );
                std::thread::sleep(Duration::from_millis(1));
            };
            assert_eq!(routed, CommandResult::Ok);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn lane_encodes_from_published_bracketed_paste_snapshot() {
    LocalSet::new()
        .run_until(async {
            let mut fx = fixture_with_seed(b"\x1b[?2004h");
            fx.attached(fx.client_a, b"lane");
            assert_eq!(
                next_write(&mut fx.pane.writer_rx).await.bytes.as_ref(),
                b"\x1b[200~lane\x1b[201~"
            );
        })
        .await;
}

/// One encoder set per pane generation: a respawned actor replaces it,
/// and a closed actor's set is pruned.
#[test]
fn encoder_cache_tracks_actor_generations() {
    let first = TerminalActor::new(80, 24).expect("first actor");
    let second = TerminalActor::new(80, 24).expect("second actor");
    let state = SharedState::new();
    let pane = state.with_mut(|s| s.seed_session("s").1);
    let mut encoders = std::collections::HashMap::new();
    let facet = |bundle: &phux_terminal::actor::TerminalActorBundle| {
        bundle.handle.terminal().expect("facet").clone()
    };
    encoder_for(&mut encoders, pane, &facet(&first)).expect("first encoder");
    encoder_for(&mut encoders, pane, &facet(&second)).expect("replacement");
    assert_eq!(encoders.len(), 1);
    assert!(
        encoders[&pane]
            .snapshot
            .same_channel(&facet(&second).input_snapshot)
    );
    drop(second);
    prune_closed_encoders(&mut encoders);
    assert!(encoders.is_empty());
}

/// 1,000 keys spread across one second stay at or under five emissions,
/// and a same-instant burst is a single leading edge. A second pane is
/// counted on its own.
#[test]
fn one_thousand_keys_in_one_second_emit_at_most_five() {
    let pane = phux_protocol::ids::ResourceId::local(3);
    let other = phux_protocol::ids::ResourceId::local(9);
    let start = std::time::Instant::now();
    let mut gate = ActivityGate::default();
    let mut emitted = 0;
    for i in 0..1000u32 {
        let now = start + Duration::from_nanos(u64::from(i) * 1_000_000_000 / 999);
        if gate.admit(&pane, now) {
            emitted += 1;
        }
    }
    assert!(emitted <= 5, "1000 keys in 1s emitted {emitted}");
    assert!(
        emitted >= 4,
        "a leading edge across one second emits about 4 Hz, got {emitted}"
    );

    let mut gate = ActivityGate::default();
    let mut burst = 0;
    for _ in 0..1000 {
        if gate.admit(&pane, start) {
            burst += 1;
        }
    }
    assert_eq!(burst, 1, "a same-instant burst is one leading edge");
    assert!(gate.admit(&other, start), "each pane has its own window");
}

/// One accepted paste tells the other subscriber, not the sender, and
/// leaves the journal head where it was.
#[tokio::test(flavor = "current_thread")]
async fn accepted_input_notifies_only_the_other_subscriber() {
    LocalSet::new()
        .run_until(async {
            let mut fx = fixture();
            let (peer_tx, mut peer_rx) = mpsc::channel(8);
            let (self_tx, mut self_rx) = mpsc::channel(8);
            fx.state.with_mut(|server| {
                server.set_client_name(fx.client_a, "ada".to_owned());
                server.subscribe_events(fx.client_a, Some(fx.pane.wire.clone()), self_tx);
                server.subscribe_events(fx.client_b, Some(fx.pane.wire.clone()), peer_tx);
            });
            let head = fx.state.with(crate::state::ServerState::journal_head);
            fx.attached(fx.client_a, b"k");
            let _ = next_write(&mut fx.pane.writer_rx).await;
            assert_eq!(fx.state.with(crate::state::ServerState::journal_head), head);
            match peer_rx.try_recv().expect("peer sees the origin") {
                crate::mailbox::Outbound::Frame(phux_protocol::wire::frame::FrameKind::Event {
                    event: phux_protocol::wire::frame::AgentEvent::InputActivity { origin },
                    stamp: None,
                    ..
                }) => assert_eq!(origin, "ada"),
                other => panic!("peer frame: {other:?}"),
            }
            assert!(peer_rx.try_recv().is_err(), "one emission for one paste");
            assert!(self_rx.try_recv().is_err(), "no self-echo");
        })
        .await;
}
