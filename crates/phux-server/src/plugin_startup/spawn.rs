//! Spawn one sidecar: its own process group, a private umask, a scrubbed
//! environment, and stdio that never touches the server's pipes.

use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use phux_plugin::startup_log;
use tokio::sync::Notify;

use super::exit_watch::{self, ExitWatch};
use super::plan::StartupSpec;
use super::reap::{self, write_pidfile};

/// Pane and hook identity a server inherits when it was itself started
/// inside a pane or from a hook. Sidecars must not look like that pane.
/// `PHUX_SOCKET` is set explicitly afterwards. `PHUX_TERMINAL_ID` is what a
/// pane child actually receives; `PHUX_PANE` and `PHUX_SESSION` are the
/// names a program might still be carrying.
const SCRUB_ENV: &[&str] = &[
    "PHUX_TERMINAL_ID",
    "PHUX_PANE",
    "PHUX_SESSION",
    "PHUX_WINDOW",
    "PHUX_EVENT",
    "PHUX_EXIT_CODE",
    "PHUX_CLIENT_ID",
    "PHUX_PLUGIN_EVENT_ID",
    "PHUX_AGENT_KIND",
    "PHUX_AGENT_NAME",
    "PHUX_FROM",
    "PHUX_TO",
    "PHUX_BRANCH",
    "PHUX_REPO",
    "PHUX_WORKTREE",
    "PHUX_FOCUS",
];

/// A live sidecar. Drop is the last resort: `SIGKILL` the group only when
/// the direct child has not been reaped. `killpg` on a pgid we already
/// waited would hit whatever process reused that group. `kill_on_drop`
/// still kills the direct child if the task is aborted before the wait.
pub(super) struct GroupChild {
    child: tokio::process::Child,
    pgid: i32,
    pub(super) pidfile: PathBuf,
    exit: ChildExit,
    watch: Option<ExitWatch>,
}

/// Distinguishes "still running" from "exited with a code" and "killed by a signal".
enum ChildExit {
    Running,
    /// `None` when the process was killed by a signal.
    Finished(Option<i32>),
}

impl GroupChild {
    /// `true` once the direct child has been waited. Polls every call so a
    /// stop of several children does not stop at the first one still running.
    pub(super) fn poll_reaped(&mut self) -> bool {
        if matches!(self.exit, ChildExit::Finished(_)) {
            return true;
        }
        match self.child.try_wait() {
            Ok(Some(status)) => {
                self.exit = ChildExit::Finished(status.code());
                true
            }
            Err(_) => {
                self.exit = ChildExit::Finished(None);
                true
            }
            Ok(None) => false,
        }
    }

    /// The exit code after [`Self::poll_reaped`] has succeeded. `None` when
    /// the process is still running or was killed by a signal.
    pub(super) const fn exit_code(&self) -> Option<i32> {
        match self.exit {
            ChildExit::Finished(code) => code,
            ChildExit::Running => None,
        }
    }

    pub(super) fn signal(&self, signal: i32) {
        reap::signal_group(self.pgid, signal);
    }

    /// `false` when exit notification could not be armed. The supervisor
    /// polls those children on a one-second fallback instead of a fixed tick.
    pub(super) const fn has_exit_watch(&self) -> bool {
        self.watch.is_some()
    }
}

impl Drop for GroupChild {
    fn drop(&mut self) {
        // `poll_reaped` records a child that already exited. Signaling its
        // process group after that races a reused pgid.
        if matches!(self.exit, ChildExit::Finished(_)) || self.poll_reaped() {
            let _ = std::fs::remove_file(&self.pidfile);
            return;
        }
        self.signal(libc::SIGKILL);
    }
}

/// Create `path` (and missing parents) mode `0700`.
pub(super) fn create_dir_private(path: &Path) -> io::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(startup_log::STATE_DIR_MODE)
        .create(path)
}

