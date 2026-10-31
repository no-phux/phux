use super::{
    PANE_KILL_POLL, PANE_KILL_REAP_BUDGET, PtyOwned, TerminalActor, debug, exit_outcome, trace,
    warn,
};

impl TerminalActor {
    /// Reap the child on PTY EOF if it has exited, returning its
    /// [`ExitOutcome`](phux_core::process::ExitOutcome) (code, signal, or
    /// unknown). A child still running is left to the shutdown path.
    pub(in crate::actor) fn reap_child_if_any(&mut self) -> phux_core::process::ExitOutcome {
        use phux_core::process::ExitOutcome;
        let Some(pty) = self.pty.as_mut() else {
            return ExitOutcome::UNKNOWN;
        };
        // EOF can precede the child becoming waitable, so retry briefly. The
        // blocking sleep is once per pane and tiny; a daemonizer that keeps
        // running exhausts it and reports unknown.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(20);
        loop {
            match pty.child.try_wait() {
                Ok(Some(status)) => {
                    debug!(?status, "child reaped on PTY EOF");
                    return exit_outcome(&status);
                }
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Ok(None) => {
                    trace!("PTY EOF but child still alive — leaving to shutdown path");
                    return ExitOutcome::UNKNOWN;
                }
                Err(err) => {
                    debug!(?err, "child try_wait failed on PTY EOF");
                    return ExitOutcome::UNKNOWN;
                }
            }
        }
    }

    /// React to PTY EOF: stop reading, reap the child, record the exit
    /// facet. The actor stays alive for late `SnapshotRequest`s and orderly
    /// shutdown; the run loop fires `exit_notify` after flushing gap
    /// resyncs. The runtime decides whether to close the pane or respawn a
    /// shell.
    pub(in crate::actor) fn handle_pty_eof(&mut self) {
        debug!("PTY EOF; recording exit and keeping actor alive for a final resync flush");
        self.pty_rx = None;
        let exit = self.reap_child_if_any();
        self.record_exit(exit);
        if self.program_status.expire_active() {
            self.publish_program_status();
        }
    }

    /// Return only an in-flight loan for EOF snapshots. Completed publication
    /// fences and history cuts still belong to this incarnation until close.
    pub(in crate::actor) fn land_native_capture(&mut self) {
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        if let Some(pending) = self.pending_native_bootstrap.take() {
            self.abort_native_bootstrap(pending, crate::native_state::NativeStateError::Resize);
        }
    }

    /// Fail any in-flight native bootstrap and tombstone every cursor,
    /// returning the terminal to the manager. The teardown paths run outside
    /// the `!bootstrap_pending` guards, so they land the cut instead of
    /// touching a loaned terminal. EOF does not retire an incarnation and
    /// uses `land_native_capture` instead. No-op without the native engine.
    pub(in crate::actor) fn land_native_cuts(&mut self) {
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        {
            let _ = self
                .invalidate_all_native_cursors(phux_protocol::wire::frame::TombstoneReason::Other);
        }
    }

    /// Release a retained pane's PTY after its process exited (ADR-0124);
    /// the grid stays. A child EOF could not reap goes to a reaper thread,
    /// decided by the recorded exit, not a second `try_wait` on a maybe
    /// recycled pid.
    pub(in crate::actor) fn release_pty_after_exit(&mut self) {
        self.pty_rx = None;
        drop(self.pty_tx.take());
        let Some(mut pty) = self.pty.take() else {
            return;
        };
        pty.fast.close();
        let pid = pty.child.process_id();
        self.released_child_pid = pid.and_then(|pid| i32::try_from(pid).ok());
        let reaped = self
            .exit
            .as_ref()
            .is_some_and(|exit| exit.status.is_some() || exit.signal.is_some());
        if !reaped {
            spawn_detached_reaper(pid);
        }
        // Both halves end on their own; detach rather than join.
        pty.reader.detach();
        drop(pty.writer_thread.take());
        drop(pty);
    }

