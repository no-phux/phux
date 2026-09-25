//! Isolated real server and PTY driver for terminal-painter.mjs.
use std::{path::PathBuf, time::Duration};

use phux_server_testkit::{SERVER_JOIN_DEADLINE, run_local, spawn_server_with_seed_cmd};

#[path = "terminal-pixels.rs"]
mod pixels;

fn main() {
    let mut args = std::env::args().skip(1);
    let addon = args.next().expect("addon path");
    let fixtures = PathBuf::from(args.next().expect("fixtures directory"));
    run_local(async move {
        let temp = tempfile::TempDir::new().expect("isolated fixture directory");
        let socket = temp.path().join("painter.sock");
        let mut command = portable_pty::CommandBuilder::new("python3");
        command.arg(fixtures.join("terminal-pty.py"));
        let (shutdown, server) =
            spawn_server_with_seed_cmd(socket.clone(), "terminal-painter", command);
        let mut bun = tokio::process::Command::new("bun");
        bun.arg(fixtures.join("terminal-painter.mjs"))
            .arg(addon)
            .arg(socket)
            .env("HOME", temp.path())
            .env("XDG_CONFIG_HOME", temp.path().join("config"))
            .env("XDG_DATA_HOME", temp.path().join("data"))
            .kill_on_drop(true);
        let result = tokio::time::timeout(Duration::from_secs(90), bun.status()).await;
        shutdown.send(()).expect("server still alive");
        tokio::time::timeout(SERVER_JOIN_DEADLINE, server)
            .await
            .expect("stop deadline")
            .expect("server task")
            .expect("server shutdown");
        assert!(
            result
                .expect("GPU fixture deadline")
                .expect("start Bun")
                .success()
        );
        pixels::verify(&fixtures.join("../../.cache/terminal-painter"));
    });
}
