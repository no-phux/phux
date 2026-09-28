//! Resize storms against a live PTY: a panicked actor surfaces as a
//! `wait_until` timeout or as the `run_async` error the teardown unwraps.

use portable_pty::CommandBuilder;

use phux_server_testkit::builder::E2eBuilder;
use phux_server_testkit::run_local;
use phux_server_testkit::tracing_capture::TracingCapture;

fn sh(script: &str) -> CommandBuilder {
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.args(["-c", script]);
    cmd
}

/// Sane drag sizes then degenerate ones (`0x0`, 1-cell, 1000x1000), sent
/// back to back, must leave the PTY at the last requested geometry.
#[ignore = "real-PTY e2e; starves the parallel pool. Run via `just stress`."]
#[test]
fn resize_storm_with_degenerate_viewports_converges_to_final_geometry() {
    run_local(async {
        let cap = TracingCapture::install("resize_storm");
        E2eBuilder::new()
            .seed_cmd(sh("while :; do stty size; sleep 0.02; done"))
            .run(|mut clients| async move {
                let client = &mut clients[0];
                let storm = [
                    (100, 30),
                    (120, 40),
                    (90, 50),
                    (140, 35),
                    (0, 0),
                    (1, 1),
                    (1, 200),
                    (200, 1),
                    (0, 0),
                    (1000, 1000),
                    (1, 1),
                    (3, 3),
                ];
                for (cols, rows) in storm {
                    client.resize(cols, rows).await;
                }
                client.resize(128, 42).await;
                // `stty size` prints `<rows> <cols>`.
                let res = client.wait_until(|s| s.contains("42 128")).await;
                // Bounded drain: the seed never goes quiet, so `screenshot()` could spin.
                client.drain_output_bounded(32).await;
                cap.attach_screen(client.snapshot_text());
                res.expect("PTY winsize never converged after the storm");
            })
            .await;
    });
}

/// Monotonic both-axes shrink across the 1-cell clamp, then boundary churn,
/// while output floods the grid: the `PageList.resizeCols` both-shrink case.
#[ignore = "real-PTY e2e; starves the parallel pool. Run via `just stress`."]
#[test]
fn both_axes_shrink_storm_under_output_does_not_panic() {
    run_local(async {
        let cap = TracingCapture::install("both_axes_shrink_storm");
        // Rate-bounded so the flood cannot starve the attach handshake.
        let seed = sh(
            "i=0; while :; do i=$((i+1)); printf 'row-%d-aaaaaaaaaaaaaaaaaaaa\\n' \"$i\"; sleep 0.005; done",
        );
        E2eBuilder::new()
            .seed_cmd(seed)
            .viewport(200, 60)
            .run(|mut clients| async move {
                let client = &mut clients[0];
                let (mut cols, mut rows) = (200u16, 60u16);
                while cols > 1 || rows > 1 {
                    cols = cols.saturating_sub(7).max(1);
                    rows = rows.saturating_sub(3).max(1);
                    client.resize(cols, rows).await;
                }
                for _ in 0..20 {
                    for (cols, rows) in [(1, 1), (4, 2), (1, 3), (2, 1)] {
                        client.resize(cols, rows).await;
                    }
                }
                // Liveness: fresh output still arrives after recovering.
                client.resize(100, 30).await;
                let res = client.wait_until(|s| s.contains("row-")).await;
                client.drain_output_bounded(32).await;
                cap.attach_screen(client.snapshot_text());
                res.expect("pane produced no output after the shrink storm");
            })
            .await;
    });
}
