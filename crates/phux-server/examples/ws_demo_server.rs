//! Standalone seeded WebSocket server, for the phux-web browser e2e.
//!
//! Runs a real phux server with a PTY-backed `default` session that prints a
//! deterministic marker (`PHUX_WEB_OK`) then idles, listening for WebSocket
//! clients on `PHUX_WS_ADDR` (default `127.0.0.1:47654`). Blocks forever.
//! Port 0 lets the kernel pick; the `listening on` line on stderr names the
//! address actually bound, which is how the browser e2e learns it.
//!
//! Honors the same `PHUX_WS_*` process configuration as `phux server`
//! (`PHUX_WS_SECURE`, `PHUX_WS_TOKENS`, `PHUX_WS_TLS_CERT`/`KEY`, …) via
//! [`ServerEnv::from_process`], so CI's authenticated fallback can probe
//! `wss://` with a bearer token.
//!
//!   PHUX_WS_ADDR=127.0.0.1:47654 cargo run --example ws_demo_server

#![allow(
    clippy::print_stderr,
    clippy::expect_used,
    clippy::doc_markdown,
    reason = "example/dev tool"
)]

use phux_protocol::wire::RemoteListenerTransport;
use phux_server::{ServerConfig, ServerEnv, ServerRuntime};
use portable_pty::CommandBuilder;

fn main() {
    let addr: std::net::SocketAddr = std::env::var("PHUX_WS_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:47654".to_owned())
        .parse()
        .expect("PHUX_WS_ADDR is a socket address");

    let socket_path = std::env::temp_dir().join(format!("phux-e2e-{}.sock", std::process::id()));

    // A PTY session that emits a deterministic marker, then stays alive.
    let mut cmd = CommandBuilder::new("sh");
    cmd.args(["-c", "printf 'PHUX_WEB_OK\\r\\n'; sleep 3600"]);

    // Standalone process binary: snapshot PHUX_* the same way `phux server`
    // does. `with_default_socket()` leaves env empty for hermetic in-process
    // tests; this example is launched with CI-supplied TLS/token env.
    let env = ServerEnv::from_process();
    let scheme = if env.ws_secure { "wss" } else { "ws" };

    let cfg = ServerConfig {
        socket_path: socket_path.clone(),
        pre_seeded_session: Some("default".to_owned()),
        seed_with_pty: true,
        seed_command: Some(cmd),
        env,
        ..ServerConfig::with_default_socket()
    };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    tokio::task::LocalSet::new().block_on(&runtime, async move {
        let server = tokio::task::spawn_local(
            ServerRuntime::new(cfg)
                .listen_ws(addr)
                .run_async(async { tokio::signal::ctrl_c().await.expect("shutdown signal") }),
        );
        let bound =
            phux_server_testkit::bound_listener_addr(&socket_path, RemoteListenerTransport::Wss)
                .await;
        eprintln!("ws-demo-server listening on {scheme}://{bound}/  (seed: default)");
        server.await.expect("server task").expect("server run");
    });
}
