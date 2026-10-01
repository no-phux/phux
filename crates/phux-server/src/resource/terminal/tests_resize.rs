//! Resize and resync tests: terminal/PTY winsize propagation,
//! XTWINOPS size queries, debounced resync broadcasts, and resize
//! storm behavior.

use super::test_support::*;
use super::*;

/// More geometry updates than the bounded mailbox can hold still apply
/// the final grid and PTY size before the attach snapshot is captured.
#[tokio::test(flavor = "current_thread")]
async fn saturated_geometry_applies_final_size_before_snapshot() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let bundle = TerminalActor::new_with_command(CommandBuilder::new("/bin/cat"), 80, 24)
                .expect("spawn");
            let master = std::sync::Arc::clone(&bundle.actor.pty.as_ref().expect("pty").master);
            let terminal = bundle.handle.terminal().expect("terminal").clone();
            let mut out = bundle.handle.output.subscribe();
            let request = ResizeRequest {
                cols: 90,
                rows: 30,
                cell_px: Some((9, 18)),
                resync_clients: true,
                resync_only: false,
                resync_for: None,
            };
            terminal.resize.try_send(request).expect("pixel donor");
            for cols in 1..=512 {
                terminal
                    .resize
                    .try_send(ResizeRequest {
                        cols,
                        rows: 30,
                        cell_px: None,
                        resync_clients: false,
                        ..request
                    })
                    .expect("geometry cannot fill the mailbox");
            }
            terminal
                .resize
                .try_send(ResizeRequest {
                    cols: 137,
                    rows: 53,
                    cell_px: None,
                    resync_clients: false,
                    ..request
                })
                .expect("final geometry");
            let (reply, snapshot) = tokio::sync::oneshot::channel();
            terminal
                .snapshot
                .try_send(SnapshotRequest {
                    scrollback: None,
                    max_bytes: 1024 * 1024,
                    max_frames: 64,
                    chunk_bytes: 64 * 1024,
                    reply,
                })
                .expect("queue attach capture before actor starts");
            let token = bundle.token;
            let join = tokio::task::spawn_local(bundle.actor.run());
            let (snapshot, _) = tokio::time::timeout(ACTOR_EXIT_DEADLINE, snapshot)
                .await
                .expect("capture deadline")
                .expect("actor reply")
                .expect("snapshot");
            assert_eq!((snapshot.cols, snapshot.rows), (137, 53));
            let size = master.lock().expect("master").get_size().expect("winsize");
            assert_eq!(
                (size.cols, size.rows, size.pixel_width, size.pixel_height),
                (137, 53, 1233, 954)
            );
            let resync = tokio::time::timeout(ACTOR_EXIT_DEADLINE, out.recv())
                .await
                .expect("resync deadline")
                .expect("resync");
            assert!(
                matches!(
                    resync,
                    PaneOutput::Resync {
                        cols: 137,
                        rows: 53,
                        reason: ResyncReason::Resize,
                        audience: ResyncAudience::Everyone,
                        ..
                    }
                ),
                "an attach-time update cannot erase an owed live resync: {resync:?}"
            );
            token.cancel();
            tokio::time::timeout(ACTOR_EXIT_DEADLINE, join)
                .await
                .expect("actor exit")
                .expect("actor task");
        })
        .await;
}

#[test]
fn geometry_pressure_preserves_targeted_recovery_and_closed_delivery() {
    let (tx, mut rx) = ResizeSender::channel(1);
    let target = ResyncTarget {
        owner: 7,
        stream_id: phux_protocol::ids::StreamId::new(3).unwrap(),
        bootstrap_id: phux_protocol::ids::BootstrapId::new(2).unwrap(),
    };
    let recovery = ResizeRequest {
        cols: 0,
        rows: 0,
        cell_px: None,
        resync_clients: true,
        resync_only: true,
        resync_for: Some(target),
    };
    tx.try_send(recovery).expect("fill recovery queue");
    assert!(matches!(
        tx.try_send(recovery),
        Err(mpsc::error::TrySendError::Full(_))
    ));
    for cols in 1..=512 {
        tx.try_send(ResizeRequest {
            cols,
            rows: 30,
            resync_only: false,
            resync_for: None,
            ..recovery
        })
        .expect("recovery pressure cannot drop geometry");
    }
    let final_size = rx.try_recv().expect("latest geometry");
    assert_eq!((final_size.cols, final_size.rows), (512, 30));
    let owed = rx.try_recv().expect("targeted recovery survives");
    assert!(owed.resync_only);
    assert_eq!(owed.resync_for, Some(target));
    drop(rx);
    assert!(matches!(
        tx.try_send(final_size),
        Err(mpsc::error::TrySendError::Closed(_))
    ));
}

