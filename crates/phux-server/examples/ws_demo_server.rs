//! Standalone seeded WebSocket server, for the phux-web browser e2e.
//!
//! Runs a real phux server with a PTY-backed `default` session that prints a
//! deterministic marker (`PHUX_WEB_OK`) then idles, listening for WebSocket
//! clients on `PHUX_WS_ADDR` (default `127.0.0.1:47654`). Blocks forever.
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
        socket_path,
        pre_seeded_session: Some("default".to_owned()),
        seed_with_pty: true,
        seed_command: Some(cmd),
        env,
        ..ServerConfig::with_default_socket()
    };

    eprintln!("ws-demo-server listening on {scheme}://{addr}/  (seed: default)");
    ServerRuntime::new(cfg)
        .listen_ws(addr)
        .run(async { tokio::signal::ctrl_c().await.expect("shutdown signal") })
        .expect("server run");
}
