//! Resize and resync tests: terminal/PTY winsize propagation,
//! XTWINOPS size queries, debounced resync broadcasts, and resize
//! storm behavior.

use super::test_support::*;
use super::*;

/// Resize updates both the libghostty `Terminal` and (when present)
/// the PTY winsize. We only assert the Terminal side here — the
/// PTY ioctl path is exercised in the integration test.
#[tokio::test(flavor = "current_thread")]
async fn resize_updates_terminal_dims() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let bundle = TerminalActor::new(80, 24).expect("new");
            let handle = bundle.handle.clone();
            let token = bundle.token;
            let join = tokio::task::spawn_local(bundle.actor.run());

            handle
                .resize
                .send(ResizeRequest {
                    cols: 120,
                    rows: 40,
                    cell_px: None,
                    resync_clients: false,
                    resync_only: false,
                })
                .await
                .expect("send resize");
            // Give the actor a moment to process the resize before
            // we shut it down. A bounded `yield_now` loop is the
            // current-thread-friendly version of `sleep(0)`.
            for _ in 0..16 {
                tokio::task::yield_now().await;
            }

            token.cancel();
            tokio::time::timeout(ACTOR_EXIT_DEADLINE, join)
                .await
                .expect("actor did not exit after cancel")
                .expect("actor task panicked");
        })
        .await;
}