#[test]
fn failed_engine_resize_does_not_settle_or_publish_the_requested_geometry() {
    let mut bundle = TerminalActor::new(80, 24).expect("terminal");
    let saved = std::mem::replace(
        bundle.actor.terminal.get_mut(),
        CanonicalTerminal::Plain(None),
    );
    let request = ResizeRequest {
        cols: 100,
        rows: 40,
        cell_px: Some((9, 18)),
        resync_clients: true,
        resync_only: false,
        resync_for: None,
    };
    assert!(
        bundle.actor.apply_resize_request(request).is_empty(),
        "a refused engine resize cannot publish replacement geometry"
    );
    *bundle.actor.terminal.get_mut() = saved;
    assert_eq!(
        bundle.actor.apply_resize_request(request).len(),
        1,
        "a successful retry owes the replacement that failure refused"
    );
    let snapshot = bundle.actor.synthesize().expect("snapshot after retry");
    assert_eq!(
        (snapshot.cols, snapshot.rows),
        (100, 40),
        "the same request must remain retryable after engine refusal"
    );
}

/// A resize with cell pixel metrics sets the kernel winsize pixels
/// (`cells x cell size`); a later pixel-less resize keeps them.
#[tokio::test(flavor = "current_thread")]
async fn resize_with_cell_px_updates_pty_winsize_pixels() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let cmd = CommandBuilder::new("/bin/cat");
            let bundle = TerminalActor::new_with_command(cmd, 80, 24).expect("spawn");
            let master = std::sync::Arc::clone(&bundle.actor.pty.as_ref().expect("pty").master);
            let handle = bundle.handle.clone();
            let token = bundle.token.clone();
            let join = tokio::task::spawn_local(bundle.actor.run());

            // Poll the kernel winsize until it reaches `want` (the
            // resize mailbox is async); bail out after a bounded wait.
            let wait_for = async |want: (u16, u16, u16, u16)| {
                let read = || {
                    let got = master
                        .lock()
                        .expect("master lock")
                        .get_size()
                        .expect("size");
                    (got.cols, got.rows, got.pixel_width, got.pixel_height)
                };
                for _ in 0..200 {
                    if read() == want {
                        return;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
                panic!(
                    "winsize never reached {want:?}; kernel reports {:?}",
                    read()
                );
            };

            // 100x40 cells at 9x18 px per cell: 900x720 px text area.
            handle
                .terminal()
                .expect("terminal facet")
                .resize
                .send(ResizeRequest {
                    cols: 100,
                    rows: 40,
                    cell_px: Some((9, 18)),
                    resync_clients: false,
                    resync_only: false,
                    resync_for: None,
                })
                .await
                .expect("send resize");
            wait_for((100, 40, 900, 720)).await;

            // Pixel-less resize: grid changes, cell size sticks.
            handle
                .terminal()
                .expect("terminal facet")
                .resize
                .send(ResizeRequest {
                    cols: 90,
                    rows: 30,
                    cell_px: None,
                    resync_clients: false,
                    resync_only: false,
                    resync_for: None,
                })
                .await
                .expect("send resize");
            wait_for((90, 30, 810, 540)).await;

            token.cancel();
            tokio::time::timeout(ACTOR_EXIT_DEADLINE, join)
                .await
                .expect("actor did not exit after cancel")
                .expect("actor task panicked");
        })
        .await;
}

