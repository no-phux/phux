use super::*;

/// Each pane's snapshot request completes before the next is asked, so
/// the connection-wide byte ceiling can only shrink as retained state
/// grows.
#[tokio::test(flavor = "current_thread")]
async fn handle_attach_bounds_snapshot_sources_sequentially() {
    const N: usize = 4;
    let local = LocalSet::new();
    local
        .run_until(async {
            let state = SharedState::new();
            let (wid, first) = state.with_mut(|s| s.seed_session("multi"));
            let mut panes = vec![first];
            state.with_mut(|s| {
                for _ in 1..N {
                    panes.push(s.add_pane_to_session(wid).expect("new_pane"));
                }
            });
            // No actor answers consumer registration: raw per-pane pumps.
            let mut stubs: Vec<StubRx> = panes
                .iter()
                .map(|&pid| {
                    let mut rx = register_stub(&state, pid);
                    rx.consumer_attach.close();
                    rx
                })
                .collect();

            let (out_tx, mut out_rx) =
                tokio::sync::mpsc::channel::<Outbound>(crate::state::DEFAULT_CLIENT_MAILBOX);
            let client_id = state.with_mut(crate::state::ServerState::new_client_id);
            let attach_task = spawn_attach(
                &state,
                client_id,
                1,
                "multi",
                &out_tx,
                ClientCapabilities::default(),
                phux_protocol::caps::BootstrapProfile::SynthesizedVtRaw,
            );

            let mut previous_max_bytes = usize::MAX;
            for (i, stub) in stubs.iter_mut().enumerate() {
                let req = tokio::time::timeout(MAILBOX_DEADLINE, stub.snapshot.recv())
                    .await
                    .unwrap_or_else(|_| panic!("snapshot request {i} never arrived"))
                    .expect("snapshot channel closed");
                assert!(req.max_bytes <= previous_max_bytes);
                previous_max_bytes = req.max_bytes;
                let payload = crate::grid::SnapshotBytes {
                    cols: 80,
                    rows: 24,
                    bytes: format!("snap-{i}").into_bytes(),
                    scrollback: Vec::new(),
                };
                req.reply
                    .send(Ok((payload, u64::try_from(i).unwrap())))
                    .expect("attach still waiting for snapshot");
            }
            expect_frames(&mut out_rx, &["ATTACHED"]).await;
            let mut counts = std::collections::BTreeMap::new();
            for _ in 0..(N * 3) {
                *counts
                    .entry(frame_name(&next_frame(&mut out_rx).await))
                    .or_insert(0) += 1;
            }
            assert_eq!(
                counts,
                [("BEGIN", N), ("CHUNK", N), ("READY", N)]
                    .into_iter()
                    .collect()
            );
            expect_frames(&mut out_rx, &["ATTACH_READY"]).await;
            attach_task.await.expect("attach task panicked");
        })
        .await;
}

