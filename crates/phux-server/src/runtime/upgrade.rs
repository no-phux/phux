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
        /// Whether its mailbox closed, reply disappeared, or deadline elapsed.
        reason: &'static str,
    },
    /// The live session tree changed while pane actors prepared their replies.
    #[error("server state changed while collecting upgrade handoffs; retry the upgrade")]
    TreeChanged,
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

/// A private executable snapshot copied from one opened source inode. Both
/// validation and exec use the snapshot, so replacing the installed path
/// cannot swap in a different image between the two operations.
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
    /// The server's effective runtime flags (phux-v45.10), read back from the
    /// upgrade context the runtime captured at startup. Re-emitted on the
    /// resume argv so `--listen` / `--quic` / `--webtransport` / `--connect`
    /// / `--hub` survive the re-exec.
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

/// The graceful-upgrade handoff a resumed image inherits through its
/// environment from the server that re-exec'd into it ([`UpgradePlan::exec`]).
///
/// Regression (phux-m5yj): the `PHUX_UPGRADE_*` variables are consumed only
/// by a `--resume` start, then removed from the process environment. They
/// used to be read lazily at upgrade time by any server, and they also leaked
/// into every pane child. A server cold-started from an upgraded server's
/// pane therefore pinned the *outer* server's installed binary and re-exec'd
/// into a different phux on its first upgrade -- one whose protocol refused
/// every client, so the resumed server never accepted again.
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
                // SAFETY: std serializes its own environment access behind a
                // process-wide lock, so this cannot race a Rust-side read. The
                // residual hazard is a concurrent libc `getenv` from foreign
                // code on another thread. Both callers, `ServerRuntime::resume`
                // and `ServerRuntime::discard_inherited_upgrade`, run from
                // `phux server` after building the current-thread runtime
                // (which starts no threads) and before the blocking
                // pool, signal handlers, or log-rotation task exist. The only
                // earlier threads are the tracing-appender worker (with
                // `PHUX_LOG` set), which only writes, and the developer-only
                // tokio-console server under `tokio_unstable`.
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

async fn request_pane_handoff(
    pane: phux_core::ids::ResourceId,
    upgrade: &mpsc::Sender<UpgradeHandleRequest>,
) -> Result<PaneUpgradeHandle, UpgradeError> {
    let (reply, rx) = oneshot::channel();
    upgrade
        .send(UpgradeHandleRequest { reply })
        .await
        .map_err(|_| UpgradeError::PaneHandoff {
            pane,
            reason: "actor mailbox closed",
        })?;
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
    /// Re-exec the new binary as `server --resume <blob_fd> --socket <path>`
    /// plus the effective runtime flags (`--listen` / `--quic` /
    /// `--webtransport` / `--hub`, phux-v45.10), replacing this process in
    /// place. Returns only on failure — and a failure is harmless: nothing
    /// was closed, so the old image keeps serving and the children stay
    /// attached.
    pub(super) fn exec(self) -> std::io::Error {
        let mut command = Command::new(&self.executable.path);
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

/// Build the full argv (after argv0) for the graceful-upgrade re-exec:
/// `server --resume <blob_fd> --socket <path>` plus one entry per effective
/// runtime flag (phux-v45.10). Pure, so the reconstruction is testable
/// without exec'ing anything.
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
        // The flag's unit is whole seconds. Round UP so a sub-second value
        // (library-only; the CLI floor is 1s) survives as 1 rather than
        // collapsing to `--exit-after-idle 0`, which would make the resumed
        // image exit the moment its last client dropped.
        let secs = idle.as_secs() + u64::from(idle.subsec_nanos() > 0);
        args.push(OsString::from("--exit-after-idle"));
        args.push(OsString::from(secs.to_string()));
    }
    args
}

