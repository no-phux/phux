//! The server's wire-id space: the interning tables that map `phux-core`
//! slotmap keys to the `u32`-wide identifiers `phux-protocol` puts on the
//! wire (ADR-0016).
//!
//! Three independent id spaces live here — sessions, terminals, and windows
//! — because `phux-core` and `phux-protocol` must not depend on each other,
//! so `phux-server` is the one place that holds both halves of every
//! mapping. See [`crate::id_bridge`] for the allocation contract.
//!
//! # One bridge, one exhaustion semantic
//!
//! All three spaces are the same type — [`IdBridge`] at three
//! instantiations. They did not used to be: the session space was the
//! reusable bridge, the terminal space was that bridge open-coded, and the
//! window space was *half* of it (forward map only, no reverse). The three
//! also disagreed on `u32` exhaustion: sessions panicked, terminals and
//! windows saturated. Collapsing them had to pick one semantic, and it
//! picked **fail fast**, because saturating hands out `u32::MAX` and then
//! keeps handing out that same id, aliasing distinct terminals onto one wire
//! id and misrouting input and output — see [`crate::id_bridge`]'s
//! "Exhaustion" section for the full argument. Every space now hands out
//! `1..=u32::MAX - 1`.
//!
//! Two consequences worth naming rather than discovering:
//!
//! * The window space **gained** a reverse map it never had. That is a real
//!   addition, not accidental bloat: it is what makes windows the same
//!   structure as the other two. Nothing reads it yet
//!   (there is no `window_from_wire`), so
//!   [`Self::bind_window`] must keep binding distinct wire ids per window or
//!   the reverse entry silently collapses.
//! * The terminal and window spaces panic where they used to saturate, and
//!   their last mintable id moved from `u32::MAX` to `u32::MAX - 1`.
//!
//! # Still asymmetric: `TerminalId` is an enum
//!
//! `phux_protocol::ids::TerminalId` is a tagged union, not a `u32` newtype.
//! Only `TerminalId::Local` is ever minted here — a satellite terminal
//! (`TerminalId::Satellite { .. }`) is addressed directly off the wire id by
//! federation routing and never enters these tables, so
//! [`Self::terminal_from_wire`] returns `None` for one by design. Unifying
//! the three spaces did not lose that property; it gave it a name. Minting
//! shape is the [`WireId`](crate::id_bridge::WireId) trait, and
//! `impl WireId for phux_protocol::ids::TerminalId` is now the single place
//! the Local-only invariant is enforced.

use phux_core::ids::{SessionId, TerminalId, WindowId};
use phux_protocol::ids::{
    SessionId as WireSessionId, TerminalId as WireTerminalId, WindowId as WireWindowId,
};

use crate::id_bridge::IdBridge;

/// Every core-id ↔ wire-id mapping the server owns, plus the monotonic
/// allocators that mint fresh wire ids.
///
/// Held as a single field on [`ServerState`](crate::state::ServerState).
/// Not thread-safe on its own; the surrounding `Mutex<ServerState>`
/// provides synchronization.
///
/// Wire ids start at `1` (`0` is reserved as a sentinel) and are never
/// reused: retiring an entity drops both directions of its mapping but
/// leaves the allocator where it is.
#[derive(Debug)]
pub struct IdSpace {
    /// Bridge between core slotmap [`SessionId`]s and wire-level
    /// `phux_protocol::ids::SessionId` (u32).
    sessions: IdBridge<SessionId, WireSessionId>,
    /// Bridge between core [`TerminalId`]s and wire-level
    /// `phux_protocol::ids::TerminalId`. Its reverse direction is
    /// load-bearing: it is the existence oracle every Terminal-scoped
    /// command validates against.
    terminals: IdBridge<TerminalId, WireTerminalId>,
    /// Bridge between core [`WindowId`]s and wire-level
    /// `phux_protocol::ids::WindowId`; used to populate
    /// [`phux_protocol::wire::info::WindowInfo::id`] in the `ATTACHED`
    /// snapshot.
    windows: IdBridge<WindowId, WireWindowId>,
}

