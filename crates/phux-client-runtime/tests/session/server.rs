//! Real server composition for the QUIC session fixtures.
use super::*;

/// A server on `socket` that also listens on loopback QUIC; returns the
/// address the listener bound. `tokens` names a bearer store the listener
/// then requires, as an enrolled host's does.
#[allow(
    clippy::redundant_pub_crate,
    reason = "integration fixture shared only with the parent test module"
)]
pub(super) async fn quic_server(
    tmp: &TempDir,
    socket: &Path,
    tokens: Option<&Path>,
) -> (oneshot::Sender<()>, ServerTask, SocketAddr) {
    let cert = tmp.path().join("cert.pem");
    let key = tmp.path().join("key.pem");
    phux_server::transport::tls::ensure_self_signed(&cert, &key).expect("self-signed cert");
    let mut config = ServerConfig {
        socket_path: socket.to_path_buf(),
        pre_seeded_session: Some("main".to_owned()),
        seed_with_pty: false,
        seed_command: None,
        // Only what this test sets: never the shell's `PHUX_*`.
        env: ServerEnv {
            tls_cert: Some(cert),
            tls_key: Some(key),
            ws_secure: tokens.is_some(),
            ws_tokens: tokens.map(Path::to_path_buf),
            ..ServerEnv::default()
        },
        ..ServerConfig::with_default_socket()
    };
    // A transport test: an embedder engine admits the loopback dial
    // (ADR-0072); no `phux server` mode does (ADR-0179).
    phux_server_testkit::embedder::embed_owner_engine(&mut config);
    let (shutdown, stopped) = oneshot::channel::<()>();
    let server = tokio::task::spawn_local(async move {
        phux_server_testkit::terminal_server(config)
            .listen_quic(SocketAddr::from(([127, 0, 0, 1], 0)))
            .run_async(async move {
                let _ = stopped.await;
            })
            .await
    });
    let addr = bound_listener_addr(socket, RemoteListenerTransport::Quic).await;
    (shutdown, server, addr)
}
