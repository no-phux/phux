//! Actor lifecycle tests: construction, seeded colors, ask markers,
//! PTY spawn/adopt, signals, pane kill, input/output interleaving,
//! native-engine requests, pwd, and cancellation.

use super::test_support::*;
use super::*;

#[path = "tests_fixture_groups.rs"]
mod fixture_groups;
use fixture_groups::{FixtureGroup, FixturePane};

#[test]
fn seeded_default_colors_are_installed_before_actor_run() {
    use phux_protocol::caps::{TerminalColor, TerminalDefaultColors};

    let colors = TerminalDefaultColors {
        foreground: TerminalColor {
            r: 208,
            g: 208,
            b: 208,
        },
        background: TerminalColor {
            r: 18,
            g: 24,
            b: 27,
        },
    };
    let bundle = TerminalActor::build_with_token_and_colors(
        80,
        24,
        None,
        test_scrollback(100),
        CancellationToken::new(),
        Some(colors),
    )
    .expect("actor");
    let mut actor = bundle.actor;
    {
        let canonical = actor.terminal.borrow();
        let terminal = canonical.try_terminal().expect("no capture in flight");
        assert_eq!(
            terminal.default_fg_color().expect("foreground"),
            Some(libghostty_vt::style::RgbColor {
                r: 208,
                g: 208,
                b: 208,
            })
        );
        assert_eq!(
            terminal.default_bg_color().expect("background"),
            Some(libghostty_vt::style::RgbColor {
                r: 18,
                g: 24,
                b: 27,
            })
        );
    }

    let (_pty_output, mut pty_input) = actor.install_test_pty_channels();
    let first = b"\x1b]10;?\x1b";
    let second = b"\\\x1b]11;?\x1b\\";
    actor.terminal.borrow_mut().vt_write(first);
    actor.answer_color_queries(first);
    actor.terminal.borrow_mut().vt_write(second);
    actor.answer_color_queries(second);
    assert_eq!(
        pty_input.try_recv().expect("OSC 10 reply").bytes.as_ref(),
        b"\x1b]10;rgb:d0d0/d0d0/d0d0\x1b\\"
    );
    assert_eq!(
        pty_input.try_recv().expect("OSC 11 reply").bytes.as_ref(),
        b"\x1b]11;rgb:1212/1818/1b1b\x1b\\"
    );
}

#[test]
fn ask_marker_parse() {
    for title in ["", "vim README.md", "phux-ask", "phux-ask[q1]"] {
        assert_eq!(AskMarker::parse(title), None, "{title:?}");
    }
    let cases: &[(&str, &str, &str, &[&str])] = &[
        ("phux-ask:Proceed?", "", "Proceed?", &[]),
        (
            "phux-ask[q1]:Deploy to prod??s=Yes|No|Hold",
            "q1",
            "Deploy to prod?",
            &["Yes", "No", "Hold"],
        ),
        ("phux-ask:Ready??s=", "", "Ready?", &[]),
        ("phux-ask:Pick??s=a||b", "", "Pick?", &["a", "b"]),
    ];
    for (title, id, question, suggestions) in cases {
        let marker = AskMarker::parse(title).expect("a phux-ask marker");
        assert_eq!(marker.id, *id, "{title}");
        assert_eq!(marker.question, *question, "{title}");
        assert_eq!(marker.suggestions, *suggestions, "{title}");
    }
}

/// A blank pane's snapshot opens with the reset preamble.
#[test]
fn synthesize_blank_pane_returns_reset_preamble() {
    let bundle = TerminalActor::new(80, 24).expect("new");
    let snap = bundle.actor.synthesize().expect("synthesize");
    assert_eq!(snap.cols, 80);
    assert_eq!(snap.rows, 24);
    assert!(snap.bytes.starts_with(b"\x1b[!p\x1b[2J\x1b[H"));
}

/// Seed bytes reach the synthesized snapshot.
#[test]
fn synthesize_seeded_pane_carries_visible_text() {
    let bundle = TerminalActor::new_with_seed(20, 5, b"hello").expect("new_with_seed");
    let snap = bundle.actor.synthesize().expect("synthesize");
    let body = String::from_utf8_lossy(&snap.bytes);
    assert!(
        body.contains("hello"),
        "synthesized bytes should contain seeded text, got: {body:?}"
    );
}

/// The actor answers `SnapshotRequest` with what the synthesizer produces.
#[tokio::test(flavor = "current_thread")]
async fn actor_responds_to_snapshot_request_on_localset() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let bundle = TerminalActor::new_with_seed(20, 5, b"hi there").expect("new_with_seed");
            let handle = bundle.handle.clone();
            // Dropping the token does not cancel; keep it anyway.
            let _token = bundle.token;
            tokio::task::spawn_local(bundle.actor.run());

            let (reply_tx, reply_rx) = oneshot::channel();
            handle
                .terminal()
                .expect("terminal facet")
                .snapshot
                .send(SnapshotRequest {
                    scrollback: None,
                    max_bytes: usize::MAX,
                    max_frames: usize::MAX,
                    chunk_bytes: 1,
                    reply: reply_tx,
                })
                .await
                .expect("send snapshot request");
            let (snap, base_seq) = reply_rx
                .await
                .expect("snapshot reply")
                .expect("snapshot synthesis");
            assert_eq!(snap.cols, 20);
            assert_eq!(snap.rows, 5);
            assert_eq!(base_seq, 0);
            let body = String::from_utf8_lossy(&snap.bytes);
            assert!(
                body.contains("hi there"),
                "actor-synthesized bytes should contain seeded text"
            );
        })
        .await;
}

/// A PTY-less actor's upgrade handle has a snapshot but no descriptors.
#[tokio::test(flavor = "current_thread")]
async fn upgrade_handle_no_pty_has_snapshot_but_no_descriptors() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let bundle = TerminalActor::new_with_seed(20, 5, b"seeded").expect("new_with_seed");
            let handle = bundle.handle.clone();
            let _token = bundle.token;
            tokio::task::spawn_local(bundle.actor.run());

            let (reply_tx, reply_rx) = oneshot::channel();
            handle
                .upgrade
                .send(UpgradeHandleRequest { reply: reply_tx })
                .await
                .expect("send upgrade request");
            let h = reply_rx.await.expect("upgrade reply");
            assert!(h.master_fd.is_none());
            assert_eq!(h.child_pid, None);
            assert_eq!((h.cols, h.rows), (20, 5));
            assert!(
                String::from_utf8_lossy(&h.vt_replay_bytes).contains("seeded"),
                "replay snapshot should carry the seeded text"
            );
        })
        .await;
}

/// A PTY-backed actor's upgrade handle carries the master fd and child pid.
#[tokio::test(flavor = "current_thread")]
async fn upgrade_handle_with_pty_exposes_fd_and_pid() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut cmd = portable_pty::CommandBuilder::new("sleep");
            cmd.arg("30");
            let bundle = TerminalActor::new_with_command(cmd, 80, 24).expect("new_with_command");
            let handle = bundle.handle.clone();
            let token = bundle.token.clone();
            tokio::task::spawn_local(bundle.actor.run());

            let (reply_tx, reply_rx) = oneshot::channel();
            handle
                .upgrade
                .send(UpgradeHandleRequest { reply: reply_tx })
                .await
                .expect("send upgrade request");
            let h = reply_rx.await.expect("upgrade reply");
            assert!(h.master_fd.is_some(), "PTY actor should expose a master fd");
            assert!(h.child_pid.is_some(), "PTY actor should expose a child pid");

            // Cancel so the actor reaps the `sleep` child.
            token.cancel();
        })
        .await;
}

