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
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::ServerState;
use crate::resource::ResourceHandle;
use crate::terminal_actor::{PaneUpgradeHandle, TerminalActor, UpgradeHandleRequest};
use crate::upgrade::blob::{
    BLOB_VERSION, Counters, LayoutBlob, PaneBlob, RetainedExitBlob, SessionBlob, SplitDirBlob,
    StateBlob, WindowBlob,
};

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
    /// Assemble a [`StateBlob`] from the live session/window/pane tree for a
    /// graceful upgrade.
    ///
    /// Walks sessions → windows → panes, keying everything by wire id, and
    /// asks each pane's actor (over its `upgrade` mailbox) for the PTY
    /// descriptors + replay snapshot. `listener_fd` is the inherited
    /// `UnixListener` descriptor the orchestrator will pass to the new image.
    ///
    /// Must run inside the `LocalSet` that owns the pane actors (it awaits
    /// their replies). A pane whose actor cannot be reached is recorded from
    /// its descriptor with no handoff — the resume path then has nothing to
    /// re-adopt for it.
    pub async fn build_upgrade_blob(&self, listener_fd: RawFd) -> StateBlob {
        let mut handoffs = HashMap::new();
        let tids: Vec<ResourceId> = self.resources.resource_ids();
        for tid in tids {
            if let Some(handoff) = self.request_pane_handoff(tid).await {
                handoffs.insert(tid, handoff);
            }
        }
        self.assemble_upgrade_blob(listener_fd, &handoffs)
    }

    /// Record the upgrade context — the listening socket's raw fd, path, and
    /// the server's effective runtime flags (phux-v45.10) — at startup, for
    /// `handle_upgrade` to read when building the handoff.
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

    /// Clone every resource's [`ResourceHandle`] so the runtime can query
    /// each engine's upgrade handoff *outside* the `ServerState` lock (it
    /// can't hold the `Arc<Mutex<_>>` across the await; see
    /// [`Self::assemble_upgrade_blob`]).
    pub(crate) fn upgrade_handles(&self) -> Vec<(ResourceId, ResourceHandle)> {
        self.all_resource_handles()
    }

    /// Assemble the [`StateBlob`] from the live tree plus a pre-fetched map of
    /// per-pane handoffs — synchronous, so the runtime can call it under the
    /// state lock after gathering the handoffs out of lock.
    pub(crate) fn assemble_upgrade_blob(
        &self,
        listener_fd: RawFd,
        handoffs: &HashMap<ResourceId, PaneUpgradeHandle>,
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
                    // ADR-0124 §6: a retained pane has no process to
                    // re-adopt, so its PTY does not cross and the new image
                    // closes it with the exit it kept; a live pane's
                    // retention request crosses with it.
                    let retained = self.retained_exit(tid).map(exit_blob);
                    panes.push(pane_blob(
                        pane_wire,
                        window_wire,
                        desc,
                        &self.config.term,
                        handoffs.get(&tid).filter(|_| retained.is_none()),
                        retained,
                        self.retain_request(tid),
                    ));
                }
            }
        }

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
        }
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
        title: handoff
            .as_ref()
            .and_then(|h| h.title.clone())
            .or_else(|| desc.title.clone()),
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

/// Each rebuilt pane's core id and the one-shot exit receiver the runtime
/// restores its lifecycle watcher from.
type PaneExitWatchers = Vec<(
    ResourceId,
    oneshot::Receiver<phux_core::process::ExitOutcome>,
)>;

/// What the pane pass produces: the wire-id -> core-id map the re-link passes
/// resolve against, and each rebuilt pane's exit receiver.
struct RebuiltPanes {
    core_ids: HashMap<u32, ResourceId>,
    exit_watchers: PaneExitWatchers,
}

