//! Agent-friendly scenario: concurrent attach to same session.
//!
//! **Scenario:** Two clients attach to the same session simultaneously and both
//! immediately start reading output. This tests:
//! - No lag: both should receive ATTACHED + SNAPSHOT in reasonable time
//! - No duplication: each client sees the output once, not twice
//! - No corruption: pane state remains consistent across both clients
//!
//! **Why this matters:** The TUI and agents both need to attach to the same
//! session concurrently without hanging or seeing garbage output.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]
#![allow(clippy::future_not_send, reason = "LocalSet-driven tests")]
#![allow(clippy::print_stdout, reason = "test diagnostics")]
#![allow(clippy::uninlined_format_args, reason = "test readability")]

use std::time::{Duration, Instant};

use phux_protocol::wire::frame::{
    FrameKind, TYPE_ATTACHED, TYPE_TERMINAL_OUTPUT, TYPE_TERMINAL_SNAPSHOT,
};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::timeout;

use crate::common::screen::Screen;
use crate::common::{
    SOCKET_CONNECT_DEADLINE, attach_by_name, recv_typed, run_local, send_frame,
    spawn_server_with_seed_cmd, wait_for_socket,
};

/// Attach and drain initial ATTACHED + `TERMINAL_SNAPSHOT`, measuring latency.
/// Returns (stream, `latency_ms`, `screen_content`)
async fn attach_and_measure(
    socket_path: &std::path::Path,
    label: &str,
) -> (UnixStream, u128, String) {
    let start = Instant::now();
    let mut stream = wait_for_socket(socket_path, SOCKET_CONNECT_DEADLINE).await;
    send_frame(&mut stream, &attach_by_name("default")).await;

    // Expect ATTACHED
    let (type_byte, _attached) = recv_typed(&mut stream).await;
    assert_eq!(
        type_byte, TYPE_ATTACHED,
        "{label}: first frame must be ATTACHED"
    );

    // Expect TERMINAL_SNAPSHOT
    let (type_byte, _snap) = recv_typed(&mut stream).await;
    assert_eq!(
        type_byte, TYPE_TERMINAL_SNAPSHOT,
        "{label}: second frame must be TERMINAL_SNAPSHOT"
    );

    let latency = start.elapsed().as_millis();

    // Drain any immediate output
    let mut screen = Screen::new(80, 24).expect("failed to create screen");
    let deadline = Instant::now() + Duration::from_millis(100);
    while Instant::now() < deadline {
        let remaining = deadline - Instant::now();
        if let Ok((type_byte, frame)) = timeout(remaining, recv_typed(&mut stream)).await
            && type_byte == TYPE_TERMINAL_OUTPUT
            && let FrameKind::TerminalOutput { bytes, .. } = frame
        {
            screen.write(&bytes);
        }
    }

    (stream, latency, screen.snapshot_text())
}

#[test]
fn concurrent_attach_no_lag_or_duplication() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");

        // Seed with a simple command that prints once then waits
        let cmd = CommandBuilder::new("/bin/sh");
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "default", cmd);

        // ============================================================
        // Scenario: Two concurrent attaches
        // ============================================================
        let socket_a = socket_path.clone();
        let socket_b = socket_path.clone();

        // Spawn both attaches concurrently
        let attach_a =
            tokio::task::spawn_local(
                async move { attach_and_measure(&socket_a, "client_A").await },
            );
        let attach_b =
            tokio::task::spawn_local(
                async move { attach_and_measure(&socket_b, "client_B").await },
            );

        let (res_a, res_b) = tokio::join!(attach_a, attach_b);
        let (_stream_a, latency_a, screen_a) = res_a.unwrap();
        let (_stream_b, latency_b, screen_b) = res_b.unwrap();

        // ============================================================
        // Assertions
        // ============================================================

        // No excessive lag (both should attach within 200ms)
        assert!(
            latency_a < 200,
            "client_A attach took {latency_a}ms (should be <200ms)",
        );
        assert!(
            latency_b < 200,
            "client_B attach took {latency_b}ms (should be <200ms)",
        );

        // No duplication: both should see the same content (no doubled output)
        // The snapshot should show the initial shell prompt, not doubled
        assert_eq!(
            screen_a, screen_b,
            "both clients should see identical output; \
             if content differs, one may be receiving duplicated frames"
        );

        // Sanity: we got *some* initial output
        assert!(
            !screen_a.is_empty(),
            "client_A screen should not be empty after attach"
        );

        println!(
            "✓ concurrent attach latencies: A={}ms B={}ms",
            latency_a, latency_b
        );
        println!("✓ no duplication: both clients see identical content");

        shutdown_tx.send(()).ok();
        server_handle.await.ok();
    });
}
