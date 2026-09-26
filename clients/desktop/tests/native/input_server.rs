//! Isolated real-server launcher for input-smoke.cjs; see native/INPUT.md.
use phux_server_testkit::{SERVER_JOIN_DEADLINE, run_local, spawn_server_with};
use std::time::Duration;

fn main() {
    let addon = std::env::args()
        .nth(1)
        .expect("input_server /absolute/addon.node");
    run_local(async move {
        let temp = tempfile::TempDir::new().expect("fixture dir");
        let socket = temp.path().join("input.sock");
        let (shutdown, server) = spawn_server_with(socket.clone(), Some("input-smoke"), |config| {
            config.seed_with_pty = true;
            config.seed_command = Some(portable_pty::CommandBuilder::new("/bin/sh"));
        });
        let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/native/input-smoke.cjs");
        let status = tokio::time::timeout(
            Duration::from_secs(60),
            tokio::process::Command::new("node")
                .arg(script)
                .arg(addon)
                .arg(socket)
                .kill_on_drop(true)
                .status(),
        )
        .await
        .expect("smoke deadline")
        .expect("node");
        shutdown.send(()).expect("shutdown");
        tokio::time::timeout(SERVER_JOIN_DEADLINE, server)
            .await
            .expect("shutdown deadline")
            .expect("server join")
            .expect("server stop");
        assert!(status.success(), "input smoke failed: {status}");
    });
}
