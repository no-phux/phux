use super::*;

/// A peer that connects and never speaks is timed out rather than
/// wedging the listener, which then serves the next client.
#[tokio::test(start_paused = true)]
async fn a_silent_peer_does_not_wedge_the_listener() {
    let (listener, addr, token_hex, _tokens) = token_listener().await;
    let _silent = TcpStream::connect(addr).await.unwrap();

    let Err(err) = listener.accept().await else {
        panic!("a peer that never speaks must be timed out");
    };
    let rejection = err
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<WsPeerRejection>())
        .expect("timeout is reported as a typed peer rejection");
    assert_eq!(rejection.stage, WsAcceptStage::Upgrade);

    // Paused time would let a healthy upgrade lose to the virtual
    // deadline while the kernel is briefly pending.
    tokio::time::resume();
    let (reads, _, _) =
        exchange(&listener, addr, bearer_request(addr, &token_hex), &[&FRAME]).await;
    assert_eq!(
        reads[0].as_ref().unwrap().as_ref().unwrap().as_ref(),
        &FRAME
    );
}

/// A message larger than any frame is refused as a framing violation
/// without the server buffering it whole first.
#[tokio::test]
async fn an_oversized_message_is_a_framing_violation() {
    let (listener, addr, token_hex, _tokens) = token_listener().await;
    let max = phux_protocol::wire::frame::MAX_FRAME_LEN as usize;
    let mut oversized = vec![0_u8; LENGTH_PREFIX + max + 1];
    oversized[..LENGTH_PREFIX].copy_from_slice(&3_u32.to_be_bytes());
    let server = async {
        let (mut reader, _writer, _peer) = listener.accept().await.unwrap();
        reader.read_frame().await
    };
    let client = async {
        let tcp = TcpStream::connect(addr).await.unwrap();
        let (mut ws, _) = tokio_tungstenite::client_async(bearer_request(addr, &token_hex), tcp)
            .await
            .expect("upgrade");
        // The server stops reading partway, so the send may break.
        let _ = ws.send(Message::Binary(oversized.into())).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let (read, ()) = tokio::join!(server, client);
    let err = read.expect_err("an oversized message");
    let violation = err
        .get_ref()
        .and_then(|source| source.downcast_ref::<framing::FramingError>())
        .expect("a typed framing violation");
    assert!(
        matches!(violation, framing::FramingError::LengthOutOfRange { .. }),
        "{violation:?}"
    );
}

/// A peer that connects and never speaks holds one admission slot, not
/// the listener: a healthy peer behind it upgrades at once instead of
/// after the silent one's deadline.
#[tokio::test]
async fn a_silent_peer_does_not_delay_other_admissions() {
    let (listener, addr, token_hex, _tokens) = token_listener().await;
    let _silent = TcpStream::connect(addr).await.unwrap();
    let (reads, _, _) = tokio::time::timeout(
        Duration::from_secs(3),
        exchange(&listener, addr, bearer_request(addr, &token_hex), &[&FRAME]),
    )
    .await
    .expect("the healthy peer is admitted without waiting on the silent one");
    assert_eq!(
        reads[0].as_ref().unwrap().as_ref().unwrap().as_ref(),
        &FRAME
    );
}

#[test]
fn websocket_accept_errors_are_typed_and_classified_without_display_parsing() {
    use tokio_tungstenite::tungstenite::http;
    let source_ip = "192.0.2.41".parse().unwrap();
    let tls_error = ws_accept_error(WsAcceptStage::TlsHandshake, source_ip);
    assert_eq!(
        tls_error.to_string(),
        "WebSocket TLS handshake failed (source_ip=192.0.2.41)"
    );
    let typed = tls_error
        .get_ref()
        .and_then(|source| source.downcast_ref::<WsPeerRejection>())
        .expect("listener errors retain the safe concrete type");
    assert_eq!(
        (typed.stage, typed.source_ip),
        (WsAcceptStage::TlsHandshake, source_ip)
    );

    let auth_error = WebSocketError::Http(Box::new(
        http::Response::builder()
            .status(http::StatusCode::UNAUTHORIZED)
            .body(None::<Vec<u8>>)
            .unwrap(),
    ));
    assert_eq!(
        classify_ws_upgrade_stage(&auth_error),
        WsAcceptStage::PairingAuthentication
    );

    let unsafe_underlying = WebSocketError::Protocol(
        tokio_tungstenite::tungstenite::error::ProtocolError::InvalidHeader(Box::new(
            "authorization".parse().unwrap(),
        )),
    );
    assert_eq!(
        classify_ws_upgrade_error(&unsafe_underlying, source_ip).to_string(),
        "WebSocket upgrade failed (source_ip=192.0.2.41)"
    );
}

#[test]
fn peer_rejection_warning_limiter_is_global_bounded_and_deterministic() {
    let start = Instant::now();
    let mut limiter = PeerRejectionWarnLimiter::new();
    let interval = WS_REJECTION_WARN_INTERVAL;
    for (at, want) in [
        (start, PeerRejectionWarnDecision::Emit { suppressed: 0 }),
        (
            start + interval.saturating_sub(Duration::from_nanos(1)),
            PeerRejectionWarnDecision::Suppress,
        ),
        (
            start + interval,
            PeerRejectionWarnDecision::Emit { suppressed: 1 },
        ),
        (start + interval, PeerRejectionWarnDecision::Suppress),
        (
            start + interval * 2,
            PeerRejectionWarnDecision::Emit { suppressed: 1 },
        ),
    ] {
        assert_eq!(limiter.observe(at), want);
    }
    limiter.suppressed = u64::MAX;
    assert_eq!(
        limiter.observe(start + interval * 2),
        PeerRejectionWarnDecision::Suppress
    );
    assert_eq!(limiter.suppressed, u64::MAX);
}
