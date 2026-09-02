//! The session tree and the per-session/per-window ledgers hung off it:
//! the canonical [`Registry`], the last-touch ordering that resolves
//! `AttachTarget::Last`, and the two cwd-inheritance ledgers (phux-nyx).
//!
//! Five fields that were flat on [`super::ServerState`] live here because
//! they share one lifetime — a session's (or one of its windows')
//! existence in the registry. An entry appears when the registry mints the
//! entity and every one of them disappears in `ServerState::reap_terminal`'s
//! cascade. Keeping them together is what makes "forget everything about
//! this session" one call instead of a `retire`/`remove`/`remove` triple
//! open-coded in `state::reap`, which is where they drifted out of step
//! before.
//!
//! `session_root` and `window_last_cwd` are deliberately in the same
//! struct: they are the two halves of the `defaults.cwd-inheritance`
//! ledger and are read and written together by `resolve_inherited_cwd`,
//! `assemble_upgrade_blob`, and `rebuild_from_blob`. Splitting them would
//! put one policy's state in two places.
//!
//! # Ownership boundary
//!
//! This type owns the *session tree and its ledgers*, not anything that
//! also needs a wire id, a pane actor, or a client record. Those stay on
//! `ServerState` and reach in through the `pub(super)` [`Self::registry`]
//! field (matching `state::config` and `state::client_table`):
//!
//! * `reap_terminal` / `reap_window_if_empty` cascade the registry removal
//!   and then retire wire ids, pane actor handles, per-Terminal metadata,
//!   and the agent-record arbiter; only the two `forget_*` calls belong
//!   here.
//! * `add_pane_to_terminal_owner` resolves a wire id through `state::id_space`
//!   before it can name a pane, so it stays on `ServerState` and calls
//!   [`Self::add_pane_beside`].
//! * `build_session_snapshot` / `attach_snapshot_panes` clone entities out
//!   of the registry precisely so they can interleave `&mut` wire-id
//!   interning; that clone-then-intern shape is load-bearing and stays put.
//! * `assemble_upgrade_blob` / `rebuild_from_blob` straddle this table, the
//!   id space, and the pane actors in one pass — including
//!   `next_touch_timestamp`, which rides in the same `Counters` struct as
//!   the id-space allocators — so they stay on `ServerState` and use the
//!   paired getters/setters below.
//! * `attach` / `attached_clients_to_detach` join a name lookup here to the
//!   client table.
//!
//! Nothing here is `async` and nothing awaits, so the state lock can never
//! be held across a suspension point through this type.
//!
//! The struct and every method are `pub(super)`: the accessors the runtime
//! calls stay on `ServerState` (see `state::sessions`, `state::cwd`), so
//! the four previously-private ledgers stay exactly as unreachable from
//! outside `state` as they were as private fields. The one previously-`pub`
//! field, `registry`, is reachable through the
//! [`super::ServerState::registry`] / [`super::ServerState::registry_mut`]
//! accessor pair, which is exactly as permissive as the bare field it
//! replaces.

use std::collections::HashMap;
use std::path::PathBuf;

use phux_core::ids::{SessionId, TerminalId, WindowId};
use phux_core::registry::Registry;
use phux_core::session::Session;

use super::RenameOutcome;

