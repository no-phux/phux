//! The server's wire-id space (ADR-0016): the sessions, terminals, and
//! windows bridges between `phux-core` keys and `phux-protocol` `u32` ids,
//! held here because those crates must not depend on each other.
//!
//! All three are an [`IdBridge`] with one exhaustion rule: fail fast rather
//! than alias (see [`crate::id_bridge`]), so ids run `1..=u32::MAX - 1`.
//! Only `ResourceId::Local` is minted; satellite terminals never enter
//! these tables, so [`Self::terminal_from_wire`] returns `None` for them.

use phux_core::ids::{ResourceId, SessionId, WindowId};
use phux_protocol::ids::{
    ResourceId as WireResourceId, SessionId as WireSessionId, WindowId as WireWindowId,
};

use crate::id_bridge::IdBridge;

/// Every core-id ↔ wire-id mapping and the monotonic allocators. Wire ids
/// start at 1 and are never reused.
#[derive(Debug)]
pub struct IdSpace {
    /// Session bridge.
    sessions: IdBridge<SessionId, WireSessionId>,
    /// Terminal bridge; its reverse map is the existence check for every
    /// Terminal-scoped command.
    terminals: IdBridge<ResourceId, WireResourceId>,
    /// Window bridge.
    windows: IdBridge<WindowId, WireWindowId>,
    /// The terminal id space's instance token (ADR-0109), minted with the
    /// allocator; an upgrade restores both.
    instance: phux_protocol::ids::ServerInstance,
}

/// Mint a random instance token from the OS CSPRNG.
#[allow(
    clippy::expect_used,
    reason = "a server cannot safely hand out ids without a token that names their space"
)]
fn fresh_instance() -> phux_protocol::ids::ServerInstance {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes).expect("OS CSPRNG unavailable for the instance token");
    phux_protocol::ids::ServerInstance::new(bytes)
}

impl Default for IdSpace {
    fn default() -> Self {
        Self::new()
    }
}

impl IdSpace {
    /// An empty id space with a fresh instance token.
    #[must_use]
    pub fn new() -> Self {
        Self {
            sessions: IdBridge::new(),
            terminals: IdBridge::new(),
            windows: IdBridge::new(),
            instance: fresh_instance(),
        }
    }

    // -- instance token (ADR-0109) -------------------------------------

    /// The instance token naming this id space.
    #[must_use]
    pub const fn instance(&self) -> phux_protocol::ids::ServerInstance {
        self.instance
    }

    /// Adopt `instance` after an upgrade restored its allocators.
    pub(super) const fn set_instance(&mut self, instance: phux_protocol::ids::ServerInstance) {
        self.instance = instance;
    }

    // -- sessions -----------------------------------------------------

    /// Wire session id for `core`, allocating if needed.
    ///
    /// # Panics
    ///
    /// If the space is exhausted (see [`IdBridge::intern`]).
    pub fn intern_session(&mut self, core: SessionId) -> WireSessionId {
        self.sessions.intern(core)
    }

    /// Forward lookup without allocating.
    #[must_use]
    pub fn session_wire(&self, core: SessionId) -> Option<WireSessionId> {
        self.sessions.wire(core).copied()
    }

    /// Reverse lookup: which core session (if any) does `wire` name?
    #[must_use]
    pub fn resolve_session(&self, wire: WireSessionId) -> Option<SessionId> {
        self.sessions.resolve(&wire)
    }

    /// Bind a session mapping from an upgrade blob (ADR-0032).
    pub fn bind_session(&mut self, core: SessionId, wire: WireSessionId) {
        self.sessions.bind(core, wire);
    }

    /// Drop `core`'s session mapping (the id is not reused).
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

    /// Wire terminal id, allocating if needed; idempotent (callers rely on
    /// re-interning).
    ///
    /// # Panics
    ///
    /// If the space is exhausted.
    pub(super) fn intern_terminal(&mut self, terminal: ResourceId) -> WireResourceId {
        self.terminals.intern(terminal)
    }

    /// Reverse lookup; `None` for satellite ids by design.
    #[must_use]
    pub(super) fn terminal_from_wire(&self, wire: &WireResourceId) -> Option<ResourceId> {
        self.terminals.resolve(wire)
    }

    /// Forward lookup without allocating.
    #[must_use]
    pub(super) fn terminal_wire(&self, terminal: ResourceId) -> Option<&WireResourceId> {
        self.terminals.wire(terminal)
    }

    /// Bind a terminal mapping from an upgrade blob so the later intern is a
    /// no-op.
    pub(super) fn bind_terminal(&mut self, terminal: ResourceId, wire: WireResourceId) {
        self.terminals.bind(terminal, wire);
    }

    /// Drop `terminal`'s mapping and return the retired wire id (the
    /// metadata scope and arbiter are keyed by it). Not reused.
    pub(super) fn retire_terminal(&mut self, terminal: ResourceId) -> Option<WireResourceId> {
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

    /// Wire window id, allocating if needed.
    ///
    /// # Panics
    ///
    /// If the space is exhausted.
    pub(super) fn intern_window(&mut self, window: WindowId) -> WireWindowId {
        self.windows.intern(window)
    }

    /// Forward lookup without allocating.
    #[must_use]
    pub(super) fn window_wire(&self, window: WindowId) -> Option<WireWindowId> {
        self.windows.wire(window).copied()
    }

    /// Bind a window mapping from an upgrade blob.
    pub(super) fn bind_window(&mut self, window: WindowId, wire: WireWindowId) {
        self.windows.bind(window, wire);
    }

    /// Drop `window`'s mapping (not reused).
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
    fn two_of_each() -> (Registry, [SessionId; 2], [WindowId; 2], [ResourceId; 2]) {
        let mut reg = Registry::new();
        let s0 = reg.new_session("s0".to_owned());
        let s1 = reg.new_session("s1".to_owned());
        let w0 = reg.new_window(s0).expect("window 0");
        let w1 = reg.new_window(s1).expect("window 1");
        let t0 = reg.new_terminal(w0).expect("terminal 0");
        let t1 = reg.new_terminal(w1).expect("terminal 1");
        (reg, [s0, s1], [w0, w1], [t0, t1])
    }

    // --- the u32 boundary: `u32::MAX - 1` is the last id minted ---

    #[test]
    #[should_panic(expected = "terminal wire-id space exhausted")]
    fn terminal_space_panics_instead_of_saturating() {
        let (_reg, _, _, terminals) = two_of_each();
        let mut space = IdSpace::new();
        space.set_next_terminal_wire(u32::MAX - 1);
        let _ = space.intern_terminal(terminals[0]);
        let _ = space.intern_terminal(terminals[1]);
    }

    // -- the ResourceId-is-an-enum invariant --------------------------

    #[test]
    fn terminal_from_wire_is_none_for_a_satellite_id() {
        let (_reg, _, _, terminals) = two_of_each();
        let mut space = IdSpace::new();
        let wire = space.intern_terminal(terminals[0]);
        let raw = wire.local_id().expect("minted id is Local");
        let satellite = WireResourceId::satellite("peer", raw);
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
            WireResourceId::local(1)
        );
        assert_eq!(
            space.intern_terminal(terminals[1]),
            WireResourceId::local(2)
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
            WireResourceId::local(2)
        );

        let _ = space.intern_window(windows[0]);
        space.retire_window(windows[0]);
        assert!(space.window_wire(windows[0]).is_none());
        assert_eq!(space.intern_window(windows[1]), WireWindowId(2));
    }
}
