//! Resize and resync tests: terminal/PTY winsize propagation,
//! XTWINOPS size queries, debounced resync broadcasts, and resize
//! storm behavior.

use super::test_support::*;
use super::*;

/// A resize re-broadcasts a full snapshot (reset preamble plus prior
/// content) as a `Resync` carrying the new dims.
#[tokio::test]
async fn resize_rebroadcasts_grid_snapshot_for_phux_8v1() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let bundle = TerminalActor::new_with_seed(80, 24, b"phux8v1-marker").expect("seed");
            let handle = bundle.handle.clone();
            let token = bundle.token;
            // Subscribe BEFORE the actor runs so we don't miss the
            // resize broadcast.
            let mut out = handle.output.subscribe();
            let join = tokio::task::spawn_local(bundle.actor.run());

            handle
                .terminal()
                .expect("terminal facet")
                .resize
                .send(ResizeRequest {
                    cols: 40,
                    rows: 10,
                    cell_px: None,
                    resync_clients: true,
                    resync_only: false,
                    resync_for: None,
                })
                .await
                .expect("send resize");

            // Collect broadcast bytes and the resync's dims.
            let mut acc: Vec<u8> = Vec::new();
            let mut resync_dims: Option<(u16, u16)> = None;
            let deadline = tokio::time::Instant::now() + ACTOR_EXIT_DEADLINE;
            while tokio::time::Instant::now() < deadline {
                match tokio::time::timeout(DRAIN_POLL_TICK, out.recv()).await {
                    Ok(Ok(ResourceOutput::Resync {
                        cols, rows, bytes, ..
                    })) => {
                        resync_dims = Some((cols, rows));
                        acc.extend_from_slice(&bytes);
                        if contains_subslice(&acc, b"\x1b[!p")
                            && contains_subslice(&acc, b"phux8v1-marker")
                        {
                            break;
                        }
                    }
                    Ok(Ok(ResourceOutput::Live { bytes, .. })) => acc.extend_from_slice(&bytes),
                    Ok(Ok(ResourceOutput::Control { .. })) => {}
                    Ok(Err(_)) => break,                      // channel closed
                    Err(_) => tokio::task::yield_now().await, // poll tick
                }
            }
            assert_eq!(
                resync_dims,
                Some((40, 10)),
                "resize resync must carry the post-reflow grid dims (phux-3ns5)",
            );

            assert!(
                contains_subslice(&acc, b"\x1b[!p"),
                "resize broadcast missing DECSTR snapshot preamble; got {:?}",
                String::from_utf8_lossy(&acc),
            );
            assert!(
                contains_subslice(&acc, b"phux8v1-marker"),
                "resize broadcast did not re-send pre-resize grid content; got {:?}",
                String::from_utf8_lossy(&acc),
            );

            token.cancel();
            tokio::time::timeout(ACTOR_EXIT_DEADLINE, join)
                .await
                .expect("actor did not exit after cancel")
                .expect("actor task panicked");
        })
        .await;
}

/// Advance paused time past the resync debounce, yielding first so the
/// actor has armed it.
async fn settle_past_resync_debounce() {
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(RESIZE_RESYNC_DEBOUNCE * 4).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

/// Drain every `ResourceOutput::Resync` currently queued on `out`, returning
/// the grid each one carried. Live output and lag drops are not resyncs.
fn drain_resync_dims(
    out: &mut tokio::sync::broadcast::Receiver<ResourceOutput>,
) -> Vec<(u16, u16)> {
    let mut dims = Vec::new();
    loop {
        match out.try_recv() {
            Ok(ResourceOutput::Resync { cols, rows, .. }) => dims.push((cols, rows)),
            Ok(ResourceOutput::Live { .. } | ResourceOutput::Control { .. })
            | Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {}
            Err(_) => break,
        }
    }
    dims
}

mod geometry;
mod pty_geometry;
mod resync;
mod storms;
