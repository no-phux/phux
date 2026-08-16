//! Actor lifecycle tests: construction, seeded colors, ask markers,
//! PTY spawn/adopt, signals, pane kill, input/output interleaving,
//! native-engine requests, pwd, and cancellation.

use super::test_support::*;
use super::*;

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
        100,
        CancellationToken::new(),
        Some(colors),
    )
    .expect("actor");
    let mut actor = bundle.actor;
    {
        let terminal = actor.terminal.borrow();
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

/// phux-07y: `shell_command` runs the user's command via
/// `<resolved shell> -c <command>` so quoting / args work and the
/// pane closes when the command exits. phux-i0e8.4.1: the shell is
/// the resolved default (`defaults.shell` → `$SHELL` → `/bin/sh`),
/// passed in by the caller.
#[test]
fn shell_command_wraps_in_shell_dash_c() {
    let cmd = shell_command("/opt/fancy/fish", "btop --utf-force", false);
    let argv = cmd.get_argv();
    assert_eq!(argv.len(), 3, "expected [shell, -c, command]");
    assert_eq!(argv[0], "/opt/fancy/fish");
    assert_eq!(argv[1], "-c");
    assert_eq!(argv[2], "btop --utf-force");
}

/// phux-2sl6: a non-`phux-ask` title is not an ask marker.
#[test]
fn ask_marker_rejects_non_ask_titles() {
    assert_eq!(AskMarker::parse(""), None);
    assert_eq!(AskMarker::parse("vim README.md"), None);
    // Bare prefix with no `:` carries no question — not a marker.
    assert_eq!(AskMarker::parse("phux-ask"), None);
    assert_eq!(AskMarker::parse("phux-ask[q1]"), None);
}

/// phux-2sl6: the minimal `phux-ask:<question>` form yields an empty id,
/// the question, and no suggestions.
#[test]
fn ask_marker_parses_bare_question() {
    let marker = AskMarker::parse("phux-ask:Proceed?").expect("a phux-ask marker");
    assert_eq!(marker.id, "");
    assert_eq!(marker.question, "Proceed?");
    assert!(marker.suggestions.is_empty());
}

/// phux-2sl6: the full `phux-ask[<id>]:<question>?s=a|b|c` form yields
/// the id, the question, and the `|`-delimited suggestions in order.
#[test]
fn ask_marker_parses_id_and_suggestions() {
    let marker =
        AskMarker::parse("phux-ask[q1]:Deploy to prod??s=Yes|No|Hold").expect("a phux-ask marker");
    assert_eq!(marker.id, "q1");
    assert_eq!(marker.question, "Deploy to prod?");
    assert_eq!(
        marker.suggestions,
        vec!["Yes".to_owned(), "No".to_owned(), "Hold".to_owned()],
    );
}

/// phux-2sl6: an empty `?s=` suffix (or empty options) yields no
/// suggestions, never a vector with empty strings.
#[test]
fn ask_marker_drops_empty_suggestions() {
    let marker = AskMarker::parse("phux-ask:Ready??s=").expect("a phux-ask marker");
    assert_eq!(marker.question, "Ready?");
    assert!(marker.suggestions.is_empty());

    let marker = AskMarker::parse("phux-ask:Pick??s=a||b").expect("a phux-ask marker");
    assert_eq!(
        marker.suggestions,
        vec!["a".to_owned(), "b".to_owned()],
        "empty inter-pipe segments are dropped",
    );
}

/// Direct synchronous test: snapshot-of-blank-Terminal yields the
/// expected reset preamble. Doesn't spawn the actor; exercises the
/// synthesis helper directly.
#[test]
fn synthesize_blank_pane_returns_reset_preamble() {
    let bundle = TerminalActor::new(80, 24).expect("new");
    let snap = bundle.actor.synthesize().expect("synthesize");
    assert_eq!(snap.cols, 80);
    assert_eq!(snap.rows, 24);
    assert!(snap.bytes.starts_with(b"\x1b[!p\x1b[2J\x1b[H"));
}

/// Synchronous test: seed bytes flow through to the synthesized
/// snapshot. Exercises [`TerminalActor::new_with_seed`].
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

/// Async test: the actor responds to `SnapshotRequest` over the
/// `LocalSet` and ships back the same bytes the synchronous
/// synthesizer would.
#[tokio::test(flavor = "current_thread")]
async fn actor_responds_to_snapshot_request_on_localset() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let bundle = TerminalActor::new_with_seed(20, 5, b"hi there").expect("new_with_seed");
            let handle = bundle.handle.clone();
            // Hold the token; under new semantics dropping it does
            // NOT cancel, so the actor is alive regardless. Keep
            // the binding for parallel structure with the other
            // tests in this module.
            let _token = bundle.token;
            tokio::task::spawn_local(bundle.actor.run());

            let (reply_tx, reply_rx) = oneshot::channel();
            handle
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

/// A no-PTY actor answers `UpgradeHandleRequest` with the replay snapshot
/// and dims but no descriptors — there is no child to hand off.
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
            assert_eq!(h.master_fd, None);
            assert_eq!(h.child_pid, None);
            assert_eq!((h.cols, h.rows), (20, 5));
            assert!(
                String::from_utf8_lossy(&h.vt_replay_bytes).contains("seeded"),
                "replay snapshot should carry the seeded text"
            );
        })
        .await;
}