/// `new_with_adopted_pty` replays the seed, exposes the adopted child, and
/// surfaces its live output.
#[tokio::test(flavor = "current_thread")]
async fn adopted_actor_replays_seed_and_serves_live_pty() {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    use std::io::Write;
    use std::os::fd::{BorrowedFd, IntoRawFd};

    #[allow(
        clippy::future_not_send,
        reason = "current-thread test helper; the actor's TerminalHandle is intentionally !Sync"
    )]
    async fn snapshot(handle: &ResourceHandle) -> String {
        let (reply, rx) = oneshot::channel();
        handle
            .terminal()
            .expect("terminal facet")
            .snapshot
            .send(SnapshotRequest {
                scrollback: None,
                max_bytes: usize::MAX,
                max_frames: usize::MAX,
                chunk_bytes: 1,
                reply,
            })
            .await
            .expect("send snapshot");
        String::from_utf8_lossy(
            &rx.await
                .expect("snapshot reply")
                .expect("snapshot synthesis")
                .0
                .bytes,
        )
        .into_owned()
    }

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            // A real PTY with a `cat` child that echoes input.
            let sys = native_pty_system();
            let pair = sys
                .openpty(PtySize {
                    rows: 5,
                    cols: 20,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .expect("openpty");
            let child = pair
                .slave
                .spawn_command(CommandBuilder::new("cat"))
                .expect("spawn cat");
            drop(pair.slave);
            let pid = i32::try_from(child.process_id().expect("pid")).expect("pid fits i32");
            let master_fd = pair.master.as_raw_fd().expect("master fd");
            // An owned duplicate of the master for the actor to adopt; the
            // test keeps `pair.master` to write into the PTY.
            // SAFETY: `master_fd` is open and outlives this borrow.
            let dup_fd = unsafe { BorrowedFd::borrow_raw(master_fd) }
                .try_clone_to_owned()
                .expect("dup master")
                .into_raw_fd();
            let mut writer = pair.master.take_writer().expect("take writer");
            drop(child); // the adopted actor becomes the sole reaper.

            let bundle = TerminalActor::new_with_adopted_pty(
                dup_fd,
                pid,
                20,
                5,
                test_scrollback(1000),
                CancellationToken::new(),
                b"resumed",
            )
            .expect("new_with_adopted_pty");
            let handle = bundle.handle.clone();
            let token = bundle.token.clone();
            tokio::task::spawn_local(bundle.actor.run());

            // Seed replayed synchronously into the rebuilt grid.
            assert!(
                snapshot(&handle).await.contains("resumed"),
                "adopted actor should replay the seed snapshot"
            );

            // The adopted child is live and wired into the actor.
            let (reply, rx) = oneshot::channel();
            handle
                .upgrade
                .send(UpgradeHandleRequest { reply })
                .await
                .expect("send upgrade");
            let h = rx.await.expect("upgrade reply");
            assert_eq!(h.child_pid, Some(pid));
            assert!(h.master_fd.is_some());

            // `cat` echoes; the adopted actor's grid shows it.
            writer.write_all(b"ping\n").expect("write to pty");
            writer.flush().expect("flush");
            let mut saw = false;
            for _ in 0..40 {
                if snapshot(&handle).await.contains("ping") {
                    saw = true;
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            assert!(saw, "adopted actor should surface the child's echo");

            token.cancel();
        })
        .await;
}

/// ADR-0033: Freeze/Resume flip the lifecycle and broadcast
/// `TerminalControl`; Kill terminates the child (EOF fires exit notify).
#[tokio::test(flavor = "current_thread")]
#[allow(
    clippy::too_many_lines,
    reason = "end-to-end PTY signal test: three signal round-trips plus subscriber setup"
)]
async fn signal_freezes_resumes_and_kills_the_child() {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    use std::os::fd::{BorrowedFd, IntoRawFd};

    #[allow(
        clippy::future_not_send,
        reason = "current-thread test helper; the actor's TerminalHandle is intentionally !Sync"
    )]
    async fn next_control(
        rx: &mut mpsc::Receiver<crate::resource::event_sink::Emitted>,
    ) -> (
        ControlAction,
        ResourceLifecycle,
        Option<phux_protocol::ids::IdempotencyKey>,
    ) {
        // Skip incidental Dirty/Idle events.
        let scan = async {
            loop {
                let emitted = rx.recv().await.expect("event channel open");
                if let AgentEvent::TerminalControl {
                    action, lifecycle, ..
                } = emitted.event
                {
                    return (action, lifecycle, emitted.operation_id);
                }
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), scan)
            .await
            .expect("a TerminalControl event should arrive")
    }

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let sys = native_pty_system();
            let pair = sys
                .openpty(PtySize {
                    rows: 5,
                    cols: 20,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .expect("openpty");
            let child = pair
                .slave
                .spawn_command(CommandBuilder::new("cat"))
                .expect("spawn cat");
            drop(pair.slave);
            let pid = i32::try_from(child.process_id().expect("pid")).expect("pid fits i32");
            let master_fd = pair.master.as_raw_fd().expect("master fd");
            // SAFETY: `master_fd` is open and outlives this borrow.
            let dup_fd = unsafe { BorrowedFd::borrow_raw(master_fd) }
                .try_clone_to_owned()
                .expect("dup master")
                .into_raw_fd();
            drop(child); // the adopted actor becomes the sole reaper.

            let bundle = TerminalActor::new_with_adopted_pty(
                dup_fd,
                pid,
                20,
                5,
                test_scrollback(1000),
                CancellationToken::new(),
                b"",
            )
            .expect("new_with_adopted_pty");
            let handle = bundle.handle.clone();
            let token = bundle.token.clone();
            let mut exit_rx = bundle.exit_notify.expect("exit notify");
            // Observe the TerminalControl events the runtime would journal.
            let mut actor = bundle.actor;
            let (evt_tx, mut evt_rx) = mpsc::channel::<crate::resource::event_sink::Emitted>(64);
            actor.set_event_sink(evt_tx);
            tokio::task::spawn_local(actor.run());

            let by = phux_protocol::ids::ClientId::new(7);

            // Freeze → Frozen.
            let (reply, ack) = oneshot::channel();
            handle
                .control
                .send(ControlRequest::Signal {
                    signal: TerminalSignal::Freeze,
                    input_holder: None,
                    by,
                    reply,
                    operation_id: phux_protocol::ids::IdempotencyKey::new([4; 16]),
                })
                .await
                .expect("send freeze");
            ack.await.expect("freeze ack").expect("freeze delivered");
            let (action, lifecycle, operation_id) = next_control(&mut evt_rx).await;
            assert_eq!(action, ControlAction::Frozen);
            assert_eq!(lifecycle, ResourceLifecycle::Frozen);
            assert_eq!(
                operation_id,
                phux_protocol::ids::IdempotencyKey::new([4; 16]),
                "a keyed signal's terminal_control carries its operation_id (L1 §5.1.1)"
            );

            // Resume → Running.
            let (reply, ack) = oneshot::channel();
            handle
                .control
                .send(ControlRequest::Signal {
                    signal: TerminalSignal::Resume,
                    input_holder: None,
                    by,
                    reply,
                    operation_id: None,
                })
                .await
                .expect("send resume");
            ack.await.expect("resume ack").expect("resume delivered");
            let (action, lifecycle, operation_id) = next_control(&mut evt_rx).await;
            assert_eq!(operation_id, None, "an unkeyed signal carries none");
            assert_eq!(action, ControlAction::Resumed);
            assert_eq!(lifecycle, ResourceLifecycle::Running);

            // Kill → the child actually dies; its EOF fires the exit notify.
            let (reply, ack) = oneshot::channel();
            handle
                .control
                .send(ControlRequest::Signal {
                    signal: TerminalSignal::Kill,
                    input_holder: None,
                    by,
                    reply,
                    operation_id: None,
                })
                .await
                .expect("send kill");
            ack.await.expect("kill ack").expect("kill delivered");
            tokio::time::timeout(std::time::Duration::from_secs(5), &mut exit_rx)
                .await
                .expect("killed child should exit and notify")
                .expect("exit notify channel");

            token.cancel();
        })
        .await;
}

/// Test-only hangup ceiling for the flush fixtures (a deadline, so idle
/// traps still return on the first poll).
const CONTENDED_FLUSH_GRACE: std::time::Duration = std::time::Duration::from_millis(2500);

/// More than the reader channel plus kernel buffer can absorb, derived from
/// the production constants.
const TERMINAL_FLUSH_BYTES: usize =
    super::spawn::PTY_CHANNEL_DEPTH * super::spawn::PTY_READ_CHUNK + 512 * 1024;

/// Poll until `path` exists or 30 s pass (shell scheduling, not the flush).
async fn wait_until_fixture_exists(path: &std::path::Path) -> bool {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while !path.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .is_ok()
}

