//! Auto-spawning a server for a client verb, and handing an older one over.

use std::path::Path;
use std::process::Stdio;
use std::time::Instant;

use phux_config::socket::{self, SocketState};

use super::run::open_server_log;
use super::{
    AUTO_SPAWN_POLL_INTERVAL, AUTO_SPAWN_SOCKET_TIMEOUT, EnsureDisposition, SPAWN_LOCK_TIMEOUT,
    ensure,
};

/// Environment variable that gives an auto-spawned daemon an idle backstop,
/// in seconds (`1..=86_400`, as `--exit-after-idle`). Public so the
/// integration harness re-arms it by name.
pub const AUTO_SPAWN_IDLE_ENV: &str = "PHUX_AUTO_SPAWN_EXIT_AFTER_IDLE";

/// The upper bound `--exit-after-idle` accepts.
const AUTO_SPAWN_IDLE_MAX_SECS: u64 = 86_400;

/// Resolve the idle limit an auto-spawned daemon should carry, if any.
/// `None` in production: an unattended server stays up (ADR-0063); the
/// variable is an opt-in for test harnesses so auto-spawned daemons cannot
/// leak. A malformed value warns (unless `quiet`) and is ignored.
fn auto_spawn_idle_limit(quiet: bool) -> Option<u64> {
    let raw = std::env::var_os(AUTO_SPAWN_IDLE_ENV)?;
    let text = raw.to_string_lossy();
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let parsed = parse_auto_spawn_idle(trimmed);
    if parsed.is_none() && !quiet {
        warn!(
            "ignoring {AUTO_SPAWN_IDLE_ENV}={trimmed} \
             (want whole seconds, 1..={AUTO_SPAWN_IDLE_MAX_SECS}); \
             the auto-spawned server will have no idle limit"
        );
    }
    parsed
}

/// The pure half of [`auto_spawn_idle_limit`], matching the flag's range.
pub(super) fn parse_auto_spawn_idle(raw: &str) -> Option<u64> {
    raw.parse::<u64>()
        .ok()
        .filter(|secs| (1..=AUTO_SPAWN_IDLE_MAX_SECS).contains(secs))
}

/// Fork-exec the current binary as a detached `phux server --daemonize`, with
/// stderr on the canonical server log, and wait for it to accept. The child is
/// not kept: it owns its own lifecycle.
pub(crate) fn maybe_auto_spawn_server(
    socket_path: &Path,
    session: Option<&str>,
    seed_command: Option<&str>,
    quiet: bool,
    login_shell: bool,
) -> std::io::Result<()> {
    let current_exe = std::env::current_exe()?;
    let log_path = phux_server::telemetry::server_log_path();

    // The banner names the log; suppressed under `--json`.
    if !quiet {
        info!(
            "starting server at {} (auto-spawn, session={}; log: {})",
            socket_path.display(),
            session.unwrap_or("none"),
            log_path.display()
        );
    }

    // Best-effort: fall back to /dev/null if the log cannot be opened.
    let log = open_server_log(&log_path).ok();
    // Where this daemon's output starts in the shared, append-only log.
    let log_offset = log
        .as_ref()
        .and_then(|file| file.metadata().ok())
        .map(|metadata| metadata.len());

    let mut cmd = std::process::Command::new(current_exe);
    cmd.arg("server")
        .arg("--socket")
        .arg(socket_path)
        .arg("--daemonize")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        // A `switch-host` successor's hand-off marker describes its own
        // terminal; the server and every pane it spawns must not inherit it.
        .env_remove(phux_tui::attach::hosts::ALT_SCREEN_HANDOFF_ENV);
    // A server started from a non-login environment gets the service marker, so
    // its panes run login shells (ADR-0120).
    if login_shell {
        cmd.env(crate::commands::service::SERVICE_MANAGED_ENV, "1");
    }
    match session {
        Some(name) => {
            cmd.arg("--session").arg(name);
        }
        None => {
            cmd.arg("--no-seed");
        }
    }
    if let Some(seed) = seed_command {
        cmd.arg("--seed-command").arg(seed);
    }
    // Passed as a flag so a leaked server shows its bound in `ps`.
    if let Some(secs) = auto_spawn_idle_limit(quiet) {
        cmd.arg("--exit-after-idle").arg(secs.to_string());
    }
    match log {
        Some(file) => {
            cmd.stderr(file);
        }
        None => {
            cmd.stderr(Stdio::null());
        }
    }

    // `--daemonize` only calls `setsid`, so this is the server process itself.
    // It is dropped (not killed) once accepting: it owns its own lifecycle.
    let mut child = ensure::spawn_daemon(&mut cmd)?;

    wait_until_accepting(socket_path, "auto-spawned server", &log_path, || {
        // `try_wait` is a non-blocking `waitpid`: watching the daemon this way
        // cannot change its lifetime, unlike another connect.
        let status = child.try_wait().ok().flatten()?;
        Some(early_exit_error(socket_path, status, &log_path, log_offset))
    })
}

