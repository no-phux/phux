//! Fixtures shared by the service suites: one `phux` run against a socket,
//! and a guarded auto-spawned server.

#![allow(
    unreachable_pub,
    reason = "shared by sibling suites in this test binary"
)]
#![allow(clippy::expect_used, reason = "tests")]

use std::path::{Path, PathBuf};
use std::process::Output;

use crate::common;

/// `phux <args...> --socket <socket>`, run to completion with stdin closed.
pub fn phux_on(socket: &Path, args: &[&str]) -> Output {
    common::phux_cmd(crate::runner::phux_bin())
        .args(args)
        .arg("--socket")
        .arg(socket)
        .output()
        .expect("run phux")
}

/// A server auto-spawned on `<tempdir>/phux.sock`. Drop stops it and then
/// removes the directory, so a failing assertion cannot leak a daemon
/// holding a PTY (phux-whhd).
pub struct LiveServer {
    pub socket: PathBuf,
    server: common::AutoSpawnedServer,
    _dir: tempfile::TempDir,
}

impl LiveServer {
    /// A fresh directory and the socket path inside it nothing listens on.
    pub fn cold() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("phux.sock");
        (dir, socket)
    }

    /// `phux new --session <session>` on a cold socket. The guard is armed
    /// before the spawn, so a panic in between still reaps (phux-e4qx).
    pub fn start(session: &str) -> Self {
        let (dir, socket) = Self::cold();
        let mut server = Self::guard(dir, socket);
        let out = phux_on(&server.socket, &["new", "--session", session, "--json"]);
        assert!(
            out.status.success(),
            "phux new must start a server.\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            common::wait_until_accepting(&server.socket),
            "server must be up"
        );
        server.pid();
        server
    }

    /// Guard whatever server ends up on `socket` inside `dir`.
    pub fn guard(dir: tempfile::TempDir, socket: PathBuf) -> Self {
        Self {
            server: common::AutoSpawnedServer::new(crate::runner::phux_bin(), socket.clone()),
            socket,
            _dir: dir,
        }
    }

    /// The live daemon's pid, recorded so Drop waits for it to exit.
    pub fn pid(&mut self) -> u32 {
        self.server.capture_pid()
    }
}