/// Without client pixel metrics the winsize still carries nonzero pixels
/// from [`DEFAULT_CELL_PX`], at spawn and after a pixel-less resize.
#[tokio::test(flavor = "current_thread")]
async fn winsize_pixels_default_when_no_client_reports_metrics() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let cmd = CommandBuilder::new("/bin/cat");
            let bundle = TerminalActor::new_with_command(cmd, 80, 24).expect("spawn");
            let master = std::sync::Arc::clone(&bundle.actor.pty.as_ref().expect("pty").master);
            let handle = bundle.handle.clone();
            let token = bundle.token.clone();
            let join = tokio::task::spawn_local(bundle.actor.run());

            let (cell_w, cell_h) = DEFAULT_CELL_PX;

            // Spawn-time winsize: derived from the fallback cell size,
            // never zero. 80x24 cells at 8x16 px -> 640x384 px.
            let spawned = master
                .lock()
                .expect("master lock")
                .get_size()
                .expect("size");
            assert_eq!(
                (spawned.pixel_width, spawned.pixel_height),
                (80 * cell_w, 24 * cell_h),
                "spawn-time winsize must carry nonzero default pixel dims",
            );

            let wait_for = async |want: (u16, u16, u16, u16)| {
                let read = || {
                    let got = master
                        .lock()
                        .expect("master lock")
                        .get_size()
                        .expect("size");
                    (got.cols, got.rows, got.pixel_width, got.pixel_height)
                };
                for _ in 0..200 {
                    if read() == want {
                        return;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
                panic!(
                    "winsize never reached {want:?}; kernel reports {:?}",
                    read()
                );
            };

            // A pixel-less resize keeps deriving pixels from the default
            // cell size: 100x40 cells at 8x16 px -> 800x640 px.
            handle
                .terminal()
                .expect("terminal facet")
                .resize
                .send(ResizeRequest {
                    cols: 100,
                    rows: 40,
                    cell_px: None,
                    resync_clients: false,
                    resync_only: false,
                    resync_for: None,
                })
                .await
                .expect("send resize");
            wait_for((100, 40, 100 * cell_w, 40 * cell_h)).await;

            token.cancel();
            tokio::time::timeout(ACTOR_EXIT_DEADLINE, join)
                .await
                .expect("actor did not exit after cancel")
                .expect("actor task panicked");
        })
        .await;
}

