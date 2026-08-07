//! The server's wire-id space: the interning tables that map `phux-core`
//! slotmap keys to the `u32`-wide identifiers `phux-protocol` puts on the
//! wire (ADR-0016).
//!
//! Three independent id spaces live here — sessions, terminals, and windows
//! — because `phux-core` and `phux-protocol` must not depend on each other,
//! so `phux-server` is the one place that holds both halves of every
//! mapping. See [`crate::id_bridge`] for the allocation contract; the
//! terminal and window spaces follow the same shape.
//!
//! # Why sessions go through `IdBridge` and the others do not
//!
//! The session space is the reusable [`IdBridge`]; the terminal and window
//! spaces are the same data structure open-coded on this type. That
//! asymmetry is deliberate and predates this module: the two are not
//! behaviourally interchangeable.
//!
//! * [`IdBridge::intern`] panics on `u32` exhaustion;
//!   [`IdSpace::intern_terminal`] / [`IdSpace::intern_window`] saturate.
//! * `phux_protocol::ids::TerminalId` is an enum, not a newtype. Only
//!   `TerminalId::Local` is ever minted here — a satellite terminal
//!   (`TerminalId::Satellite { .. }`) is addressed directly off the wire id
//!   by federation routing and never enters these tables, so
//!   [`IdSpace::terminal_from_wire`] returns `None` for one by design.
//!
//! Collapsing them into one generic bridge would change one of those
//! behaviours; it stays out of scope here.

use std::collections::HashMap;

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
    session_id_bridge: IdBridge,
    /// Wire-side identifier for each core pane id. Allocated monotonically
    /// from `1`. Mirrors the [`IdBridge`] shape used for session ids — see
    /// the module doc for why it is open-coded rather than shared.
    terminal_forward: HashMap<TerminalId, WireTerminalId>,
    /// Reverse of [`Self::terminal_forward`]. Load-bearing: it is the
    /// existence oracle every Terminal-scoped command validates against.
    terminal_reverse: HashMap<WireTerminalId, TerminalId>,
    /// Next terminal wire id to hand out.
    next_terminal_wire_id: u32,
    /// Wire-side identifier for each core window id. Same shape as the pane
    /// space above; used to populate
    /// [`phux_protocol::wire::info::WindowInfo::id`] in the `ATTACHED`
    /// snapshot.
    window_forward: HashMap<WindowId, WireWindowId>,
    /// Next window wire id to hand out.
    next_window_wire_id: u32,
}

impl Default for IdSpace {
    fn default() -> Self {
        Self::new()
    }
}

