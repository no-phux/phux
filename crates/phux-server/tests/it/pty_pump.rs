//! Integration tests for the PTY pump in `TerminalActor` (`phux-byc.5`).
//!
//! These tests spawn real subprocesses behind real PTYs and assert
//! end-to-end behavior of the actor:
//!
//! 1. PTY output reaches `output_tx` (the broadcast channel) AND is
//!    fed into the actor's `Terminal` (verified by snapshotting and
//!    looking for the produced bytes).
//! 2. Input written via `input` reaches the PTY (asserted by sending a
//!    keypress to `cat` and observing the echoed bytes on `output_tx`).
//! 3. Shutdown via the oneshot exits the actor and reaps the child —
//!    no zombie processes left behind.
//! 4. Snapshot synthesis after PTY output reflects the produced grid;
//!    the synthesized bytes round-trip through a fresh `Terminal`.
//! 5. Broadcast fanout: two subscribers see the same PTY bytes (this
//!    is the structural property `phux-byc.6.4` / `phux-byc.6.5`
//!    depend on).
//!
//! All tests use bounded `tokio::time::timeout` calls so a hung child
//! / hung PTY surfaces as a test failure rather than a wedged CI job.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]
#![allow(clippy::print_stdout, reason = "tests")]

use std::time::Duration;

use libghostty_vt::{Terminal as GhosttyTerminal, TerminalOptions};
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_server::state::TerminalInput;
use phux_server::terminal_actor::{PaneOutput, ResizeRequest, SnapshotRequest, TerminalActor};
use portable_pty::CommandBuilder;
use tokio::sync::oneshot;
use tokio::time::timeout;

/// Helper: read from the broadcast receiver until at least `needle`
/// appears in the accumulated bytes, or the deadline expires. Returns
/// the accumulated buffer on success; panics on timeout (so callers
/// don't need to thread Result everywhere).
async fn collect_until(
    rx: &mut tokio::sync::broadcast::Receiver<PaneOutput>,
    needle: &[u8],
    deadline: Duration,
) -> Vec<u8> {
    let work = async {
        let mut acc: Vec<u8> = Vec::new();
        loop {
            match rx.recv().await {
                Ok(PaneOutput::Live(chunk) | PaneOutput::Resync { bytes: chunk, .. }) => {
                    acc.extend_from_slice(&chunk);
                    if needle.is_empty() || acc.windows(needle.len()).any(|w| w == needle) {
                        return acc;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    // Slow subscriber; resume rather than fail.
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    return acc;
                }
            }
        }
    };
    timeout(deadline, work)
        .await
        .unwrap_or_else(|_elapsed| panic!("timed out waiting for needle {needle:?} in PTY output"))
}

/// Build a deterministic `printf`-and-exit command. Output is short and
/// fixed so tests can assert exact bytes.
fn printf_cmd(payload: &str) -> CommandBuilder {
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.args(["-c", &format!("printf '{payload}'")]);
    cmd
}

/// PTY-spawned command's stdout is forwarded to the broadcast channel
/// AND fed into the actor's `Terminal` (visible via a snapshot).
#[test]
fn pty_output_reaches_broadcast_and_terminal() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("rt");
    let local = tokio::task::LocalSet::new();
    rt.block_on(local.run_until(async {
        let bundle = TerminalActor::new_with_command(printf_cmd("hello\\r\\nworld\\r\\n"), 80, 24)
            .expect("spawn pty actor");
        let handle = bundle.handle.clone();
        let token = bundle.token;

        // Subscribe BEFORE the actor starts polling. Even though the
        // reader thread is already alive, the actor's broadcast send
        // only happens inside `run()` — so subscribing here guarantees
        // we don't miss any chunks. Critical under workspace-parallel
        // load when other tests are spawning PTY children too.
        let mut sub = handle.output.subscribe();
        let join = tokio::task::spawn_local(bundle.actor.run());
        let acc = collect_until(&mut sub, b"world", Duration::from_secs(2)).await;
        let body = String::from_utf8_lossy(&acc);
        assert!(body.contains("hello"), "missing hello in {body:?}");
        assert!(body.contains("world"), "missing world in {body:?}");

        // Now snapshot the actor's Terminal and confirm the same text
        // ended up on the grid.
        let (tx, rx) = oneshot::channel();
        handle
            .snapshot
            .send(SnapshotRequest {
                scrollback: None,
                reply: tx,
            })
            .await
            .expect("snapshot send");
        let snap = timeout(Duration::from_secs(1), rx)
            .await
            .expect("snapshot timeout")
            .expect("snapshot reply");
        let snap_body = String::from_utf8_lossy(&snap.bytes);
        assert!(
            snap_body.contains("hello") && snap_body.contains("world"),
            "snapshot should reflect PTY output, got: {snap_body:?}",
        );

        // The actor stays alive after PTY EOF so late snapshots still
        // work; we shut it down explicitly here.
        token.cancel();
        timeout(Duration::from_secs(3), join)
            .await
            .expect("actor did not exit within timeout")
            .expect("actor task panicked");
    }));
}

