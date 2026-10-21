async fn client(
    reader: impl FrameReader + 'static,
    token: CancellationToken,
    root_token: CancellationToken,
) -> Recorder {
    let recorder = Recorder::default();
    Box::pin(handle_client(
        reader,
        recorder.clone(),
        SharedState::new(),
        ClientId(9),
        token,
        root_token,
        None,
        TransportType::UnixSocket,
        false,
    ))
    .await
    .expect("clean close");
    recorder
}

/// However a connection ends, its token fires, so no per-connection task
/// outlives it.
#[tokio::test(flavor = "current_thread")]
async fn eof_cancels_the_connection_token() {
    LocalSet::new()
        .run_until(async {
            let token = CancellationToken::new();
            let mut eof = Script::new([]);
            eof.eof = true;
            client(eof, token.clone(), CancellationToken::new()).await;
            assert!(token.is_cancelled());
        })
        .await;
}

/// Run a negotiated connection over `frames` (HELLO first, EOF last)
/// and decode everything the server wrote, plus whether it closed.
async fn negotiated(frames: impl IntoIterator<Item = FrameKind>) -> (Vec<FrameKind>, bool) {
    let state = SharedState::new();
    let client_id = state.with_mut(|server| {
        let client_id = server.new_client_id();
        server.set_peer_identity(
            client_id,
            phux_protocol::policy::PeerIdentity {
                uid: nix::unistd::geteuid().as_raw(),
                pid: None,
                exe_path: None,
                mcp_host_key: None,
                transport: TransportType::UnixSocket,
                source_addr: None,
            },
        );
        client_id
    });
    let hello = FrameKind::Hello {
        client_name: "dispatch-characterization".to_owned(),
        protocol_major: phux_protocol::PROTOCOL_VERSION.major,
        protocol_minor: phux_protocol::PROTOCOL_VERSION.minor,
        protocol_patch: phux_protocol::PROTOCOL_VERSION.patch,
        client_caps: phux_protocol::caps::ClientCapabilities::new(),
    };
    let mut script = Script::new(std::iter::once(hello).chain(frames));
    script.eof = true;
    let recorder = Recorder::default();
    LocalSet::new()
        .run_until(Box::pin(handle_client(
            script,
            recorder.clone(),
            state,
            client_id,
            CancellationToken::new(),
            CancellationToken::new(),
            None,
            TransportType::UnixSocket,
            false,
        )))
        .await
        .expect("clean close");
    let decoded = recorder
        .slices()
        .iter()
        .map(|slice| FrameKind::decode(slice).expect("frame").0)
        .collect();
    let closed = recorder.events().last() == Some(&WriterEvent::Close);
    (decoded, closed)
}

/// A negotiated PING is answered and the connection stays up until EOF.
#[tokio::test(flavor = "current_thread")]
async fn a_negotiated_ping_is_answered_and_keeps_the_connection() {
    let (frames, closed) = negotiated([FrameKind::Ping { nonce: 7 }]).await;
    assert!(
        matches!(frames.first(), Some(FrameKind::HelloOk { .. })),
        "{frames:?}"
    );
    assert!(
        frames
            .iter()
            .any(|frame| matches!(frame, FrameKind::Pong { nonce: 7 })),
        "{frames:?}"
    );
    assert!(
        !frames
            .iter()
            .any(|frame| matches!(frame, FrameKind::Error { .. }))
    );
    assert!(!closed, "EOF leaves nobody to close for");
}

/// A server-to-client kind from a client is direction-invalid: ERROR,
/// DETACHED, close.
#[tokio::test(flavor = "current_thread")]
async fn a_server_frame_from_a_client_closes_the_connection() {
    let (frames, closed) = negotiated([FrameKind::Pong { nonce: 1 }]).await;
    assert!(
        frames.iter().any(|frame| matches!(
            frame,
            FrameKind::Error { code: ErrorCode::InvalidCommand, message, .. }
                if message.contains("negotiated phase")
        )),
        "{frames:?}"
    );
    assert!(matches!(
        frames.last(),
        Some(FrameKind::Detached {
            reason: Some(DetachReason::ProtocolError),
            ..
        })
    ));
    assert!(closed);
}

