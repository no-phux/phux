//! Resize-storm stress test (e2e flywheel item 6).
//!
//! A real client whose host terminal is being dragged generates a burst
//! of `VIEWPORT_RESIZE` frames in quick succession. Each one reflows the
//! PTY (`handle_attach::apply_attach_viewport` → winsize ioctl) and asks
//! the inner program to repaint. The failure mode this guards against is a
//! panic or a stuck grid: the server must absorb the storm and the final
//! geometry must converge to the last size requested.
//!
//! Built on [`E2eBuilder`]: a seed pane loops `stty size` so the kernel's
//! view of the winsize is observable on the wire, then the test fires a
//! rapid sequence of resizes and asserts the final reported dimensions
//! match the last resize. No panic + dimension convergence.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]
#![allow(clippy::future_not_send, reason = "LocalSet-driven tests")]

use portable_pty::CommandBuilder;

use phux_server_testkit::builder::E2eBuilder;
use phux_server_testkit::run_local;
use phux_server_testkit::tracing_capture::TracingCapture;

/// A storm of resizes must not panic the server and must leave the PTY at
/// the final requested geometry. `stty size` reports `<rows> <cols>`.
#[ignore = "real-PTY e2e; starves the parallel pool. Run via `just e2e`."]
#[test]
fn resize_storm_converges_to_final_geometry() {
    run_local(async {
        let cap = TracingCapture::install("resize_storm");

        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.args(["-c", "while :; do stty size; sleep 0.03; done"]);

        E2eBuilder::new()
            .session("default")
            .seed_cmd(cmd)
            .viewport(80, 24)
            .run(|mut clients| async move {
                let client = &mut clients[0];

                // Fire a rapid storm of distinct sizes. The cadence (no
                // sleep between sends) is the worst case: the server's
                // resize coalescing / actor mailbox must not wedge.
                let storm: &[(u16, u16)] = &[
                    (100, 30),
                    (120, 40),
                    (90, 50),
                    (140, 35),
                    (110, 45),
                    (130, 38),
                ];
                for &(c, r) in storm {
                    client.resize(c, r).await;
                }

                // The last resize wins. Drive a final, settled resize and
                // wait for `stty size` to report it. Final size chosen
                // distinct from every storm entry so the match is exact.
                let (final_cols, final_rows) = (128u16, 42u16);
                client.resize(final_cols, final_rows).await;

                // `stty size` prints `<rows> <cols>`. Wait until the oracle
                // shows the post-storm geometry.
                let needle = format!("{final_rows} {final_cols}");
                let res = client.wait_until(|s| s.contains(&needle)).await;
                // Bounded drain, not screenshot(): the `stty size` seed loops
                // every 30ms; screenshot's "drain until 20ms quiet" can spin
                // against it under load (phux-s2iw class). wait_until already
                // populated the oracle; a bounded drain refreshes it.
                client.drain_output_bounded(32).await;
                cap.attach_screen(client.snapshot_text());
                assert!(
                    res.is_ok(),
                    "PTY winsize never converged to {final_cols}x{final_rows} \
                     after a resize storm; screen=\n{}",
                    res.unwrap_err(),
                );
            })
            .await;
    });
}