    /// Tear down the PTY: gracefully stop a live child, drop the master, and
    /// join the bridge threads (bounded). Errors are logged.
    #[allow(
        clippy::future_not_send,
        reason = "ADR-0014: TerminalActor owns !Send Terminal; lives on LocalSet"
    )]
    pub(in crate::actor) async fn shutdown_pty(&mut self) {
        let Some(mut pty) = self.pty.take() else {
            return;
        };
        // Close the PTY receiver first. The actor stops draining it now, and
        // a full queue would park the reader, leaving the master unread and
        // wedging a child mid-flush in `write(2)`. Dropped, the reader
        // switches to discarding until EOF.
        self.pty_rx = None;
        // Hang up the child gracefully. A failed `try_wait` counts as alive:
        // signalling a dead child is harmless, not signalling a live one
        // blocked the reap forever.
        let observed = pty.child.try_wait();
        if needs_termination(&observed) {
            if let Err(err) = &observed {
                debug!(
                    ?err,
                    "pty child try_wait failed; assuming alive and terminating"
                );
            }
            Self::terminate_child_group(&mut pty).await;
        } else {
            trace!("pty child already exited");
        }
        // Close the writer channel so the writer thread exits.
        drop(self.pty_tx.take());
        pty.fast.close();
        // Reap without blocking this task; see `reap_bounded`.
        let child_pid = pty.child.process_id();
        if matches!(
            reap_bounded(|| pty.child.try_wait()).await,
            ReapOutcome::Expired
        ) {
            warn!("pty child did not exit within the reap budget; handing it to a reaper thread");
            spawn_detached_reaper(child_pid);
        }
        // Drop the PTY before joining: a reader blocked in `read(2)` (the
        // drain budget is only checked between reads) never returns while
        // we hold the master. Even then the reader's dup'd fd may keep it
        // blocked, so the joins are bounded.
        let reader_thread = pty.reader.shutdown();
        let writer_thread = pty.writer_thread.take();
        drop(pty);
        join_thread_bounded(reader_thread, "pty reader").await;
        join_thread_bounded(writer_thread, "pty writer").await;
    }

    /// Gracefully stop a live PTY child on teardown: `SIGHUP` both the
    /// foreground process group (snapshotted first, since the shell may exit
    /// at once) and the shell's group, wait up to
    /// [`super::super::PANE_KILL_GRACE`] for them to exit, then `SIGKILL` survivors.
    /// The master stays open so the foreground job can flush.
    #[allow(
        clippy::future_not_send,
        reason = "ADR-0014: TerminalActor owns !Send Terminal; lives on LocalSet"
    )]
    pub(in crate::actor) async fn terminate_child_group(pty: &mut PtyOwned) {
        let groups = pane_signal_groups(pty);
        if groups.is_empty() {
            hard_kill_child(pty);
            return;
        }
        if !hangup_pane_groups(&groups) {
            hard_kill_child(pty);
            return;
        }
        if await_pane_group_exit(pty, &groups).await {
            return;
        }
        hard_kill_pane_groups(&groups);
    }
}

/// Snapshot the process groups a hangup must reach, foreground job first
/// (`tcgetpgrp` fails once the shell exits).
fn pane_signal_groups(pty: &PtyOwned) -> Vec<nix::unistd::Pid> {
    use nix::unistd::Pid;

    let shell_group = pty
        .child
        .process_id()
        .and_then(|id| i32::try_from(id).ok())
        .map(Pid::from_raw);
    let foreground_group = pty
        .master
        .lock()
        .ok()
        .and_then(|master| master.process_group_leader())
        .filter(|id| *id > 0)
        .map(Pid::from_raw);

    let mut groups = Vec::with_capacity(2);
    if let Some(group) = foreground_group {
        groups.push(group);
    }
    if let Some(group) = shell_group
        && !groups.contains(&group)
    {
        groups.push(group);
    }
    groups
}

/// `SIGHUP` every group; `true` if at least one took it.
fn hangup_pane_groups(groups: &[nix::unistd::Pid]) -> bool {
    use nix::errno::Errno;
    use nix::sys::signal::{Signal, killpg};

    let mut delivered = false;
    for &group in groups {
        match killpg(group, Signal::SIGHUP) {
            Ok(()) => delivered = true,
            Err(Errno::ESRCH) => {}
            Err(err) => debug!(?err, ?group, "SIGHUP to pane group failed"),
        }
    }
    delivered
}

