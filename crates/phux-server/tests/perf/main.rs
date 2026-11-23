//! Wall-clock perf gates, `#[ignore]`d into `just e2e` (real PTYs and
//! load-sensitive timing starve the parallel pool). Coarse regression
//! tripwires with wide headroom, not microbenchmarks: time-to-settle is
//! first drained byte to screen idle, so a quadratic diff or a stalled
//! broadcast pump blows past the ceilings by orders of magnitude.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]
#![allow(clippy::future_not_send, reason = "LocalSet-driven tests")]
#![allow(
    clippy::print_stderr,
    reason = "gates print the measurement for triage"
)]

use std::time::Duration;

use phux_server_testkit::builder::{
    DEFAULT_IDLE_MS, E2eBuilder, colored_burst_bytes, colored_burst_command,
};
use phux_server_testkit::run_local;
use phux_server_testkit::tracing_capture::TracingCapture;
use std::io::Write as _;

/// Measured ~330-360 ms on an M-series laptop under the contended pool.
const MULTI_CLIENT_CEILING: Duration = Duration::from_secs(8);

/// 60 repaints of a 40-row screen, one 256-color foreground per row,
/// emitted after the clients attach.
fn row_colored_burst() -> Vec<u8> {
    let mut out = Vec::new();
    for g in 1..=60 {
        out.extend_from_slice(b"\x1b[H");
        for r in 1..=40 {
            write!(
                out,
                "\x1b[38;5;{}mrow {r:02} g{g} colored-chunk colored-chunk\r\n",
                16 + r
            )
            .expect("vec write");
        }
    }
    out.extend_from_slice(b"BURSTDONE");
    out
}

/// Two clients drain one burst concurrently; the slower one's settle is
/// gated, so fanout must not starve a subscriber.
#[ignore = "real-PTY e2e; starves the parallel pool. Run via `just e2e`."]
#[test]
fn two_client_burst_slowest_settles_under_ceiling() {
    let dir = tempfile::TempDir::new().expect("burst dir");
    let burst = dir.path().join("burst.bin");
    let gate = dir.path().join("gate");
    std::fs::write(&burst, row_colored_burst()).expect("write burst");
    run_local(async {
        let cap = TracingCapture::install("latency_multi");
        E2eBuilder::new()
            .seed_cmd(colored_burst_command(&burst, &gate))
            .viewport(80, 40)
            .clients(2)
            .run(move |mut clients| async move {
                let [a, b] = clients.as_mut_slice() else {
                    unreachable!()
                };
                std::fs::write(&gate, b"").expect("open burst gate after both clients attach");
                let complete =
                    |screen: &mut phux_server_testkit::screen::Screen| screen.contains("BURSTDONE");
                let (settle_a, settle_b) = tokio::join!(
                    a.converge_until_with_timeout(DEFAULT_IDLE_MS, MULTI_CLIENT_CEILING, complete),
                    b.converge_until_with_timeout(DEFAULT_IDLE_MS, MULTI_CLIENT_CEILING, complete)
                );
                let slowest = settle_a.max(settle_b);
                cap.attach_screen(a.screenshot().await.snapshot_text());
                eprintln!("perf[multi]: settle A={settle_a:?} B={settle_b:?}");
                assert!(
                    a.screenshot().await.contains("BURSTDONE")
                        && b.screenshot().await.contains("BURSTDONE"),
                    "both clients must observe the full burst",
                );
                assert!(
                    slowest <= MULTI_CLIENT_CEILING,
                    "slowest time-to-settle {slowest:?} exceeded {MULTI_CLIENT_CEILING:?}",
                );
            })
            .await;
    });
}

/// Worst-case colored shape: an SGR change per cell, 80x40, 24 repaints,
/// `cat`'d from precomputed bytes so the emitter's cost does not track host
/// load. Settle is normalized per producer repaint so a regression cannot
/// hide behind a short burst.
#[ignore = "real-PTY e2e; starves the parallel pool. Run via `just e2e`."]
#[test]
fn colored_burst_settles_under_ceiling() {
    const COLS: u16 = 80;
    const ROWS: u16 = 40;
    const GENS: u16 = 24;
    const PER_REPAINT_CEILING: Duration = Duration::from_secs(1);
    let ceiling = PER_REPAINT_CEILING * u32::from(GENS);

    let dir = tempfile::TempDir::new().expect("burst dir");
    let burst = dir.path().join("burst.bin");
    let gate = dir.path().join("gate");
    std::fs::write(&burst, colored_burst_bytes(COLS, ROWS, GENS)).expect("write burst");
    let cmd = colored_burst_command(&burst, &gate);

    run_local(async move {
        let cap = TracingCapture::install("colored_output");
        E2eBuilder::new()
            .seed_cmd(cmd)
            .viewport(COLS, ROWS)
            .run(move |mut clients| async move {
                std::fs::write(&gate, b"").expect("open burst gate");
                let client = &mut clients[0];
                // Marker-gated: a quiet gap mid-burst is not settle.
                let settle = client
                    .converge_until_with_timeout(DEFAULT_IDLE_MS, ceiling, |s| {
                        s.contains("COLORDONE")
                    })
                    .await;
                let screen = client.screenshot().await.snapshot_text();
                cap.attach_screen(screen.clone());
                eprintln!("perf[colored]: settle={settle:?} over {GENS} repaints");
                assert!(screen.contains("COLORDONE"), "burst never completed:\n{screen}");
                assert!(
                    settle <= ceiling,
                    "colored time-to-settle {settle:?} exceeded {ceiling:?} ({PER_REPAINT_CEILING:?}/repaint)",
                );
            })
            .await;
    });
    drop(dir);
}
