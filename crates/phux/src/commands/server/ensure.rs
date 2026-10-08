//! One-shot startup watchdog and ownership of its temporary service commands.

use std::cell::RefCell;
use std::io::{self, Read as _};
use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use rustix::process::{Pid, Signal, WaitId, WaitIdOptions};

/// Overall ceiling for `phux server --ensure`.
const TIMEOUT: Duration = Duration::from_secs(10);

/// Test-only override for `TIMEOUT`, in whole seconds.
///
/// Lets the deadline tests run at 1s. A missing, unparseable, or zero value
/// falls back to `TIMEOUT`: a harness typo must not uncap the wait.
pub const ENSURE_TIMEOUT_ENV: &str = "PHUX_ENSURE_TIMEOUT_SECS";

fn timeout() -> Duration {
    timeout_from(std::env::var(ENSURE_TIMEOUT_ENV).ok().as_deref())
}

/// The parsing half, testable without touching the process environment.
fn timeout_from(raw: Option<&str>) -> Duration {
    raw.and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .map_or(TIMEOUT, Duration::from_secs)
}

const REAP_TIMEOUT: Duration = Duration::from_secs(1);
const POLL: Duration = Duration::from_millis(25);

thread_local! {
    // Only the ensure worker opts in. Other service/attach callers retain their
    // existing lifecycle, and the daemon is never registered as a helper.
    static HELPERS: RefCell<Option<Arc<Mutex<Helpers>>>> = const { RefCell::new(None) };
}

#[derive(Default)]
struct Helpers {
    cancelled: bool,
    child: Option<Child>,
}

fn lock(helpers: &Mutex<Helpers>) -> MutexGuard<'_, Helpers> {
    helpers
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn cancelled() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "coordinator startup cancelled")
}

impl Helpers {
    fn check_active(&self) -> io::Result<()> {
        if self.cancelled {
            return Err(cancelled());
        }
        Ok(())
    }

    /// The leader stays unreaped until after group cleanup, so its PID cannot
    /// be recycled into an unrelated process group between observation and kill.
    fn finish(&mut self) -> io::Result<std::process::ExitStatus> {
        let mut child = self.child.take().ok_or_else(cancelled)?;
        if let Some(pid) = child_pid(&child) {
            let _ = rustix::process::kill_process_group(pid, Signal::KILL);
        }
        // Also target the owned handle if the executable changed its group.
        let _ = child.kill();
        reap_helper(&mut child)
    }

    fn cancel(&mut self) {
        self.cancelled = true;
        if self.child.is_some()
            && let Err(err) = self.finish()
        {
            eprintln!("phux server --ensure: {err}");
        }
    }
}

fn child_pid(child: &Child) -> Option<Pid> {
    i32::try_from(child.id()).ok().and_then(Pid::from_raw)
}

/// SIGKILL can remain pending in uninterruptible kernel I/O. Bound cleanup as
/// well as startup, and report the unreaped PID rather than hanging the GUI.
fn reap_helper(child: &mut Child) -> io::Result<std::process::ExitStatus> {
    let deadline = std::time::Instant::now() + REAP_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if std::time::Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "temporary service helper {} did not reap within {REAP_TIMEOUT:?} after SIGKILL",
                    child.id()
                ),
            ));
        }
        std::thread::sleep(POLL);
    }
}

/// Installed before starting work, and dropped on success, error, or unwind.
struct Cleanup(Arc<Mutex<Helpers>>);

impl Drop for Cleanup {
    fn drop(&mut self) {
        lock(&self.0).cancel();
    }
}

pub(super) fn with_deadline(socket_path: PathBuf) -> io::Result<super::EnsureDisposition> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(watch(socket_path))
}