/// Fork `spec` as a process-group leader.
pub(super) fn spawn_child(
    spec: &StartupSpec,
    socket: &Path,
    state_dir: &Path,
    exited: &Arc<Notify>,
) -> io::Result<GroupChild> {
    let Some(program) = spec.command.first() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "startup command is empty",
        ));
    };
    let log_path = startup_log::log_path(state_dir, &spec.plugin_id, &spec.startup_id);
    let pidfile = startup_log::pid_path(state_dir, &spec.plugin_id, &spec.startup_id);
    let plugin_state = startup_log::state_dir(state_dir, &spec.plugin_id, &spec.startup_id);
    if let Some(parent) = log_path.parent() {
        create_dir_private(parent)?;
    }
    create_dir_private(&plugin_state)?;
    let log = open_append(&log_path)?;
    let null = File::open("/dev/null")?;
    let log_fd = log.as_raw_fd();
    let null_fd = null.as_raw_fd();

    let mut cmd = tokio::process::Command::new(program);
    cmd.args(spec.command.iter().skip(1))
        .current_dir(&spec.plugin_root)
        .kill_on_drop(true)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for key in SCRUB_ENV {
        cmd.env_remove(key);
    }
    for key in crate::upgrade::HANDOFF_ENV_VARS {
        cmd.env_remove(key);
    }
    cmd.env("PHUX_SOCKET", socket)
        .env("PHUX_PLUGIN_ID", &spec.plugin_id)
        .env("PHUX_PLUGIN_NAME", &spec.plugin_name)
        .env("PHUX_PLUGIN_VERSION", &spec.plugin_version)
        .env("PHUX_PLUGIN_ROOT", &spec.plugin_root)
        .env("PHUX_PLUGIN_STATE", &plugin_state);
    // Captured before `fork`. The child compares `getppid` to this value,
    // which is correct when the server is pid 1 or a subreaper reparents it.
    // SAFETY: `getpid` reads this process's pid.
    let parent_pid = unsafe { libc::getpid() };
    // SAFETY: `pre_exec` runs in the forked child before `exec`, on the
    // thread that called `spawn`. The body only uses async-signal-safe
    // calls (`umask`, `prctl`, `getppid`, `raise`, `dup2`, `close_range`,
    // `fcntl`, `getdtablesize`). `log` and `null` stay open in the parent
    // until `spawn` returns, so the raw fds are still valid in the child.
    // See [`child_prelude`] for why `PR_SET_PDEATHSIG` is only safe here.
    unsafe {
        cmd.pre_exec(move || child_prelude(parent_pid, null_fd, log_fd));
    }
    let child = cmd.spawn()?;
    drop(log);
    drop(null);
    let Some(raw_pid) = child.id() else {
        return Err(io::Error::other("startup child has no pid"));
    };
    let Ok(pid) = i32::try_from(raw_pid) else {
        return Err(io::Error::other("startup pid does not fit i32"));
    };
    let group = process_group(pid);
    if let Err(err) = write_pidfile(&pidfile, pid, group) {
        tracing::warn!(
            pid,
            error = %err,
            "startup: could not write the pidfile; a later start cannot reap this sidecar"
        );
    }
    Ok(GroupChild {
        child,
        pgid: group,
        pidfile,
        exit: ChildExit::Running,
        watch: exit_watch::watch_exit(pid, exited),
    })
}

fn process_group(pid: i32) -> i32 {
    // SAFETY: `pid` is a child this process just spawned. `getpgid` reads
    // its group; a failure falls back to the pid, which `process_group(0)`
    // made the leader.
    let group = unsafe { libc::getpgid(pid) };
    if group > 1 { group } else { pid }
}

fn open_append(path: &Path) -> io::Result<File> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(startup_log::LOG_FILE_MODE)
        .open(path)
}

