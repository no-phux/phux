//! Staging and validating the replacement binary before the irreversible
//! re-exec (ADR-0032): pin a private snapshot, then probe it with
//! `--version` / `config check` and refuse a dev/release build-kind change.

use std::os::fd::RawFd;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use phux_config::instance::{BuildKind, PROBE_BUILD_KIND_ENV, build_kind};

use super::{FdFlagsGuard, UPGRADE_SNAPSHOT_DIR, UPGRADE_SOURCE_EXE, UpgradeError};
use crate::runtime::delegation;
use crate::upgrade::blob::StateBlob;

/// A private executable snapshot copied from one opened source inode, so the
/// installed path cannot be swapped between copy and validation. Re-exec
/// uses the installed path while it still matches: macOS Application
/// Firewall allowlists by path, and a deleted tempfile never can be.
pub(super) struct PinnedExecutable {
    pub(super) path: PathBuf,
    pub(super) source_path: PathBuf,
    pub(super) dir: tempfile::TempDir,
}

impl PinnedExecutable {
    pub(super) fn open(path: &Path) -> std::io::Result<Self> {
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

        let mut source = std::fs::File::open(path)?;
        let mode = source.metadata()?.permissions().mode();
        let dir = tempfile::Builder::new().prefix("phux-upgrade-").tempdir()?;
        let pinned = dir.path().join("phux");
        let mut target = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(mode)
            .open(&pinned)?;
        std::io::copy(&mut source, &mut target)?;
        target.sync_all()?;
        drop(target);
        Ok(Self {
            path: pinned,
            source_path: path.to_path_buf(),
            dir,
        })
    }

    /// Path to `execve`: the installed source, so the running image is a
    /// path operators can allowlist. The snapshot stays for validation; if
    /// the source no longer matches, refuse rather than exec the tempfile.
    pub(super) fn exec_path(&self) -> std::io::Result<&Path> {
        if files_have_same_bytes(&self.source_path, &self.path)? {
            Ok(self.source_path.as_path())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "installed binary changed after validation; aborting upgrade so the old image keeps serving",
            ))
        }
    }
}

/// Remove the private executable snapshot after a successful re-exec. Unix
/// keeps the mapped image alive after unlink. `dir` is the snapshot directory
/// the upgrading image handed down (see [`InheritedUpgradeEnv`]).
#[allow(
    clippy::redundant_pub_crate,
    reason = "reachable only via the deliberate super re-export in upgrade/mod.rs"
)]
pub(crate) fn cleanup_executable_snapshot(dir: Option<&Path>) {
    let Some(path) = dir else {
        return;
    };
    let is_ours = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("phux-upgrade-"))
        && path.parent() == Some(std::env::temp_dir().as_path());
    if is_ours {
        let _ = std::fs::remove_dir_all(path);
    }
}

/// Pin and validate the replacement image before any descriptor flag changes.
/// A broken replacement binary must leave the old process's descriptor policy
/// untouched.
pub(super) fn pin_validated_executable(
    inherited_source: Option<&Path>,
) -> Result<PinnedExecutable, UpgradeError> {
    // An image upgraded from a pre-#1111 server inherited its
    // `current_exe` verbatim, ` (deleted)` suffix included.
    let source_exe = inherited_source
        .map_or_else(phux_config::instance::running_executable, |path| {
            Ok(phux_config::instance::without_deleted_suffix(path))
        })?;
    let executable = PinnedExecutable::open(&source_exe)?;
    validate_binary(&executable.path)?;
    Ok(executable)
}

/// The upgrade handoff a resumed image inherits through its environment.
/// The `PHUX_UPGRADE_*` variables are consumed only by a `--resume` start
/// and then removed, so they never leak into panes and steer an unrelated
/// server started from one onto the wrong binary.
#[derive(Debug)]
#[allow(
    clippy::redundant_pub_crate,
    reason = "reachable only via the deliberate super re-export in upgrade/mod.rs"
)]
pub(crate) struct InheritedUpgradeEnv {
    /// Installed executable the next upgrade pins instead of `current_exe`.
    pub(crate) source_exe: Option<PathBuf>,
    /// The previous image's private executable snapshot directory.
    pub(crate) snapshot_dir: Option<PathBuf>,
}

impl InheritedUpgradeEnv {
    /// Nothing inherited: every cold start.
    pub(crate) const fn none() -> Self {
        Self {
            source_exe: None,
            snapshot_dir: None,
        }
    }

    /// Read the handoff variables once and remove them from this process's
    /// environment, so nothing this image later spawns can inherit them.
    pub(crate) fn take_from_env() -> Self {
        let inherited = Self {
            source_exe: std::env::var_os(UPGRADE_SOURCE_EXE).map(PathBuf::from),
            snapshot_dir: std::env::var_os(UPGRADE_SNAPSHOT_DIR).map(PathBuf::from),
        };
        for key in crate::upgrade::HANDOFF_ENV_VARS {
            if std::env::var_os(key).is_some() {
                // SAFETY: std serializes its own environment access, so the
                // only hazard is a concurrent libc `getenv` on another thread.
                // Both callers run from `phux server` before the blocking
                // pool, signal handlers, or log-rotation task exist; the only
                // earlier threads (the tracing-appender worker, tokio-console
                // under `tokio_unstable`) never read the environment.
                unsafe { std::env::remove_var(key) };
            }
        }
        inherited
    }
}