impl ServerState {
    /// Rebuild the session/window/pane tree from a [`StateBlob`] in the
    /// re-exec'd image (ADR-0032): recreate every entity under its recorded
    /// wire id, restore the id allocators + cwd/last-touched metadata, and
    /// spawn a pane actor that re-adopts the inherited PTY (or, for a pane with
    /// no handoff, replays its snapshot into a fresh no-PTY actor). Returns each
    /// rebuilt pane's exit receiver so the runtime can restore its lifecycle
    /// watcher after releasing the state lock.
    ///
    /// Reconstruction is transactional: the blob is validated, the tree is
    /// built on a fresh [`ServerState`], and only a complete success is
    /// committed onto `self`. A validation, registry, or actor failure
    /// leaves this state untouched so a resume cannot serve a partial tree.
    ///
    /// Linear reconstruction: create entities, bind wire ids, spawn actors,
    /// re-link the tree, restore counters. The order of the passes is the
    /// meaning — each one resolves references the previous one bound.
    ///
    /// Must run inside the `LocalSet` that owns pane actors (it spawns them).
    ///
    /// # Errors
    /// [`RebuildError`] on incomplete topology, a registry insertion
    /// failure, an actor build failure, or a dangling wire-id reference.
    #[allow(
        clippy::type_complexity,
        reason = "the runtime immediately consumes each rebuilt pane id and its one-shot exit receiver"
    )]
    pub fn rebuild_from_blob(
        &mut self,
        blob: &StateBlob,
    ) -> Result<PaneExitWatchers, RebuildError> {
        validate_upgrade_blob(blob)?;
        let mut fresh = Self::new();
        fresh.config.scrollback = self.config.scrollback;
        // ADR-0109: a blob from an image that predates the instance token
        // leaves this process's token in place. Seed it onto the scratch
        // state so commit cannot replace it with a second mint.
        if blob.counters.server_instance.is_none() {
            fresh.idspace.set_instance(self.idspace.instance());
        }
        let session_core = fresh.rebuild_sessions(blob);
        let window_core = fresh.rebuild_windows(blob, &session_core)?;
        let panes = fresh.rebuild_panes(blob, &window_core)?;
        fresh.relink_window_contents(blob, &window_core, &panes.core_ids)?;
        fresh.relink_session_windows(blob, &session_core, &window_core)?;
        fresh.restore_counters(blob);
        self.commit_rebuilt_tree(fresh);
        Ok(panes.exit_watchers)
    }

    /// Install a fully rebuilt tree, replacing only the tables reconstruction
    /// owns. Startup gates already written on `self` (config, hub, policy,
    /// upgrade context) stay put.
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
    fn rebuild_panes(
        &mut self,
        blob: &StateBlob,
        window_core: &HashMap<u32, WindowId>,
    ) -> Result<RebuiltPanes, RebuildError> {
        let scrollback = self.config.scrollback;
        let mut panes = RebuiltPanes {
            core_ids: HashMap::new(),
            exit_watchers: Vec::with_capacity(blob.panes.len()),
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
            // Pre-bind the wire id so `spawn_resource_actor`'s intern is a
            // no-op (it returns the existing mapping instead of allocating a
            // fresh one that would diverge from the blob).
            self.idspace
                .bind_terminal(core, WireResourceId::local(p.wire_id));
            let crate::terminal_actor::TerminalActorBundle {
                actor,
                handle,
                token,
                exit_notify,
            } = bundle;
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

    /// Close every pane the old image retained after its process exited
    /// (ADR-0124 §6). Each was rebuilt without a PTY only so it closes
    /// through its exit watcher like any other resource, with
    /// `RESOURCE_CLOSED { SERVER_SHUTDOWN }` and its `pane_closed`. Returns
    /// how many closed.
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

/// Build one pane's actor around its inherited PTY, or — for a pane the old
/// image handed off no PTY for — around a fresh no-PTY actor its snapshot is
/// replayed into. A pane carrying only one of the two PTY identities is a
/// corrupt handoff, not a no-PTY pane.
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
/// applies them, so seeding a fresh `Terminal` reproduces the same grid.
fn pane_seed(p: &PaneBlob) -> Vec<u8> {
    let mut seed = Vec::with_capacity(p.scrollback_bytes.len() + p.vt_replay_bytes.len());
    seed.extend_from_slice(&p.scrollback_bytes);
    seed.extend_from_slice(&p.vt_replay_bytes);
    seed
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
    use super::{RebuildError, validate_upgrade_blob};
    use crate::state::ServerState;
    use crate::terminal_actor::TerminalActor;
    use crate::upgrade::blob::{
        BLOB_VERSION, Counters, LayoutBlob, PaneBlob, SessionBlob, SplitDirBlob, StateBlob,
        WindowBlob,
    };
    use std::path::PathBuf;

    /// Walk a one-session/one-window/one-pane state into a blob: the tree
    /// links resolve by wire id and the pane carries the actor's replay
    /// snapshot.
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

    /// Build a state → blob → rebuild into a fresh state → blob again. The
    /// tree, wire ids, and counters round-trip exactly, and the rebuilt pane
    /// replays its seed.
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
            let exit_watchers = fresh.rebuild_from_blob(&blob).expect("rebuild");
            assert_eq!(
                exit_watchers.len(),
                blob.panes.len(),
                "every rebuilt pane must return an exit receiver for the runtime watcher"
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

    /// ADR-0109: the instance token changes exactly when pane ids can
    /// repeat. A cold start mints a new one; an upgrade, which restores the
    /// allocators, restores it too; a blob from an image that predates the
    /// token leaves the new image's fresh one in place (ids still cannot
    /// repeat, and no client can hold a token that image never issued).
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
                upgraded.rebuild_from_blob(&blob).expect("rebuild");
                assert_eq!(upgraded.idspace.instance(), token, "an upgrade keeps it");

                let mut legacy = state.build_upgrade_blob(7).await;
                legacy.counters.server_instance = None;
                let mut from_legacy = ServerState::new();
                let fresh = from_legacy.idspace.instance();
                from_legacy.rebuild_from_blob(&legacy).expect("rebuild");
                assert_eq!(
                    from_legacy.idspace.instance(),
                    fresh,
                    "nothing to restore: the fresh token stands"
                );
            })
            .await;
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

    /// A complete one-session/one-window/one-pane no-PTY blob rebuilds, and a
    /// marker session already on the destination is replaced only after the
    /// whole tree is ready.
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
                state.rebuild_from_blob(&handoff).expect("rebuild");
                assert_eq!(session_names(&state), vec!["main".to_owned()]);
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
                match state.rebuild_from_blob(&handoff) {
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

    /// A blob that passes topology checks but fails while building a pane
    /// actor must not install the sessions that were rebuilt before the
    /// failing pane.
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
                match state.rebuild_from_blob(&handoff) {
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