async fn watch(socket_path: PathBuf) -> io::Result<super::EnsureDisposition> {
    use tokio::signal::unix::{SignalKind, signal};
    // Fail closed if cancellation cannot be observed. No helper exists yet.
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let cleanup = Cleanup(Arc::default());
    let helpers = Arc::clone(&cleanup.0);
    let (sender, receiver) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("coordinator-ensure".to_owned())
        .spawn(move || {
            HELPERS.set(Some(helpers));
            let result = {
                let _log_guard = crate::init_noninteractive_tracing();
                super::ensure_accepting(&socket_path)
            };
            HELPERS.set(None);
            let _ = sender.send(result);
        })?;
    let deadline = timeout();
    tokio::select! {
        result = receiver => result.map_err(io::Error::other)?,
        () = tokio::time::sleep(deadline) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("coordinator startup did not complete within {deadline:?}; see {}",
                phux_server::telemetry::server_log_path().display()),
        )),
        _ = terminate.recv() => Err(cancelled()),
        _ = interrupt.recv() => Err(cancelled()),
    }
}

/// Serialize cancellation with daemon creation without transferring ownership
/// of the daemon to cleanup. Once spawned, it belongs to its own lifecycle.
///
/// The daemon inherits nothing but the stdio set on `command`: a descriptor
/// this process inherited would otherwise stay open for the server's lifetime,
/// and a caller reading this process's output to EOF would wait on the server
/// rather than on us (phux-5wxp.5).
pub(super) fn spawn_daemon(command: &mut Command) -> io::Result<Child> {
    phux_server::daemon::withhold_inherited_descriptors();
    HELPERS.with_borrow(|helpers| {
        let Some(helpers) = helpers else {
            return command.spawn();
        };
        let state = lock(helpers);
        state.check_active()?;
        let child = command.spawn();
        drop(state);
        child
    })
}

/// Service commands have no useful stdout here. Capture stderr for `run_tool`'s
/// diagnostic while the watchdog retains the Child needed to kill and reap.
pub(in crate::commands) fn service_output(command: &mut Command) -> io::Result<Output> {
    HELPERS.with_borrow(|helpers| match helpers {
        Some(helpers) => managed_output(command, helpers),
        None => command.output(),
    })
}

fn managed_output(command: &mut Command, helpers: &Mutex<Helpers>) -> io::Result<Output> {
    let mut stderr_pipe = spawn_helper(command, helpers)?;
    let mut stderr = Vec::new();
    stderr_pipe.read_to_end(&mut stderr)?;
    let status = wait_for_helper(helpers)?;
    Ok(Output {
        status,
        stdout: Vec::new(),
        stderr,
    })
}

fn spawn_helper(
    command: &mut Command,
    helpers: &Mutex<Helpers>,
) -> io::Result<std::process::ChildStderr> {
    let mut state = lock(helpers);
    state.check_active()?;
    let mut child = command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    let stderr = child.stderr.take();
    state.child = Some(child);
    drop(state);
    stderr.ok_or_else(|| io::Error::other("service helper stderr missing"))
}

fn wait_for_helper(helpers: &Mutex<Helpers>) -> io::Result<std::process::ExitStatus> {
    loop {
        {
            let mut state = lock(helpers);
            state.check_active()?;
            let pid = state
                .child
                .as_ref()
                .and_then(child_pid)
                .ok_or_else(cancelled)?;
            if rustix::process::waitid(
                WaitId::Pid(pid),
                WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
            )?
            .is_some()
            {
                return state.finish();
            }
        }
        std::thread::sleep(POLL);
    }
}

#[cfg(test)]
mod timeout_tests {
    use super::{Duration, TIMEOUT, timeout_from};

    /// The shipped ceiling, a whitespace-tolerant override, and fail-closed
    /// fallback for junk and zero.
    #[test]
    fn timeout_override_parses_and_falls_back_to_the_shipped_ceiling() {
        assert_eq!(TIMEOUT, Duration::from_secs(10));
        assert_eq!(timeout_from(None), TIMEOUT);
        assert_eq!(timeout_from(Some("1")), Duration::from_secs(1));
        assert_eq!(timeout_from(Some("  3\n")), Duration::from_secs(3));
        for raw in ["", "0", "-1", "abc", "1.5", "99999999999999999999"] {
            assert_eq!(timeout_from(Some(raw)), TIMEOUT, "input {raw:?}");
        }
    }
}
