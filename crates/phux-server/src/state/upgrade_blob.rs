//! The graceful-upgrade state blob's producer and consumer (ADR-0032):
//! [`ServerState::build_upgrade_blob`] walks the live tree into a
//! [`StateBlob`](crate::upgrade::blob::StateBlob), and
//! [`ServerState::rebuild_from_blob`] reconstructs the tree from one in the
//! re-exec'd image.

use std::collections::{HashMap, HashSet};
use std::os::fd::{AsRawFd, RawFd};
use std::path::PathBuf;
use std::time::{Duration, UNIX_EPOCH};

use phux_core::ids::{ResourceId, SessionId, WindowId};
use phux_core::terminal::TerminalFacet;
use phux_core::window::{LayoutNode, SplitDir};
use phux_protocol::ids::{
    ResourceId as WireResourceId, SessionId as WireSessionId, WindowId as WireWindowId,
};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::ServerState;
use crate::resource::ResourceHandle;
use crate::resource::agent_session::{AgentSessionActor, AgentSessionBootstrap, BootstrapRequest};
use crate::terminal_actor::{PaneUpgradeHandle, TerminalActor, UpgradeHandleRequest};
use crate::upgrade::blob::{
    AgentSessionBlob, BLOB_VERSION, Counters, LayoutBlob, PaneBlob, RetainedExitBlob, SessionBlob,
    SplitDirBlob, StateBlob, WindowBlob,
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

/// Errors rebuilding a [`ServerState`] from a [`StateBlob`].
#[derive(Debug, thiserror::Error)]
pub enum RebuildError {
    /// The registry rejected a session/window/pane insertion.
    #[error("registry rebuild: {0}")]
    Registry(#[from] phux_core::registry::RegistryError),
    /// A pane's actor could not be rebuilt around its adopted PTY.
    #[error("actor rebuild: {0}")]
    Actor(#[from] crate::terminal_actor::TerminalActorError),
    /// The blob references a wire id that no earlier entity defined (e.g. a
    /// window naming a session not in the blob).
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
    pub async fn build_upgrade_blob(&self, listener_fd: RawFd) -> StateBlob {
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
        self.assemble_upgrade_blob(listener_fd, &handoffs)
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
    /// Terminals in the serializable session tree (the same walk as
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
        carried.sort_by_key(|(id, _)| self.terminal_wire(*id));
        carried
    }

    /// Live Terminals in the serializable session tree, with their handles.
    fn carried_panes(&self) -> Vec<(ResourceId, ResourceHandle)> {
        self.sessions
            .registry
            .sessions()
            .filter(|(sid, _)| self.session_wire(*sid).is_some())
            .flat_map(|(_, session)| session.windows.iter())
            .filter(|wid| self.window_wire(**wid).is_some())
            .filter_map(|wid| self.sessions.registry.window(*wid))
            .flat_map(|window| window.slots.iter().copied())
            .filter(|tid| {
                self.sessions.registry.terminal(*tid).is_some()
                    && self.terminal_wire(*tid).is_some()
                    && self.retained_exit(*tid).is_none()
            })
            .filter_map(|tid| {
                self.resource_handle(tid)
                    .filter(|handle| handle.kind == phux_core::resource::ResourceKind::Terminal)
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

    /// Assemble the blob from the live tree plus pre-fetched handoffs,
    /// under the lock. Agent sessions come only from their cuts, so a blob
    /// assembled with no handoffs is the tree's identity alone: a busy
    /// agent's appends never make it look changed.
    pub(crate) fn assemble_upgrade_blob(
        &self,
        listener_fd: RawFd,
        handoffs: &UpgradeHandoffs,
    ) -> StateBlob {
        let mut sessions = Vec::new();
        let mut windows = Vec::new();
        let mut panes = Vec::new();

        for (sid, session) in self.sessions.registry.sessions() {
            let Some(session_wire) = self.session_wire(sid) else {
                continue;
            };
            sessions.push(SessionBlob {
                wire_id: session_wire,
                name: session.name.clone(),
                window_wire_ids: session
                    .windows
                    .iter()
                    .filter_map(|w| self.window_wire(*w))
                    .collect(),
                active_window: session.active.and_then(|w| self.window_wire(w)),
                created_at_unix_nanos: session
                    .created_at
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |duration| duration.as_nanos()),
                last_touched: self.sessions.last_touched_at(sid),
                root: self.sessions.root(sid).cloned(),
                keep_empty: session.keep_empty,
            });

            for &wid in &session.windows {
                let (Some(window), Some(window_wire)) =
                    (self.sessions.registry.window(wid), self.window_wire(wid))
                else {
                    continue;
                };
                windows.push(WindowBlob {
                    wire_id: window_wire,
                    session_wire_id: session_wire,
                    pane_wire_ids: window
                        .slots
                        .iter()
                        .filter_map(|t| self.terminal_wire(*t))
                        .collect(),
                    active_resource: window.active.and_then(|t| self.terminal_wire(t)),
                    layout: window.layout.as_ref().and_then(|l| self.layout_to_blob(l)),
                    last_cwd: self.sessions.last_cwd(wid).cloned(),
                });

                for &tid in &window.slots {
                    let (Some(desc), Some(pane_wire)) = (
                        self.sessions.registry.terminal(tid),
                        self.terminal_wire(tid),
                    ) else {
                        continue;
                    };
                    // ADR-0124 §6: a retained pane's PTY does not cross; a
                    // live pane's retention request does.
                    let retained = self.retained_exit(tid).map(exit_blob);
                    panes.push(pane_blob(
                        pane_wire,
                        window_wire,
                        desc,
                        &self.config.term,
                        handoffs.panes.get(&tid).filter(|_| retained.is_none()),
                        retained,
                        self.retain_request(tid),
                    ));
                }
            }
        }

        let live_panes: HashSet<u32> = panes
            .iter()
            .filter(|pane| pane.retained_exit.is_none())
            .map(|pane| pane.wire_id)
            .collect();
        let agent_sessions = self.agent_session_blobs(&handoffs.agent_sessions, &live_panes);

        StateBlob {
            version: BLOB_VERSION,
            listener_fd,
            counters: Counters {
                next_session_wire_id: self.idspace.next_session_wire(),
                next_terminal_wire_id: self.idspace.next_terminal_wire(),
                next_window_wire_id: self.idspace.next_window_wire(),
                next_touch_timestamp: self.sessions.next_touch_timestamp(),
                server_instance: Some(*self.idspace.instance().as_bytes()),
            },
            sessions,
            windows,
            panes,
            agent_sessions,
        }
    }

    /// One [`AgentSessionBlob`] per cut whose session is still registered
    /// under a pane that crosses live, in wire-id order. A session that
    /// closed, or whose parent stopped crossing, since its cut is dropped.
    fn agent_session_blobs(
        &self,
        cuts: &HashMap<ResourceId, AgentSessionBootstrap>,
        live_panes: &HashSet<u32>,
    ) -> Vec<AgentSessionBlob> {
        let mut blobs: Vec<AgentSessionBlob> = cuts
            .iter()
            .filter_map(|(&id, cut)| {
                let desc = self.sessions.registry.resource(id)?;
                let facet = desc.agent.as_ref()?;
                let parent_wire_id = self.terminal_wire(desc.parent?)?;
                if !live_panes.contains(&parent_wire_id) {
                    return None;
                }
                Some(AgentSessionBlob {
                    wire_id: self.terminal_wire(id)?,
                    parent_wire_id,
                    provider: facet.provider.clone(),
                    native_id: facet.native_id.clone(),
                    state: facet.state.clone(),
                    base_seq: cut.base_seq,
                    dropped: cut.dropped,
                    ended: cut.ended,
                    // Stamped records are canonical JSON, so always UTF-8.
                    records: cut
                        .records
                        .iter()
                        .map(|record| String::from_utf8_lossy(record).into_owned())
                        .collect(),
                })
            })
            .collect();
        blobs.sort_by_key(|blob| blob.wire_id);
        blobs
    }

    /// Ask one pane's actor for its upgrade handoff. `None` when the pane has
    /// no registered handle or the actor has gone away.
    async fn request_pane_handoff(&self, tid: ResourceId) -> Option<PaneUpgradeHandle> {
        let handle = self.resource_handle(tid)?;
        let (reply, rx) = oneshot::channel();
        handle
            .upgrade
            .send(UpgradeHandleRequest { reply })
            .await
            .ok()?;
        rx.await.ok()
    }

    fn session_wire(&self, sid: SessionId) -> Option<u32> {
        self.idspace
            .session_wire(sid)
            .map(phux_protocol::SessionId::get)
    }

    fn window_wire(&self, wid: WindowId) -> Option<u32> {
        self.idspace
            .window_wire(wid)
            .map(phux_protocol::WindowId::get)
    }

    fn terminal_wire(&self, tid: ResourceId) -> Option<u32> {
        self.idspace
            .terminal_wire(tid)
            .and_then(phux_protocol::ResourceId::local_id)
    }

    /// Map a [`LayoutNode`] to its wire-id-keyed [`LayoutBlob`] mirror. Returns
    /// `None` if any referenced pane lacks a wire id (it would not round-trip).
    fn layout_to_blob(&self, node: &LayoutNode) -> Option<LayoutBlob> {
        match node {
            LayoutNode::Leaf(tid) => self.terminal_wire(*tid).map(LayoutBlob::Leaf),
            LayoutNode::Split {
                dir,
                ratio,
                left,
                right,
            } => Some(LayoutBlob::Split {
                dir: match dir {
                    SplitDir::Horizontal => SplitDirBlob::Horizontal,
                    SplitDir::Vertical => SplitDirBlob::Vertical,
                },
                ratio: *ratio,
                left: Box::new(self.layout_to_blob(left)?),
                right: Box::new(self.layout_to_blob(right)?),
            }),
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
    };
    bootstrap.send(request).await.ok()?;
    rx.await.ok()
}

/// The stream cut a resumed engine continues from, as the blob carried it.
fn agent_session_cut(carried: &AgentSessionBlob) -> AgentSessionBootstrap {
    AgentSessionBootstrap {
        base_seq: carried.base_seq,
        records: carried
            .records
            .iter()
            .map(|record| bytes::Bytes::from(record.clone()))
            .collect(),
        dropped: carried.dropped,
        ended: carried.ended,
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

/// What the pane pass produces: the wire-id -> core-id map the re-link passes
/// resolve against, each rebuilt pane's exit receiver, and what the caller's
/// wiring returned for each pane.
struct RebuiltPanes<W> {
    core_ids: HashMap<u32, ResourceId>,
    exit_watchers: PaneExitWatchers,
    wired: Vec<(ResourceId, W)>,
}

/// A committed rebuild: every resource's exit receiver, and each pane's
/// wiring (see [`ServerState::rebuild_from_blob`]).
#[derive(Debug)]
pub(crate) struct RebuiltTree<W> {
    /// Each rebuilt resource's (pane or agent session) exit receiver.
    pub(crate) exit_watchers: PaneExitWatchers,
    /// Each rebuilt pane's core id and what `wire_pane` returned for it.
    pub(crate) wired_panes: Vec<(ResourceId, W)>,
}

impl ServerState {
    /// Rebuild the tree from a [`StateBlob`] in the re-exec'd image
    /// (ADR-0032): recreate entities under their wire ids, restore
    /// allocators and ledgers, and spawn actors that re-adopt PTYs (or
    /// replay snapshots), then rebind carried agent sessions to their panes.
    /// `wire_pane` runs on each pane's actor before it starts, so the runtime
    /// installs the same sinks a fresh spawn gets (the agent detector arms
    /// only when its sink is present). Returns each rebuilt resource's exit
    /// receiver and each pane's wiring.
    ///
    /// Transactional: built on a fresh state and committed only on full
    /// success. The pass order resolves each pass's references. Runs inside
    /// the actors' `LocalSet`.
    ///
    /// # Errors
    ///
    /// [`RebuildError`] for bad topology, registry or actor failures, or
    /// dangling wire ids.
    #[allow(
        clippy::type_complexity,
        reason = "the runtime immediately consumes each rebuilt pane id and its one-shot exit receiver"
    )]
    pub(crate) fn rebuild_from_blob<W>(
        &mut self,
        blob: &StateBlob,
        wire_pane: impl FnMut(ResourceId, &mut TerminalActor) -> W,
    ) -> Result<RebuiltTree<W>, RebuildError> {
        validate_upgrade_blob(blob)?;
        let mut fresh = Self::new();
        fresh.config.scrollback = self.config.scrollback;
        fresh.config.agent_log_bytes = self.config.agent_log_bytes;
        // ADR-0109: a pre-token blob keeps this process's token.
        if blob.counters.server_instance.is_none() {
            fresh.idspace.set_instance(self.idspace.instance());
        }
        let session_core = fresh.rebuild_sessions(blob);
        let window_core = fresh.rebuild_windows(blob, &session_core)?;
        let mut panes = fresh.rebuild_panes(blob, &window_core, wire_pane)?;
        fresh.relink_window_contents(blob, &window_core, &panes.core_ids)?;
        fresh.relink_session_windows(blob, &session_core, &window_core)?;
        fresh.rebuild_agent_sessions(blob, &mut panes);
        fresh.restore_counters(blob);
        self.commit_rebuilt_tree(fresh);
        Ok(RebuiltTree {
            exit_watchers: panes.exit_watchers,
            wired_panes: panes.wired,
        })
    }

    /// Recreate every carried `AgentSession` under its recorded wire id,
    /// bound to its rebuilt parent pane, with an engine that continues the
    /// old stream: same record counter, same retained tail, same `ended`.
    /// The parent's `REPORT_AGENT_STATE` is re-bound to the stream
    /// (ADR-0103 §6). A session whose parent pane is not in the blob is
    /// dropped; losing one session must not cost the whole tree.
    fn rebuild_agent_sessions<W>(&mut self, blob: &StateBlob, panes: &mut RebuiltPanes<W>) {
        let log_bytes = self.config.agent_log_bytes;
        for carried in &blob.agent_sessions {
            let Some(&parent) = panes.core_ids.get(&carried.parent_wire_id) else {
                tracing::warn!(
                    session = carried.wire_id,
                    parent = carried.parent_wire_id,
                    "resume: agent session's parent pane did not cross; dropped"
                );
                continue;
            };
            let facet = phux_core::resource::AgentFacet {
                provider: carried.provider.clone(),
                native_id: carried.native_id.clone(),
                state: carried.state.clone(),
            };
            let core = match self.sessions.registry.new_agent_session(parent, facet) {
                Ok(core) => core,
                Err(error) => {
                    tracing::warn!(
                        session = carried.wire_id,
                        %error,
                        "resume: agent session could not be re-registered; dropped"
                    );
                    continue;
                }
            };
            let token = CancellationToken::new();
            let bundle = AgentSessionActor::restore(
                parent,
                &carried.provider,
                carried.native_id.as_deref(),
                token.clone(),
                log_bytes,
                agent_session_cut(carried),
            );
            if let (Some(parent_handle), Ok(session)) =
                (self.resource_handle(parent), bundle.handle.agent_session())
            {
                // The parent's mailbox is fresh, so this cannot be full.
                let _ = parent_handle.control.try_send(
                    crate::resource::ControlRequest::BindAgentSession {
                        append: session.append.clone(),
                    },
                );
            }
            // Pre-bind so the registration's intern returns the blob's id.
            self.idspace
                .bind_terminal(core, WireResourceId::local(carried.wire_id));
            self.spawn_resource_actor(core, bundle.handle, token, bundle.actor.run());
            panes.exit_watchers.push((core, bundle.exit_notify));
        }
    }

    /// Install a rebuilt tree, replacing only what reconstruction owns.
    fn commit_rebuilt_tree(&mut self, mut fresh: Self) {
        std::mem::swap(&mut self.sessions, &mut fresh.sessions);
        std::mem::swap(&mut self.idspace, &mut fresh.idspace);
        std::mem::swap(&mut self.resources, &mut fresh.resources);
        std::mem::swap(&mut self.retained, &mut fresh.retained);
    }

    /// Recreate every session under its recorded wire id, restoring its
    /// creation time, last-touched stamp, and root.
    fn rebuild_sessions(&mut self, blob: &StateBlob) -> HashMap<u32, SessionId> {
        let mut session_core: HashMap<u32, SessionId> = HashMap::new();
        for s in &blob.sessions {
            let core = self.sessions.registry.new_session(s.name.clone());
            self.idspace
                .bind_session(core, WireSessionId::new(s.wire_id));
            if let Some(sess) = self.sessions.registry.session_mut(core)
                && let Some(created) = unix_nanos_to_systemtime(s.created_at_unix_nanos)
            {
                sess.created_at = created;
            }
            // ADR-0105: a keep-empty session, including one with no windows,
            // survives the upgrade with its mark.
            if let Some(sess) = self.sessions.registry.session_mut(core) {
                sess.keep_empty = s.keep_empty;
            }
            if let Some(ts) = s.last_touched {
                self.sessions.bind_last_touched(core, ts);
            }
            if let Some(root) = &s.root {
                self.sessions.bind_root(core, root.clone());
            }
            session_core.insert(s.wire_id, core);
        }
        session_core
    }

    /// Recreate every window under its recorded wire id, attached to the
    /// session the blob names.
    fn rebuild_windows(
        &mut self,
        blob: &StateBlob,
        session_core: &HashMap<u32, SessionId>,
    ) -> Result<HashMap<u32, WindowId>, RebuildError> {
        let mut window_core: HashMap<u32, WindowId> = HashMap::new();
        for w in &blob.windows {
            let session =
                *session_core
                    .get(&w.session_wire_id)
                    .ok_or(RebuildError::DanglingRef {
                        kind: "session",
                        id: w.session_wire_id,
                    })?;
            let core = self.sessions.registry.new_window(session)?;
            self.idspace.bind_window(core, WireWindowId::new(w.wire_id));
            if let Some(cwd) = &w.last_cwd {
                self.sessions.bind_last_cwd(core, cwd.clone());
            }
            window_core.insert(w.wire_id, core);
        }
        Ok(window_core)
    }

    /// Recreate every pane under its recorded wire id, in the window the blob
    /// names, and spawn the actor that re-adopts its PTY.
    fn rebuild_panes<W>(
        &mut self,
        blob: &StateBlob,
        window_core: &HashMap<u32, WindowId>,
        mut wire_pane: impl FnMut(ResourceId, &mut TerminalActor) -> W,
    ) -> Result<RebuiltPanes<W>, RebuildError> {
        let scrollback = self.config.scrollback;
        let mut panes = RebuiltPanes {
            core_ids: HashMap::new(),
            exit_watchers: Vec::with_capacity(blob.panes.len()),
            wired: Vec::with_capacity(blob.panes.len()),
        };
        for p in &blob.panes {
            let window = *window_core
                .get(&p.window_wire_id)
                .ok_or(RebuildError::DanglingRef {
                    kind: "window",
                    id: p.window_wire_id,
                })?;
            let core = self.sessions.registry.new_terminal(window)?;
            if let Some(desc) = self.sessions.registry.terminal_mut(core) {
                desc.dims = (p.cols, p.rows);
                desc.cwd.clone_from(&p.cwd);
                desc.title.clone_from(&p.title);
            }

            let bundle = pane_actor_bundle(p, scrollback)?;
            // Pre-bind so the spawn's intern returns the blob's id.
            self.idspace
                .bind_terminal(core, WireResourceId::local(p.wire_id));
            let crate::terminal_actor::TerminalActorBundle {
                mut actor,
                handle,
                token,
                exit_notify,
            } = bundle;
            panes.wired.push((core, wire_pane(core, &mut actor)));
            self.spawn_resource_actor(core, handle, token, actor.run());
            // ADR-0124: what the old image knew about this pane's retention.
            if let Some(secs) = p.retain_secs {
                self.note_retain_request(core, secs);
            }
            if let Some(exit) = p.retained_exit {
                self.restore_retained_exit(core, exit_facet(exit));
            }
            if let Some(exit_notify) = exit_notify {
                panes.exit_watchers.push((core, exit_notify));
            }
            panes.core_ids.insert(p.wire_id, core);
        }
        Ok(panes)
    }

    /// Re-apply window pane order / active / layout (the auto-split layout
    /// `new_terminal` produced is discarded for the blob's).
    fn relink_window_contents(
        &mut self,
        blob: &StateBlob,
        window_core: &HashMap<u32, WindowId>,
        pane_core: &HashMap<u32, ResourceId>,
    ) -> Result<(), RebuildError> {
        for w in &blob.windows {
            let Some(&core) = window_core.get(&w.wire_id) else {
                continue;
            };
            let panes = resolve_ids(&w.pane_wire_ids, pane_core, "pane")?;
            let active = match w.active_resource {
                Some(id) => Some(
                    *pane_core
                        .get(&id)
                        .ok_or(RebuildError::DanglingRef { kind: "pane", id })?,
                ),
                None => None,
            };
            let layout = w
                .layout
                .as_ref()
                .map(|l| layout_from_blob(l, pane_core))
                .transpose()?;
            if let Some(win) = self.sessions.registry.window_mut(core) {
                win.slots = panes;
                win.active = active;
                win.layout = layout;
            }
        }
        Ok(())
    }

    /// Re-apply session window order / active.
    fn relink_session_windows(
        &mut self,
        blob: &StateBlob,
        session_core: &HashMap<u32, SessionId>,
        window_core: &HashMap<u32, WindowId>,
    ) -> Result<(), RebuildError> {
        for s in &blob.sessions {
            let Some(&core) = session_core.get(&s.wire_id) else {
                continue;
            };
            let windows = resolve_ids(&s.window_wire_ids, window_core, "window")?;
            let active = match s.active_window {
                Some(id) => Some(
                    *window_core
                        .get(&id)
                        .ok_or(RebuildError::DanglingRef { kind: "window", id })?,
                ),
                None => None,
            };
            if let Some(sess) = self.sessions.registry.session_mut(core) {
                sess.windows = windows;
                sess.active = active;
            }
        }
        Ok(())
    }

    /// Close every pane the old image retained after exit (ADR-0124 §6),
    /// through its exit watcher. Returns how many.
    pub fn close_upgrade_retained(&mut self, blob: &StateBlob) -> u32 {
        let retained: Vec<ResourceId> = blob
            .panes
            .iter()
            .filter(|pane| pane.retained_exit.is_some())
            .filter_map(|pane| self.terminal_from_wire(&WireResourceId::local(pane.wire_id)))
            .collect();
        self.close_resources(
            &retained,
            phux_protocol::wire::frame::CloseReason::ServerShutdown,
        )
    }

    /// Restore the allocators above every restored id.
    fn restore_counters(&mut self, blob: &StateBlob) {
        self.idspace
            .set_next_session_wire(blob.counters.next_session_wire_id);
        self.idspace
            .set_next_terminal_wire(blob.counters.next_terminal_wire_id);
        self.idspace
            .set_next_window_wire(blob.counters.next_window_wire_id);
        self.sessions
            .set_next_touch_timestamp(blob.counters.next_touch_timestamp);
        // ADR-0109: the terminal allocator was restored, so no id can repeat
        // and the token that names the id space must survive with it.
        if let Some(bytes) = blob.counters.server_instance {
            self.idspace
                .set_instance(phux_protocol::ids::ServerInstance::new(bytes));
        }
    }
}

/// Build a pane actor around its inherited PTY, or a PTY-less actor that
/// replays its snapshot. Only one of the two PTY ids is corruption.
fn pane_actor_bundle(
    p: &PaneBlob,
    scrollback: phux_config::ScrollbackLimits,
) -> Result<crate::terminal_actor::TerminalActorBundle, RebuildError> {
    let seed = pane_seed(p);
    Ok(match (p.master_fd, p.child_pid) {
        (Some(master_fd), Some(child_pid)) => TerminalActor::new_with_adopted_pty(
            master_fd,
            child_pid,
            p.cols,
            p.rows,
            scrollback,
            CancellationToken::new(),
            &seed,
        )?,
        (None, None) => TerminalActor::new_with_seed(p.cols, p.rows, &seed)?,
        _ => {
            return Err(RebuildError::InvalidPtyPair { wire_id: p.wire_id });
        }
    })
}

/// Check the blob's topology is complete and internally consistent before
/// any registry mutation, so a bad handoff cannot leave a partial tree.
pub(crate) fn validate_upgrade_blob(blob: &StateBlob) -> Result<(), RebuildError> {
    let sessions = unique_wire_ids("session", blob.sessions.iter().map(|s| s.wire_id))?;
    let windows = unique_wire_ids("window", blob.windows.iter().map(|w| w.wire_id))?;
    let panes = unique_wire_ids("pane", blob.panes.iter().map(|p| p.wire_id))?;
    // Agent sessions share the pane wire-id space: one id, one resource.
    unique_wire_ids(
        "resource",
        blob.panes
            .iter()
            .map(|p| p.wire_id)
            .chain(blob.agent_sessions.iter().map(|a| a.wire_id)),
    )?;

    for s in &blob.sessions {
        require_refs("window", &s.window_wire_ids, &windows)?;
        if let Some(id) = s.active_window {
            require_ref("window", id, &windows)?;
        }
    }
    for w in &blob.windows {
        require_ref("session", w.session_wire_id, &sessions)?;
        require_refs("pane", &w.pane_wire_ids, &panes)?;
        if let Some(id) = w.active_resource {
            require_ref("pane", id, &panes)?;
        }
        if let Some(layout) = &w.layout {
            validate_layout(layout, &panes)?;
        }
    }
    for p in &blob.panes {
        require_ref("window", p.window_wire_id, &windows)?;
        match (p.master_fd, p.child_pid) {
            (Some(_), Some(_)) | (None, None) => {}
            _ => return Err(RebuildError::InvalidPtyPair { wire_id: p.wire_id }),
        }
    }
    Ok(())
}

fn unique_wire_ids(
    kind: &'static str,
    ids: impl IntoIterator<Item = u32>,
) -> Result<HashSet<u32>, RebuildError> {
    let mut seen = HashSet::new();
    for id in ids {
        if !seen.insert(id) {
            return Err(RebuildError::DuplicateId { kind, id });
        }
    }
    Ok(seen)
}

fn require_ref(kind: &'static str, id: u32, known: &HashSet<u32>) -> Result<(), RebuildError> {
    if known.contains(&id) {
        Ok(())
    } else {
        Err(RebuildError::DanglingRef { kind, id })
    }
}

fn require_refs(kind: &'static str, ids: &[u32], known: &HashSet<u32>) -> Result<(), RebuildError> {
    ids.iter().try_for_each(|id| require_ref(kind, *id, known))
}

fn validate_layout(node: &LayoutBlob, panes: &HashSet<u32>) -> Result<(), RebuildError> {
    match node {
        LayoutBlob::Leaf(id) => require_ref("pane", *id, panes),
        LayoutBlob::Split { left, right, .. } => {
            validate_layout(left, panes)?;
            validate_layout(right, panes)
        }
    }
}

/// Resolve a list of wire ids to their rebuilt core ids.
fn resolve_ids<K: Copy>(
    wire_ids: &[u32],
    map: &HashMap<u32, K>,
    kind: &'static str,
) -> Result<Vec<K>, RebuildError> {
    wire_ids
        .iter()
        .map(|id| {
            map.get(id)
                .copied()
                .ok_or(RebuildError::DanglingRef { kind, id: *id })
        })
        .collect()
}

/// `UNIX_EPOCH + nanos`, or `None` if the value overflows a `u64` of
/// nanoseconds (≈ year 2554 — far past any real timestamp).
fn unix_nanos_to_systemtime(nanos: u128) -> Option<std::time::SystemTime> {
    u64::try_from(nanos)
        .ok()
        .map(|n| UNIX_EPOCH + Duration::from_nanos(n))
}

/// Concatenate a pane's scrollback then viewport replay — the order a client
/// applies them, so seeding a fresh `Terminal` reproduces the same grid —
/// then the program's OSC title as an `OSC 2`, so the rebuilt engine reports
/// the title the program set rather than none.
fn pane_seed(p: &PaneBlob) -> Vec<u8> {
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

/// Rebuild a [`LayoutNode`] from its [`LayoutBlob`] mirror, resolving pane wire
/// ids to core ids.
fn layout_from_blob(
    node: &LayoutBlob,
    panes: &HashMap<u32, ResourceId>,
) -> Result<LayoutNode, RebuildError> {
    match node {
        LayoutBlob::Leaf(wire) => {
            panes
                .get(wire)
                .copied()
                .map(LayoutNode::Leaf)
                .ok_or(RebuildError::DanglingRef {
                    kind: "pane",
                    id: *wire,
                })
        }
        LayoutBlob::Split {
            dir,
            ratio,
            left,
            right,
        } => Ok(LayoutNode::Split {
            dir: match dir {
                SplitDirBlob::Horizontal => SplitDir::Horizontal,
                SplitDirBlob::Vertical => SplitDir::Vertical,
            },
            ratio: *ratio,
            left: Box::new(layout_from_blob(left, panes)?),
            right: Box::new(layout_from_blob(right, panes)?),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::{RebuildError, WireResourceId, validate_upgrade_blob};
    use crate::state::ServerState;
    use crate::terminal_actor::TerminalActor;
    use crate::upgrade::blob::{
        AgentSessionBlob, BLOB_VERSION, Counters, LayoutBlob, PaneBlob, SessionBlob, SplitDirBlob,
        StateBlob, WindowBlob,
    };
    use std::path::PathBuf;

    /// One pane's tree walks into a blob with resolved links and a snapshot.
    #[tokio::test(flavor = "current_thread")]
    async fn build_upgrade_blob_captures_tree_and_snapshot() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut state = ServerState::new();

                // A session/window/pane in the registry.
                let sid = state.registry_mut().new_session("main".to_owned());
                let wid = state.registry_mut().new_window(sid).expect("new_window");
                let tid = state
                    .registry_mut()
                    .new_terminal(wid)
                    .expect("new_terminal");
                let session_wire = state.idspace.intern_session(sid).get();
                let window_wire = state.intern_window_wire(wid).get();

                // A real (no-PTY) actor answering the upgrade request.
                let bundle = TerminalActor::new_with_seed(20, 5, b"hello").expect("new_with_seed");
                let handle = bundle.handle.clone();
                let token = bundle.token.clone();
                tokio::task::spawn_local(bundle.actor.run());
                let pane_wire = state
                    .register_resource_handle(tid, handle, token)
                    .local_id()
                    .expect("local wire id");

                let blob = state.build_upgrade_blob(7).await;

                assert_eq!(blob.listener_fd, 7);
                assert_eq!(blob.sessions.len(), 1);
                assert_eq!(blob.windows.len(), 1);
                assert_eq!(blob.panes.len(), 1);

                let s = &blob.sessions[0];
                assert_eq!(s.wire_id, session_wire);
                assert_eq!(s.name, "main");
                assert_eq!(s.window_wire_ids, vec![window_wire]);

                let w = &blob.windows[0];
                assert_eq!(w.wire_id, window_wire);
                assert_eq!(w.session_wire_id, session_wire);
                assert_eq!(w.pane_wire_ids, vec![pane_wire]);

                let p = &blob.panes[0];
                assert_eq!(p.wire_id, pane_wire);
                assert_eq!(p.window_wire_id, window_wire);
                assert_eq!((p.cols, p.rows), (20, 5));
                assert_eq!(p.master_fd, None, "no-PTY actor has no master fd");
                assert_eq!(p.child_pid, None);
                assert!(
                    String::from_utf8_lossy(&p.vt_replay_bytes).contains("hello"),
                    "pane snapshot should carry the actor's seeded text"
                );

                // Counters sit above every minted id.
                assert!(blob.counters.next_session_wire_id > session_wire);
                assert!(blob.counters.next_window_wire_id > window_wire);
                assert!(blob.counters.next_terminal_wire_id > pane_wire);
            })
            .await;
    }

    /// state → blob → rebuild → blob round-trips tree, ids, and counters.
    #[tokio::test(flavor = "current_thread")]
    async fn rebuild_from_blob_round_trips_the_tree() {
        let local = tokio::task::LocalSet::new();
        Box::pin(local.run_until(async {
            let mut state = ServerState::new();
            let sid = state.registry_mut().new_session("main".to_owned());
            let wid = state.registry_mut().new_window(sid).expect("new_window");
            let tid = state
                .registry_mut()
                .new_terminal(wid)
                .expect("new_terminal");
            state.idspace.intern_session(sid);
            state.intern_window_wire(wid);
            let bundle = TerminalActor::new_with_seed(20, 5, b"hello").expect("new_with_seed");
            tokio::task::spawn_local(bundle.actor.run());
            state.register_resource_handle(tid, bundle.handle, bundle.token);

            // ADR-0105: a keep-empty mark, and a keep-empty session with
            // no windows at all, must both survive the round trip.
            state
                .registry_mut()
                .session_mut(sid)
                .expect("session")
                .keep_empty = true;
            let parked = state.seed_empty_session("parked");
            state.idspace.intern_session(parked);

            let blob = state.build_upgrade_blob(7).await;

            // Rebuild into a brand-new state, then re-emit a blob from it.
            let mut fresh = ServerState::new();
            // phux-1x9s.6: every pane's actor passes through the runtime's
            // wiring before it runs, as a fresh spawn's does.
            let rebuilt = fresh
                .rebuild_from_blob(&blob, |pane, actor| (pane, actor.has_pty()))
                .expect("rebuild");
            assert_eq!(
                rebuilt.exit_watchers.len(),
                blob.panes.len(),
                "every rebuilt pane must return an exit receiver for the runtime watcher"
            );
            assert_eq!(
                rebuilt.wired_panes.len(),
                blob.panes.len(),
                "every rebuilt pane must be wired before its actor runs"
            );
            assert!(
                rebuilt
                    .wired_panes
                    .iter()
                    .all(|(pane, (wired, _))| pane == wired),
                "the wiring sees the pane it is handed back for"
            );
            let blob2 = fresh.build_upgrade_blob(7).await;

            assert_eq!(blob.sessions, blob2.sessions, "sessions round-trip");
            assert!(
                blob2.sessions.iter().all(|s| s.keep_empty),
                "keep-empty marks round-trip"
            );
            assert!(
                blob2
                    .sessions
                    .iter()
                    .any(|s| s.name == "parked" && s.window_wire_ids.is_empty()),
                "an empty keep-empty session survives with zero windows"
            );
            assert_eq!(blob.windows, blob2.windows, "windows + layout round-trip");
            assert_eq!(blob.counters, blob2.counters, "id allocators round-trip");
            assert_eq!(blob.panes.len(), blob2.panes.len());

            let (p1, p2) = (&blob.panes[0], &blob2.panes[0]);
            assert_eq!(p1.wire_id, p2.wire_id);
            assert_eq!(p1.window_wire_id, p2.window_wire_id);
            assert_eq!((p1.cols, p1.rows), (p2.cols, p2.rows));
            assert!(
                String::from_utf8_lossy(&p2.vt_replay_bytes).contains("hello"),
                "rebuilt pane should replay its seed snapshot"
            );
        }))
        .await;
    }

    /// ADR-0109: the instance token changes exactly when ids can repeat.
    #[tokio::test(flavor = "current_thread")]
    async fn the_instance_token_survives_an_upgrade_and_only_an_upgrade() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let state = ServerState::new();
                let token = state.idspace.instance();
                let blob = state.build_upgrade_blob(7).await;
                assert_eq!(blob.counters.server_instance, Some(*token.as_bytes()));

                let mut upgraded = ServerState::new();
                assert_ne!(
                    upgraded.idspace.instance(),
                    token,
                    "a cold start mints a new token"
                );
                upgraded
                    .rebuild_from_blob(&blob, |_, _| ())
                    .expect("rebuild");
                assert_eq!(upgraded.idspace.instance(), token, "an upgrade keeps it");

                let mut legacy = state.build_upgrade_blob(7).await;
                legacy.counters.server_instance = None;
                let mut from_legacy = ServerState::new();
                let fresh = from_legacy.idspace.instance();
                from_legacy
                    .rebuild_from_blob(&legacy, |_, _| ())
                    .expect("rebuild");
                assert_eq!(
                    from_legacy.idspace.instance(),
                    fresh,
                    "nothing to restore: the fresh token stands"
                );
            })
            .await;
    }

    /// The program's title, as an `OSC 2` a pane emitted before the upgrade.
    const PROGRAM_TITLE: &str = "vim README.md";

    /// One pane whose actor saw `seed`, with `user_title` on its registry
    /// entry, carried through blob -> rebuild -> a second blob (a second
    /// upgrade). Returns the twice-upgraded state.
    async fn upgrade_twice(user_title: Option<&str>, seed: &[u8]) -> ServerState {
        let mut state = ServerState::new();
        let sid = state.registry_mut().new_session("main".to_owned());
        let wid = state.registry_mut().new_window(sid).expect("new_window");
        let tid = state
            .registry_mut()
            .new_terminal(wid)
            .expect("new_terminal");
        state
            .registry_mut()
            .terminal_mut(tid)
            .expect("terminal")
            .title = user_title.map(ToOwned::to_owned);
        state.idspace.intern_session(sid);
        state.intern_window_wire(wid);
        let bundle = TerminalActor::new_with_seed(20, 5, seed).expect("new_with_seed");
        tokio::task::spawn_local(bundle.actor.run());
        state.register_resource_handle(tid, bundle.handle, bundle.token);

        let blob = state.build_upgrade_blob(7).await;
        let mut once = ServerState::new();
        once.rebuild_from_blob(&blob, |_, _| ())
            .expect("first rebuild");
        let blob = once.build_upgrade_blob(7).await;
        let mut twice = ServerState::new();
        twice
            .rebuild_from_blob(&blob, |_, _| ())
            .expect("second rebuild");
        twice
    }

    /// `GET_STATE`'s `title` for the state's only pane: the user-set title.
    fn get_state_title(state: &mut ServerState) -> Option<String> {
        let sid = state
            .registry()
            .sessions()
            .map(|(id, _)| id)
            .next()
            .expect("a session");
        let snapshot = state.build_session_snapshot(sid).expect("snapshot");
        assert_eq!(snapshot.resources.len(), 1);
        snapshot.resources[0].title.clone()
    }

    /// `GET_SCREEN`'s `title` for the state's only pane: the engine's OSC title.
    async fn get_screen_title(state: &ServerState) -> Option<String> {
        let handles = state.upgrade_handles();
        assert_eq!(handles.len(), 1);
        let (reply, rx) = tokio::sync::oneshot::channel();
        handles[0]
            .1
            .terminal()
            .expect("terminal handle")
            .screen
            .send(crate::resource::terminal::ScreenRequest {
                pane: 1,
                scrollback: None,
                cells: false,
                format: 0,
                reply,
            })
            .await
            .expect("send screen request");
        match rx.await.expect("screen reply") {
            crate::resource::terminal::ScreenReply::Projection(screen) => screen.title,
            other @ crate::resource::terminal::ScreenReply::TooLarge { .. } => {
                panic!("unexpected screen reply: {other:?}")
            }
        }
    }

    /// phux-xy9y: a pane no one named keeps no user title across upgrades,
    /// while the title its program set still reads back from the engine.
    #[tokio::test(flavor = "current_thread")]
    async fn an_upgrade_keeps_the_program_title_out_of_the_user_title() {
        let local = tokio::task::LocalSet::new();
        Box::pin(local.run_until(async {
            let seed = format!("hello\x1b]2;{PROGRAM_TITLE}\x07");
            let mut state = upgrade_twice(None, seed.as_bytes()).await;
            assert_eq!(
                get_state_title(&mut state),
                None,
                "GET_STATE's user-set title must stay unset"
            );
            assert_eq!(
                get_screen_title(&state).await.as_deref(),
                Some(PROGRAM_TITLE),
                "GET_SCREEN's OSC title must still report the program's title"
            );
        }))
        .await;
    }

    /// phux-xy9y: a user-set title survives unchanged even when the program
    /// set a different OSC title, and neither overwrites the other.
    #[tokio::test(flavor = "current_thread")]
    async fn an_upgrade_keeps_the_user_title_beside_the_program_title() {
        let local = tokio::task::LocalSet::new();
        Box::pin(local.run_until(async {
            let seed = format!("hello\x1b]2;{PROGRAM_TITLE}\x07");
            let mut state = upgrade_twice(Some("build"), seed.as_bytes()).await;
            assert_eq!(get_state_title(&mut state).as_deref(), Some("build"));
            assert_eq!(
                get_screen_title(&state).await.as_deref(),
                Some(PROGRAM_TITLE)
            );

            // No OSC title at all: still nothing in the engine, user title kept.
            let mut state = upgrade_twice(Some("build"), b"hello").await;
            assert_eq!(get_state_title(&mut state).as_deref(), Some("build"));
            assert_eq!(get_screen_title(&state).await, None);
        }))
        .await;
    }

    /// A replayed title cannot break out of its `OSC 2`.
    #[test]
    fn the_replayed_title_drops_control_characters() {
        assert_eq!(
            super::osc_title_sequence("a\x07b\x1b]0;c"),
            b"\x1b]2;ab]0;c\x1b\\".to_vec()
        );
    }

    fn empty_counters() -> Counters {
        Counters {
            next_session_wire_id: 10,
            next_terminal_wire_id: 10,
            next_window_wire_id: 10,
            next_touch_timestamp: 1,
            server_instance: None,
        }
    }

    fn session(wire_id: u32, windows: Vec<u32>) -> SessionBlob {
        SessionBlob {
            wire_id,
            name: "main".to_owned(),
            window_wire_ids: windows,
            active_window: None,
            created_at_unix_nanos: 0,
            last_touched: None,
            root: None,
            keep_empty: false,
        }
    }

    fn window(wire_id: u32, session_wire_id: u32, panes: Vec<u32>) -> WindowBlob {
        WindowBlob {
            wire_id,
            session_wire_id,
            pane_wire_ids: panes,
            active_resource: None,
            layout: None,
            last_cwd: None,
        }
    }

    fn no_pty_pane(wire_id: u32, window_wire_id: u32) -> PaneBlob {
        PaneBlob {
            wire_id,
            window_wire_id,
            cols: 80,
            rows: 24,
            cell_px: None,
            cwd: PathBuf::from("/tmp"),
            title: None,
            osc_title: None,
            term: "xterm-256color".to_owned(),
            child_pid: None,
            master_fd: None,
            vt_replay_bytes: Vec::new(),
            scrollback_bytes: Vec::new(),
            retained_exit: None,
            retain_secs: None,
        }
    }

    fn blob(
        sessions: Vec<SessionBlob>,
        windows: Vec<WindowBlob>,
        panes: Vec<PaneBlob>,
    ) -> StateBlob {
        StateBlob {
            version: BLOB_VERSION,
            listener_fd: 7,
            counters: empty_counters(),
            sessions,
            windows,
            panes,
            agent_sessions: Vec::new(),
        }
    }

    fn session_names(state: &ServerState) -> Vec<String> {
        let mut names: Vec<String> = state
            .registry()
            .sessions()
            .map(|(_, s)| s.name.clone())
            .collect();
        names.sort();
        names
    }

    /// A complete blob rebuilds; existing state is replaced only once the
    /// tree is ready.
    #[tokio::test(flavor = "current_thread")]
    async fn rebuild_from_a_complete_blob_is_transactional() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut state = ServerState::new();
                state.registry_mut().new_session("already-here".to_owned());
                let handoff = blob(
                    vec![session(1, vec![2])],
                    vec![window(2, 1, vec![3])],
                    vec![no_pty_pane(3, 2)],
                );
                validate_upgrade_blob(&handoff).expect("complete topology");
                state
                    .rebuild_from_blob(&handoff, |_, _| ())
                    .expect("rebuild");
                assert_eq!(session_names(&state), vec!["main".to_owned()]);
            })
            .await;
    }

    /// phux-lv57: a resumed tree has served no client in this process, so
    /// the rebuild must not arm last-session self-exit. Only a client that
    /// attaches (or creates a session) after the re-exec may arm it; until
    /// then a resumed pane's exit leaves the server running.
    #[tokio::test(flavor = "current_thread")]
    async fn rebuild_does_not_arm_last_session_self_exit() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut state = ServerState::new();
                let handoff = blob(
                    vec![session(1, vec![2])],
                    vec![window(2, 1, vec![3])],
                    vec![no_pty_pane(3, 2)],
                );
                state
                    .rebuild_from_blob(&handoff, |_, _| ())
                    .expect("rebuild");
                assert_eq!(session_names(&state), vec!["main".to_owned()]);
                assert!(
                    !state.has_served_client(),
                    "a resumed tree must not count as having served clients"
                );
            })
            .await;
    }

    /// A session that names a window the blob does not contain fails closed
    /// and leaves the destination state untouched.
    #[tokio::test(flavor = "current_thread")]
    async fn dangling_window_id_does_not_mutate_state() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut state = ServerState::new();
                state.registry_mut().new_session("already-here".to_owned());
                let handoff = blob(vec![session(1, vec![99])], Vec::new(), Vec::new());
                match state.rebuild_from_blob(&handoff, |_, _| ()) {
                    Err(RebuildError::DanglingRef { kind, id }) => {
                        assert_eq!(kind, "window");
                        assert_eq!(id, 99);
                    }
                    other => panic!("expected dangling window, got {other:?}"),
                }
                assert_eq!(session_names(&state), vec!["already-here".to_owned()]);
            })
            .await;
    }

    #[test]
    fn duplicate_session_wire_id_is_rejected() {
        let handoff = blob(
            vec![session(1, Vec::new()), session(1, Vec::new())],
            Vec::new(),
            Vec::new(),
        );
        match validate_upgrade_blob(&handoff) {
            Err(RebuildError::DuplicateId { kind, id }) => {
                assert_eq!(kind, "session");
                assert_eq!(id, 1);
            }
            other => panic!("expected duplicate session, got {other:?}"),
        }
    }

    fn agent_session(wire_id: u32, parent_wire_id: u32) -> AgentSessionBlob {
        AgentSessionBlob {
            wire_id,
            parent_wire_id,
            provider: "claude".to_owned(),
            native_id: None,
            state: None,
            base_seq: 1,
            dropped: 0,
            ended: false,
            records: vec!["{\"seq\":1,\"ts_ms\":1,\"type\":\"prompt\",\"data\":{}}\n".to_owned()],
        }
    }

    /// Panes and agent sessions share one wire-id space.
    #[test]
    fn an_agent_session_reusing_a_pane_wire_id_is_rejected() {
        let mut handoff = blob(
            vec![session(1, vec![2])],
            vec![window(2, 1, vec![3])],
            vec![no_pty_pane(3, 2)],
        );
        handoff.agent_sessions = vec![agent_session(3, 3)];
        match validate_upgrade_blob(&handoff) {
            Err(RebuildError::DuplicateId { kind, id }) => {
                assert_eq!(kind, "resource");
                assert_eq!(id, 3);
            }
            other => panic!("expected duplicate resource id, got {other:?}"),
        }
    }

    /// A session whose parent pane is not in the blob is dropped; the rest
    /// of the tree, and every other session, still resumes.
    #[tokio::test(flavor = "current_thread")]
    async fn an_agent_session_without_its_parent_pane_is_dropped() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut state = ServerState::new();
                let mut handoff = blob(
                    vec![session(1, vec![2])],
                    vec![window(2, 1, vec![3])],
                    vec![no_pty_pane(3, 2)],
                );
                handoff.agent_sessions = vec![agent_session(4, 3), agent_session(5, 99)];
                let watchers = state
                    .rebuild_from_blob(&handoff, |_, _| ())
                    .expect("rebuild")
                    .exit_watchers;
                assert_eq!(watchers.len(), 2, "the pane and its one session");
                let kept = state.terminal_from_wire(&WireResourceId::local(4));
                assert!(kept.is_some(), "the bound session resumes under its id");
                assert!(
                    state
                        .terminal_from_wire(&WireResourceId::local(5))
                        .is_none(),
                    "the orphaned session is dropped"
                );
            })
            .await;
    }

    #[test]
    fn incomplete_pty_pair_is_rejected() {
        let mut pane = no_pty_pane(3, 2);
        pane.master_fd = Some(11);
        let handoff = blob(
            vec![session(1, vec![2])],
            vec![window(2, 1, vec![3])],
            vec![pane],
        );
        match validate_upgrade_blob(&handoff) {
            Err(RebuildError::InvalidPtyPair { wire_id }) => assert_eq!(wire_id, 3),
            other => panic!("expected incomplete PTY pair, got {other:?}"),
        }
    }

    #[test]
    fn layout_leaf_must_name_a_pane_in_the_blob() {
        let mut win = window(2, 1, vec![3]);
        win.layout = Some(LayoutBlob::Split {
            dir: SplitDirBlob::Horizontal,
            ratio: 0.5,
            left: Box::new(LayoutBlob::Leaf(3)),
            right: Box::new(LayoutBlob::Leaf(99)),
        });
        let handoff = blob(
            vec![session(1, vec![2])],
            vec![win],
            vec![no_pty_pane(3, 2)],
        );
        match validate_upgrade_blob(&handoff) {
            Err(RebuildError::DanglingRef { kind, id }) => {
                assert_eq!(kind, "pane");
                assert_eq!(id, 99);
            }
            other => panic!("expected dangling layout pane, got {other:?}"),
        }
    }

    /// A pane actor failure installs none of the rebuilt sessions.
    #[tokio::test(flavor = "current_thread")]
    async fn pane_actor_failure_does_not_install_a_partial_tree() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut state = ServerState::new();
                state.registry_mut().new_session("already-here".to_owned());

                let mut bad = no_pty_pane(4, 2);
                // libghostty refuses a zero-sized grid; topology is otherwise
                // complete, so this is a mid-rebuild actor failure.
                bad.cols = 0;
                bad.rows = 0;
                let handoff = blob(
                    vec![session(1, vec![2])],
                    vec![window(2, 1, vec![3, 4])],
                    vec![no_pty_pane(3, 2), bad],
                );
                validate_upgrade_blob(&handoff).expect("topology is complete");
                match state.rebuild_from_blob(&handoff, |_, _| ()) {
                    Err(RebuildError::Actor(_)) => {}
                    other => panic!("expected actor rebuild failure, got {other:?}"),
                }
                assert_eq!(
                    session_names(&state),
                    vec!["already-here".to_owned()],
                    "a mid-rebuild actor failure must not commit the partial tree"
                );
            })
            .await;
    }
}
