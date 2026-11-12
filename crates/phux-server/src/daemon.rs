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

/// Append this binary's directory to `PATH` when no `phux` resolves on it.
///
/// Panes inherit the server's `PATH`, and agents in them run `phux` (the skill
/// says to). The installer's `~/.local/bin` is often missing from a server an
/// app or launchd started. Call before the server starts any thread.
pub fn expose_own_cli() {
    let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf))
    else {
        return;
    };
    let path = std::env::var_os("PATH").unwrap_or_default();
    if let Some(joined) = path_with_cli(&path, &dir) {
        // SAFETY: the caller runs this at startup, before any other thread
        // exists to read the environment concurrently.
        unsafe { std::env::set_var("PATH", joined) };
    }
}

/// `path` with `dir` appended, or `None` when a `phux` already resolves.
fn path_with_cli(path: &std::ffi::OsStr, dir: &std::path::Path) -> Option<std::ffi::OsString> {
    if std::env::split_paths(path).any(|d| d.join("phux").is_file()) {
        return None;
    }
    std::env::join_paths(std::env::split_paths(path).chain([dir.to_path_buf()])).ok()
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

/// Spawn `command` as a daemon that disclaims responsibility for itself.
///
/// macOS then asks the server, not the app that started it, for privacy grants
/// and keeps them on the server. Returns its pid; nothing owns or reaps it.
/// `Command` cannot set that spawn attribute, so this is `posix_spawn` with
/// the same program, arguments and environment changes, and what `Command`
/// would set up: default signals and mask, and only stdio inherited. Stdin and
/// stdout are `/dev/null`; stderr is `log`, else `/dev/null`. The command's
/// own stdio and working directory are not read.
#[cfg(target_os = "macos")]
pub fn spawn_disclaimed(
    command: &std::process::Command,
    log: Option<&std::fs::File>,
) -> std::io::Result<libc::pid_t> {
    use std::ffi::CString;
    use std::io;
    use std::os::fd::AsRawFd as _;
    use std::os::unix::ffi::OsStrExt as _;

    unsafe extern "C" {
        // libSystem, macOS 10.14+; Terminal-style apps use it for their children.
        fn responsibility_spawnattrs_setdisclaim(
            attr: *mut libc::posix_spawnattr_t,
            disclaim: libc::c_int,
        ) -> libc::c_int;
    }
    fn c(bytes: &[u8]) -> io::Result<CString> {
        CString::new(bytes).map_err(io::Error::other)
    }
    fn check(code: libc::c_int) -> io::Result<()> {
        if code == 0 {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(code))
        }
    }

    let program = c(command.get_program().as_bytes())?;
    let words = c_strings(std::iter::once(command.get_program()).chain(command.get_args()))?;
    let pairs = spawn_env(std::env::vars_os(), command.get_envs());
    let pairs = c_strings(pairs.iter().map(std::ffi::OsString::as_os_str))?;
    let argv = null_terminated(&words);
    let envp = null_terminated(&pairs);
    let null = c(b"/dev/null")?;
    let flags = libc::POSIX_SPAWN_CLOEXEC_DEFAULT
        | libc::POSIX_SPAWN_SETSIGMASK
        | libc::POSIX_SPAWN_SETSIGDEF;
    let flags = libc::c_short::try_from(flags).map_err(io::Error::other)?;

    let mut pid: libc::pid_t = 0;
    // SAFETY: every pointer passed outlives the calls, and the attribute and
    // file-action objects are initialised before use and destroyed after.
    unsafe {
        let mut actions: libc::posix_spawn_file_actions_t = std::ptr::null_mut();
        check(libc::posix_spawn_file_actions_init(&raw mut actions))?;
        let mut attr: libc::posix_spawnattr_t = std::ptr::null_mut();
        if let Err(err) = check(libc::posix_spawnattr_init(&raw mut attr)) {
            libc::posix_spawn_file_actions_destroy(&raw mut actions);
            return Err(err);
        }
        let result = (|| {
            let a = &raw mut actions;
            check(libc::posix_spawn_file_actions_addopen(
                a,
                0,
                null.as_ptr(),
                libc::O_RDONLY,
                0,
            ))?;
            check(libc::posix_spawn_file_actions_addopen(
                a,
                1,
                null.as_ptr(),
                libc::O_WRONLY,
                0,
            ))?;
            check(log.map_or_else(
                || libc::posix_spawn_file_actions_addopen(a, 2, null.as_ptr(), libc::O_WRONLY, 0),
                |file| libc::posix_spawn_file_actions_adddup2(a, file.as_raw_fd(), 2),
            ))?;
            let mut none: libc::sigset_t = 0;
            libc::sigemptyset(&raw mut none);
            let mut pipe: libc::sigset_t = 0;
            libc::sigemptyset(&raw mut pipe);
            libc::sigaddset(&raw mut pipe, libc::SIGPIPE);
            check(libc::posix_spawnattr_setsigmask(
                &raw mut attr,
                &raw const none,
            ))?;
            check(libc::posix_spawnattr_setsigdefault(
                &raw mut attr,
                &raw const pipe,
            ))?;
            check(libc::posix_spawnattr_setflags(&raw mut attr, flags))?;
            check(responsibility_spawnattrs_setdisclaim(&raw mut attr, 1))?;
            check(libc::posix_spawnp(
                &raw mut pid,
                program.as_ptr(),
                &raw const actions,
                &raw const attr,
                argv.as_ptr(),
                envp.as_ptr(),
            ))
        })();
        libc::posix_spawnattr_destroy(&raw mut attr);
        libc::posix_spawn_file_actions_destroy(&raw mut actions);
        result?;
    }
    Ok(pid)
}

