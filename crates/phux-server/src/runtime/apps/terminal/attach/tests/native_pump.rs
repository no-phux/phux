//! Native pump final drains and final capture failures.
#![allow(
    clippy::wildcard_imports,
    reason = "the attach test topics share the parent test namespace"
)]

use super::*;
use phux_terminal::actor::TerminalResource as _;

pub(super) fn native_profile() -> BootstrapProfile {
    BootstrapProfile::NativeState {
        codec: phux_protocol::caps::EngineCodec::LibghosttySnapshotV1,
        features: phux_protocol::caps::EngineFeatureSet::required_native(),
    }
}

/// Start a native-profile ATTACH pump on `terminal` whose published
/// bootstrap covers nothing yet.
pub(super) fn spawn_native_pump(
    terminal: phux_terminal::actor::TerminalHandle,
    output: &tokio::sync::broadcast::Sender<ResourceOutput>,
    resize: ResizeSender,
) -> TwoPumpConsumer {
    let (out_tx, out_rx) = tokio::sync::mpsc::channel(32);
    let ctx = native_pump_context(out_tx, terminal, resize);
    let (gate_tx, gate_rx) = oneshot::channel();
    gate_tx
        .send(OutputPumpStart {
            superseded: false,
            published_cut: 0,
            replay: Vec::new(),
            live: None,
            interest: None,
        })
        .unwrap_or_else(|_| panic!("gate receiver alive"));
    let live = output.subscribe();
    let task = tokio::task::spawn_local(async move { run_output_pump(&ctx, gate_rx, live).await });
    TwoPumpConsumer { out_rx, task }
}

fn native_pump_context(
    out_tx: tokio::sync::mpsc::Sender<Outbound>,
    terminal: phux_terminal::actor::TerminalHandle,
    resize: ResizeSender,
) -> OutputPumpContext {
    OutputPumpContext {
        out_tx,
        resize,
        wire_terminal_id: phux_protocol::ids::ResourceId::local(1),
        stream_id: two_pump_stream(),
        initial_bootstrap_id: two_pump_initial_generation(),
        client_id: ClientId(7),
        client_caps: ClientCapabilities::default(),
        profile: BootstrapStreamProfile::NativeState {
            codec: phux_protocol::caps::EngineCodec::LibghosttySnapshotV1,
        },
        limits: BootstrapLimits::default(),
        lag_label: "native test pump",
        stale_skip: false,
        cancel: None,
        drain: None,
        snapshot: None,
        last_seq: None,
        terminal,
    }
}

#[tokio::test(start_paused = true)]
async fn final_drain_wins_over_expired_gap_and_ready_broadcasts() {
    let (handle, _, _, _) = native_attach_handle();
    let terminal = handle.terminal().unwrap().clone();
    let (out_tx, _) = tokio::sync::mpsc::channel(8);
    let mut ctx = native_pump_context(out_tx, terminal.clone(), terminal.resize.clone());
    let drain = CancellationToken::new();
    ctx.drain = Some(drain.clone());
    let mut generation = PumpGeneration::opened_at(0, two_pump_initial_generation());
    generation.fence_for_gap();
    generation.note_resync_requested();
    tokio::time::advance(std::time::Duration::from_secs(30)).await;
    let mut output = handle.output.subscribe();
    handle
        .output
        .send(ResourceOutput::Live {
            seq: 1,
            bytes: bytes::Bytes::from_static(b"old"),
            at: std::time::Instant::now(),
        })
        .unwrap();
    drain.cancel();
    assert!(matches!(
        next_wait(&ctx, &generation, &mut output, false).await,
        Some(pump::PumpWait::FinalDrain)
    ));
}

#[tokio::test(start_paused = true)]
async fn invalidated_final_native_capture_is_a_failure_without_partial_publication() {
    let (diagnostics, _guard) = super::publication_diagnostics::capture();
    let (handle, _, mut captures, _) = native_attach_handle();
    let terminal = handle.terminal().unwrap().clone();
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel(8);
    let ctx = native_pump_context(out_tx, terminal.clone(), terminal.resize.clone());
    let mut output = handle.output.subscribe();
    let mut generation = PumpGeneration::opened_at(0, two_pump_initial_generation());
    let final_capture = ctx.publish_final_generation(&mut generation, &mut output);
    tokio::pin!(final_capture);
    let request = tokio::select! {
        biased;
        result = &mut final_capture => panic!("capture finished without an actor reply: {result:?}"),
        request = captures.recv() => request.unwrap(),
    };
    request
        .reply
        .send(Err(
            phux_terminal_api::native_state::NativeStateError::Resize,
        ))
        .unwrap();
    assert!(matches!(
        final_capture.await,
        Err(PumpFault::PublicationNotActivated)
    ));
    assert!(
        out_rx.try_recv().is_err(),
        "failed capture published a partial generation"
    );
    super::publication_diagnostics::assert_event(&diagnostics, "final_native_capture", "refused");
}

