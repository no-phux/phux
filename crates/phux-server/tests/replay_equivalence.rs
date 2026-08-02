//! Replay-equivalence property tests for the pane output stream (phux-r4k).
//!
//! The server brings a fresh consumer up to date with a snapshot
//! (`vt_replay_bytes`, served by `SnapshotRequest`) and thereafter forwards
//! every PTY byte over the `TERMINAL_OUTPUT` broadcast. An audit established
//! the broadcast stream is *complete* (no dropped bytes on the happy path),
//! but nothing asserted the stronger *totality* property these two channels
//! are supposed to jointly guarantee:
//!
//!   For a consumer that captures a snapshot `S0` and then applies every
//!   byte the server broadcasts afterwards, the reconstructed grid is equal
//!   to the server's own authoritative grid at that later instant.
//!
//! We pin that down by comparing two libghostty grids, line for line:
//!
//!   * the consumer's reconstruction: `S0.bytes ++ broadcast-tail`, replayed
//!     into a fresh `Terminal` (via the `Screen` oracle), and
//!   * the server's authority: a *fresh* snapshot `S1` taken once the stream
//!     has settled (`S1` IS the server's serialized grid right then).
//!
//! # Ordering contract this relies on (see `terminal_actor.rs::run`)
//!
//! The actor is a single `biased` `select!` loop on one task. The PTY arm
//! writes a chunk to the `Terminal` and broadcasts that same chunk, from the
//! *same* arm, with no interleaving against the snapshot arm. A
//! `SnapshotRequest` is served in a different arm by walking the live
//! `Terminal`. There is no sequence number tying a snapshot to a position in
//! the broadcast stream; the only ordering is the causal serialization of the
//! loop. So "capture S0, then everything after it" must avoid both an overlap
//! (a chunk already in S0 that we also replay) and a gap (a chunk in S1 that we
//! never collected).
//!
//! We make that race-free with a *deterministic, single-shot* producer rather
//! than `cat`'s cooked-mode double echo (whose line-discipline echo and stdout
//! echo arrive as separate chunks with load-dependent timing — that is exactly
//! what makes a naive `cat`-based version flaky). The pane runs a shell that
//! blocks on `read`, and each newline we send releases one `printf` of a fixed,
//! self-positioning block emitted in a single write:
//!
//!   * S0 is taken *before* any block is released, so the stream is empty at S0
//!     and there is nothing to reconcile — no overlap.
//!   * Each block ends in a unique sentinel token; we collect the broadcast
//!     until the sentinel of the *last* block appears, so the full stream up to
//!     S1 is captured — no gap. Because each block is one atomic `printf`, the
//!     sentinel appears exactly once, eliminating the "stopped at the first of
//!     two echoes" hazard.
//!
//! Blocks position themselves with `\r\n` so the only input echo in the stream
//! (the newline released to `read`) is a cursor move that lands identically in
//! both the server grid and the reconstruction.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

mod common;

use std::time::Duration;

use common::screen::Screen;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_server::grid::SnapshotBytes;
use phux_server::state::TerminalInput;
use phux_server::terminal_actor::{PaneOutput, ResizeRequest, SnapshotRequest, TerminalActor};
use tokio::sync::broadcast::Receiver;
use tokio::sync::broadcast::error::TryRecvError;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::time::timeout;

/// Generous per-step deadline. The work resolves in milliseconds on the
/// happy path; the margin only absorbs scheduler latency under parallel
/// nextest load (real PTY children contend for CPU). A genuine hang still
/// fails the test, just later.
const STEP_DEADLINE: Duration = Duration::from_secs(10);

/// A bare `Enter` press, used to release one `read` in the producer shell.
const fn enter_key() -> KeyEvent {
    KeyEvent {
        action: KeyAction::Press,
        key: PhysicalKey::Enter,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: None,
        unshifted_codepoint: None,
    }
}

/// Spawn a pane whose shell blocks on `read` between fixed `printf` blocks.
///
/// The script is `read _; printf '<b0>'; read _; printf '<b1>'; ...; sleep N`.
/// Each [`release_block`] sends one newline, unblocking the next `read` so the
/// shell prints the next block as a single atomic write. No cooked-mode echo
/// race: the only input echo is the newline itself (a harmless cursor move that
/// the self-positioning `\r\n`-prefixed blocks make irrelevant to content).
fn producer(blocks: &[&str]) -> portable_pty::CommandBuilder {
    let mut script = String::new();
    for b in blocks {
        script.push_str("read _; printf '");
        script.push_str(b);
        script.push_str("'; ");
    }
    // Keep the pane alive after the last block so late snapshots still work.
    script.push_str("sleep 3600");
    let mut cmd = portable_pty::CommandBuilder::new("/bin/sh");
    cmd.args(["-c", &script]);
    cmd
}

/// Release exactly one `printf` block by satisfying one `read`.
async fn release_block(input: &mpsc::Sender<TerminalInput>) {
    input
        .send(TerminalInput::Key(enter_key()))
        .await
        .expect("send newline to release block");
}

