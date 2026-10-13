//! Startup reconciliation (ADR-0162 §5): what became of each activation a
//! previous server run left live.
//!
//! [`decide`] is pure: given the recorded state and [`ProcIdentity`] and
//! what the OS says now, it returns a [`Verdict`]. The shim around it
//! ([`observe`], [`boot_id`], [`proc_identity`]) asks the OS.
//! [`crate::ServerState::reconcile`] commits the verdicts as executor
//! reports. Nothing is ever relaunched here.

use phux_core::{ExecState, ProcIdentity};

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
        return Verdict::Unobserved;
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
            .then(|| crate::proc_query::process_start_time(pid))
            .flatten(),
    }
}

/// The identity of the process `pid`, recorded when an activation starts
/// (R12).
///
/// `None` when its start time cannot be read. The process group is read
/// once; a Terminal child leads its own group, and a mismatch is logged and
/// recorded as found.
#[must_use]
pub fn proc_identity(pid: i32) -> Option<ProcIdentity> {
    let start_raw = crate::proc_query::process_start_time(pid)?;
    let boot = boot_id()?;
    let group = nix::unistd::getpgid(Some(nix::unistd::Pid::from_raw(pid)))
        .map_or(pid, nix::unistd::Pid::as_raw);
    if group != pid {
        tracing::warn!(pid, group, "terminal child does not lead its process group");
    }
    Some(ProcIdentity {
        pid,
        pgid: group,
        start_raw,
        boot_id: boot,
    })
}

/// This boot's identity: Linux `/proc/sys/kernel/random/boot_id`, macOS
/// `kern.boottime` (seconds and microseconds, big-endian).
#[must_use]
pub fn boot_id() -> Option<[u8; 16]> {
    platform_boot_id()
}

#[cfg(target_os = "linux")]
fn platform_boot_id() -> Option<[u8; 16]> {
    let text = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    parse_boot_id(&text)
}

/// A UUID's 32 hex digits, dashes ignored.
#[cfg_attr(
    not(target_os = "linux"),
    allow(
        dead_code,
        reason = "compiled everywhere so the parser is tested everywhere"
    )
)]
fn parse_boot_id(text: &str) -> Option<[u8; 16]> {
    let hex: String = text.trim().chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 {
        return None;
    }
    let mut out = [0_u8; 16];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

#[cfg(target_os = "macos")]
fn platform_boot_id() -> Option<[u8; 16]> {
    let mut tv = libc::timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    let mut len = std::mem::size_of::<libc::timeval>();
    let name = c"kern.boottime";
    // SAFETY: `name` is a NUL-terminated C string; `tv` and `len` point at
    // a `timeval` and its size, which `kern.boottime` writes; no new value
    // is set (null, 0).
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            std::ptr::from_mut(&mut tv).cast(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || len != std::mem::size_of::<libc::timeval>() {
        return None;
    }
    let mut out = [0_u8; 16];
    out[..8].copy_from_slice(&tv.tv_sec.to_be_bytes());
    out[8..].copy_from_slice(&i64::from(tv.tv_usec).to_be_bytes());
    Some(out)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const fn platform_boot_id() -> Option<[u8; 16]> {
    None
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
    fn no_recorded_identity_is_unobserved() {
        let never = |_| -> Observed { panic!("must not probe") };
        assert_eq!(
            decide(ExecState::Starting, false, None, Some(BOOT), never),
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

    #[test]
    fn boot_ids_parse_as_uuids() {
        assert_eq!(
            parse_boot_id("01234567-89ab-cdef-0123-456789abcdef\n"),
            Some([
                0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab,
                0xcd, 0xef
            ])
        );
        assert_eq!(parse_boot_id("short"), None);
    }
}
