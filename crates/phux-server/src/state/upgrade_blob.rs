//! The graceful-upgrade state blob's tree (ADR-0032):
//! [`ServerState::assemble_upgrade_blob`] walks the live sessions, windows and
//! panes into a [`StateBlob`](crate::upgrade::blob::StateBlob), and
//! [`ServerState::rebuild_from_blob`] reconstructs that tree in the re-exec'd
//! image. What a kind's engines carry is its app's: each app writes its own
//! part of the blob at the cut and rebuilds it through a [`KindRebuild`]
//! (`runtime::apps`), so this module names no engine.
//!
//! The blob carries the organization records ([`OrgBlob`]) and, for each
//! carried session and resource, its wire id and record key, so the resumed
//! image rebinds the same records to new handles under the same wire ids.
//! Windows are synthesized (one per session with live panes) for older
//! readers. A blob without records, from an image that predates them, is
//! imported: its sessions become records marked awaiting organization, each
//! window becomes a group of the session's arrangement, and each live pane
//! becomes a resource bound to its running process as activation 1.

use std::collections::{HashMap, HashSet};
use std::os::fd::RawFd;
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
use tokio::sync::oneshot;

use super::ServerState;
use crate::resource::ResourceHandle;
use crate::upgrade::blob::{
    BLOB_VERSION, Counters, LayoutBlob, MetadataBlob, OrgBlob, PaneBlob, RetainedExitBlob,
    SessionBlob, SplitDirBlob, StateBlob, WindowBlob,
};

/// Errors rebuilding a [`ServerState`] from a [`StateBlob`].
#[derive(Debug, thiserror::Error)]
pub enum RebuildError {
    /// A rebuilt engine does not match the resource record it would own.
    #[error("engine registration: {0}")]
    Registration(#[from] super::ResourceRegistrationError),
    /// The carried records break an organization invariant.
    #[error("organization rebuild: {0}")]
    Organization(#[from] phux_core::InvariantViolation),
    /// The store refused the carried or imported records.
    #[error("organization store: {0}")]
    Store(#[from] crate::org_store::StoreError),
    /// An engine could not be rebuilt around what its app carried.
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

    /// The live tree panes a re-exec carries, with their handles: Terminal
    /// members with wire ids, in the walk [`Self::assemble_upgrade_blob`]
    /// makes ([`Self::carried_panes`]), less retained panes, whose PTY does
    /// not cross (ADR-0124 §6). The Terminal app asks exactly these for a
    /// handoff; other kinds cross through their own apps. Names every handle
    /// that outlived its registry entry on the way.
    pub(crate) fn upgrade_handles(&self) -> Vec<(ResourceId, ResourceHandle)> {
        let carried = self.carried_panes();
        self.warn_orphan_resource_handles();
        carried
    }

    /// Live Terminals with wire ids, in session then membership order,
    /// with their handles.
    pub(crate) fn carried_panes(&self) -> Vec<(ResourceId, ResourceHandle)> {
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
            .filter_map(|tid| Some((tid, self.resource_handle(tid)?.clone())))
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

/// One [`PaneBlob`] from the registry alone. The pane's engine state (its
/// PTY, live size, replay) is its app's to add at the cut.
fn pane_blob(
    wire_id: u32,
    window_wire_id: u32,
    desc: &TerminalFacet,
    term: &str,
    retained_exit: Option<RetainedExitBlob>,
    retain_secs: Option<u32>,
) -> PaneBlob {
    PaneBlob {
        wire_id,
        window_wire_id,
        cols: desc.dims.0,
        rows: desc.dims.1,
        cell_px: None,
        cwd: desc.cwd.clone(),
        // The user-set title only: the program's OSC title is engine state
        // and rides separately, so an upgrade never promotes it to a name.
        title: desc.title.clone(),
        osc_title: None,
        term: term.to_owned(),
        child_pid: None,
        master_fd: None,
        vt_replay_bytes: Vec::new(),
        scrollback_bytes: Vec::new(),
        retained_exit,
        retain_secs,
        key: None,
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

/// The wire id of every resource `blob` carries. They share one id space.
pub(crate) fn resource_wires(blob: &StateBlob) -> impl Iterator<Item = u32> + '_ {
    blob.panes
        .iter()
        .map(|p| p.wire_id)
        .chain(blob.agent_sessions.iter().map(|a| a.wire_id))
        .chain(blob.documents.iter().map(|d| d.wire_id))
}

/// The panes `blob` carries live. Any other carried resource's parent is
/// one: a retained pane's children do not cross.
pub(crate) fn live_pane_wires(blob: &StateBlob) -> HashSet<u32> {
    blob.panes
        .iter()
        .filter(|pane| pane.retained_exit.is_none())
        .map(|pane| pane.wire_id)
        .collect()
}

/// Each rebuilt resource's core id and the one-shot exit receiver the
/// runtime restores its lifecycle watcher from.
pub(crate) type ExitWatchers = Vec<(
    ResourceId,
    oneshot::Receiver<phux_core::process::ExitOutcome>,
)>;

#[cfg(test)]
mod tests;

mod assemble;
mod import;
mod rebuild;
mod resume;
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
use rebuild::*;
#[allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "re-export for siblings"
)]
pub(crate) use rebuild::{RebuiltTree, dry_run_records};
pub(crate) use resume::{KindRebuild, Missing, ResumeCx};
pub(crate) use validate::validate_upgrade_blob;
