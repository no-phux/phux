//! Auto-spawn on a cold socket and graceful socket cleanup. Real binary,
//! real socket directory.

#![allow(clippy::expect_used, clippy::panic, reason = "tests")]

use crate::common;

use std::time::{Duration, Instant};

use crate::support::{LiveServer, phux_on};

const DEADLINE: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(50);

/// A headless create on a cold socket starts an unseeded server: the
/// requested session is the only one, and asking for `default` (the seed's
/// name) succeeds rather than colliding with a seed the CLI itself made.
#[test]
fn headless_new_on_a_cold_socket_creates_only_the_requested_session() {
    let (dir, socket) = LiveServer::cold();

    let out = phux_on(&socket, &["new", "--session", "default", "--json"]);
    let mut server = LiveServer::guard(dir, socket.clone());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "`new -s default` on a cold socket must not collide with a seed.\n\
         stdout: {stdout}\nstderr: {stderr}"
    );
    server.pid();

    let listed = phux_on(&socket, &["ls", "--json"]);
    let listed: serde_json::Value =
        serde_json::from_slice(&listed.stdout).expect("ls --json is JSON");
    let names: Vec<&str> = listed["sessions"]
        .as_array()
        .expect("sessions array")
        .iter()
        .filter_map(|session| session["name"].as_str())
        .collect();
    assert_eq!(names, ["default"], "no stray seed session: {listed}");
}

/// SIGTERM runs the graceful shutdown, so the socket is unlinked (supervisors
/// and test guards send SIGTERM).
#[test]
fn sigterm_unlinks_the_socket_instead_of_leaving_a_stale_entry() {
    let (dir, socket) = LiveServer::cold();

    // The guard matters even though the body ends by stopping the server: an
    // assertion that fails before the SIGTERM would otherwise leak a daemon
    // holding a PTY (phux-whhd). It is idempotent against an already-stopped
    // server -- `status` then reports no pid and the guard returns.
    let out = phux_on(&socket, &["new", "--session", "graceful", "--json"]);
    assert!(
        out.status.success(),
        "phux new must start a server.\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(common::wait_until_accepting(&socket), "server must be up");
    let mut server = LiveServer::guard(dir, socket.clone());
    let pid = server.pid();

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