/// A PTY-backed actor answers `UpgradeHandleRequest` with the live master
/// fd and child PID — the descriptors the re-exec'd image re-adopts.
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

            // Cancel so the actor reaps the `sleep` child instead of
            // leaking it past the test.
            token.cancel();
        })
        .await;
}

/// `new_with_adopted_pty` rebuilds a working actor around an inherited PTY:
/// it replays the seed snapshot into the fresh grid, exposes the adopted
/// child, and surfaces the live child's output — proving the resume path's
/// actor construction end to end.
#[tokio::test(flavor = "current_thread")]
async fn adopted_actor_replays_seed_and_serves_live_pty() {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    use std::io::Write;
    use std::os::fd::{BorrowedFd, IntoRawFd};

    #[allow(
        clippy::future_not_send,
        reason = "current-thread test helper; the actor's TerminalHandle is intentionally !Sync"
    )]
    async fn snapshot(handle: &TerminalHandle) -> String {
        let (reply, rx) = oneshot::channel();
        handle
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
            // test keeps `pair.master` to write into the PTY. `std`-only
            // (no `libc`, which is macOS-gated in this crate).
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
                1000,
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

            // Live byte flow: write into the PTY; `cat` echoes; the adopted
            // actor's grid surfaces it.
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

/// ADR-0033 end-to-end: a `Freeze` (SIGSTOP) flips the pane to `Frozen`
/// and broadcasts a `TerminalControl` event; `Resume` (SIGCONT) returns it
/// to `Running`; `Kill` (SIGKILL) actually terminates the child (its EOF
/// fires the exit notification). Exercises the actor's signal deliverer
/// (`killpg`) and the control-event broadcast over a real PTY child.
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
    async fn next_control(rx: &mut mpsc::Receiver<Outbound>) -> (ControlAction, TerminalLifecycle) {
        // Scan past any incidental grid events (Dirty/Idle) for the next
        // supervisory TerminalControl broadcast.
        let scan = async {
            loop {
                let Outbound::Frame(frame) = rx.recv().await.expect("event channel open") else {
                    panic!("unexpected terminal outbound sentinel")
                };
                if let FrameKind::Event {
                    event:
                        AgentEvent::TerminalControl {
                            action, lifecycle, ..
                        },
                    ..
                } = frame
                {
                    return (action, lifecycle);
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
                1000,
                CancellationToken::new(),
                b"",
            )
            .expect("new_with_adopted_pty");
            let handle = bundle.handle.clone();
            let token = bundle.token.clone();
            let mut exit_rx = bundle.exit_notify.expect("exit notify");
            tokio::task::spawn_local(bundle.actor.run());

            // Subscribe to the agent-event stream so we observe the
            // TerminalControl broadcasts.
            let (evt_tx, mut evt_rx) = mpsc::channel::<Outbound>(64);
            handle
                .subscribe_to_events
                .send(SubscribeToEventsRequest {
                    subscriber: TerminalEventSubscriber {
                        outbound: evt_tx,
                        event_types: Vec::new(),
                    },
                    wire_terminal_id: 1,
                })
                .await
                .expect("subscribe");

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
                })
                .await
                .expect("send freeze");
            ack.await.expect("freeze ack").expect("freeze delivered");
            let (action, lifecycle) = next_control(&mut evt_rx).await;
            assert_eq!(action, ControlAction::Frozen);
            assert_eq!(lifecycle, TerminalLifecycle::Frozen);

            // Resume → Running.
            let (reply, ack) = oneshot::channel();
            handle
                .control
                .send(ControlRequest::Signal {
                    signal: TerminalSignal::Resume,
                    input_holder: None,
                    by,
                    reply,
                })
                .await
                .expect("send resume");
            ack.await.expect("resume ack").expect("resume delivered");
            let (action, lifecycle) = next_control(&mut evt_rx).await;
            assert_eq!(action, ControlAction::Resumed);
            assert_eq!(lifecycle, TerminalLifecycle::Running);

            // Kill → the child actually dies; its EOF fires the exit notify.
            let (reply, ack) = oneshot::channel();
            handle
                .control
                .send(ControlRequest::Signal {
                    signal: TerminalSignal::Kill,
                    input_holder: None,
                    by,
                    reply,
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

/// phux-sw1: killing a pane (cancel the actor token → `shutdown_pty`)
/// must give a foreground job in a process group distinct from the shell a
/// chance to flush before it dies. This reproduces interactive job-control
/// topology rather than testing a shell and child that share one group.
///
/// **This test is load-sensitive by construction and cannot be made
/// otherwise from inside the test** (phux-axos). Everything BEFORE the
/// hangup is fenced by the `armed` barrier below, so no amount of
/// scheduling delay can reorder it. What remains is the assertion itself:
/// the trap handler has to run inside [`PANE_KILL_GRACE`], which is a
/// **500ms product budget**, not a test constant. Under starvation a shell
/// can miss that window while the product behaves exactly as designed.
/// Lengthening the grace to suit the test would change shipped behavior,
/// and no barrier can fence a window the product itself defines — so the
/// mitigation lives in `.config/nextest.toml`, which gives this test
/// `threads-required = 'num-cpus'` to clear the runner's own pool. That
/// cannot clear load from OUTSIDE the runner; a failure here on a box
/// running several concurrent builds says something about the box.
#[tokio::test(flavor = "current_thread")]
async fn pane_kill_lets_foreground_process_flush_before_death() {
    use portable_pty::CommandBuilder;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let marker = dir.path().join("flushed");
            let armed = dir.path().join("armed");

            // The script announces that its HUP trap is installed. Without
            // that handshake the test races the shell: a distinct process
            // group is established at exec, but `trap` runs afterwards, so
            // a SIGHUP arriving in between is handled by the DEFAULT
            // disposition — the shell dies silently and the marker is
            // never written. That gap is why this test failed under a
            // fully parallel `just test` (phux-2390) with an empty marker
            // in 0.7s: not a timeout, a missing synchronization point.
            let script = dir.path().join("foreground.sh");
            std::fs::write(
                &script,
                "trap 'printf flushed > \"$PHUX_TEST_MARKER\"; exit 0' HUP\n\
                     printf armed > \"$PHUX_TEST_ARMED\"\n\
                     while :; do sleep 30; done\n",
            )
            .expect("write foreground script");

            // Monitor mode makes the script a foreground job in its own
            // process group, matching an interactive shell running Claude.
            let mut cmd = CommandBuilder::new("/bin/sh");
            cmd.arg("-c");
            cmd.arg(format!("set -m; /bin/sh {}", script.display()));
            cmd.env("PHUX_TEST_MARKER", &marker);
            cmd.env("PHUX_TEST_ARMED", &armed);

            let token = CancellationToken::new();
            let bundle = TerminalActor::build_with_token(20, 5, Some(cmd), 1000, token.clone())
                .expect("build actor");
            let actor = bundle.actor;
            let pty = actor.pty.as_ref().expect("test actor has PTY");
            let shell_group = i32::try_from(pty.child.process_id().expect("shell pid"))
                .expect("shell pid fits i32");
            let master = std::sync::Arc::clone(&pty.master);
            let run = tokio::task::spawn_local(actor.run());

            // ONE barrier, not two deadlines (phux-axos). `armed` is
            // written by the inner shell AFTER `trap`, and the inner
            // shell is the process the outer shell's `set -m` put in its
            // own group at exec — so the file existing implies BOTH
            // preconditions this test needs, and implies them in the only
            // order they can happen in. There used to be a separate 2s
            // poll for the distinct process group ahead of this wait;
            // that deadline bought nothing (the `armed` wait strictly
            // dominates it) and cost a second way to fail for reasons
            // that are not the subject.
            //
            // The budget is deliberately generous and deliberately NOT
            // the subject's. What it covers is two `/bin/sh` processes
            // being forked, exec'd and scheduled — ambient work whose
            // cost is unbounded in machine load (phux-m64c measured a
            // freshly spawned `/bin/sh` taking 7.8s to run its first
            // instruction on a loaded box). The thing under test is what
            // `shutdown_pty` does AFTERWARDS, and it keeps its own budget
            // below. A pane that never arms fails here, against its own
            // number, with a message that names the environment rather
            // than blaming the flush path.
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

            // Now that the job is armed, the PTY's foreground group is
            // settled and can be read once instead of polled for.
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
            tokio::time::timeout(std::time::Duration::from_secs(5), run)
                .await
                .expect("actor shutdown timed out")
                .expect("actor task failed");

            let body = std::fs::read_to_string(&marker).unwrap_or_default();
            assert!(
                body.contains("flushed"),
                "foreground process must run its SIGHUP flush handler before \
                     the pane is killed; marker={body:?}",
            );
        })
        .await;
}

/// Interactive-latency regression gate: a queued keystroke must
/// interleave with a large pending PTY-output burst rather than wait
/// for the entire burst to drain. Pre-queues ~800KB of output (far
/// exceeding `MAX_PTY_COALESCE_BYTES`) plus one input event, runs the
/// actor, and asserts the input reaches the PTY writer channel while
/// the burst is still draining (the cumulative broadcast bytes seen
/// at that moment are far below the full burst). Fails if input is
/// serviced only after the entire burst drains (output-first ordering
/// or an unbounded coalesce that never yields).
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

            // Subscribe before spawning so no broadcast frame is
            // missed. This is the deterministic ordering gate: the
            // cumulative output bytes observed at the instant input
            // lands must be far below the full burst.
            let mut out_rx = handle.output.subscribe();

            // Pre-queue a burst far larger than MAX_PTY_COALESCE_BYTES
            // so it spans many capped vt_writes.
            let chunk = vec![b'x'; CHUNK_LEN];
            for _ in 0..CHUNK_COUNT {
                pty_evt_tx
                    .send(PtyEvent::Bytes(chunk.clone()))
                    .expect("queue burst");
            }
            // Queue ONE input event. With bracketed-paste mode 2004
            // off (a fresh Terminal's default) a Trusted paste of
            // b"x" encodes to exactly b"x" on the writer channel.
            handle
                .input
                .send(TerminalInput::Paste(PasteEvent {
                    trust: PasteTrust::Trusted,
                    data: b"x".to_vec(),
                }))
                .await
                .expect("queue input");

            tokio::task::spawn_local(actor.run());

            // The keystroke must be serviced mid-burst: it interleaves
            // rather than waiting for the whole burst to drain. The
            // timeout is only a backstop against a wedged actor — the
            // byte-ordering check below is what actually proves
            // "mid-burst", so the duration carries no meaning and is
            // sized to be unreachable under load (see
            // `ACTOR_EXIT_DEADLINE`).
            let got = tokio::time::timeout(ACTOR_EXIT_DEADLINE, writer_rx.recv())
                .await
                .expect("input must be serviced mid-burst, not after it");
            let got_bytes = got.map(|request| request.bytes);
            assert_eq!(
                got_bytes.as_deref(),
                Some(b"x".as_ref()),
                "queued keystroke should reach the PTY writer while the burst drains",
            );

            // Count broadcast bytes the actor has emitted so far.
            // Account Lagged-skipped frames toward the total (the
            // broadcast channel is bounded; a fast burst can lag this
            // receiver) so the ordering gate cannot under-report and
            // pass spuriously.
            let mut emitted: usize = 0;
            loop {
                match out_rx.try_recv() {
                    Ok(PaneOutput::Live { bytes, .. } | PaneOutput::Resync { bytes, .. }) => {
                        emitted += bytes.len();
                    }
                    Ok(PaneOutput::Control { .. }) => {}
                    Err(tokio::sync::broadcast::error::TryRecvError::Lagged(n)) => {
                        // Each lagged frame is one coalesced payload of
                        // at most MAX_PTY_COALESCE_BYTES; bound the
                        // skipped volume by that cap so the assertion
                        // stays conservative (never under-reports).
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
                    .send(PtyEvent::Bytes(vec![b'x'; CHUNK_BYTES]))
                    .expect("queue sustained PTY output");
            }
            let (reply, replied) = oneshot::channel();
            handle
                .native_bootstrap
                .send(NativeBootstrapRequest {
                    owner: 7,
                    terminal_id: phux_protocol::ids::TerminalId::local(1),
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
                if let PaneOutput::Live { seq, bytes } = output {
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
    let (pty_tx, mut pty) = mpsc::unbounded_channel();

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
        .send(PtyEvent::Bytes(vec![b'x']))
        .expect("ready PTY event");
    assert!(matches!(
        recv_native_or_pty(&mut native, Some(&mut pty), false).await,
        NativeOrPty::Pty(Some(PtyEvent::Bytes(bytes))) if bytes == b"x"
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
    let terminal_id = phux_protocol::ids::TerminalId::local(1);
    let stream_id = phux_protocol::ids::StreamId::new(1).expect("stream id");
    let bootstrap_id = phux_protocol::ids::BootstrapId::new(1).expect("bootstrap id");
    let cursor: crate::native_state::OpaqueHistoryCursor =
        [1; libghostty_vt::snapshot::incremental::TOKEN_LEN];
    actor.native_cursor_owners.insert(
        7,
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

    actor.raw_seq = 5;
    actor
        .output_tx
        .send(PaneOutput::Live {
            seq: 5,
            bytes: Bytes::from_static(b"prior"),
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

/// phux-rv52: a pane created while a client is attached is resized by the
/// layout the instant it exists, and `invalidate_all_native_cursors`
/// drains every binding. The `HISTORY_REQUEST` the client already sent for
/// the generation it was just handed then arrives against no binding at
/// all. That race is routine, so it must be answered with a per-replica
/// `HISTORY_TOMBSTONE` -- never a `NativeStateError`, which the connection
/// pump escalates to a connection-scoped Error frame and which used to
/// take the whole attach down.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
async fn a_cursor_invalidated_by_resize_is_tombstoned_never_faulted() {
    let bundle = TerminalActor::new(20, 5).expect("new actor");
    let mut actor = bundle.actor;
    let terminal_id = phux_protocol::ids::TerminalId::local(1);
    let stream_id = phux_protocol::ids::StreamId::new(1).expect("stream id");
    let bootstrap_id = phux_protocol::ids::BootstrapId::new(1).expect("bootstrap id");
    let cursor: crate::native_state::OpaqueHistoryCursor =
        [1; libghostty_vt::snapshot::incremental::TOKEN_LEN];
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
        answered
            .await
            .expect("history reply")
            .result
            .expect("an unusable cursor is answered, never faulted")
    };

    // The binding is gone entirely: the mid-attach resize drained it.
    actor.native_cursor_owners.insert(7, binding());
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

    // The binding exists but names an older generation: the client paged
    // against the bootstrap it held before the resize replaced it.
    actor.native_cursor_owners.insert(7, binding());
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
        actor.native_cursor_owners.contains_key(&7),
        "answering a stale request must not release the live binding"
    );
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
                handle: &TerminalHandle,
            ) -> Result<NativeBootstrapReply, crate::native_state::NativeStateError> {
                let (reply, replied) = oneshot::channel();
                handle
                    .native_bootstrap
                    .send(NativeBootstrapRequest {
                        owner: 11,
                        terminal_id: phux_protocol::ids::TerminalId::local(2),
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

            let retry_permit = outbound
                .clone()
                .reserve_owned()
                .await
                .expect("retry request permit");
            let (retry_reply, retried) = oneshot::channel();
            handle
                .native_history
                .send(NativeHistoryRequest {
                    permit: retry_permit,
                    owner: 11,
                    terminal_id: phux_protocol::ids::TerminalId::local(2),
                    stream_id: phux_protocol::ids::StreamId::new(2).expect("stream id"),
                    bootstrap_id: phux_protocol::ids::BootstrapId::new(4).expect("bootstrap id"),
                    cursor: cursor.clone(),
                    max_bytes: phux_protocol::caps::BootstrapLimits::default()
                        .max_history_page_bytes(),
                    max_rows: 128,
                    limits: phux_protocol::caps::BootstrapLimits::default(),
                    reply: retry_reply,
                })
                .await
                .expect("retry history request");
            let result = retried
                .await
                .expect("retry reply")
                .result
                .expect("history remains valid after capture allocation failures");
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
                let permit = outbound
                    .clone()
                    .reserve_owned()
                    .await
                    .expect("continuation request permit");
                let (reply, response) = oneshot::channel();
                handle
                    .native_history
                    .send(NativeHistoryRequest {
                        permit,
                        owner: 11,
                        terminal_id: phux_protocol::ids::TerminalId::local(2),
                        stream_id: phux_protocol::ids::StreamId::new(2).expect("stream id"),
                        bootstrap_id: phux_protocol::ids::BootstrapId::new(4)
                            .expect("bootstrap id"),
                        cursor: request_cursor,
                        max_bytes: phux_protocol::caps::BootstrapLimits::default()
                            .max_history_page_bytes(),
                        max_rows: 128,
                        limits: phux_protocol::caps::BootstrapLimits::default(),
                        reply,
                    })
                    .await
                    .expect("send continuation request");
                let frame = response
                    .await
                    .expect("continuation reply")
                    .result
                    .expect("continuation host");
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
/// phux-cs6: the actor answers a `PwdRequest` with its PTY child's
/// live working directory. A shell is spawned that `cd`s into a
/// freshly-created temp dir and then blocks (`read`), so its CWD is
/// the temp dir when the kernel query runs. This is the actor-level
/// proof of the inherit-focused acceptance criterion.
#[tokio::test(flavor = "current_thread")]
async fn actor_responds_to_pwd_request_with_pty_child_cwd() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            // Canonicalize: macOS hands back the realpath
            // (/private/var/... for /var/...), which is what the
            // kernel query returns too.
            let dir_path = dir.path().canonicalize().expect("canonicalize tempdir");

            let mut cmd = CommandBuilder::new("/bin/sh");
            cmd.arg("-c");
            cmd.arg(format!("cd '{}' && read _", dir_path.display()));
            let bundle = TerminalActor::build_with_token(
                20,
                5,
                Some(cmd),
                DEFAULT_MAX_SCROLLBACK,
                CancellationToken::new(),
            )
            .expect("build_with_token");
            let handle = bundle.handle.clone();
            let token = bundle.token;
            let join = tokio::task::spawn_local(bundle.actor.run());

            // Poll the actor until the shell has executed the `cd`.
            // The query races the child's startup, so retry briefly.
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
            let mut got: Option<String> = None;
            while tokio::time::Instant::now() < deadline {
                let (reply_tx, reply_rx) = oneshot::channel();
                handle
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

/// phux-cs6: a no-PTY actor has no child to query, so `pwd` is `None`
/// and the spawn path falls back to a non-inherited default.
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
                .pwd
                .send(PwdRequest { reply: reply_tx })
                .await
                .expect("send pwd request");
            assert_eq!(reply_rx.await.expect("pwd reply"), None);
        })
        .await;
}

/// The actor stops promptly when its cancellation token fires,
/// even if input/snapshot channels stay open.
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
            let _ = handle.snapshot.try_send(SnapshotRequest {
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

/// A parent token's `.cancel()` propagates to a `child_token()`-
/// linked `TerminalActor`, which exits within a short deadline. Pins
/// down the hierarchical cascade introduced by the
/// `CancellationToken` refactor.
#[tokio::test(flavor = "current_thread")]
async fn parent_token_cancel_cascades_to_pane_actor() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let parent = CancellationToken::new();
            let child = parent.child_token();
            let bundle =
                TerminalActor::build_with_token(20, 5, None, DEFAULT_MAX_SCROLLBACK, child)
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
