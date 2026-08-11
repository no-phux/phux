//! Binary-level end-to-end coverage for `phux kill --server` (phux-pimp).
//!
//! phux had four ways to start a server — auto-spawn, `phux server`, service
//! supervision, and the ADR-0032 re-exec — and no way to stop one. Three
//! places nonetheless documented `phux kill --server` as though it existed:
//! ADR-0080's Decision, `docs/operations.md`, and a live test assertion in
//! `service.rs`. These tests are what makes those three true.
//!
//! The stop is a wire command rather than a signal, and the reason is
//! load-bearing enough to restate here: a signal-killed server exits
//! non-zero-equivalent, and launchd's `KeepAlive{SuccessfulExit: false}`
//! restarts it after `ThrottleInterval`. A signal-based stop would therefore
//! have contradicted ADR-0080's own promise — "a deliberately stopped server
//! stays stopped" — on the platform phux mostly runs on. `SHUTDOWN` cancels
//! the root token and the process exits 0.

#![allow(clippy::expect_used, clippy::panic, reason = "tests")]

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

const PHUX: &str = env!("CARGO_BIN_EXE_phux");
const DEADLINE: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(50);

/// Backstop for a test that fails before it reaches its own stop (phux-whhd).
struct Cleanup {
    socket: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = Command::new(PHUX)
            .args(["kill", "--server", "--socket"])
            .arg(&self.socket)
            .output();
    }
}

fn wait_until_accepting(socket: &Path) -> bool {
    let deadline = Instant::now() + DEADLINE;
    while Instant::now() < deadline {
        if std::os::unix::net::UnixStream::connect(socket).is_ok() {
            return true;
        }
        std::thread::sleep(POLL);
    }
    false
}

fn start_server(socket: &Path, session: &str) {
    let out = Command::new(PHUX)
        .args(["new", "--session", session, "--json", "--socket"])
        .arg(socket)
        .output()
        .expect("run phux new");
    assert!(
        out.status.success(),
        "phux new must start a server.\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(wait_until_accepting(socket), "server must be up");
}

/// The contract: the server stops, and the socket is gone when the command
/// returns.
///
/// Asserting on the socket rather than on the exit code is deliberate. The
/// caller's next act is usually to start a replacement, and a command that
/// returns while the old server still holds the socket makes that fail —
/// which is the bug `phux service install` hits (phux-67wg).
#[test]
fn kill_server_stops_the_server_and_frees_the_socket() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("phux.sock");
    start_server(&socket, "doomed");
    let _cleanup = Cleanup {
        socket: socket.clone(),
        _dir: dir,
    };

    let killed = Command::new(PHUX)
        .args(["kill", "--server", "--socket"])
        .arg(&socket)
        .output()
        .expect("run phux kill --server");

    assert!(
        killed.status.success(),
        "kill --server must succeed.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&killed.stdout),
        String::from_utf8_lossy(&killed.stderr)
    );
    assert!(
        std::os::unix::net::UnixStream::connect(&socket).is_err(),
        "the server must be gone by the time the command returns, not merely \
         asked to go"
    );
    assert!(
        !socket.exists(),
        "a clean shutdown unlinks its socket; leaving the file behind is the \
         stale entry the next client trips over"
    );
}

/// Stopping something already stopped is success.
///
/// "Make it not be running" is the caller's actual intent, and a script that
/// stops a server it is not sure is up should not need to branch. This is
/// also what lets the `Cleanup` guard above run unconditionally.
#[test]
fn kill_server_is_idempotent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("phux.sock");
    start_server(&socket, "doomed");
    let _cleanup = Cleanup {
        socket: socket.clone(),
        _dir: dir,
    };

    for attempt in 1..=2 {
        let killed = Command::new(PHUX)
            .args(["kill", "--server", "--socket"])
            .arg(&socket)
            .output()
            .expect("run phux kill --server");
        assert!(
            killed.status.success(),
            "attempt {attempt} must succeed.\nstderr: {}",
            String::from_utf8_lossy(&killed.stderr)
        );
    }
}

/// A stale socket is not a live server, so stopping is still success — and
/// the entry is reaped rather than left for the next client.
#[test]
fn kill_server_reaps_a_stale_socket() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("phux.sock");

    // Exactly what a SIGKILLed server leaves behind.
    let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
    drop(listener);
    assert!(socket.exists());

    let killed = Command::new(PHUX)
        .args(["kill", "--server", "--socket"])
        .arg(&socket)
        .output()
        .expect("run phux kill --server");

    assert!(
        killed.status.success(),
        "a stale socket means no server is running, which is what was asked for"
    );
    assert!(
        !socket.exists(),
        "the stale entry should be reaped on the way past"
    );
}

/// `--server` and a selector are mutually exclusive, and one is required.
///
/// Guards the direction this could regress in: `kill` previously took a
/// required positional, so making it optional to admit `--server` must not
/// turn a bare `phux kill` into a no-op that exits 0.
#[test]
fn kill_requires_exactly_one_of_target_or_server() {
    let bare = Command::new(PHUX)
        .args(["kill"])
        .output()
        .expect("run phux kill");
    assert!(
        !bare.status.success(),
        "a bare `phux kill` must still be a usage error, not a silent no-op"
    );

    let both = Command::new(PHUX)
        .args(["kill", "--server", "somesession"])
        .output()
        .expect("run phux kill --server somesession");
    assert!(
        !both.status.success(),
        "`--server` and a selector are different operations and must not combine"
    );
}
