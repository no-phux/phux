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
    /// The process may have outlived its owner: mark it `Lost`.
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
    match recorded_process(recorded, boot, observe) {
        ProcessPresence::Gone => Verdict::Unobserved,
        _ if state == ExecState::Lost => Verdict::Keep,
        _ => Verdict::Lost,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProcessPresence {
    Matching,
    Gone,
    Uncertain,
}

fn recorded_process(
    recorded: ProcIdentity,
    boot: Option<[u8; 16]>,
    observe: impl FnOnce(i32) -> Observed,
) -> ProcessPresence {
    let Some(boot) = boot else {
        return ProcessPresence::Uncertain;
    };
    if boot != recorded.boot_id {
        return ProcessPresence::Gone;
    }
    match observe(recorded.pid) {
        Observed { alive: false, .. } => ProcessPresence::Gone,
        Observed {
            start_raw: Some(start),
            ..
        } if start == recorded.start_raw => ProcessPresence::Matching,
        Observed {
            start_raw: Some(_), ..
        } => ProcessPresence::Gone,
        Observed {
            start_raw: None, ..
        } => ProcessPresence::Uncertain,
    }
}

/// Whether the recorded process still exists under its original identity.
pub(crate) fn process_presence(recorded: ProcIdentity) -> ProcessPresence {
    recorded_process(recorded, boot_id(), observe)
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

    /// The probe a case expects: `None` must never be called.
    fn probe(expected: Option<Observed>) -> impl FnOnce(i32) -> Observed {
        move |pid| {
            assert_eq!(pid, 4242);
            expected.expect("must not probe")
        }
    }

    #[allow(
        clippy::unnecessary_wraps,
        reason = "a probed case; `None` is a case that must not probe"
    )]
    const fn seen(alive: bool, start_raw: Option<u64>) -> Option<Observed> {
        Some(Observed { alive, start_raw })
    }

    /// Each recorded activation and what the restart concludes about it.
    #[test]
    fn decide_reads_each_recorded_activation() {
        let other_boot = Some([8; 16]);
        #[rustfmt::skip]
        let cases = [
            ("vouched", ExecState::Running, true, Some(recorded(1)), Some(BOOT), None, Verdict::Keep),
            ("settled", ExecState::Completed, false, Some(recorded(1)), Some(BOOT), None, Verdict::Keep),
            ("gone pid", ExecState::Running, false, Some(recorded(1)), Some(BOOT), seen(false, None), Verdict::Unobserved),
            ("reused pid", ExecState::Running, false, Some(recorded(1)), Some(BOOT), seen(true, Some(2)), Verdict::Unobserved),
            ("another boot, no probe", ExecState::Running, false, Some(recorded(1)), other_boot, None, Verdict::Unobserved),
            ("matching live process", ExecState::Running, false, Some(recorded(1)), Some(BOOT), seen(true, Some(1)), Verdict::Lost),
            ("live pid but unreadable start", ExecState::Running, false, Some(recorded(1)), Some(BOOT), seen(true, None), Verdict::Lost),
            ("boot probe unavailable", ExecState::Stopping, false, Some(recorded(1)), None, None, Verdict::Lost),
            ("lost identity still uncertain", ExecState::Lost, false, Some(recorded(1)), Some(BOOT), seen(true, None), Verdict::Keep),
            ("lost again: a second restart changes nothing", ExecState::Lost, false, Some(recorded(1)), Some(BOOT), seen(true, Some(1)), Verdict::Keep),
            ("lost, then ended", ExecState::Lost, false, Some(recorded(1)), Some(BOOT), seen(false, None), Verdict::Unobserved),
            ("commit that never launched", ExecState::Starting, false, None, Some(BOOT), None, Verdict::NeverLaunched),
            ("running with no identity", ExecState::Running, false, None, Some(BOOT), None, Verdict::Unobserved),
        ];
        for (label, state, vouched, identity, boot, observed, want) in cases {
            assert_eq!(
                decide(state, vouched, identity, boot, probe(observed)),
                want,
                "{label}"
            );
        }
    }
}
