//! The session tree and its ledgers: the [`Registry`], last-touch order
//! (for `AttachTarget::Last`), the two `defaults.cwd-inheritance` ledgers,
//! and keep-empty release tracking. Entries share the entity's lifetime and
//! are dropped in the reap cascade. Logic that also needs wire ids, actors,
//! or clients stays on `ServerState`, reaching in through
//! [`Self::registry`]. Everything is `pub(super)` and sync.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use phux_core::ids::{ResourceId, SessionId, WindowId};
use phux_core::registry::Registry;
use phux_core::session::Session;

use super::RenameOutcome;

/// The session/window/pane tree plus its per-session and per-window
/// ledgers.
#[derive(Debug)]
pub(super) struct SessionTable {
    /// Every session, window, and pane. `pub(super)` for disjoint-field
    /// borrows; read outside `state` via `ServerState::registry`.
    pub(super) registry: Registry,
    /// Per-session last-touch stamps resolving `AttachTarget::Last` (only
    /// the order matters).
    last_touched: HashMap<SessionId, u64>,
    /// Next last-touch stamp: monotonic, saturating (a wrap would reorder
    /// `Last`).
    next_touch_timestamp: u64,
    /// Frozen creation directory per session (`session-root` policy).
    roots: HashMap<SessionId, PathBuf>,
    /// Latest cwd per window (`last-cwd-per-window` policy).
    window_last_cwd: HashMap<WindowId, PathBuf>,
    /// Keep-empty sessions a group `KILL_RESOURCES` released (ADR-0105);
    /// their clients get `SESSION_KILLED` when the cascade removes them.
    released_keep_empty: HashSet<SessionId>,
    /// Removed released sessions whose clients still await detach.
    killed_pending: Vec<SessionId>,
}

impl Default for SessionTable {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionTable {
    /// Build an empty table with the last-touch clock at `1`.
    #[must_use]
    pub(super) fn new() -> Self {
        Self {
            registry: Registry::new(),
            last_touched: HashMap::new(),
            next_touch_timestamp: 1,
            roots: HashMap::new(),
            window_last_cwd: HashMap::new(),
            released_keep_empty: HashSet::new(),
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

    /// Look up the active pane of the active window of `session`, if any.
    #[must_use]
    pub(super) fn active_pane_of(&self, session: SessionId) -> Option<ResourceId> {
        let session = self.registry.session(session)?;
        let window_id = session.active?;
        let window = self.registry.window(window_id)?;
        window.active
    }

    /// Borrow the session named `name`, if it exists.
    #[must_use]
    pub(super) fn by_name(&self, name: &str) -> Option<&Session> {
        let id = self.find_by_name(name)?;
        self.registry.session(id)
    }

    /// The [`SessionId`] named `name`.
    pub(super) fn find_by_name(&self, name: &str) -> Option<SessionId> {
        self.registry
            .sessions()
            .find(|(_, s)| s.name == name)
            .map(|(id, _)| id)
    }

    /// The window owning `session`'s active pane.
    #[must_use]
    pub(super) fn active_window_of(&self, session: SessionId) -> Option<WindowId> {
        self.registry.session(session)?.active
    }

    /// `session`'s seed (oldest) pane.
    #[must_use]
    pub(super) fn seed_pane_of(&self, session: SessionId) -> Option<ResourceId> {
        let session = self.registry.session(session)?;
        let window_id = *session.windows.first()?;
        let window = self.registry.window(window_id)?;
        window.slots.first().copied()
    }

    // -- mutation -------------------------------------------------------

    /// Rename `current` to `new_name`; names are unique.
    pub(super) fn rename(&mut self, current: &str, new_name: &str) -> RenameOutcome {
        let Some(id) = self.find_by_name(current) else {
            return RenameOutcome::NotFound;
        };
        // Renaming to the same name succeeds.
        if current != new_name && self.find_by_name(new_name).is_some() {
            return RenameOutcome::NameTaken;
        }
        if let Some(session) = self.registry.session_mut(id) {
            new_name.clone_into(&mut session.name);
        }
        RenameOutcome::Renamed
    }

    /// Seed a session, window, and pane.
    ///
    /// # Panics
    ///
    /// Only on a `Registry` regression (the parents were just created).
    #[allow(clippy::expect_used, reason = "unreachable: parent just created")]
    pub(super) fn seed(&mut self, name: &str) -> (SessionId, WindowId, ResourceId) {
        let sid = self.registry.new_session(name.to_owned());
        let wid = self.registry.new_window(sid).expect("session just created");
        let pid = self
            .registry
            .new_terminal(wid)
            .expect("window just created");
        (sid, wid, pid)
    }

    /// Create a keep-empty session with no windows (ADR-0105).
    pub(super) fn seed_empty(&mut self, name: &str) -> SessionId {
        let sid = self.registry.new_session(name.to_owned());
        if let Some(session) = self.registry.session_mut(sid) {
            session.keep_empty = true;
        }
        sid
    }

    /// Add a pane to `session`'s first window, creating one if the session
    /// has none (ADR-0105). `None` if `session` is unknown.
    #[must_use]
    pub(super) fn add_pane(&mut self, session: SessionId) -> Option<ResourceId> {
        let existing = self.registry.session(session)?.windows.first().copied();
        let wid = match existing {
            Some(wid) => wid,
            None => self.registry.new_window(session).ok()?,
        };
        self.registry.new_terminal(wid).ok()
    }

    /// Add a pane to the window that owns `owner`.
    #[must_use]
    pub(super) fn add_pane_beside(&mut self, owner: ResourceId) -> Option<ResourceId> {
        let window = self.registry.resource(owner)?.window?;
        self.registry.new_terminal(window).ok()
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

    /// `window`'s latest recorded cwd.
    #[must_use]
    pub(super) fn last_cwd(&self, window: WindowId) -> Option<&PathBuf> {
        self.window_last_cwd.get(&window)
    }

    /// Record `window`'s latest cwd.
    pub(super) fn record_last_cwd(&mut self, window: WindowId, cwd: PathBuf) {
        self.window_last_cwd.insert(window, cwd);
    }

    // -- teardown -------------------------------------------------------

    /// Forget a removed window's cwd.
    pub(super) fn forget_window(&mut self, window: WindowId) {
        self.window_last_cwd.remove(&window);
    }

    /// Drop a removed session's last-touch stamp and frozen root.
    pub(super) fn forget_session(&mut self, session: SessionId) {
        self.last_touched.remove(&session);
        self.roots.remove(&session);
        self.released_keep_empty.remove(&session);
    }

    // -- ADR-0105 group-kill ledger -------------------------------------

    /// Remember that a group kill released `session`'s keep-empty mark.
    pub(super) fn mark_released(&mut self, session: SessionId) {
        self.released_keep_empty.insert(session);
    }

    /// Queue a removed, released session's clients for `SESSION_KILLED`.
    pub(super) fn note_removed(&mut self, session: SessionId) {
        if self.released_keep_empty.remove(&session) {
            self.killed_pending.push(session);
        }
    }

    /// Drain the removed, released sessions whose clients still need a detach.
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

    /// Restore a window's last-cwd entry recorded in an upgrade blob.
    pub(super) fn bind_last_cwd(&mut self, window: WindowId, cwd: PathBuf) {
        self.window_last_cwd.insert(window, cwd);
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
