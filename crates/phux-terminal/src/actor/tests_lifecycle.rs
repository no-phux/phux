//! Actor lifecycle tests: construction, seeded colors, ask markers,
//! PTY spawn/adopt, signals, pane kill, input/output interleaving,
//! native-engine requests, pwd, and cancellation.

use super::test_support::*;
use super::*;

#[path = "tests_fixture_groups.rs"]
mod fixture_groups;
use fixture_groups::{FixtureGroup, FixturePane};

#[allow(
    clippy::future_not_send,
    reason = "the actor fixture runs on a LocalSet with the !Send canonical terminal"
)]
async fn snapshot_under_pty_pressure(refill_chunks: usize) {
    let bundle = TerminalActor::new(80, 24).expect("actor");
    let mut actor = bundle.actor;
    let (pty_tx, _writer) = actor.install_test_pty_channels();
    let mut output = bundle.handle.output.subscribe();
    for index in 0..200 {
        pty_tx
            .try_send(pressure_chunk(index))
            .expect("queued output");
    }
    let refill_tx = pty_tx.clone();
    let producer = tokio::task::spawn_local(async move {
        for index in 200..200 + refill_chunks {
            refill_tx
                .send(pressure_chunk(index))
                .await
                .expect("refill output");
        }
    });
    let (reply, replied) = oneshot::channel();
    bundle
        .handle
        .terminal()
        .expect("terminal")
        .snapshot
        .try_send(SnapshotRequest {
            image_protocols: phux_protocol::caps::ImageProtocolSet::all(),
            scrollback: None,
            max_bytes: usize::MAX,
            max_frames: usize::MAX,
            chunk_bytes: 1,
            reply,
        })
        .expect("snapshot request");
    let run = tokio::task::spawn_local(actor.run());
    let (_, base_seq) = tokio::time::timeout(ACTOR_EXIT_DEADLINE, replied)
        .await
        .expect("bounded reply")
        .expect("reply")
        .expect("snapshot");
    assert_eq!(
        base_seq, 1,
        "snapshot must precede the second ingress burst"
    );
    let mut bytes = Vec::new();
    let total_bytes = (200 + refill_chunks) * 1024;
    let mut expected_seq = 1;
    while bytes.len() < total_bytes {
        let next = tokio::time::timeout(ACTOR_EXIT_DEADLINE, output.recv())
            .await
            .expect("output deadline")
            .expect("output");
        if let PaneOutput::Live {
            seq, bytes: chunk, ..
        } = next
        {
            assert_eq!(seq, expected_seq, "no output sequence gaps");
            expected_seq += 1;
            bytes.extend_from_slice(&chunk);
        }
    }
    let expected: Vec<_> = (0..200 + refill_chunks).flat_map(pressure_bytes).collect();
    assert_eq!(
        bytes, expected,
        "service preserves ordered, distinguishable PTY chunks"
    );
    producer.await.expect("producer finished");
    bundle.token.cancel();
    run.await.expect("actor stopped");
}

fn pressure_chunk(index: usize) -> PtyEvent {
    PtyEvent::Bytes {
        chunk: Bytes::from(pressure_bytes(index)),
        read_at: std::time::Instant::now(),
    }
}

fn pressure_bytes(index: usize) -> Vec<u8> {
    let marker = format!("chunk-{index:08x} ");
    let mut bytes = vec![b'x'; 1024];
    bytes[..marker.len()].copy_from_slice(marker.as_bytes());
    bytes
}

/// Test-only hangup ceiling for the flush fixtures (a deadline, so idle
/// traps still return on the first poll).
const CONTENDED_FLUSH_GRACE: std::time::Duration = std::time::Duration::from_millis(2500);

/// More than the reader channel plus kernel buffer can absorb, derived from
/// the production constants (`PTY_READ_BATCH` bytes per queued slot).
const TERMINAL_FLUSH_BYTES: usize =
    super::spawn::PTY_CHANNEL_DEPTH * super::spawn::PTY_READ_BATCH + 512 * 1024;

