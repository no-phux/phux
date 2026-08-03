//! Wall-clock latency gate (e2e flywheel item 3).
//!
//! The existing perf gate ([`perf_bursty_output`]) counts *allocations*
//! per synthesis tick — a regression detector for the diff hot path. This
//! is its wall-clock sibling: it drives a representative burst against the
//! REAL server over the wire and asserts the end-to-end time from input to
//! screen-settle stays under a generous ceiling. Same philosophy as the
//! alloc gate: a coarse regression tripwire, not a microbenchmark. The
//! ceiling has wide headroom over the measured time on this machine so it
//! is not flaky across allocator/scheduler differences and under the
//! contended nextest pool.
//!
//! Two scenarios:
//!   1. Heavy colored output (the bursty-output repro shape) drained to
//!      settle on a single client.
//!   2. A 2-client attach: both clients converge on the same burst, and
//!      the slower client's time-to-settle is gated (the fanout must not
//!      starve a second subscriber).
//!
//! `time-to-settle` is measured by [`ClientHandle::converge`]: the
//! duration from the first drained byte to the screen going idle for the
//! convergence window. It captures input→render→settle, the latency a
//! user actually feels.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]
#![allow(clippy::future_not_send, reason = "LocalSet-driven tests")]
#![allow(
    clippy::print_stderr,
    reason = "perf gate prints the measured latency for triage on failure"
)]

use std::time::Duration;

use portable_pty::CommandBuilder;

use phux_server_testkit::builder::{DEFAULT_IDLE_MS, E2eBuilder};
use phux_server_testkit::run_local;
use phux_server_testkit::tracing_capture::TracingCapture;

/// Time-to-settle ceiling for a single heavy-output burst. Measured on
/// this machine (M-series, nix devshell) at ~330-360 ms end-to-end (first
/// byte to screen-idle) including the contended nextest pool; the 8s
/// ceiling is ~22x headroom so scheduler jitter never trips it. A real
/// regression (e.g. the per-consumer diff going quadratic, or the
/// broadcast pump stalling) blows past it by orders of magnitude. Settle
/// time is measured from the first byte, so the idle convergence window
/// ([`DEFAULT_IDLE_MS`]) is not counted as latency.
const SETTLE_CEILING: Duration = Duration::from_secs(8);

/// A burst of heavy colored output: 40 rows, each a distinct 256-color
/// foreground with churn chunks. Mirrors the alloc-gate's `write_burst`
/// shape (zsh completion menu / syntax-highlighted scroll). Printed by the
/// seed pane on a short delay so the clients are attached first and the
/// burst lands as a live delta they must drain.
fn burst_command() -> CommandBuilder {
    // Build a shell one-liner that emits the colored burst after a brief
    // settle, then idles so the connection stays open for teardown.
    let mut script = String::new();
    script.push_str("sleep 0.3; ");
    // 60 repaints of a 40-row colored screen: enough churn to be a real
    // burst, fast enough to finish well within the ceiling.
    script.push_str("for g in $(seq 1 60); do ");
    script.push_str("printf '\\033[H'; ");
    script.push_str("for r in $(seq 1 40); do ");
    script.push_str("printf '\\033[38;5;%dmrow %02d g%d colored-chunk colored-chunk\\r\\n' ");
    script.push_str("$((16 + r % 200)) $r $g; ");
    script.push_str("done; ");
    script.push_str("done; ");
    script.push_str("printf 'BURSTDONE'; sleep 30");

    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.args(["-c", &script]);
    cmd
}

/// Single-client heavy-burst latency gate.
#[ignore = "real-PTY e2e; starves the parallel pool. Run via `just e2e`."]
#[test]
fn single_client_heavy_burst_settles_under_ceiling() {
    run_local(async {
        let cap = TracingCapture::install("latency_single");

        E2eBuilder::new()
            .session("default")
            .seed_cmd(burst_command())
            .viewport(80, 40)
            .run(|mut clients| async move {
                let client = &mut clients[0];
                // Converge: drain until the screen is idle for the window.
                // The returned duration is first-byte→settle.
                let settle = client.converge(DEFAULT_IDLE_MS).await;
                cap.attach_screen(client.screenshot().await.snapshot_text());

                eprintln!(
                    "perf_latency[single]: time-to-settle = {} ms (ceiling {} ms)",
                    settle.as_millis(),
                    SETTLE_CEILING.as_millis(),
                );
                assert!(
                    client.screenshot().await.contains("BURSTDONE"),
                    "burst never completed; screen=\n{}",
                    client.screenshot().await.snapshot_text(),
                );
                assert!(
                    settle <= SETTLE_CEILING,
                    "single-client time-to-settle {} ms exceeded ceiling {} ms; \
                     a wall-clock latency regression in the emit/diff path",
                    settle.as_millis(),
                    SETTLE_CEILING.as_millis(),
                );
            })
            .await;
    });
}

/// Two-client attach: the slower subscriber's time-to-settle is gated.
/// Fanout must not starve the second client.
#[ignore = "real-PTY e2e; starves the parallel pool. Run via `just e2e`."]
#[test]
fn two_client_burst_slowest_settles_under_ceiling() {
    run_local(async {
        let cap = TracingCapture::install("latency_multi");

        E2eBuilder::new()
            .session("default")
            .seed_cmd(burst_command())
            .viewport(80, 40)
            .clients(2)
            .run(|mut clients| async move {
                // Drive both to settle concurrently so the measurement
                // reflects shared-fanout contention, not serial draining.
                let (a, rest) = clients.split_first_mut().expect("two clients");
                let b = &mut rest[0];
                let (settle_a, settle_b) =
                    tokio::join!(a.converge(DEFAULT_IDLE_MS), b.converge(DEFAULT_IDLE_MS));
                let slowest = settle_a.max(settle_b);

                cap.attach_screen(a.screenshot().await.snapshot_text());

                eprintln!(
                    "perf_latency[multi]: settle A={} ms B={} ms slowest={} ms (ceiling {} ms)",
                    settle_a.as_millis(),
                    settle_b.as_millis(),
                    slowest.as_millis(),
                    SETTLE_CEILING.as_millis(),
                );
                assert!(
                    a.screenshot().await.contains("BURSTDONE")
                        && b.screenshot().await.contains("BURSTDONE"),
                    "both clients must observe the full burst (fanout completeness)",
                );
                assert!(
                    slowest <= SETTLE_CEILING,
                    "two-client slowest time-to-settle {} ms exceeded ceiling {} ms; \
                     fanout starved a subscriber",
                    slowest.as_millis(),
                    SETTLE_CEILING.as_millis(),
                );
            })
            .await;
    });
}