/// XTWINOPS end to end: a PTY child's `CSI 14 t` / `CSI 18 t` get the
/// latest geometry back through `on_size` and `on_pty_write` (seen via tty
/// echo).
#[tokio::test(flavor = "current_thread")]
async fn xtwinops_size_queries_answered_from_resized_geometry() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut cmd = CommandBuilder::new("/bin/sh");
            // One query pair per input line, so the test can re-trigger
            // if an early line raced the resize.
            cmd.args(["-c", r"while read _; do printf '\033[14t\033[18t'; done"]);
            let bundle = TerminalActor::new_with_command(cmd, 80, 24).expect("spawn");
            let pty_in = bundle.actor.pty_tx.clone().expect("pty writer");
            let handle = bundle.handle.clone();
            let token = bundle.token;
            let mut out = handle.output.subscribe();
            let join = tokio::task::spawn_local(bundle.actor.run());

            handle
                .terminal()
                .expect("terminal facet")
                .resize
                .send(ResizeRequest {
                    cols: 100,
                    rows: 40,
                    cell_px: Some((9, 18)),
                    resync_clients: false,
                    resync_only: false,
                    resync_for: None,
                })
                .await
                .expect("send resize");
            // Let the actor drain the resize before the first query.
            for _ in 0..16 {
                tokio::task::yield_now().await;
            }

            // Replies: `ESC [ 4 ; h ; w t` and `ESC [ 8 ; rows ; cols t`,
            // echoed with ESC possibly rendered as `^[`.
            let seen = |acc: &[u8], tail: &[u8]| {
                contains_subslice(acc, &[b"\x1b[", tail].concat())
                    || contains_subslice(acc, &[b"^[[", tail].concat())
            };
            let mut acc: Vec<u8> = Vec::new();
            let mut found = false;
            let deadline = tokio::time::Instant::now() + ACTOR_EXIT_DEADLINE;
            let mut round = 0_usize;
            while tokio::time::Instant::now() < deadline {
                // Re-poke in case the first `go` beat the child's startup.
                if round.is_multiple_of(16) {
                    pty_in
                        .try_send(EncodedInputRequest::legacy(b"go\n".to_vec()))
                        .expect("pty write");
                }
                round += 1;
                match tokio::time::timeout(DRAIN_POLL_TICK, out.recv()).await {
                    Ok(Ok(PaneOutput::Live { bytes, .. })) => acc.extend_from_slice(&bytes),
                    Ok(Ok(PaneOutput::Resync { bytes, .. })) => {
                        acc.extend_from_slice(&bytes);
                    }
                    Ok(Ok(PaneOutput::Control { .. })) | Err(_) => {}
                    Ok(Err(_)) => break, // channel closed
                }
                if seen(&acc, b"4;720;900t") && seen(&acc, b"8;40;100t") {
                    found = true;
                    break;
                }
            }
            assert!(
                found,
                "XTWINOPS replies never observed; output so far: {:?}",
                String::from_utf8_lossy(&acc),
            );

            // The writer bridge exits on channel close; `shutdown_pty`
            // joins it, so the test's sender clone must drop first.
            drop(pty_in);
            token.cancel();
            tokio::time::timeout(ACTOR_EXIT_DEADLINE, join)
                .await
                .expect("actor did not exit after cancel")
                .expect("actor task panicked");
        })
        .await;
}

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
                    Ok(Ok(PaneOutput::Resync {
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
                    Ok(Ok(PaneOutput::Live { bytes, .. })) => acc.extend_from_slice(&bytes),
                    Ok(Ok(PaneOutput::Control { .. })) => {}
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

/// Drain every `PaneOutput::Resync` currently queued on `out`, returning
/// the grid each one carried. Live output and lag drops are not resyncs.
fn drain_resync_dims(out: &mut tokio::sync::broadcast::Receiver<PaneOutput>) -> Vec<(u16, u16)> {
    let mut dims = Vec::new();
    loop {
        match out.try_recv() {
            Ok(PaneOutput::Resync { cols, rows, .. }) => dims.push((cols, rows)),
            Ok(PaneOutput::Live { .. } | PaneOutput::Control { .. })
            | Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {}
            Err(_) => break,
        }
    }
    dims
}

/// A resize repeating the settled geometry publishes no resync (it would
/// rotate the bootstrap generation for nothing); a real one still does.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_no_op_resize_publishes_no_resync_for_phux_a5xj() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let bundle = TerminalActor::new_with_seed(80, 24, b"a5xj-marker").expect("seed");
            let handle = bundle.handle.clone();
            let token = bundle.token;
            // Subscribe before the actor runs so no broadcast is missed.
            let mut out = handle.output.subscribe();
            let join = tokio::task::spawn_local(bundle.actor.run());

            // A coalesced excursion that never reached the actor is still
            // a no-op, not a reason to replace an established generation.
            handle
                .terminal()
                .expect("terminal facet")
                .resize
                .try_send(ResizeRequest {
                    cols: 100,
                    rows: 40,
                    cell_px: None,
                    resync_clients: true,
                    resync_only: false,
                    resync_for: None,
                })
                .expect("transient geometry");
            // Exactly what the reflow emits for a pane the spawn already
            // sized: the geometry it is already at.
            handle
                .terminal()
                .expect("terminal facet")
                .resize
                .send(ResizeRequest {
                    cols: 80,
                    rows: 24,
                    cell_px: None,
                    resync_clients: true,
                    resync_only: false,
                    resync_for: None,
                })
                .await
                .expect("send no-op resize");
            settle_past_resync_debounce().await;
            assert_eq!(
                drain_resync_dims(&mut out),
                Vec::new(),
                "a resize to the settled geometry must not rotate the generation",
            );

            // A real reflow still resyncs with the new dims.
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
                .expect("send real resize");
            settle_past_resync_debounce().await;
            assert_eq!(
                drain_resync_dims(&mut out),
                vec![(40, 10)],
                "a resize that actually reflowed must still resync exactly once",
            );

            token.cancel();
            tokio::time::timeout(ACTOR_EXIT_DEADLINE, join)
                .await
                .expect("actor did not exit after cancel")
                .expect("actor task panicked");
        })
        .await;
}

/// Two pumps falling behind in one window get one snapshot addressed to
/// both and nobody else, and the grid does not move.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn targeted_gap_resyncs_coalesce_into_one_snapshot_for_exactly_their_pumps() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let bundle = TerminalActor::new_with_seed(80, 24, b"auqy-marker").expect("seed");
            let handle = bundle.handle.clone();
            let token = bundle.token;
            let mut out = handle.output.subscribe();
            let join = tokio::task::spawn_local(bundle.actor.run());

            let pump = |owner| ResyncTarget {
                owner,
                stream_id: phux_protocol::ids::StreamId::new(1).expect("stream id"),
                bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("bootstrap id"),
            };
            for owner in [4, 9, 4] {
                handle
                    .terminal()
                    .expect("terminal facet")
                    .resize
                    .send(ResizeRequest {
                        cols: 0,
                        rows: 0,
                        cell_px: None,
                        resync_clients: true,
                        resync_only: true,
                        resync_for: Some(pump(owner)),
                    })
                    .await
                    .expect("send targeted resync_only");
            }
            settle_past_resync_debounce().await;

            let mut resyncs = Vec::new();
            loop {
                match out.try_recv() {
                    Ok(PaneOutput::Resync {
                        cols,
                        rows,
                        reason,
                        audience,
                        ..
                    }) => resyncs.push((cols, rows, reason, audience)),
                    Ok(PaneOutput::Live { .. } | PaneOutput::Control { .. })
                    | Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {}
                    Err(_) => break,
                }
            }
            assert_eq!(
                resyncs,
                vec![(
                    80,
                    24,
                    ResyncReason::OutboundGap,
                    ResyncAudience::Only(vec![pump(4), pump(9)].into()),
                )],
                "one synthesis, addressed to exactly the pumps that asked",
            );

            token.cancel();
            tokio::time::timeout(ACTOR_EXIT_DEADLINE, join)
                .await
                .expect("actor did not exit after cancel")
                .expect("actor task panicked");
        })
        .await;
}