/// The canonical session/window/pane tree plus the per-session and
/// per-window ledgers keyed on it.
///
/// Held as a single field on [`super::ServerState`]. Not thread-safe on
/// its own; the surrounding `Mutex<ServerState>` provides synchronization.
#[derive(Debug)]
pub(super) struct SessionTable {
    /// Canonical domain state: every session, window, and pane the server
    /// owns (phux-byc.1/phux-byc.2).
    ///
    /// The one field here with readers outside `state`; they reach it
    /// through [`super::ServerState::registry`] /
    /// [`super::ServerState::registry_mut`]. `pub(super)` inside `state`
    /// so the cross-cluster methods that stay on `ServerState` keep
    /// disjoint-field borrow splitting against `idspace`, the client
    /// table, and the pane table.
    pub(super) registry: Registry,
    /// Per-session last-touch order used to resolve
    /// [`phux_protocol::wire::frame::AttachTarget::Last`].
    ///
    /// Updated by the runtime after successful attach and after valid
    /// input/focus dispatch. The value is a server-local monotonic
    /// timestamp: ordering is the only observable contract.
    last_touched: HashMap<SessionId, u64>,
    /// Next last-touch stamp to hand out. Monotonic from `1` and saturating
    /// rather than wrapping, because a wrapped stamp would silently reorder
    /// `AttachTarget::Last`. Private: [`Self::touch`] is its only legal
    /// mutator outside the graceful-upgrade restore
    /// ([`Self::set_next_touch_timestamp`]).
    next_touch_timestamp: u64,
    /// Frozen session-creation directory per session (phux-nyx,
    /// `defaults.cwd-inheritance = session-root`). Captured the first time
    /// a `session-root` spawn resolves a session's seed-pane CWD and reused
    /// thereafter, so later `cd`s in the seed pane do not move the root.
    /// Cleared by [`Self::forget_session`] with the rest of a session's
    /// bookkeeping on teardown.
    roots: HashMap<SessionId, PathBuf>,
    /// Most-recent observed working directory per window (phux-nyx,
    /// `defaults.cwd-inheritance = last-cwd-per-window`). Updated whenever a
    /// `last-cwd-per-window` spawn resolves the window's active-pane CWD;
    /// new panes in that window inherit the latest value. Cleared by
    /// [`Self::forget_window`] with the window's bookkeeping on teardown.
    window_last_cwd: HashMap<WindowId, PathBuf>,
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
        }
    }

    // -- last-touch ordering -------------------------------------------

    /// Most-recently-touched live session, if any. Resolves
    /// `AttachTarget::Last`.
    ///
    /// Filters against the registry so a killed session cannot win on a
    /// stale stamp.
    #[must_use]
    pub(super) fn most_recently_touched(&self) -> Option<SessionId> {
        self.last_touched
            .iter()
            .filter(|(sid, _)| self.registry.session(**sid).is_some())
            .max_by_key(|(_, touched_at)| *touched_at)
            .map(|(sid, _)| *sid)
    }

    /// Whether any session has ever been touched, including sessions that are
    /// no longer live. This distinguishes a genuinely untouched server from
    /// stale touch history when resolving `AttachTarget::Last`.
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
    pub(super) fn active_pane_of(&self, session: SessionId) -> Option<TerminalId> {
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

    /// Look up the [`SessionId`] for a name by scanning the registry.
    ///
    /// Uses [`Registry::sessions`] directly — no side ledger required.
    pub(super) fn find_by_name(&self, name: &str) -> Option<SessionId> {
        self.registry
            .sessions()
            .find(|(_, s)| s.name == name)
            .map(|(id, _)| id)
    }

    /// Resolve the window that owns `session`'s active pane, if any. The
    /// `last-cwd-per-window` policy keys its ledger on this window.
    #[must_use]
    pub(super) fn active_window_of(&self, session: SessionId) -> Option<WindowId> {
        self.registry.session(session)?.active
    }

    /// Resolve the seed (oldest) pane of `session` — the first pane of its
    /// first window. The `session-root` policy reads this pane's CWD to
    /// establish the session's creation directory.
    #[must_use]
    pub(super) fn seed_pane_of(&self, session: SessionId) -> Option<TerminalId> {
        let session = self.registry.session(session)?;
        let window_id = *session.windows.first()?;
        let window = self.registry.window(window_id)?;
        window.panes.first().copied()
    }

    // -- mutation -------------------------------------------------------

    /// Rename the session named `current` to `new_name`, in place.
    ///
    /// Mirrors `CREATE_SESSION`'s uniqueness rule: names are unique within
    /// the registry, so a `new_name` already in use is rejected. Resolution
    /// uses the same registry scan as every other name lookup (no side
    /// ledger, per [`Self::by_name`]), so there is nothing else to keep in
    /// sync.
    pub(super) fn rename(&mut self, current: &str, new_name: &str) -> RenameOutcome {
        let Some(id) = self.find_by_name(current) else {
            return RenameOutcome::NotFound;
        };
        // A no-op rename (current == new_name) resolves to the same session,
        // so the duplicate check would otherwise reject it. Treat it as
        // success: the name already is what was asked for.
        if current != new_name && self.find_by_name(new_name).is_some() {
            return RenameOutcome::NameTaken;
        }
        // Resolution above guarantees the id is live, so `session_mut` is
        // `Some`; the rename is a single field write.
        if let Some(session) = self.registry.session_mut(id) {
            new_name.clone_into(&mut session.name);
        }
        RenameOutcome::Renamed
    }

    /// Seed a session+window+pane. Returns the new
    /// `(SessionId, WindowId, TerminalId)`.
    ///
    /// # Panics
    ///
    /// Panics if the registry rejects the freshly-allocated session or
    /// window ids — both branches are unreachable because the parent
    /// entity was created on the line above. A panic here indicates a
    /// `phux-core::Registry` regression.
    #[allow(clippy::expect_used, reason = "unreachable: parent just created")]
    pub(super) fn seed(&mut self, name: &str) -> (SessionId, WindowId, TerminalId) {
        let sid = self.registry.new_session(name.to_owned());
        let wid = self.registry.new_window(sid).expect("session just created");
        let pid = self
            .registry
            .new_terminal(wid)
            .expect("window just created");
        (sid, wid, pid)
    }

    /// Add a new pane to `session`'s first window.
    ///
    /// Returns `None` if `session` is unknown or has no window —
    /// unreachable for a seeded session, which always has at least one
    /// window.
    #[must_use]
    pub(super) fn add_pane(&mut self, session: SessionId) -> Option<TerminalId> {
        let wid = self.registry.session(session)?.windows.first().copied()?;
        self.registry.new_terminal(wid).ok()
    }

    /// Add a pane to the exact window that owns `owner`.
    ///
    /// The registry half of `ServerState::add_pane_to_terminal_owner`,
    /// which resolves the caller's wire id through the id space before it
    /// can name `owner`.
    #[must_use]
    pub(super) fn add_pane_beside(&mut self, owner: TerminalId) -> Option<TerminalId> {
        let window = self.registry.terminal(owner)?.window;
        self.registry.new_terminal(window).ok()
    }

    // -- cwd-inheritance ledgers (phux-nyx) -----------------------------

    /// Read the frozen session-creation directory recorded for `session`
    /// under the `session-root` cwd-inheritance policy, if one has been
    /// captured.
    #[must_use]
    pub(super) fn root(&self, session: SessionId) -> Option<&PathBuf> {
        self.roots.get(&session)
    }

    /// Freeze `root` as `session`'s creation directory the first time it is
    /// observed; later calls are no-ops so a `cd` in the seed pane cannot
    /// move an already-recorded root. Returns the effective recorded root.
    pub(super) fn record_root(&mut self, session: SessionId, root: PathBuf) -> &PathBuf {
        self.roots.entry(session).or_insert(root)
    }

    /// Read the most-recent working directory recorded for `window` under
    /// the `last-cwd-per-window` cwd-inheritance policy, if any.
    #[must_use]
    pub(super) fn last_cwd(&self, window: WindowId) -> Option<&PathBuf> {
        self.window_last_cwd.get(&window)
    }

    /// Record `cwd` as `window`'s most-recent working directory,
    /// overwriting any prior value.
    pub(super) fn record_last_cwd(&mut self, window: WindowId, cwd: PathBuf) {
        self.window_last_cwd.insert(window, cwd);
    }

    // -- teardown -------------------------------------------------------

    /// Drop a removed window's ledger entry so a reused window id can never
    /// inherit a dead window's directory.
    pub(super) fn forget_window(&mut self, window: WindowId) {
        self.window_last_cwd.remove(&window);
    }

    /// Drop a removed session's last-touch stamp and frozen root.
    pub(super) fn forget_session(&mut self, session: SessionId) {
        self.last_touched.remove(&session);
        self.roots.remove(&session);
    }

    // -- graceful-upgrade round trip (ADR-0032) -------------------------

    /// This session's last-touch stamp, for the upgrade blob.
    #[must_use]
    pub(super) fn last_touched_at(&self, session: SessionId) -> Option<u64> {
        self.last_touched.get(&session).copied()
    }

    /// Restore a session's last-touch stamp recorded in an upgrade blob.
    /// Pair with [`Self::set_next_touch_timestamp`].
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

    /// The next last-touch stamp this table would hand out. Serialized in
    /// the upgrade blob's `Counters` alongside the id-space allocators.
    #[must_use]
    pub(super) const fn next_touch_timestamp(&self) -> u64 {
        self.next_touch_timestamp
    }

    /// Restore the last-touch clock after a graceful upgrade.
    pub(super) const fn set_next_touch_timestamp(&mut self, next: u64) {
        self.next_touch_timestamp = next;
    }
}
