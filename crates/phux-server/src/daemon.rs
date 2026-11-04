//! Descriptor hygiene for a daemon spawned by a short-lived client.
//!
//! An auto-spawned server outlives the client that started it, so every
//! descriptor it inherits stays open for the server's whole lifetime. Rust
//! opens its own descriptors close-on-exec, so what a spawner would pass along
//! is what *it* inherited from its caller. On macOS that includes a sibling's
//! capture pipe: `Command` there creates pipes with `pipe()` and sets
//! `FD_CLOEXEC` in a second step, so a caller spawning from several threads
//! can leak one child's stdout pipe into another. Handed on to the daemon, that
//! pipe never reaches EOF, and the caller's read of a `phux server --ensure`
//! that exited long ago blocks until the server does (phux-5wxp.5).

use std::os::fd::{BorrowedFd, RawFd};

/// Mark every descriptor above stderr close-on-exec, so the next child this
/// process spawns inherits only the stdio `Command` sets for it.
///
/// For a process about to spawn a daemon and with no descriptor it means any
/// child to inherit. The descriptors stay open here; only future children stop
/// inheriting them. Best-effort: an unreadable descriptor table, or a
/// descriptor closed meanwhile, leaves that flag as it was.
pub fn withhold_inherited_descriptors() {
    let Ok(entries) = std::fs::read_dir(DESCRIPTOR_TABLE) else {
        return;
    };
    // Act while iterating: the listing's own descriptor is still open, and is
    // close-on-exec already.
    for fd in entries.filter_map(|entry| descriptor_number(&entry.ok()?)) {
        if fd > 2 {
            set_cloexec(fd);
        }
    }
}

/// Raise this process's open-descriptor soft limit as far as the hard limit
/// allows, returning the new soft limit when it was raised.
///
/// A server started by a GUI app inherits launchd's soft limit of 256 on
/// macOS, which a few dozen panes exhaust: a PTY master costs three
/// descriptors (master, reader, writer) and a graceful upgrade duplicates each
/// one more, so new panes fail with `dup of fd 255 failed` and the upgrade
/// handoff with a half PTY pair. Where the hard limit is unlimited (macOS) the
/// kernel still refuses an unlimited soft limit, so a finite ceiling is tried
/// instead.
#[must_use]
pub fn raise_descriptor_limit() -> Option<u64> {
    use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};

    let Rlimit { current, maximum } = getrlimit(Resource::Nofile);
    [maximum, Some(1 << 20), Some(10_240)]
        .into_iter()
        .flatten()
        .find(|&want| {
            current.is_some_and(|have| want > have)
                && setrlimit(
                    Resource::Nofile,
                    Rlimit {
                        current: Some(want),
                        maximum,
                    },
                )
                .is_ok()
        })
}

/// The descriptor a descriptor-table entry names.
fn descriptor_number(entry: &std::fs::DirEntry) -> Option<RawFd> {
    entry.file_name().to_str()?.parse().ok()
}

/// The per-process view of open descriptors.
#[cfg(target_os = "linux")]
const DESCRIPTOR_TABLE: &str = "/proc/self/fd";
#[cfg(not(target_os = "linux"))]
const DESCRIPTOR_TABLE: &str = "/dev/fd";

fn set_cloexec(fd: RawFd) {
    use rustix::io::{FdFlags, fcntl_getfd, fcntl_setfd};

    // SAFETY: `fd` was listed in this process's descriptor table a moment ago
    // and the borrow ends within this call; nothing here closes it or takes
    // ownership. If another thread closed it meanwhile, `fcntl` fails with
    // `EBADF`, which is ignored; if the number was reused, the new descriptor
    // was opened close-on-exec by Rust already, so setting the flag is a no-op.
    let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
    if let Ok(flags) = fcntl_getfd(borrowed)
        && !flags.contains(FdFlags::CLOEXEC)
    {
        let _ = fcntl_setfd(borrowed, flags | FdFlags::CLOEXEC);
    }
}

// Exercised end to end, in its own process, by the service suite's
// `the_spawned_daemon_does_not_hold_the_callers_descriptors`. A unit test here
// would flip descriptor flags process-wide under `cargo test`, racing the
// upgrade tests that clear `FD_CLOEXEC` and assert on it.

#[cfg(test)]
mod tests {
    use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};

    #[test]
    fn raise_descriptor_limit_lifts_a_launchd_sized_soft_limit() {
        let before = getrlimit(Resource::Nofile);
        let low = Rlimit {
            current: Some(256),
            maximum: before.maximum,
        };
        setrlimit(Resource::Nofile, low).expect("lowering the soft limit needs no privilege");
        let raised = super::raise_descriptor_limit().expect("256 is raisable");
        assert!(raised > 256);
        assert_eq!(getrlimit(Resource::Nofile).current, Some(raised));
        let _ = setrlimit(Resource::Nofile, before);
    }
}