/// Validate the replacement image can run *and* load this host's config.
///
/// `--version` only proves the file is executable. Config is loaded after
/// `execve`, which is irreversible: a missing `extends` layer then takes
/// the live server down (phux-69pq.12). `config check` uses the same
/// loader `phux server` does, so a failure here leaves the old image
/// serving.
fn validate_binary(exe: &Path) -> Result<(), UpgradeError> {
    probe_binary(exe, &["--version"])?;
    probe_binary(exe, &["config", "check"])?;
    Ok(())
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]

    use std::net::SocketAddr;
    use std::os::fd::AsRawFd;

    use super::*;

    const WS: &str = "127.0.0.1:8787";
    const QUIC: &str = "0.0.0.0:4433";
    const WT: &str = "0.0.0.0:4434";

    fn flags(ws: Option<&str>, quic: Option<&str>, wt: Option<&str>, hub: bool) -> RuntimeFlags {
        let addr = |s: &str| s.parse::<SocketAddr>().unwrap();
        RuntimeFlags {
            ws_addr: ws.map(addr),
            quic_addr: quic.map(addr),
            wt_addr: wt.map(addr),
            connect: None,
            hub,
            exit_after_idle: None,
            upgrade_source_exe: None,
        }
    }

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

    /// The base of the resume argv is invariant: subcommand, blob fd, socket.
    const BASE: [&str; 5] = ["server", "--resume", "7", "--socket", "/run/phux/phux.sock"];

    /// phux-v45.10 regression matrix: every combination of the opt-in runtime
    /// flags must be reconstructed on the re-exec argv — the original bug was
    /// an argv of only `server --resume <fd> --socket <path>`, silently
    /// dropping `--listen`, `--quic`, and `--hub` across `phux server
    /// upgrade` (and later, in the same class, `--webtransport` — phux-0wmf).
    #[test]
    fn resume_args_reconstructs_every_flag_combination() {
        type Case<'a> = (
            Option<&'a str>,
            Option<&'a str>,
            Option<&'a str>,
            bool,
            &'a [&'a str],
        );
        let cases: [Case<'_>; 16] = [
            (None, None, None, false, &[]),
            (Some(WS), None, None, false, &["--listen", WS]),
            (None, Some(QUIC), None, false, &["--quic", QUIC]),
            (None, None, Some(WT), false, &["--webtransport", WT]),
            (None, None, None, true, &["--hub"]),
            (
                Some(WS),
                Some(QUIC),
                None,
                false,
                &["--listen", WS, "--quic", QUIC],
            ),
            (
                Some(WS),
                None,
                Some(WT),
                false,
                &["--listen", WS, "--webtransport", WT],
            ),
            (Some(WS), None, None, true, &["--listen", WS, "--hub"]),
            (
                None,
                Some(QUIC),
                Some(WT),
                false,
                &["--quic", QUIC, "--webtransport", WT],
            ),
            (None, Some(QUIC), None, true, &["--quic", QUIC, "--hub"]),
            (None, None, Some(WT), true, &["--webtransport", WT, "--hub"]),
            (
                Some(WS),
                Some(QUIC),
                Some(WT),
                false,
                &["--listen", WS, "--quic", QUIC, "--webtransport", WT],
            ),
            (
                Some(WS),
                Some(QUIC),
                None,
                true,
                &["--listen", WS, "--quic", QUIC, "--hub"],
            ),
            (
                Some(WS),
                None,
                Some(WT),
                true,
                &["--listen", WS, "--webtransport", WT, "--hub"],
            ),
            (
                None,
                Some(QUIC),
                Some(WT),
                true,
                &["--quic", QUIC, "--webtransport", WT, "--hub"],
            ),
            (
                Some(WS),
                Some(QUIC),
                Some(WT),
                true,
                &[
                    "--listen",
                    WS,
                    "--quic",
                    QUIC,
                    "--webtransport",
                    WT,
                    "--hub",
                ],
            ),
        ];
        for (ws, quic, wt, hub, extra) in cases {
            let mut expected: Vec<String> = BASE.iter().map(ToString::to_string).collect();
            expected.extend(extra.iter().map(ToString::to_string));
            assert_eq!(
                args_as_strings(flags(ws, quic, wt, hub)),
                expected,
                "argv mismatch for ws={ws:?} quic={quic:?} wt={wt:?} hub={hub}",
            );
        }
    }

    /// The default (UDS-only, non-hub) server re-execs with the bare argv —
    /// no spurious flags invented for surfaces it never served.
    #[test]
    fn resume_args_default_flags_add_nothing() {
        assert_eq!(args_as_strings(RuntimeFlags::default()), BASE);
    }

    #[test]
    fn resume_args_preserves_ad_hoc_connector() {
        let flags = RuntimeFlags {
            connect: Some("relay.example:4433".to_owned()),
            ..RuntimeFlags::default()
        };
        let mut expected: Vec<String> = BASE.iter().map(ToString::to_string).collect();
        expected.extend(["--connect".to_owned(), "relay.example:4433".to_owned()]);
        assert_eq!(args_as_strings(flags), expected);
    }

    /// An ephemeral server's lifetime survives its own upgrade. Dropping it
    /// here would silently promote a bounded harness daemon to an immortal
    /// one — the leak this flag exists to close, reintroduced by the one
    /// operation whose whole promise is "same server, new image".
    #[test]
    fn resume_args_preserves_ephemeral_lifetime() {
        let flags = RuntimeFlags {
            exit_after_idle: Some(std::time::Duration::from_secs(90)),
            ..RuntimeFlags::default()
        };
        let mut expected: Vec<String> = BASE.iter().map(ToString::to_string).collect();
        expected.extend(["--exit-after-idle".to_owned(), "90".to_owned()]);
        assert_eq!(args_as_strings(flags), expected);
    }

    /// A sub-second lifetime (reachable only through `ServerConfig`, which
    /// tests use) rounds UP. Truncation would emit `--exit-after-idle 0`,
    /// making the resumed image exit the instant its last client dropped —
    /// strictly more eager than the server it replaced.
    #[test]
    fn resume_args_rounds_sub_second_lifetime_up() {
        let flags = RuntimeFlags {
            exit_after_idle: Some(std::time::Duration::from_millis(300)),
            ..RuntimeFlags::default()
        };
        let mut expected: Vec<String> = BASE.iter().map(ToString::to_string).collect();
        expected.extend(["--exit-after-idle".to_owned(), "1".to_owned()]);
        assert_eq!(args_as_strings(flags), expected);
    }

    /// The flags land in the plan from the shared-state upgrade context —
    /// the same channel `prepare_upgrade` reads — not from anywhere argv-ish.
    #[test]
    fn upgrade_context_round_trips_runtime_flags() {
        let state = SharedState::new();
        assert!(
            state.with(|s| s.upgrade_context().is_none()),
            "no context before serving"
        );
        let captured = flags(Some(WS), Some(QUIC), Some(WT), true);
        state.with_mut(|s| {
            s.set_upgrade_context(3, PathBuf::from("/tmp/phux.sock"), captured.clone());
        });
        let (fd, path, roundtripped) = state
            .with(|s| {
                s.upgrade_context()
                    .map(|(fd, path, flags)| (fd, path.to_path_buf(), flags))
            })
            .expect("context set");
        assert_eq!(fd, 3);
        assert_eq!(path, PathBuf::from("/tmp/phux.sock"));
        assert_eq!(roundtripped, captured);
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

    #[test]
    fn executable_validation_and_exec_share_one_private_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("phux");
        std::fs::copy("/usr/bin/true", &path).unwrap();
        std::fs::set_permissions(
            &path,
            std::fs::metadata("/usr/bin/true").unwrap().permissions(),
        )
        .unwrap();
        let pinned = PinnedExecutable::open(&path).unwrap();

        let replacement = dir.path().join("replacement");
        std::fs::copy("/usr/bin/false", &replacement).unwrap();
        std::fs::set_permissions(
            &replacement,
            std::fs::metadata("/usr/bin/false").unwrap().permissions(),
        )
        .unwrap();
        std::fs::rename(&replacement, &path).unwrap();

        validate_binary(&pinned.path).expect("the pinned image remains the validated one");
        assert!(
            validate_binary(&path).is_err(),
            "the replaced filesystem path now names a different image"
        );
    }

    fn write_stub_phux(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;

        let path = dir.join("phux");
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn validate_binary_refuses_when_config_check_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_stub_phux(
            dir.path(),
            "#!/bin/sh\n\
             case \"$1\" in\n\
             --version) echo phux 0; exit 0 ;;\n\
             config) echo 'extends layer missing' >&2; exit 1 ;;\n\
             *) exit 0 ;;\n\
             esac\n",
        );
        let err = validate_binary(&path).expect_err("a broken config must abort the upgrade");
        assert!(matches!(err, UpgradeError::Validation(_)), "got {err:?}");
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

    #[tokio::test]
    async fn pane_handoff_aborts_when_actor_mailbox_is_missing() {
        let (upgrade, receiver) = mpsc::channel(1);
        drop(receiver);

        let result = request_pane_handoff(phux_core::ids::ResourceId::default(), &upgrade).await;

        assert!(matches!(
            result,
            Err(UpgradeError::PaneHandoff {
                reason: "actor mailbox closed",
                ..
            })
        ));
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