/// The error for a spawned server that exited before accepting: its exit
/// status and the tail of what it wrote to the log (its bind or config error).
fn early_exit_error(
    socket_path: &Path,
    status: std::process::ExitStatus,
    log_path: &Path,
    log_offset: Option<u64>,
) -> std::io::Error {
    let output = log_offset
        .and_then(|offset| startup_output(log_path, offset))
        .map(|tail| format!(": {tail}"))
        .unwrap_or_default();
    std::io::Error::other(format!(
        "auto-spawned server exited before accepting on {} ({status}){output} (see {})",
        socket_path.display(),
        log_path.display(),
    ))
}

/// The last few lines a spawned server appended to its log from `offset`.
fn startup_output(log_path: &Path, offset: u64) -> Option<String> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    /// Enough for the fatal line and its context, not a whole crash dump.
    const MAX_LINES: usize = 3;
    const MAX_BYTES: u64 = 16 * 1024;
    let mut file = std::fs::File::open(log_path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(offset.max(len.saturating_sub(MAX_BYTES))))
        .ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let tail = lines[lines.len().saturating_sub(MAX_LINES)..].join("; ");
    (!tail.is_empty()).then_some(tail)
}

/// Block until `socket_path` accepts (not merely exists), or the auto-spawn
/// deadline passes. `what` names what is being waited on. `exited` reports a
/// server that died before accepting, which fails the wait at once instead of
/// at the deadline.
fn wait_until_accepting(
    socket_path: &Path,
    what: &str,
    log_path: &Path,
    mut exited: impl FnMut() -> Option<std::io::Error>,
) -> std::io::Result<()> {
    let deadline = Instant::now() + AUTO_SPAWN_SOCKET_TIMEOUT;
    loop {
        if socket::probe(socket_path) == SocketState::Live {
            return Ok(());
        }
        if let Some(err) = exited() {
            // A loser of a bind race exits because another server is live.
            if socket::probe(socket_path) == SocketState::Live {
                return Ok(());
            }
            return Err(err);
        }
        if Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!(
                    "{what} did not accept on {} within {:?} (see {})",
                    socket_path.display(),
                    AUTO_SPAWN_SOCKET_TIMEOUT,
                    log_path.display(),
                ),
            ));
        }
        std::thread::sleep(AUTO_SPAWN_POLL_INTERVAL);
    }
}

/// Ensure a server is accepting on `socket_path`, starting one if not. The
/// single entry point every client verb uses: a stale socket is reaped rather
/// than trusted, and a profile-scoped lock elects one spawner among
/// concurrent invocations. A live server costs one probe and no lock.
pub(crate) fn ensure_server(
    socket_path: &Path,
    session: &str,
    seed_command: Option<&str>,
    quiet: bool,
) -> std::io::Result<EnsureDisposition> {
    ensure_server_with(socket_path, Some(session), seed_command, quiet, false)
}

/// [`ensure_server`] without a seed session (`phux new --empty`, ADR-0105).
pub(crate) fn ensure_server_unseeded(
    socket_path: &Path,
    quiet: bool,
) -> std::io::Result<EnsureDisposition> {
    ensure_server_with(socket_path, None, None, quiet, false)
}

/// [`ensure_server`] for `phux bootstrap` (`phux attach --ssh`, ADR-0120): a
/// server started here gets the login-shell marker, because ssh ran it
/// non-interactively.
pub(crate) fn ensure_server_for_bootstrap(socket_path: &Path) -> std::io::Result<()> {
    ensure_server_with(
        socket_path,
        Some(&crate::commands::attach::resolved_default_session_name()),
        crate::commands::attach::configured_spawn_on_attach().as_deref(),
        false,
        true,
    )
    .map(|_| ())
}

/// The shared body of the `ensure_server*` entry points.
fn ensure_server_with(
    socket_path: &Path,
    session: Option<&str>,
    seed_command: Option<&str>,
    quiet: bool,
    login_shell: bool,
) -> std::io::Result<EnsureDisposition> {
    if socket::probe(socket_path) == SocketState::Live {
        // A live socket may be a supervised server login started; retire a
        // leftover `--adopt` marker.
        crate::commands::service::sweep_stale_adoption_marker(socket_path);
        if !quiet {
            reconcile_version_skew(socket_path);
        }
        return Ok(EnsureDisposition::Reused);
    }

    // Serialise the spawn decision. Failing to lock is not fatal: the server's
    // own bind-time probe still rejects a duplicate.
    let guard = SpawnLock::acquire(&socket::spawn_lock_path(socket_path));

    // Re-probe under the lock: the previous holder likely spawned it.
    if socket::probe(socket_path) == SocketState::Live {
        crate::commands::service::sweep_stale_adoption_marker(socket_path);
        return Ok(EnsureDisposition::Joined);
    }

    // Nothing is accepting; remove a dead server's socket entry.
    if let Err(err) = socket::reap_stale(socket_path) {
        tracing::warn!(
            path = %socket_path.display(),
            error = %err,
            "could not remove stale socket entry",
        );
    }

    // A pending `--adopt` unit for this socket gets first refusal (ADR-0088);
    // an ordinary installed unit does not, so a stopped server stays stopped.
    if matches!(
        crate::commands::service::complete_pending_adoption(socket_path, quiet),
        crate::commands::service::Handover::Started
    ) {
        let result = wait_until_accepting(
            socket_path,
            "the supervised server",
            &phux_server::telemetry::server_log_path(),
            // The init system owns this process; there is no child to watch.
            || None,
        );
        drop(guard);
        return noting_failure(
            socket_path,
            result.map(|()| EnsureDisposition::SupervisedStarted),
        );
    }

    let result = maybe_auto_spawn_server(socket_path, session, seed_command, quiet, login_shell)
        .map(|()| EnsureDisposition::DaemonStarted);
    drop(guard);
    noting_failure(socket_path, result)
}

