//! End-to-end coverage for auto-spawn against a **stale** socket (phux-zomb.1).
//!
//! The bug this pins was the whole of "phux is broken and I have to `rm` a
//! socket to use it". A server that dies without unlinking — `SIGKILL`, a
//! panic, a supervisor tearing it down, a hard reboot — leaves its socket file
//! behind. Auto-spawn used to be gated on `!socket_path.exists()`, so from
//! that moment on every invocation found a file, declined to start a server,
//! and then failed to connect to the nothing that was listening. The wedge was
//! permanent and self-inflicted: nothing in phux ever cleaned it up.
//!
//! These tests drive the real binary against a real socket directory, because
//! the defect lived precisely in the seam between "what the filesystem says"
//! and "what a process will answer" — a seam that a mocked filesystem cannot
//! reproduce.

#![allow(clippy::expect_used, clippy::panic, reason = "tests")]

use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const PHUX: &str = env!("CARGO_BIN_EXE_phux");
const DEADLINE: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(50);

/// Kill whatever server ended up on `socket`, so a failing assertion cannot
/// leak a daemon holding a PTY.
///
/// The server is auto-spawned and daemonised, so there is no `Child` to
/// reap — its pid comes back over the wire in `phux status --json` (the
/// server reads it from `SO_PEERCRED`). There is deliberately no
/// "stop the server" verb to lean on here; `phux kill` takes a session
/// selector, and killing sessions is not the same as ending the daemon.
struct Cleanup {
    socket: PathBuf,
    _dir: tempfile::TempDir,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        let Ok(out) = Command::new(PHUX)
            .args(["status", "--json", "--socket"])
            .arg(&self.socket)
            .output()
        else {
            return;
        };
        let Ok(doc) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else {
            return;
        };
        let Some(pid) = doc["pid"].as_i64() else {
            return;
        };
        // SIGTERM, not SIGKILL: the server's shutdown path is what unlinks
        // the socket, and leaving a stale entry behind in a temp dir that is
        // about to be removed would be a strange way to end a test about
        // stale entries.
        let _ = Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .output();
    }
}

/// A socket file with no listener — exactly what a `SIGKILL`ed server leaves.
fn leave_stale_socket(path: &Path) {
    let listener = UnixListener::bind(path).expect("bind the doomed listener");
    drop(listener);
    assert!(
        path.exists(),
        "a Unix socket file outlives its listener; without that this test proves nothing"
    );
    assert!(
        std::os::unix::net::UnixStream::connect(path).is_err(),
        "nothing may be accepting on the stale socket"
    );
}

/// Wait for `socket` to accept, so the assertion is on liveness rather than on
/// the file's existence — the distinction this whole module is about.
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

/// `phux new` against a stale socket must reap it and start a server, not
/// report a dead socket to the user.
#[test]
fn auto_spawn_reaps_a_stale_socket_instead_of_wedging() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("phux.sock");
    leave_stale_socket(&socket);

    let _cleanup = Cleanup {
        socket: socket.clone(),
        _dir: dir,
    };

    let out = Command::new(PHUX)
        .args(["new", "--session", "revived", "--json", "--socket"])
        .arg(&socket)
        .output()
        .expect("run phux new");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "a stale socket must not be fatal.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("\"session\": \"revived\""),
        "the session should have been created on a freshly spawned server.\n\
         stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        wait_until_accepting(&socket),
        "a server must be accepting on the reaped path"
    );
}

/// The same path, twice: the second invocation must reuse the live server
/// rather than reaping a socket that is now healthy.
///
/// Guards the direction the fix could over-correct in. "Always unlink before
/// spawning" would also cure the wedge — by killing a working server's socket
/// on every subsequent command.
#[test]
fn a_second_invocation_reuses_the_live_server() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("phux.sock");
    leave_stale_socket(&socket);

    let _cleanup = Cleanup {
        socket: socket.clone(),
        _dir: dir,
    };

    let first = Command::new(PHUX)
        .args(["new", "--session", "first", "--json", "--socket"])
        .arg(&socket)
        .output()
        .expect("run phux new (first)");
    assert!(first.status.success(), "first invocation must succeed");
    assert!(wait_until_accepting(&socket), "server must be up");

    let second = Command::new(PHUX)
        .args(["new", "--session", "second", "--json", "--socket"])
        .arg(&socket)
        .output()
        .expect("run phux new (second)");
    assert!(
        second.status.success(),
        "second invocation must succeed against the live server.\nstderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );

    // Both sessions live on ONE server — proof the second run reused it
    // rather than reaping the socket and starting a replacement.
    let listed = Command::new(PHUX)
        .args(["ls", "--socket"])
        .arg(&socket)
        .output()
        .expect("run phux ls");
    let sessions = String::from_utf8_lossy(&listed.stdout);
    assert!(
        sessions.contains("first") && sessions.contains("second"),
        "both sessions must be on the same server; got:\n{sessions}"
    );
}
