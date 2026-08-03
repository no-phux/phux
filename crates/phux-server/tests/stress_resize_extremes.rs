//! Resize-extremes adversarial stress (crash-hunt wave).
//!
//! The companion to `stress_resize_storm`: instead of a sane sequence of
//! drag sizes, this fires the *pathological* edges — `0x0`, `1x1`,
//! `1x200`, `200x1`, a 1000x1000 monster, and a tight both-axes-shrink
//! storm — at a real PTY-backed pane while output is flowing. Each
//! scenario asserts the server does NOT panic (a panicked actor task
//! surfaces as a wire hang → `wait_until`/`converge` timeout, or as the
//! `run_async` error the harness teardown unwraps) and converges to a
//! coherent final geometry.
//!
//! Why this is a separate file: it stays in the heavy `just e2e` lane
//! (`#[ignore]`) so the sub-ms resize bursts don't starve the
//! latency-sensitive timing tests in the default pool.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]
#![allow(clippy::future_not_send, reason = "LocalSet-driven tests")]

use portable_pty::CommandBuilder;

use phux_server_testkit::builder::E2eBuilder;
use phux_server_testkit::run_local;
use phux_server_testkit::tracing_capture::TracingCapture;

/// Drive a `stty size` loop and hammer it with degenerate viewports
/// including `0x0` and `1x1`, then settle to a sane size and assert the
/// PTY winsize converges. The zero-dimension and 1-cell cases are the
/// ones most likely to trip a `Terminal::resize` clamp bug or the
/// known `PageList.resizeCols` overflow at the boundary.
#[ignore = "real-PTY e2e; starves the parallel pool. Run via `just e2e`."]
#[test]
fn resize_degenerate_viewports_do_not_panic() {
    run_local(async {
        let cap = TracingCapture::install("resize_degenerate");

        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.args(["-c", "while :; do stty size; sleep 0.02; done"]);

        E2eBuilder::new()
            .session("default")
            .seed_cmd(cmd)
            .viewport(80, 24)
            .run(|mut clients| async move {
                let client = &mut clients[0];

                // Degenerate sizes, fired with no inter-send delay. The
                // server must absorb every one without panicking the pane
                // actor. `0x0` exercises the zero-dimension clamp path; the
                // mixed 1-cell / huge sizes exercise the resize-clamp and
                // the both-axes-shrink path at the boundary.
                let storm: &[(u16, u16)] = &[
                    (0, 0),
                    (1, 1),
                    (1, 200),
                    (200, 1),
                    (1, 1),
                    (0, 0),
                    (1000, 1000),
                    (1, 1),
                    (3, 3),
                    (2, 2),
                    (1, 1),
                ];
                for &(c, r) in storm {
                    // `resize_raw`: push the exact degenerate dims over the
                    // wire without forcing the oracle into a zero-dim grid.
                    client.resize_raw(c, r).await;
                }

                // Settle to a sane geometry and confirm the PTY recovers.
                let (final_cols, final_rows) = (88u16, 26u16);
                client.resize(final_cols, final_rows).await;
                let needle = format!("{final_rows} {final_cols}");
                let res = client.wait_until(|s| s.contains(&needle)).await;
                // Bounded drain, not `screenshot()`: the `stty size` seed loops
                // every 20ms, so screenshot's "drain until 20ms quiet" can spin
                // forever against it (same hang class as the both-axes storm).
                client.drain_output_bounded(32).await;
                cap.attach_screen(client.snapshot_text());
                assert!(
                    res.is_ok(),
                    "PTY winsize never converged to {final_cols}x{final_rows} \
                     after a degenerate-resize storm (server actor may have \
                     panicked); screen=\n{}",
                    res.unwrap_err(),
                );
            })
            .await;
    });
}

/// A both-axes-shrink storm under live output. Every step shrinks BOTH
/// cols and rows from the previous, repeatedly crossing the 1-cell
/// clamp boundary, while a colored burst floods the grid. This is the
/// worst case for the `PageList.resizeCols` both-shrink overflow: the
/// libghostty-vt 0.2.0 engine must hold at every step including the clamp to 1.
#[ignore = "real-PTY e2e; starves the parallel pool. Run via `just e2e`."]
#[test]
fn both_axes_shrink_storm_under_output_does_not_panic() {
    run_local(async {
        let cap = TracingCapture::install("both_axes_shrink_storm");

        // Continuous, rate-bounded output reflowed on every shrink. A
        // tiny per-line sleep keeps the flood from starving the
        // current-thread runtime (an unbounded `printf` loop outruns the
        // PTY pump and stalls the attach handshake — a harness artifact,
        // not the bug under test).
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.args([
            "-c",
            "i=0; while :; do i=$((i+1)); \
             printf 'row-%d-aaaaaaaaaaaaaaaaaaaa\\n' \"$i\"; sleep 0.005; done",
        ]);

        E2eBuilder::new()
            .session("default")
            .seed_cmd(cmd)
            .viewport(200, 60)
            .run(|mut clients| async move {
                let client = &mut clients[0];

                // Monotonic both-axes shrink crossing the clamp boundary.
                let mut c: u16 = 200;
                let mut r: u16 = 60;
                while c > 1 || r > 1 {
                    c = c.saturating_sub(7).max(1);
                    r = r.saturating_sub(3).max(1);
                    client.resize_raw(c, r).await;
                }
                // Several rounds of hitting the 1x1 floor then growing a
                // touch and shrinking again — the boundary churn.
                for _ in 0..20 {
                    client.resize_raw(1, 1).await;
                    client.resize_raw(4, 2).await;
                    client.resize_raw(1, 3).await;
                    client.resize_raw(2, 1).await;
                }

                // Recover to a readable size. The seed pane emits forever,
                // so liveness is "fresh output still arrives" — a panicked
                // or wedged actor produces nothing and this `wait_until`
                // times out. (A crash mid-storm would also surface as a
                // server error at teardown.)
                client.resize(100, 30).await;
                let res = client.wait_until(|s| s.contains("row-")).await;
                // Snapshot the oracle WITHOUT `screenshot()`: the seed emits
                // every 5ms, so screenshot's "drain until 20ms quiet" never
                // terminates here (the pre-existing hang). wait_until already
                // populated the oracle; a bounded drain refreshes it.
                client.drain_output_bounded(32).await;
                cap.attach_screen(client.snapshot_text());
                assert!(
                    res.is_ok(),
                    "pane produced no output after the both-shrink storm — \
                     actor likely wedged or crashed; screen=\n{}",
                    res.unwrap_err(),
                );
            })
            .await;
    });
}