/// Park a failed start for the verb's later JSON `no_server` error, which
/// otherwise knows only that nothing answered (`error.auto_start_error`).
fn noting_failure(
    socket_path: &Path,
    result: std::io::Result<EnsureDisposition>,
) -> std::io::Result<EnsureDisposition> {
    if let Err(err) = &result {
        crate::commands::json_err::record_auto_start_failure(socket_path, err);
    }
    result
}

/// Whether a server running build `theirs` should be handed over to this
/// binary, build `ours`: only when it is older. A newer server on the same
/// protocol major interoperates with this client, and handing it over would
/// downgrade it. A build that is not a plain `X.Y.Z` (a checkout or `next`
/// build) cannot be ordered, so any difference keeps the original rule and
/// upgrades: that is the replaced-binary case this hand-over exists for, and
/// declining would strand an old server with no signal.
pub(super) fn server_is_older(theirs: &str, ours: &str) -> bool {
    use phux_client_runtime::release::Version;
    if theirs == ours {
        return false;
    }
    match (Version::parse(theirs), Version::parse(ours)) {
        (Some(theirs), Some(ours)) => theirs < ours,
        _ => true,
    }
}

/// Hand a server running an older build over to this one in place
/// (ADR-0032 re-exec, panes survive), for binaries replaced by a package
/// manager. A newer server is left alone. Best-effort: on refusal the old
/// server keeps running.
fn reconcile_version_skew(socket_path: &Path) {
    let ours = env!("CARGO_PKG_VERSION");
    let Some(theirs) = phux_server::health::running_version() else {
        return;
    };
    if !server_is_older(&theirs, ours) {
        if theirs != ours {
            tracing::debug!(theirs, ours, "running server is newer; leaving it in place");
        }
        return;
    }

    info!("the running server is {theirs}, this binary is {ours} — upgrading it in place");
    match crate::commands::upgrade::request_upgrade(socket_path) {
        Ok(crate::commands::upgrade::UpgradeAck::Upgrading) => {
            // Wait for the re-exec'd server so the caller's connect does not race it.
            let deadline = Instant::now() + AUTO_SPAWN_SOCKET_TIMEOUT;
            while Instant::now() < deadline {
                if socket::probe(socket_path) == SocketState::Live {
                    return;
                }
                std::thread::sleep(AUTO_SPAWN_POLL_INTERVAL);
            }
        }
        Ok(
            crate::commands::upgrade::UpgradeAck::Refused(message)
            | crate::commands::upgrade::UpgradeAck::Unexpected(message),
        ) => {
            warn!("the server declined the upgrade ({message}); continuing on {theirs}");
        }
        Err(err) => {
            warn!("could not upgrade the running server ({err}); continuing on {theirs}");
        }
    }
}

/// An advisory `flock` held for the duration of a spawn decision, scoped to
/// the profile's runtime dir. The lock file is never unlinked (a new inode
/// would not exclude holders of the old one).
pub(super) struct SpawnLock(pub(super) Option<std::fs::File>);

impl SpawnLock {
    /// Take the lock, waiting up to [`SPAWN_LOCK_TIMEOUT`]; unlocked on failure.
    pub(super) fn acquire(path: &Path) -> Self {
        let Some(parent) = path.parent() else {
            return Self(None);
        };
        if std::fs::create_dir_all(parent).is_err() {
            return Self(None);
        }
        let Ok(file) = rustix::fs::open(
            path,
            rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::WRONLY
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        ) else {
            return Self(None);
        };
        let file = std::fs::File::from(file);
        if !file.metadata().is_ok_and(|metadata| metadata.is_file()) {
            return Self(None);
        }
        let deadline = Instant::now() + SPAWN_LOCK_TIMEOUT;
        loop {
            match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => return Self(Some(file)),
                Err(rustix::io::Errno::WOULDBLOCK) if Instant::now() < deadline => {
                    std::thread::sleep(AUTO_SPAWN_POLL_INTERVAL);
                }
                Err(_) => return Self(None),
            }
        }
    }
}

impl Drop for SpawnLock {
    fn drop(&mut self) {
        if let Some(file) = self.0.take() {
            // Best-effort: closing the descriptor releases the lock anyway.
            let _ = rustix::fs::flock(&file, rustix::fs::FlockOperation::Unlock);
        }
    }
}