/// Wait out [`super::super::pane_kill_grace`], `true` once every group exited.
/// Reaps the shell as it goes so its zombie does not keep its group alive.
#[allow(
    clippy::future_not_send,
    reason = "ADR-0014: TerminalActor owns !Send Terminal; lives on LocalSet"
)]
async fn await_pane_group_exit(pty: &mut PtyOwned, groups: &[nix::unistd::Pid]) -> bool {
    use nix::errno::Errno;
    use nix::sys::signal::killpg;

    // Tests may start the ceiling only once a trap-started marker exists.
    #[cfg(test)]
    if let Some(gate) = super::super::pane_kill_grace_gate() {
        let hold = tokio::time::Instant::now() + super::super::PANE_KILL_GRACE_GATE_WAIT;
        loop {
            if let Err(err) = pty.child.try_wait() {
                debug!(?err, "try_wait during pane-kill grace gate failed");
            }
            if groups
                .iter()
                .all(|&group| matches!(killpg(group, None), Err(Errno::ESRCH)))
            {
                return true;
            }
            if gate.exists() || tokio::time::Instant::now() >= hold {
                break;
            }
            tokio::time::sleep(PANE_KILL_POLL).await;
        }
    }

    let deadline = tokio::time::Instant::now() + super::super::pane_kill_grace();
    while tokio::time::Instant::now() < deadline {
        if let Err(err) = pty.child.try_wait() {
            debug!(?err, "try_wait during pane-kill grace failed");
        }
        if groups
            .iter()
            .all(|&group| matches!(killpg(group, None), Err(Errno::ESRCH)))
        {
            return true;
        }
        tokio::time::sleep(PANE_KILL_POLL).await;
    }
    false
}

/// Whether a `try_wait` result needs termination: only a confirmed exit
/// says no; an error means "unknown", treated as alive.
const fn needs_termination(observed: &std::io::Result<Option<portable_pty::ExitStatus>>) -> bool {
    !matches!(observed, Ok(Some(_)))
}

/// What [`reap_bounded`] concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReapOutcome {
    /// The child was collected; nothing is left behind.
    Reaped,
    /// `try_wait` itself failed. Nothing more to try here.
    Failed,
    /// The budget ran out with the child still running.
    Expired,
}

