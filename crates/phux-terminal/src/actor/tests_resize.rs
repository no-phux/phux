//! Resize and resync tests: terminal/PTY winsize propagation,
//! XTWINOPS size queries, debounced resync broadcasts, and resize
//! storm behavior.

use super::test_support::*;
use super::*;

/// A resize re-broadcasts a full snapshot (reset preamble plus prior
/// content) as a `Resync` carrying the new dims.
#[tokio::test]
async fn resize_rebroadcasts_grid_snapshot_for_phux_8v1() {
    on_local!(async {
        let bundle = TerminalActor::new_with_seed(80, 24, b"phux8v1-marker").expect("seed");
        let handle = bundle.handle.clone();
        let token = bundle.token;
        // Subscribe BEFORE the actor runs so we don't miss the
        // resize broadcast.
        let mut out = handle.output.subscribe();
        let join = tokio::task::spawn_local(bundle.actor.run());

        resize(&handle, grid_resize(40, 10)).await;

        let (dims, acc) = await_resync_with(&mut out, b"phux8v1-marker").await;
        assert_eq!(
            dims,
            Some((40, 10)),
            "resize resync must carry the post-reflow grid dims (phux-3ns5)",
        );
        assert_resync_bytes(&acc, b"phux8v1-marker");

        stop(&token, join).await;
    })
    .await;
}

/// Collect output until a resync has re-sent the snapshot preamble and
/// `marker`, or the deadline passes. Returns the last resync's grid and
/// every byte seen.
async fn await_resync_with(
    out: &mut tokio::sync::broadcast::Receiver<ResourceOutput>,
    marker: &[u8],
) -> (Option<(u16, u16)>, Vec<u8>) {
    let mut acc: Vec<u8> = Vec::new();
    let mut dims = None;
    let deadline = tokio::time::Instant::now() + ACTOR_EXIT_DEADLINE;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(DRAIN_POLL_TICK, out.recv()).await {
            Ok(Ok(ResourceOutput::Resync {
                cols, rows, bytes, ..
            })) => {
                dims = Some((cols, rows));
                acc.extend_from_slice(&bytes);
                if contains_subslice(&acc, b"\x1b[!p") && contains_subslice(&acc, marker) {
                    break;
                }
            }
            Ok(Ok(ResourceOutput::Live { bytes, .. })) => acc.extend_from_slice(&bytes),
            Ok(Ok(ResourceOutput::Control { .. })) => {}
            Ok(Err(_)) => break,
            Err(_) => tokio::task::yield_now().await,
        }
    }
    (dims, acc)
}

/// A resync re-sent the DECSTR snapshot preamble and the grid's `marker`.
fn assert_resync_bytes(acc: &[u8], marker: &[u8]) {
    let text = String::from_utf8_lossy(acc);
    assert!(
        contains_subslice(acc, b"\x1b[!p"),
        "no DECSTR preamble: {text:?}"
    );
    assert!(
        contains_subslice(acc, marker),
        "grid content not re-sent: {text:?}"
    );
}

type SharedMaster = std::sync::Arc<std::sync::Mutex<Box<dyn portable_pty::MasterPty + Send>>>;

/// The kernel winsize: `(cols, rows, pixel_width, pixel_height)`.
fn winsize(master: &SharedMaster) -> (u16, u16, u16, u16) {
    let got = master
        .lock()
        .expect("master lock")
        .get_size()
        .expect("size");
    (got.cols, got.rows, got.pixel_width, got.pixel_height)
}

/// Poll the kernel winsize until it reaches `want` (the resize mailbox is
/// async), failing after a bounded wait.
async fn wait_for_winsize(master: &SharedMaster, want: (u16, u16, u16, u16)) {
    for _ in 0..200 {
        if winsize(master) == want {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!(
        "winsize never reached {want:?}; kernel reports {:?}",
        winsize(master)
    );
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
