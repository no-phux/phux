//! The graceful-upgrade state blob's producer and consumer (ADR-0032):
//! [`ServerState::build_upgrade_blob`] walks the live tree into a
//! [`StateBlob`](crate::upgrade::blob::StateBlob), and
//! [`ServerState::rebuild_from_blob`] reconstructs the tree from one in the
//! re-exec'd image.
//!
//! The blob carries the organization records ([`OrgBlob`]) and, for each
//! carried session, pane, and agent session, its wire id and record key, so
//! the resumed image rebinds the same records to new handles under the same
//! wire ids. Windows are synthesized (one per session with live panes) for
//! older readers. A blob without records, from an image that predates them,
//! is imported: its sessions become records marked awaiting organization,
//! each window becomes a group of the session's arrangement, and each live
//! pane becomes a resource bound to its running process as activation 1.

use phux_resource::agent_session::AgentSessionResource as _;
use std::collections::{HashMap, HashSet};
use std::os::fd::{AsRawFd, RawFd};
use std::path::PathBuf;

use phux_core::ids::{ResourceId, SessionId};
use phux_core::terminal::TerminalFacet;
use phux_core::{
    Activation, ActivationId, AgentLaunch, Arrangement, ArrangementKey, ArrangementOwner, EntryRef,
    ExecState, ExitPolicy, FreshKeys, Group, LaunchSpec, LayoutNode, OperationId, Org, Provenance,
    Resource, ResourceKey, ResourceKind, Revision, Session, SessionKey, SessionLifetime, SplitDir,
    Stamp, TerminalLaunch,
};
use phux_protocol::ids::{
    ResourceId as WireResourceId, SessionId as WireSessionId, WindowId as WireWindowId,
};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::ServerState;
use crate::resource::ResourceHandle;
use crate::resource::agent_session::{AgentSessionActor, AgentSessionBootstrap, BootstrapRequest};
use crate::resource::handoff::{PaneUpgradeHandle, UpgradeHandleRequest};
use crate::upgrade::blob::{
    AgentSessionBlob, BLOB_VERSION, Counters, LayoutBlob, MetadataBlob, OrgBlob, PaneBlob,
    RetainedExitBlob, SessionBlob, SplitDirBlob, StateBlob, WindowBlob,
};

/// What the reversible half of an upgrade collected from live engines: each
/// carried pane's PTY handoff and each carried agent session's stream cut.
#[derive(Debug, Default)]
pub(crate) struct UpgradeHandoffs {
    /// PTY handoff + replay snapshot per carried pane.
    pub(crate) panes: HashMap<ResourceId, PaneUpgradeHandle>,
    /// Record-stream cut per carried `AgentSession`. A session without one
    /// is not carried.
    pub(crate) agent_sessions: HashMap<ResourceId, AgentSessionBootstrap>,
}

/// One pane an app rebuilt around its inherited state, ready to register.
pub(crate) struct RebuiltPane<W> {
    /// The engine's handle.
    pub(crate) handle: ResourceHandle,
    /// The engine's cancellation token.
    pub(crate) token: tokio_util::sync::CancellationToken,
    /// The engine, not yet running.
    pub(crate) engine: std::pin::Pin<Box<dyn std::future::Future<Output = ()>>>,
    /// Fires when the engine's child exits.
    pub(crate) exit_notify: Option<tokio::sync::oneshot::Receiver<phux_core::ExitOutcome>>,
    /// What the caller's wiring returned.
    pub(crate) wired: W,
}

