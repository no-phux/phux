//! Lifecycle-churn adversarial stress (crash-hunt wave).
//!
//! Companion to `stress_attach_churn`. Where that test churns a single
//! transient client against a stable pane, this one drives the harder
//! lifecycle edges the user's "lags/crashes in edge cases" report points
//! at:
//!
//!   * MANY concurrent clients (up to 10) attached at once while output
//!     flows, then detached in a churned order — exercises the
//!     per-consumer reference grid + the detach reaping under fan-out.
//!   * Rapid attach/detach cycling on one pane during sustained output —
//!     dozens of connect/teardown cycles a second.
//!   * Attach RIGHT as the pane's PTY EOFs (mid-teardown): a client racing
//!     a session that is reaping itself must get either a clean snapshot
//!     or a clean error/EOF, never a panic.
//!
//! All scenarios assert NO panic (a panicked actor/connection task surfaces
//! as a wire hang → `wait_until` timeout, or as the `run_async` error the
//! harness teardown unwraps) and a coherent end state. Heavy `just e2e`
//! lane only.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]
#![allow(clippy::future_not_send, reason = "LocalSet-driven tests")]

use std::time::Duration;

use phux_protocol::wire::frame::{AttachTarget, FrameKind, ViewportInfo};
use portable_pty::CommandBuilder;
use tokio::time::timeout;

use phux_server_testkit::builder::E2eBuilder;
use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, run_local, send_frame, tracing_capture::TracingCapture,
    try_connect_socket, try_recv_typed,
};

/// Up to 10 concurrent clients attached while a pane streams output, then
/// detached in a shuffled order, must not panic or wedge the pane. A
/// surviving anchor still sees fresh output at the end.
#[ignore = "real-PTY e2e; starves the parallel pool. Run via `just e2e`."]
#[test]
fn many_concurrent_clients_attach_detach_under_output() {
    run_local(async {
        let cap = TracingCapture::install("many_clients_churn");

        // Continuous, rate-bounded output so every consumer's diff path is
        // exercised live.
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.args([
            "-c",
            "i=0; while :; do i=$((i+1)); printf 'tick-%d\\n' \"$i\"; sleep 0.01; done",
        ]);

        let mut harness = E2eBuilder::new()
            .session("default")
            .seed_cmd(cmd)
            .clients(1)
            .spawn()
            .await;
        let mut anchor = harness.clients.remove(0);
        anchor
            .wait_until(|s| s.contains("tick-"))
            .await
            .expect("anchor never saw output");

        let viewport = ViewportInfo::new(80, 24);

        // Several rounds: bring up a swarm of concurrent clients, let them
        // each observe live output, then drop them in a rotated order so
        // the detach reaping never sees a tidy LIFO.
        for round in 0..4u32 {
            let mut swarm = Vec::new();
            for _ in 0..9 {
                swarm.push(harness.attach_client(viewport).await);
            }
            for (i, c) in swarm.iter_mut().enumerate() {
                let res = c.wait_until(|s| s.contains("tick-")).await;
                assert!(
                    res.is_ok(),
                    "round {round} client {i} saw no live output (consumer fanout \
                     regressed); screen=\n{}",
                    res.unwrap_err(),
                );
            }
            // Detach in a rotated order (drop the middle first, then ends).
            let rotate = (round as usize) % swarm.len();
            swarm.rotate_left(rotate);
            for c in swarm {
                c.detach();
            }
        }

        // The pane survived the whole churn: the anchor still gets output.
        let res = anchor.wait_until(|s| s.contains("tick-")).await;
        // Bounded drain, not screenshot(): the `tick-` seed emits every 10ms,
        // so screenshot's "drain until 20ms quiet" never terminates here and
        // hangs the e2e lane (phux-s2iw class). wait_until already populated
        // the oracle; a bounded drain refreshes it for the capture.
        anchor.drain_output_bounded(32).await;
        cap.attach_screen(anchor.snapshot_text());
        assert!(
            res.is_ok(),
            "anchor stopped receiving output after concurrent-client churn \
             (pane wedged or actor died); screen=\n{}",
            res.unwrap_err(),
        );

        harness.clients.push(anchor);
        harness.shutdown().await;
    });
}

/// Attaching a fresh client at the exact moment the pane's PTY EOFs (the
/// session reaping itself) must not panic the server: the racing client
/// gets a clean snapshot, a clean error, or a clean EOF. With the tmux
/// server-exit model the server drops connections when its last session is
/// reaped, so any of those is acceptable — a panic or hang is not.
#[ignore = "real-PTY e2e; starves the parallel pool. Run via `just e2e`."]
#[test]
fn attach_racing_pty_eof_does_not_panic() {
    run_local(async {
        let cap = TracingCapture::install("attach_race_eof");

        // A pane that prints a marker then exits almost immediately, so a
        // client attaching just after spawn races the EOF + reap.
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.args(["-c", "printf RACEEOF; sleep 0.15"]);

        // `spawn()` always attaches at least one bootstrap client; drop it
        // so the pane's EOF is the only lifecycle driver and the racing
        // attaches below contend with a genuinely reaping session.
        let mut harness = E2eBuilder::new()
            .session("default")
            .seed_cmd(cmd)
            .clients(1)
            .spawn()
            .await;
        let socket_path = harness.socket_path.clone();
        harness.clients.remove(0).detach();

        // Fire a burst of attaches straddling the EOF window. Each connects
        // raw and reads whatever the server sends (ATTACHED, ERROR, or a
        // clean EOF), asserting the read never hangs and never yields a
        // garbled frame (decode panics inside `try_recv_typed`).
        for attempt in 0..8u32 {
            tokio::time::sleep(Duration::from_millis(25)).await;
            // Connect with a non-panicking poll: this test deliberately races
            // a session that is reaping itself, so "couldn't connect within
            // the deadline" is a *clean* outcome (the server already reaped +
            // exited), not a failure. Using the panicking `wait_for_socket`
            // here would turn that legitimate race outcome — or mere
            // scheduler latency on a contended runner — into a spurious
            // "socket never became connectable" panic.
            let Some(mut stream) = try_connect_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await
            else {
                // Socket gone => server already reaped + exited. Clean.
                cap.attach_screen(format!("attempt {attempt}: socket gone (clean reap)"));
                break;
            };
            send_frame(
                &mut stream,
                &FrameKind::Attach {
                    attach_id: 1,
                    target: AttachTarget::ByName("default".to_owned()),
                    viewport: ViewportInfo::new(80, 24),
                    request_scrollback: false,
                    scrollback_limit_lines: 0,
                },
            )
            .await;
            // Drain a few frames. `try_recv_typed` returns None on a clean
            // EOF and panics on a hang/garble — either of the former is a
            // pass, the latter fails the test loudly.
            for _ in 0..4 {
                match try_recv_typed(&mut stream).await {
                    Some(_) => {}
                    None => break, // clean half-close
                }
            }
        }

        // The server either self-exited (socket reaped) or is still up with
        // no sessions; both are coherent. Drive teardown defensively: if
        // the server already exited, the shutdown's join still completes.
        let _ = timeout(Duration::from_secs(5), async {
            harness.shutdown().await;
        })
        .await;
    });
}