/// Drain the broadcast until `needle` appears in the accumulated bytes,
/// returning everything received. Panics on the deadline (a stuck pane is a
/// loud failure, not a silent pass).
async fn collect_until(rx: &mut Receiver<PaneOutput>, needle: &[u8]) -> Vec<u8> {
    let work = async {
        let mut acc: Vec<u8> = Vec::new();
        loop {
            match rx.recv().await {
                Ok(
                    PaneOutput::Live { bytes: chunk, .. } | PaneOutput::Resync { bytes: chunk, .. },
                ) => {
                    acc.extend_from_slice(&chunk);
                    if acc.windows(needle.len()).any(|w| w == needle) {
                        return acc;
                    }
                }
                Ok(PaneOutput::Control { .. }) => {}
                // A slow subscriber dropping bytes would make the replay
                // assertion meaningless (we'd be missing stream content), so
                // a lag is a hard failure here, unlike in liveness-only tests.
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    panic!(
                        "broadcast lagged by {n}; replay totality cannot hold with dropped bytes"
                    )
                }
                Err(broadcast::error::RecvError::Closed) => {
                    panic!("broadcast closed before {needle:?} arrived; acc={acc:?}")
                }
            }
        }
    };
    timeout(STEP_DEADLINE, work)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {needle:?} on broadcast"))
}

/// Non-blocking sweep of any bytes already queued on the receiver. Used after a
/// sentinel has been observed, to fold trailing bytes (e.g. the resize resync
/// snapshot) into the collected stream. A lag here is still a hard failure: a
/// dropped chunk breaks totality.
fn drain_pending(rx: &mut Receiver<PaneOutput>) -> Vec<u8> {
    let mut acc = Vec::new();
    loop {
        match rx.try_recv() {
            Ok(PaneOutput::Live { bytes: chunk, .. } | PaneOutput::Resync { bytes: chunk, .. }) => {
                acc.extend_from_slice(&chunk);
            }
            Ok(PaneOutput::Control { .. }) => {}
            Err(TryRecvError::Empty | TryRecvError::Closed) => return acc,
            Err(TryRecvError::Lagged(n)) => {
                panic!("broadcast lagged by {n} during drain; replay totality cannot hold")
            }
        }
    }
}

/// Request a fresh snapshot from the actor and await the reply.
async fn snapshot(snap_tx: &mpsc::Sender<SnapshotRequest>) -> SnapshotBytes {
    let (tx, rx) = oneshot::channel();
    snap_tx
        .send(SnapshotRequest {
            scrollback: None,
            reply: tx,
        })
        .await
        .expect("send snapshot request");
    timeout(STEP_DEADLINE, rx)
        .await
        .expect("snapshot timed out")
        .expect("snapshot reply dropped")
        .0
}

/// Replay `bytes` into a fresh `Screen` sized `cols x rows` and return the
/// row-major plain-text grid. This is exactly what a reconnecting consumer
/// does: feed the snapshot (and any subsequent output) through one VT parser.
fn replay_grid(cols: u16, rows: u16, bytes: &[u8]) -> Vec<String> {
    let mut screen = Screen::new(cols, rows).expect("screen");
    screen.write(bytes);
    screen.rows()
}

/// Pretty side-by-side diff for assertion messages.
fn grid_diff(reconstructed: &[String], authoritative: &[String]) -> String {
    use std::fmt::Write as _;
    let n = reconstructed.len().max(authoritative.len());
    let mut out = String::new();
    for i in 0..n {
        let l = reconstructed.get(i).map_or("<none>", String::as_str);
        let r = authoritative.get(i).map_or("<none>", String::as_str);
        let mark = if l == r { "  " } else { "!!" };
        let _ = writeln!(out, "{mark} row {i:>3}: recon={l:?} auth={r:?}");
    }
    out
}

/// Steady-state totality: snapshot S0 plus the broadcast bytes produced
/// afterwards reconstructs the server's authoritative grid (snapshot S1).
///
/// No resize: this is the baseline the audit claimed holds. It must pass.
#[test]
fn snapshot_plus_output_reconstructs_server_grid() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("rt");
    let local = tokio::task::LocalSet::new();
    rt.block_on(local.run_until(async {
        // Two self-positioning blocks; the second ends in the sentinel token.
        let bundle = TerminalActor::new_with_command(
            producer(&["\\r\\nalpha\\r\\nbravo", "\\r\\ncharlie\\r\\nSENTINEL"]),
            80,
            24,
        )
        .expect("spawn producer");
        let handle = bundle.handle.clone();
        let token = bundle.token;

        // Subscribe before the actor runs so we never miss a chunk.
        let mut rx = handle.output.subscribe();
        let join = tokio::task::spawn_local(bundle.actor.run());

        // Take S0 BEFORE releasing any block. The shell is blocked on the
        // first `read`, so the stream is empty and S0 has nothing to reconcile
        // — this is what makes the capture race-free.
        let s0 = snapshot(&handle.snapshot).await;

        // Release both blocks; collect the broadcast through the final
        // sentinel. Each block is one atomic printf, so the sentinel appears
        // exactly once and the whole stream up to S1 is captured.
        release_block(&handle.input).await;
        release_block(&handle.input).await;
        let mut tail = collect_until(&mut rx, b"SENTINEL").await;

        // Sweep any bytes still queued behind the sentinel, then take S1 — it
        // reflects exactly the stream we have collected.
        tail.extend_from_slice(&drain_pending(&mut rx));
        let s1 = snapshot(&handle.snapshot).await;

        assert_eq!(
            (s0.cols, s0.rows),
            (s1.cols, s1.rows),
            "no resize occurred; snapshot dimensions must match",
        );

        let mut reconstructed_bytes = s0.bytes.clone();
        reconstructed_bytes.extend_from_slice(&tail);
        let reconstructed = replay_grid(s1.cols, s1.rows, &reconstructed_bytes);
        let authoritative = replay_grid(s1.cols, s1.rows, &s1.bytes);

        assert_eq!(
            reconstructed,
            authoritative,
            "S0 + output stream must reconstruct the server grid exactly.\n{}",
            grid_diff(&reconstructed, &authoritative),
        );

        token.cancel();
        let _ = timeout(STEP_DEADLINE, join).await;
    }));
}

