//! The Terminal engine's answer to the `process` facet of
//! `GET_TERMINAL_STATE` (L1 §6.3, PHA-406 D5).
//!
//! Every fact is kernel-sourced or mark-sourced and best-effort: a failed
//! query is `None`, never a guess. Only the PTY's own child and the tty's
//! foreground process group are ever queried, and only the foreground's
//! argv0 basename leaves the server.

use std::time::{SystemTime, UNIX_EPOCH};

use phux_core::process::{
    ExitOutcome, ForegroundProcess, ProcessExit, ProcessIdentity, TerminalProcessState,
};

use super::{ProcessFacetRequest, PtyOwned, TerminalActor};

/// Facts about the PTY child captured once, right after it is spawned or
/// adopted, when its pid certainly still names it.
#[derive(Debug, Default)]
pub(super) struct ChildFacts {
    /// The child's start time in Unix ms: the pid generation.
    pub(super) start_ms: Option<u64>,
    /// The child's working directory at spawn; empty when unknown.
    pub(super) cwd: String,
}

impl ChildFacts {
    /// Query the kernel about the child of `pty`. `std`'s spawn returns only
    /// after `exec` succeeded, so the child's `chdir` has already happened
    /// and the cwd read here is its real starting directory.
    pub(super) fn capture(pty: Option<&PtyOwned>) -> Self {
        let Some(pid) = pty.and_then(|p| p.child.process_id()) else {
            return Self::default();
        };
        Self {
            start_ms: i32::try_from(pid)
                .ok()
                .and_then(crate::proc_query::process_start_ms),
            cwd: crate::cwd_query::process_cwd(pid)
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default(),
        }
    }
}

impl TerminalActor {
    /// Answer a [`ProcessFacetRequest`].
    pub(super) fn reply_process_facet(&self, req: ProcessFacetRequest) {
        let _ = req.reply.send(self.process_facet());
    }

    /// The typed process facet. `foreground` and `cwd` are live kernel
    /// queries, so they are `None` once the child has exited; `child` keeps
    /// naming the process that ran, from the identity captured at spawn.
    pub(super) fn process_facet(&self) -> TerminalProcessState {
        let live = self.exit.is_none();
        TerminalProcessState {
            child: self.child_identity(),
            foreground: live.then(|| self.foreground_process()).flatten(),
            cwd: live.then(|| self.child_cwd()).flatten(),
            prompt: self.prompt.facet(),
            exit: self.exit.clone(),
        }
    }

    /// Record the exit facet for the outcome PTY EOF reaped.
    pub(super) fn record_exit(&mut self, outcome: ExitOutcome) {
        self.exit = Some(ProcessExit::observed(outcome, unix_now_ms()));
    }

    fn child_pid(&self) -> Option<i32> {
        let pid = self.pty.as_ref()?.child.process_id()?;
        i32::try_from(pid).ok()
    }

    fn child_identity(&self) -> Option<ProcessIdentity> {
        Some(ProcessIdentity {
            pid: self.child_pid()?,
            start_ms: self.child_start_ms,
        })
    }

    /// The tty's foreground process group: its pgid, the group leader's
    /// start time, and the leader's argv0 basename (nothing else of argv).
    ///
    /// The group can change hands while argv and the start time are read.
    /// A reading that straddled two groups would pair one group's id with
    /// another's name, so a pgid that moved during the read is no answer.
    fn foreground_process(&self) -> Option<ForegroundProcess> {
        let master_fd = self.pty.as_ref()?.master.lock().ok()?.as_raw_fd()?;
        let pgid = crate::proc_query::foreground_pgid(master_fd)?;
        let foreground = ForegroundProcess {
            pgid,
            start_ms: self.group_leader_start_ms(pgid),
            name: crate::proc_query::process_argv(pgid)
                .and_then(|argv| crate::proc_query::argv0_name(&argv)),
        };
        let unchanged = crate::proc_query::foreground_pgid(master_fd) == Some(pgid);
        unchanged.then_some(foreground)
    }

    /// Start time of the group leader `pgid`. When the leader is the PTY
    /// child itself, reuse the identity captured at spawn so the two facets
    /// always agree on that process's generation.
    fn group_leader_start_ms(&self, pgid: i32) -> Option<u64> {
        if self.child_pid() == Some(pgid) {
            return self.child_start_ms;
        }
        crate::proc_query::process_start_ms(pgid)
    }

    fn child_cwd(&self) -> Option<String> {
        let pid = u32::try_from(self.child_pid()?).ok()?;
        crate::cwd_query::process_cwd(pid).map(|path| path.to_string_lossy().into_owned())
    }
}

/// Wall-clock now in Unix ms; `None` for a clock before the epoch.
fn unix_now_ms() -> Option<u64> {
    let since_epoch = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
    u64::try_from(since_epoch.as_millis()).ok()
}