/// Poll the SIGHUP flush marker until it lands or the grace expires.
async fn wait_for_flush_marker(path: &std::path::Path) -> String {
    let started = tokio::time::Instant::now();
    loop {
        let body = std::fs::read_to_string(path).unwrap_or_default();
        if body.contains("flushed")
            || started.elapsed() >= CONTENDED_FLUSH_GRACE + std::time::Duration::from_millis(500)
        {
            return body;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

/// Killing a pane gives a foreground job in its own process group a chance
/// to flush before it dies. The hangup ceiling is stretched for load;
/// nextest gives this test all CPUs.
#[tokio::test(flavor = "current_thread")]
async fn pane_kill_lets_foreground_process_flush_before_death() {
    use portable_pty::CommandBuilder;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let marker = dir.path().join("flushed");
            let armed = dir.path().join("armed");
            let foreground = FixtureGroup::new(dir.path(), "PHUX_TEST_FOREGROUND");

            // The script signals once its HUP trap is installed; a SIGHUP
            // before `trap` would hit the default disposition.
            let script = dir.path().join("foreground.sh");
            // A builtin wait lets HUP run the trap directly.
            std::fs::write(
                &script,
                foreground.script(
                    "trap 'printf flushed > \"$PHUX_TEST_MARKER\"; exit 0' HUP\n\
                     printf armed > \"$PHUX_TEST_ARMED\"\n\
                     while :; do read _; done\n",
                ),
            )
            .expect("write foreground script");

            // Monitor mode: the script is a foreground job in its own group.
            let mut cmd = CommandBuilder::new("/bin/sh");
            cmd.arg("-c");
            cmd.arg(format!(
                "set -m; trap ':' HUP; /bin/sh {}",
                script.display()
            ));
            cmd.env("PHUX_TEST_MARKER", &marker);
            cmd.env("PHUX_TEST_ARMED", &armed);
            foreground.configure(&mut cmd);

            let token = CancellationToken::new();
            let bundle = TerminalActor::build_with_token(
                20,
                5,
                Some(cmd),
                test_scrollback(1000),
                token.clone(),
            )
            .expect("build actor");
            let actor = bundle.actor;
            let pty = actor.pty.as_ref().expect("test actor has PTY");
            let shell_group = i32::try_from(pty.child.process_id().expect("shell pid"))
                .expect("shell pid fits i32");
            let _pane_cleanup = FixturePane(shell_group);
            let master = std::sync::Arc::clone(&pty.master);
            let run = tokio::task::spawn_local(actor.run());

            // One barrier: `armed` is written after `trap` by the process
            // `set -m` put in its own group, so it implies both
            // preconditions. The generous budget covers process startup
            // under load, not the subject under test.
            tokio::time::timeout(std::time::Duration::from_secs(30), async {
                while !armed.exists() {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .expect(
                "foreground job never installed its SIGHUP trap: the fixture shells did not \
                     get scheduled, which is an environment problem (machine load), not a \
                     failure of the flush-before-death path this test covers",
            );
            let _grace = stretch_pane_kill_grace(CONTENDED_FLUSH_GRACE);

            // Armed, so the foreground group is settled.
            let foreground_group = master
                .lock()
                .expect("master lock")
                .process_group_leader()
                .expect("an armed foreground job has a process group");
            assert_ne!(
                foreground_group, shell_group,
                "the fixture must reproduce interactive job-control topology: a foreground \
                     job in a group distinct from the shell's",
            );

            // Kill the pane. The actor's shutdown runs SIGHUP + grace.
            token.cancel();
            let body = wait_for_flush_marker(&marker).await;
            tokio::time::timeout(std::time::Duration::from_secs(5), run)
                .await
                .expect("actor shutdown timed out")
                .expect("actor task failed");

            assert!(
                body.contains("flushed"),
                "foreground process must run its SIGHUP flush handler before \
                     the pane is killed; marker={body:?}",
            );
        })
        .await;
}

/// The hangup grace lets a foreground job finish flushing to the terminal:
/// the reader must keep draining during teardown, or the child wedges in
/// `write(2)` and is hard-killed before the marker appears.
///
/// Fixture requirements: the outer shell survives SIGHUP (a dying session
/// leader revokes the tty, masking the bug); the trap masks further hangups
/// (so `cat` survives the second SIGHUP); and `&&` so the marker means
/// every byte arrived.
#[tokio::test(flavor = "current_thread")]
async fn pane_kill_lets_a_terminal_flush_finish_inside_the_grace() {
    use portable_pty::CommandBuilder;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let marker = dir.path().join("flushed");
            let started = dir.path().join("started");
            let armed = dir.path().join("armed");
            let foreground = FixtureGroup::new(dir.path(), "PHUX_TEST_FOREGROUND");
            let payload = dir.path().join("payload");
            let status = dir.path().join("status");
            let stderr = dir.path().join("err");
            std::fs::write(&payload, vec![b'.'; TERMINAL_FLUSH_BYTES]).expect("write flush payload");

            // `cat` moves megabytes within the 500 ms budget; a builtin
            // `read` lets HUP run the trap at once.
            let script = dir.path().join("foreground.sh");
            std::fs::write(
                &script,
                foreground.script(
                    "trap 'trap \"\" HUP; printf flushing > \"$PHUX_TEST_STARTED\"; \
                     cat \"$PHUX_TEST_PAYLOAD\" 2>\"$PHUX_TEST_ERR\"; s=$?; \
                     printf %s \"$s\" > \"$PHUX_TEST_STATUS\"; \
                     [ \"$s\" -eq 0 ] && printf flushed > \"$PHUX_TEST_MARKER\"; exit 0' HUP\n\
                     printf armed > \"$PHUX_TEST_ARMED\"\n\
                     while :; do read _; done\n",
                ),
            )
            .expect("write foreground script");

            let mut cmd = CommandBuilder::new("/bin/sh");
            cmd.arg("-c");
            // A caught HUP on the session leader resets across `exec`, so the
            // inner shell installs its own trap while this shell keeps the
            // tty alive.
            cmd.arg(format!(
                "set -m; trap ':' HUP; /bin/sh {}",
                script.display()
            ));
            cmd.env("PHUX_TEST_MARKER", &marker);
            cmd.env("PHUX_TEST_STARTED", &started);
            cmd.env("PHUX_TEST_ARMED", &armed);
            cmd.env("PHUX_TEST_PAYLOAD", &payload);
            cmd.env("PHUX_TEST_STATUS", &status);
            cmd.env("PHUX_TEST_ERR", &stderr);
            foreground.configure(&mut cmd);

            let token = CancellationToken::new();
            let bundle = TerminalActor::build_with_token(
                80,
                24,
                Some(cmd),
                test_scrollback(1000),
                token.clone(),
            )
            .expect("build actor");
            let actor = bundle.actor;
            let pty = actor.pty.as_ref().expect("test actor has PTY");
            let shell_group = i32::try_from(pty.child.process_id().expect("shell pid"))
                .expect("shell pid fits i32");
            let _pane_cleanup = FixturePane(shell_group);
            let master = std::sync::Arc::clone(&pty.master);
            let run = tokio::task::spawn_local(actor.run());

            assert!(
                wait_until_fixture_exists(&armed).await,
                "foreground job never installed its SIGHUP trap: the fixture shells did not \
                     get scheduled, which is an environment problem (machine load), not a \
                     failure of the flush-before-death path this test covers",
            );
            let _grace = stretch_pane_kill_grace_after(CONTENDED_FLUSH_GRACE, Some(&started));

            let foreground_group = master
                .lock()
                .expect("master lock")
                .process_group_leader()
                .expect("an armed foreground job has a process group");
            assert_ne!(
                foreground_group, shell_group,
                "the fixture must reproduce interactive job-control topology: a foreground \
                     job in a group distinct from the shell's",
            );

            let killed_at = std::time::Instant::now();
            token.cancel();
            assert!(
                wait_until_fixture_exists(&started).await,
                "SIGHUP trap never started: the fixture shell did not get scheduled \
                     after hangup, which is an environment problem (machine load), not a \
                     failure of the flush-before-death path this test covers",
            );
            let body = wait_for_flush_marker(&marker).await;
            tokio::time::timeout(std::time::Duration::from_secs(10), run)
                .await
                .expect("actor shutdown stalled: the pane-kill path did not complete")
                .expect("actor task failed");
            let shutdown_took = killed_at.elapsed();
            let cat_status = std::fs::read_to_string(&status).unwrap_or_default();
            let cat_err = std::fs::read_to_string(&stderr).unwrap_or_default();
            assert!(
                body.contains("flushed"),
                "a foreground job flushing {TERMINAL_FLUSH_BYTES} bytes to the TERMINAL must finish \
                     inside the hangup grace. An empty marker with a non-zero cat status means \
                     it could not write: either it blocked against an undrained PTY and was \
                     hard-killed mid-flush, or the terminal was revoked out from under it. \
                     marker={body:?} cat_status={cat_status:?} cat_err={cat_err:?} \
                     shutdown_took={shutdown_took:?}",
            );
        })
        .await;
}

/// A process that escaped the snapshotted groups and holds the slave open
/// cannot hang the server: teardown drops the PTY before a bounded join.
///
/// On macOS this passes even without the fix (the kernel revokes the tty
/// when the session leader exits); on Linux it is a real gate. The
/// kernel-independent property is pinned by
/// `teardown_policy_tests::a_thread_that_never_exits_is_detached_rather_than_joined`.
#[tokio::test(flavor = "current_thread")]
async fn pane_kill_is_bounded_when_a_detached_process_holds_the_slave_open() {
    use portable_pty::CommandBuilder;

    let ceiling = PANE_KILL_GRACE * 8;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let holder_cleanup = FixtureGroup::new(dir.path(), "PHUX_TEST_HOLDER");
            let holder = holder_cleanup.pid_file().to_owned();
            let script = dir.path().join("holder.sh");
            // No `exec`: the holder shell keeps naming this tempdir for
            // cleanup. Shell and sleep both hold the slave open.
            std::fs::write(&script, holder_cleanup.script("/bin/sleep 3600\n"))
                .expect("write holder script");
            let mut cmd = CommandBuilder::new("/bin/sh");
            cmd.arg("-c");
            // The pane's own child dies to the hangup, leaving only the holder.
            cmd.arg(format!("set -m; /bin/sh {} & exec cat", script.display()));
            holder_cleanup.configure(&mut cmd);

            let token = CancellationToken::new();
            let bundle = TerminalActor::build_with_token(
                80,
                24,
                Some(cmd),
                test_scrollback(1000),
                token.clone(),
            )
            .expect("build actor");
            let actor = bundle.actor;
            let _pane_cleanup = FixturePane::for_actor(&actor);
            let run = tokio::task::spawn_local(actor.run());

            // Wait for the holder's pid file.
            tokio::time::timeout(std::time::Duration::from_secs(30), async {
                while !holder.exists() {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .expect(
                "the fixture never forked its detached holder: an environment problem \
                     (machine load), not a failure of the path this test covers",
            );

            let killed_at = std::time::Instant::now();
            token.cancel();
            tokio::time::timeout(ceiling, run)
                .await
                .expect(
                    "pane teardown never completed while a detached process held the slave \
                     open: the actor is blocked joining a reader parked in read(2), and on a \
                     shared current-thread runtime that is every pane on the server",
                )
                .expect("actor task failed");
            let shutdown_took = killed_at.elapsed();

            assert!(
                shutdown_took < ceiling,
                "teardown must be bounded by our own budgets, not by whether some process \
                     we never signalled decides to close the slave; took {shutdown_took:?}",
            );
        })
        .await;
}

/// A child that refuses to die cannot hang the server: the spewer escapes
/// into its own process group (unreachable by the signalling path), so
/// teardown finishes because it is bounded. The reap budget itself is
/// covered by `teardown_policy_tests`.
#[tokio::test(flavor = "current_thread")]
async fn pane_kill_is_bounded_when_an_escaped_child_ignores_the_hangup_and_spews() {
    use portable_pty::CommandBuilder;

    let ceiling = PANE_KILL_GRACE * 8;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let armed = dir.path().join("armed");
            let payload = dir.path().join("payload");
            let foreground = FixtureGroup::new(dir.path(), "PHUX_TEST_FOREGROUND");
            let holder_cleanup = FixtureGroup::new(dir.path(), "PHUX_TEST_HOLDER");
            std::fs::write(&payload, vec![b'.'; 256 * 1024]).expect("write payload");
            let spewer = dir.path().join("holder.sh");
            std::fs::write(
                &spewer,
                holder_cleanup
                    .script("trap '' HUP\nwhile :; do cat \"$PHUX_TEST_PAYLOAD\" || exit; done\n"),
            )
            .expect("write holder script");

            // `trap '' HUP` survives `exec`; `set -m` puts the loop in its
            // own group. It ignores hangup, never sees the hard kill, and
            // writes nonstop.
            let script = dir.path().join("foreground.sh");
            std::fs::write(
                &script,
                foreground.script(
                    "trap '' HUP\n\
                     set -m\n\
                     /bin/sh \"$PHUX_TEST_SPEWER\" &\n\
                     while [ ! -s \"$PHUX_TEST_HOLDER\" ]; do /bin/sleep 0.01; done\n\
                     printf armed > \"$PHUX_TEST_ARMED\"\n\
                     wait\n",
                ),
            )
            .expect("write foreground script");

            let mut cmd = CommandBuilder::new("/bin/sh");
            cmd.arg("-c");
            cmd.arg(format!("set -m; trap '' HUP; /bin/sh {}", script.display()));
            cmd.env("PHUX_TEST_ARMED", &armed);
            cmd.env("PHUX_TEST_PAYLOAD", &payload);
            cmd.env("PHUX_TEST_SPEWER", &spewer);
            foreground.configure(&mut cmd);
            holder_cleanup.configure(&mut cmd);

            let token = CancellationToken::new();
            let bundle = TerminalActor::build_with_token(
                80,
                24,
                Some(cmd),
                test_scrollback(1000),
                token.clone(),
            )
            .expect("build actor");
            let actor = bundle.actor;
            let _pane_cleanup = FixturePane::for_actor(&actor);
            let run = tokio::task::spawn_local(actor.run());

            tokio::time::timeout(std::time::Duration::from_secs(30), async {
                while !armed.exists() {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .expect(
                "the fixture shell never started: an environment problem (machine load), \
                     not a failure of the path this test covers",
            );

            let killed_at = std::time::Instant::now();
            token.cancel();
            tokio::time::timeout(ceiling, run)
                .await
                .expect(
                    "pane teardown never completed for a child that escaped the signalled \
                     groups and keeps writing: the actor is blocked, and on a shared \
                     current-thread runtime every pane is blocked with it",
                )
                .expect("actor task failed");
            let shutdown_took = killed_at.elapsed();

            assert!(
                shutdown_took < ceiling,
                "teardown must be bounded by the grace, reap and join budgets, not by the \
                     child's willingness to exit; took {shutdown_took:?}",
            );
            // `armed` is written only after the holder pid is on record.
        })
        .await;
}

/// A queued keystroke interleaves with a large pending output burst instead
/// of waiting for it to drain.
#[tokio::test(flavor = "current_thread")]
async fn input_interleaves_with_a_large_pty_output_burst() {
    use phux_protocol::input::paste::{PasteEvent, PasteTrust};

    const CHUNK_LEN: usize = 4096;
    const CHUNK_COUNT: usize = 200;
    const BURST_BYTES: usize = CHUNK_LEN * CHUNK_COUNT;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let bundle = TerminalActor::new(80, 24).expect("new");
            let handle = bundle.handle.clone();
            let token = bundle.token.clone();
            let mut actor = bundle.actor;
            let (pty_evt_tx, mut writer_rx) = actor.install_test_pty_channels();

            // Subscribe first so no frame is missed.
            let mut out_rx = handle.output.subscribe();

            // A burst spanning many capped writes.
            let chunk = Bytes::from(vec![b'x'; CHUNK_LEN]);
            for _ in 0..CHUNK_COUNT {
                pty_evt_tx
                    .try_send(PtyEvent::Bytes {
                        chunk: chunk.clone(),
                        read_at: std::time::Instant::now(),
                    })
                    .expect("queue burst");
            }
            // With mode 2004 off, a trusted paste of "x" encodes to "x".
            handle
                .terminal()
                .expect("terminal facet")
                .input
                .send(TerminalInput::Paste(PasteEvent {
                    trust: PasteTrust::Trusted,
                    data: b"x".to_vec(),
                }))
                .await
                .expect("queue input");

            tokio::task::spawn_local(actor.run());

            // The timeout is only a backstop; the byte count below proves
            // "mid-burst".
            let got = tokio::time::timeout(ACTOR_EXIT_DEADLINE, writer_rx.recv())
                .await
                .expect("input must be serviced mid-burst, not after it");
            let got_bytes = got.map(|request| request.bytes);
            assert_eq!(
                got_bytes.as_deref(),
                Some(b"x".as_ref()),
                "queued keystroke should reach the PTY writer while the burst drains",
            );

            // Count emitted bytes, crediting lagged frames at the cap so
            // the check never under-reports.
            let mut emitted: usize = 0;
            loop {
                match out_rx.try_recv() {
                    Ok(PaneOutput::Live { bytes, .. } | PaneOutput::Resync { bytes, .. }) => {
                        emitted += bytes.len();
                    }
                    Ok(PaneOutput::Control { .. }) => {}
                    Err(tokio::sync::broadcast::error::TryRecvError::Lagged(n)) => {
                        let skipped = usize::try_from(n).unwrap_or(usize::MAX);
                        emitted += skipped.saturating_mul(MAX_PTY_COALESCE_BYTES);
                    }
                    Err(_) => break,
                }
            }
            token.cancel();
            assert!(
                emitted < BURST_BYTES,
                "input must land mid-burst: cumulative output {emitted} should be \
                     below the full burst {BURST_BYTES}",
            );
        })
        .await;
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[test]
fn native_step_allocation_never_exceeds_remaining_capture_budget() {
    assert_eq!(native_step_bytes(10, 7, 8).expect("remaining step"), 3);
    assert_eq!(native_step_bytes(10, 0, 8).expect("full step"), 8);
    assert!(matches!(
        native_step_bytes(10, 10, 8),
        Err(crate::native_state::NativeStateError::LimitExceeded)
    ));
}

/// `defaults.history-bytes` reaches libghostty and binds (ADR-0094).
#[test]
fn configured_history_bytes_decides_retained_scrollback() {
    fn retained_rows(bytes: u32) -> usize {
        let bundle = TerminalActor::build_with_token(
            200,
            50,
            None,
            phux_config::ScrollbackLimits::new(500_000, bytes),
            CancellationToken::new(),
        )
        .expect("actor");
        {
            let mut terminal = bundle.actor.terminal.borrow_mut();
            for row in 0..40_000 {
                terminal.vt_write(format!("scrollback row {row}\r\n").as_bytes());
            }
        }
        let canonical = bundle.actor.terminal.borrow();
        let rows = canonical
            .try_terminal()
            .expect("no capture in flight")
            .scrollback_rows()
            .expect("retained scrollback rows");
        drop(canonical);
        bundle.token.cancel();
        rows
    }

    let shallow = retained_rows(phux_config::DEFAULT_HISTORY_BYTES);
    let deep = retained_rows(8 * 1024 * 1024);
    assert!(
        deep > shallow * 2,
        "a 4x byte bound must retain materially more history: {shallow} -> {deep}",
    );
    assert!(
        shallow > 0,
        "the shipped byte bound must still retain history, saw {shallow}",
    );
}

/// The bootstrap scratch is sized for one record, not the staging budget.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[test]
fn initial_native_scratch_is_one_record_window_not_the_staging_budget() {
    use super::native::{INITIAL_NATIVE_SCRATCH_BYTES, initial_native_scratch_bytes};

    assert_eq!(
        initial_native_scratch_bytes(crate::native_state::MAX_NATIVE_PREFIX_BYTES),
        INITIAL_NATIVE_SCRATCH_BYTES,
    );
    assert_eq!(initial_native_scratch_bytes(4_096), 4_096);
    assert_eq!(
        initial_native_scratch_bytes(INITIAL_NATIVE_SCRATCH_BYTES),
        INITIAL_NATIVE_SCRATCH_BYTES,
    );
}

/// Scrollback pushes the prefix past the 64 KiB seed window, so the
/// `OutOfSpace` retry must widen scratch.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
async fn progressive_native_ready_stays_within_one_seed_window() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let bundle = TerminalActor::new(200, 50).expect("new actor");
            let handle = bundle.handle.clone();
            let token = bundle.token.clone();
            let actor = bundle.actor;
            {
                let mut terminal = actor.terminal.borrow_mut();
                for row in 0..2_000 {
                    terminal.vt_write(format!("scratch-row-{row:05} {row:<180}\r\n").as_bytes());
                }
            }
            let (reply, replied) = oneshot::channel();
            handle
                .terminal()
                .expect("terminal facet")
                .native_bootstrap
                .send(NativeBootstrapRequest {
                    owner: 3,
                    terminal_id: phux_protocol::ids::ResourceId::local(1),
                    stream_id: phux_protocol::ids::StreamId::new(1).expect("stream id"),
                    bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("bootstrap id"),
                    limits: phux_protocol::caps::BootstrapLimits::new(
                        phux_protocol::MAX_BOOTSTRAP_CHUNK_BYTES,
                        phux_protocol::DEFAULT_HISTORY_PAGE_BYTES,
                    )
                    .expect("wide negotiated bootstrap bound"),
                    max_bytes: crate::native_state::MAX_NATIVE_PREFIX_BYTES,
                    max_frames: crate::native_state::MAX_NATIVE_PREFIX_CHUNKS + 2,
                    reply,
                })
                .await
                .expect("send native request");
            let run = tokio::task::spawn_local(actor.run());
            let capture = tokio::time::timeout(ACTOR_EXIT_DEADLINE, replied)
                .await
                .expect("native bootstrap stalled")
                .expect("native reply dropped")
                .expect("native capture");
            let widest = capture
                .frames
                .iter()
                .filter_map(|frame| match frame {
                    FrameKind::BootstrapChunk { payload, .. } => Some(payload.len()),
                    _ => None,
                })
                .max()
                .expect("at least one bootstrap chunk");
            assert!(
                widest <= super::native::INITIAL_NATIVE_SCRATCH_BYTES,
                "progressive READY record exceeded the {} byte seed window: {widest}",
                super::native::INITIAL_NATIVE_SCRATCH_BYTES,
            );
            assert!(
                capture
                    .frames
                    .iter()
                    .any(|frame| matches!(frame, FrameKind::BootstrapReady { .. })),
                "bootstrap must reach READY without eager history",
            );
            token.cancel();
            run.await.expect("actor run");
        })
        .await;
}

