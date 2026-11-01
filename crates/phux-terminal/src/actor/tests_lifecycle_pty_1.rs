use super::*;

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
                image_protocols: phux_protocol::caps::ImageProtocolSet::all(),
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
                .terminal()
                .expect("terminal facet")
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
        rx: &mut mpsc::Receiver<phux_resource::event_sink::Emitted>,
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
            let (evt_tx, mut evt_rx) = mpsc::channel::<phux_resource::event_sink::Emitted>(64);
            actor.set_event_sink(evt_tx);
            tokio::task::spawn_local(actor.run());

            let by = phux_protocol::ids::ClientId::new(7);

            // Freeze → Frozen.
            let (reply, ack) = oneshot::channel();
            handle
                .terminal()
                .expect("a Terminal")
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
                .terminal()
                .expect("a Terminal")
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
                .terminal()
                .expect("a Terminal")
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
                    Ok(
                        ResourceOutput::Live { bytes, .. } | ResourceOutput::Resync { bytes, .. },
                    ) => {
                        emitted += bytes.len();
                    }
                    Ok(ResourceOutput::Control { .. }) => {}
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