#[tokio::test(start_paused = true)]
async fn unanswered_final_native_capture_has_a_deadline() {
    let (diagnostics, _guard) = super::publication_diagnostics::capture();
    let (handle, _, mut captures, _) = native_attach_handle();
    let terminal = handle.terminal().unwrap().clone();
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel(8);
    let ctx = native_pump_context(out_tx, terminal.clone(), terminal.resize.clone());
    let mut output = handle.output.subscribe();
    let mut generation = PumpGeneration::opened_at(0, two_pump_initial_generation());
    let capture = ctx.publish_final_generation(&mut generation, &mut output);
    tokio::pin!(capture);
    let held_reply = tokio::select! {
        biased;
        result = &mut capture => panic!("capture finished without an actor reply: {result:?}"),
        request = captures.recv() => request.unwrap(),
    };
    tokio::time::advance(std::time::Duration::from_secs(1)).await;
    assert!(matches!(
        capture.await,
        Err(PumpFault::PublicationNotActivated)
    ));
    assert!(held_reply.reply.is_closed());
    assert!(out_rx.try_recv().is_err());
    super::publication_diagnostics::assert_event(&diagnostics, "final_native_capture", "deadline");
}

#[tokio::test(start_paused = true)]
async fn unanswered_final_synthesized_capture_keeps_its_one_second_deadline() {
    let (diagnostics, _guard) = super::publication_diagnostics::capture();
    let (handle, _, _, _) = native_attach_handle();
    let terminal = handle.terminal().unwrap().clone();
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel(8);
    let mut ctx = native_pump_context(out_tx, terminal.clone(), terminal.resize.clone());
    let (snapshot, mut captures) = tokio::sync::mpsc::channel(1);
    ctx.profile = BootstrapStreamProfile::SynthesizedVtRaw;
    ctx.snapshot = Some(snapshot);
    let mut output = handle.output.subscribe();
    let mut generation = PumpGeneration::opened_at(0, two_pump_initial_generation());
    let capture = ctx.publish_final_generation(&mut generation, &mut output);
    tokio::pin!(capture);
    let held_reply = tokio::select! {
        result = &mut capture => panic!("capture finished before its deadline: {result:?}"),
        request = captures.recv() => request.unwrap(),
    };
    tokio::time::advance(std::time::Duration::from_secs(1)).await;
    assert!(matches!(
        capture.await,
        Err(PumpFault::PublicationNotActivated)
    ));
    assert!(held_reply.reply.is_closed());
    assert!(out_rx.try_recv().is_err());
    super::publication_diagnostics::assert_event(
        &diagnostics,
        "final_synthesized_capture",
        "deadline",
    );
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_superseded_final_publication_with_lost_resync_spends_the_existing_gap_budget() {
    crate::runtime::test_support::local(async {
        let (handle, _, mut captures, mut publications) = native_attach_handle();
        let terminal = handle.terminal().unwrap().clone();
        let (resize, mut requests) = ResizeSender::channel(8);
        let (out_tx, mut out_rx) = tokio::sync::mpsc::channel(32);
        let mut ctx = native_pump_context(out_tx, terminal, resize);
        let drain = CancellationToken::new();
        drain.cancel();
        ctx.drain = Some(drain);
        let output = handle.output.subscribe();
        let mut pump = tokio::task::spawn_local(async move {
            run_started_output_pump(
                &ctx,
                OutputPumpStart {
                    superseded: false,
                    published_cut: 0,
                    replay: Vec::new(),
                    live: None,
                    interest: None,
                },
                output,
            )
            .await
        });
        let capture = captures.recv().await.expect("final capture");
        let reply = empty_native_checkpoint(&capture);
        capture.reply.send(Ok(reply)).expect("capture reply");
        let publication = publications.recv().await.expect("final activation");
        publication
            .reply
            .send(Err(
                phux_terminal_api::native_state::NativeStateError::Resize,
            ))
            .expect("supersession reply");
        let mut attempts = 0;
        loop {
            tokio::select! {
                biased;
                result = &mut pump => {
                    assert!(matches!(result.expect("pump"), Some(PumpFault::GenerationLost)));
                    break;
                }
                request = requests.recv() => {
                    assert!(request.expect("gap request").resync_only);
                    attempts += 1;
                    assert!(attempts <= 5, "existing gap retry budget");
                }
            }
        }
        assert_eq!(attempts, 5);
        let mut errors = 0;
        while let Ok(frame) = out_rx.try_recv() {
            if matches!(frame, Outbound::Frame(FrameKind::Error { .. })) {
                errors += 1;
            }
        }
        assert_eq!(errors, 1, "bounded failure, not false final-drain success");
    })
    .await;
}

/// The actor's reply to `capture`: an empty checkpoint at sequence 0.
pub(super) fn empty_native_checkpoint(
    capture: &phux_terminal::actor::NativeBootstrapRequest,
) -> phux_terminal::actor::NativeBootstrapReply {
    phux_terminal::actor::NativeBootstrapReply {
        frames: vec![
            FrameKind::BootstrapBegin {
                terminal_id: capture.terminal_id.clone(),
                stream_id: capture.stream_id,
                bootstrap_id: capture.bootstrap_id,
                profile: BootstrapStreamProfile::NativeState {
                    codec: phux_protocol::caps::EngineCodec::LibghosttySnapshotV1,
                },
                cols: 80,
                rows: 24,
                base_seq: 0,
            },
            FrameKind::BootstrapReady {
                terminal_id: capture.terminal_id.clone(),
                stream_id: capture.stream_id,
                bootstrap_id: capture.bootstrap_id,
                history_cursor: None,
            },
        ],
        retained_bytes: 0,
        base_seq: 0,
        capture: phux_terminal_api::actor::NativeCapture::new(capture.owner, capture.terminal_id.clone(), capture.stream_id, capture.bootstrap_id, [9; 32]),
    }
}