/// Errors rebuilding a [`ServerState`] from a [`StateBlob`].
#[derive(Debug, thiserror::Error)]
pub enum RebuildError {
    /// The carried records break an organization invariant.
    #[error("organization rebuild: {0}")]
    Organization(#[from] phux_core::InvariantViolation),
    /// The store refused the carried or imported records.
    #[error("organization store: {0}")]
    Store(#[from] crate::org_store::StoreError),
    /// A pane's actor could not be rebuilt around its adopted PTY.
    #[error("actor rebuild: {0}")]
    Actor(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// The blob references a wire id that no earlier entity defined (e.g. a
    /// window naming a session not in the blob), or a key no record has.
    #[error("blob references unknown {kind} wire id {id}")]
    DanglingRef {
        /// Which kind of entity the dangling id was expected to name.
        kind: &'static str,
        /// The unresolved wire id.
        id: u32,
    },
    /// A PTY-backed pane must carry both inherited identities.
    #[error("pane wire id {wire_id} has only one of master fd and child pid")]
    InvalidPtyPair {
        /// Stable pane identity from the handoff blob.
        wire_id: u32,
    },
    /// The blob lists the same wire id twice for one kind of entity.
    #[error("blob repeats {kind} wire id {id}")]
    DuplicateId {
        /// Which kind of entity repeated.
        kind: &'static str,
        /// The repeated wire id.
        id: u32,
    },
}

impl ServerState {
    /// Assemble a [`StateBlob`] from the live tree for a graceful upgrade,
    /// asking each pane actor for its PTY descriptors and replay snapshot.
    /// Runs inside the actors' `LocalSet`; an unreachable pane is recorded
    /// without a handoff.
    #[allow(
        clippy::future_not_send,
        reason = "ADR-0014: the server runs on a LocalSet; pane actors are reached through !Send local futures"
    )]
    pub async fn build_upgrade_blob(&mut self, listener_fd: RawFd) -> StateBlob {
        self.intern_upgrade_wires();
        let mut handoffs = UpgradeHandoffs::default();
        let tids: Vec<ResourceId> = self
            .upgrade_handles()
            .into_iter()
            .map(|(tid, _)| tid)
            .collect();
        for tid in tids {
            if let Some(handoff) = self.request_pane_handoff(tid).await {
                handoffs.panes.insert(tid, handoff);
            }
        }
        for (session, bootstrap) in self.upgrade_agent_sessions() {
            // Not sealed: this walk re-execs nothing, so the old image
            // must keep accepting appends.
            if let Some(cut) = request_agent_session_cut(&bootstrap, false).await {
                handoffs.agent_sessions.insert(session, cut);
            }
        }
        self.with_upgrade_metadata(self.assemble_upgrade_blob(listener_fd, &handoffs))
    }

    /// Attach the carried L3 metadata to an assembled blob. Kept out of
    /// [`Self::assemble_upgrade_blob`] so a layout write during the handoff
    /// does not read as a tree change and abort the upgrade.
    pub(crate) fn with_upgrade_metadata(&self, mut blob: StateBlob) -> StateBlob {
        blob.metadata = self
            .metadata
            .upgrade_entries()
            .map(|(group, key, value)| MetadataBlob {
                group: group.map(phux_protocol::ids::GroupId::get),
                key: key.to_owned(),
                value: value.to_vec(),
            })
            .collect();
        // Controller ledgers change with SDK traffic, not organization shape.
        // Capture them at the sealed cut, never in the tree-identity comparison.
        blob.native_agents = self.native_agents.bindings.values().cloned().collect();
        blob
    }

    /// Intern a wire id for every session, synthetic window, and live
    /// resource, so the blob names each one. Run under the lock that
    /// captures the tree identity.
    pub(crate) fn intern_upgrade_wires(&mut self) {
        let sessions: Vec<SessionId> = self
            .sessions
            .registry
            .sessions()
            .map(|(id, _)| id)
            .collect();
        for sid in sessions {
            let _ = self.idspace.intern_session(sid);
            if !self.legacy_panes_of(sid).is_empty() {
                let _ = self.intern_window_wire(sid);
            }
        }
        for id in self.resource_ids() {
            if self.sessions.registry.resource(id).is_some() {
                let _ = self.intern_resource_wire(id);
            }
        }
    }

    /// Record the upgrade context (listener fd, path, runtime flags).
    pub(crate) fn set_upgrade_context(
        &mut self,
        listener_fd: RawFd,
        socket_path: PathBuf,
        flags: crate::runtime::RuntimeFlags,
    ) {
        self.lifecycle
            .set_upgrade_context(listener_fd, socket_path, flags);
    }

    /// The upgrade context `(listener_fd, socket_path, runtime_flags)`, if
    /// serving has begun.
    pub(crate) fn upgrade_context(
        &self,
    ) -> Option<(RawFd, &std::path::Path, crate::runtime::RuntimeFlags)> {
        self.lifecycle.upgrade_context()
    }

    /// The handles of exactly the panes whose PTY a re-exec carries: live
    /// Terminals with wire ids (the same walk as
    /// [`Self::assemble_upgrade_blob`]). Everything else in the resource
    /// table is left alone: an `AgentSession` has no PTY and no upgrade
    /// mailbox (it crosses through [`Self::upgrade_agent_sessions`]), and a
    /// retained pane's PTY does not cross (ADR-0124 §6). Asking those would
    /// only let an engine with nothing to hand off abort the upgrade.
    pub(crate) fn upgrade_handles(&self) -> Vec<(ResourceId, ResourceHandle)> {
        let carried = self.carried_panes();
        self.warn_orphan_resource_handles();
        carried
    }

    /// The `AgentSession`s a re-exec carries, each with its bootstrap
    /// mailbox: every session whose parent is a pane
    /// [`Self::upgrade_handles`] carries. A session under a retained or
    /// uncarried pane is dropped, as its parent is.
    pub(crate) fn upgrade_agent_sessions(
        &self,
    ) -> Vec<(ResourceId, mpsc::Sender<BootstrapRequest>)> {
        let parents: HashSet<ResourceId> = self
            .carried_panes()
            .into_iter()
            .map(|(tid, _)| tid)
            .collect();
        let mut carried: Vec<(ResourceId, mpsc::Sender<BootstrapRequest>)> = self
            .resource_ids()
            .into_iter()
            .filter(|id| self.sessions.registry.resource(*id).is_some())
            .filter_map(|id| {
                let handle = self.resource_handle(id)?;
                let session = handle.agent_session().ok()?;
                handle
                    .parent
                    .filter(|parent| parents.contains(parent))
                    .map(|_| (id, session.bootstrap.clone()))
            })
            .collect();
        carried.sort_by_key(|(id, _)| self.resource_wire(*id));
        carried
    }

    /// Live Terminals with wire ids, in session then membership order,
    /// with their handles.
    fn carried_panes(&self) -> Vec<(ResourceId, ResourceHandle)> {
        let mut sessions: Vec<SessionId> = self
            .sessions
            .registry
            .sessions()
            .map(|(id, _)| id)
            .collect();
        sessions.sort_by_key(|sid| self.session_wire(*sid));
        sessions
            .into_iter()
            .filter(|sid| self.session_wire(*sid).is_some())
            .flat_map(|sid| self.legacy_panes_of(sid))
            .filter(|tid| self.resource_wire(*tid).is_some() && self.retained_exit(*tid).is_none())
            .filter_map(|tid| {
                self.resource_handle(tid)
                    .filter(|handle| {
                        crate::runtime::apps::app_for(handle.kind)
                            .is_some_and(|app| app.upgrade_sender(handle).is_some())
                    })
                    .map(|handle| (tid, handle.clone()))
            })
            .collect()
    }

    /// A handle whose resource left the registry is a leak: its engine is
    /// unreachable through the tree and nothing will reap it. Name each one
    /// so a leak path shows up in the log instead of in a failed upgrade.
    fn warn_orphan_resource_handles(&self) {
        for tid in self.resource_ids() {
            if self.sessions.registry.resource(tid).is_none() {
                tracing::warn!(
                    resource = ?tid,
                    "upgrade: resource handle outlived its registry entry; not carried"
                );
            }
        }
    }
}

/// Build one [`PaneBlob`], preferring the actor's live values and falling back
/// to the descriptor when there is no handoff.
fn pane_blob(
    wire_id: u32,
    window_wire_id: u32,
    desc: &TerminalFacet,
    term: &str,
    handoff: Option<&PaneUpgradeHandle>,
    retained_exit: Option<RetainedExitBlob>,
    retain_secs: Option<u32>,
) -> PaneBlob {
    let (cols, rows) = handoff.as_ref().map_or(desc.dims, |h| (h.cols, h.rows));
    PaneBlob {
        wire_id,
        window_wire_id,
        cols,
        rows,
        cell_px: handoff.as_ref().and_then(|h| h.cell_px),
        cwd: handoff
            .as_ref()
            .and_then(|h| h.cwd.as_deref())
            .map_or_else(|| desc.cwd.clone(), PathBuf::from),
        // The user-set title only: the program's OSC title is engine state
        // and rides separately, so an upgrade never promotes it to a name.
        title: desc.title.clone(),
        osc_title: handoff.as_ref().and_then(|h| h.osc_title.clone()),
        term: term.to_owned(),
        child_pid: handoff.as_ref().and_then(|h| h.child_pid),
        master_fd: handoff.and_then(|h| h.master_fd.as_ref().map(AsRawFd::as_raw_fd)),
        vt_replay_bytes: handoff
            .as_ref()
            .map(|h| h.vt_replay_bytes.clone())
            .unwrap_or_default(),
        scrollback_bytes: handoff
            .map(|h| h.scrollback_bytes.clone())
            .unwrap_or_default(),
        retained_exit,
        retain_secs,
        key: None,
    }
}

/// Cut one agent session's record stream for the upgrade, sealing it there
/// when `seal` (see [`BootstrapRequest::seal`]). `None` when the engine is
/// gone: the session is then simply not carried.
pub(crate) async fn request_agent_session_cut(
    bootstrap: &mpsc::Sender<BootstrapRequest>,
    seal: bool,
) -> Option<AgentSessionBootstrap> {
    let (reply, rx) = oneshot::channel();
    let request = BootstrapRequest {
        reply,
        seal: seal.then_some(true),
        after_seq: None,
    };
    bootstrap.send(request).await.ok()?;
    rx.await.ok()
}

/// The stream cut a resumed engine continues from, as the blob carried it.
fn agent_session_cut(carried: &AgentSessionBlob) -> AgentSessionBootstrap {
    AgentSessionBootstrap {
        base_seq: carried.base_seq,
        resume_after_seq: None,
        records: carried
            .records
            .iter()
            .map(|record| bytes::Bytes::from(record.clone()))
            .collect(),
        dropped: carried.dropped,
        ended: carried.ended,
        native_publications: carried.native_publications.clone(),
        native_publication_high_water: carried.native_publication_high_water,
    }
}

/// A retained pane's exit facet as the upgrade blob carries it.
const fn exit_blob(facet: phux_protocol::wire::info::ExitFacet) -> RetainedExitBlob {
    RetainedExitBlob {
        exit_status: facet.exit_status,
        signal: facet.signal,
        exited_at_ms: facet.exited_at_ms,
        retained_until_ms: facet.retained_until_ms,
    }
}

/// The exit facet a resumed image restores from the blob.
const fn exit_facet(blob: RetainedExitBlob) -> phux_protocol::wire::info::ExitFacet {
    phux_protocol::wire::info::ExitFacet::new(blob.exited_at_ms, blob.retained_until_ms)
        .with_exit_status(blob.exit_status)
        .with_signal(blob.signal)
}

/// Each rebuilt resource's (pane or agent session) core id and the one-shot
/// exit receiver the runtime restores its lifecycle watcher from.
type PaneExitWatchers = Vec<(
    ResourceId,
    oneshot::Receiver<phux_core::process::ExitOutcome>,
)>;

/// Concatenate a pane's scrollback then viewport replay — the order a client
/// applies them, so seeding a fresh `Terminal` reproduces the same grid —
/// then the program's OSC title as an `OSC 2`, so the rebuilt engine reports
/// the title the program set rather than none.
pub(crate) fn pane_seed(p: &PaneBlob) -> Vec<u8> {
    let mut seed = Vec::with_capacity(p.scrollback_bytes.len() + p.vt_replay_bytes.len());
    seed.extend_from_slice(&p.scrollback_bytes);
    seed.extend_from_slice(&p.vt_replay_bytes);
    if let Some(title) = p.osc_title.as_deref() {
        seed.extend_from_slice(&osc_title_sequence(title));
    }
    seed
}

/// `OSC 2 ; title ST`, with control characters dropped so a title can
/// neither terminate the sequence early nor smuggle another one in.
fn osc_title_sequence(title: &str) -> Vec<u8> {
    let clean: String = title.chars().filter(|c| !c.is_control()).collect();
    let mut seq = Vec::with_capacity(clean.len() + 6);
    seq.extend_from_slice(b"\x1b]2;");
    seq.extend_from_slice(clean.as_bytes());
    seq.extend_from_slice(b"\x1b\\");
    seq
}

#[cfg(test)]
mod tests;

mod assemble;
mod import;
mod rebuild;
mod validate;

#[allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "re-export for siblings"
)]
use assemble::*;
#[allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "re-export for siblings"
)]
use import::*;
#[allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "re-export for siblings"
)]
pub(crate) use rebuild::dry_run_records;
#[allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "re-export for siblings"
)]
use rebuild::*;
pub(crate) use validate::validate_upgrade_blob;
