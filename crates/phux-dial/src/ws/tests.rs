//! WebSocket transport, address parsing and liveness regressions.

#![allow(clippy::expect_used, reason = "tests")]

    use super::*;

    #[test]
    fn parses_ws_and_wss_targets() {
        let ws = WsTarget::parse("ws://127.0.0.1:8787/path").expect("ws");
        assert_eq!(
            (ws.host.as_str(), ws.port, ws.secure),
            ("127.0.0.1", 8787, false)
        );
        assert!(ws.is_loopback());

        let wss = WsTarget::parse("wss://example.com/phux").expect("wss");
        assert_eq!(
            (wss.host.as_str(), wss.port, wss.secure),
            ("example.com", 443, true)
        );
        assert!(!wss.is_loopback());

        assert!(WsTarget::parse("https://example.com/").is_err());
    }

    #[test]
    fn parses_ipv6_loopback_targets() {
        for scheme in ["ws", "wss"] {
            let target = WsTarget::parse(&format!("{scheme}://[::1]:8787/phux"))
                .expect("IPv6 WebSocket target");
            assert_eq!(target.host, "[::1]");
            assert_eq!(target.server_name, "::1");
            assert_eq!(target.port, 8787);
            assert!(target.is_loopback());
        }
    }

    fn plain_dial(url: String) -> WsDial {
        WsDial {
            url,
            token: None,
            trust: CertTrust::SkipVerify,
            tls_server_name: None,
            identity: None,
        }
    }

    #[tokio::test]
    async fn refused_tcp_connect_classifies_unreachable() {
        // Bind then drop a listener so the port is known-refusing.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("local addr").port();
        drop(listener);

        let err = dial(&plain_dial(format!("ws://127.0.0.1:{port}")))
            .await
            .expect_err("nothing is listening");
        assert!(matches!(err, DialError::Unreachable(_)), "got {err:?}");
        assert!(
            err.to_string()
                .starts_with("transport connect error: dial 127.0.0.1:"),
            "got {err}"
        );
    }

    /// `.invalid` (RFC 2606) never resolves: the `MagicDNS`-down shape.
    #[tokio::test]
    async fn unresolvable_hostname_classifies_unreachable() {
        let err = dial(&plain_dial(
            "ws://phux-test-nxdomain.invalid:8787".to_owned(),
        ))
        .await
        .expect_err(".invalid never resolves");
        assert!(matches!(err, DialError::Unreachable(_)), "got {err:?}");
        assert!(
            err.to_string().contains("name resolution failed"),
            "got {err}"
        );
    }

    #[tokio::test]
    async fn explicit_no_identity_dials_pinned_ipv6_wss_with_bearer_auth() {
        const TOKEN: &str = "11111111111111111111111111111111";
        let dir = tempfile::tempdir().expect("tempdir");
        let tls = crate::testing::server_tls(dir.path(), None);
        let fingerprint =
            crate::cert::cert_fingerprint(&dir.path().join("cert.pem")).expect("certificate pin");
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));
        let listener = tokio::net::TcpListener::bind("[::1]:0")
            .await
            .expect("bind WSS fixture");
        let port = listener.local_addr().expect("fixture address").port();

        let server = async move {
            let (tcp, _) = listener.accept().await.expect("accept TCP");
            let tls = acceptor.accept(tcp).await.expect("accept TLS");
            #[allow(
                clippy::result_large_err,
                reason = "tokio-tungstenite fixes the HTTP rejection response type for its handshake callback"
            )]
            let mut ws = tokio_tungstenite::accept_hdr_async(
                tls,
                |request: &tokio_tungstenite::tungstenite::handshake::server::Request, response| {
                    assert_eq!(
                        request.headers().get("host").expect("IPv6 Host authority"),
                        format!("[::1]:{port}").as_str()
                    );
                    assert_eq!(
                        request
                            .headers()
                            .get("authorization")
                            .expect("bearer header")
                            .to_str()
                            .expect("ASCII bearer header"),
                        format!("Bearer {TOKEN}")
                    );
                    Ok(response)
                },
            )
            .await
            .expect("upgrade WebSocket");
            assert_eq!(
                ws.next()
                    .await
                    .expect("binary message")
                    .expect("read message"),
                Message::Binary(b"pinned-wss".to_vec().into())
            );
        };
        let client = async {
            let mut ws = dial_with_identity(
                &WsDial {
                    url: format!("wss://[::1]:{port}"),
                    token: Some(TOKEN.to_owned()),
                    trust: CertTrust::Pinned(fingerprint),
                    tls_server_name: None,
                    identity: None,
                },
                &TlsClientIdentity::None,
            )
            .await
            .expect("explicit pinned WSS dial");
            ws.send(Message::Binary(b"pinned-wss".to_vec().into()))
                .await
                .expect("send payload");
        };

        tokio::join!(server, client);
    }

    /// Compressed test timings: the production 10s/30s shape, 100x faster.
    const TEST_PING: Duration = Duration::from_millis(100);
    const TEST_DEAD: Duration = Duration::from_millis(300);

    /// Silence walks the state machine: quiet -> ping -> quiet while waiting
    /// for the answer -> dead once the whole window elapses.
    #[test]
    fn silence_pings_then_declares_the_peer_dead() {
        let start = Instant::now();
        let mut keepalive = WsKeepalive::with_timings(start, TEST_PING, TEST_DEAD);

        assert_eq!(keepalive.poll(start), WsLiveness::Idle(TEST_PING));
        assert_eq!(keepalive.poll(start + TEST_PING), WsLiveness::Ping);

        keepalive.note_ping(start + TEST_PING);
        assert_eq!(
            keepalive.poll(start + TEST_PING),
            WsLiveness::Idle(TEST_PING)
        );
        assert_eq!(keepalive.poll(start + TEST_PING * 2), WsLiveness::Ping);

        keepalive.note_ping(start + TEST_PING * 2);
        assert_eq!(keepalive.poll(start + TEST_DEAD), WsLiveness::Dead);
    }

    /// A ping late in the window must not schedule a nap past the death
    /// deadline, or a dead peer would outlive its own timeout.
    #[test]
    fn the_nap_never_overshoots_the_death_deadline() {
        let start = Instant::now();
        let mut keepalive = WsKeepalive::with_timings(start, TEST_PING, TEST_DEAD);
        let late = start + Duration::from_millis(250);
        keepalive.note_ping(late);
        assert_eq!(
            keepalive.poll(late),
            WsLiveness::Idle(Duration::from_millis(50))
        );
    }

    /// Any inbound message rearms the whole policy: the death deadline moves
    /// and the ping schedule restarts.
    #[test]
    fn inbound_traffic_rearms_after_a_ping() {
        let start = Instant::now();
        let mut keepalive = WsKeepalive::with_timings(start, TEST_PING, TEST_DEAD);
        keepalive.note_ping(start + TEST_PING);

        let pong = start + TEST_PING + Duration::from_millis(10);
        keepalive.note_inbound(pong);

        assert_eq!(keepalive.poll(pong), WsLiveness::Idle(TEST_PING));
        assert_eq!(
            keepalive.poll(pong + TEST_DEAD - Duration::from_millis(1)),
            WsLiveness::Ping
        );
        assert_eq!(keepalive.poll(pong + TEST_DEAD), WsLiveness::Dead);
    }

    /// Accept one WebSocket connection. A `responsive` peer keeps reading
    /// (so tungstenite answers pings); otherwise it holds the socket open
    /// and never polls it: the far end of a half-open TCP connection.
    async fn spawn_ws_server(responsive: bool) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.expect("accept");
            let mut ws = tokio_tungstenite::accept_async(tcp)
                .await
                .expect("ws upgrade");
            if responsive {
                while ws.next().await.is_some() {}
            } else {
                std::future::pending::<()>().await;
            }
        });
        format!("ws://127.0.0.1:{port}")
    }

    async fn connect_halves(url: &str, opened: Instant) -> (WsReader, WsWriter) {
        let ws = dial(&plain_dial(url.to_owned())).await.expect("dial");
        let (tx, rx) = ws.split();
        let keepalive = WsKeepalive::with_timings(opened, TEST_PING, TEST_DEAD);
        (WsReader { rx, keepalive }, WsWriter { tx })
    }

    /// A stalled peer is reported as `Stalled` within the liveness window
    /// instead of hanging, which is what puts a network-switched laptop on
    /// the reconnect path.
    ///
    /// The lower bound is measured from `opened`, the policy's own origin,
    /// not from after the handshake: timing from after the dial subtracted
    /// the handshake and made the bound load-dependent.
    #[tokio::test]
    async fn keepalive_reports_a_stalled_peer_instead_of_hanging() {
        let url = spawn_ws_server(false).await;
        let opened = Instant::now();
        let (mut reader, mut writer) = connect_halves(&url, opened).await;

        let err =
            tokio::time::timeout(TEST_DEAD * 10, recv_message_alive(&mut reader, &mut writer))
                .await
                .expect("the keepalive must bound the read")
                .expect_err("a silent peer is not a clean close");

        assert!(matches!(err, DialError::Stalled(_)), "got {err:?}");
        assert!(
            err.to_string().starts_with("transport stalled: "),
            "got {err}"
        );
        assert!(
            opened.elapsed() >= TEST_DEAD,
            "must not condemn a peer before its window elapses: {:?}",
            opened.elapsed()
        );
    }

    /// No false positives: a quiet peer that answers pings stays up across
    /// many liveness windows.
    #[tokio::test]
    async fn a_quiet_but_healthy_peer_is_never_declared_dead() {
        let url = spawn_ws_server(true).await;
        let (mut reader, mut writer) = connect_halves(&url, Instant::now()).await;

        let outcome =
            tokio::time::timeout(TEST_DEAD * 8, recv_message_alive(&mut reader, &mut writer)).await;
        assert!(
            outcome.is_err(),
            "a quiet healthy connection stays open; got {outcome:?}"
        );
    }

    /// A caller-owned probe sees the pong through the activity-aware API.
    #[tokio::test]
    async fn activity_receive_surfaces_a_probe_pong() {
        let url = spawn_ws_server(true).await;
        let (mut reader, mut writer) = connect_halves(&url, Instant::now()).await;

        writer.send_ping().await.expect("probe ping");
        let activity =
            tokio::time::timeout(TEST_DEAD, recv_activity_alive(&mut reader, &mut writer))
                .await
                .expect("responsive peer answers before the probe deadline")
                .expect("probe receive");

        assert_eq!(activity, WsActivity::Control);
    }