/// Everything the re-exec'd image must inherit needs `FD_CLOEXEC` cleared: the
/// blob, the listener, every pane master, and every delegated socket.
pub(super) fn clear_inherited_cloexec(
    blob_fd: RawFd,
    listener_fd: RawFd,
    blob: &StateBlob,
) -> Result<FdFlagsGuard, UpgradeError> {
    let mut fd_flags = FdFlagsGuard::new();
    fd_flags.clear_cloexec(blob_fd)?;
    fd_flags.clear_cloexec(listener_fd)?;
    for pane in &blob.panes {
        if let Some(master_fd) = pane.master_fd {
            fd_flags.clear_cloexec(master_fd)?;
        }
    }
    for delegation in &blob.delegations {
        fd_flags.clear_cloexec(delegation::carried_fd(delegation))?;
    }
    Ok(fd_flags)
}

pub(super) fn validate_binary(exe: &Path) -> Result<(), UpgradeError> {
    probe_binary(exe, &["--version"])?;
    probe_binary(exe, &["config", "check"])?;
    refuse_build_kind_change(exe)
}

/// Keep a server on its kind of build across a hot swap.
///
/// The swap re-execs whatever sits at the installed path, so a dev build
/// copied over the installed binary would otherwise become the production
/// server with every live pane in it. A deliberate switch is a restart, not
/// an upgrade.
fn refuse_build_kind_change(exe: &Path) -> Result<(), UpgradeError> {
    let output = probe_output(
        Command::new(exe)
            .arg("--version")
            .env(PROBE_BUILD_KIND_ENV, "1"),
    )?;
    let candidate = output
        .status
        .success()
        .then(|| BuildKind::parse(&String::from_utf8_lossy(&output.stdout)))
        .flatten();
    upgrade_kind_refusal(build_kind(), candidate).map_or(Ok(()), |reason| {
        Err(UpgradeError::Validation(format!(
            "{reason}: {}. To switch deliberately, stop this server and start the other build",
            exe.display()
        )))
    })
}

/// Why a server of kind `current` must not re-exec into `candidate`
/// (`None`: a binary that predates the build-kind probe).
pub(super) fn upgrade_kind_refusal(
    current: BuildKind,
    candidate: Option<BuildKind>,
) -> Option<String> {
    let refused = match (current, candidate) {
        (BuildKind::Dev, _) | (BuildKind::Release, Some(BuildKind::Release)) => false,
        (_, Some(BuildKind::Dev)) | (BuildKind::Release, _) => true,
        (BuildKind::Local, _) => false,
    };
    refused.then(|| {
        let target = candidate.map_or("an unidentified build", |kind| match kind {
            BuildKind::Dev => "a dev build",
            BuildKind::Local => "a local build",
            BuildKind::Release => "a release build",
        });
        format!(
            "refusing to hot-swap a {} server into {target}",
            current.as_str()
        )
    })
}

/// Run a probe of the snapshot. Another thread's `fork` (a pane spawn, a
/// parallel test) can inherit the snapshot's write descriptor for the
/// instant before its own `exec`, and executing the file then fails with
/// ETXTBSY; that window is microseconds, so retry briefly before giving up.
///
/// macOS also has a narrower, kernel-level race: the first `exec` of a file
/// that was just written or copied can be killed with SIGKILL ("Code
/// Signature Invalid") while AMFI's validation cache catches up, even for a
/// binary whose signature is fine (a regular file copy of a signed image,
/// moments after `write`/`copy` returns). A second attempt after a short
/// delay reliably succeeds, so this is retried the same way as ETXTBSY
/// rather than surfacing a spurious validation failure.
fn probe_output(command: &mut Command) -> std::io::Result<std::process::Output> {
    let mut delay = Duration::from_millis(1);
    loop {
        match command.output() {
            Err(err)
                if err.kind() == std::io::ErrorKind::ExecutableFileBusy
                    && delay < Duration::from_millis(500) =>
            {
                std::thread::sleep(delay);
                delay *= 2;
            }
            Ok(ref output)
                if killed_by_signature_race(output) && delay < Duration::from_millis(500) =>
            {
                std::thread::sleep(delay);
                delay *= 2;
            }
            result => return result,
        }
    }
}

/// Whether `output` looks like the macOS first-exec code-signature race
/// (SIGKILL, no output at all) rather than a deliberate kill.
#[cfg(target_os = "macos")]
fn killed_by_signature_race(output: &std::process::Output) -> bool {
    use std::os::unix::process::ExitStatusExt as _;
    output.status.signal() == Some(9) && output.stdout.is_empty() && output.stderr.is_empty()
}

#[cfg(not(target_os = "macos"))]
fn killed_by_signature_race(_output: &std::process::Output) -> bool {
    false
}

fn probe_binary(exe: &Path, args: &[&str]) -> Result<(), UpgradeError> {
    let output = probe_output(Command::new(exe).args(args))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = stderr.trim();
    Err(UpgradeError::Validation(if detail.is_empty() {
        format!(
            "`{} {}` exited with {}",
            exe.display(),
            args.join(" "),
            output.status
        )
    } else {
        format!("`{} {}` failed: {detail}", exe.display(), args.join(" "))
    }))
}

fn files_have_same_bytes(a: &Path, b: &Path) -> std::io::Result<bool> {
    use std::io::Read as _;

    let mut fa = std::fs::File::open(a)?;
    let mut fb = std::fs::File::open(b)?;
    if fa.metadata()?.len() != fb.metadata()?.len() {
        return Ok(false);
    }
    let mut ba = [0_u8; 8192];
    let mut bb = [0_u8; 8192];
    loop {
        let na = fa.read(&mut ba)?;
        let nb = fb.read(&mut bb)?;
        if na != nb || ba[..na] != bb[..na] {
            return Ok(false);
        }
        if na == 0 {
            return Ok(true);
        }
    }
}