/// ATTACH registers a per-consumer state-sync lifecycle (gated until
/// `ATTACH_READY`), a replacement ATTACH retires the prior consumer
/// before emitting anything, and teardown detaches the consumer.
#[tokio::test(flavor = "current_thread")]
async fn attach_registers_and_detach_unregisters_consumer_lifecycle() {
    let local = LocalSet::new();
    local
        .run_until(async {
            let state = SharedState::new();
            let (_sid, pid) = state.with_mut(|s| s.seed_session("lifecycle"));
            let mut rx = register_stub(&state, pid);
            let (out_tx, mut out_rx) =
                tokio::sync::mpsc::channel::<Outbound>(crate::state::DEFAULT_CLIENT_MAILBOX);
            let client_id = state.with_mut(crate::state::ServerState::new_client_id);
            let caps = ClientCapabilities::default()
                .with_output_mode(phux_protocol::caps::OutputMode::StateSync);
            let profile = phux_protocol::caps::BootstrapProfile::SynthesizedVtStateSync;
            let bootstrap = ["BEGIN", "CHUNK", "READY", "ATTACH_READY"];

            let attach_task =
                spawn_attach(&state, client_id, 1, "lifecycle", &out_tx, caps, profile);
            let attach_req = tokio::time::timeout(MAILBOX_DEADLINE, rx.consumer_attach.recv())
                .await
                .expect("ConsumerAttachRequest never arrived")
                .expect("consumer_attach channel closed");
            assert_eq!(attach_req.client_id, wire_client(client_id));
            assert!(attach_req.wire_terminal_id >= 1);
            let live_gate = attach_req.live_gate.clone();
            assert!(!*live_gate.borrow(), "live output waits for ATTACH_READY");
            attach_req
                .reply
                .send(Ok(state_sync_outcome(b"snap")))
                .expect("send attach reply");
            expect_frames(&mut out_rx, &["ATTACHED"]).await;
            attach_task.await.expect("attach task panicked");
            expect_frames(&mut out_rx, &bootstrap).await;

            let second = spawn_attach(&state, client_id, 2, "lifecycle", &out_tx, caps, profile);
            let retired = tokio::time::timeout(MAILBOX_DEADLINE, rx.consumer_detach.recv())
                .await
                .expect("replacement did not retire the prior consumer")
                .expect("consumer detach channel closed");
            assert_eq!(retired.client_id, wire_client(client_id));
            assert!(
                out_rx.try_recv().is_err(),
                "replacement ATTACHED must wait until prior emission is retired",
            );
            retired.reply.send(()).expect("ack retirement");
            let replacement = tokio::time::timeout(MAILBOX_DEADLINE, rx.consumer_attach.recv())
                .await
                .expect("replacement ConsumerAttachRequest did not arrive")
                .expect("consumer attach channel closed");
            let replacement_gate = replacement.live_gate.clone();
            assert!(!*replacement_gate.borrow());
            replacement
                .reply
                .send(Ok(state_sync_outcome(b"replacement")))
                .expect("ack replacement consumer");
            assert!(matches!(
                next_frame(&mut out_rx).await,
                FrameKind::Attached { attach_id: 2, .. }
            ));
            expect_frames(&mut out_rx, &bootstrap).await;
            second.await.expect("replacement attach task panicked");
            assert!(*replacement_gate.borrow());
            assert!(
                *live_gate.borrow(),
                "aggregate completion releases live output"
            );

            detach_and_release_consumer_state(&state, client_id);
            let detach_req = tokio::time::timeout(MAILBOX_DEADLINE, rx.consumer_detach.recv())
                .await
                .expect("ConsumerDetachRequest never arrived")
                .expect("consumer_detach channel closed");
            assert_eq!(detach_req.client_id, wire_client(client_id));
            assert!(state.with(|s| !s.attached().contains_key(&client_id)));
        })
        .await;
}

