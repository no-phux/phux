//! Kernel process introspection for the agent detector (ADR-0046),
//! best-effort like [`crate::cwd_query`]: any failure yields `None`.
//!
//! [`foreground_pgid`] (`tcgetpgrp` via `nix`) finds the process group
//! owning the pane's tty; [`process_argv`] reads its argv (Linux
//! `/proc/<pid>/cmdline`, macOS `sysctl(KERN_PROCARGS2)`). Pane content is
//! never read, and pgids and argv are logged at `trace` at most.

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

/// Foreground process group of the PTY `master_fd`, or `None`.
#[must_use]
pub(crate) fn foreground_pgid(master_fd: RawFd) -> Option<i32> {
    if master_fd < 0 {
        return None;
    }
    // SAFETY: `master_fd` is the actor's own live master fd; the borrow is
    // used only for this call and never stored. A bad fd yields `EBADF`,
    // mapped to `None`.
    let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(master_fd) };
    nix::unistd::tcgetpgrp(borrowed)
        .ok()
        .map(nix::unistd::Pid::as_raw)
        .filter(|pgid| *pgid > 0)
}

/// The argv of `pid`, or `None`.
#[must_use]
pub(crate) fn process_argv(pid: i32) -> Option<Vec<String>> {
    if pid <= 0 {
        return None;
    }
    platform::process_argv(pid)
}

/// A process's display name: argv0's basename without a login dash. The
/// only part of argv that may leave the server; `None` if empty.
#[must_use]
pub(crate) fn argv0_name(argv: &[String]) -> Option<String> {
    let first = argv.first()?;
    let base = first.rsplit('/').next().unwrap_or(first);
    let name = base.trim_start_matches('-');
    (!name.is_empty()).then(|| name.to_owned())
}

/// `pid`'s start time in a platform unit (Linux clock ticks since boot,
/// macOS microseconds since the epoch), compared only for equality; pairs
/// with a recycled pid to tell processes apart. `None` is "no answer".
#[must_use]
pub(crate) fn process_start_time(pid: i32) -> Option<u64> {
    if pid <= 0 {
        return None;
    }
    start::start_time(pid)
}

/// `pid`'s start time in Unix ms (Linux adds `/proc/stat` `btime`, read
/// once, so it is stable per process to within a second of absolute
/// error).
#[must_use]
pub(crate) fn process_start_ms(pid: i32) -> Option<u64> {
    if pid <= 0 {
        return None;
    }
    start::start_ms(pid)
}

