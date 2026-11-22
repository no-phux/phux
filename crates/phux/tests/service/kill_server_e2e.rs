//! `phux kill --server`. The stop is the `SHUTDOWN` wire command, not a
//! signal: a signal-killed server would be restarted by launchd's
//! `KeepAlive{SuccessfulExit: false}`, and a deliberate stop must stick
//! (ADR-0080).

#![allow(clippy::expect_used, clippy::panic, reason = "tests")]

use crate::support::{LiveServer, phux_on};

/// The server stops and the socket is gone when the command returns, so a
/// replacement can start immediately.
#[test]
fn kill_server_stops_the_server_and_frees_the_socket() {
    let server = LiveServer::start("doomed");
    let socket = &server.socket;

    let killed = phux_on(socket, &["kill", "--server"]);

    assert!(
        killed.status.success(),
        "kill --server must succeed.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&killed.stdout),
        String::from_utf8_lossy(&killed.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&killed.stderr),
        "phux: ok: server stopped\n"
    );
    assert!(
        std::os::unix::net::UnixStream::connect(socket).is_err(),
        "the server must be gone by the time the command returns, not merely \
         asked to go"
    );
    assert!(
        !socket.exists(),
        "a clean shutdown unlinks its socket; leaving the file behind is the \
         stale entry the next client trips over"
    );
}

/// A stale socket is not a live server, so stopping is still success — and
/// the entry is reaped rather than left for the next client.
#[test]
fn kill_server_reaps_a_stale_socket() {
    let (_dir, ref socket) = LiveServer::cold();

    // Exactly what a SIGKILLed server leaves behind.
    let listener = std::os::unix::net::UnixListener::bind(socket).expect("bind");
    drop(listener);
    assert!(socket.exists());

    let killed = phux_on(socket, &["kill", "--server"]);

    assert!(
        killed.status.success(),
        "a stale socket means no server is running, which is what was asked for"
    );
    assert!(
        !socket.exists(),
        "the stale entry should be reaped on the way past"
    );
}