/// Poll until the child is collected or [`PANE_KILL_REAP_BUDGET`] expires,
/// never blocking the task (a blocking `waitpid` on an escaped child would
/// freeze the shared runtime). Takes a closure so it is testable.
async fn reap_bounded<F>(mut poll: F) -> ReapOutcome
where
    F: FnMut() -> std::io::Result<Option<portable_pty::ExitStatus>>,
{
    let deadline = tokio::time::Instant::now() + PANE_KILL_REAP_BUDGET;
    loop {
        match poll() {
            Ok(Some(status)) => {
                debug!(?status, "pty child reaped");
                return ReapOutcome::Reaped;
            }
            Ok(None) => {}
            Err(err) => {
                debug!(?err, "pty child wait failed");
                return ReapOutcome::Failed;
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return ReapOutcome::Expired;
        }
        tokio::time::sleep(PANE_KILL_POLL).await;
    }
}

/// Hand a child that outlived the reap budget to a detached `waitpid`
/// thread; nothing else would ever reap it.
fn spawn_detached_reaper(pid: Option<u32>) {
    let Some(pid) = pid.and_then(|raw| i32::try_from(raw).ok()) else {
        warn!("pty child outlived the reap budget and has no pid; it will stay a zombie");
        return;
    };
    let spawned = std::thread::Builder::new()
        .name("phux-pty-reaper".to_owned())
        .spawn(move || {
            let _ = nix::sys::wait::waitpid(nix::unistd::Pid::from_raw(pid), None);
        });
    if let Err(err) = spawned {
        warn!(
            ?err,
            pid, "could not spawn a reaper thread; the child will stay a zombie"
        );
    }
}

/// Join a bridge thread, detaching it if it misses
/// [`PANE_KILL_REAP_BUDGET`] (threads cannot be cancelled; a bare `join`
/// could freeze the runtime).
async fn join_thread_bounded(handle: Option<std::thread::JoinHandle<()>>, what: &'static str) {
    let Some(handle) = handle else {
        return;
    };
    let deadline = tokio::time::Instant::now() + PANE_KILL_REAP_BUDGET;
    while !handle.is_finished() {
        if tokio::time::Instant::now() >= deadline {
            warn!(
                thread = what,
                "pty bridge thread did not exit within the budget; detaching it"
            );
            return;
        }
        tokio::time::sleep(PANE_KILL_POLL).await;
    }
    let _ = handle.join();
}

/// `SIGKILL` the child directly. `portable_pty`'s killer sends `SIGHUP`
/// and sleeps up to 200 ms on the actor task.
fn hard_kill_child(pty: &mut PtyOwned) {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;

    if let Some(pid) = pty
        .child
        .process_id()
        .and_then(|raw| i32::try_from(raw).ok())
    {
        let _ = kill(Pid::from_raw(pid), Signal::SIGKILL);
        return;
    }
    // No pid to aim at: the library killer is all that is left.
    let _ = pty.child.kill();
}

/// Backstop: `SIGKILL` every surviving group.
fn hard_kill_pane_groups(groups: &[nix::unistd::Pid]) {
    use nix::errno::Errno;
    use nix::sys::signal::{Signal, killpg};

    for &group in groups {
        if !matches!(killpg(group, None), Err(Errno::ESRCH)) {
            let _ = killpg(group, Signal::SIGKILL);
        }
    }
}

#[cfg(test)]
mod teardown_policy_tests {
    use super::{ReapOutcome, join_thread_bounded, needs_termination, reap_bounded};
    use crate::actor::{PANE_KILL_POLL, PANE_KILL_REAP_BUDGET};

    /// A thread that never exits costs the budget and is then abandoned
    /// (the portable form of the slave-held-open hang).
    #[tokio::test(start_paused = true)]
    async fn a_thread_that_never_exits_is_detached_rather_than_joined() {
        let (release, parked) = std::sync::mpsc::channel::<()>();
        let handle = std::thread::spawn(move || {
            // Stand-in for a reader blocked on a slave nobody closes.
            let _ = parked.recv();
        });

        let started = tokio::time::Instant::now();
        join_thread_bounded(Some(handle), "never-exits").await;
        let waited = started.elapsed();

        assert!(
            waited >= PANE_KILL_REAP_BUDGET,
            "the join must serve its whole budget before detaching; gave up after {waited:?}",
        );
        // Let the thread finish so the test leaves nothing parked behind.
        let _ = release.send(());
    }

    /// A thread that exits is joined promptly.
    #[tokio::test(start_paused = true)]
    async fn a_thread_that_exits_is_joined_without_serving_the_budget() {
        let handle = std::thread::spawn(|| {});
        while !handle.is_finished() {
            std::thread::yield_now();
        }

        let started = tokio::time::Instant::now();
        join_thread_bounded(Some(handle), "exits").await;

        assert!(
            started.elapsed() < PANE_KILL_REAP_BUDGET,
            "a bridge thread that has already exited must not cost a budget",
        );
    }

    /// An unknown `try_wait` result means terminate.
    #[test]
    fn only_a_confirmed_exit_skips_termination() {
        assert!(
            !needs_termination(&Ok(Some(portable_pty::ExitStatus::with_exit_code(0)))),
            "a reaped child needs nothing"
        );
        assert!(needs_termination(&Ok(None)), "still running");
        assert!(
            needs_termination(&Err(std::io::Error::other("try_wait blew up"))),
            "an unreadable status must be read as alive: signalling a dead child is \
             harmless, stranding a live one is not",
        );
    }

    /// A child that never exits cannot hold the actor.
    #[tokio::test(start_paused = true)]
    async fn a_child_that_never_exits_expires_the_budget_instead_of_blocking() {
        let started = tokio::time::Instant::now();
        let mut polls = 0_u32;
        let outcome = reap_bounded(|| {
            polls += 1;
            Ok(None)
        })
        .await;

        assert_eq!(outcome, ReapOutcome::Expired);
        assert!(
            started.elapsed() >= PANE_KILL_REAP_BUDGET,
            "the reap must serve its whole budget before abandoning the child",
        );
        // It has to actually poll, not just sleep out the budget once.
        let expected = PANE_KILL_REAP_BUDGET.as_millis() / PANE_KILL_POLL.as_millis();
        assert!(
            u128::from(polls) >= expected,
            "expected ~{expected} polls across the budget, got {polls}",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_child_that_exits_is_reaped_without_serving_the_budget() {
        let started = tokio::time::Instant::now();
        let mut remaining = 2_u32;
        let outcome = reap_bounded(|| {
            if remaining == 0 {
                return Ok(Some(portable_pty::ExitStatus::with_exit_code(0)));
            }
            remaining -= 1;
            Ok(None)
        })
        .await;

        assert_eq!(outcome, ReapOutcome::Reaped);
        assert!(
            started.elapsed() < PANE_KILL_REAP_BUDGET,
            "a child that exits early must not cost the whole budget",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_failing_try_wait_gives_up_rather_than_spinning_out_the_budget() {
        let outcome = reap_bounded(|| Err(std::io::Error::other("no such child"))).await;
        assert_eq!(
            outcome,
            ReapOutcome::Failed,
            "there is nothing to retry when the status itself is unreadable",
        );
    }
}