/// A native replacement bootstrap that fails after a resync closes the
/// connection with `CODEC_UNAVAILABLE` and rolls back the consumer.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
#[allow(
    clippy::too_many_lines,
    reason = "one scripted actor conversation from attach through fatal resync"
)]
async fn native_resync_capture_failure_closes_connection_and_rolls_back_consumer() {
    use phux_protocol::caps::{
        BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, EngineCodec, EngineFeatureSet,
    };

    use crate::terminal_actor::{
        ConsumerAttachOutcome, NativeBootstrapReply, PaneOutput, ResyncReason,
    };

    let local = LocalSet::new();
    local
        .run_until(async {
            let state = SharedState::new();
            let (_sid, terminal) = state.with_mut(|s| s.seed_session("native-fatal"));
            let (handle, mut rx) = stub_terminal();
            let output_tx = handle.output.clone();
            state.with_mut(|s| {
                let _ = s.register_resource_handle(terminal, handle, CancellationToken::new());
            });

            let client_id = state.with_mut(crate::state::ServerState::new_client_id);
            let (out_tx, mut out_rx) =
                tokio::sync::mpsc::channel::<Outbound>(crate::state::DEFAULT_CLIENT_MAILBOX);
            let connection_token = CancellationToken::new();
            let task_token = connection_token.clone();
            let state_for_task = state.clone();
            let attach_task = tokio::task::spawn_local(async move {
                let root_token = CancellationToken::new();
                let mut output_pumps = JoinSet::new();
                handle_attach(
                    &state_for_task,
                    client_id,
                    1,
                    AttachTarget::ByName("native-fatal".to_owned()),
                    ViewportInfo::new(80, 24),
                    false,
                    0,
                    None,
                    &out_tx,
                    ClientCapabilities::default(),
                    BootstrapProfile::NativeState {
                        codec: EngineCodec::LibghosttySnapshotV1,
                        features: EngineFeatureSet::required_native(),
                    },
                    BootstrapLimits::default(),
                    &root_token,
                    &mut output_pumps,
                    &task_token,
                    false,
                )
                .await;
                task_token.cancelled().await;
                abort_output_pumps(&mut output_pumps, client_id, "fatal-native-resync").await;
            });

            let registration = tokio::time::timeout(MAILBOX_DEADLINE, rx.consumer_attach.recv())
                .await
                .expect("consumer registration timed out")
                .expect("consumer registration sender closed");
            registration
                .reply
                .send(Ok(ConsumerAttachOutcome {
                    tick_managed: false,
                    state_sync_bootstrap: None,
                }))
                .expect("consumer registration reply");

            let initial = tokio::time::timeout(MAILBOX_DEADLINE, rx.native_bootstrap.recv())
                .await
                .expect("initial native request timed out")
                .expect("native request sender closed");
            let terminal_id = initial.terminal_id.clone();
            let stream_id = initial.stream_id;
            let bootstrap_id = initial.bootstrap_id;
            initial
                .reply
                .send(Ok(NativeBootstrapReply {
                    frames: vec![
                        FrameKind::BootstrapBegin {
                            terminal_id: terminal_id.clone(),
                            stream_id,
                            bootstrap_id,
                            profile: BootstrapStreamProfile::NativeState {
                                codec: EngineCodec::LibghosttySnapshotV1,
                            },
                            cols: 80,
                            rows: 24,
                            base_seq: 0,
                        },
                        FrameKind::BootstrapChunk {
                            terminal_id: terminal_id.clone(),
                            stream_id,
                            bootstrap_id,
                            chunk_seq: 0,
                            payload: bytes::Bytes::from_static(b"opaque-checkpoint"),
                        },
                        FrameKind::BootstrapReady {
                            terminal_id,
                            stream_id,
                            bootstrap_id,
                            history_cursor: None,
                        },
                    ],
                    retained_bytes: b"opaque-checkpoint".len(),
                    base_seq: 0,
                    publication_cursor: [9; 32],
                }))
                .expect("initial native reply");
            let publication = tokio::time::timeout(MAILBOX_DEADLINE, rx.native_publication.recv())
                .await
                .expect("initial native publication timed out")
                .expect("native publication sender closed");
            assert_eq!(publication.cursor, [9; 32]);
            publication
                .reply
                .send(Ok(crate::terminal_actor::NativePublicationReply {
                    replay: Vec::new(),
                    live: output_tx.subscribe(),
                }))
                .expect("initial native publication reply");
            expect_frames(
                &mut out_rx,
                &["ATTACHED", "BEGIN", "CHUNK", "READY", "ATTACH_READY"],
            )
            .await;

            output_tx
                .send(PaneOutput::Live {
                    seq: 1,
                    bytes: bytes::Bytes::from_static(b"live"),
                    at: std::time::Instant::now(),
                })
                .expect("live receiver");
            assert!(matches!(
                next_frame(&mut out_rx).await,
                FrameKind::ResourceOutput { seq: 1, .. }
            ));

            output_tx
                .send(PaneOutput::Resync {
                    cols: 80,
                    rows: 24,
                    bytes: bytes::Bytes::new(),
                    reason: ResyncReason::OutboundGap,
                    audience: crate::terminal_actor::ResyncAudience::Everyone,
                    base_seq: 1,
                })
                .expect("resync receiver");
            assert!(matches!(
                next_frame(&mut out_rx).await,
                FrameKind::BootstrapTombstone {
                    last_valid_seq: 1,
                    ..
                }
            ));
            let failed = tokio::time::timeout(MAILBOX_DEADLINE, rx.native_bootstrap.recv())
                .await
                .expect("replacement native request timed out")
                .expect("native request sender closed");
            failed
                .reply
                .send(Err(crate::native_state::NativeStateError::OutOfMemory))
                .expect("replacement failure reply");

            assert!(matches!(
                next_frame(&mut out_rx).await,
                FrameKind::Error {
                    code: ErrorCode::CodecUnavailable,
                    ..
                }
            ));
            tokio::time::timeout(MAILBOX_DEADLINE, connection_token.cancelled())
                .await
                .expect("connection was not cancelled");
            tokio::time::timeout(MAILBOX_DEADLINE, rx.consumer_detach.recv())
                .await
                .expect("consumer rollback timed out")
                .expect("consumer detach sender closed");
            assert!(
                state.with(|s| !s.attached().contains_key(&client_id)),
                "fatal native replacement left the aggregate consumer attached"
            );
            attach_task.await.expect("attach task");
        })
        .await;
}