impl IdSpace {
    /// Build an empty id space with both open-coded allocators at `1`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            session_id_bridge: IdBridge::new(),
            terminal_forward: HashMap::new(),
            terminal_reverse: HashMap::new(),
            next_terminal_wire_id: 1,
            window_forward: HashMap::new(),
            next_window_wire_id: 1,
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
        self.session_id_bridge.intern(core)
    }

    /// Forward lookup without allocating. `None` if `core` was never
    /// interned.
    #[must_use]
    pub fn session_wire(&self, core: SessionId) -> Option<WireSessionId> {
        self.session_id_bridge.wire(core)
    }

    /// Reverse lookup: which core session (if any) does `wire` name?
    #[must_use]
    pub fn resolve_session(&self, wire: WireSessionId) -> Option<SessionId> {
        self.session_id_bridge.resolve(wire)
    }

    /// Bind a specific `core → wire` session mapping recorded in a
    /// graceful-upgrade state blob (ADR-0032) rather than allocating a
    /// fresh id. Pair with [`Self::set_next_session_wire`].
    pub fn bind_session(&mut self, core: SessionId, wire: WireSessionId) {
        self.session_id_bridge.bind(core, wire);
    }

    /// Drop both directions of `core`'s session mapping. Idempotent; the
    /// retired wire id is not reused.
    pub fn forget_session(&mut self, core: SessionId) {
        self.session_id_bridge.forget(core);
    }

    /// The next session wire id this space would allocate.
    #[must_use]
    pub const fn next_session_wire(&self) -> u32 {
        self.session_id_bridge.next_wire()
    }

    /// Restore the session allocator after a graceful upgrade.
    pub const fn set_next_session_wire(&mut self, next: u32) {
        self.session_id_bridge.set_next(next);
    }

    // -- terminals ----------------------------------------------------

    /// Wire pane id for `terminal`, allocating one if needed.
    ///
    /// Idempotent: a second call for the same `terminal` returns the same
    /// wire id. Several runtime call sites rely on that (they re-intern
    /// rather than thread the id through), so it must stay so.
    pub(super) fn intern_terminal(&mut self, terminal: TerminalId) -> WireTerminalId {
        if let Some(w) = self.terminal_forward.get(&terminal) {
            return w.clone();
        }
        let raw = self.next_terminal_wire_id;
        self.next_terminal_wire_id = self.next_terminal_wire_id.saturating_add(1);
        let wire = WireTerminalId::local(raw);
        self.terminal_forward.insert(terminal, wire.clone());
        self.terminal_reverse.insert(wire.clone(), terminal);
        wire
    }

    /// Reverse lookup: which core pane id (if any) does `wire` resolve to?
    #[must_use]
    pub(super) fn terminal_from_wire(&self, wire: &WireTerminalId) -> Option<TerminalId> {
        self.terminal_reverse.get(wire).copied()
    }

    /// Forward lookup without allocating. `None` if `terminal` was never
    /// interned.
    #[must_use]
    pub(super) fn terminal_wire(&self, terminal: TerminalId) -> Option<&WireTerminalId> {
        self.terminal_forward.get(&terminal)
    }

    /// Bind a specific `core → wire` pane mapping from a graceful-upgrade
    /// state blob (ADR-0032). Pre-binding is what makes the subsequent
    /// [`Self::intern_terminal`] a no-op instead of minting a fresh id that
    /// would diverge from the blob.
    pub(super) fn bind_terminal(&mut self, terminal: TerminalId, wire: WireTerminalId) {
        self.terminal_forward.insert(terminal, wire.clone());
        self.terminal_reverse.insert(wire, terminal);
    }

    /// Drop both directions of `terminal`'s mapping and hand the retired
    /// wire id back.
    ///
    /// The caller needs it: the per-Terminal L3 metadata scope and the
    /// agent-record arbiter are both keyed by wire id, and they are only
    /// reachable while this mapping still exists. Returns `None` if
    /// `terminal` was never interned. The wire id is not reused.
    pub(super) fn retire_terminal(&mut self, terminal: TerminalId) -> Option<WireTerminalId> {
        let wire = self.terminal_forward.remove(&terminal)?;
        self.terminal_reverse.remove(&wire);
        Some(wire)
    }

    /// The next pane wire id this space would allocate.
    #[must_use]
    pub(super) const fn next_terminal_wire(&self) -> u32 {
        self.next_terminal_wire_id
    }

    /// Restore the pane allocator after a graceful upgrade.
    pub(super) const fn set_next_terminal_wire(&mut self, next: u32) {
        self.next_terminal_wire_id = next;
    }

    // -- windows ------------------------------------------------------

    /// Wire window id for `window`, allocating one if needed. Idempotent.
    pub(super) fn intern_window(&mut self, window: WindowId) -> WireWindowId {
        if let Some(w) = self.window_forward.get(&window) {
            return *w;
        }
        let raw = self.next_window_wire_id;
        self.next_window_wire_id = self.next_window_wire_id.saturating_add(1);
        let wire = WireWindowId(raw);
        self.window_forward.insert(window, wire);
        wire
    }

    /// Forward lookup without allocating. `None` if `window` was never
    /// interned.
    #[must_use]
    pub(super) fn window_wire(&self, window: WindowId) -> Option<WireWindowId> {
        self.window_forward.get(&window).copied()
    }

    /// Bind a specific `core → wire` window mapping from a
    /// graceful-upgrade state blob (ADR-0032). Pair with
    /// [`Self::set_next_window_wire`].
    pub(super) fn bind_window(&mut self, window: WindowId, wire: WireWindowId) {
        self.window_forward.insert(window, wire);
    }

    /// Drop `window`'s mapping. Idempotent; the retired wire id is not
    /// reused.
    pub(super) fn retire_window(&mut self, window: WindowId) {
        self.window_forward.remove(&window);
    }

    /// The next window wire id this space would allocate.
    #[must_use]
    pub(super) const fn next_window_wire(&self) -> u32 {
        self.next_window_wire_id
    }

    /// Restore the window allocator after a graceful upgrade.
    pub(super) const fn set_next_window_wire(&mut self, next: u32) {
        self.next_window_wire_id = next;
    }
}
