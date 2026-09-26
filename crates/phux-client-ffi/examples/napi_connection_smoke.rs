//! Actual addon lifecycle and daemon-incarnation test, using only owned PTYs.
//! Run after building `napi_host` and copying its dylib to `napi_host.node`:
//! ```text
//! cargo run -p phux-client-ffi --no-default-features --features napi
//!   --example napi_connection_smoke -- /absolute/path/napi_host.node
//! ```
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::future_not_send,
    reason = "bounded executable fixture with real LocalSet server"
)]

use std::time::Duration;

use phux_server_testkit::{SERVER_JOIN_DEADLINE, run_local, spawn_server_with};

fn main() {
    let addon = std::env::args().nth(1).expect("pass the addon .node path");
    let production = std::env::args().any(|arg| arg == "--production-host");
    run_local(async move {
        tokio::time::timeout(Duration::from_secs(60), run(&addon, production))
            .await
            .expect("connection smoke deadline");
    });
}

async fn run(addon: &str, production: bool) {
    let tmp = tempfile::TempDir::new().expect("owned fixture directory");
    let socket = tmp.path().join("server.sock");
    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/napi-connection-smoke.cjs");
    let (shutdown, server) = spawn_server_with(socket.clone(), Some("napi-smoke"), |config| {
        config.seed_with_pty = true;
    });
    // Start against an already-listening daemon. Initial home selection must
    // travel in connect options, not win a race against HELLO processing.
    while !socket.exists() {
        assert!(!server.is_finished(), "server exited before binding");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let mut child = tokio::process::Command::new("node")
        .arg(script)
        .arg(addon)
        .arg(&socket)
        .arg(tmp.path())
        .args(production.then_some("--production-host"))
        .kill_on_drop(true)
        .spawn()
        .expect("start node fixture");
    wait_marker(&mut child, &tmp.path().join("restart")).await;
    shutdown.send(()).expect("restart old daemon");
    tokio::time::timeout(SERVER_JOIN_DEADLINE, server)
        .await
        .expect("old daemon deadline")
        .expect("old daemon join")
        .expect("old daemon shutdown");
    let (shutdown, server) = spawn_server_with(socket.clone(), Some("napi-smoke"), |config| {
        config.seed_with_pty = true;
    });
    let status = child.wait().await.expect("node exit");
    let _ = shutdown.send(());
    tokio::time::timeout(SERVER_JOIN_DEADLINE, server)
        .await
        .expect("daemon deadline")
        .expect("daemon join")
        .expect("daemon shutdown");
    assert!(status.success(), "connection smoke failed: {status}");
}

async fn wait_marker(child: &mut tokio::process::Child, marker: &std::path::Path) {
    while !marker.exists() {
        assert!(
            child.try_wait().expect("node status").is_none(),
            "node exited before {}",
            marker.display()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
