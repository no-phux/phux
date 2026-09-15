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

/// The display name of a process from its argv: argv0's basename with a
/// login-shell dash stripped (`-zsh` and `/bin/zsh` both read `zsh`).
///
/// This is the only piece of a queried argv that may leave the server
/// (the same boundary as `phux.pane-occupant/v1`); the argv tail can carry
/// secrets and never does. `None` for an empty argv or an empty name.
#[must_use]
pub(crate) fn argv0_name(argv: &[String]) -> Option<String> {
    let first = argv.first()?;
    let base = first.rsplit('/').next().unwrap_or(first);
    let name = base.trim_start_matches('-');
    (!name.is_empty()).then(|| name.to_owned())
}

/// The start time of `pid`'s process in a platform-specific unit that is
/// only ever compared for equality against another reading of the same
/// platform: clock ticks since boot on Linux, microseconds since the Unix
/// epoch on macOS.
///
/// A process group id or pid is a small integer the kernel recycles; pairing
/// it with this value is what makes a recycled number distinguishable from
/// the process that held it before (ADR-0046 occupant identity, and the
/// `process` facet's pid generation).
///
/// Best-effort like its siblings: a dead pid, a permission error or an
/// unsupported platform all yield `None`, never an error a caller has to
/// handle. `None` is "no answer", never "no start time".
#[must_use]
pub(crate) fn process_start_time(pid: i32) -> Option<u64> {
    if pid <= 0 {
        return None;
    }
    start::start_time(pid)
}

/// The start time of `pid`'s process in Unix milliseconds.
///
/// macOS reports the start as a wall-clock `timeval`, so this is exact to the
/// millisecond. Linux reports clock ticks since boot; the conversion adds the
/// boot time from `/proc/stat` (`btime`, whole seconds), read once per server
/// process, so the value carries up to one second of absolute error but is
/// identical for one process on every query. `None` whenever any of the
/// inputs is unavailable.
#[must_use]
pub(crate) fn process_start_ms(pid: i32) -> Option<u64> {
    if pid <= 0 {
        return None;
    }
    start::start_ms(pid)
}

/// Start-time queries. Moved here from `agent_detect::identify`, whose
/// placement note asked for exactly this, so the `PROC_PIDTBSDINFO` FFI block
/// exists once.
///
/// No new dependency: macOS reads it through the `libc` this crate already
/// declares under a `cfg(target_os = "macos")` gate for
/// [`crate::cwd_query`]'s `proc_pidinfo`; Linux reads `/proc` with plain
/// `std` and asks `sysconf(_SC_CLK_TCK)` through `nix`.
mod start {
    /// Extract field 22 (`starttime`) from the contents of `/proc/<pid>/stat`.
    ///
    /// Compiled on every platform so the parser — the only part of the Linux
    /// path with any logic in it — is unit-tested wherever the suite runs,
    /// rather than only on a Linux CI leg.
    ///
    /// Field 2 (`comm`) is the executable name in parentheses, and it may
    /// contain BOTH spaces and parentheses (`(sh -c (weird))`), so the fields
    /// cannot simply be whitespace-split. Anchoring on the LAST `)` is the
    /// documented way to parse this file: everything after it is fields 3
    /// onward, whitespace-separated, so `starttime` is the 20th of them.
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

    /// Convert a start time in clock ticks since boot into Unix ms, given the
    /// boot time in Unix seconds and the tick rate. `None` on a zero tick
    /// rate or overflow — never a guessed value.
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

    /// `sysconf(_SC_CLK_TCK)`: the unit of `/proc/<pid>/stat` field 22.
    /// Compiled on every Unix (the call exists everywhere) so the API is
    /// type-checked off a Linux CI leg; only Linux uses it.
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

    /// The boot time, read from `/proc/stat` once per server process.
    ///
    /// The kernel recomputes `btime` from the wall clock, so a clock step
    /// moves it. Reading it once keeps one process's `start_ms` identical
    /// on every query for the life of this server, which is what a pid
    /// generation must be. A failed read is not cached; the next call
    /// retries.
    #[cfg(target_os = "linux")]
    fn boot_secs() -> Option<u64> {
        static BOOT_SECS: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
        if let Some(secs) = BOOT_SECS.get() {
            return Some(*secs);
        }
        let secs = parse_btime(&std::fs::read_to_string("/proc/stat").ok()?)?;
        Some(*BOOT_SECS.get_or_init(|| secs))
    }

    /// `proc_pidinfo(PROC_PIDTBSDINFO)` fills a `proc_bsdinfo`, whose
    /// `pbi_start_tvsec` / `pbi_start_tvusec` are the process's start time.
    /// The struct layout comes from `libc`, never a hand-written mirror.
    ///
    /// The two halves are folded into one `u64` of microseconds. Seconds
    /// alone would be too coarse for exactly the case this exists for: a
    /// pgid recycled within the same second is precisely the narrow window
    /// that makes reuse possible at all.
    #[cfg(target_os = "macos")]
    pub(super) fn start_time(pid: i32) -> Option<u64> {
        // SAFETY: `proc_bsdinfo` is a plain C struct of integers and char
        // arrays; all-zero is a valid value for every field.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
        // SAFETY: `proc_pidinfo` fills at most `size` bytes into `&mut
        // info`, which is a zeroed, correctly-aligned, owned `proc_bsdinfo`
        // of exactly that size; `size` is that struct's own size, so the
        // kernel cannot overrun it. `pid` is validated positive by the
        // caller. The call only reads kernel state for `pid` and writes into
        // our buffer. A return value short of the full struct size
        // (including 0 on a dead pid or EPERM) means the struct was not
        // fully populated and is treated as "no answer", exactly as
        // `crate::cwd_query` treats its sibling call.
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

    /// THE reason this is not a whitespace split. `comm` is attacker- (or
    /// merely user-) controlled and carries both spaces and parentheses;
    /// a naive parser reads a field from the middle of the process name.
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

    /// The live half: this test process can read its own start time, and
    /// reads the SAME value twice. A query that varied per call would make
    /// every identity recheck a fabricated restart.
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
        // 2020-01-01, and no later than now (plus Linux's one-second
        // `btime` granularity).
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
