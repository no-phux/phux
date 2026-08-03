//! Attach/detach-churn stress test (e2e flywheel item 6).
//!
//! Models a flaky client (or a fleet of short-lived agents) repeatedly
//! attaching and dropping against a live session. The server must reap
//! each connection cleanly, keep the underlying pane alive across the
//! churn, and serve a correct snapshot to every fresh client. The failure
//! modes this guards against: a `ClientId` slot leak, a per-pane
//! subscriber list that grows without bound (broadcast capacity
//! eviction), or a panic during connection teardown.
//!
//! Built on [`E2eBuilder`] / [`Harness`]: one long-lived "anchor" client
//! holds the session open while a churn loop attaches a transient client,
//! observes the live pane via the snapshot, then drops it — repeatedly.
//! A unique marker printed once by the seed pane is asserted visible on
//! each fresh attach (the snapshot still reconstructs the grid after
//! churn) and on the anchor at the end (the pane survived).

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]
#![allow(clippy::future_not_send, reason = "LocalSet-driven tests")]

use phux_protocol::wire::frame::ViewportInfo;
use portable_pty::CommandBuilder;

use phux_server_testkit::builder::E2eBuilder;
use phux_server_testkit::run_local;
use phux_server_testkit::tracing_capture::TracingCapture;

/// Rapid attach→observe→detach churn must not panic, leak client slots, or
/// kill the pane. A survivor (anchor) client sees correct state at the end.
#[ignore = "real-PTY e2e; starves the parallel pool. Run via `just e2e`."]
#[test]
fn attach_detach_churn_keeps_pane_alive() {
    // A pane that prints a stable marker once, then idles. Every fresh
    // attach must reconstruct the marker from the snapshot, proving the
    // grid survives the churn.
    const MARKER: &str = "CHURNMARKER";
    run_local(async {
        let cap = TracingCapture::install("attach_churn");

        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.args(["-c", "printf CHURNMARKER; sleep 30"]);

        // One anchor client keeps the session open across the churn.
        let mut harness = E2eBuilder::new()
            .session("default")
            .seed_cmd(cmd)
            .clients(1)
            .spawn()
            .await;

        // Take the anchor out of the harness so we can drive it directly;
        // the harness retains the shutdown handles.
        let mut anchor = harness.clients.remove(0);

        // Anchor sees the marker (it lands as a live delta after attach).
        let res = anchor.wait_until(|s| s.contains(MARKER)).await;
        cap.attach_screen(anchor.screenshot().await.snapshot_text());
        assert!(
            res.is_ok(),
            "anchor never saw the seed marker; screen=\n{}",
            res.unwrap_err(),
        );

        // Churn: attach a transient client, confirm it reconstructs the
        // marker from its snapshot/live stream, then drop it. Repeat.
        let viewport = ViewportInfo::new(80, 24);
        for round in 0..12u32 {
            let mut transient = harness.attach_client(viewport).await;
            let res = transient.wait_until(|s| s.contains(MARKER)).await;
            cap.attach_screen(transient.screenshot().await.snapshot_text());
            assert!(
                res.is_ok(),
                "churn round {round}: fresh client did not see the marker \
                 (snapshot reconstruction or subscriber fanout regressed); \
                 screen=\n{}",
                res.unwrap_err(),
            );
            // Hard detach: drop the stream. The server must reap it.
            transient.detach();
        }

        // The pane survived the churn: the anchor still shows the marker
        // and the connection is still live.
        let still = anchor.screenshot().await.contains(MARKER);
        cap.attach_screen(anchor.screenshot().await.snapshot_text());
        assert!(
            still,
            "anchor lost the pane content after attach/detach churn",
        );

        // Put the anchor back so the harness drops every stream on shutdown.
        harness.clients.push(anchor);
        harness.shutdown().await;
    });
}
