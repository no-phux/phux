//! Client churn against a live pane: the server must reap every connection,
//! keep the pane alive, and serve correct state to each fresh client (no
//! client-slot leak, unbounded subscriber list, or teardown panic).

use std::time::Duration;

use phux_protocol::wire::frame::ViewportInfo;
use portable_pty::CommandBuilder;

use phux_server_testkit::builder::E2eBuilder;
use phux_server_testkit::tracing_capture::TracingCapture;
use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, attach_by_name, run_local, send_frame, try_connect_socket,
    try_recv_typed,
};

fn sh(script: &str) -> CommandBuilder {
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.args(["-c", script]);
    cmd
}

/// Twelve attach/observe/drop rounds: each fresh client must rebuild a
/// once-printed marker from its snapshot, and the anchor keeps the pane.
#[ignore = "real-PTY e2e; starves the parallel pool. Run via `just stress`."]
#[test]
fn attach_detach_churn_keeps_pane_alive() {
    run_local(async {
        let cap = TracingCapture::install("attach_churn");
        let mut harness = E2eBuilder::new()
            .seed_cmd(sh("printf CHURNMARKER; sleep 30"))
            .spawn()
            .await;
        let mut anchor = harness.clients.remove(0);
        anchor
            .wait_until(|s| s.contains("CHURNMARKER"))
            .await
            .expect("anchor never saw the seed marker");

        for round in 0..12u32 {
            let mut transient = harness.attach_client(ViewportInfo::new(80, 24)).await;
            let res = transient.wait_until(|s| s.contains("CHURNMARKER")).await;
            cap.attach_screen(transient.screenshot().await.snapshot_text());
            res.unwrap_or_else(|screen| {
                panic!("round {round}: fresh client lost the marker:\n{screen}")
            });
            transient.detach();
        }
        assert!(
            anchor.screenshot().await.contains("CHURNMARKER"),
            "anchor lost the pane"
        );

        harness.clients.push(anchor);
        harness.shutdown().await;
    });
}

/// Swarms of nine concurrent clients under live output, dropped in rotated
/// order; the anchor still receives output afterwards.
#[ignore = "real-PTY e2e; starves the parallel pool. Run via `just stress`."]
#[test]
fn many_concurrent_clients_attach_detach_under_output() {
    run_local(async {
        let cap = TracingCapture::install("many_clients_churn");
        let mut harness = E2eBuilder::new()
            .seed_cmd(sh(
                "i=0; while :; do i=$((i+1)); printf 'tick-%d\\n' \"$i\"; sleep 0.01; done",
            ))
            .spawn()
            .await;
        let mut anchor = harness.clients.remove(0);
        anchor
            .wait_until(|s| s.contains("tick-"))
            .await
            .expect("anchor output");

        for round in 0..4usize {
            let mut swarm = Vec::new();
            for _ in 0..9 {
                swarm.push(harness.attach_client(ViewportInfo::new(80, 24)).await);
            }
            for (i, client) in swarm.iter_mut().enumerate() {
                client
                    .wait_until(|s| s.contains("tick-"))
                    .await
                    .unwrap_or_else(|screen| {
                        panic!("round {round} client {i}: no live output:\n{screen}")
                    });
            }
            swarm.rotate_left(round);
            for client in swarm {
                client.detach();
            }
        }
        let res = anchor.wait_until(|s| s.contains("tick-")).await;
        // Bounded drain: the seed never goes quiet, so `screenshot()` could spin.
        anchor.drain_output_bounded(32).await;
        cap.attach_screen(anchor.snapshot_text());
        res.expect("anchor stopped receiving output after churn");

        harness.clients.push(anchor);
        harness.shutdown().await;
    });
}

/// Attaches racing the pane's EOF and the server's self-exit must each end
/// in a snapshot, an error, or a clean EOF: never a hang or a garbled frame.
#[ignore = "real-PTY e2e; starves the parallel pool. Run via `just stress`."]
#[test]
fn attach_racing_pty_eof_does_not_panic() {
    run_local(async {
        let mut harness = E2eBuilder::new()
            .seed_cmd(sh("printf RACEEOF; sleep 0.15"))
            .spawn()
            .await;
        let socket_path = harness.socket_path.clone();
        harness.clients.remove(0).detach();

        for _ in 0..8 {
            tokio::time::sleep(Duration::from_millis(25)).await;
            // A socket that is already gone is a clean reap, not a failure.
            let Some(mut stream) = try_connect_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await
            else {
                break;
            };
            send_frame(&mut stream, &attach_by_name("default")).await;
            for _ in 0..4 {
                if try_recv_typed(&mut stream).await.is_none() {
                    break;
                }
            }
        }
        let _ = tokio::time::timeout(Duration::from_secs(5), harness.shutdown()).await;
    });
}