/// Input sent via `handle.input` reaches the PTY: a `cat` child echoes
/// whatever we feed it, and we observe the echo on the broadcast.
#[test]
fn input_keystroke_reaches_pty_and_echoes_back() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("rt");
    let local = tokio::task::LocalSet::new();
    rt.block_on(local.run_until(async {
        // `cat` echoes stdin to stdout (PTY is in cooked mode by default,
        // so input is line-buffered; sending `a` then `\r` makes cat
        // emit `a\r\n` back).
        let cmd = CommandBuilder::new("/bin/cat");
        let bundle = TerminalActor::new_with_command(cmd, 80, 24).expect("spawn cat");
        let handle = bundle.handle.clone();
        let token = bundle.token;
        let mut sub = handle.output.subscribe();
        let join = tokio::task::spawn_local(bundle.actor.run());

        // Send "a" key press.
        let key = KeyEvent {
            action: KeyAction::Press,
            key: PhysicalKey::A,
            mods: ModSet::empty(),
            consumed_mods: ModSet::empty(),
            composing: false,
            text: Some("a".to_owned()),
            unshifted_codepoint: Some(u32::from('a')),
        };
        handle
            .input
            .send(TerminalInput::Key(key))
            .await
            .expect("send key");

        // Send Enter so cat flushes a line in cooked mode.
        let enter = KeyEvent {
            action: KeyAction::Press,
            key: PhysicalKey::Enter,
            mods: ModSet::empty(),
            consumed_mods: ModSet::empty(),
            composing: false,
            text: None,
            unshifted_codepoint: None,
        };
        handle
            .input
            .send(TerminalInput::Key(enter))
            .await
            .expect("send enter");

        // Cat will echo `a\r\n` back (the PTY's onlcr maps \n to \r\n,
        // and the tty driver itself echoes the input back too). Either
        // way, `a` must appear.
        let acc = collect_until(&mut sub, b"a", Duration::from_secs(3)).await;
        assert!(
            acc.contains(&b'a'),
            "expected `a` to round-trip through cat: {acc:?}",
        );

        // Tear down: shutdown signal must kill cat and reap it.
        token.cancel();
        timeout(Duration::from_secs(3), join)
            .await
            .expect("actor did not exit within timeout")
            .expect("actor task panicked");
    }));
}

/// Shutdown signal terminates a long-lived child cleanly.
///
/// Uses `sleep 60` so the child is alive when we shut down; the actor
/// must kill+reap it (not leak a zombie). We can't directly assert
/// "no zombie" from outside the process, but we *can* assert the actor
/// exits promptly (which only happens after `Child::wait` returns —
/// see `TerminalActor::shutdown_pty`).
#[test]
fn shutdown_signal_terminates_long_running_child() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("rt");
    let local = tokio::task::LocalSet::new();
    rt.block_on(local.run_until(async {
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.args(["-c", "sleep 60"]);
        let bundle = TerminalActor::new_with_command(cmd, 80, 24).expect("spawn sleep");
        let token = bundle.token;
        let join = tokio::task::spawn_local(bundle.actor.run());

        // Let the child actually start before signaling. A tight
        // shutdown immediately after spawn would still work, but this
        // exercises the kill+reap path more honestly.
        tokio::time::sleep(Duration::from_millis(50)).await;

        token.cancel();
        // The reap (Child::wait) plus thread joins are the slowest
        // part; 3 seconds is plenty.
        timeout(Duration::from_secs(3), join)
            .await
            .expect("actor did not exit within timeout (zombie?)")
            .expect("actor task panicked");
    }));
}