/// ATTACH id zero is reserved: the connection closes as malformed.
#[tokio::test(flavor = "current_thread")]
async fn a_zero_attach_id_closes_the_connection() {
    let (frames, closed) = negotiated([FrameKind::Attach {
        attach_id: 0,
        target: phux_protocol::wire::frame::AttachTarget::ByName("none".to_owned()),
        viewport: phux_protocol::wire::frame::ViewportInfo::new(80, 24),
        request_scrollback: false,
        scrollback_limit_lines: 0,
        role_policy: None,
    }])
    .await;
    assert!(
        frames.iter().any(|frame| matches!(
            frame,
            FrameKind::Error { code: ErrorCode::MalformedMessage, message, .. }
                if message.contains("attach_id must be nonzero")
        )),
        "{frames:?}"
    );
    assert!(closed);
}

/// A reused ATTACH id is refused, but the connection stays up.
#[tokio::test(flavor = "current_thread")]
async fn a_reused_attach_id_is_refused_without_closing() {
    let attach = || FrameKind::Attach {
        attach_id: 3,
        target: phux_protocol::wire::frame::AttachTarget::ByName("none".to_owned()),
        viewport: phux_protocol::wire::frame::ViewportInfo::new(80, 24),
        request_scrollback: false,
        scrollback_limit_lines: 0,
        role_policy: None,
    };
    let (frames, closed) = Box::pin(negotiated([
        attach(),
        attach(),
        FrameKind::Ping { nonce: 5 },
    ]))
    .await;
    assert!(
        frames.iter().any(|frame| matches!(
            frame,
            FrameKind::Error { code: ErrorCode::MalformedMessage, message, .. }
                if message.contains("already used on this connection")
        )),
        "{frames:?}"
    );
    assert!(
        frames
            .iter()
            .any(|frame| matches!(frame, FrameKind::Pong { nonce: 5 })),
        "a later frame is still served: {frames:?}"
    );
    assert!(!closed);
}

struct PingBeforeHelloReader {
    remaining: u8,
}

impl FrameReader for PingBeforeHelloReader {
    async fn read_frame(&mut self) -> io::Result<Option<BytesMut>> {
        if self.remaining == 0 {
            return std::future::pending().await;
        }
        tokio::time::sleep(crate::transport::HANDSHAKE_DEADLINE / 4).await;
        self.remaining -= 1;
        let mut frame = BytesMut::new();
        FrameKind::Ping {
            nonce: u64::from(self.remaining),
        }
        .encode(&mut frame);
        Ok(Some(frame))
    }
}

/// Pre-HELLO PINGs get PONGs but cannot extend the absolute deadline,
/// which ends in ERROR, DETACHED, close.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn pre_hello_ping_cannot_reset_absolute_handshake_deadline() {
    LocalSet::new()
        .run_until(async {
            let task = tokio::task::spawn_local(client(
                PingBeforeHelloReader { remaining: 8 },
                CancellationToken::new(),
                CancellationToken::new(),
            ));
            tokio::time::advance(crate::transport::HANDSHAKE_DEADLINE).await;
            let events = task.await.expect("client task").events();
            assert!(events.contains(&WriterEvent::Frame));
            assert!(
                events.ends_with(&[
                    WriterEvent::Error,
                    WriterEvent::Detached(Some(DetachReason::ProtocolError)),
                    WriterEvent::Close,
                ]),
                "{events:?}",
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn root_cancellation_flushes_server_shutdown_before_transport_close() {
    LocalSet::new()
        .run_until(async {
            let root_token = CancellationToken::new();
            let task = tokio::task::spawn_local(client(
                Script::new([]),
                root_token.child_token(),
                root_token.clone(),
            ));
            tokio::task::yield_now().await;
            root_token.cancel();
            assert_eq!(
                task.await.expect("client task").events(),
                [
                    WriterEvent::Detached(Some(DetachReason::ServerShutdown)),
                    WriterEvent::Close
                ],
            );
        })
        .await;
}
