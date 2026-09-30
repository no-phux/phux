//! Auto-spawn against a stale socket file left by a server that died without
//! unlinking it: every later invocation used to find the file, skip the spawn,
//! and fail to connect. Real binary, real socket directory.

#![allow(clippy::expect_used, clippy::panic, reason = "tests")]

#[path = "../common/mod.rs"]
mod common;

use std::os::unix::net::UnixListener;
use std::path::Path;
use std::time::{Duration, Instant};

const PHUX: &str = env!("CARGO_BIN_EXE_phux");
const DEADLINE: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(50);

/// Kill whatever server ended up on `socket` (its pid comes from
/// `phux status --json`), so a failed assertion cannot leak a daemon.
struct Cleanup {
    _server: common::AutoSpawnedServer,
    _dir: tempfile::TempDir,
}

/// A socket file with no listener, as a `SIGKILL`ed server leaves; refusal
/// is polled because a just-closed listener can still accept off the backlog.
fn leave_stale_socket(path: &Path) {
    let listener = UnixListener::bind(path).expect("bind the doomed listener");
    drop(listener);
    assert!(
        path.exists(),
        "a Unix socket file outlives its listener; without that this test proves nothing"
    );
    let deadline = Instant::now() + DEADLINE;
    while Instant::now() < deadline {
        if std::os::unix::net::UnixStream::connect(path).is_err() {
            return;
        }
        std::thread::sleep(POLL);
    }
    panic!(
        "{} still accepts connections; it is not the stale socket this test needs",
        path.display()
    );
}

/// `phux new` against a stale socket must reap it and start a server, not
/// report a dead socket to the user.
#[test]
fn auto_spawn_reaps_a_stale_socket_instead_of_wedging() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("phux.sock");
    leave_stale_socket(&socket);

    let out = common::phux_cmd(PHUX)
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
    let mut server = common::AutoSpawnedServer::new(PHUX, socket.clone());
    server.capture_pid();
    let _cleanup = Cleanup {
        _server: server,
        _dir: dir,
    };
    assert!(
        stdout.contains("\"session\": \"revived\""),
        "the session should have been created on a freshly spawned server.\n\
         stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        common::wait_until_accepting(&socket),
        "a server must be accepting on the reaped path"
    );
}

/// A second invocation reuses the live server rather than reaping a healthy
/// socket.
#[test]
fn a_second_invocation_reuses_the_live_server() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("phux.sock");
    leave_stale_socket(&socket);

    let first = common::phux_cmd(PHUX)
        .args(["new", "--session", "first", "--json", "--socket"])
        .arg(&socket)
        .output()
        .expect("run phux new (first)");
    assert!(first.status.success(), "first invocation must succeed");
    assert!(common::wait_until_accepting(&socket), "server must be up");
    let mut server = common::AutoSpawnedServer::new(PHUX, socket.clone());
    server.capture_pid();
    let _cleanup = Cleanup {
        _server: server,
        _dir: dir,
    };

    let second = common::phux_cmd(PHUX)
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
    let listed = common::phux_cmd(PHUX)
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

/// SIGTERM runs the graceful shutdown, so the socket is unlinked (supervisors
/// and test guards send SIGTERM).
#[test]
fn sigterm_unlinks_the_socket_instead_of_leaving_a_stale_entry() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("phux.sock");

    // The guard matters even though the body ends by stopping the server: an
    // assertion that fails before the SIGTERM would otherwise leak a daemon
    // holding a PTY (phux-whhd). It is idempotent against an already-stopped
    // server -- `status` then reports no pid and the guard returns.
    let out = common::phux_cmd(PHUX)
        .args(["new", "--session", "graceful", "--json", "--socket"])
        .arg(&socket)
        .output()
        .expect("run phux new");
    assert!(
        out.status.success(),
        "phux new must start a server.\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(common::wait_until_accepting(&socket), "server must be up");
    let mut server = common::AutoSpawnedServer::new(PHUX, socket.clone());
    let pid = server.capture_pid();
    let _cleanup = Cleanup {
        _server: server,
        _dir: dir,
    };

    common::terminate(pid);

    // The socket FILE must go, not merely stop accepting: a file that outlives
    // its listener is precisely the stale entry.
    let deadline = Instant::now() + DEADLINE;
    while Instant::now() < deadline {
        if !socket.exists() {
            return;
        }
        std::thread::sleep(POLL);
    }
    panic!(
        "SIGTERM left {} behind; the graceful shutdown path did not run",
        socket.display()
    );
}
