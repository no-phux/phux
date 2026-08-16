//! Kernel-side process introspection for the agent detector (ADR-0046).
//!
//! Sibling of [`crate::cwd_query`], same shape and same contract: every
//! query is **best-effort**. A dead child, a permission error, a closed fd,
//! or an unsupported platform all yield `None`, never an error the caller
//! has to handle.
//!
//! The detector identifies WHICH agent binary is running in a pane by
//! asking the kernel, not by parsing the title. The title is a string the
//! program chose to print; the foreground process group is what the kernel
//! knows. Two calls, in order:
//!
//! 1. [`foreground_pgid`] — which process group currently owns the pane's
//!    tty (i.e. what the user is actually interacting with, not the shell
//!    that happens to be its parent).
//! 2. [`process_argv`] — that process's argv, from which
//!    [`crate::agent_detect::identify`] resolves the agent kind.
//!
//! Platform split:
//! * **`foreground_pgid`** — `tcgetpgrp(2)` through the safe `nix` wrapper.
//!   Cross-platform; no `libc`, which this crate declares only under a
//!   macOS `cfg` gate.
//! * **`process_argv`** — Linux reads `/proc/<pid>/cmdline` (NUL-separated,
//!   pure safe std I/O, no dependency at all); macOS calls
//!   `sysctl(KERN_PROCARGS2)`, one `unsafe` FFI block isolated here exactly
//!   as [`crate::cwd_query`] isolates `proc_pidinfo`.
//!
//! Nothing here reads the pane's *content*. The detector's process query
//! sees a pgid and an argv; it never logs either at anything above `trace`.

#![allow(
    clippy::redundant_pub_crate,
    reason = "private server module shared by the sibling agent_detect module"
)]
#![allow(
    clippy::similar_names,
    reason = "`argc` and `argv` are the kernel's own names for these two fields of \
              KERN_PROCARGS2; renaming them for the linter's benefit would obscure the format"
)]

use std::os::fd::RawFd;

/// Foreground process group id of the PTY whose master is `master_fd`.
///
/// `None` when the fd is dead, is not a tty, has no foreground group, or
/// the platform does not support the query.
#[must_use]
pub(crate) fn foreground_pgid(master_fd: RawFd) -> Option<i32> {
    if master_fd < 0 {
        return None;
    }
    // SAFETY: `master_fd` is the raw fd of the `PtyOwned::master` this actor
    // owns for its entire lifetime, obtained the same way the graceful-upgrade
    // handle obtains it (`terminal_actor::run_loop`'s upgrade arm). The
    // `BorrowedFd` is used only for the duration of the `tcgetpgrp` call and
    // is never stored, so it cannot outlive the master. A closed or invalid
    // fd makes `tcgetpgrp` return `EBADF`, which we map to `None`.
    let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(master_fd) };
    nix::unistd::tcgetpgrp(borrowed)
        .ok()
        .map(nix::unistd::Pid::as_raw)
        .filter(|pgid| *pgid > 0)
}

/// The full argv of `pid`, as the kernel reports it.
///
/// `None` when the pid is unknown, the process has exited, the query is
/// denied, or the platform is unsupported.
#[must_use]
pub(crate) fn process_argv(pid: i32) -> Option<Vec<String>> {
    if pid <= 0 {
        return None;
    }
    platform::process_argv(pid)
}

#[cfg(target_os = "linux")]
mod platform {
    /// `/proc/<pid>/cmdline` is the argv vector, NUL-separated, with a
    /// trailing NUL. A kernel thread has an empty cmdline; treat that as
    /// "no answer" rather than an empty argv.
    pub(super) fn process_argv(pid: i32) -> Option<Vec<String>> {
        let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        let argv: Vec<String> = raw
            .split(|b| *b == 0)
            .filter(|part| !part.is_empty())
            .map(|part| String::from_utf8_lossy(part).into_owned())
            .collect();
        (!argv.is_empty()).then_some(argv)
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ptr;

    /// `sysctl(KERN_PROCARGS2)` returns, for one pid:
    ///
    /// ```text
    /// [ argc: i32 ][ exec_path\0 ][ \0 padding ][ argv[0]\0 ... argv[argc-1]\0 ][ env... ]
    /// ```
    ///
    /// We read `argc`, skip the exec path and its alignment padding, then
    /// take exactly `argc` NUL-terminated strings. Anything malformed
    /// yields `None`.
    pub(super) fn process_argv(pid: i32) -> Option<Vec<String>> {
        let buf = procargs2(pid)?;
        parse_procargs2(&buf)
    }

    /// Fetch the raw `KERN_PROCARGS2` blob for `pid`.
    fn procargs2(pid: i32) -> Option<Vec<u8>> {
        let mut mib: [libc::c_int; 3] = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
        let mut size: libc::size_t = 0;

        // SAFETY: `mib` is a 3-element array of C ints, matching the `namelen`
        // of 3 we pass. A NULL `oldp` with a non-NULL `oldlenp` is the
        // documented "tell me the required size" form of `sysctl(3)`; the
        // kernel writes only into `size`. No buffer is read or written.
        let rc = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                3,
                ptr::null_mut(),
                ptr::addr_of_mut!(size),
                ptr::null_mut(),
                0,
            )
        };
        if rc != 0 || size == 0 {
            return None;
        }