impl Default for IdSpace {
    fn default() -> Self {
        Self::new()
    }
}

impl IdSpace {
    /// Build an empty id space with all three allocators at `1`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            sessions: IdBridge::new(),
            terminals: IdBridge::new(),
            windows: IdBridge::new(),
        }
    }

    // -- sessions -----------------------------------------------------

    /// Wire session id for `core`, allocating one if needed. Idempotent.
    ///
    /// # Panics
    ///
    /// Panics if the session wire-id space is exhausted — see
    /// [`IdBridge::intern`].
    pub fn intern_session(&mut self, core: SessionId) -> WireSessionId {
        self.sessions.intern(core)
    }

    /// Forward lookup without allocating. `None` if `core` was never
    /// interned.
    #[must_use]
    pub fn session_wire(&self, core: SessionId) -> Option<WireSessionId> {
        self.sessions.wire(core).copied()
    }

    /// Reverse lookup: which core session (if any) does `wire` name?
    #[must_use]
    pub fn resolve_session(&self, wire: WireSessionId) -> Option<SessionId> {
        self.sessions.resolve(&wire)
    }

    /// Bind a specific `core → wire` session mapping recorded in a
    /// graceful-upgrade state blob (ADR-0032) rather than allocating a
    /// fresh id. Pair with [`Self::set_next_session_wire`].
    pub fn bind_session(&mut self, core: SessionId, wire: WireSessionId) {
        self.sessions.bind(core, wire);
    }

    /// Drop both directions of `core`'s session mapping. Idempotent; the
    /// retired wire id is not reused.
    pub fn forget_session(&mut self, core: SessionId) {
        let _ = self.sessions.forget(core);
    }

    /// The next session wire id this space would allocate.
    #[must_use]
    pub const fn next_session_wire(&self) -> u32 {
        self.sessions.next_wire()
    }

    /// Restore the session allocator after a graceful upgrade.
    pub const fn set_next_session_wire(&mut self, next: u32) {
        self.sessions.set_next(next);
    }

    // -- terminals ----------------------------------------------------

    /// Wire pane id for `terminal`, allocating one if needed.
    ///
    /// Idempotent: a second call for the same `terminal` returns the same
    /// wire id. Several runtime call sites rely on that (they re-intern
    /// rather than thread the id through), so it must stay so.
    ///
    /// # Panics
    ///
    /// Panics if the terminal wire-id space is exhausted — see
    /// [`IdBridge::intern`].
    pub(super) fn intern_terminal(&mut self, terminal: TerminalId) -> WireTerminalId {
        self.terminals.intern(terminal)
    }

    /// Reverse lookup: which core pane id (if any) does `wire` resolve to?
    ///
    /// `None` for a `TerminalId::Satellite` by design — see the module doc.
    #[must_use]
    pub(super) fn terminal_from_wire(&self, wire: &WireTerminalId) -> Option<TerminalId> {
        self.terminals.resolve(wire)
    }

    /// Forward lookup without allocating. `None` if `terminal` was never
    /// interned.
    #[must_use]
    pub(super) fn terminal_wire(&self, terminal: TerminalId) -> Option<&WireTerminalId> {
        self.terminals.wire(terminal)
    }

    /// Bind a specific `core → wire` pane mapping from a graceful-upgrade
    /// state blob (ADR-0032). Pre-binding is what makes the subsequent
    /// [`Self::intern_terminal`] a no-op instead of minting a fresh id that
    /// would diverge from the blob.
    pub(super) fn bind_terminal(&mut self, terminal: TerminalId, wire: WireTerminalId) {
        self.terminals.bind(terminal, wire);
    }

    /// Drop both directions of `terminal`'s mapping and hand the retired
    /// wire id back.
    ///
    /// The caller needs it: the per-Terminal L3 metadata scope and the
    /// agent-record arbiter are both keyed by wire id, and they are only
    /// reachable while this mapping still exists. Returns `None` if
    /// `terminal` was never interned. The wire id is not reused.
    pub(super) fn retire_terminal(&mut self, terminal: TerminalId) -> Option<WireTerminalId> {
        self.terminals.forget(terminal)
    }

    /// The next pane wire id this space would allocate.
    #[must_use]
    pub(super) const fn next_terminal_wire(&self) -> u32 {
        self.terminals.next_wire()
    }

    /// Restore the pane allocator after a graceful upgrade.
    pub(super) const fn set_next_terminal_wire(&mut self, next: u32) {
        self.terminals.set_next(next);
    }

    // -- windows ------------------------------------------------------

    /// Wire window id for `window`, allocating one if needed. Idempotent.
    ///
    /// # Panics
    ///
    /// Panics if the window wire-id space is exhausted — see
    /// [`IdBridge::intern`].
    pub(super) fn intern_window(&mut self, window: WindowId) -> WireWindowId {
        self.windows.intern(window)
    }

    /// Forward lookup without allocating. `None` if `window` was never
    /// interned.
    #[must_use]
    pub(super) fn window_wire(&self, window: WindowId) -> Option<WireWindowId> {
        self.windows.wire(window).copied()
    }

    /// Bind a specific `core → wire` window mapping from a
    /// graceful-upgrade state blob (ADR-0032). Pair with
    /// [`Self::set_next_window_wire`].
    pub(super) fn bind_window(&mut self, window: WindowId, wire: WireWindowId) {
        self.windows.bind(window, wire);
    }

    /// Drop `window`'s mapping. Idempotent; the retired wire id is not
    /// reused.
    pub(super) fn retire_window(&mut self, window: WindowId) {
        let _ = self.windows.forget(window);
    }

    /// The next window wire id this space would allocate.
    #[must_use]
    pub(super) const fn next_window_wire(&self) -> u32 {
        self.windows.next_wire()
    }

    /// Restore the window allocator after a graceful upgrade.
    pub(super) const fn set_next_window_wire(&mut self, next: u32) {
        self.windows.set_next(next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_core::registry::Registry;

    /// Two distinct core ids in each space, from one registry.
    fn two_of_each() -> (Registry, [SessionId; 2], [WindowId; 2], [TerminalId; 2]) {
        let mut reg = Registry::new();
        let s0 = reg.new_session("s0".to_owned());
        let s1 = reg.new_session("s1".to_owned());
        let w0 = reg.new_window(s0).expect("window 0");
        let w1 = reg.new_window(s1).expect("window 1");
        let t0 = reg.new_terminal(w0).expect("terminal 0");
        let t1 = reg.new_terminal(w1).expect("terminal 1");
        (reg, [s0, s1], [w0, w1], [t0, t1])
    }

    // -- the u32 boundary, pinned per space ---------------------------
    //
    // One semantic for all three: the call that would mint `u32::MAX`
    // panics, so `u32::MAX - 1` is the last id any space hands out. The
    // paired non-panicking tests exist because a `#[should_panic]` test
    // cannot assert anything after the panic.

    #[test]
    fn session_space_mints_up_to_u32_max_minus_one() {
        let (_reg, sessions, _, _) = two_of_each();
        let mut space = IdSpace::new();
        space.set_next_session_wire(u32::MAX - 1);
        assert_eq!(
            space.intern_session(sessions[0]),
            WireSessionId(u32::MAX - 1)
        );
    }

    #[test]
    #[should_panic(expected = "session wire-id space exhausted")]
    fn session_space_panics_instead_of_minting_u32_max() {
        let (_reg, sessions, _, _) = two_of_each();
        let mut space = IdSpace::new();
        space.set_next_session_wire(u32::MAX - 1);
        let _ = space.intern_session(sessions[0]);
        let _ = space.intern_session(sessions[1]);
    }

    #[test]
    fn terminal_space_mints_up_to_u32_max_minus_one() {
        let (_reg, _, _, terminals) = two_of_each();
        let mut space = IdSpace::new();
        space.set_next_terminal_wire(u32::MAX - 1);
        assert_eq!(
            space.intern_terminal(terminals[0]),
            WireTerminalId::local(u32::MAX - 1)
        );
    }

    #[test]
    #[should_panic(expected = "terminal wire-id space exhausted")]
    fn terminal_space_panics_instead_of_saturating() {
        let (_reg, _, _, terminals) = two_of_each();
        let mut space = IdSpace::new();
        space.set_next_terminal_wire(u32::MAX - 1);
        let _ = space.intern_terminal(terminals[0]);
        let _ = space.intern_terminal(terminals[1]);
    }

    #[test]
    fn window_space_mints_up_to_u32_max_minus_one() {
        let (_reg, _, windows, _) = two_of_each();
        let mut space = IdSpace::new();
        space.set_next_window_wire(u32::MAX - 1);
        assert_eq!(space.intern_window(windows[0]), WireWindowId(u32::MAX - 1));
    }

    #[test]
    #[should_panic(expected = "window wire-id space exhausted")]
    fn window_space_panics_instead_of_saturating() {
        let (_reg, _, windows, _) = two_of_each();
        let mut space = IdSpace::new();
        space.set_next_window_wire(u32::MAX - 1);
        let _ = space.intern_window(windows[0]);
        let _ = space.intern_window(windows[1]);
    }

    // -- the TerminalId-is-an-enum invariant --------------------------

    #[test]
    fn intern_terminal_mints_a_local_id() {
        let (_reg, _, _, terminals) = two_of_each();
        let mut space = IdSpace::new();
        let wire = space.intern_terminal(terminals[0]);
        assert_eq!(wire, WireTerminalId::local(1));
        assert!(wire.is_local(), "only Local ids are minted here");
    }

    #[test]
    fn terminal_from_wire_is_none_for_a_satellite_id() {
        let (_reg, _, _, terminals) = two_of_each();
        let mut space = IdSpace::new();
        let wire = space.intern_terminal(terminals[0]);
        let raw = wire.local_id().expect("minted id is Local");
        let satellite = WireTerminalId::satellite("peer", raw);
        assert!(
            space.terminal_from_wire(&satellite).is_none(),
            "satellite terminals are routed by federation and never interned"
        );
        assert_eq!(space.terminal_from_wire(&wire), Some(terminals[0]));
    }

    // -- shape parity across the three spaces -------------------------

    #[test]
    fn every_space_allocates_monotonically_from_one() {
        let (_reg, sessions, windows, terminals) = two_of_each();
        let mut space = IdSpace::new();
        assert_eq!(space.intern_session(sessions[0]), WireSessionId(1));
        assert_eq!(space.intern_session(sessions[1]), WireSessionId(2));
        assert_eq!(space.intern_window(windows[0]), WireWindowId(1));
        assert_eq!(space.intern_window(windows[1]), WireWindowId(2));
        assert_eq!(
            space.intern_terminal(terminals[0]),
            WireTerminalId::local(1)
        );
        assert_eq!(
            space.intern_terminal(terminals[1]),
            WireTerminalId::local(2)
        );
    }

    #[test]
    fn retiring_does_not_recycle_wire_ids() {
        let (_reg, sessions, windows, terminals) = two_of_each();
        let mut space = IdSpace::new();

        let s0 = space.intern_session(sessions[0]);
        space.forget_session(sessions[0]);
        assert!(space.resolve_session(s0).is_none());
        assert_eq!(space.intern_session(sessions[1]), WireSessionId(2));

        let t0 = space.intern_terminal(terminals[0]);
        assert_eq!(space.retire_terminal(terminals[0]), Some(t0.clone()));
        assert!(space.terminal_from_wire(&t0).is_none());
        assert_eq!(
            space.intern_terminal(terminals[1]),
            WireTerminalId::local(2)
        );

        let _ = space.intern_window(windows[0]);
        space.retire_window(windows[0]);
        assert!(space.window_wire(windows[0]).is_none());
        assert_eq!(space.intern_window(windows[1]), WireWindowId(2));
    }
}
