use super::*;
use phux_client_runtime::engine::{EngineDocumentPoint, Scroll};
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, ServerCapabilities,
};
use phux_protocol::ids::{BootstrapId, ClientId, SessionId, StreamId, WindowId};
use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot};

async fn bootstrap(socket: &mut UnixStream, terminal: &ResourceId) {
    recv_until(socket, |_, frame| {
        matches!(frame, FrameKind::Hello { .. }).then_some(())
    })
    .await;
    send_frame(
        socket,
        &FrameKind::HelloOk {
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            server_caps: ServerCapabilities::new(),
            server_id: vec![1; 16],
            selected_profile: BootstrapProfile::SynthesizedVtRaw,
            bootstrap_limits: BootstrapLimits::default(),
        },
    )
    .await;
    let attach_id = recv_until(socket, |_, frame| match frame {
        FrameKind::Attach { attach_id, .. } => Some(attach_id),
        _ => None,
    })
    .await;
    let window = WindowId::new(1);
    send_frame(
        socket,
        &FrameKind::Attached {
            attach_id,
            initial_client_id: ClientId::new(1),
            snapshot: SessionSnapshot::new(SessionId::new(1), window, terminal.clone())
                .with_resources(vec![ResourceInfo::new(terminal.clone(), window, 20, 4)]),
        },
    )
    .await;
    let stream_id = StreamId::new(1).unwrap();
    let bootstrap_id = BootstrapId::new(1).unwrap();
    send_frame(
        socket,
        &FrameKind::BootstrapBegin {
            terminal_id: terminal.clone(),
            stream_id,
            bootstrap_id,
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 20,
            rows: 4,
            base_seq: 0,
        },
    )
    .await;
    send_frame(
        socket,
        &FrameKind::BootstrapChunk {
            terminal_id: terminal.clone(),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: b"zero\r\none\r\ntwo\r\nthree\r\nfour\r\nfive"
                .to_vec()
                .into(),
        },
    )
    .await;
    send_frame(
        socket,
        &FrameKind::BootstrapReady {
            terminal_id: terminal.clone(),
            stream_id,
            bootstrap_id,
            history_cursor: Some(b"older".to_vec().into()),
        },
    )
    .await;
    send_frame(socket, &FrameKind::AttachReady { attach_id }).await;
    // This subscription is emitted at attach release, after the bootstrap ACK.
    recv_until(socket, |_, frame| {
        matches!(frame, FrameKind::SubscribeEvents { .. }).then_some(())
    })
    .await;
}

async fn clear_prefetch(socket: &mut UnixStream, terminal: &ResourceId) {
    // Clear bootstrap's eager prefetch so the local action owns the next
    // request. ZeroLimit is retryable and keeps the history cursor valid.
    send_frame(
        socket,
        &FrameKind::HistoryRejected {
            terminal_id: terminal.clone(),
            stream_id: StreamId::new(1).unwrap(),
            bootstrap_id: BootstrapId::new(1).unwrap(),
            cursor: b"older".to_vec().into(),
            reason: phux_protocol::wire::frame::HistoryRejectionReason::ZeroLimit,
            required_bytes: 1,
            required_rows: 1,
        },
    )
    .await;
    send_frame(socket, &FrameKind::Ping { nonce: 42 }).await;
    recv_until(socket, |_, frame| {
        matches!(frame, FrameKind::Pong { nonce: 42 }).then_some(())
    })
    .await;
}

#[test]
fn view_scroll_and_pin_wake_an_idle_connected_driver_for_history() {
    run_local(async {
        for pin in [false, true] {
            let tmp = TempDir::new().unwrap();
            let path = tmp.path().join("idle.sock");
            let listener = UnixListener::bind(&path).unwrap();
            let client = Runtime::connect(Target::uds(&path), options()).unwrap();
            let (mut socket, _) = listener.accept().await.unwrap();
            let terminal = ResourceId::local(1);
            bootstrap(&mut socket, &terminal).await;
            clear_prefetch(&mut socket, &terminal).await;
            wait_for_status(&client, Status::Attached).await;
            let view = client.create_view(&terminal).unwrap();
            let anchor = client
                .engine()
                .unwrap()
                .track_view_anchor(
                    view,
                    EngineDocumentPoint {
                        space: 0,
                        column: 0,
                        row: 0,
                    },
                )
                .unwrap();
            // Let the driver enter its idle select with no transport work. No
            // pings, local drain, or input operation may provide a later wake.
            tokio::time::sleep(Duration::from_millis(100)).await;
            if pin {
                client.pin_viewport_view(view, anchor).unwrap();
            } else {
                client.scroll_view(view, Scroll::Top).unwrap();
            }
            let received = tokio::time::timeout(
                Duration::from_secs(2),
                recv_until(&mut socket, |_, frame| match frame {
                    FrameKind::HistoryRequest {
                        terminal_id,
                        cursor,
                        ..
                    } => Some((terminal_id, cursor)),
                    _ => None,
                }),
            )
            .await
            .unwrap_or_else(|error| {
                panic!("idle driver must send history immediately (pin={pin}): {error}")
            });
            assert_eq!(received.0, terminal);
            assert_eq!(received.1.as_ref(), b"older");
            client.close();
        }
    });
}