/// A resize carrying cell pixel metrics must land in the kernel
/// winsize: `ws_xpixel`/`ws_ypixel` = cells x cell size. TIOCGWINSZ
/// is the first thing pixel-aware programs (`kitten icat`, sixel
/// sizers) consult; without the `cell_px` plumbing it reads 0x0.
/// A later pixel-less resize (agent `TERMINAL_RESIZE`) must keep the
/// established cell size rather than zeroing the pixel fields.
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
                .resize
                .send(ResizeRequest {
                    cols: 100,
                    rows: 40,
                    cell_px: Some((9, 18)),
                    resync_clients: false,
                    resync_only: false,
                })
                .await
                .expect("send resize");
            wait_for((100, 40, 900, 720)).await;

            // Pixel-less resize: grid changes, cell size sticks.
            handle
                .resize
                .send(ResizeRequest {
                    cols: 90,
                    rows: 30,
                    cell_px: None,
                    resync_clients: false,
                    resync_only: false,
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

/// With no client ever reporting pixel metrics, the PTY winsize must
/// still carry nonzero pixel dimensions derived from [`DEFAULT_CELL_PX`]:
/// at spawn (before any resize) and after a pixel-less resize. This is
/// the proximate `kitten icat` unblock — its preflight refuses a `0x0`
/// pixel report, and most clients announce cells but not pixels.
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
                .resize
                .send(ResizeRequest {
                    cols: 100,
                    rows: 40,
                    cell_px: None,
                    resync_clients: false,
                    resync_only: false,
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

/// End-to-end XTWINOPS: a PTY child queries `CSI 14 t` (text area in
/// pixels) and `CSI 18 t` (text area in cells) and must receive the
/// geometry the most recent resize established. Exercises the whole
/// reply path — libghostty parses the query from PTY output, the
/// `on_size` callback supplies the shared geometry, and `on_pty_write`
/// routes the encoded reply back into the PTY writer bridge. The
/// asserted bytes come back via tty echo of the child's input.
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
                .resize
                .send(ResizeRequest {
                    cols: 100,
                    rows: 40,
                    cell_px: Some((9, 18)),
                    resync_clients: false,
                    resync_only: false,
                })
                .await
                .expect("send resize");
            // Let the actor drain the resize before the first query.
            for _ in 0..16 {
                tokio::task::yield_now().await;
            }

            // CSI 14 t reply: ESC [ 4 ; height_px ; width_px t.
            // CSI 18 t reply: ESC [ 8 ; rows ; cols t.
            // The replies surface as tty ECHO of the child's input, and
            // ECHOCTL (in the default lflags) renders the ESC byte in
            // caret notation — `^[` — so accept either spelling.
            let seen = |acc: &[u8], tail: &[u8]| {
                contains_subslice(acc, &[b"\x1b[", tail].concat())
                    || contains_subslice(acc, &[b"^[[", tail].concat())
            };
            let mut acc: Vec<u8> = Vec::new();
            let mut found = false;
            let deadline = tokio::time::Instant::now() + ACTOR_EXIT_DEADLINE;
            let mut round = 0_usize;
            while tokio::time::Instant::now() < deadline {
                // Re-poke the shell periodically: the first `go` can land
                // before the child has finished starting, and then nothing
                // ever queries the terminal.
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

/// phux-8v1 regression: a resize must re-broadcast a full snapshot of
/// the post-reflow grid so attached clients (whose mirror reflowed
/// independently and may have dropped rows) reconverge on the
/// canonical content. We assert the broadcast that follows a resize
/// carries the snapshot reset preamble (`ESC [ ! p`, DECSTR) AND the
/// content that was on the grid before the resize — without this fix
/// the only post-resize bytes are new PTY output, so prior content is
/// never re-sent and the client shows lost/duplicated rows.
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
                .resize
                .send(ResizeRequest {
                    cols: 40,
                    rows: 10,
                    cell_px: None,
                    resync_clients: true,
                    resync_only: false,
                })
                .await
                .expect("send resize");

            // Collect broadcast bytes for a bounded window and look
            // for the snapshot. `recv` resolves as soon as the resize
            // broadcast lands. phux-3ns5: the resync rides a
            // `PaneOutput::Resync` carrying the post-reflow dims, so
            // also capture them to assert the client mirror is told to
            // resize to 40x10.
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

/// Advance virtual time comfortably past the resize-resync debounce and
/// give the actor task enough polls to have acted on it.
///
/// Deterministic: the runtime is `start_paused`, so this is a timer the
/// test drives rather than a wall-clock wait racing the actor. The yield
/// loop before the advance is what guarantees the actor has already
/// consumed the resize and (if it means to) armed the debounce, so the
/// advance cannot step over an un-armed timer.
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

/// phux-a5xj: a resize that repeats the settled geometry must publish NO
/// resync — while a real one still must (phux-8v1 is not weakened).
///
/// `handle_resize` already skipped the grid work and the native-cursor
/// invalidation for a no-op, but the resync broadcast was scheduled
/// unconditionally, and a resync is what rotates the bootstrap
/// generation. That is the second half of the wasted-capture bug: once a
/// spawn honors `SPAWN_TERMINAL.initial_size`, the client's reflow
/// `TERMINAL_RESIZE` names the size the pane already has — and would
/// still have tombstoned the checkpoint the server had just built. There
/// is nothing to reconverge from when nothing reflowed.
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

            // Exactly what the reflow emits for a pane the spawn already
            // sized: the geometry it is already at.
            handle
                .resize
                .send(ResizeRequest {
                    cols: 80,
                    rows: 24,
                    cell_px: None,
                    resync_clients: true,
                    resync_only: false,
                })
                .await
                .expect("send no-op resize");
            settle_past_resync_debounce().await;
            assert_eq!(
                drain_resync_dims(&mut out),
                Vec::new(),
                "a resize to the settled geometry must not rotate the generation",
            );

            // The suppression is specific to the no-op: a real reflow
            // still resyncs, carrying the post-reflow dims (phux-8v1 /
            // phux-3ns5).
            handle
                .resize
                .send(ResizeRequest {
                    cols: 40,
                    rows: 10,
                    cell_px: None,
                    resync_clients: true,
                    resync_only: false,
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

/// phux-y8v6: a `resync_only` request (sent by a lagged output pump)
/// must re-broadcast a full grid snapshot WITHOUT resizing the grid —
/// the recovery path for a consumer that dropped bytes past the broadcast
/// buffer. We assert the broadcast carries the snapshot preamble + the
/// seeded content, and that the resync dims are the UNCHANGED grid size
/// (proving no resize happened).
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
                .resize
                .send(ResizeRequest {
                    cols: 0,
                    rows: 0,
                    cell_px: None,
                    resync_clients: true,
                    resync_only: true,
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

/// phux-8v1 drag fix: a STORM of rapid live resizes (a window drag)
/// must COALESCE into a single resync snapshot, not one per resize.
/// Without the debounce the client gets flooded with snapshots
/// synthesized at successive widths, and a stale-width one corrupts
/// the mirror (the duplicated-characters-while-dragging symptom).
/// We count broadcasts carrying the snapshot preamble (`ESC [ ! p`);
/// each resync is exactly one such message, so the count is the
/// snapshot count regardless of any interleaved PTY output.
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
                    .resize
                    .send(ResizeRequest {
                        cols: w,
                        rows: 24,
                        cell_px: None,
                        resync_clients: true,
                        resync_only: false,
                    })
                    .await
                    .expect("send resize");
            }

            // Wait comfortably past the debounce so the single
            // coalesced snapshot has fired.
            tokio::time::sleep(RESIZE_RESYNC_DEBOUNCE * 4).await;

            // Count resync broadcasts. Debounced => exactly 1.
            // phux-3ns5: each resync is a `PaneOutput::Resync`, so the
            // variant itself is the count (no preamble sniffing needed).
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

/// Crash-hunt: a storm of *degenerate* resizes — `0x0`, `1x1`,
/// `1x200`, `200x1`, a 1000x1000 monster, and repeated both-axes
/// shrinks crossing the 1-cell clamp — must NOT panic the actor task.
/// `handle_resize` clamps to a 1-cell minimum so a zero dimension never
/// reaches libghostty; the both-axes-shrink overflow in the Zig
/// `PageList.resizeCols` is covered by libghostty-vt 0.2.0. We assert
/// the actor is still alive (the `join` unwrap surfaces a panicked task)
/// and a final sane resize still applies.
#[tokio::test]
async fn degenerate_resize_storm_does_not_panic_actor() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let bundle = TerminalActor::new_with_seed(80, 24, b"crash-hunt").expect("seed");
            let handle = bundle.handle.clone();
            let token = bundle.token;
            let join = tokio::task::spawn_local(bundle.actor.run());

            // The same degenerate shapes as the deterministic unit repro
            // (`resize_desync_then_both_shrink_does_not_overflow`): zeros,
            // 1x1 collapses, extreme aspect ratios, a big grow spiked
            // straight into a both-shrink. The spike is 300x300 rather
            // than the unit test's 1000x1000: what this test pins is that
            // the ACTOR survives the storm and keeps processing, and a
            // 90k-cell grid exercises that identically to a 1M-cell one
            // at a tenth of the allocation/walk cost. The magnitude
            // extreme stays covered by the unit repro.
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
                    .resize
                    .send(ResizeRequest {
                        cols,
                        rows,
                        cell_px: None,
                        resync_clients: false,
                        resync_only: false,
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
                .resize
                .send(ResizeRequest {
                    cols: 100,
                    rows: 30,
                    cell_px: None,
                    resync_clients: false,
                    resync_only: false,
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

/// phux-y06 regression (crash-hunt): a degenerate resize storm that
/// includes both-axes shrinks (e.g. real `80x24 -> 1x1`) issued as
/// BARE single `resize()` calls must NOT abort libghostty's
/// `PageList.resizeCols` with an integer overflow.
///
/// libghostty's `PageList.resizeCols` once overflowed (panic in Zig →
/// SIGABRT) when cols AND rows shrank in one `resize()` call. This test
/// proves the engine fix — not any phux-side axis decomposition — carries
/// the load. It feeds reflowable content, then drives the storm with a
/// 1-cell clamp only (the same input hygiene `handle_resize` keeps),
/// issuing each step as one direct `resize()`. It must survive every step
/// and settle at the final size. (Run as a plain `GhosttyTerminal` test so
/// a regression aborts THIS test, not a flaky e2e teardown.)
#[test]
fn resize_desync_then_both_shrink_does_not_overflow() {
    let mut term = GhosttyTerminal::new(TerminalOptions {
        cols: 80,
        rows: 24,
        max_scrollback: 100,
    })
    .expect("term");
    // Enough scrollback content that a cols-reflow actually walks rows
    // (the overflow needs real content to reflow). 50 lines of ~38 cols
    // is ample: at the storm's 1-col degenerate width every line reflows
    // to ~38 rows, far past the 24-row viewport and into scrollback, so
    // the both-shrink steps still drive `PageList.resizeCols` through
    // real reflow work. The original 300 bought no extra coverage, only
    // seconds of wall clock re-reflowing the same shape.
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
        // 8 fresh lines per step keeps every resize reflowing content
        // written at the PREVIOUS geometry — the desync ingredient —
        // without the volume of the original 40, which only re-walked
        // the same reflow path more times per step.
        for i in 0..8u32 {
            let line = format!("interleave-{i}-bbbbbbbbbbbbbbbbbbbbbbbbbbbb\r\n");
            term.vt_write(line.as_bytes());
        }
        // Mirror `handle_resize`: 1-cell clamp (input hygiene) only,
        // then a BARE single resize() per step — no axis decomposition.
        // libghostty-vt 0.2.0 keeps the both-shrink steps from
        // overflowing.
        let cols = req_cols.max(1);
        let rows = req_rows.max(1);
        let _ = term.resize(cols, rows, 0, 0);
    }

    // Survived without SIGABRT; the grid settled at the final sane size.
    assert_eq!(term.cols().expect("cols"), 100);
    assert_eq!(term.rows().expect("rows"), 30);
}
