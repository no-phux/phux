use super::*;

#[tokio::test]
async fn reconnect_preserves_initial_tui_hello_and_reuses_the_connection() {
    use futures_util::{SinkExt, StreamExt};
    use phux_protocol::caps::{
        BootstrapCapabilities, ServerCapabilities, select_bootstrap_profile,
    };
    use phux_protocol::wire::frame::FrameKind;
    use tokio_tungstenite::tungstenite::Message;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dial = Dial::Ws(WsDial {
        url: format!("ws://{}", listener.local_addr().unwrap()),
        token: None,
        trust: CertTrust::SkipVerify,
        tls_server_name: None,
        identity: None,
    });
    let peer = tokio::spawn(async move {
        let mut offers = Vec::new();
        for _ in 0..2 {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let message = socket.next().await.unwrap().unwrap();
            let (hello, _) = FrameKind::decode(&message.into_data()).unwrap();
            let FrameKind::Hello { client_caps, .. } = &hello else {
                panic!("first protocol frame must be HELLO");
            };
            let (selected_profile, bootstrap_limits) =
                select_bootstrap_profile(client_caps, &BootstrapCapabilities::new()).unwrap();
            let mut bytes = bytes::BytesMut::new();
            FrameKind::HelloOk {
                protocol_major: phux_protocol::PROTOCOL_VERSION.major,
                protocol_minor: phux_protocol::PROTOCOL_VERSION.minor,
                protocol_patch: phux_protocol::PROTOCOL_VERSION.patch,
                server_caps: ServerCapabilities::new(),
                server_id: Vec::new(),
                selected_profile: Some(selected_profile),
                bootstrap_limits,
            }
            .encode(&mut bytes);
            socket.send(Message::Binary(bytes.freeze())).await.unwrap();
            offers.push(hello);
            let message = socket.next().await.unwrap().unwrap();
            assert!(
                matches!(
                    FrameKind::decode(&message.into_data()).unwrap().0,
                    FrameKind::Ping { nonce: 42 }
                ),
                "connection was renegotiated instead of reused"
            );
            let mut bytes = bytes::BytesMut::new();
            FrameKind::Pong { nonce: 42 }.encode(&mut bytes);
            socket.send(Message::Binary(bytes.freeze())).await.unwrap();
        }
        offers
    });
    let mut initial = phux_tui::attach::connect_for_attach(&dial).await.unwrap();
    initial.send(&FrameKind::Ping { nonce: 42 }).await.unwrap();
    assert!(matches!(
        initial.recv().await.unwrap(),
        FrameKind::Pong { nonce: 42 }
    ));
    drop(initial);
    let ReconnectOutcome::Connectable(Some(mut resumed)) =
        wait_until_connectable(&dial, with_deadline(Duration::from_secs(2))).await
    else {
        panic!("remote reconnect did not retain its negotiated connection");
    };
    resumed.send(&FrameKind::Ping { nonce: 42 }).await.unwrap();
    assert!(matches!(
        resumed.recv().await.unwrap(),
        FrameKind::Pong { nonce: 42 }
    ));
    let offers = peer.await.unwrap();
    assert_eq!(
        offers[0], offers[1],
        "reconnect changed the TUI HELLO contract"
    );
}