/// Child-side setup. Runs after `fork` and before `exec`.
///
/// `PR_SET_PDEATHSIG` is delivered when the parent *thread* exits, not when
/// `execve` replaces the process image. That is safe only because sidecars
/// are spawned from the server's long-lived `LocalSet` thread
/// (`Command::spawn` forks on the caller, and this task is `spawn_local`
/// there) and are stopped before upgrade calls `execve`. Do not move this
/// spawn onto a short-lived thread or a worker pool: the sidecar would be
/// signaled when that thread ended, and an upgrade that skipped the stop
/// would leave it running with no supervisor.
fn child_prelude(parent_pid: i32, null_fd: i32, log_fd: i32) -> io::Result<()> {
    // SAFETY: this runs in the single-threaded child immediately after
    // fork. Every call below is async-signal-safe.
    unsafe {
        libc::umask(0o077);
        arm_parent_death_signal(parent_pid);
        if libc::dup2(null_fd, 0) < 0 || libc::dup2(log_fd, 1) < 0 || libc::dup2(log_fd, 2) < 0 {
            return Err(io::Error::last_os_error());
        }
        mark_cloexec_above_stderr();
    }
    Ok(())
}

/// Linux only. macOS has no `PR_SET_PDEATHSIG`; orphan reap covers it.
///
/// # Safety
///
/// Caller is `child_prelude`, in the forked child. `parent_pid` is the
/// pid captured in the parent before `fork`.
#[cfg(target_os = "linux")]
unsafe fn arm_parent_death_signal(parent_pid: i32) {
    // SAFETY: caller is `child_prelude`, in the forked child.
    unsafe {
        libc::prctl(
            libc::PR_SET_PDEATHSIG,
            libc::SIGTERM as libc::c_ulong,
            0,
            0,
            0,
        );
        // The parent can die between fork and prctl. `getppid() == 1` is
        // the wrong check: a pid-1 server is still our parent, and a
        // subreaper is not pid 1. Compare against the pid captured before
        // the fork.
        if libc::getppid() != parent_pid {
            libc::raise(libc::SIGTERM);
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn arm_parent_death_signal(_parent_pid: i32) {}

/// Mark every descriptor above stderr `FD_CLOEXEC` so a successful `exec`
/// drops them. Do not close them: std's exec-error pipe lives above stderr,
/// and closing it makes `spawn` return `Ok` when the program is missing.
/// The child then dies with `SIGABRT` and a typo becomes a crash loop.
fn mark_cloexec_above_stderr() {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: `close_range` is one syscall and async-signal-safe.
        // `CLOSE_RANGE_CLOEXEC` sets the flag; it does not close. This runs
        // in the child, so it does not flip the server's own descriptor
        // flags (that would race the upgrade handoff; see
        // `daemon::withhold_inherited_descriptors`). A non-zero return is
        // `ENOSYS` or `EINVAL` on kernels before 5.9, and the loop below
        // covers that.
        let flags = libc::CLOSE_RANGE_CLOEXEC.cast_signed();
        if unsafe { libc::close_range(3, u32::MAX, flags) } == 0 {
            return;
        }
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    mark_cloexec_loop();
}

/// `FD_CLOEXEC` from 3 up to `getdtablesize()`. No allocation. Async-signal-safe.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn mark_cloexec_loop() {
    // SAFETY: `getdtablesize` and `fcntl` are async-signal-safe. Missing
    // descriptors return `EBADF` and are skipped.
    let limit = unsafe { libc::getdtablesize() };
    if limit <= 3 {
        return;
    }
    for fd in 3..limit {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags < 0 {
            continue;
        }
        unsafe {
            libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::Notify;

    use super::spawn_child;
    use crate::plugin_startup::StartupSpec;

    #[test]
    fn missing_program_is_a_spawn_error() {
        let dir = tempfile::tempdir().unwrap();
        let spec = StartupSpec {
            plugin_id: "plug".to_owned(),
            plugin_name: "Plug".to_owned(),
            plugin_version: "0.1.0".to_owned(),
            startup_id: "web".to_owned(),
            command: vec!["/no/such/phux-startup-program".to_owned()],
            plugin_root: dir.path().to_path_buf(),
            manifest_hash: 1,
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        local.block_on(&rt, async {
            let notify = Arc::new(Notify::new());
            match spawn_child(
                &spec,
                std::path::Path::new("/tmp/phux-missing.sock"),
                dir.path(),
                &notify,
            ) {
                Err(err) => assert_eq!(err.kind(), std::io::ErrorKind::NotFound, "{err}"),
                Ok(_) => panic!("a missing program must not look like a live child"),
            }
        });
    }
}