/// Every reader of the canonical terminal degrades, rather than aborts,
/// while a capture holds it.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
async fn every_terminal_reader_degrades_while_a_capture_holds_it() {
    let bundle = TerminalActor::new(80, 24).expect("new actor");
    let mut actor = bundle.actor;
    let (reply, _replied) = oneshot::channel();
    actor.start_native_bootstrap(NativeBootstrapRequest {
        owner: 31,
        terminal_id: phux_protocol::ids::ResourceId::local(1),
        stream_id: phux_protocol::ids::StreamId::new(1).expect("stream id"),
        bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("bootstrap id"),
        limits: phux_protocol::caps::BootstrapLimits::default(),
        max_bytes: crate::native_state::MAX_NATIVE_PREFIX_BYTES,
        max_frames: crate::native_state::MAX_NATIVE_PREFIX_CHUNKS + 2,
        reply,
    });
    assert!(
        actor.terminal.borrow().try_terminal().is_none(),
        "the capture must actually hold the terminal for this to test anything",
    );

    // None may panic; each must report the loan.
    assert!(
        matches!(
            actor.synthesize(),
            Err(crate::grid::SynthesisError::TerminalUnavailable)
        ),
        "snapshot synthesis must refuse, not abort",
    );
    assert!(
        matches!(
            actor.screen_state(1, None, false, 0),
            Err(crate::grid::SynthesisError::TerminalUnavailable)
        ),
        "GET_SCREEN must refuse, not abort",
    );
    assert!(
        actor.viewport_lines().is_none(),
        "the agent detector must skip its tick, not abort",
    );
    assert!(
        !actor.refresh_title(),
        "a title read must report unchanged, not abort",
    );
    // Void readers: the assertion is simply that these return at all.
    actor.publish_input_snapshot();

    // ...and the terminal is usable again once the capture lands.
    actor.land_native_cuts();
    assert!(
        actor.terminal.borrow().try_terminal().is_some(),
        "landing the cut must return the terminal",
    );
    assert!(
        actor.synthesize().is_ok(),
        "synthesis works once it is back"
    );
}

/// `flush_final_gap_resync` runs outside the bootstrap guards; with a
/// capture in flight and a queued resize it must land the cut first.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
async fn teardown_drain_survives_an_in_flight_native_capture() {
    let bundle = TerminalActor::new(80, 24).expect("new actor");
    let mut actor = bundle.actor;
    let (reply, replied) = oneshot::channel();
    actor.start_native_bootstrap(NativeBootstrapRequest {
        owner: 31,
        terminal_id: phux_protocol::ids::ResourceId::local(1),
        stream_id: phux_protocol::ids::StreamId::new(1).expect("stream id"),
        bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("bootstrap id"),
        limits: phux_protocol::caps::BootstrapLimits::default(),
        max_bytes: crate::native_state::MAX_NATIVE_PREFIX_BYTES,
        max_frames: crate::native_state::MAX_NATIVE_PREFIX_CHUNKS + 2,
        reply,
    });
    assert!(
        actor.pending_native_bootstrap.is_some(),
        "the capture must be in flight for this to be the race under test",
    );

    // The queued resize is what reaches the terminal during the drain.
    bundle
        .handle
        .terminal()
        .expect("terminal facet")
        .resize
        .send(super::ResizeRequest {
            cols: 100,
            rows: 40,
            cell_px: None,
            resync_clients: true,
            resync_only: false,
            resync_for: None,
        })
        .await
        .expect("queue a resize behind the capture");

    let mut resync = super::run_loop::ResyncDebounce::idle();
    let _ = actor.flush_final_gap_resync(&mut resync);

    assert!(
        actor.pending_native_bootstrap.is_none(),
        "the in-flight cut must be landed, not left holding the terminal",
    );
    assert!(
        replied.await.expect("capture reply").is_err(),
        "the waiter must be answered, not left hanging on a cut that was discarded",
    );
    actor.terminal.borrow_mut().vt_write(b"ok");
}