#[cfg(target_os = "macos")]
fn c_strings<'a>(
    items: impl Iterator<Item = &'a std::ffi::OsStr>,
) -> std::io::Result<Vec<std::ffi::CString>> {
    use std::os::unix::ffi::OsStrExt as _;
    items
        .map(|item| std::ffi::CString::new(item.as_bytes()).map_err(std::io::Error::other))
        .collect()
}

/// The `char *const[]` `posix_spawn` takes, borrowing `strings`.
#[cfg(target_os = "macos")]
fn null_terminated(strings: &[std::ffi::CString]) -> Vec<*mut libc::c_char> {
    strings
        .iter()
        .map(|s| s.as_ptr().cast_mut())
        .chain([std::ptr::null_mut()])
        .collect()
}

/// `inherited` with `changes` applied as `Command` applies them (a value sets,
/// `None` removes), as `KEY=VALUE` pairs.
#[cfg(target_os = "macos")]
fn spawn_env<'a>(
    inherited: impl Iterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
    changes: impl Iterator<Item = (&'a std::ffi::OsStr, Option<&'a std::ffi::OsStr>)>,
) -> Vec<std::ffi::OsString> {
    let mut env: std::collections::BTreeMap<_, _> = inherited.collect();
    for (key, value) in changes {
        match value {
            Some(value) => env.insert(key.to_owned(), value.to_owned()),
            None => env.remove(key),
        };
    }
    env.into_iter()
        .map(|(mut pair, value)| {
            pair.push("=");
            pair.push(value);
            pair
        })
        .collect()
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

    #[cfg(target_os = "macos")]
    #[test]
    fn spawn_env_sets_and_removes_over_the_inherited_environment() {
        use std::ffi::{OsStr, OsString};
        let inherited =
            [("A", "1"), ("B", "2")].map(|(k, v)| (OsString::from(k), OsString::from(v)));
        let changes = [
            (OsStr::new("B"), None),
            (OsStr::new("C"), Some(OsStr::new("3"))),
            (OsStr::new("A"), Some(OsStr::new("x"))),
        ];
        assert_eq!(
            super::spawn_env(inherited.into_iter(), changes.into_iter()),
            ["A=x", "C=3"].map(OsString::from)
        );
    }

    #[test]
    fn panes_find_the_cli_only_when_path_lacks_one() {
        let own = std::path::Path::new("/opt/phux Server.app/Contents/MacOS");
        let joined = super::path_with_cli("/usr/bin:/bin".as_ref(), own).expect("appended");
        assert_eq!(joined, "/usr/bin:/bin:/opt/phux Server.app/Contents/MacOS");

        let bin = tempfile::tempdir().expect("tempdir");
        std::fs::write(bin.path().join("phux"), "").expect("stub");
        let with = std::env::join_paths([bin.path(), "/bin".as_ref()]).expect("joins");
        assert_eq!(
            super::path_with_cli(&with, own),
            None,
            "the person's own phux wins"
        );
    }
}