/// Totality across a resize. The resize mutates the server grid via the
/// `resize` channel — geometry the byte stream itself does not describe. The
/// only thing that carries that state to a live consumer is the post-reflow
/// *resync snapshot* the actor re-broadcasts as ordinary output (phux-8v1).
///
/// This probes whether that resync mechanism is enough for full totality:
/// can a consumer holding S0 (at the old size) plus the entire broadcast
/// (including the resync snapshot) reconstruct the server's authoritative
/// grid at the NEW size (S1)?
///
/// If it passes, the resync-snapshot-in-broadcast covers the resize and the
/// stream is total across geometry changes. If it diverges, the assertion is
/// kept real (not weakened) and the test is `#[ignore]`d with the finding.
#[test]
fn snapshot_plus_output_reconstructs_server_grid_across_resize() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("rt");
    let local = tokio::task::LocalSet::new();
    rt.block_on(local.run_until(async {
        // First block before the resize, second (sentinel-bearing) after.
        let bundle = TerminalActor::new_with_command(
            producer(&["\\r\\nalpha\\r\\nbravo", "\\r\\ncharlie\\r\\nSENTINEL"]),
            80,
            24,
        )
        .expect("spawn producer");
        let handle = bundle.handle.clone();
        let token = bundle.token;

        let mut rx = handle.output.subscribe();
        let join = tokio::task::spawn_local(bundle.actor.run());

        // S0 at the OLD size, before any block — race-free (see steady test).
        let s0 = snapshot(&handle.snapshot).await;
        assert_eq!(
            (s0.cols, s0.rows),
            (80, 24),
            "S0 taken at the original size",
        );

        // Pre-resize block so the old-size grid is non-trivial.
        release_block(&handle.input).await;
        let mut tail = collect_until(&mut rx, b"bravo").await;

        // Live resize to a different geometry. `resync_clients: true` arms the
        // debounced resync snapshot the actor re-broadcasts as output.
        handle
            .resize
            .send(ResizeRequest {
                cols: 100,
                rows: 30,
                cell_px: None,
                resync_clients: true,
                resync_only: false,
            })
            .await
            .expect("resize");

        // Post-resize block ending in the sentinel, so we know we have the full
        // post-resize stream.
        release_block(&handle.input).await;
        tail.extend_from_slice(&collect_until(&mut rx, b"SENTINEL").await);

        // The debounced resync snapshot (RESIZE_RESYNC_DEBOUNCE = 50ms) can
        // land after the sentinel. Poll past the debounce, sweeping every
        // trailing byte — including that resync snapshot — into the tail.
        for _ in 0..40 {
            tail.extend_from_slice(&drain_pending(&mut rx));
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        tail.extend_from_slice(&drain_pending(&mut rx));

        // S1 is the server's authoritative grid at the NEW size.
        let s1 = snapshot(&handle.snapshot).await;
        assert_eq!(
            (s1.cols, s1.rows),
            (100, 30),
            "S1 taken at the resized geometry",
        );

        // The consumer reconstructs at the new size (its mirror would have
        // reflowed to the VIEWPORT_RESIZE it received). Replay S0 (old-size
        // bytes) + the full post-S0 stream into a new-size grid.
        let mut reconstructed_bytes = s0.bytes.clone();
        reconstructed_bytes.extend_from_slice(&tail);
        let reconstructed = replay_grid(s1.cols, s1.rows, &reconstructed_bytes);
        let authoritative = replay_grid(s1.cols, s1.rows, &s1.bytes);

        assert_eq!(
            reconstructed,
            authoritative,
            "S0 + output stream (incl. resync snapshot) must reconstruct the \
             resized server grid.\n{}",
            grid_diff(&reconstructed, &authoritative),
        );

        token.cancel();
        let _ = timeout(STEP_DEADLINE, join).await;
    }));
}