/// Resetting for a replacement child while a capture holds the terminal
/// must land the cut first instead of aborting the server.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
async fn reset_for_replacement_survives_an_in_flight_native_capture() {
    let bundle = TerminalActor::new(80, 24).expect("new actor");
    let mut actor = bundle.actor;
    let (reply, replied) = oneshot::channel();
    actor.start_native_bootstrap(NativeBootstrapRequest {
        owner: 31,
        terminal_id: phux_protocol::ids::ResourceId::local(1),
        stream_id: phux_protocol::ids::StreamId::new(1).expect("stream id"),
        bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("bootstrap id"),
        limits: phux_protocol::caps::BootstrapLimits::default(),
        max_bytes: crate::native_state::MAX_NATIVE_PREFIX_BYTES,
        max_frames: crate::native_state::MAX_NATIVE_PREFIX_CHUNKS + 2,
        reply,
    });
    assert!(
        actor.pending_native_bootstrap.is_some(),
        "the capture must be in flight for this to be the race under test",
    );

    // The panic was here.
    actor.reset_for_replacement();

    assert!(
        actor.pending_native_bootstrap.is_none(),
        "the in-flight cut must be landed, not left holding the terminal",
    );
    assert!(
        replied.await.expect("capture reply").is_err(),
        "the waiter must be answered, not left hanging on a cut that was discarded",
    );
    actor.terminal.borrow_mut().vt_write(b"ok");
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
async fn native_bootstrap_capture_lifetime_retires_staged_state() {
    let bundle = TerminalActor::new(80, 24).expect("new actor");
    let mut actor = bundle.actor;
    let (reply, replied) = oneshot::channel();
    actor.start_native_bootstrap(NativeBootstrapRequest {
        owner: 31,
        terminal_id: phux_protocol::ids::ResourceId::local(1),
        stream_id: phux_protocol::ids::StreamId::new(1).expect("stream id"),
        bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("bootstrap id"),
        limits: phux_protocol::caps::BootstrapLimits::default(),
        max_bytes: crate::native_state::MAX_NATIVE_PREFIX_BYTES,
        max_frames: crate::native_state::MAX_NATIVE_PREFIX_CHUNKS + 2,
        reply,
    });
    actor
        .pending_native_bootstrap
        .as_mut()
        .expect("pending capture")
        .started_at = tokio::time::Instant::now()
        .checked_sub(super::NATIVE_CAPTURE_LIFETIME + std::time::Duration::from_millis(1))
        .expect("test instant");

    actor.step_native_bootstrap();

    assert!(actor.pending_native_bootstrap.is_none());
    assert_eq!(
        replied.await.expect("capture reply").unwrap_err(),
        crate::native_state::NativeStateError::LimitExceeded
    );
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
async fn frame_ack_waits_until_progressive_prefix_returns_the_terminal() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let bundle = TerminalActor::new(80, 24).expect("new actor");
            let handle = bundle.handle.clone();
            let token = bundle.token.clone();
            let mut actor = bundle.actor;
            let client_id = ClientId(41);
            let stream_id = phux_protocol::ids::StreamId::new(42).expect("stream id");
            let bootstrap_id = phux_protocol::ids::BootstrapId::new(43).expect("bootstrap id");
            let (outbound, _outbound_rx) = mpsc::channel(4);
            let (_live_gate_tx, live_gate) = watch::channel(true);
            let (attach_reply, attached) = oneshot::channel();
            actor.handle_consumer_attach(ConsumerAttachRequest {
                client_id,
                outbound,
                wire_terminal_id: 1,
                stream_id,
                bootstrap_id,
                wants_state_sync: true,
                state_sync_scrollback: None,
                bootstrap_max_bytes: usize::MAX,
                bootstrap_max_frames: usize::MAX,
                bootstrap_chunk_bytes: 1,
                loss_tolerant: false,
                live_gate,
                reply: attach_reply,
            });
            attached.await.expect("attach reply").expect("attach");

            let (capture_reply, captured) = oneshot::channel();
            actor.start_native_bootstrap(NativeBootstrapRequest {
                owner: 41,
                terminal_id: phux_protocol::ids::ResourceId::local(1),
                stream_id,
                bootstrap_id,
                limits: phux_protocol::caps::BootstrapLimits::default(),
                max_bytes: crate::native_state::MAX_NATIVE_PREFIX_BYTES,
                max_frames: crate::native_state::MAX_NATIVE_PREFIX_CHUNKS + 2,
                reply: capture_reply,
            });
            handle
                .consumer_ack
                .send(ConsumerAckRequest {
                    client_id,
                    stream_id,
                    bootstrap_id,
                    seq: 1,
                })
                .await
                .expect("queue ack during prefix");

            let run = tokio::task::spawn_local(actor.run());
            tokio::time::timeout(ACTOR_EXIT_DEADLINE, captured)
                .await
                .expect("prefix stalled behind deferred ack")
                .expect("capture reply dropped")
                .expect("progressive prefix");
            token.cancel();
            run.await.expect("actor run");
        })
        .await;
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
fn apply_native_wire_frame(
    kernel: &mut phux_client_core::session::SessionKernel<
        phux_client_core::engine::ghostty::GhosttyAdapter,
    >,
    frame: &FrameKind,
    effects: &mut phux_client_core::session::EffectBuffer,
) {
    use phux_client_core::engine::CanonicalGeometry;
    use phux_client_core::session::KernelInput;

    let mut encoded = (&[][..]).into();
    frame.encode(&mut encoded);
    let (decoded, tail) = FrameKind::decode(&encoded).expect("public wire decode");
    assert!(tail.is_empty());
    let input = match &decoded {
        FrameKind::BootstrapBegin {
            terminal_id,
            stream_id,
            bootstrap_id,
            profile,
            cols,
            rows,
            base_seq,
        } => KernelInput::BootstrapBegin {
            terminal_id,
            stream_id: *stream_id,
            bootstrap_id: *bootstrap_id,
            profile: *profile,
            geometry: CanonicalGeometry::new(*cols, *rows).expect("geometry"),
            base_seq: *base_seq,
        },
        FrameKind::BootstrapChunk {
            terminal_id,
            stream_id,
            bootstrap_id,
            chunk_seq,
            payload,
        } => KernelInput::BootstrapChunk {
            terminal_id,
            stream_id: *stream_id,
            bootstrap_id: *bootstrap_id,
            chunk_seq: *chunk_seq,
            payload,
        },
        FrameKind::BootstrapReady {
            terminal_id,
            stream_id,
            bootstrap_id,
            history_cursor,
        } => KernelInput::BootstrapReady {
            terminal_id,
            stream_id: *stream_id,
            bootstrap_id: *bootstrap_id,
            history_cursor: history_cursor.as_deref(),
        },
        FrameKind::HistoryPage {
            terminal_id,
            stream_id,
            bootstrap_id,
            page_seq,
            cursor,
            next_cursor,
            payload,
            rows,
        } => KernelInput::HistoryPage {
            terminal_id,
            stream_id: *stream_id,
            bootstrap_id: *bootstrap_id,
            page_seq: *page_seq,
            rows: *rows,
            payload,
            cursor,
            next_cursor: next_cursor.as_deref(),
        },
        FrameKind::ResourceOutput {
            terminal_id,
            stream_id,
            bootstrap_id,
            seq,
            bytes,
        } => KernelInput::ResourceOutput {
            terminal_id,
            stream_id: *stream_id,
            bootstrap_id: *bootstrap_id,
            seq: *seq,
            payload: bytes,
        },
        other => panic!("unexpected native flow frame: {other:?}"),
    };
    kernel
        .update(input, effects)
        .expect("kernel accepts server wire frame");
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
fn take_history_request(
    effects: &mut phux_client_core::session::EffectBuffer,
) -> Option<(Vec<u8>, u32, u32)> {
    use phux_client_core::session::{KernelEffect, KernelSend};

    let request = effects
        .as_slice()
        .iter()
        .rev()
        .find_map(|effect| match effect {
            KernelEffect::Send(KernelSend::HistoryRequest {
                cursor,
                max_bytes,
                max_rows,
                ..
            }) => Some((cursor.clone(), *max_bytes, *max_rows)),
            _ => None,
        });
    effects.clear();
    request
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[allow(
    clippy::future_not_send,
    clippy::too_many_lines,
    reason = "the LocalSet acceptance keeps the non-Send native actor and kernel in one real wire-flow fixture"
)]
async fn run_native_server_kernel_flow(
    history_rows: usize,
    history_config: phux_client_core::history::HistoryCacheConfig,
    interleave_live: bool,
) -> (phux_client_core::history::HistoryStatus, usize, usize) {
    use phux_client_core::engine::ghostty::GhosttyAdapter;
    use phux_client_core::session::{EffectBuffer, SessionKernel};
    use phux_protocol::caps::{BootstrapProfile, EngineCodec, EngineFeatureSet};

    let token = CancellationToken::new();
    let bundle = TerminalActor::build_with_token(
        200,
        3,
        None,
        test_scrollback(u32::try_from(history_rows.max(16)).expect("test history rows")),
        token.clone(),
    )
    .expect("native actor");
    let handle = bundle.handle.clone();
    let mut actor = bundle.actor;
    for row in 0..history_rows {
        actor.vt_write_for_test(format!("server-history-{row:04}\r\n").as_bytes());
    }
    let (pty_tx, _writer_rx) = actor.install_test_pty_channels();
    let run = tokio::task::spawn_local(actor.run());

    let terminal_id = phux_protocol::ids::ResourceId::local(71);
    let stream_id = phux_protocol::ids::StreamId::new(72).expect("stream id");
    let bootstrap_id = phux_protocol::ids::BootstrapId::new(73).expect("bootstrap id");
    let limits = phux_protocol::caps::BootstrapLimits::default();
    let (reply, captured) = oneshot::channel();
    handle
        .terminal()
        .expect("terminal facet")
        .native_bootstrap
        .send(NativeBootstrapRequest {
            owner: 71,
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            limits,
            max_bytes: crate::native_state::MAX_NATIVE_PREFIX_BYTES,
            max_frames: crate::native_state::MAX_NATIVE_PREFIX_CHUNKS + 2,
            reply,
        })
        .await
        .expect("bootstrap request");
    let capture = captured
        .await
        .expect("bootstrap reply")
        .expect("bootstrap capture");
    let cursor = capture.publication_cursor;

    let mut kernel = SessionKernel::with_history_config(
        GhosttyAdapter::new(limits),
        BootstrapProfile::NativeState {
            codec: EngineCodec::LibghosttySnapshotV1,
            features: EngineFeatureSet::required_native(),
        },
        history_config,
    );
    let mut effects = EffectBuffer::new();
    for frame in &capture.frames {
        apply_native_wire_frame(&mut kernel, frame, &mut effects);
    }
    assert!(kernel.published(&terminal_id).is_some());

    let (publication_reply, published) = oneshot::channel();
    handle
        .terminal()
        .expect("terminal facet")
        .native_publication
        .send(NativePublicationRequest {
            owner: 71,
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            cursor,
            reply: publication_reply,
        })
        .await
        .expect("publication request");
    let mut live = published
        .await
        .expect("publication reply")
        .expect("publication")
        .live;

    let (outbound, _outbound_rx) = mpsc::channel(2);
    let mut pages = 0_usize;
    let mut authenticated_rows = 0_usize;
    let mut next_request = take_history_request(&mut effects);
    while let Some((wire_cursor, max_bytes, max_rows)) = next_request {
        let permit = outbound
            .clone()
            .reserve_owned()
            .await
            .expect("history permit");
        let (reply, response) = oneshot::channel();
        handle
            .terminal()
            .expect("terminal facet")
            .native_history
            .send(NativeHistoryRequest {
                permit,
                owner: 71,
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                cursor: wire_cursor.into(),
                max_bytes,
                max_rows,
                limits,
                reply,
            })
            .await
            .expect("history request");
        let frame = response
            .await
            .expect("history reply")
            .result
            .expect("history capture");
        let FrameKind::HistoryPage { rows, .. } = &frame else {
            panic!("cooperative request must produce one authenticated unit: {frame:?}");
        };
        pages += 1;
        authenticated_rows += *rows as usize;
        apply_native_wire_frame(&mut kernel, &frame, &mut effects);
        next_request = take_history_request(&mut effects);

        if interleave_live && pages == 1 {
            pty_tx
                .send(PtyEvent::Bytes {
                    chunk: Bytes::from_static(b"live-between-server-pages\r\n"),
                    read_at: std::time::Instant::now(),
                })
                .await
                .expect("live PTY bytes");
            let PaneOutput::Live { seq, bytes, .. } = live.recv().await.expect("live output")
            else {
                panic!("expected live output");
            };
            apply_native_wire_frame(
                &mut kernel,
                &FrameKind::ResourceOutput {
                    terminal_id: terminal_id.clone(),
                    stream_id,
                    bootstrap_id,
                    seq,
                    bytes,
                },
                &mut effects,
            );
        }
        assert!(pages < 10_000, "history flow must terminate");
    }

    let status = kernel
        .history_cache(&terminal_id)
        .expect("history cache")
        .status();
    token.cancel();
    run.await.expect("actor run");
    (status, pages, authenticated_rows)
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
async fn actual_server_wire_kernel_history_reaches_finish_with_zero_and_discarded_rows() {
    use phux_client_core::history::{HistoryCacheConfig, HistoryLoadState};

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (empty, empty_units, empty_rows) =
                run_native_server_kernel_flow(0, HistoryCacheConfig::default(), false).await;
            assert_eq!(empty.state, HistoryLoadState::Complete);
            assert_eq!(empty_units, 1, "empty history still authenticates FINISH");
            assert_eq!(empty_rows, 0);

            let tiny = HistoryCacheConfig {
                max_materialized_rows: 1,
                prefetch_rows: 2,
                request_max_rows: 1024,
                ..HistoryCacheConfig::default()
            };
            let (history, units, authenticated_rows) =
                run_native_server_kernel_flow(3_000, tiny, true).await;
            assert_eq!(history.state, HistoryLoadState::Complete);
            assert!(units >= 3, "two history pages plus FINISH are required");
            assert!(authenticated_rows > history.materialized_rows);
            assert!(history.materialized_rows <= 1);
        })
        .await;
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
async fn saturated_history_busy_hint_clamps_rows_on_the_public_wire() {
    let bundle = TerminalActor::new(20, 5).expect("actor");
    let mut actor = bundle.actor;
    let (outbound, _outbound_rx) = mpsc::channel(MAX_NATIVE_HISTORY_CLIENTS + 1);
    let mut saturated = None;
    for owner in 0..=MAX_NATIVE_HISTORY_CLIENTS {
        let permit = outbound
            .clone()
            .reserve_owned()
            .await
            .expect("history permit");
        let (reply, response) = oneshot::channel();
        actor.handle_native_history(NativeHistoryRequest {
            permit,
            owner: owner as u64,
            terminal_id: phux_protocol::ids::ResourceId::local(1),
            stream_id: phux_protocol::ids::StreamId::new(1).expect("stream id"),
            bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("bootstrap id"),
            cursor: Bytes::from_static(b"not-a-valid-native-cursor"),
            max_bytes: u32::MAX,
            max_rows: u32::MAX,
            limits: phux_protocol::caps::BootstrapLimits::default(),
            reply,
        });
        if owner == MAX_NATIVE_HISTORY_CLIENTS {
            saturated = Some(response);
        }
    }
    let frame = saturated
        .expect("saturated reply")
        .await
        .expect("busy response")
        .result
        .expect("busy frame");
    let mut encoded = (&[][..]).into();
    frame.encode(&mut encoded);
    let (decoded, tail) = FrameKind::decode(&encoded).expect("public wire decode");
    assert!(tail.is_empty());
    assert!(matches!(
        decoded,
        FrameKind::HistoryRejected {
            reason: phux_protocol::wire::frame::HistoryRejectionReason::Busy,
            required_rows: phux_protocol::MAX_HISTORY_PAGE_ROWS,
            ..
        }
    ));
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
async fn releasing_an_owner_answers_pending_history_and_promotes_the_backlog() {
    use phux_protocol::wire::frame::HistoryTombstoneReason;
    for mode in ["release", "reattach", "expire"] {
        let bundle = TerminalActor::new(20, 5).expect("actor");
        let mut actor = bundle.actor;
        let (outbound, _outbound_rx) = mpsc::channel(2);
        let mut replies = Vec::new();
        for owner in [7, 8] {
            let permit = outbound
                .clone()
                .reserve_owned()
                .await
                .expect("history permit");
            let (reply, response) = oneshot::channel();
            actor.handle_native_history(NativeHistoryRequest {
                permit,
                owner,
                terminal_id: phux_protocol::ids::ResourceId::local(1),
                stream_id: phux_protocol::ids::StreamId::new(1).expect("stream id"),
                bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("bootstrap id"),
                cursor: Bytes::from_static(b"pending-cursor"),
                max_bytes: 1024,
                max_rows: 64,
                limits: phux_protocol::caps::BootstrapLimits::default(),
                reply,
            });
            replies.push(response);
        }

        let expected_reason = if mode == "expire" {
            actor.native_cursor_owners.insert(
                NativeCursorKey::new(7, phux_protocol::ids::StreamId::new(1).expect("stream")),
                NativeCursorOwner {
                    cursor: [0; 32],
                    record_index: 0,
                    touched: tokio::time::Instant::now() - NATIVE_HISTORY_TTL,
                    next_page_seq: 1,
                    terminal_id: phux_protocol::ids::ResourceId::local(1),
                    stream_id: phux_protocol::ids::StreamId::new(1).expect("stream"),
                    bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("bootstrap"),
                },
            );
            let mut controls = actor.core.output_tx.subscribe();
            actor.expire_native_cursors();
            assert!(matches!(
                controls.try_recv().expect("expiry control"),
                PaneOutput::Control {
                    frame: FrameKind::HistoryTombstone {
                        reason: HistoryTombstoneReason::Expired,
                        ..
                    },
                    ..
                }
            ));
            HistoryTombstoneReason::Expired
        } else if mode == "reattach" {
            actor.invalidate_native_owner(
                7,
                phux_protocol::ids::StreamId::new(1).expect("stream"),
                phux_protocol::wire::frame::TombstoneReason::ExplicitReattach,
            );
            HistoryTombstoneReason::Released
        } else {
            actor.release_native_owner(7);
            HistoryTombstoneReason::Released
        };
        let released = replies
            .remove(0)
            .try_recv()
            .expect("released reply without a later actor turn")
            .result;
        assert!(matches!(
            released,
            Ok(FrameKind::HistoryTombstone {
                reason,
                ..
            }) if reason == expected_reason
        ));
        assert_eq!(
            actor
                .pending_native_history
                .as_ref()
                .map(|pending| pending.request.owner),
            Some(8)
        );
        let _ = actor
            .invalidate_all_native_cursors(phux_protocol::wire::frame::TombstoneReason::Resize);
        assert!(actor.pending_native_history.is_none());
        assert!(actor.native_history_backlog.is_empty());
        assert!(matches!(
            replies
                .remove(0)
                .try_recv()
                .expect("resize answers parked request")
                .result,
            Ok(FrameKind::HistoryTombstone {
                reason: phux_protocol::wire::frame::HistoryTombstoneReason::Released,
                ..
            })
        ));
    }
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
async fn native_request_runs_after_one_bounded_pty_turn_and_preserves_raw_bytes() {
    const CHUNKS: usize = 200;
    const CHUNK_BYTES: usize = 1024;
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let bundle = TerminalActor::new(80, 24).expect("new actor");
            let handle = bundle.handle.clone();
            let token = bundle.token.clone();
            let mut actor = bundle.actor;
            let (pty_tx, _writer_rx) = actor.install_test_pty_channels();
            let mut raw_rx = handle.output.subscribe();
            for _ in 0..CHUNKS {
                pty_tx
                    .try_send(PtyEvent::Bytes {
                        chunk: Bytes::from(vec![b'x'; CHUNK_BYTES]),
                        read_at: std::time::Instant::now(),
                    })
                    .expect("queue sustained PTY output");
            }
            let (reply, replied) = oneshot::channel();
            handle
                .terminal()
                .expect("terminal facet")
                .native_bootstrap
                .send(NativeBootstrapRequest {
                    owner: 7,
                    terminal_id: phux_protocol::ids::ResourceId::local(1),
                    stream_id: phux_protocol::ids::StreamId::new(1).expect("stream id"),
                    bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("bootstrap id"),
                    limits: phux_protocol::caps::BootstrapLimits::new(
                        phux_protocol::MAX_BOOTSTRAP_CHUNK_BYTES,
                        phux_protocol::DEFAULT_HISTORY_PAGE_BYTES,
                    )
                    .expect("wide negotiated bootstrap bound"),
                    max_bytes: crate::native_state::MAX_NATIVE_PREFIX_BYTES,
                    max_frames: crate::native_state::MAX_NATIVE_PREFIX_CHUNKS + 2,
                    reply,
                })
                .await
                .expect("send native request");
            let run = tokio::task::spawn_local(actor.run());
            let capture = tokio::time::timeout(ACTOR_EXIT_DEADLINE, replied)
                .await
                .expect("native request starved behind PTY")
                .expect("native reply dropped")
                .expect("native capture");
            assert_eq!(
                capture.base_seq, 1,
                "native request must run after one bounded ready PTY turn"
            );
            let retained_capacity = capture
                .frames
                .into_iter()
                .try_fold(0_usize, |total, frame| {
                    let capacity = match frame {
                        FrameKind::BootstrapChunk { payload, .. } => payload
                            .try_into_mut()
                            .expect("actor owns compact chunk allocation")
                            .capacity(),
                        FrameKind::BootstrapReady {
                            history_cursor: Some(cursor),
                            ..
                        } => cursor
                            .try_into_mut()
                            .expect("actor owns compact cursor allocation")
                            .capacity(),
                        _ => 0,
                    };
                    total.checked_add(capacity)
                })
                .expect("retained capacity sum");
            assert_eq!(capture.retained_bytes, retained_capacity);

            let mut expected_seq = 1_u64;
            let mut raw_bytes = 0_usize;
            while raw_bytes < CHUNKS * CHUNK_BYTES {
                let output = tokio::time::timeout(ACTOR_EXIT_DEADLINE, raw_rx.recv())
                    .await
                    .expect("raw output stalled")
                    .expect("raw output channel closed");
                if let PaneOutput::Live { seq, bytes, .. } = output {
                    assert_eq!(seq, expected_seq);
                    expected_seq += 1;
                    raw_bytes += bytes.len();
                }
            }
            assert_eq!(raw_bytes, CHUNKS * CHUNK_BYTES);
            token.cancel();
            run.await.expect("actor run");
        })
        .await;
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
async fn combined_native_pty_ingress_never_parks_on_a_silent_source() {
    let (_bootstrap_tx, bootstrap) = mpsc::channel(1);
    let (_history_tx, history) = mpsc::channel(1);
    let (_publication_tx, publication) = mpsc::channel(1);
    let (release_tx, release) = mpsc::channel(4);
    let mut native = NativeRequestReceivers {
        bootstrap,
        publication,
        history,
        release,
    };
    let (pty_tx, mut pty) = mpsc::channel(4);

    release_tx
        .send(NativeReleaseRequest { owner: 1 })
        .await
        .expect("first native request");
    release_tx
        .send(NativeReleaseRequest { owner: 2 })
        .await
        .expect("second native request");
    for owner in [1, 2] {
        let ingress = tokio::time::timeout(
            ACTOR_EXIT_DEADLINE,
            recv_native_or_pty(&mut native, Some(&mut pty), false),
        )
        .await
        .expect("silent PTY must not park native control");
        assert!(matches!(
            ingress,
            NativeOrPty::Native(NativeActorRequest::Release(NativeReleaseRequest {
                owner: actual
            })) if actual == owner
        ));
    }

    release_tx
        .send(NativeReleaseRequest { owner: 3 })
        .await
        .expect("ready native request");
    pty_tx
        .try_send(PtyEvent::Bytes {
            chunk: Bytes::from_static(b"x"),
            read_at: std::time::Instant::now(),
        })
        .expect("ready PTY event");
    assert!(matches!(
        recv_native_or_pty(&mut native, Some(&mut pty), false).await,
        NativeOrPty::Pty(Some(PtyEvent::Bytes { chunk: bytes, .. })) if bytes.as_ref() == b"x"
    ));
    assert!(matches!(
        recv_native_or_pty(&mut native, Some(&mut pty), true).await,
        NativeOrPty::Native(NativeActorRequest::Release(NativeReleaseRequest {
            owner: 3
        }))
    ));
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[test]
fn resize_tombstone_is_ordered_after_every_queued_live_sequence() {
    let bundle = TerminalActor::new(20, 5).expect("new actor");
    let mut actor = bundle.actor;
    let mut output = bundle.handle.output.subscribe();
    let terminal_id = phux_protocol::ids::ResourceId::local(1);
    let stream_id = phux_protocol::ids::StreamId::new(1).expect("stream id");
    let bootstrap_id = phux_protocol::ids::BootstrapId::new(1).expect("bootstrap id");
    let cursor: crate::native_state::OpaqueHistoryCursor = [1; crate::native_state::TOKEN_LEN];
    actor.native_cursor_owners.insert(
        NativeCursorKey::new(7, stream_id),
        NativeCursorOwner {
            cursor,
            record_index: 0,
            touched: tokio::time::Instant::now(),
            next_page_seq: 1,
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
        },
    );

    actor.core.seq = 5;
    actor
        .core
        .output_tx
        .send(PaneOutput::Live {
            seq: 5,
            bytes: Bytes::from_static(b"prior"),
            at: std::time::Instant::now(),
        })
        .expect("queue prior live output");
    actor.invalidate_all_native_cursors(phux_protocol::wire::frame::TombstoneReason::Resize);

    assert!(matches!(
        output.try_recv(),
        Ok(PaneOutput::Live { seq: 5, .. })
    ));
    let Ok(PaneOutput::Control { owner: 7, frame }) = output.try_recv() else {
        panic!("ordered generation tombstone");
    };
    assert!(matches!(
        frame,
        FrameKind::BootstrapTombstone {
            terminal_id: actual_terminal,
            stream_id: actual_stream,
            bootstrap_id: actual_bootstrap,
            reason: phux_protocol::wire::frame::TombstoneReason::Resize,
            last_valid_seq: 5,
        } if actual_terminal == terminal_id
            && actual_stream == stream_id
            && actual_bootstrap == bootstrap_id
    ));
}

/// An attach-time reflow owes a resync to exactly the pumps it tombstoned.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
async fn an_attach_time_reflow_owes_a_resync_to_the_native_pumps_it_tombstoned() {
    let bundle = TerminalActor::new(20, 5).expect("new actor");
    let mut actor = bundle.actor;
    let stream_id = phux_protocol::ids::StreamId::new(3).expect("stream id");
    actor.native_cursor_owners.insert(
        NativeCursorKey::new(7, stream_id),
        NativeCursorOwner {
            cursor: [1; crate::native_state::TOKEN_LEN],
            record_index: 0,
            touched: tokio::time::Instant::now(),
            next_page_seq: 1,
            terminal_id: phux_protocol::ids::ResourceId::local(1),
            stream_id,
            bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("bootstrap id"),
        },
    );
    let attach_time_reflow = |cols, rows| ResizeRequest {
        cols,
        rows,
        cell_px: None,
        resync_clients: false,
        resync_only: false,
        resync_for: None,
    };

    let owed = actor.apply_resize_request(attach_time_reflow(30, 8));
    assert_eq!(
        owed,
        vec![super::run_loop::OwedResync {
            reason: crate::resource::ResyncReason::Resize,
            target: Some(crate::resource::ResyncTarget {
                owner: 7,
                stream_id,
                bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("bootstrap id"),
            }),
        }],
        "the tombstoned native pump is owed a resync addressed to it",
    );

    let owed = actor.apply_resize_request(attach_time_reflow(40, 9));
    assert!(
        owed.is_empty(),
        "with nothing native left to tombstone, an attach-time reflow owes nothing",
    );
}

/// A `HISTORY_REQUEST` racing a resize that drained its binding gets a
/// per-replica `HISTORY_TOMBSTONE`, never a connection-level error.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
async fn a_cursor_invalidated_by_resize_is_tombstoned_never_faulted() {
    let bundle = TerminalActor::new(20, 5).expect("new actor");
    let mut actor = bundle.actor;
    let terminal_id = phux_protocol::ids::ResourceId::local(1);
    let stream_id = phux_protocol::ids::StreamId::new(1).expect("stream id");
    let bootstrap_id = phux_protocol::ids::BootstrapId::new(1).expect("bootstrap id");
    let cursor: crate::native_state::OpaqueHistoryCursor = [1; crate::native_state::TOKEN_LEN];
    let wire_cursor = Bytes::copy_from_slice(&cursor);
    let binding = || NativeCursorOwner {
        cursor,
        record_index: 0,
        touched: tokio::time::Instant::now(),
        next_page_seq: 1,
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
    };
    let (outbound, _outbound_rx) = dummy_outbound();

    let request = async |actor: &mut TerminalActor, bootstrap_id| {
        let permit = outbound
            .clone()
            .reserve_owned()
            .await
            .expect("history request permit");
        let (reply, answered) = oneshot::channel();
        actor.handle_native_history(NativeHistoryRequest {
            permit,
            owner: 7,
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            cursor: wire_cursor.clone(),
            max_bytes: phux_protocol::caps::BootstrapLimits::default().max_history_page_bytes(),
            max_rows: 128,
            limits: phux_protocol::caps::BootstrapLimits::default(),
            reply,
        });
        actor.cooperative_native_step();
        answered
            .await
            .expect("history reply")
            .result
            .expect("an unusable cursor is answered, never faulted")
    };

    // The binding is gone entirely: the mid-attach resize drained it.
    actor
        .native_cursor_owners
        .insert(NativeCursorKey::new(7, stream_id), binding());
    actor.invalidate_all_native_cursors(phux_protocol::wire::frame::TombstoneReason::Resize);
    assert!(actor.native_cursor_owners.is_empty());
    let frame = request(&mut actor, bootstrap_id).await;
    assert!(
        matches!(
            &frame,
            FrameKind::HistoryTombstone {
                reason: phux_protocol::wire::frame::HistoryTombstoneReason::Stale,
                cursor: echoed,
                ..
            } if *echoed == wire_cursor
        ),
        "a drained binding tombstones the cursor: {frame:?}"
    );

    // The binding names the generation the resize replaced.
    actor
        .native_cursor_owners
        .insert(NativeCursorKey::new(7, stream_id), binding());
    let superseded = phux_protocol::ids::BootstrapId::new(2).expect("bootstrap id");
    let frame = request(&mut actor, superseded).await;
    assert!(
        matches!(
            &frame,
            FrameKind::HistoryTombstone {
                reason: phux_protocol::wire::frame::HistoryTombstoneReason::Stale,
                ..
            }
        ),
        "a superseded generation tombstones the cursor: {frame:?}"
    );
    assert!(
        actor
            .native_cursor_owners
            .contains_key(&NativeCursorKey::new(7, stream_id)),
        "answering a stale request must not release the live binding"
    );
}

/// Two native pumps from one client on one pane do not invalidate each
/// other.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
fn capture_native_for_pump(
    actor: &mut TerminalActor,
    owner: u64,
    stream_id: phux_protocol::ids::StreamId,
    bootstrap_id: phux_protocol::ids::BootstrapId,
) -> NativeBootstrapReply {
    let (reply, mut response) = oneshot::channel();
    actor.handle_native_bootstrap(NativeBootstrapRequest {
        owner,
        terminal_id: phux_protocol::ids::ResourceId::local(1),
        stream_id,
        bootstrap_id,
        limits: phux_protocol::caps::BootstrapLimits::default(),
        max_bytes: crate::native_state::MAX_NATIVE_PREFIX_BYTES,
        max_frames: crate::native_state::MAX_NATIVE_PREFIX_CHUNKS + 2,
        reply,
    });
    for _ in 0..64 {
        match response.try_recv() {
            Ok(result) => return result.expect("native capture"),
            Err(oneshot::error::TryRecvError::Empty) => actor.cooperative_native_step(),
            Err(oneshot::error::TryRecvError::Closed) => {
                panic!("native bootstrap reply dropped")
            }
        }
    }
    panic!("native capture did not finish")
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
fn activate_native_for_pump(
    actor: &mut TerminalActor,
    owner: u64,
    stream_id: phux_protocol::ids::StreamId,
    bootstrap_id: phux_protocol::ids::BootstrapId,
    cursor: crate::native_state::OpaqueHistoryCursor,
) -> Result<NativePublicationReply, crate::native_state::NativeStateError> {
    let (reply, mut response) = oneshot::channel();
    actor.handle_native_publication(NativePublicationRequest {
        owner,
        terminal_id: phux_protocol::ids::ResourceId::local(1),
        stream_id,
        bootstrap_id,
        cursor,
        reply,
    });
    response
        .try_recv()
        .expect("publication is answered on the same turn")
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
async fn two_native_pumps_from_one_client_do_not_tombstone_each_other() {
    let bundle = TerminalActor::new(20, 5).expect("new actor");
    let mut actor = bundle.actor;
    let mut output = bundle.handle.output.subscribe();
    let owner = 7_u64;
    let stream_a = phux_protocol::ids::StreamId::new(1).expect("stream");
    let stream_b = phux_protocol::ids::StreamId::new(2).expect("stream");
    let bootstrap_a = phux_protocol::ids::BootstrapId::new(1).expect("bootstrap");
    let bootstrap_b = phux_protocol::ids::BootstrapId::new(2).expect("bootstrap");

    let first = capture_native_for_pump(&mut actor, owner, stream_a, bootstrap_a);
    let second = capture_native_for_pump(&mut actor, owner, stream_b, bootstrap_b);

    let mut tombstones = Vec::new();
    while let Ok(message) = output.try_recv() {
        if let PaneOutput::Control {
            frame: FrameKind::BootstrapTombstone { stream_id, .. },
            ..
        } = message
        {
            tombstones.push(stream_id);
        }
    }
    assert!(
        tombstones.is_empty(),
        "a sibling pump must not tombstone the first: {tombstones:?}"
    );
    assert!(
        actor
            .native_cursor_owners
            .contains_key(&NativeCursorKey::new(owner, stream_a))
            && actor
                .native_cursor_owners
                .contains_key(&NativeCursorKey::new(owner, stream_b)),
        "both pumps keep a live binding"
    );
    activate_native_for_pump(
        &mut actor,
        owner,
        stream_a,
        bootstrap_a,
        first.publication_cursor,
    )
    .expect("first pump still activates publication");
    activate_native_for_pump(
        &mut actor,
        owner,
        stream_b,
        bootstrap_b,
        second.publication_cursor,
    )
    .expect("second pump activates publication");

    actor.release_native_owner(owner);
    assert!(
        actor.native_cursor_owners.is_empty(),
        "client detach still releases every pump for that owner"
    );
}

/// Recapturing one `(owner, stream_id)` tombstones only that binding.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
async fn recapturing_the_same_owner_stream_tombstones_only_that_binding() {
    let bundle = TerminalActor::new(20, 5).expect("new actor");
    let mut actor = bundle.actor;
    let mut output = bundle.handle.output.subscribe();
    let owner = 7_u64;
    let stream_a = phux_protocol::ids::StreamId::new(1).expect("stream");
    let stream_b = phux_protocol::ids::StreamId::new(2).expect("stream");
    let bootstrap_a = phux_protocol::ids::BootstrapId::new(1).expect("bootstrap");
    let bootstrap_b = phux_protocol::ids::BootstrapId::new(2).expect("bootstrap");
    let bootstrap_a2 = phux_protocol::ids::BootstrapId::new(3).expect("bootstrap");

    let first = capture_native_for_pump(&mut actor, owner, stream_a, bootstrap_a);
    let sibling = capture_native_for_pump(&mut actor, owner, stream_b, bootstrap_b);
    while output.try_recv().is_ok() {}

    let replacement = capture_native_for_pump(&mut actor, owner, stream_a, bootstrap_a2);
    let mut tombstoned = Vec::new();
    while let Ok(message) = output.try_recv() {
        if let PaneOutput::Control {
            frame:
                FrameKind::BootstrapTombstone {
                    stream_id,
                    bootstrap_id,
                    reason,
                    ..
                },
            ..
        } = message
        {
            tombstoned.push((stream_id, bootstrap_id, reason));
        }
    }
    assert_eq!(
        tombstoned,
        [(
            stream_a,
            bootstrap_a,
            phux_protocol::wire::frame::TombstoneReason::ExplicitReattach
        )],
        "recapture tombstones only the same (owner, stream) generation"
    );
    assert_eq!(
        actor
            .native_cursor_owners
            .get(&NativeCursorKey::new(owner, stream_a))
            .map(|binding| binding.bootstrap_id),
        Some(bootstrap_a2)
    );
    assert_eq!(
        actor
            .native_cursor_owners
            .get(&NativeCursorKey::new(owner, stream_b))
            .map(|binding| binding.bootstrap_id),
        Some(bootstrap_b)
    );
    assert!(
        activate_native_for_pump(
            &mut actor,
            owner,
            stream_a,
            bootstrap_a,
            first.publication_cursor,
        )
        .is_err(),
        "the recaptured generation cannot activate publication"
    );
    activate_native_for_pump(
        &mut actor,
        owner,
        stream_b,
        bootstrap_b,
        sibling.publication_cursor,
    )
    .expect("sibling pump still activates");
    activate_native_for_pump(
        &mut actor,
        owner,
        stream_a,
        bootstrap_a2,
        replacement.publication_cursor,
    )
    .expect("replacement pump activates");
}

/// Two pumps of one client joining one capture both stay waiters and
/// activate publication.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
async fn two_same_client_pumps_can_share_one_pending_capture() {
    let bundle = TerminalActor::new(20, 5).expect("new actor");
    let mut actor = bundle.actor;
    let owner = 7_u64;
    let stream_a = phux_protocol::ids::StreamId::new(1).expect("stream");
    let stream_b = phux_protocol::ids::StreamId::new(2).expect("stream");
    let bootstrap_a = phux_protocol::ids::BootstrapId::new(1).expect("bootstrap");
    let bootstrap_b = phux_protocol::ids::BootstrapId::new(2).expect("bootstrap");
    let request = |stream_id, bootstrap_id, reply| NativeBootstrapRequest {
        owner,
        terminal_id: phux_protocol::ids::ResourceId::local(1),
        stream_id,
        bootstrap_id,
        limits: phux_protocol::caps::BootstrapLimits::default(),
        max_bytes: crate::native_state::MAX_NATIVE_PREFIX_BYTES,
        max_frames: crate::native_state::MAX_NATIVE_PREFIX_CHUNKS + 2,
        reply,
    };
    let (reply_a, mut response_a) = oneshot::channel();
    actor.handle_native_bootstrap(request(stream_a, bootstrap_a, reply_a));
    let (reply_b, mut response_b) = oneshot::channel();
    actor.handle_native_bootstrap(request(stream_b, bootstrap_b, reply_b));
    assert_eq!(
        actor
            .pending_native_bootstrap
            .as_ref()
            .map(|pending| pending.waiters.len()),
        Some(2),
        "both pumps wait on the shared capture"
    );

    let mut first = None;
    let mut second = None;
    for _ in 0..64 {
        if first.is_none()
            && let Ok(result) = response_a.try_recv()
        {
            first = Some(result.expect("first capture"));
        }
        if second.is_none()
            && let Ok(result) = response_b.try_recv()
        {
            second = Some(result.expect("second capture"));
        }
        if first.is_some() && second.is_some() {
            break;
        }
        actor.cooperative_native_step();
    }
    let first = first.expect("first capture finished");
    let second = second.expect("second capture finished");
    assert_eq!(first.publication_cursor, second.publication_cursor);
    activate_native_for_pump(
        &mut actor,
        owner,
        stream_a,
        bootstrap_a,
        first.publication_cursor,
    )
    .expect("first shared waiter activates");
    activate_native_for_pump(
        &mut actor,
        owner,
        stream_b,
        bootstrap_b,
        second.publication_cursor,
    )
    .expect("second shared waiter activates");
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
#[allow(
    clippy::too_many_lines,
    reason = "the test keeps fault injection and the full cursor-continuation proof in one LocalSet lifecycle"
)]
async fn capture_host_allocation_failures_release_state_and_history_still_pages() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            async fn request_prefix(
                handle: &ResourceHandle,
            ) -> Result<NativeBootstrapReply, crate::native_state::NativeStateError> {
                let (reply, replied) = oneshot::channel();
                handle
                    .terminal()
                    .expect("terminal facet")
                    .native_bootstrap
                    .send(NativeBootstrapRequest {
                        owner: 11,
                        terminal_id: phux_protocol::ids::ResourceId::local(2),
                        stream_id: phux_protocol::ids::StreamId::new(2).expect("stream id"),
                        bootstrap_id: phux_protocol::ids::BootstrapId::new(4)
                            .expect("bootstrap id"),
                        limits: phux_protocol::caps::BootstrapLimits::default(),
                        max_bytes: crate::native_state::MAX_NATIVE_PREFIX_BYTES,
                        max_frames: crate::native_state::MAX_NATIVE_PREFIX_CHUNKS + 2,
                        reply,
                    })
                    .await
                    .expect("send bootstrap");
                replied.await.expect("bootstrap reply")
            }

            async fn request_history_unit(
                handle: &ResourceHandle,
                outbound: &mpsc::Sender<Outbound>,
                cursor: Bytes,
            ) -> FrameKind {
                let permit = outbound
                    .clone()
                    .reserve_owned()
                    .await
                    .expect("history request permit");
                let (reply, response) = oneshot::channel();
                handle
                    .terminal()
                    .expect("terminal facet")
                    .native_history
                    .send(NativeHistoryRequest {
                        permit,
                        owner: 11,
                        terminal_id: phux_protocol::ids::ResourceId::local(2),
                        stream_id: phux_protocol::ids::StreamId::new(2).expect("stream id"),
                        bootstrap_id: phux_protocol::ids::BootstrapId::new(4)
                            .expect("bootstrap id"),
                        cursor,
                        max_bytes: phux_protocol::caps::BootstrapLimits::default()
                            .max_history_page_bytes(),
                        max_rows: 128,
                        limits: phux_protocol::caps::BootstrapLimits::default(),
                        reply,
                    })
                    .await
                    .expect("send history request");
                response
                    .await
                    .expect("history reply")
                    .result
                    .expect("history host")
            }

            let bundle = TerminalActor::new(20, 5).expect("new actor");
            let handle = bundle.handle.clone();
            let token = bundle.token.clone();
            let (outbound, _outbound_rx) = mpsc::channel(2);
            let run = tokio::task::spawn_local(bundle.actor.run());

            FAIL_NEXT_NATIVE_HOST_ALLOC.with(|fail| fail.set(true));
            assert!(matches!(
                request_prefix(&handle).await,
                Err(crate::native_state::NativeStateError::OutOfMemory)
            ));
            PANIC_NEXT_NATIVE_HOST_ALLOC.with(|panic| panic.set(true));
            assert!(matches!(
                request_prefix(&handle).await,
                Err(crate::native_state::NativeStateError::OutOfMemory)
            ));

            let prefix = request_prefix(&handle).await.expect("bootstrap capture");
            let cursor = prefix
                .frames
                .into_iter()
                .find_map(|frame| match frame {
                    FrameKind::BootstrapReady {
                        history_cursor: Some(cursor),
                        ..
                    } => Some(cursor),
                    _ => None,
                })
                .expect("bootstrap ready cursor");

            let result = request_history_unit(&handle, &outbound, cursor.clone()).await;
            let FrameKind::HistoryPage {
                page_seq,
                cursor: echoed,
                next_cursor,
                rows,
                ..
            } = result
            else {
                panic!("expected history page");
            };
            assert_eq!(page_seq, 1);
            assert!(rows <= 128);
            assert_eq!(echoed, cursor);

            let mut next_cursor = next_cursor;
            let mut expected_page_seq = 2_u64;
            while let Some(request_cursor) = next_cursor {
                assert_eq!(request_cursor, cursor, "cursor is stable and opaque");
                let frame = request_history_unit(&handle, &outbound, request_cursor).await;
                let FrameKind::HistoryPage {
                    page_seq,
                    cursor: echoed,
                    next_cursor: following,
                    rows,
                    ..
                } = frame
                else {
                    panic!("continuation must end through authenticated FINISH");
                };
                assert_eq!(page_seq, expected_page_seq);
                assert_eq!(echoed, cursor);
                assert!(rows <= 128);
                next_cursor = following;
                expected_page_seq = expected_page_seq.checked_add(1).expect("bounded sequence");
                assert!(expected_page_seq <= 4_099, "bounded continuation");
            }
            token.cancel();
            run.await.expect("actor run");
        })
        .await;
}
/// The actor answers `PwdRequest` with the PTY child's live cwd.
#[tokio::test(flavor = "current_thread")]
async fn actor_responds_to_pwd_request_with_pty_child_cwd() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            // macOS returns the realpath, like the kernel query.
            let dir_path = dir.path().canonicalize().expect("canonicalize tempdir");

            let mut cmd = CommandBuilder::new("/bin/sh");
            cmd.arg("-c");
            cmd.arg(format!("cd '{}' && read _", dir_path.display()));
            let bundle = TerminalActor::build_with_token(
                20,
                5,
                Some(cmd),
                DEFAULT_SCROLLBACK,
                CancellationToken::new(),
            )
            .expect("build_with_token");
            let handle = bundle.handle.clone();
            let token = bundle.token;
            let join = tokio::task::spawn_local(bundle.actor.run());

            // The query races the shell's `cd`; retry briefly.
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
            let mut got: Option<String> = None;
            while tokio::time::Instant::now() < deadline {
                let (reply_tx, reply_rx) = oneshot::channel();
                handle
                    .terminal()
                    .expect("terminal facet")
                    .pwd
                    .send(PwdRequest { reply: reply_tx })
                    .await
                    .expect("send pwd request");
                got = reply_rx.await.expect("pwd reply");
                if got.as_deref() == Some(dir_path.to_str().expect("utf8 path")) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
            assert_eq!(
                got.as_deref(),
                Some(dir_path.to_str().expect("utf8 path")),
                "actor should report the PTY child's live CWD",
            );

            token.cancel();
            let _ = tokio::time::timeout(ACTOR_EXIT_DEADLINE, join).await;
        })
        .await;
}