/// Start-time queries: macOS `PROC_PIDTBSDINFO` via `libc`, Linux `/proc`
/// plus `sysconf(_SC_CLK_TCK)`.
mod start {
    /// Field 22 (`starttime`) of `/proc/<pid>/stat`. `comm` may contain
    /// spaces and parentheses, so parse after the last `)`. Compiled
    /// everywhere so it is tested everywhere.
    #[cfg_attr(
        not(target_os = "linux"),
        allow(
            dead_code,
            reason = "compiled everywhere so the parser is covered off a Linux CI leg"
        )
    )]
    pub(super) fn parse_stat_starttime(stat: &str) -> Option<u64> {
        /// `starttime` is field 22; the first field after the `)` is field 3.
        const STARTTIME_INDEX_AFTER_COMM: usize = 22 - 3;
        let after_comm = stat.rsplit_once(')')?.1;
        after_comm
            .split_whitespace()
            .nth(STARTTIME_INDEX_AFTER_COMM)?
            .parse::<u64>()
            .ok()
    }

    /// The `btime` line of `/proc/stat`: boot time in whole Unix seconds.
    #[cfg_attr(
        not(target_os = "linux"),
        allow(
            dead_code,
            reason = "compiled everywhere so the parser is covered off a Linux CI leg"
        )
    )]
    pub(super) fn parse_btime(proc_stat: &str) -> Option<u64> {
        proc_stat
            .lines()
            .find_map(|line| line.strip_prefix("btime "))
            .and_then(|secs| secs.trim().parse::<u64>().ok())
    }

    /// Clock ticks since boot to Unix ms; `None` on a zero rate or overflow.
    #[cfg_attr(
        not(target_os = "linux"),
        allow(
            dead_code,
            reason = "compiled everywhere so the conversion is covered off a Linux CI leg"
        )
    )]
    pub(super) fn ticks_since_boot_to_unix_ms(
        ticks: u64,
        boot_secs: u64,
        ticks_per_sec: u64,
    ) -> Option<u64> {
        if ticks_per_sec == 0 {
            return None;
        }
        let since_boot_ms = ticks.checked_mul(1000)? / ticks_per_sec;
        boot_secs.checked_mul(1000)?.checked_add(since_boot_ms)
    }

    /// `sysconf(_SC_CLK_TCK)` (compiled on all Unix for type-checking).
    #[cfg_attr(
        not(target_os = "linux"),
        allow(dead_code, reason = "only Linux reports start times in clock ticks")
    )]
    pub(super) fn clock_ticks_per_second() -> Option<u64> {
        nix::unistd::sysconf(nix::unistd::SysconfVar::CLK_TCK)
            .ok()
            .flatten()
            .and_then(|ticks| u64::try_from(ticks).ok())
            .filter(|ticks| *ticks > 0)
    }

    #[cfg(target_os = "linux")]
    pub(super) fn start_time(pid: i32) -> Option<u64> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        parse_stat_starttime(&stat)
    }

    #[cfg(target_os = "linux")]
    pub(super) fn start_ms(pid: i32) -> Option<u64> {
        let ticks = start_time(pid)?;
        ticks_since_boot_to_unix_ms(ticks, boot_secs()?, clock_ticks_per_second()?)
    }

    /// Boot time from `/proc/stat`, read once so a clock step cannot change
    /// a process's start; failures are not cached.
    #[cfg(target_os = "linux")]
    fn boot_secs() -> Option<u64> {
        static BOOT_SECS: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
        if let Some(secs) = BOOT_SECS.get() {
            return Some(*secs);
        }
        let secs = parse_btime(&std::fs::read_to_string("/proc/stat").ok()?)?;
        Some(*BOOT_SECS.get_or_init(|| secs))
    }

    /// `proc_pidinfo(PROC_PIDTBSDINFO)` start time as microseconds (seconds
    /// alone would miss a same-second pgid reuse).
    #[cfg(target_os = "macos")]
    pub(super) fn start_time(pid: i32) -> Option<u64> {
        // SAFETY: `proc_bsdinfo` is a plain C struct of integers and char
        // arrays; all-zero is a valid value for every field.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
        // SAFETY: `info` is an owned, zeroed, aligned `proc_bsdinfo` and
        // `size` is its exact size, so the kernel cannot overrun it. A short
        // write (dead pid, EPERM) is treated as no answer.
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                std::ptr::addr_of_mut!(info).cast::<std::os::raw::c_void>(),
                size,
            )
        };
        if written < size {
            return None;
        }
        Some(
            info.pbi_start_tvsec
                .saturating_mul(1_000_000)
                .saturating_add(info.pbi_start_tvusec),
        )
    }

    /// macOS already reports wall-clock microseconds.
    #[cfg(target_os = "macos")]
    pub(super) fn start_ms(pid: i32) -> Option<u64> {
        start_time(pid).map(|micros| micros / 1000)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub(super) const fn start_time(_pid: i32) -> Option<u64> {
        None
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub(super) const fn start_ms(_pid: i32) -> Option<u64> {
        None
    }
}

#[cfg(target_os = "linux")]
mod platform {
    /// `/proc/<pid>/cmdline`: NUL-separated argv; empty (kernel thread) is
    /// no answer.
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

    /// `sysctl(KERN_PROCARGS2)`: `[argc: i32][exec_path\0][padding]
    /// [argv...\0][env...]`; read exactly `argc` strings, else `None`.
    pub(super) fn process_argv(pid: i32) -> Option<Vec<String>> {
        let buf = procargs2(pid)?;
        parse_procargs2(&buf)
    }

    /// Fetch the raw `KERN_PROCARGS2` blob for `pid`.
    fn procargs2(pid: i32) -> Option<Vec<u8>> {
        let mut mib: [libc::c_int; 3] = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
        let mut size: libc::size_t = 0;

        // SAFETY: `mib` has 3 ints matching `namelen`; NULL `oldp` is the
        // size query, so the kernel writes only `size`.
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
        // SAFETY: `buf` is an owned `size`-byte allocation and `size` is
        // passed as `oldlenp`, so the kernel writes at most `size` bytes.
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

    /// Pure parser for the `KERN_PROCARGS2` layout.
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
mod start_tests {
    use super::start::{parse_btime, parse_stat_starttime, ticks_since_boot_to_unix_ms};
    use super::{process_start_ms, process_start_time};

    /// A real `/proc/<pid>/stat` prefix, truncated after `starttime`.
    fn stat_line(comm: &str, starttime: u64) -> String {
        let mut fields = vec!["S".to_owned()]; // field 3
        // Fields 4..=21 — eighteen more before `starttime` (field 22).
        for i in 4..=21u64 {
            fields.push(i.to_string());
        }
        fields.push(starttime.to_string());
        fields.push("999999".to_owned()); // field 23, must not be read
        format!("1234 ({comm}) {}", fields.join(" "))
    }

    #[test]
    fn reads_field_22_of_proc_stat() {
        assert_eq!(
            parse_stat_starttime(&stat_line("claude", 4_242_424)),
            Some(4_242_424),
        );
    }

    /// `comm` with spaces and parentheses parses correctly.
    #[test]
    fn a_comm_containing_spaces_and_parens_does_not_shift_the_fields() {
        assert_eq!(
            parse_stat_starttime(&stat_line("sh -c (weird) ((", 77)),
            Some(77),
            "anchoring on the LAST paren is what makes this parse",
        );
    }

    #[test]
    fn a_truncated_or_malformed_stat_line_is_no_answer() {
        assert_eq!(parse_stat_starttime(""), None);
        assert_eq!(
            parse_stat_starttime("1234 (claude) S 1 2 3"),
            None,
            "too few fields to reach starttime",
        );
        assert_eq!(parse_stat_starttime("1234 claude S"), None, "no paren");
        let non_numeric = stat_line("claude", 5).replace(" 5 ", " notanumber ");
        assert_eq!(
            parse_stat_starttime(&non_numeric),
            None,
            "a field that is not a number is no answer, never a guess",
        );
    }

    #[test]
    fn btime_is_read_from_its_own_line() {
        let proc_stat = "cpu  1 2 3\nintr 5\nbtime 1700000000\nprocesses 9\n";
        assert_eq!(parse_btime(proc_stat), Some(1_700_000_000));
        assert_eq!(parse_btime("cpu 1\n"), None, "no btime line");
        assert_eq!(parse_btime("btime soon\n"), None, "non-numeric");
    }

    #[test]
    fn ticks_convert_to_unix_ms_and_refuse_a_zero_rate() {
        // 250 ticks at 100 Hz is 2.5 s after a boot at t = 1000 s.
        assert_eq!(ticks_since_boot_to_unix_ms(250, 1000, 100), Some(1_002_500));
        assert_eq!(ticks_since_boot_to_unix_ms(250, 1000, 0), None);
        assert_eq!(ticks_since_boot_to_unix_ms(u64::MAX, 1, 100), None);
    }

    #[test]
    fn impossible_pids_are_no_answer() {
        for pid in [0, -1, i32::MAX] {
            assert_eq!(process_start_time(pid), None);
            assert_eq!(process_start_ms(pid), None);
        }
    }

    /// This process reads its own start time, identically twice.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn process_start_ms_of_self_is_stable_and_nonzero() {
        let pid = i32::try_from(std::process::id()).expect("pid fits i32");
        let raw = process_start_time(pid).expect("our own start time is queryable");
        assert!(raw > 0, "a real start time, not a placeholder");
        assert_eq!(process_start_time(pid), Some(raw), "stable across calls");

        let first = process_start_ms(pid).expect("our own start ms is queryable");
        assert_eq!(process_start_ms(pid), Some(first), "stable across calls");
        let now_ms = u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_millis(),
        )
        .expect("ms fits u64");
        assert!(first > 1_577_836_800_000, "a Unix-ms value, got {first}");
        assert!(
            first <= now_ms + 1_000,
            "{first} is in the future of {now_ms}"
        );
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::{argv0_name, foreground_pgid, process_argv};

    #[test]
    fn argv0_name_is_the_basename_without_a_login_dash() {
        let argv = |args: &[&str]| args.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            argv0_name(&argv(&["/bin/sleep", "30"])),
            Some("sleep".to_owned())
        );
        assert_eq!(argv0_name(&argv(&["-zsh"])), Some("zsh".to_owned()));
        assert_eq!(
            argv0_name(&argv(&["vim", "secret.txt"])),
            Some("vim".to_owned())
        );
        assert_eq!(argv0_name(&[]), None);
        assert_eq!(argv0_name(&argv(&["/usr/bin/"])), None);
    }

    /// A non-tty fd has no foreground group: `None`, not an error.
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
