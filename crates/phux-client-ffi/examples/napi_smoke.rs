//! Runs the JS encoder against an isolated real PTY-backed phux server.
//! Build the NAPI library first, then pass its .node copy as argv[1].
//! With no mode the main smoke and the fault cases both run. The fault cases
//! need the `napi_host` example's fixture-only `NativeClientLease` export, so
//! `--production-host` runs only the main smoke against the production wrapper
//! and `--faults` runs only the fault cases against the `napi_host` fixture.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "executable test fixture"
)]
#![allow(
    clippy::future_not_send,
    reason = "the real server owns its engine on a LocalSet"
)]

use std::time::Duration;

use phux_server_testkit::{SERVER_JOIN_DEADLINE, run_local, spawn_server_with};

mod napi_fault;

fn main() {
    let addon = std::env::args().nth(1).expect(
        "usage: napi_smoke /absolute/path/phux_client_ffi.node [--production-host|--faults]",
    );
    let mode = std::env::args().nth(2);
    let (main_smoke, faults) = match mode.as_deref() {
        None => (true, true),
        Some("--production-host") => (true, false),
        Some("--faults") => (false, true),
        Some(other) => panic!("unknown napi_smoke mode {other}"),
    };
    run_local(async move {
        if main_smoke {
            run_main_smoke(&addon).await;
        }
        if faults {
            for restart in [false, true] {
                tokio::time::timeout(Duration::from_secs(45), napi_fault::run(&addon, restart))
                    .await
                    .expect("fault fixture deadline");
            }
        }
    });
}

async fn run_main_smoke(addon: &str) {
    let tmp = tempfile::TempDir::new().expect("fixture directory");
    let socket = tmp.path().join("napi.sock");
    let (shutdown, server) = spawn_server_with(socket.clone(), Some("napi-smoke"), |config| {
        config.seed_with_pty = true;
    });
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/napi-smoke.cjs");
    let mut node = tokio::process::Command::new("node");
    node.arg("--expose-gc")
        .arg(script)
        .arg(addon)
        .arg(socket)
        .kill_on_drop(true);
    let status = tokio::time::timeout(Duration::from_secs(120), node.status())
        .await
        .expect("JS smoke deadline")
        .expect("start node");
    let _ = shutdown.send(());
    tokio::time::timeout(SERVER_JOIN_DEADLINE, server)
        .await
        .expect("server stop deadline")
        .expect("server join")
        .expect("server shutdown");
    assert!(status.success(), "NAPI JS smoke failed: {status}");
}
