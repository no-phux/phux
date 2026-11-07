//! The explicit WebSocket server composition exercised by the runtime client.
use super::*;

#[test]
fn ipv6_websocket_attaches_exchanges_output_and_joins_on_close() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let mut config = phux_server::ServerConfig {
            socket_path: socket.clone(),
            pre_seeded_session: Some("main".to_owned()),
            seed_with_pty: false,
            seed_command: None,
            ..phux_server::ServerConfig::with_default_socket()
        };
        phux_server_testkit::embedder::embed_owner_engine(&mut config);
        let (shutdown, stop) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::task::spawn_local(async move {
            phux_server_testkit::terminal_server(config)
                .listen_ws((std::net::Ipv6Addr::LOCALHOST, 0).into())
                .run_async(async move {
                    let _ = stop.await;
                })
                .await
                .expect("IPv6 WebSocket test server");
        });
        let address = phux_server_testkit::bound_listener_addr(
            &socket,
            phux_protocol::wire::RemoteListenerTransport::Wss,
        )
        .await;
        assert!(address.is_ipv6());
        let client =
            Runtime::connect(Target::ws(format!("ws://{address}/phux")), options()).unwrap();
        wait_for_status(&client, Status::Attached).await;
        let terminal = spawn_cat(&client).await;
        assert!(client.send_text(&terminal, "ipv6-runtime"));
        wait_for_text(&client, &terminal, "ipv6-runtime").await;
        client.close();
        wait_for_status(&client, Status::Closed).await;
        // Dropping the last client joins the actual driver thread.
        let joined = tokio::task::spawn_blocking(move || drop(client));
        tokio::time::timeout(DEADLINE, joined)
            .await
            .unwrap()
            .unwrap();
        drop(shutdown);
        tokio::time::timeout(DEADLINE, server)
            .await
            .unwrap()
            .unwrap();
    });
}
