use super::*;

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[test]
fn native_step_allocation_never_exceeds_remaining_capture_budget() {
    assert_eq!(native_step_bytes(10, 7, 8).expect("remaining step"), 3);
    assert_eq!(native_step_bytes(10, 0, 8).expect("full step"), 8);
    assert!(matches!(
        native_step_bytes(10, 10, 8),
        Err(crate::native_state::NativeStateError::LimitExceeded)
    ));
}

/// The bootstrap scratch is sized for one record, not the staging budget.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[test]
fn initial_native_scratch_is_one_record_window_not_the_staging_budget() {
    use super::super::native::{INITIAL_NATIVE_SCRATCH_BYTES, initial_native_scratch_bytes};

    assert_eq!(
        initial_native_scratch_bytes(crate::native_state::MAX_NATIVE_PREFIX_BYTES),
        INITIAL_NATIVE_SCRATCH_BYTES,
    );
    assert_eq!(initial_native_scratch_bytes(4_096), 4_096);
    assert_eq!(
        initial_native_scratch_bytes(INITIAL_NATIVE_SCRATCH_BYTES),
        INITIAL_NATIVE_SCRATCH_BYTES,
    );
}

/// Scrollback pushes the prefix past the 64 KiB seed window, so the
/// `OutOfSpace` retry must widen scratch.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
async fn progressive_native_ready_stays_within_one_seed_window() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let bundle = TerminalActor::new(200, 50).expect("new actor");
            let handle = bundle.handle.clone();
            let token = bundle.token.clone();
            let actor = bundle.actor;
            {
                let mut terminal = actor.terminal.borrow_mut();
                for row in 0..2_000 {
                    terminal.vt_write(format!("scratch-row-{row:05} {row:<180}\r\n").as_bytes());
                }
            }
            let (reply, replied) = oneshot::channel();
            handle
                .terminal()
                .expect("terminal facet")
                .native_bootstrap
                .send(NativeBootstrapRequest {
                    owner: 3,
                    terminal_id: phux_protocol::ids::ResourceId::local(1),
                    stream_id: phux_protocol::ids::StreamId::new(1).expect("stream id"),
                    bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("bootstrap id"),
                    limits: phux_protocol::caps::BootstrapLimits::new(
                        phux_protocol::MAX_BOOTSTRAP_CHUNK_BYTES,
                        phux_protocol::DEFAULT_HISTORY_PAGE_BYTES,
                    )
                    .expect("wide negotiated bootstrap bound"),
                    max_bytes: crate::native_state::MAX_NATIVE_PREFIX_BYTES,
                    max_frames: crate::native_state::MAX_NATIVE_BOOTSTRAP_FRAMES,
                    reply,
                })
                .await
                .expect("send native request");
            let run = tokio::task::spawn_local(actor.run());
            let capture = tokio::time::timeout(ACTOR_EXIT_DEADLINE, replied)
                .await
                .expect("native bootstrap stalled")
                .expect("native reply dropped")
                .expect("native capture");
            let widest = capture
                .frames
                .iter()
                .filter_map(|frame| match frame {
                    FrameKind::BootstrapChunk { payload, .. } => Some(payload.len()),
                    _ => None,
                })
                .max()
                .expect("at least one bootstrap chunk");
            assert!(
                widest <= super::super::native::INITIAL_NATIVE_SCRATCH_BYTES,
                "progressive READY record exceeded the {} byte seed window: {widest}",
                super::super::native::INITIAL_NATIVE_SCRATCH_BYTES,
            );
            assert!(
                capture
                    .frames
                    .iter()
                    .any(|frame| matches!(frame, FrameKind::BootstrapReady { .. })),
                "bootstrap must reach READY without eager history",
            );
            token.cancel();
            run.await.expect("actor run");
        })
        .await;
}

/// Every reader of the canonical terminal degrades, rather than aborts,
/// while a capture holds it.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
async fn every_terminal_reader_degrades_while_a_capture_holds_it() {
    let bundle = TerminalActor::new(80, 24).expect("new actor");
    let mut actor = bundle.actor;
    let (reply, _replied) = oneshot::channel();
    actor.start_native_bootstrap(NativeBootstrapRequest {
        owner: 31,
        terminal_id: phux_protocol::ids::ResourceId::local(1),
        stream_id: phux_protocol::ids::StreamId::new(1).expect("stream id"),
        bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("bootstrap id"),
        limits: phux_protocol::caps::BootstrapLimits::default(),
        max_bytes: crate::native_state::MAX_NATIVE_PREFIX_BYTES,
        max_frames: crate::native_state::MAX_NATIVE_BOOTSTRAP_FRAMES,
        reply,
    });
    assert!(
        actor.terminal.borrow().try_terminal().is_none(),
        "the capture must actually hold the terminal for this to test anything",
    );

    // None may panic; each must report the loan.
    assert!(
        matches!(
            actor.synthesize_with_scrollback(None),
            Err(crate::grid::SynthesisError::TerminalUnavailable)
        ),
        "snapshot synthesis must refuse, not abort",
    );
    assert!(
        matches!(
            actor.screen_state(1, None, false, 0),
            Err(crate::grid::SynthesisError::TerminalUnavailable)
        ),
        "GET_SCREEN must refuse, not abort",
    );
    assert!(
        actor.viewport_lines().is_none(),
        "the agent detector must skip its tick, not abort",
    );
    assert!(
        !actor.refresh_title(),
        "a title read must report unchanged, not abort",
    );
    // Void readers: the assertion is simply that these return at all.
    actor.publish_input_snapshot();

    // ...and the terminal is usable again once the capture lands.
    actor.land_native_cuts();
    assert!(
        actor.terminal.borrow().try_terminal().is_some(),
        "landing the cut must return the terminal",
    );
    assert!(
        actor.synthesize_with_scrollback(None).is_ok(),
        "synthesis works once it is back"
    );
}