/// A `resync_only` request re-broadcasts the grid without resizing it.
#[tokio::test]
async fn resync_only_request_rebroadcasts_snapshot_without_resizing() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let bundle = TerminalActor::new_with_seed(80, 24, b"resync-only-marker").expect("seed");
            let handle = bundle.handle.clone();
            let token = bundle.token;
            let mut out = handle.output.subscribe();
            let join = tokio::task::spawn_local(bundle.actor.run());

            // resync_only: geometry fields are ignored, so the bogus 0x0
            // must NOT become the grid size.
            handle
                .terminal()
                .expect("terminal facet")
                .resize
                .send(ResizeRequest {
                    cols: 0,
                    rows: 0,
                    cell_px: None,
                    resync_clients: true,
                    resync_only: true,
                    resync_for: None,
                })
                .await
                .expect("send resync_only");

            let mut acc: Vec<u8> = Vec::new();
            let mut resync_dims: Option<(u16, u16)> = None;
            let deadline = tokio::time::Instant::now() + ACTOR_EXIT_DEADLINE;
            while tokio::time::Instant::now() < deadline {
                match tokio::time::timeout(DRAIN_POLL_TICK, out.recv()).await {
                    Ok(Ok(PaneOutput::Resync {
                        cols, rows, bytes, ..
                    })) => {
                        resync_dims = Some((cols, rows));
                        acc.extend_from_slice(&bytes);
                        if contains_subslice(&acc, b"\x1b[!p")
                            && contains_subslice(&acc, b"resync-only-marker")
                        {
                            break;
                        }
                    }
                    Ok(Ok(PaneOutput::Live { bytes, .. })) => acc.extend_from_slice(&bytes),
                    Ok(Ok(PaneOutput::Control { .. })) => {}
                    Ok(Err(_)) => break,
                    Err(_) => tokio::task::yield_now().await,
                }
            }

            assert_eq!(
                resync_dims,
                Some((80, 24)),
                "resync_only must keep the grid size, not adopt the ignored 0x0",
            );
            assert!(
                contains_subslice(&acc, b"\x1b[!p"),
                "resync_only broadcast missing DECSTR snapshot preamble; got {:?}",
                String::from_utf8_lossy(&acc),
            );
            assert!(
                contains_subslice(&acc, b"resync-only-marker"),
                "resync_only broadcast did not re-send grid content; got {:?}",
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

/// A storm of live resizes coalesces into one resync.
#[tokio::test]
async fn rapid_resizes_coalesce_into_one_resync_snapshot() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let bundle = TerminalActor::new_with_seed(80, 24, b"drag-marker").expect("seed");
            let handle = bundle.handle.clone();
            let token = bundle.token;
            let mut out = handle.output.subscribe();
            let join = tokio::task::spawn_local(bundle.actor.run());

            // Fire a storm of live resizes back-to-back, well within
            // the RESIZE_RESYNC_DEBOUNCE window.
            for w in [70u16, 60, 50, 60, 70, 80, 90, 100] {
                handle
                    .terminal()
                    .expect("terminal facet")
                    .resize
                    .send(ResizeRequest {
                        cols: w,
                        rows: 24,
                        cell_px: None,
                        resync_clients: true,
                        resync_only: false,
                        resync_for: None,
                    })
                    .await
                    .expect("send resize");
            }

            // Wait comfortably past the debounce so the single
            // coalesced snapshot has fired.
            tokio::time::sleep(RESIZE_RESYNC_DEBOUNCE * 4).await;

            // Each resync is one `PaneOutput::Resync`.
            let mut snapshots = 0usize;
            loop {
                match out.try_recv() {
                    Ok(PaneOutput::Resync { bytes, .. }) => {
                        debug_assert!(contains_subslice(&bytes, b"\x1b[!p"));
                        snapshots += 1;
                    }
                    // Live output and a lagged drop are both "not a
                    // resync" — skip and keep draining.
                    Ok(PaneOutput::Live { .. } | PaneOutput::Control { .. })
                    | Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {}
                    Err(_) => break,
                }
            }
            assert_eq!(
                snapshots, 1,
                "a resize storm must coalesce into exactly one resync snapshot, got {snapshots}",
            );

            token.cancel();
            tokio::time::timeout(ACTOR_EXIT_DEADLINE, join)
                .await
                .expect("actor did not exit after cancel")
                .expect("actor task panicked");
        })
        .await;
}

