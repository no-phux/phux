//! Graceful-upgrade orchestration (ADR-0032): build the handoff blob, clear
//! `FD_CLOEXEC` on inherited descriptors, validate the on-disk binary, and
//! re-exec it as `server --resume <fd>` plus the effective runtime flags
//! (`--listen` / `--quic` / `--webtransport` / `--connect` / `--hub`) so the
//! resumed image serves the same surface the old one did.
//!
//! Split into [`prepare_upgrade`] (everything reversible — if it fails the old
//! image keeps serving and no child is stranded) and [`UpgradePlan::exec`]
//! (the irreversible re-exec). The caller acks the client between the two.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use futures_util::stream::{FuturesUnordered, StreamExt as _};
use tokio::sync::{mpsc, oneshot};

use phux_config::instance::{BuildKind, PROBE_BUILD_KIND_ENV, build_kind};

use super::RuntimeFlags;
use crate::state::SharedState;
use crate::terminal_actor::{PaneUpgradeHandle, UpgradeHandleRequest};
use crate::upgrade::blob::StateBlob;

const PANE_HANDOFF_TIMEOUT: Duration = Duration::from_secs(2);
const UPGRADE_SOURCE_EXE: &str = crate::upgrade::SOURCE_EXE_ENV;
const UPGRADE_SNAPSHOT_DIR: &str = crate::upgrade::SNAPSHOT_DIR_ENV;

