//! Wire-level integration test for the `ATTACH`-time PTY resize seam
//! (`phux-2lj`).
//!
//! Reproduces the "vim renders only half the screen" bug: the server
//! spawns the seed pane's PTY at a hardcoded 80x24 (see
//! `seed_session_with_pty` in `runtime.rs`), and used to leave the
//! winsize untouched on `ATTACH`. A client whose host terminal was
//! larger (e.g. 120x48) thus saw a vim buffer that only filled the
//! top 24 rows, because vim itself believed the kernel reported a
//! 24-row PTY.
//!
//! This test drives the actual wire path:
//!
//! 1. Pre-seed a PTY-backed session whose command is
//!    `sh -c 'while :; do stty size; sleep 0.05; done'`. `stty size`
//!    prints the kernel's PTY winsize as `rows cols` (newline-
//!    terminated) every ~50ms. By looping we sidestep the race
//!    between "PTY spawns" and "ATTACH-induced resize lands" — we
//!    just keep polling until we see the post-resize size.
//! 2. Connect, send `ATTACH { viewport: ViewportInfo::new(120, 40) }`,
//!    drain `ATTACHED` + `TERMINAL_SNAPSHOT`.
//! 3. Accumulate `TERMINAL_OUTPUT` frames until we see the byte
//!    sequence `40 120` (matching `stty size`'s `rows cols`
//!    convention). If the fix is reverted, the loop only ever prints
//!    `24 80` and the test times out.
//!
//! Why a loop in the seed command instead of a one-shot `stty size`?
//! Because we don't control the ordering of "PTY spawn → write
//! stty output" vs "ATTACH → `handle_attach::apply_attach_viewport` →
//! winsize ioctl". The loop guarantees that some `stty size`
//! invocation runs *after* the resize lands; the test then catches
//! that one.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

mod common;

use phux_protocol::wire::frame::{
    AttachTarget, FrameKind, TYPE_ATTACHED, TYPE_BOOTSTRAP_BEGIN, TYPE_TERMINAL_OUTPUT,
    ViewportInfo,
};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::timeout;

use crate::common::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, recv_typed, run_local, send_frame,
    spawn_server_with_seed_cmd, wait_for_socket,
};

/// Drain `TERMINAL_OUTPUT` frames until `needle` appears in the
/// accumulated bytes or [`WIRE_RECV_TIMEOUT`] elapses.
///
/// We accumulate across frames because `stty size`'s output may arrive
/// chunked (the actor broadcasts whatever the PTY pump hands us; chunk
/// boundaries are unpredictable).
async fn await_output_substring(stream: &mut UnixStream, needle: &[u8]) -> Vec<u8> {
    let mut acc: Vec<u8> = Vec::new();
    let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline - tokio::time::Instant::now();
        let Ok((type_byte, frame)) = timeout(remaining, recv_typed(stream)).await else {
            break;
        };
        if type_byte != TYPE_TERMINAL_OUTPUT {
            // Snapshots / metadata / acks — irrelevant for this assertion.
            continue;
        }
        if let FrameKind::TerminalOutput { bytes, .. } = frame {
            acc.extend_from_slice(&bytes);
            // `windows().any(eq)` is the byte-substring search; standard
            // library has no `slice::contains_slice` yet.
            if acc.windows(needle.len()).any(|w| w == needle) {
                return acc;
            }
        }
    }
    acc
}

/// `phux-2lj`: ATTACH must propagate the client's outer viewport into
/// the spawned PTY's kernel winsize.
///
/// Drives the seed command described in the module doc and asserts that
/// some `stty size` invocation reports the post-resize dimensions. With
/// the fix reverted (i.e. `handle_attach` ignoring `viewport`), the
/// PTY stays at the hardcoded 80x24 spawn size and the loop only ever
/// prints `24 80` — the assertion fails by timeout.
#[test]
fn attach_resizes_seed_pty_to_client_viewport() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");

        // Loop `stty size` so we catch a print that happens *after* the
        // ATTACH-induced resize. 50ms cadence keeps the test fast on
        // CI while still giving the resize plenty of time to land.
        // Using `/bin/sh` (POSIX) avoids any bash-only constructs.
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.args(["-c", "while :; do stty size; sleep 0.05; done"]);
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "default", cmd);

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;

        // ---- ATTACH with a non-default viewport ----
        // 120x40 chosen to be wildly different from the 80x24 default
        // so the assertion has no chance of false-positive matching.
        let viewport = ViewportInfo::new(120, 40);
        send_frame(
            &mut stream,
            &FrameKind::Attach {
                attach_id: 1,
                target: AttachTarget::ByName("default".to_owned()),
                viewport,
                request_scrollback: false,
                scrollback_limit_lines: 0,
            },
        )
        .await;

        // ---- ATTACHED ----
        let (type_byte, _attached) = recv_typed(&mut stream).await;
        assert_eq!(
            type_byte, TYPE_ATTACHED,
            "first server-to-client frame must be ATTACHED",
        );

        // ---- TERMINAL_SNAPSHOT (one per pane in focused window) ----
        let (type_byte, _snap) = recv_typed(&mut stream).await;
        assert_eq!(
            type_byte, TYPE_BOOTSTRAP_BEGIN,
            "second server-to-client frame must be TERMINAL_SNAPSHOT",
        );

        // ---- Wait for `stty size` to report the post-resize dims ----
        // `stty size` prints `<rows> <cols>\n`. We search for `40 120`
        // (the rows and cols we asked for); CR/LF terminators may or
        // may not be present depending on cooked-mode terminal driver,
        // so we leave them off the needle and rely on `windows()` to
        // catch an embedded match.
        let acc = await_output_substring(&mut stream, b"40 120").await;
        assert!(
            acc.windows(b"40 120".len()).any(|w| w == b"40 120"),
            "PTY winsize never reported as {}x{} after ATTACH \
             (got {} bytes: {:?}). Regression: handle_attach is not \
             propagating ATTACH.viewport to the PTY.",
            120,
            40,
            acc.len(),
            String::from_utf8_lossy(&acc),
        );

        // Clean teardown — the seed command is an infinite loop, so the
        // shutdown_tx + JoinHandle cancellation cascade is what kills it.
        drop(stream);
        shutdown_tx.send(()).ok();
        timeout(crate::common::SERVER_JOIN_DEADLINE, server_handle)
            .await
            .expect("server did not shut down after the shutdown signal")
            .expect("server join")
            .expect("server run_async ok");
    });
}