/// A storm of degenerate resizes (zeros, 1x1, extreme ratios, a big spike
/// into a both-axes shrink) does not panic the actor, and a sane resize
/// still applies.
#[tokio::test]
async fn degenerate_resize_storm_does_not_panic_actor() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let bundle = TerminalActor::new_with_seed(80, 24, b"crash-hunt").expect("seed");
            let handle = bundle.handle.clone();
            let token = bundle.token;
            let join = tokio::task::spawn_local(bundle.actor.run());

            // A 300x300 spike exercises the actor like the unit repro's
            // 1000x1000 at a fraction of the cost.
            let storm: &[(u16, u16)] = &[
                (0, 0),
                (1, 1),
                (1, 200),
                (200, 1),
                (0, 0),
                (300, 300),
                (1, 1),
                (3, 3),
                (2, 2),
                (1, 1),
                (5, 1),
                (1, 5),
                (1, 1),
            ];
            for &(cols, rows) in storm {
                handle
                    .terminal()
                    .expect("terminal facet")
                    .resize
                    .send(ResizeRequest {
                        cols,
                        rows,
                        cell_px: None,
                        resync_clients: false,
                        resync_only: false,
                        resync_for: None,
                    })
                    .await
                    .expect("send resize");
            }
            // Let the actor drain the whole mailbox.
            for _ in 0..64 {
                tokio::task::yield_now().await;
            }

            // A final sane resize must still take effect — proof the
            // actor survived and is processing, not wedged.
            handle
                .terminal()
                .expect("terminal facet")
                .resize
                .send(ResizeRequest {
                    cols: 100,
                    rows: 30,
                    cell_px: None,
                    resync_clients: false,
                    resync_only: false,
                    resync_for: None,
                })
                .await
                .expect("send final resize");
            for _ in 0..16 {
                tokio::task::yield_now().await;
            }

            token.cancel();
            tokio::time::timeout(ACTOR_EXIT_DEADLINE, join)
                .await
                .expect("actor did not exit after cancel")
                .expect("actor task panicked under degenerate resize storm");
        })
        .await;
}

/// Both-axes shrinks issued as single `resize()` calls do not overflow
/// libghostty's `PageList.resizeCols` (a plain terminal test, so a
/// regression aborts here).
#[test]
fn resize_desync_then_both_shrink_does_not_overflow() {
    let mut term = {
        let mut terminal = GhosttyTerminal::new(80, 24).expect("term");
        terminal.set_scrollback_max_lines(Some(100)).expect("term");
        terminal
    };
    // Enough content that a 1-col reflow pushes rows into scrollback.
    for i in 0..50u32 {
        let line = format!("row-{i}-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\r\n");
        term.vt_write(line.as_bytes());
    }

    // The minimal trigger: a 0x0 (fails, no-op) immediately followed by
    // a both-shrink to 1x1, then the wider degenerate storm.
    let storm: &[(u16, u16)] = &[
        (0, 0),
        (1, 1),
        (1, 200),
        (200, 1),
        (0, 0),
        (1000, 1000),
        (1, 1),
        (3, 3),
        (2, 2),
        (1, 1),
        (100, 30),
    ];
    for &(req_cols, req_rows) in storm {
        // Fresh lines each step reflow content from the previous geometry.
        for i in 0..8u32 {
            let line = format!("interleave-{i}-bbbbbbbbbbbbbbbbbbbbbbbbbbbb\r\n");
            term.vt_write(line.as_bytes());
        }
        // Mirror `handle_resize`: 1-cell clamp, one bare resize per step.
        let cols = req_cols.max(1);
        let rows = req_rows.max(1);
        let _ = term.resize(cols, rows, 0, 0);
    }

    // Survived without SIGABRT; the grid settled at the final sane size.
    assert_eq!(term.cols().expect("cols"), 100);
    assert_eq!(term.rows().expect("rows"), 30);
}
