use super::*;
mod native;
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
            selected_profile: Some(BootstrapProfile::SynthesizedVtRaw),
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
fn synchronized_output_watchdog_publishes_and_wakes_an_idle_driver() {
    struct FrameWake {
        publication: Arc<phux_client_runtime::publication::Publication>,
        terminal: ResourceId,
        after: u64,
        seen: std::sync::atomic::AtomicBool,
    }
    impl Listener for FrameWake {
        fn on_activity(&self) {
            if self
                .publication
                .generation(&self.terminal)
                .is_some_and(|g| g > self.after)
            {
                self.seen.store(true, Ordering::SeqCst);
            }
        }
    }
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("sync.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let client = Runtime::connect(Target::uds(&path), options()).unwrap();
        let (mut socket, _) = listener.accept().await.unwrap();
        let terminal = ResourceId::local(1);
        bootstrap(&mut socket, &terminal).await;
        clear_prefetch(&mut socket, &terminal).await;
        wait_for_status(&client, Status::Attached).await;
        let committed = client.acquire(&terminal).unwrap();
        let wakes = Arc::new(FrameWake {
            publication: Arc::clone(client.publication()),
            terminal: terminal.clone(),
            after: committed.generation,
            seen: std::sync::atomic::AtomicBool::new(false),
        });
        client.set_listener(wakes.clone());
        let _ = client.take_events();
        send_frame(
            &mut socket,
            &FrameKind::ResourceOutput {
                terminal_id: terminal.clone(),
                stream_id: StreamId::new(1).unwrap(),
                bootstrap_id: BootstrapId::new(1).unwrap(),
                seq: 1,
                bytes: b"\x1b[?2026h\x1b[2J\x1b[Hwaiting".to_vec().into(),
            },
        )
        .await;
        wait_for_event(&client, "held output", |event| {
            matches!(event, Event::TerminalChanged { .. }).then_some(())
        })
        .await;
        assert!(Arc::ptr_eq(&committed, &client.acquire(&terminal).unwrap()));
        // No input, local projection command, end marker, or server traffic:
        // only the driver's deadline can release and announce this frame.
        tokio::time::timeout(
            Duration::from_secs(3),
            wait_until("watchdog wake", || wakes.seen.load(Ordering::SeqCst)),
        )
        .await
        .expect("idle watchdog must wake the listener");
        assert_eq!(client.acquire(&terminal).unwrap().text(), "waiting");
        assert!(client.take_events().iter().any(|event| {
            matches!(event, Event::TerminalChanged { terminal_id } if terminal_id == &terminal)
        }));
        client.close();
    });
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