/// Errors preparing a graceful upgrade. Any of these leaves the running server
/// untouched (the children are never stranded — see the module docs).
#[derive(Debug, thiserror::Error)]
pub(super) enum UpgradeError {
    /// The server hasn't captured its upgrade context yet (not serving).
    #[error("server not ready for upgrade (no listener context)")]
    NoContext,
    /// The handoff blob could not be serialized.
    #[error("serialize handoff blob: {0}")]
    Blob(#[from] crate::upgrade::blob::BlobError),
    /// A descriptor / temp-file operation failed.
    #[error("upgrade io: {0}")]
    Io(#[from] std::io::Error),
    /// The on-disk binary failed its pre-commit validation, so the upgrade is
    /// aborted before anything irreversible happens.
    #[error("new binary failed validation: {0}")]
    Validation(String),
    /// A live pane actor did not return the handoff required to preserve it.
    #[error("pane {pane:?} did not provide an upgrade handoff: {reason}")]
    PaneHandoff {
        /// The pane whose actor failed to answer.
        pane: phux_core::ids::ResourceId,
        /// What went wrong after the actor accepted the request.
        reason: &'static str,
    },
    /// The live session tree changed while pane actors prepared their replies.
    #[error("server state changed while collecting upgrade handoffs; retry the upgrade")]
    TreeChanged,
    /// A pane in the tree has an exited engine its exit watcher has not
    /// reaped yet. Same class as [`Self::TreeChanged`]: the reap is imminent.
    #[error("pane {pane:?} is exiting and not yet reaped; retry the upgrade")]
    PaneExiting {
        /// The pane whose engine is gone.
        pane: phux_core::ids::ResourceId,
    },
    /// A pane must carry both sides of a PTY handoff or neither side.
    #[error("pane {pane:?} returned an invalid PTY handoff (master fd and child pid must match)")]
    InvalidPaneHandoff {
        /// The pane whose actor returned an inconsistent pair.
        pane: phux_core::ids::ResourceId,
    },
    /// All pane actors share one bounded preparation window.
    #[error("pane upgrade handoffs did not complete within the aggregate deadline")]
    HandoffDeadline,
}

/// A private executable snapshot copied from one opened source inode, so the
/// installed path cannot be swapped between copy and validation. Re-exec
/// uses the installed path while it still matches: macOS Application
/// Firewall allowlists by path, and a deleted tempfile never can be.
struct PinnedExecutable {
    path: PathBuf,
    source_path: PathBuf,
    dir: tempfile::TempDir,
}

impl PinnedExecutable {
    fn open(path: &Path) -> std::io::Result<Self> {
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
    fn exec_path(&self) -> std::io::Result<&Path> {
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
pub(super) fn cleanup_executable_snapshot(dir: Option<&Path>) {
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

/// Restores the exact descriptor flags if preparation or `exec` returns.
struct FdFlagsGuard {
    originals: Vec<(RawFd, rustix::io::FdFlags)>,
}

impl FdFlagsGuard {
    const fn new() -> Self {
        Self {
            originals: Vec::new(),
        }
    }

    fn clear_cloexec(&mut self, fd: RawFd) -> std::io::Result<()> {
        use rustix::io::{FdFlags, fcntl_getfd, fcntl_setfd};

        // SAFETY: callers keep every descriptor open until this guard drops;
        // borrowing it does not transfer ownership.
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        let flags = fcntl_getfd(borrowed)?;
        self.originals.push((fd, flags));
        fcntl_setfd(borrowed, flags.difference(FdFlags::CLOEXEC))?;
        Ok(())
    }
}

impl Drop for FdFlagsGuard {
    fn drop(&mut self) {
        use rustix::io::fcntl_setfd;

        for &(fd, flags) in self.originals.iter().rev() {
            // SAFETY: `UpgradePlan` keeps its blob file open and the server
            // retains ownership of listener/pane descriptors on failure.
            let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
            let _ = fcntl_setfd(borrowed, flags);
        }
    }
}

/// A validated, ready-to-`exec` upgrade. Holds the open blob temp file so its
/// fd stays valid until the re-exec consumes it.
pub(super) struct UpgradePlan {
    executable: PinnedExecutable,
    blob_fd: RawFd,
    socket_path: PathBuf,
    /// Re-emitted on the resume argv.
    flags: RuntimeFlags,
    _fd_flags: FdFlagsGuard,
    _blob_file: std::fs::File,
    _listener_fd: OwnedFd,
    _handoffs: HashMap<phux_core::ids::ResourceId, PaneUpgradeHandle>,
}

/// Everything `prepare_upgrade` reads out of the live server under one lock,
/// before it starts awaiting pane actors.
struct UpgradeContext {
    listener_fd: RawFd,
    socket_path: PathBuf,
    flags: RuntimeFlags,
    /// The serializable tree as it stood when the actor set was chosen. The
    /// blob is reassembled only if the tree still matches this exactly.
    tree_identity: StateBlob,
    pane_senders: Vec<(
        phux_core::ids::ResourceId,
        mpsc::Sender<UpgradeHandleRequest>,
    )>,
}

/// Do everything reversible: snapshot the tree into a handoff blob, stage it in
/// an inheritable temp file, clear `FD_CLOEXEC` on the blob / listener / every
/// pane master, and validate the on-disk binary. Returns a [`UpgradePlan`] the
/// caller execs *after* acking the client.
pub(super) async fn prepare_upgrade(state: &SharedState) -> Result<UpgradePlan, UpgradeError> {
    let UpgradeContext {
        listener_fd,
        socket_path,
        flags,
        tree_identity,
        pane_senders,
    } = capture_upgrade_context(state)?;

    let listener = dup_listener(listener_fd)?;
    let handoffs = collect_pane_handoffs(pane_senders, PANE_HANDOFF_TIMEOUT).await?;
    let blob = reassemble_unchanged_tree(
        state,
        listener_fd,
        listener.as_raw_fd(),
        &tree_identity,
        &handoffs,
    )?;

    let blob_file = stage_blob_file(&blob)?;
    let blob_fd = blob_file.as_raw_fd();
    let executable = pin_validated_executable(flags.upgrade_source_exe.as_deref())?;
    let fd_flags = clear_inherited_cloexec(blob_fd, listener_fd, &blob)?;

    Ok(UpgradePlan {
        executable,
        blob_fd,
        socket_path,
        flags,
        _fd_flags: fd_flags,
        _blob_file: blob_file,
        _listener_fd: listener,
        _handoffs: handoffs,
    })
}

/// Read the listener context, the tree identity, and one upgrade sender per
/// pane out of the live server under a single lock.
fn capture_upgrade_context(state: &SharedState) -> Result<UpgradeContext, UpgradeError> {
    state
        .with(|s| {
            s.upgrade_context()
                .map(|(listener_fd, path, flags)| UpgradeContext {
                    listener_fd,
                    socket_path: path.to_path_buf(),
                    flags,
                    tree_identity: s.assemble_upgrade_blob(listener_fd, &HashMap::new()),
                    pane_senders: s
                        .upgrade_handles()
                        .into_iter()
                        .map(|(pane, handle)| (pane, handle.upgrade))
                        .collect(),
                })
        })
        .ok_or(UpgradeError::NoContext)
}

/// Own the listener identity before awaiting actors. The duplicate, not a
/// raw descriptor owned elsewhere in the runtime, is what crosses exec.
fn dup_listener(listener_fd: RawFd) -> Result<OwnedFd, UpgradeError> {
    // SAFETY: the runtime owns the listening descriptor for its entire serve
    // loop; this borrow lasts only for the dup syscall.
    rustix::io::dup(unsafe { BorrowedFd::borrow_raw(listener_fd) })
        .map_err(|err| UpgradeError::Io(std::io::Error::from(err)))
}

/// Re-read under one lock and require the exact serializable state used to
/// choose actors to still be current. A concurrent split/close/focus/name
/// change aborts rather than pairing old handoffs with a new tree.
fn reassemble_unchanged_tree(
    state: &SharedState,
    listener_fd: RawFd,
    inherited_fd: RawFd,
    tree_identity: &StateBlob,
    handoffs: &HashMap<phux_core::ids::ResourceId, PaneUpgradeHandle>,
) -> Result<StateBlob, UpgradeError> {
    state
        .with(|s| {
            let current = s.assemble_upgrade_blob(listener_fd, &HashMap::new());
            (current == *tree_identity).then(|| s.assemble_upgrade_blob(inherited_fd, handoffs))
        })
        .ok_or(UpgradeError::TreeChanged)
}

/// Stage the blob in an anonymous temp file (auto-removed on close), rewound
/// so the resumed image reads from the start.
fn stage_blob_file(blob: &StateBlob) -> Result<std::fs::File, UpgradeError> {
    let mut blob_file = tempfile::tempfile()?;
    blob_file.write_all(&blob.to_bytes()?)?;
    blob_file.seek(SeekFrom::Start(0))?;
    Ok(blob_file)
}

/// Pin and validate the replacement image before any descriptor flag changes.
/// A broken replacement binary must leave the old process's descriptor policy
/// untouched.
fn pin_validated_executable(
    inherited_source: Option<&Path>,
) -> Result<PinnedExecutable, UpgradeError> {
    let source_exe =
        inherited_source.map_or_else(std::env::current_exe, |path| Ok(path.to_path_buf()))?;
    let executable = PinnedExecutable::open(&source_exe)?;
    validate_binary(&executable.path)?;
    Ok(executable)
}

/// The upgrade handoff a resumed image inherits through its environment.
/// The `PHUX_UPGRADE_*` variables are consumed only by a `--resume` start
/// and then removed, so they never leak into panes and steer an unrelated
/// server started from one onto the wrong binary.
#[derive(Debug)]
pub(super) struct InheritedUpgradeEnv {
    /// Installed executable the next upgrade pins instead of `current_exe`.
    pub(super) source_exe: Option<PathBuf>,
    /// The previous image's private executable snapshot directory.
    pub(super) snapshot_dir: Option<PathBuf>,
}

impl InheritedUpgradeEnv {
    /// Nothing inherited: every cold start.
    pub(super) const fn none() -> Self {
        Self {
            source_exe: None,
            snapshot_dir: None,
        }
    }

    /// Read the handoff variables once and remove them from this process's
    /// environment, so nothing this image later spawns can inherit them.
    pub(super) fn take_from_env() -> Self {
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
/// blob, the listener, and every pane master.
fn clear_inherited_cloexec(
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
    Ok(fd_flags)
}

/// Ask one pane's actor for its handoff. Only live tree Terminals are asked
/// (see `ServerState::upgrade_handles`), so a closed upgrade mailbox means
/// a pane whose engine already exited and whose exit watcher has not reaped
/// it yet: [`UpgradeError::PaneExiting`], which a retry clears. Carrying it
/// would resurrect a killed pane as a blank one that never exits. Any
/// failure after the request was accepted (a dropped reply, a half PTY pair,
/// the deadline) means a live actor could not hand off.
async fn request_pane_handoff(
    pane: phux_core::ids::ResourceId,
    upgrade: &mpsc::Sender<UpgradeHandleRequest>,
) -> Result<PaneUpgradeHandle, UpgradeError> {
    let (reply, rx) = oneshot::channel();
    if upgrade.send(UpgradeHandleRequest { reply }).await.is_err() {
        tracing::warn!(
            ?pane,
            "upgrade: pane engine already exited and is awaiting its reap; aborting for retry"
        );
        return Err(UpgradeError::PaneExiting { pane });
    }
    rx.await.map_err(|_| UpgradeError::PaneHandoff {
        pane,
        reason: "actor dropped its reply",
    })
}

async fn collect_pane_handoffs(
    handles: Vec<(
        phux_core::ids::ResourceId,
        mpsc::Sender<UpgradeHandleRequest>,
    )>,
    deadline: Duration,
) -> Result<HashMap<phux_core::ids::ResourceId, PaneUpgradeHandle>, UpgradeError> {
    tokio::time::timeout(deadline, async move {
        let pane_count = handles.len();
        let mut pending = handles
            .into_iter()
            .map(|(pane, sender)| async move {
                request_pane_handoff(pane, &sender)
                    .await
                    .map(|handoff| (pane, handoff))
            })
            .collect::<FuturesUnordered<_>>();
        let mut handoffs = HashMap::with_capacity(pane_count);
        while let Some(result) = pending.next().await {
            let (pane, handoff) = result?;
            let pair_is_valid = matches!(
                (&handoff.master_fd, handoff.child_pid),
                (Some(_), Some(1..)) | (None, None)
            );
            if !pair_is_valid {
                return Err(UpgradeError::InvalidPaneHandoff { pane });
            }
            handoffs.insert(pane, handoff);
        }
        Ok(handoffs)
    })
    .await
    .map_err(|_| UpgradeError::HandoffDeadline)?
}

impl UpgradePlan {
    /// Re-exec the new binary with [`resume_args`], replacing this process.
    /// Returns only on failure, which is harmless: nothing was closed, so the
    /// old image keeps serving.
    pub(super) fn exec(self) -> std::io::Error {
        let exe = match self.executable.exec_path() {
            Ok(path) => path.to_path_buf(),
            Err(err) => return err,
        };
        let mut command = Command::new(&exe);
        command
            .env(UPGRADE_SOURCE_EXE, &self.executable.source_path)
            .env(UPGRADE_SNAPSHOT_DIR, self.executable.dir.path())
            .args(resume_args(
                self.blob_fd,
                &self.socket_path,
                self.flags.clone(),
            ));
        command.exec()
    }
}

/// The re-exec argv (after argv0): `server --resume <blob_fd> --socket
/// <path>` plus one entry per applied runtime flag.
fn resume_args(blob_fd: RawFd, socket_path: &Path, flags: RuntimeFlags) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        OsString::from("server"),
        OsString::from("--resume"),
        OsString::from(blob_fd.to_string()),
        OsString::from("--socket"),
        socket_path.into(),
    ];
    if let Some(addr) = flags.ws_addr {
        args.push(OsString::from("--listen"));
        args.push(OsString::from(addr.to_string()));
    }
    if let Some(addr) = flags.quic_addr {
        args.push(OsString::from("--quic"));
        args.push(OsString::from(addr.to_string()));
    }
    if let Some(addr) = flags.wt_addr {
        args.push(OsString::from("--webtransport"));
        args.push(OsString::from(addr.to_string()));
    }
    if let Some(relay) = flags.connect {
        args.push(OsString::from("--connect"));
        args.push(OsString::from(relay));
    }
    if flags.hub {
        args.push(OsString::from("--hub"));
    }
    if let Some(idle) = flags.exit_after_idle {
        // Whole seconds, rounded up: never `--exit-after-idle 0`.
        let secs = idle.as_secs() + u64::from(idle.subsec_nanos() > 0);
        args.push(OsString::from("--exit-after-idle"));
        args.push(OsString::from(secs.to_string()));
    }
    args
}

/// Validate the replacement image runs *and* loads this host's config
/// before the irreversible `execve`; a config failure after exec would take
/// the live server down.
fn validate_binary(exe: &Path) -> Result<(), UpgradeError> {
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
    let output = Command::new(exe)
        .arg("--version")
        .env(PROBE_BUILD_KIND_ENV, "1")
        .output()?;
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
fn upgrade_kind_refusal(current: BuildKind, candidate: Option<BuildKind>) -> Option<String> {
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

fn probe_binary(exe: &Path, args: &[&str]) -> Result<(), UpgradeError> {
    let output = Command::new(exe).args(args).output()?;
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]

    use std::os::fd::AsRawFd;

    use super::*;

    fn args_as_strings(flags: RuntimeFlags) -> Vec<String> {
        resume_args(7, Path::new("/run/phux/phux.sock"), flags)
            .into_iter()
            .map(|a| a.into_string().unwrap())
            .collect()
    }

    fn fd_flags(fd: RawFd) -> rustix::io::FdFlags {
        // SAFETY: test callers keep the backing file open for this borrow.
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        rustix::io::fcntl_getfd(borrowed).unwrap()
    }

    fn set_fd_flags(fd: RawFd, flags: rustix::io::FdFlags) {
        // SAFETY: test callers keep the backing file open for this borrow.
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        rustix::io::fcntl_setfd(borrowed, flags).unwrap();
    }

    fn no_pty_handoff() -> PaneUpgradeHandle {
        PaneUpgradeHandle {
            master_fd: None,
            child_pid: None,
            cols: 80,
            rows: 24,
            cell_px: None,
            title: None,
            cwd: None,
            vt_replay_bytes: Vec::new(),
            scrollback_bytes: Vec::new(),
        }
    }

    /// Every opt-in flag the server applied is re-emitted on the resume argv
    /// (dropping them silently was the original upgrade bug); a default
    /// server re-execs with the bare argv; a sub-second idle lifetime rounds
    /// up rather than becoming `--exit-after-idle 0`.
    #[test]
    fn resume_args_reconstruct_the_served_surface() {
        const BASE: [&str; 5] = ["server", "--resume", "7", "--socket", "/run/phux/phux.sock"];
        assert_eq!(args_as_strings(RuntimeFlags::default()), BASE);

        let every = RuntimeFlags {
            ws_addr: Some("127.0.0.1:8787".parse().unwrap()),
            quic_addr: Some("0.0.0.0:4433".parse().unwrap()),
            wt_addr: Some("0.0.0.0:4434".parse().unwrap()),
            connect: Some("relay.example:4433".to_owned()),
            hub: true,
            exit_after_idle: Some(Duration::from_secs(90)),
            upgrade_source_exe: None,
        };
        let mut expected: Vec<&str> = BASE.to_vec();
        expected.extend([
            "--listen",
            "127.0.0.1:8787",
            "--quic",
            "0.0.0.0:4433",
            "--webtransport",
            "0.0.0.0:4434",
            "--connect",
            "relay.example:4433",
            "--hub",
            "--exit-after-idle",
            "90",
        ]);
        assert_eq!(args_as_strings(every), expected);

        let sub_second = RuntimeFlags {
            exit_after_idle: Some(Duration::from_millis(300)),
            ..RuntimeFlags::default()
        };
        assert_eq!(args_as_strings(sub_second)[5..], ["--exit-after-idle", "1"]);
    }

    #[test]
    fn descriptor_guard_restores_flags_after_partial_prepare_failure() {
        let file = tempfile::tempfile().unwrap();
        let fd = file.as_raw_fd();
        let original = fd_flags(fd).union(rustix::io::FdFlags::CLOEXEC);
        set_fd_flags(fd, original);

        let mut guard = FdFlagsGuard::new();
        guard.clear_cloexec(fd).unwrap();
        assert!(!fd_flags(fd).contains(rustix::io::FdFlags::CLOEXEC));
        let closed_file = tempfile::tempfile().unwrap();
        let closed_fd = closed_file.as_raw_fd();
        drop(closed_file);
        assert!(guard.clear_cloexec(closed_fd).is_err());
        drop(guard);

        assert_eq!(fd_flags(fd), original);
    }

    #[test]
    fn exec_failure_restores_original_descriptor_flags() {
        let listener = tempfile::tempfile().unwrap();
        let listener_fd = listener.as_raw_fd();
        let original = fd_flags(listener_fd).union(rustix::io::FdFlags::CLOEXEC);
        set_fd_flags(listener_fd, original);

        let blob_file = tempfile::tempfile().unwrap();
        let blob_fd = blob_file.as_raw_fd();
        let mut guard = FdFlagsGuard::new();
        guard.clear_cloexec(blob_fd).unwrap();
        guard.clear_cloexec(listener_fd).unwrap();
        let plan = UpgradePlan {
            executable: PinnedExecutable {
                path: PathBuf::from("/definitely/missing/phux"),
                source_path: PathBuf::from("/definitely/missing/phux"),
                dir: tempfile::tempdir().unwrap(),
            },
            blob_fd,
            socket_path: PathBuf::from("/tmp/phux.sock"),
            flags: RuntimeFlags::default(),
            _fd_flags: guard,
            _blob_file: blob_file,
            _listener_fd: tempfile::tempfile().unwrap().into(),
            _handoffs: HashMap::new(),
        };

        assert_eq!(plan.exec().kind(), std::io::ErrorKind::NotFound);
        assert_eq!(fd_flags(listener_fd), original);
    }

    fn install(from: &str, to: &Path) {
        std::fs::copy(from, to).unwrap();
        std::fs::set_permissions(to, std::fs::metadata(from).unwrap().permissions()).unwrap();
    }

    /// Re-exec lands on the installed (allowlistable) path while it still
    /// matches the validated snapshot, and aborts once the install is
    /// swapped; validation always sees the snapshot.
    #[test]
    fn upgrade_execs_the_installed_path_only_while_it_matches_the_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("phux");
        install("/usr/bin/true", &path);
        let pinned = PinnedExecutable::open(&path).unwrap();
        assert_ne!(pinned.path, pinned.source_path);
        assert_eq!(pinned.exec_path().unwrap(), pinned.source_path.as_path());

        let replacement = dir.path().join("replacement");
        install("/usr/bin/false", &replacement);
        std::fs::rename(&replacement, &path).unwrap();
        validate_binary(&pinned.path).expect("the pinned image remains the validated one");
        assert!(validate_binary(&path).is_err());
        assert!(
            pinned.exec_path().is_err(),
            "a swapped install path must abort rather than re-exec the tempfile"
        );
    }

    fn write_stub_phux(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;

        let path = dir.join("phux");
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// `config check` runs before the irreversible exec: a replacement that
    /// cannot load this host's config aborts with the loader's diagnostic.
    #[test]
    fn validate_binary_requires_version_and_config_check() {
        let dir = tempfile::tempdir().unwrap();
        let good = write_stub_phux(
            dir.path(),
            "#!/bin/sh\ncase \"$1\" in\n--version|config) exit 0 ;;\n*) exit 1 ;;\nesac\n",
        );
        validate_binary(&good).expect("a coherent replacement image must pass");

        let bad = write_stub_phux(
            dir.path(),
            "#!/bin/sh\n\
             case \"$1\" in\n\
             --version) echo phux 0; exit 0 ;;\n\
             config) echo 'extends layer missing' >&2; exit 1 ;;\n\
             *) exit 0 ;;\n\
             esac\n",
        );
        let err = validate_binary(&bad).expect_err("a broken config must abort the upgrade");
        let message = err.to_string();
        assert!(
            message.contains("config check"),
            "validation error must name the probe: {message}"
        );
        assert!(
            message.contains("extends layer missing"),
            "validation error must carry the loader diagnostic: {message}"
        );
    }

    #[test]
    fn a_production_server_never_hot_swaps_into_a_dev_build() {
        use BuildKind::{Dev, Local, Release};
        for current in [Release, Local] {
            let refusal = upgrade_kind_refusal(current, Some(Dev))
                .expect("a dev build must never replace a production server");
            assert!(refusal.contains("a dev build"), "{refusal}");
        }
        // A stamped release only ever becomes another stamped release: a
        // copied-out local `--release` build or a pre-probe binary is refused.
        assert!(upgrade_kind_refusal(Release, Some(Local)).is_some());
        assert!(upgrade_kind_refusal(Release, None).is_some());
        assert!(upgrade_kind_refusal(Release, Some(Release)).is_none());
        // An unstamped server can move onto a release; dev servers are free.
        assert!(upgrade_kind_refusal(Local, Some(Release)).is_none());
        assert!(upgrade_kind_refusal(Local, None).is_none());
        assert!(upgrade_kind_refusal(Dev, Some(Release)).is_none());
        assert!(upgrade_kind_refusal(Dev, Some(Dev)).is_none());
    }

    #[test]
    fn validate_binary_accepts_when_version_and_config_check_succeed() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_stub_phux(
            dir.path(),
            "#!/bin/sh\n\
             case \"$1\" in\n\
             --version|config) exit 0 ;;\n\
             *) exit 1 ;;\n\
             esac\n",
        );
        validate_binary(&path).expect("a coherent replacement image must pass");
    }

    /// A tree pane whose engine exited but is not reaped yet aborts as
    /// retryable, never staging a blob that would resurrect it as a blank
    /// pane in the new image.
    #[tokio::test]
    async fn pane_handoff_with_a_closed_mailbox_aborts_for_retry() {
        let (dead, receiver) = mpsc::channel(1);
        drop(receiver);
        let mut registry = phux_core::registry::Registry::new();
        let session = registry.new_session("s".to_owned());
        let window = registry.new_window(session).unwrap();
        let dead_pane = registry.new_terminal(window).unwrap();

        let err = collect_pane_handoffs(vec![(dead_pane, dead)], Duration::from_secs(1))
            .await
            .expect_err("an unreaped dead pane must not be staged");

        assert!(matches!(err, UpgradeError::PaneExiting { pane } if pane == dead_pane));
        assert!(
            err.to_string().contains("retry the upgrade"),
            "the user must be told to retry: {err}"
        );
    }

    /// A live actor that accepted the request but never answered still
    /// aborts: its PTY may be alive and must not be stranded.
    #[tokio::test]
    async fn pane_handoff_aborts_when_a_live_actor_drops_its_reply() {
        let (upgrade, mut receiver) = mpsc::channel::<UpgradeHandleRequest>(1);
        tokio::spawn(async move {
            drop(receiver.recv().await);
        });

        let result = request_pane_handoff(phux_core::ids::ResourceId::default(), &upgrade).await;

        assert!(matches!(
            result,
            Err(UpgradeError::PaneHandoff {
                reason: "actor dropped its reply",
                ..
            })
        ));
    }

    /// phux-twft regression: the live server refused every upgrade with
    /// `pane ResourceId(..) did not provide an upgrade handoff: actor mailbox
    /// closed` because the capture asked every handle in the resource table,
    /// including `AgentSession` engines (built with a closed upgrade
    /// mailbox) and handles that had outlived their registry entry. Only
    /// live tree panes are asked now, and the reversible preparation
    /// completes with exactly the live pane.
    #[tokio::test(flavor = "current_thread")]
    async fn upgrade_capture_ignores_agent_sessions_and_orphan_handles() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let state = SharedState::new();
                let (live_pane, orphan, agent) = state.with_mut(|s| {
                    let (sid, wid, live_pane) = s.seed_session("main");
                    let live = crate::terminal_actor::TerminalActor::new_with_seed(20, 5, b"live")
                        .unwrap();
                    let live_token = live.token.clone();
                    let _ = s.spawn_resource_actor(
                        live_pane,
                        live.handle,
                        live_token,
                        live.actor.run(),
                    );

                    // A leaked handle: its engine is gone and its registry
                    // entry was removed without forgetting the handle.
                    let orphan = s.add_pane_to_session(sid).unwrap();
                    let dead =
                        crate::terminal_actor::TerminalActor::new_with_seed(20, 5, b"").unwrap();
                    let _ = s.register_resource_handle(orphan, dead.handle, dead.token);
                    drop(dead.actor);
                    let _ = s.registry_mut().remove_resource(orphan);

                    // An agent session bound to the live pane.
                    let facet = phux_core::resource::AgentFacet {
                        provider: "opencode".to_owned(),
                        native_id: None,
                        state: None,
                    };
                    let agent = s
                        .registry_mut()
                        .new_agent_session(live_pane, facet)
                        .unwrap();
                    let token = tokio_util::sync::CancellationToken::new();
                    let bundle = crate::resource::agent_session::AgentSessionActor::build(
                        live_pane,
                        "opencode",
                        None,
                        token.clone(),
                        4096,
                    );
                    let _ = s.spawn_resource_actor(agent, bundle.handle, token, bundle.actor.run());

                    let _ = s.build_session_snapshot(sid);
                    let _ = s.intern_window_wire(wid);
                    s.set_upgrade_context(
                        7,
                        PathBuf::from("/nonexistent/phux.sock"),
                        RuntimeFlags::default(),
                    );
                    (live_pane, orphan, agent)
                });

                let context = capture_upgrade_context(&state).unwrap();
                let asked: Vec<_> = context.pane_senders.iter().map(|(id, _)| *id).collect();
                assert_eq!(asked, vec![live_pane], "only the live tree pane is asked");
                assert!(
                    !asked.contains(&agent),
                    "an agent session has no PTY to hand off"
                );
                assert!(!asked.contains(&orphan), "an orphan handle is not a pane");

                let handoffs = collect_pane_handoffs(context.pane_senders, Duration::from_secs(2))
                    .await
                    .expect("an agent session or orphan handle must not abort");
                assert_eq!(handoffs.len(), 1);
                assert!(handoffs.contains_key(&live_pane));

                let blob = reassemble_unchanged_tree(
                    &state,
                    context.listener_fd,
                    context.listener_fd,
                    &context.tree_identity,
                    &handoffs,
                )
                .expect("the tree did not change");
                assert_eq!(blob.panes.len(), 1, "only the live pane crosses");
                assert!(
                    blob.panes[0]
                        .vt_replay_bytes
                        .windows(4)
                        .any(|w| w == b"live"),
                    "the live pane carries its own handoff"
                );
            })
            .await;
    }

    #[tokio::test(start_paused = true)]
    async fn pane_handoff_collection_aborts_at_its_aggregate_deadline() {
        let (upgrade, _receiver) = mpsc::channel(1);

        let result = collect_pane_handoffs(
            vec![(phux_core::ids::ResourceId::default(), upgrade)],
            Duration::from_secs(2),
        )
        .await;

        assert!(matches!(result, Err(UpgradeError::HandoffDeadline)));
    }

    #[tokio::test(start_paused = true)]
    async fn pane_handoffs_are_collected_concurrently() {
        let mut handles = Vec::new();
        for _ in 0..2 {
            let (sender, mut receiver) = mpsc::channel::<UpgradeHandleRequest>(1);
            tokio::spawn(async move {
                let request = receiver.recv().await.unwrap();
                tokio::time::sleep(Duration::from_millis(1_500)).await;
                let _ = request.reply.send(no_pty_handoff());
            });
            handles.push((phux_core::ids::ResourceId::default(), sender));
        }

        let handoffs = collect_pane_handoffs(handles, Duration::from_secs(2))
            .await
            .expect("two 1.5s actors fit in one 2s window only when concurrent");
        assert_eq!(handoffs.len(), 1, "the duplicate fixture pane id coalesces");
    }

    #[tokio::test]
    async fn pane_handoff_rejects_a_half_present_pty_pair() {
        let pane = phux_core::ids::ResourceId::default();
        let (sender, mut receiver) = mpsc::channel::<UpgradeHandleRequest>(1);
        tokio::spawn(async move {
            let request = receiver.recv().await.unwrap();
            let mut handoff = no_pty_handoff();
            handoff.child_pid = Some(42);
            let _ = request.reply.send(handoff);
        });

        let result = collect_pane_handoffs(vec![(pane, sender)], Duration::from_secs(1)).await;
        assert!(matches!(
            result,
            Err(UpgradeError::InvalidPaneHandoff { pane: found }) if found == pane
        ));
    }
}