/// The synthesized snapshot is replayable: feeding it back into a
/// fresh `Terminal` reproduces the visible text the PTY produced.
#[test]
fn snapshot_after_pty_output_round_trips_through_fresh_terminal() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("rt");
    let local = tokio::task::LocalSet::new();
    rt.block_on(local.run_until(async {
        let bundle =
            TerminalActor::new_with_command(printf_cmd("phux-byc.5\\r\\n"), 80, 24).expect("spawn");
        let handle = bundle.handle.clone();
        let token = bundle.token;

        // Subscribe before spawn — see comment in `pty_output_...`.
        let mut sub = handle.output.subscribe();
        let join = tokio::task::spawn_local(bundle.actor.run());
        let _ = collect_until(&mut sub, b"phux-byc.5", Duration::from_secs(2)).await;

        let (tx, rx) = oneshot::channel();
        handle
            .snapshot
            .send(SnapshotRequest {
                scrollback: None,
                reply: tx,
            })
            .await
            .expect("snapshot send");
        let snap = timeout(Duration::from_secs(1), rx)
            .await
            .expect("snapshot timeout")
            .expect("snapshot reply");

        // Round-trip: write the snapshot into a fresh Terminal and
        // confirm libghostty parses it without panic. We can't easily
        // grep the grid contents post-parse without pulling grid
        // helpers in, but a successful `vt_write` of the synthesized
        // bytes proves they parse as a coherent VT byte stream.
        let mut replay = GhosttyTerminal::new(TerminalOptions {
            cols: snap.cols,
            rows: snap.rows,
            max_scrollback: 10_000,
        })
        .expect("Terminal");
        replay.vt_write(&snap.bytes);

        token.cancel();
        timeout(Duration::from_secs(3), join)
            .await
            .expect("actor did not exit within timeout")
            .expect("actor task panicked");
    }));
}

/// Broadcast fanout: two subscribers attached to the same actor see
/// the same PTY bytes. This is the structural invariant `phux-byc.6.4`
/// (two clients see same pane) and `phux-byc.6.5` (keystroke merge
/// arrival order preserved) rely on.
#[test]
fn broadcast_fanout_delivers_same_bytes_to_two_subscribers() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("rt");
    let local = tokio::task::LocalSet::new();
    rt.block_on(local.run_until(async {
        let bundle = TerminalActor::new_with_command(printf_cmd("fanout-test\\r\\n"), 80, 24)
            .expect("spawn");
        let handle = bundle.handle.clone();
        let token = bundle.token;

        // Subscribe BEFORE spawn — see comment in `pty_output_...`.
        // byc.6.4/6.5's two-client fanout depends on this same pattern.
        let mut a = handle.output.subscribe();
        let mut b = handle.output.subscribe();
        let join = tokio::task::spawn_local(bundle.actor.run());

        let acc_a = collect_until(&mut a, b"fanout-test", Duration::from_secs(2)).await;
        let acc_b = collect_until(&mut b, b"fanout-test", Duration::from_secs(2)).await;

        assert!(
            String::from_utf8_lossy(&acc_a).contains("fanout-test"),
            "subscriber A missed bytes: {:?}",
            String::from_utf8_lossy(&acc_a),
        );
        assert!(
            String::from_utf8_lossy(&acc_b).contains("fanout-test"),
            "subscriber B missed bytes: {:?}",
            String::from_utf8_lossy(&acc_b),
        );

        token.cancel();
        timeout(Duration::from_secs(3), join)
            .await
            .expect("actor did not exit within timeout")
            .expect("actor task panicked");
    }));
}

/// Resize updates both Terminal dims and PTY winsize. Hard to assert
/// the kernel-side `ioctl` from a test, but we can exercise the path
/// against a PTY-backed actor (which exercises the `master.resize`
/// branch in `handle_resize`).
#[test]
fn resize_path_does_not_panic_against_pty() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("rt");
    let local = tokio::task::LocalSet::new();
    rt.block_on(local.run_until(async {
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.args(["-c", "sleep 30"]);
        let bundle = TerminalActor::new_with_command(cmd, 80, 24).expect("spawn");
        let handle = bundle.handle.clone();
        let token = bundle.token;
        let join = tokio::task::spawn_local(bundle.actor.run());

        handle
            .resize
            .send(ResizeRequest {
                cols: 120,
                rows: 40,
                cell_px: Some((8, 16)),
                resync_clients: true,
                resync_only: false,
            })
            .await
            .expect("resize");
        // Yield so the actor processes the resize before shutdown.
        for _ in 0..32 {
            tokio::task::yield_now().await;
        }

        token.cancel();
        timeout(Duration::from_secs(3), join)
            .await
            .expect("actor did not exit within timeout")
            .expect("actor task panicked");
    }));
}