/// `flush_final_gap_resync` runs outside the bootstrap guards; with a
/// capture in flight and a queued resize it must land the cut first.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
async fn teardown_drain_survives_an_in_flight_native_capture() {
    let bundle = TerminalActor::new(80, 24).expect("new actor");
    let mut actor = bundle.actor;
    let (reply, replied) = oneshot::channel();
    actor.start_native_bootstrap(NativeBootstrapRequest {
        owner: 31,
        terminal_id: phux_protocol::ids::ResourceId::local(1),
        stream_id: phux_protocol::ids::StreamId::new(1).expect("stream id"),
        bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("bootstrap id"),
        limits: phux_protocol::caps::BootstrapLimits::default(),
        max_bytes: crate::native_state::MAX_NATIVE_PREFIX_BYTES,
        max_frames: crate::native_state::MAX_NATIVE_BOOTSTRAP_FRAMES,
        reply,
    });
    assert!(
        actor.pending_native_bootstrap.is_some(),
        "the capture must be in flight for this to be the race under test",
    );

    // The queued resize is what reaches the terminal during the drain.
    bundle
        .handle
        .terminal()
        .expect("terminal facet")
        .resize
        .send(super::super::ResizeRequest {
            cols: 100,
            rows: 40,
            cell_px: None,
            resync_clients: true,
            resync_only: false,
            resync_for: None,
        })
        .await
        .expect("queue a resize behind the capture");

    let mut resync = super::super::run_loop::ResyncDebounce::idle();
    let _ = actor.flush_final_gap_resync(&mut resync);

    assert!(
        actor.pending_native_bootstrap.is_none(),
        "the in-flight cut must be landed, not left holding the terminal",
    );
    assert!(
        replied.await.expect("capture reply").is_err(),
        "the waiter must be answered, not left hanging on a cut that was discarded",
    );
    actor.terminal.borrow_mut().vt_write(b"ok");
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
async fn native_bootstrap_capture_lifetime_retires_staged_state() {
    let bundle = TerminalActor::new(80, 24).expect("new actor");
    let mut actor = bundle.actor;
    let (reply, replied) = oneshot::channel();
    actor.start_native_bootstrap(NativeBootstrapRequest {
        owner: 31,
        terminal_id: phux_protocol::ids::ResourceId::local(1),
        stream_id: phux_protocol::ids::StreamId::new(1).expect("stream id"),
        bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("bootstrap id"),
        limits: phux_protocol::caps::BootstrapLimits::default(),
        max_bytes: crate::native_state::MAX_NATIVE_PREFIX_BYTES,
        max_frames: crate::native_state::MAX_NATIVE_BOOTSTRAP_FRAMES,
        reply,
    });
    actor
        .pending_native_bootstrap
        .as_mut()
        .expect("pending capture")
        .started_at = tokio::time::Instant::now()
        .checked_sub(super::super::NATIVE_CAPTURE_LIFETIME + std::time::Duration::from_millis(1))
        .expect("test instant");

    actor.step_native_bootstrap();

    assert!(actor.pending_native_bootstrap.is_none());
    assert_eq!(
        replied.await.expect("capture reply").unwrap_err(),
        crate::native_state::NativeStateError::LimitExceeded
    );
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
async fn frame_ack_waits_until_progressive_prefix_returns_the_terminal() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let bundle = TerminalActor::new(80, 24).expect("new actor");
            let handle = bundle.handle.clone();
            let token = bundle.token.clone();
            let mut actor = bundle.actor;
            let client_id = ClientId(41);
            let stream_id = phux_protocol::ids::StreamId::new(42).expect("stream id");
            let bootstrap_id = phux_protocol::ids::BootstrapId::new(43).expect("bootstrap id");
            let (outbound, _outbound_rx) = mpsc::channel(4);
            let (_live_gate_tx, live_gate) = watch::channel(true);
            let (attach_reply, attached) = oneshot::channel();
            actor.handle_consumer_attach(ConsumerAttachRequest {
                image_protocols: phux_protocol::caps::ImageProtocolSet::all(),
                client_id,
                outbound,
                wire_terminal_id: 1,
                stream_id,
                bootstrap_id,
                wants_state_sync: true,
                state_sync_scrollback: None,
                bootstrap_max_bytes: usize::MAX,
                bootstrap_max_frames: usize::MAX,
                bootstrap_chunk_bytes: 1,
                loss_tolerant: false,
                live_gate,
                reply: attach_reply,
            });
            attached.await.expect("attach reply").expect("attach");

            let (capture_reply, captured) = oneshot::channel();
            actor.start_native_bootstrap(NativeBootstrapRequest {
                owner: 41,
                terminal_id: phux_protocol::ids::ResourceId::local(1),
                stream_id,
                bootstrap_id,
                limits: phux_protocol::caps::BootstrapLimits::default(),
                max_bytes: crate::native_state::MAX_NATIVE_PREFIX_BYTES,
                max_frames: crate::native_state::MAX_NATIVE_BOOTSTRAP_FRAMES,
                reply: capture_reply,
            });
            handle
                .terminal()
                .expect("terminal facet")
                .consumer_ack
                .send(ConsumerAckRequest {
                    client_id,
                    stream_id,
                    bootstrap_id,
                    seq: 1,
                })
                .await
                .expect("queue ack during prefix");

            let run = tokio::task::spawn_local(actor.run());
            tokio::time::timeout(ACTOR_EXIT_DEADLINE, captured)
                .await
                .expect("prefix stalled behind deferred ack")
                .expect("capture reply dropped")
                .expect("progressive prefix");
            token.cancel();
            run.await.expect("actor run");
        })
        .await;
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
async fn native_request_runs_after_one_bounded_pty_turn_and_preserves_raw_bytes() {
    const CHUNKS: usize = 200;
    const CHUNK_BYTES: usize = 1024;
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let bundle = TerminalActor::new(80, 24).expect("new actor");
            let handle = bundle.handle.clone();
            let token = bundle.token.clone();
            let mut actor = bundle.actor;
            let (pty_tx, _writer_rx) = actor.install_test_pty_channels();
            let mut raw_rx = handle.output.subscribe();
            for _ in 0..CHUNKS {
                pty_tx
                    .try_send(PtyEvent::Bytes {
                        chunk: Bytes::from(vec![b'x'; CHUNK_BYTES]),
                        read_at: std::time::Instant::now(),
                    })
                    .expect("queue sustained PTY output");
            }
            let (reply, replied) = oneshot::channel();
            handle
                .terminal()
                .expect("terminal facet")
                .native_bootstrap
                .send(NativeBootstrapRequest {
                    owner: 7,
                    terminal_id: phux_protocol::ids::ResourceId::local(1),
                    stream_id: phux_protocol::ids::StreamId::new(1).expect("stream id"),
                    bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("bootstrap id"),
                    limits: phux_protocol::caps::BootstrapLimits::new(
                        phux_protocol::MAX_BOOTSTRAP_CHUNK_BYTES,
                        phux_protocol::DEFAULT_HISTORY_PAGE_BYTES,
                    )
                    .expect("wide negotiated bootstrap bound"),
                    max_bytes: crate::native_state::MAX_NATIVE_PREFIX_BYTES,
                    max_frames: crate::native_state::MAX_NATIVE_BOOTSTRAP_FRAMES,
                    reply,
                })
                .await
                .expect("send native request");
            let run = tokio::task::spawn_local(actor.run());
            let capture = tokio::time::timeout(ACTOR_EXIT_DEADLINE, replied)
                .await
                .expect("native request starved behind PTY")
                .expect("native reply dropped")
                .expect("native capture");
            assert_eq!(
                capture.base_seq, 1,
                "native request must run after one bounded ready PTY turn"
            );
            let retained_capacity = capture
                .frames
                .into_iter()
                .try_fold(0_usize, |total, frame| {
                    let capacity = match frame {
                        FrameKind::BootstrapChunk { payload, .. } => payload
                            .try_into_mut()
                            .expect("actor owns compact chunk allocation")
                            .capacity(),
                        FrameKind::BootstrapReady {
                            history_cursor: Some(cursor),
                            ..
                        } => cursor
                            .try_into_mut()
                            .expect("actor owns compact cursor allocation")
                            .capacity(),
                        _ => 0,
                    };
                    total.checked_add(capacity)
                })
                .expect("retained capacity sum");
            assert_eq!(capture.retained_bytes, retained_capacity);

            let mut expected_seq = 1_u64;
            let mut raw_bytes = 0_usize;
            while raw_bytes < CHUNKS * CHUNK_BYTES {
                let output = tokio::time::timeout(ACTOR_EXIT_DEADLINE, raw_rx.recv())
                    .await
                    .expect("raw output stalled")
                    .expect("raw output channel closed");
                if let PaneOutput::Live { seq, bytes, .. } = output {
                    assert_eq!(seq, expected_seq);
                    expected_seq += 1;
                    raw_bytes += bytes.len();
                }
            }
            assert_eq!(raw_bytes, CHUNKS * CHUNK_BYTES);
            token.cancel();
            run.await.expect("actor run");
        })
        .await;
}
