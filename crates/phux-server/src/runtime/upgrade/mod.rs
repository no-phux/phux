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

use tokio::sync::mpsc;

use super::RuntimeFlags;
use crate::resource::agent_session::BootstrapRequest;
use crate::resource::handoff::{PaneUpgradeHandle, UpgradeHandleRequest};
use crate::state::{SharedState, UpgradeHandoffs};
use crate::upgrade::blob::StateBlob;

mod handoff;
#[cfg(test)]
mod tests;
pub(super) mod validate;

use handoff::{AgentSessionSeal, collect_agent_session_cuts, collect_pane_handoffs};
pub(super) use validate::{InheritedUpgradeEnv, cleanup_executable_snapshot};
use validate::{PinnedExecutable, clear_inherited_cloexec, pin_validated_executable};

const PANE_HANDOFF_TIMEOUT: Duration = Duration::from_secs(2);
const AGENT_ROLLBACK_TIMEOUT: Duration = Duration::from_secs(2);
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
    /// The resumed image would start with `--hub` and fail to validate the
    /// on-disk satellite registry after the irreversible exec, so the
    /// upgrade is refused while the panes are still ours.
    #[error("satellite registry would fail hub startup after upgrade: {0}")]
    HubRegistry(String),
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
    /// SDK-owned streams are part of a durable provider session, so unlike
    /// optional attached streams they may never be dropped at an upgrade cut.
    #[error("native agent {agent:?} did not provide its mandatory upgrade cut")]
    NativeAgentHandoff {
        agent: phux_protocol::ids::ResourceId,
    },
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
    fd_flags: FdFlagsGuard,
    _blob_file: std::fs::File,
    _listener_fd: OwnedFd,
    /// Each carried delegated socket's duplicated listener (ADR-0163 §5).
    _delegated_listeners: Vec<OwnedFd>,
    _handoffs: HashMap<phux_core::ids::ResourceId, PaneUpgradeHandle>,
    /// Lifted only if `exec` returns, so the old image keeps streaming.
    agent_seal: AgentSessionSeal,
    _native_seal: Option<NativeBrokerSeal>,
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
    /// Bootstrap mailbox of every agent session the upgrade carries.
    agent_session_senders: Vec<(phux_core::ids::ResourceId, mpsc::Sender<BootstrapRequest>)>,
}

/// Stop native input/receipt mutation across the sealed upgrade cut. The
/// durable SDK keeps draining into its bounded output/receipt spool; rollback
/// resumes this broker, while successful exec replaces it with restored state.
struct NativeBrokerSeal(SharedState);
impl NativeBrokerSeal {
    fn new(state: &SharedState) -> Result<Self, UpgradeError> {
        let admitted = state.with_mut(|s| {
            let broker = s.native_agents_mut();
            if broker.sealed {
                return false;
            }
            broker.sealed = true;
            true
        });
        admitted
            .then(|| Self(state.clone()))
            .ok_or(UpgradeError::TreeChanged)
    }
}
impl Drop for NativeBrokerSeal {
    fn drop(&mut self) {
        self.0.with_mut(|s| {
            s.native_agents_mut().sealed = false;
            for wake in s.native_agents().wakes.values() {
                wake.notify_waiters();
            }
        });
    }
}

/// Do everything reversible: snapshot the tree into a handoff blob, stage it in
/// an inheritable temp file, clear `FD_CLOEXEC` on the blob / listener / every
/// pane master, and validate the on-disk binary. Returns a [`UpgradePlan`] the
/// caller execs *after* acking the client.
pub(super) async fn prepare_upgrade(state: &SharedState) -> Result<UpgradePlan, UpgradeError> {
    // Sidecars stop before anything irreversible. A failure below starts
    // them again; a successful exec replaces the process, and the resumed
    // image starts them after accept. They are not in the state blob.
    crate::plugin_startup::pause(state).await;
    match prepare_upgrade_inner(state).await {
        Ok(plan) => Ok(plan),
        Err(err) => {
            crate::plugin_startup::resume(state);
            Err(err)
        }
    }
}

