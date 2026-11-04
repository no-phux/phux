use super::*;

/// The anonymous loopback listener serves at most its connection cap at
/// once (256 in production): one past it is closed as soon as it is
/// admitted, and a slot a connection gives back is reused.
#[tokio::test(flavor = "current_thread")]
async fn the_anonymous_listener_caps_live_connections() {
    assert_eq!(
        WsListener::bind("127.0.0.1:0".parse().unwrap(), AllowedOrigins::default())
            .await
            .unwrap()
            .max_connections(),
        Some(ANONYMOUS_MAX_CONNECTIONS)
    );
    LocalSet::new()
        .run_until(async {
            let listener =
                WsListener::bind("127.0.0.1:0".parse().unwrap(), AllowedOrigins::default())
                    .await
                    .unwrap()
                    .with_connection_cap(2);
            let addr = listener.local_addr().unwrap();
            let state = crate::state::SharedState::new();
            // A transport test: an embedder engine admits the peer (ADR-0072).
            state.with_mut(|s| {
                s.set_policy_engine(std::sync::Arc::new(crate::policy::OwnerForAll));
            });
            let root_token = CancellationToken::new();
            let accept_state = state.clone();
            let accept_token = root_token.clone();
            let accept_task = tokio::task::spawn_local(async move {
                crate::runtime::connections::accept_loop(
                    &listener,
                    accept_state,
                    accept_token,
                    None,
                )
                .await
            });
            let connect = || async move {
                let tcp = TcpStream::connect(addr).await.unwrap();
                tokio_tungstenite::client_async(request(addr), tcp)
                    .await
                    .expect("the upgrade completes")
                    .0
            };
            // Closed by the server within the window, or still open.
            let closed_within = |mut client: WebSocketStream<TcpStream>, window| async move {
                let closed = matches!(
                    tokio::time::timeout(window, client.next()).await,
                    Ok(None | Some(Err(_) | Ok(Message::Close(_))))
                );
                (closed, client)
            };
            let window = Duration::from_millis(500);

            let first = connect().await;
            let (closed, second) = closed_within(connect().await, window).await;
            assert!(!closed, "the second connection is within the cap");
            let (closed, _third) = closed_within(connect().await, window).await;
            assert!(closed, "a connection past the cap is closed");

            drop(first);
            let mut reused = false;
            for _ in 0..20 {
                let (closed, client) = closed_within(connect().await, window).await;
                if !closed {
                    reused = true;
                    drop(client);
                    break;
                }
            }
            assert!(reused, "a returned slot is reused");
            drop(second);
            root_token.cancel();
            let _ = accept_task.await;
        })
        .await;
}

#[tokio::test]
async fn anonymous_listener_refuses_browser_credentials_but_accepts_public_protocol() {
    let listener = WsListener::bind("127.0.0.1:0".parse().unwrap(), AllowedOrigins::default())
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let token = hex::encode(TEST_TOKEN);
    let (_, response) = refused_handshake(
        &listener,
        addr,
        browser_request(addr, &format!("phux.v1, phux.bearer.{token}")),
    )
    .await;
    assert_generic_unauthorized(response);

    let (_, identity, response) =
        exchange(&listener, addr, browser_request(addr, "phux.v1"), &[]).await;
    assert!(identity.credential.is_none());
    assert_eq!(response.headers()["sec-websocket-protocol"], "phux.v1");
}

/// A web page the user happens to visit cannot drive the anonymous
/// loopback listener: its handshake carries a foreign `Origin` and is
/// refused. Native clients (no `Origin`) and locally served pages still
/// connect, and an operator can name extra origins.
#[tokio::test]
async fn anonymous_listener_refuses_foreign_browser_origins() {
    let with_origin = |addr: SocketAddr, origin: &str| {
        let mut req = request(addr);
        req.headers_mut().insert("origin", origin.parse().unwrap());
        req
    };
    let listener = WsListener::bind("127.0.0.1:0".parse().unwrap(), AllowedOrigins::default())
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    for origin in [
        "https://evil.example",
        "null",
        "http://127.0.0.1.evil.example",
    ] {
        let (_, error) = refused_handshake(&listener, addr, with_origin(addr, origin)).await;
        let WebSocketError::Http(response) = error else {
            panic!("{origin}: expected an HTTP refusal");
        };
        assert_eq!(
            response.status(),
            tokio_tungstenite::tungstenite::http::StatusCode::FORBIDDEN,
            "{origin}"
        );
    }
    for origin in [
        "http://127.0.0.1:4321",
        "http://localhost:8080",
        "https://[::1]",
    ] {
        exchange(&listener, addr, with_origin(addr, origin), &[]).await;
    }
    exchange(&listener, addr, request(addr), &[]).await;

    let fronted = WsListener::bind(
        "127.0.0.1:0".parse().unwrap(),
        AllowedOrigins::parse(Some("https://phux.sh")),
    )
    .await
    .unwrap();
    let addr = fronted.local_addr().unwrap();
    exchange(&fronted, addr, with_origin(addr, "https://phux.sh"), &[]).await;
}

#[test]
fn origin_policy_parses_lists_wildcards_and_loopback_forms() {
    let policy = AllowedOrigins::parse(Some(" https://a.example/ , https://B.example"));
    assert!(policy.admits("https://a.example"));
    assert!(policy.admits("https://b.example"));
    assert!(!policy.admits("https://c.example"));
    assert!(AllowedOrigins::parse(Some("*")).admits("https://c.example"));
    let loopback = AllowedOrigins::default();
    for admitted in [
        "http://localhost",
        "http://127.0.0.2:9",
        "https://[::1]:443",
    ] {
        assert!(loopback.admits(admitted), "{admitted}");
    }
    for refused in [
        "null",
        "file://",
        "http://localhost.evil.example",
        "http://evil.example#@127.0.0.1",
        "http://[::1].evil.example",
        "ws://127.0.0.1",
    ] {
        assert!(!loopback.admits(refused), "{refused}");
    }
}
