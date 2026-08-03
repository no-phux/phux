//! The graceful-upgrade state blob's producer and consumer (ADR-0032):
//! [`ServerState::build_upgrade_blob`] walks the live tree into a
//! [`StateBlob`](crate::upgrade::blob::StateBlob), and
//! [`ServerState::rebuild_from_blob`] reconstructs the tree from one in the
//! re-exec'd image.

use std::collections::HashMap;
use std::os::fd::RawFd;
use std::path::PathBuf;
use std::time::{Duration, UNIX_EPOCH};

use phux_core::ids::{SessionId, TerminalId, WindowId};
use phux_core::terminal::TerminalDescriptor;
use phux_core::window::{LayoutNode, SplitDir};
use phux_protocol::ids::{
    SessionId as WireSessionId, TerminalId as WireTerminalId, WindowId as WireWindowId,
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::ServerState;
use crate::terminal_actor::{
    PaneUpgradeHandle, TerminalActor, TerminalHandle, UpgradeHandleRequest,
};
use crate::upgrade::blob::{
    BLOB_VERSION, Counters, LayoutBlob, PaneBlob, SessionBlob, SplitDirBlob, StateBlob, WindowBlob,
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
        let tids: Vec<TerminalId> = self.terminal_table.terminal_ids();
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

    /// Clone every pane's [`TerminalHandle`] so the runtime can query each
    /// actor's upgrade handoff *outside* the `ServerState` lock (it can't hold
    /// the `Arc<Mutex<_>>` across the await; see
    /// [`Self::assemble_upgrade_blob`]).
    pub(crate) fn upgrade_handles(&self) -> Vec<(TerminalId, TerminalHandle)> {
        self.all_terminal_handles()
    }

    /// Assemble the [`StateBlob`] from the live tree plus a pre-fetched map of
    /// per-pane handoffs — synchronous, so the runtime can call it under the
    /// state lock after gathering the handoffs out of lock.
    pub(crate) fn assemble_upgrade_blob(
        &self,
        listener_fd: RawFd,
        handoffs: &HashMap<TerminalId, PaneUpgradeHandle>,
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
                    .map(|d| d.as_nanos())
                    .unwrap_or(0),
                last_touched: self.sessions.last_touched_at(sid),
                root: self.sessions.root(sid).cloned(),
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
                        .panes
                        .iter()
                        .filter_map(|t| self.terminal_wire(*t))
                        .collect(),
                    active_pane: window.active.and_then(|t| self.terminal_wire(t)),
                    layout: window.layout.as_ref().and_then(|l| self.layout_to_blob(l)),
                    last_cwd: self.sessions.last_cwd(wid).cloned(),
                });

                for &tid in &window.panes {
                    let (Some(desc), Some(pane_wire)) = (
                        self.sessions.registry.terminal(tid),
                        self.terminal_wire(tid),
                    ) else {
                        continue;
                    };
                    let handoff = handoffs.get(&tid).cloned();
                    panes.push(pane_blob(
                        pane_wire,
                        window_wire,
                        desc,
                        &self.config.term,
                        handoff,
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
            },
            sessions,
            windows,
            panes,
        }
    }

    /// Ask one pane's actor for its upgrade handoff. `None` when the pane has
    /// no registered handle or the actor has gone away.
    async fn request_pane_handoff(&self, tid: TerminalId) -> Option<PaneUpgradeHandle> {
        let handle = self.terminal_handle(tid)?;
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

    fn terminal_wire(&self, tid: TerminalId) -> Option<u32> {
        self.idspace
            .terminal_wire(tid)
            .and_then(phux_protocol::TerminalId::local_id)
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
    desc: &TerminalDescriptor,
    term: &str,
    handoff: Option<PaneUpgradeHandle>,
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
        master_fd: handoff.as_ref().and_then(|h| h.master_fd),
        vt_replay_bytes: handoff
            .as_ref()
            .map(|h| h.vt_replay_bytes.clone())
            .unwrap_or_default(),
        scrollback_bytes: handoff.map(|h| h.scrollback_bytes).unwrap_or_default(),
    }
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
    /// Must run inside the `LocalSet` that owns pane actors (it spawns them).
    ///
    /// # Errors
    /// [`RebuildError`] on a registry insertion failure, an actor build
    /// failure, or a dangling wire-id reference in the blob.
    #[allow(
        clippy::too_many_lines,
        reason = "linear reconstruction: create entities, bind wire ids, spawn actors, re-link the tree, restore counters — three short passes whose order is the meaning; splitting fragments it."
    )]
    #[allow(
        clippy::type_complexity,
        reason = "the runtime immediately consumes each rebuilt pane id and its one-shot exit receiver"
    )]
    pub fn rebuild_from_blob(
        &mut self,
        blob: &StateBlob,
    ) -> Result<Vec<(TerminalId, oneshot::Receiver<Option<i32>>)>, RebuildError> {
        let max_scrollback = self.config.history_limit;
        let mut session_core: HashMap<u32, SessionId> = HashMap::new();
        let mut window_core: HashMap<u32, WindowId> = HashMap::new();
        let mut pane_core: HashMap<u32, TerminalId> = HashMap::new();
        let mut exit_watchers = Vec::with_capacity(blob.panes.len());

        for s in &blob.sessions {
            let core = self.sessions.registry.new_session(s.name.clone());
            self.idspace
                .bind_session(core, WireSessionId::new(s.wire_id));
            if let Some(sess) = self.sessions.registry.session_mut(core)
                && let Some(created) = unix_nanos_to_systemtime(s.created_at_unix_nanos)
            {
                sess.created_at = created;
            }
            if let Some(ts) = s.last_touched {
                self.sessions.bind_last_touched(core, ts);
            }
            if let Some(root) = &s.root {
                self.sessions.bind_root(core, root.clone());
            }
            session_core.insert(s.wire_id, core);
        }

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

            let seed = pane_seed(p);
            let bundle = match (p.master_fd, p.child_pid) {
                (Some(master_fd), Some(child_pid)) => TerminalActor::new_with_adopted_pty(
                    master_fd,
                    child_pid,
                    p.cols,
                    p.rows,
                    max_scrollback,
                    CancellationToken::new(),
                    &seed,
                )?,
                _ => TerminalActor::new_with_seed(p.cols, p.rows, &seed)?,
            };

            // Pre-bind the wire id so `spawn_terminal_actor`'s intern is a
            // no-op (it returns the existing mapping instead of allocating a
            // fresh one that would diverge from the blob).
            self.idspace
                .bind_terminal(core, WireTerminalId::local(p.wire_id));
            let crate::terminal_actor::TerminalActorBundle {
                actor,
                handle,
                token,
                exit_notify,
            } = bundle;
            self.spawn_terminal_actor(core, handle, token, actor.run());
            if let Some(exit_notify) = exit_notify {
                exit_watchers.push((core, exit_notify));
            }
            pane_core.insert(p.wire_id, core);
        }

        // Re-apply window pane order / active / layout (the auto-split layout
        // `new_terminal` produced is discarded for the blob's).
        for w in &blob.windows {
            let Some(&core) = window_core.get(&w.wire_id) else {
                continue;
            };
            let panes = resolve_ids(&w.pane_wire_ids, &pane_core);
            let active = w.active_pane.and_then(|id| pane_core.get(&id).copied());
            let layout = w
                .layout
                .as_ref()
                .and_then(|l| layout_from_blob(l, &pane_core));
            if let Some(win) = self.sessions.registry.window_mut(core) {
                win.panes = panes;
                win.active = active;
                win.layout = layout;
            }
        }

        // Re-apply session window order / active.
        for s in &blob.sessions {
            let Some(&core) = session_core.get(&s.wire_id) else {
                continue;
            };
            let windows = resolve_ids(&s.window_wire_ids, &window_core);
            let active = s.active_window.and_then(|id| window_core.get(&id).copied());
            if let Some(sess) = self.sessions.registry.session_mut(core) {
                sess.windows = windows;
                sess.active = active;
            }
        }

        // Restore the allocators above every restored id.
        self.idspace
            .set_next_session_wire(blob.counters.next_session_wire_id);
        self.idspace
            .set_next_terminal_wire(blob.counters.next_terminal_wire_id);
        self.idspace
            .set_next_window_wire(blob.counters.next_window_wire_id);
        self.sessions
            .set_next_touch_timestamp(blob.counters.next_touch_timestamp);

        Ok(exit_watchers)
    }
}

/// Resolve a list of wire ids to their rebuilt core ids, dropping any that
/// didn't resolve (a dangling reference is silently skipped at the list level;
/// structural references error in `rebuild_from_blob`).
fn resolve_ids<K: Copy>(wire_ids: &[u32], map: &HashMap<u32, K>) -> Vec<K> {
    wire_ids
        .iter()
        .filter_map(|id| map.get(id).copied())
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
/// ids to core ids. `None` if any referenced pane is missing.
fn layout_from_blob(node: &LayoutBlob, panes: &HashMap<u32, TerminalId>) -> Option<LayoutNode> {
    match node {
        LayoutBlob::Leaf(wire) => panes.get(wire).copied().map(LayoutNode::Leaf),
        LayoutBlob::Split {
            dir,
            ratio,
            left,
            right,
        } => Some(LayoutNode::Split {
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
    use crate::state::ServerState;
    use crate::terminal_actor::TerminalActor;

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
                    .register_terminal_handle(tid, handle, token)
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
        local
            .run_until(async {
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
                state.register_terminal_handle(tid, bundle.handle, bundle.token);

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
            })
            .await;
    }
}