async fn prepare_upgrade_inner(state: &SharedState) -> Result<UpgradePlan, UpgradeError> {
    let native_seal = NativeBrokerSeal::new(state)?;
    let UpgradeContext {
        listener_fd,
        socket_path,
        flags,
        tree_identity,
        pane_senders,
        agent_session_senders,
    } = capture_upgrade_context(state)?;
    refuse_invalid_hub_registry(state, &flags)?;

    let listener = dup_listener(listener_fd)?;
    let (panes, (agent_sessions, mut agent_seal)) = tokio::join!(
        collect_pane_handoffs(pane_senders, PANE_HANDOFF_TIMEOUT),
        collect_agent_session_cuts(agent_session_senders, PANE_HANDOFF_TIMEOUT),
    );
    let staged: Result<_, UpgradeError> = async {
        let handoffs = UpgradeHandoffs {
            panes: panes?,
            agent_sessions,
        };
        if let Some(agent) = state.with(|s| {
            s.native_agents()
                .bindings
                .values()
                .filter(|binding| !binding.closed)
                .find(|binding| {
                    s.resource_from_wire(&binding.agent)
                        .is_none_or(|id| !handoffs.agent_sessions.contains_key(&id))
                })
                .map(|binding| binding.agent.clone())
        }) {
            return Err(UpgradeError::NativeAgentHandoff { agent });
        }
        let blob = reassemble_unchanged_tree(
            state,
            listener_fd,
            listener.as_raw_fd(),
            &tree_identity,
            &handoffs,
        )?;
        let (blob, delegated_listeners) = super::delegation::carry_delegations(state, blob)?;

        let blob_file = stage_blob_file(&blob)?;
        let blob_fd = blob_file.as_raw_fd();
        let executable = pin_validated_executable(flags.upgrade_source_exe.as_deref())?;
        let fd_flags = clear_inherited_cloexec(blob_fd, listener_fd, &blob)?;
        Ok((
            handoffs,
            blob_file,
            blob_fd,
            executable,
            fd_flags,
            delegated_listeners,
        ))
    }
    .await;

    let (handoffs, blob_file, blob_fd, executable, fd_flags, delegated_listeners) = match staged {
        Ok(parts) => parts,
        Err(error) => {
            // Await responsive actors before reporting the failure (#1035).
            // Drop retains responsibility for actors that miss rollback's
            // deadline, including ones that never answered their initial cut.
            agent_seal.lift().await;
            return Err(error);
        }
    };

    Ok(UpgradePlan {
        executable,
        blob_fd,
        socket_path,
        flags,
        fd_flags,
        _blob_file: blob_file,
        _listener_fd: listener,
        _delegated_listeners: delegated_listeners,
        _handoffs: handoffs.panes,
        agent_seal,
        _native_seal: Some(native_seal),
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
                    flags: served_flags(flags, s.hub_has_links()),
                    tree_identity: s
                        .assemble_upgrade_blob(listener_fd, &UpgradeHandoffs::default()),
                    pane_senders: s
                        .upgrade_handles()
                        .into_iter()
                        .filter_map(|(pane, handle)| {
                            Some((
                                pane,
                                crate::runtime::apps::app_for(handle.kind)?
                                    .upgrade_sender(&handle)?,
                            ))
                        })
                        .collect(),
                    agent_session_senders: s.upgrade_agent_sessions(),
                })
        })
        .ok_or(UpgradeError::NoContext)
}

/// The flags the resumed image must serve: the builder's, plus `--hub` when
/// a config-reload doorbell promoted this server into a hub after boot
/// (phux-lpn7.3), so the re-exec keeps dialing its satellites instead of
/// waiting for the next doorbell.
const fn served_flags(mut flags: RuntimeFlags, promoted_hub: bool) -> RuntimeFlags {
    flags.hub |= promoted_hub;
    flags
}

/// A `--hub` resume validates `[[satellites]]` at startup and exits on a
/// malformed or duplicate entry, after exec has already consumed the running
/// image. The live doorbell only warns and keeps its table, so check here,
/// before anything irreversible. No source (an embedder supplied none) means
/// nothing to re-read.
fn refuse_invalid_hub_registry(
    state: &SharedState,
    flags: &RuntimeFlags,
) -> Result<(), UpgradeError> {
    if !flags.hub {
        return Ok(());
    }
    let Some(source) = state.with(crate::state::ServerState::hub_satellite_source) else {
        return Ok(());
    };
    source.validate().map_err(UpgradeError::HubRegistry)
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
    handoffs: &UpgradeHandoffs,
) -> Result<StateBlob, UpgradeError> {
    state
        .with(|s| {
            let current = s.assemble_upgrade_blob(listener_fd, &UpgradeHandoffs::default());
            (current == *tree_identity)
                .then(|| s.with_upgrade_metadata(s.assemble_upgrade_blob(inherited_fd, handoffs)))
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

impl UpgradePlan {
    /// Re-exec the new binary with [`resume_args`], replacing this process.
    /// Returns only on failure, after restoring descriptor flags and awaiting
    /// the same bounded stream rollback as a failed preparation.
    pub(super) async fn exec(mut self) -> std::io::Error {
        let error = self.try_exec();
        drop(self.fd_flags);
        self.agent_seal.lift().await;
        error
    }

    fn try_exec(&self) -> std::io::Error {
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
    if let Some(path) = flags.autosave {
        args.push(OsString::from("--autosave"));
        args.push(path.into_os_string());
    }
    if let Some(idle) = flags.exit_after_idle {
        // Whole seconds, rounded up: never `--exit-after-idle 0`.
        let secs = idle.as_secs() + u64::from(idle.subsec_nanos() > 0);
        args.push(OsString::from("--exit-after-idle"));
        args.push(OsString::from(secs.to_string()));
    }
    args
}
