//! Startup reconciliation (ADR-0162 §5): what became of each activation a
//! previous server run left live.
//!
//! [`decide`] is pure: given the recorded state and [`ProcIdentity`] and
//! what the OS says now, it returns a [`Verdict`]. The shim around it
//! ([`observe`], [`boot_id`], [`proc_identity`]) asks the OS.
//! [`crate::ServerState::reconcile`] commits the verdicts as executor
//! reports. Nothing is ever relaunched here.

use phux_core::{ExecState, ProcIdentity};
pub use phux_resource::proc_query::{boot_id, proc_identity};

/// What the OS reports about a recorded pid right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Observed {
    /// The pid names a live process (signal 0 succeeded or was refused for
    /// permission).
    pub alive: bool,
    /// That process's raw start time, when it could be read.
    pub start_raw: Option<u64>,
}

/// What reconciliation does with one activation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Leave it: a live handover vouched for it, or it is already settled
    /// or already lost to a process that still runs.
    Keep,
    /// The process outlived its owner: mark it `Lost`.
    Lost,
    /// The process is gone: mark it `Completed` with outcome `Unobserved`.
    Unobserved,
    /// Launch intent committed and no process was ever recorded. Mark it
    /// `Failed`: committed, never launched. Not a completed unobserved exit.
    NeverLaunched,
}

/// Decide one activation's fate.
///
/// * `vouched`: a live-upgrade handover rebound it to an engine in this
///   process.
/// * `recorded`: the identity recorded when it started.
/// * `boot`: this boot's identity; a different boot means the pid is gone.
/// * `observe`: asks the OS about a pid.
#[must_use]
pub fn decide(
    state: ExecState,
    vouched: bool,
    recorded: Option<ProcIdentity>,
    boot: Option<[u8; 16]>,
    observe: impl FnOnce(i32) -> Observed,
) -> Verdict {
    if vouched || !(state.is_current() || state == ExecState::Lost) {
        return Verdict::Keep;
    }
    let Some(recorded) = recorded else {
        return if state == ExecState::Starting {
            Verdict::NeverLaunched
        } else {
            Verdict::Unobserved
        };
    };
    if boot != Some(recorded.boot_id) {
        return Verdict::Unobserved;
    }
    let now = observe(recorded.pid);
    if now.alive && now.start_raw == Some(recorded.start_raw) {
        if state == ExecState::Lost {
            Verdict::Keep
        } else {
            Verdict::Lost
        }
    } else {
        Verdict::Unobserved
    }
}

/// Ask the OS about `pid`.
#[must_use]
pub fn observe(pid: i32) -> Observed {
    use nix::errno::Errno;
    use nix::sys::signal::kill;
    use nix::unistd::Pid;

    if pid <= 0 {
        return Observed {
            alive: false,
            start_raw: None,
        };
    }
    let alive = matches!(kill(Pid::from_raw(pid), None), Ok(()) | Err(Errno::EPERM));
    Observed {
        alive,
        start_raw: alive
            .then(|| phux_resource::proc_query::process_start_time(pid))
            .flatten(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOOT: [u8; 16] = [7; 16];

    const fn recorded(start_raw: u64) -> ProcIdentity {
        ProcIdentity {
            pid: 4242,
            pgid: 4242,
            start_raw,
            boot_id: BOOT,
        }
    }

    fn running(alive: bool, start: Option<u64>) -> impl FnOnce(i32) -> Observed {
        move |pid| {
            assert_eq!(pid, 4242);
            Observed {
                alive,
                start_raw: start,
            }
        }
    }

    #[test]
    fn a_vouched_or_settled_activation_is_kept() {
        let never = |_| -> Observed { panic!("must not probe") };
        assert_eq!(
            decide(
                ExecState::Running,
                true,
                Some(recorded(1)),
                Some(BOOT),
                never
            ),
            Verdict::Keep
        );
        assert_eq!(
            decide(
                ExecState::Completed,
                false,
                Some(recorded(1)),
                Some(BOOT),
                never
            ),
            Verdict::Keep
        );
    }

    #[test]
    fn a_gone_pid_is_unobserved() {
        let v = decide(
            ExecState::Running,
            false,
            Some(recorded(1)),
            Some(BOOT),
            running(false, None),
        );
        assert_eq!(v, Verdict::Unobserved);
    }

    #[test]
    fn a_reused_pid_is_unobserved() {
        let v = decide(
            ExecState::Running,
            false,
            Some(recorded(1)),
            Some(BOOT),
            running(true, Some(2)),
        );
        assert_eq!(v, Verdict::Unobserved);
    }

    #[test]
    fn another_boot_is_unobserved_without_probing() {
        let never = |_| -> Observed { panic!("must not probe") };
        let v = decide(
            ExecState::Running,
            false,
            Some(recorded(1)),
            Some([8; 16]),
            never,
        );
        assert_eq!(v, Verdict::Unobserved);
    }

    #[test]
    fn a_matching_live_process_is_lost_once() {
        let v = decide(
            ExecState::Running,
            false,
            Some(recorded(1)),
            Some(BOOT),
            running(true, Some(1)),
        );
        assert_eq!(v, Verdict::Lost);
        let again = decide(
            ExecState::Lost,
            false,
            Some(recorded(1)),
            Some(BOOT),
            running(true, Some(1)),
        );
        assert_eq!(again, Verdict::Keep, "a second restart changes nothing");
        let ended = decide(
            ExecState::Lost,
            false,
            Some(recorded(1)),
            Some(BOOT),
            running(false, None),
        );
        assert_eq!(ended, Verdict::Unobserved);
    }

    #[test]
    fn a_commit_that_never_launched_is_not_an_unobserved_exit() {
        let never = |_| -> Observed { panic!("must not probe") };
        assert_eq!(
            decide(ExecState::Starting, false, None, Some(BOOT), never),
            Verdict::NeverLaunched
        );
        assert_eq!(
            decide(ExecState::Running, false, None, Some(BOOT), never),
            Verdict::Unobserved
        );
    }

    #[test]
    fn this_process_has_an_identity() {
        let pid = i32::try_from(std::process::id()).expect("pid");
        let observed = observe(pid);
        assert!(observed.alive);
        if let Some(id) = proc_identity(pid) {
            assert_eq!(Some(id.start_raw), observed.start_raw);
            assert_eq!(Some(id.boot_id), boot_id());
        }
    }
}