/// A PTY-less actor answers `pwd` with `None`.
#[tokio::test(flavor = "current_thread")]
async fn actor_pwd_request_is_none_without_pty() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let bundle = TerminalActor::new_with_seed(20, 5, b"no pty here").expect("seed");
            let handle = bundle.handle.clone();
            let _token = bundle.token;
            tokio::task::spawn_local(bundle.actor.run());

            let (reply_tx, reply_rx) = oneshot::channel();
            handle
                .terminal()
                .expect("terminal facet")
                .pwd
                .send(PwdRequest { reply: reply_tx })
                .await
                .expect("send pwd request");
            assert_eq!(reply_rx.await.expect("pwd reply"), None);
        })
        .await;
}

/// The actor stops promptly on cancellation, channels open or not.
#[tokio::test(flavor = "current_thread")]
async fn actor_exits_on_cancellation() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let bundle = TerminalActor::new(20, 5).expect("new");
            let handle = bundle.handle.clone();
            let token = bundle.token;
            let join = tokio::task::spawn_local(bundle.actor.run());

            token.cancel();
            tokio::time::timeout(ACTOR_EXIT_DEADLINE, join)
                .await
                .expect("actor did not exit after cancel")
                .expect("actor task panicked");

            let (reply_tx, reply_rx) = oneshot::channel();
            let _ = handle
                .terminal()
                .expect("terminal facet")
                .snapshot
                .try_send(SnapshotRequest {
                    scrollback: None,
                    max_bytes: usize::MAX,
                    max_frames: usize::MAX,
                    chunk_bytes: 1,
                    reply: reply_tx,
                });
            drop(reply_rx);
        })
        .await;
}

/// Cancelling a parent token stops a child-token actor.
#[tokio::test(flavor = "current_thread")]
async fn parent_token_cancel_cascades_to_pane_actor() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let parent = CancellationToken::new();
            let child = parent.child_token();
            let bundle = TerminalActor::build_with_token(20, 5, None, DEFAULT_SCROLLBACK, child)
                .expect("build_with_token");
            let join = tokio::task::spawn_local(bundle.actor.run());

            parent.cancel();

            tokio::time::timeout(ACTOR_EXIT_DEADLINE, join)
                .await
                .expect("actor did not exit after its parent token was cancelled")
                .expect("actor task panicked");
        })
        .await;
}
