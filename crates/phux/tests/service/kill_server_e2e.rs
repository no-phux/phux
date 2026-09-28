//! `phux kill --server`. The stop is the `SHUTDOWN` wire command, not a
//! signal: a signal-killed server would be restarted by launchd's
//! `KeepAlive{SuccessfulExit: false}`, and a deliberate stop must stick
//! (ADR-0080).

#![allow(clippy::expect_used, clippy::panic, reason = "tests")]

#[path = "../common/mod.rs"]
mod common;

use std::path::Path;
use std::process::Command;

const PHUX: &str = env!("CARGO_BIN_EXE_phux");

/// Backstop for a test that fails before it reaches its own stop (phux-whhd).
struct Cleanup {
    _server: common::AutoSpawnedServer,
    _dir: tempfile::TempDir,
}

fn spawn_session(socket: &Path, session: &str) {
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
    assert!(common::wait_until_accepting(socket), "server must be up");
}

fn start_server(socket: &Path, session: &str) -> common::AutoSpawnedServer {
    // Arm before spawn: a panic in `phux new` or `wait_until_accepting` must
    // still Drop a guard that can reap via the live socket (phux-e4qx).
    let mut server = common::AutoSpawnedServer::new(PHUX, socket.to_owned());
    spawn_session(socket, session);
    server.capture_pid();
    server
}

fn status_pid(socket: &Path) -> u32 {
    let output = Command::new(PHUX)
        .args(["status", "--json", "--socket"])
        .arg(socket)
        .output()
        .expect("run phux status --json");
    assert!(
        output.status.success(),
        "status --json must succeed.\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let doc: serde_json::Value = serde_json::from_slice(&output.stdout).expect("status emits JSON");
    u32::try_from(doc["pid"].as_u64().expect("status JSON names server pid"))
        .expect("server pid fits u32")
}

/// The server stops and the socket is gone when the command returns, so a
/// replacement can start immediately.
#[test]
fn kill_server_stops_the_server_and_frees_the_socket() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("phux.sock");
    let server = start_server(&socket, "doomed");
    let _cleanup = Cleanup {
        _server: server,
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

/// Stopping something already stopped is success (the intent is "not
/// running").
#[test]
fn kill_server_is_idempotent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("phux.sock");
    let server = start_server(&socket, "doomed");
    let _cleanup = Cleanup {
        _server: server,
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

/// `--server` and a selector are exclusive and one is required: a bare
/// `phux kill` must not become an exit-0 no-op.
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

/// phux-e4qx: Drop must reap a live daemon even when `capture_pid` never ran.
#[test]
fn drop_reaps_a_daemon_whose_pid_was_never_captured() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("phux.sock");
    let server = common::AutoSpawnedServer::new(PHUX, socket.clone());
    spawn_session(&socket, "uncaptured");

    let pid = status_pid(&socket);
    assert!(
        common::process_exists(pid),
        "precondition: the daemon must be alive before Drop"
    );

    drop(server);

    assert!(
        !common::process_exists(pid),
        "Drop must reap the daemon even when capture_pid never ran"
    );
    assert!(
        std::os::unix::net::UnixStream::connect(&socket).is_err(),
        "the socket must not still be live after Drop"
    );
}

/// Acceptance for phux-e4qx: a forced panic after spawn, before `capture_pid`,
/// must not leave an orphan in `ps`.
#[test]
fn panic_unwind_reaps_a_daemon_whose_pid_was_never_captured() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("phux.sock");
    let pid = std::cell::Cell::new(None);

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _server = common::AutoSpawnedServer::new(PHUX, socket.clone());
        spawn_session(&socket, "panic-reap");
        pid.set(Some(status_pid(&socket)));
        panic!("forced phux-e4qx unwind");
    }));
    assert!(outcome.is_err(), "the forced panic must have unwound");

    let pid = pid.get().expect("daemon pid observed before panic");
    assert!(
        !common::process_exists(pid),
        "unwinding Drop must reap the uncaptured daemon; {pid} is still in ps"
    );
}
