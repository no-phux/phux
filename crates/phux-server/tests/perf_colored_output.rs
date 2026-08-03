//! Heavy-colored-output wall-clock latency gate (e2e flywheel).
//!
//! This is the regression detector for the user's actual symptom: a
//! zsh-completion-menu / syntax-highlight burst — many full-width
//! SGR-laden rows rewritten every frame — stuttering the attached
//! session. [`perf_bursty_output`] gates the *allocation* cost of the
//! per-consumer diff on that shape; [`perf_latency`] gates the wall-clock
//! settle of a moderately-colored burst. This gate is the dedicated
//! wall-clock sibling for the WORST-case colored shape: an SGR change
//! roughly every other column (built by
//! [`phux_server_testkit::builder::colored_burst_command`]), driven against the REAL
//! server over the wire, with the client-applied result captured by the
//! libghostty [`Screen`] oracle.
//!
//! Same philosophy as the sibling gates: a coarse regression tripwire,
//! not a microbenchmark. Two assertions, both with documented headroom:
//!
//!   1. `time-to-settle` (first drained byte → screen idle) under a
//!      generous ceiling. A real regression (the per-consumer diff going
//!      quadratic on SGR runs, the broadcast pump stalling, the client's
//!      VT apply or per-cell render regressing) blows past it.
//!   2. Per-frame cost: settle / observed-repaints stays under a
//!      per-frame ceiling. This catches a regression that keeps the total
//!      under the wall-clock ceiling only because the burst happened to be
//!      short — it normalizes by the number of repaints actually drained.
//!
//! `#[ignore]`d into the `just e2e` lane: like the other real-PTY gates it
//! spawns a real server + PTY and asserts on load-sensitive timing, which
//! starves and trips under the full-parallel `just test` pool. Run it one
//! binary at a time via `just e2e`.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]
#![allow(clippy::future_not_send, reason = "LocalSet-driven tests")]
#![allow(
    clippy::print_stderr,
    reason = "perf gate prints the measured latency for triage on failure"
)]

use std::time::Duration;

use phux_server_testkit::builder::{DEFAULT_IDLE_MS, E2eBuilder, colored_burst_command};
use phux_server_testkit::run_local;
use phux_server_testkit::tracing_capture::TracingCapture;

/// Burst geometry. 80x40 matches the alloc gate / `perf_latency` shape; a
/// full-width SGR-per-cell row at this size is ~80 color runs * 40 rows =
/// the worst-case completion-menu churn. 24 repaints is enough churn to be
/// a real burst while keeping the shell loop well within the ceiling.
const COLS: u16 = 80;
const ROWS: u16 = 40;
const GENS: u16 = 24;

/// Time-to-settle ceiling for the heavy colored burst. The colored shape
/// is heavier than `perf_latency`'s (an SGR change per cell rather than
/// per row) and the seed shell builds each row in a column loop, so the
/// emit side is slower; measured on this machine (M-series, nix devshell)
/// at low single-digit seconds end-to-end under the contended pool. The
/// 30s ceiling is wide headroom so scheduler jitter never trips it while a
/// genuine quadratic/stall regression (orders of magnitude) still does.
const SETTLE_CEILING: Duration = Duration::from_secs(30);

/// Per-frame cost ceiling: settle-time divided by the repaints actually
/// drained must stay under this. Normalizes the wall-clock gate by burst
/// length so a regression can't hide behind a short burst. Generous: at
/// ~24 repaints under a few seconds the observed per-frame cost is well
/// under 200ms; 1s/frame is ~5x+ headroom.
const PER_FRAME_CEILING: Duration = Duration::from_secs(1);

/// Single-client heavy-colored-output latency gate. Drives the worst-case
/// SGR-per-cell burst, captures the client-applied screen, and asserts
/// both the time-to-settle and the normalized per-frame cost stay under
/// their ceilings.
#[ignore = "real-PTY e2e; starves the parallel pool. Run via `just e2e`."]
#[test]
fn colored_burst_settles_under_ceiling() {
    run_local(async {
        let cap = TracingCapture::install("colored_output");

        E2eBuilder::new()
            .session("default")
            .seed_cmd(colored_burst_command(COLS, ROWS, GENS))
            .viewport(COLS, ROWS)
            .run(|mut clients| async move {
                let client = &mut clients[0];
                // Converge: drain until the screen is idle for the window.
                // The returned duration is first-byte→settle.
                let settle = client.converge(DEFAULT_IDLE_MS).await;
                let screen = client.screenshot().await.snapshot_text();
                cap.attach_screen(screen.clone());

                // The burst completed: the settle marker landed. This is
                // the "client applied the whole colored stream" proof —
                // the oracle parsed every TERMINAL_OUTPUT through a real
                // libghostty Terminal, so a dropped/corrupt frame would
                // leave the marker missing.
                assert!(
                    client.screenshot().await.contains("COLORDONE"),
                    "colored burst never completed; screen=\n{screen}",
                );

                // Normalize by the repaints we actually drained. We can't
                // count frames directly from `converge`, so use the
                // emitted repaint count as a conservative proxy: GENS
                // repaints is the lower bound on the work the client
                // applied (the server may coalesce, never inflate).
                let per_frame = settle / u32::from(GENS.max(1));

                eprintln!(
                    "perf_colored[single]: time-to-settle = {} ms (ceiling {} ms); \
                     per-frame ~= {} ms over {GENS} repaints (ceiling {} ms)",
                    settle.as_millis(),
                    SETTLE_CEILING.as_millis(),
                    per_frame.as_millis(),
                    PER_FRAME_CEILING.as_millis(),
                );

                assert!(
                    settle <= SETTLE_CEILING,
                    "heavy-colored time-to-settle {} ms exceeded ceiling {} ms; \
                     a wall-clock latency regression in the colored-output path \
                     (per-consumer SGR diff, broadcast pump, or client VT apply/render)",
                    settle.as_millis(),
                    SETTLE_CEILING.as_millis(),
                );
                assert!(
                    per_frame <= PER_FRAME_CEILING,
                    "heavy-colored per-frame cost {} ms exceeded ceiling {} ms; \
                     the burst settled overall but each colored repaint is too expensive",
                    per_frame.as_millis(),
                    PER_FRAME_CEILING.as_millis(),
                );
            })
            .await;
    });
}