/// Poll until `path` exists or 30 s pass (shell scheduling, not the flush).
async fn wait_until_fixture_exists(path: &std::path::Path) -> bool {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while !path.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .is_ok()
}

/// Poll the SIGHUP flush marker until it lands or the grace expires.
async fn wait_for_flush_marker(path: &std::path::Path) -> String {
    let started = tokio::time::Instant::now();
    loop {
        let body = std::fs::read_to_string(path).unwrap_or_default();
        if body.contains("flushed")
            || started.elapsed() >= CONTENDED_FLUSH_GRACE + std::time::Duration::from_millis(500)
        {
            return body;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
fn apply_native_wire_frame(
    kernel: &mut phux_client_core::session::SessionKernel<
        phux_client_core::engine::ghostty::GhosttyAdapter,
    >,
    frame: &FrameKind,
    effects: &mut phux_client_core::session::EffectBuffer,
) {
    use phux_client_core::engine::CanonicalGeometry;
    use phux_client_core::session::KernelInput;

    let mut encoded = (&[][..]).into();
    frame.encode(&mut encoded);
    let (decoded, tail) = FrameKind::decode(&encoded).expect("public wire decode");
    assert!(tail.is_empty());
    let input = match &decoded {
        FrameKind::BootstrapBegin {
            terminal_id,
            stream_id,
            bootstrap_id,
            profile,
            cols,
            rows,
            base_seq,
        } => KernelInput::BootstrapBegin {
            terminal_id,
            stream_id: *stream_id,
            bootstrap_id: *bootstrap_id,
            profile: *profile,
            geometry: CanonicalGeometry::new(*cols, *rows).expect("geometry"),
            base_seq: *base_seq,
        },
        FrameKind::BootstrapChunk {
            terminal_id,
            stream_id,
            bootstrap_id,
            chunk_seq,
            payload,
        } => KernelInput::BootstrapChunk {
            terminal_id,
            stream_id: *stream_id,
            bootstrap_id: *bootstrap_id,
            chunk_seq: *chunk_seq,
            payload,
        },
        FrameKind::BootstrapReady {
            terminal_id,
            stream_id,
            bootstrap_id,
            history_cursor,
        } => KernelInput::BootstrapReady {
            terminal_id,
            stream_id: *stream_id,
            bootstrap_id: *bootstrap_id,
            history_cursor: history_cursor.as_deref(),
        },
        FrameKind::HistoryPage {
            terminal_id,
            stream_id,
            bootstrap_id,
            page_seq,
            cursor,
            next_cursor,
            payload,
            rows,
        } => KernelInput::HistoryPage {
            terminal_id,
            stream_id: *stream_id,
            bootstrap_id: *bootstrap_id,
            page_seq: *page_seq,
            rows: *rows,
            payload,
            cursor,
            next_cursor: next_cursor.as_deref(),
        },
        FrameKind::ResourceOutput {
            terminal_id,
            stream_id,
            bootstrap_id,
            seq,
            bytes,
        } => KernelInput::ResourceOutput {
            terminal_id,
            stream_id: *stream_id,
            bootstrap_id: *bootstrap_id,
            seq: *seq,
            payload: bytes,
        },
        other => panic!("unexpected native flow frame: {other:?}"),
    };
    kernel
        .update(input, effects)
        .expect("kernel accepts server wire frame");
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
fn take_history_request(
    effects: &mut phux_client_core::session::EffectBuffer,
) -> Option<(Vec<u8>, u32, u32)> {
    use phux_client_core::session::{KernelEffect, KernelSend};

    let request = effects
        .as_slice()
        .iter()
        .rev()
        .find_map(|effect| match effect {
            KernelEffect::Send(KernelSend::HistoryRequest {
                cursor,
                max_bytes,
                max_rows,
                ..
            }) => Some((cursor.clone(), *max_bytes, *max_rows)),
            _ => None,
        });
    effects.clear();
    request
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[allow(
    clippy::future_not_send,
    clippy::too_many_lines,
    reason = "the LocalSet acceptance keeps the non-Send native actor and kernel in one real wire-flow fixture"
)]
async fn run_native_server_kernel_flow(
    history_rows: usize,
    history_config: phux_client_core::history::HistoryCacheConfig,
    interleave_live: bool,
) -> (phux_client_core::history::HistoryStatus, usize, usize) {
    use phux_client_core::engine::ghostty::GhosttyAdapter;
    use phux_client_core::session::{EffectBuffer, SessionKernel};
    use phux_protocol::caps::{BootstrapProfile, EngineCodec, EngineFeatureSet};

    let token = CancellationToken::new();
    let bundle = TerminalActor::build_with_token(
        200,
        3,
        None,
        test_scrollback(u32::try_from(history_rows.max(16)).expect("test history rows")),
        token.clone(),
    )
    .expect("native actor");
    let handle = bundle.handle.clone();
    let mut actor = bundle.actor;
    for row in 0..history_rows {
        actor.vt_write_for_test(format!("server-history-{row:04}\r\n").as_bytes());
    }
    let (pty_tx, _writer_rx) = actor.install_test_pty_channels();
    let run = tokio::task::spawn_local(actor.run());

    let terminal_id = phux_protocol::ids::ResourceId::local(71);
    let stream_id = phux_protocol::ids::StreamId::new(72).expect("stream id");
    let bootstrap_id = phux_protocol::ids::BootstrapId::new(73).expect("bootstrap id");
    let limits = phux_protocol::caps::BootstrapLimits::default();
    let (reply, captured) = oneshot::channel();
    handle
        .terminal()
        .expect("terminal facet")
        .native_bootstrap
        .send(NativeBootstrapRequest {
            owner: 71,
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            limits,
            max_bytes: crate::native_state::MAX_NATIVE_PREFIX_BYTES,
            max_frames: crate::native_state::MAX_NATIVE_BOOTSTRAP_FRAMES,
            reply,
        })
        .await
        .expect("bootstrap request");
    let capture = captured
        .await
        .expect("bootstrap reply")
        .expect("bootstrap capture");
    let cursor = capture.publication_cursor;

    let mut kernel = SessionKernel::with_history_config(
        GhosttyAdapter::new(limits),
        BootstrapProfile::NativeState {
            codec: EngineCodec::LibghosttySnapshotV1,
            features: EngineFeatureSet::required_native(),
        },
        history_config,
    );
    let mut effects = EffectBuffer::new();
    for frame in &capture.frames {
        apply_native_wire_frame(&mut kernel, frame, &mut effects);
    }
    assert!(kernel.published(&terminal_id).is_some());

    let (publication_reply, published) = oneshot::channel();
    handle
        .terminal()
        .expect("terminal facet")
        .native_publication
        .send(NativePublicationRequest {
            owner: 71,
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            cursor,
            reply: publication_reply,
        })
        .await
        .expect("publication request");
    let mut live = published
        .await
        .expect("publication reply")
        .expect("publication")
        .live;

    let (outbound, _outbound_rx) = mpsc::channel(2);
    let mut pages = 0_usize;
    let mut authenticated_rows = 0_usize;
    let mut next_request = take_history_request(&mut effects);
    while let Some((wire_cursor, max_bytes, max_rows)) = next_request {
        let permit = outbound
            .clone()
            .reserve_owned()
            .await
            .expect("history permit");
        let (reply, response) = oneshot::channel();
        handle
            .terminal()
            .expect("terminal facet")
            .native_history
            .send(NativeHistoryRequest {
                permit,
                owner: 71,
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                cursor: wire_cursor.into(),
                max_bytes,
                max_rows,
                limits,
                reply,
            })
            .await
            .expect("history request");
        let frame = response
            .await
            .expect("history reply")
            .result
            .expect("history capture");
        let FrameKind::HistoryPage { rows, .. } = &frame else {
            panic!("cooperative request must produce one authenticated unit: {frame:?}");
        };
        pages += 1;
        authenticated_rows += *rows as usize;
        apply_native_wire_frame(&mut kernel, &frame, &mut effects);
        next_request = take_history_request(&mut effects);

        if interleave_live && pages == 1 {
            pty_tx
                .send(PtyEvent::Bytes {
                    chunk: Bytes::from_static(b"live-between-server-pages\r\n"),
                    read_at: std::time::Instant::now(),
                })
                .await
                .expect("live PTY bytes");
            let PaneOutput::Live { seq, bytes, .. } = live.recv().await.expect("live output")
            else {
                panic!("expected live output");
            };
            apply_native_wire_frame(
                &mut kernel,
                &FrameKind::ResourceOutput {
                    terminal_id: terminal_id.clone(),
                    stream_id,
                    bootstrap_id,
                    seq,
                    bytes,
                },
                &mut effects,
            );
        }
        assert!(pages < 10_000, "history flow must terminate");
    }

    let status = kernel
        .history_cache(&terminal_id)
        .expect("history cache")
        .status();
    token.cancel();
    run.await.expect("actor run");
    (status, pages, authenticated_rows)
}

/// Two native pumps from one client on one pane do not invalidate each
/// other.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
fn capture_native_for_pump(
    actor: &mut TerminalActor,
    owner: u64,
    stream_id: phux_protocol::ids::StreamId,
    bootstrap_id: phux_protocol::ids::BootstrapId,
) -> NativeBootstrapReply {
    let (reply, mut response) = oneshot::channel();
    actor.handle_native_bootstrap(NativeBootstrapRequest {
        owner,
        terminal_id: phux_protocol::ids::ResourceId::local(1),
        stream_id,
        bootstrap_id,
        limits: phux_protocol::caps::BootstrapLimits::default(),
        max_bytes: crate::native_state::MAX_NATIVE_PREFIX_BYTES,
        max_frames: crate::native_state::MAX_NATIVE_BOOTSTRAP_FRAMES,
        reply,
    });
    for _ in 0..64 {
        match response.try_recv() {
            Ok(result) => return result.expect("native capture"),
            Err(oneshot::error::TryRecvError::Empty) => actor.cooperative_native_step(),
            Err(oneshot::error::TryRecvError::Closed) => {
                panic!("native bootstrap reply dropped")
            }
        }
    }
    panic!("native capture did not finish")
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
fn activate_native_for_pump(
    actor: &mut TerminalActor,
    owner: u64,
    stream_id: phux_protocol::ids::StreamId,
    bootstrap_id: phux_protocol::ids::BootstrapId,
    cursor: crate::native_state::OpaqueHistoryCursor,
) -> Result<NativePublicationReply, crate::native_state::NativeStateError> {
    let (reply, mut response) = oneshot::channel();
    actor.handle_native_publication(NativePublicationRequest {
        owner,
        terminal_id: phux_protocol::ids::ResourceId::local(1),
        stream_id,
        bootstrap_id,
        cursor,
        reply,
    });
    response
        .try_recv()
        .expect("publication is answered on the same turn")
}

#[path = "tests_lifecycle_construction_1.rs"]
mod construction_1;

#[path = "tests_lifecycle_pty_1.rs"]
mod pty_1;

#[path = "tests_lifecycle_termination_1.rs"]
mod termination_1;

#[path = "tests_lifecycle_termination_2.rs"]
mod termination_2;

#[path = "tests_lifecycle_native_1.rs"]
mod native_1;

#[path = "tests_lifecycle_native_2.rs"]
mod native_2;

#[path = "tests_lifecycle_native_3.rs"]
mod native_3;

#[path = "tests_lifecycle_history_1.rs"]
mod history_1;

#[path = "tests_lifecycle_process_1.rs"]
mod process_1;

#[path = "tests_snapshot_images.rs"]
mod snapshot_images;
