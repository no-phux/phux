//! The organization registry and its per-session ledgers: last-touch order
//! (for `AttachTarget::Last`), the two `defaults.cwd-inheritance` ledgers,
//! and the sessions a commit deleted whose clients still await a detach.
//! Records change only through [`super::ServerState::commit`]; this table
//! holds what is not a record. Everything is `pub(super)` and sync.

use std::collections::HashMap;
use std::path::PathBuf;

use phux_core::ids::{ResourceId, SessionId};
use phux_core::registry::Registry;
use phux_core::resource::ResourceKind;
use phux_core::session::Session;

/// The registry plus its per-session ledgers.
#[derive(Debug)]
pub(super) struct SessionTable {
    /// Every session and resource record with its live handle.
    /// `pub(super)` for disjoint-field borrows; read outside `state` via
    /// `ServerState::registry`.
    pub(super) registry: Registry,
    /// Per-session last-touch stamps resolving `AttachTarget::Last` (only
    /// the order matters).
    last_touched: HashMap<SessionId, u64>,
    /// Next last-touch stamp: monotonic, saturating (a wrap would reorder
    /// `Last`).
    next_touch_timestamp: u64,
    /// Frozen creation directory per session (`session-root` policy).
    roots: HashMap<SessionId, PathBuf>,
    /// Latest cwd per session (`last-cwd-per-window` policy, which names the
    /// session's one view now that windows are gone).
    last_cwd: HashMap<SessionId, PathBuf>,
    /// Sessions a commit deleted whose clients still await
    /// `DETACHED { SESSION_KILLED }`.
    killed_pending: Vec<SessionId>,
}

impl SessionTable {
    /// Build a table over `registry` with the last-touch clock at `1`.
    #[must_use]
    pub(super) fn new(registry: Registry) -> Self {
        Self {
            registry,
            last_touched: HashMap::new(),
            next_touch_timestamp: 1,
            roots: HashMap::new(),
            last_cwd: HashMap::new(),
            killed_pending: Vec::new(),
        }
    }

    // -- last-touch ordering -------------------------------------------

    /// The most recently touched live session (`AttachTarget::Last`).
    #[must_use]
    pub(super) fn most_recently_touched(&self) -> Option<SessionId> {
        self.last_touched
            .iter()
            .filter(|(sid, _)| self.registry.session(**sid).is_some())
            .max_by_key(|(_, touched_at)| *touched_at)
            .map(|(sid, _)| *sid)
    }

    /// Whether any session was ever touched (live or not).
    #[must_use]
    pub(super) fn has_touch_history(&self) -> bool {
        !self.last_touched.is_empty()
    }

    /// Mark `session` as touched by attach/input/focus activity.
    pub(super) fn touch(&mut self, session: SessionId) {
        let touched_at = self.next_touch_timestamp;
        self.next_touch_timestamp = self.next_touch_timestamp.saturating_add(1);
        self.last_touched.insert(session, touched_at);
    }

    // -- session lookup -------------------------------------------------

    /// The Terminal a session-addressed action without a pane lands on
    /// (R14): the member that last took human input, else the first leaf
    /// of the arrangement's first group, else the first Terminal member.
    #[must_use]
    pub(super) fn default_pane_of(&self, session: SessionId) -> Option<ResourceId> {
        let record = self.registry.session(session)?;
        let is_terminal = |id: &ResourceId| {
            self.registry
                .resource(*id)
                .is_some_and(|r| r.kind == ResourceKind::Terminal)
        };
        if let Some(id) = record
            .recent
            .and_then(|k| self.registry.resource_id(&k))
            .filter(is_terminal)
        {
            return Some(id);
        }
        let arranged = record
            .arrangement
            .and_then(|k| self.registry.org().arrangement(&k))
            .and_then(|a| a.resources().into_iter().next())
            .and_then(|k| self.registry.resource_id(&k))
            .filter(is_terminal);
        arranged.or_else(|| self.seed_pane_of(session))
    }

    /// Borrow the session named `name`, if it exists.
    #[must_use]
    pub(super) fn by_name(&self, name: &str) -> Option<&Session> {
        self.registry.org().session_by_name(name)
    }

    /// The [`SessionId`] named `name`.
    pub(super) fn find_by_name(&self, name: &str) -> Option<SessionId> {
        self.registry.session_by_name(name)
    }

    /// `session`'s oldest Terminal member.
    #[must_use]
    pub(super) fn seed_pane_of(&self, session: SessionId) -> Option<ResourceId> {
        self.terminal_members(session).into_iter().next()
    }

    /// `session`'s Terminal members, in membership order.
    #[must_use]
    pub(super) fn terminal_members(&self, session: SessionId) -> Vec<ResourceId> {
        self.registry
            .members(session)
            .into_iter()
            .filter(|id| {
                self.registry
                    .resource(*id)
                    .is_some_and(|r| r.kind == ResourceKind::Terminal)
            })
            .collect()
    }

    // -- cwd-inheritance ledgers (phux-nyx) -----------------------------

    /// `session`'s frozen creation directory, if captured.
    #[must_use]
    pub(super) fn root(&self, session: SessionId) -> Option<&PathBuf> {
        self.roots.get(&session)
    }

    /// Record `session`'s root the first time only; returns the recorded
    /// root.
    pub(super) fn record_root(&mut self, session: SessionId, root: PathBuf) -> &PathBuf {
        self.roots.entry(session).or_insert(root)
    }

    /// `session`'s latest recorded cwd.
    #[must_use]
    pub(super) fn last_cwd(&self, session: SessionId) -> Option<&PathBuf> {
        self.last_cwd.get(&session)
    }

    /// Record `session`'s latest cwd.
    pub(super) fn record_last_cwd(&mut self, session: SessionId, cwd: PathBuf) {
        self.last_cwd.insert(session, cwd);
    }

    // -- teardown -------------------------------------------------------

    /// Drop a removed session's last-touch stamp, frozen root, and cwd.
    pub(super) fn forget_session(&mut self, session: SessionId) {
        self.last_touched.remove(&session);
        self.roots.remove(&session);
        self.last_cwd.remove(&session);
    }

    /// Queue a deleted session's clients for `SESSION_KILLED`.
    pub(super) fn note_removed(&mut self, session: SessionId) {
        self.killed_pending.push(session);
    }

    /// Drain the deleted sessions whose clients still need a detach.
    pub(super) fn take_killed(&mut self) -> Vec<SessionId> {
        std::mem::take(&mut self.killed_pending)
    }

    // -- graceful-upgrade round trip (ADR-0032) -------------------------

    /// This session's last-touch stamp, for the upgrade blob.
    #[must_use]
    pub(super) fn last_touched_at(&self, session: SessionId) -> Option<u64> {
        self.last_touched.get(&session).copied()
    }

    /// Restore a session's last-touch stamp from an upgrade blob.
    pub(super) fn bind_last_touched(&mut self, session: SessionId, touched_at: u64) {
        self.last_touched.insert(session, touched_at);
    }

    /// Restore a session's frozen root recorded in an upgrade blob.
    pub(super) fn bind_root(&mut self, session: SessionId, root: PathBuf) {
        self.roots.insert(session, root);
    }

    /// The next last-touch stamp (serialized in the upgrade blob).
    #[must_use]
    pub(super) const fn next_touch_timestamp(&self) -> u64 {
        self.next_touch_timestamp
    }

    /// Restore the last-touch clock after a graceful upgrade.
    pub(super) const fn set_next_touch_timestamp(&mut self, next: u64) {
        self.next_touch_timestamp = next;
    }
}