        let mut buf = vec![0u8; size];
        // SAFETY: `buf` is an owned, initialized allocation of exactly `size`
        // bytes, and we pass `&mut size` as `oldlenp`, so the kernel writes at
        // most `size` bytes into it and updates `size` with how many it
        // actually wrote. `mib`/`namelen` are as above. The call reads kernel
        // state for `pid` and writes only into our buffer.
        let rc = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                3,
                buf.as_mut_ptr().cast::<libc::c_void>(),
                ptr::addr_of_mut!(size),
                ptr::null_mut(),
                0,
            )
        };
        if rc != 0 {
            return None;
        }
        buf.truncate(size);
        Some(buf)
    }

    /// Pure parser for the `KERN_PROCARGS2` layout. Unit-tested against a
    /// hand-built blob so the format handling is exercised without a live
    /// process.
    fn parse_procargs2(buf: &[u8]) -> Option<Vec<String>> {
        const ARGC_LEN: usize = 4;
        let argc_bytes: [u8; ARGC_LEN] = buf.get(..ARGC_LEN)?.try_into().ok()?;
        let argc = usize::try_from(i32::from_ne_bytes(argc_bytes)).ok()?;
        if argc == 0 {
            return None;
        }

        let rest = buf.get(ARGC_LEN..)?;
        // Skip the exec path (NUL-terminated) ...
        let exec_end = rest.iter().position(|b| *b == 0)?;
        let mut cursor = exec_end + 1;
        // ... and the NUL padding the kernel inserts to realign argv.
        while rest.get(cursor) == Some(&0) {
            cursor += 1;
        }

        let mut argv = Vec::with_capacity(argc);
        for _ in 0..argc {
            let tail = rest.get(cursor..)?;
            let end = tail.iter().position(|b| *b == 0).unwrap_or(tail.len());
            argv.push(String::from_utf8_lossy(&tail[..end]).into_owned());
            cursor += end + 1;
        }
        Some(argv)
    }

    #[cfg(test)]
    #[allow(clippy::expect_used, reason = "tests")]
    mod tests {
        use super::parse_procargs2;

        fn blob(argc: i32, exec_path: &str, pad: usize, argv: &[&str]) -> Vec<u8> {
            let mut out = argc.to_ne_bytes().to_vec();
            out.extend_from_slice(exec_path.as_bytes());
            out.push(0);
            out.extend(std::iter::repeat_n(0u8, pad));
            for arg in argv {
                out.extend_from_slice(arg.as_bytes());
                out.push(0);
            }
            out.extend_from_slice(b"PATH=/usr/bin\0");
            out
        }

        #[test]
        fn parses_argv_past_exec_path_and_padding() {
            let raw = blob(2, "/usr/local/bin/node", 6, &["node", "/opt/cli.js"]);
            let argv = parse_procargs2(&raw).expect("parses");
            assert_eq!(argv, vec!["node".to_owned(), "/opt/cli.js".to_owned()]);
        }

        #[test]
        fn parses_with_no_padding() {
            let raw = blob(1, "/bin/claude", 0, &["claude"]);
            assert_eq!(
                parse_procargs2(&raw).expect("parses"),
                vec!["claude".to_owned()]
            );
        }

        #[test]
        fn stops_at_argc_and_does_not_leak_the_environment() {
            let raw = blob(1, "/bin/claude", 2, &["claude"]);
            let argv = parse_procargs2(&raw).expect("parses");
            assert_eq!(argv.len(), 1, "env must not be mistaken for argv");
        }

        #[test]
        fn truncated_and_degenerate_blobs_are_none() {
            assert!(parse_procargs2(&[]).is_none());
            assert!(parse_procargs2(&[1, 2]).is_none());
            assert!(parse_procargs2(&0i32.to_ne_bytes()).is_none(), "argc 0");
            // argc claims 3 args but the blob holds none.
            let mut raw = 3i32.to_ne_bytes().to_vec();
            raw.extend_from_slice(b"/bin/x\0");
            assert!(parse_procargs2(&raw).is_none());
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod platform {
    pub(super) fn process_argv(_pid: i32) -> Option<Vec<String>> {
        None
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::{foreground_pgid, process_argv};

    /// A regular file is not a tty, so it has no foreground process group.
    /// The query must degrade to `None`, not error.
    #[test]
    fn foreground_pgid_on_a_non_tty_is_none() {
        use std::os::fd::AsRawFd;
        let file = tempfile::tempfile().expect("temp file");
        assert_eq!(foreground_pgid(file.as_raw_fd()), None);
    }

    #[test]
    fn foreground_pgid_on_a_bogus_fd_is_none() {
        assert_eq!(foreground_pgid(-1), None);
        assert_eq!(foreground_pgid(i32::MAX), None);
    }

    /// The test binary can read its own argv back from the kernel.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn process_argv_of_self_contains_the_test_binary() {
        let pid = i32::try_from(std::process::id()).expect("pid fits i32");
        let argv = process_argv(pid).expect("self argv is queryable");
        assert!(!argv.is_empty());
        // argv[0] is the test binary's path, whatever the harness chose.
        let own = std::env::args().next().expect("argv[0]");
        assert_eq!(argv[0], own);
    }

    #[test]
    fn process_argv_of_impossible_pids_is_none() {
        assert_eq!(process_argv(0), None);
        assert_eq!(process_argv(-1), None);
        assert_eq!(process_argv(i32::MAX), None);
    }
}
