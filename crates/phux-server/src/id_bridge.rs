//! Bridge between `phux_core::ids::SessionId` (slotmap key, in-process
//! registry detail) and `phux_protocol::ids::SessionId` (`u32` newtype on
//! the wire).
//!
//! # Why two `SessionId` types?
//!
//! `phux-core` keys its [`Registry`](phux_core::registry::Registry) with
//! `slotmap::SlotMap`, whose keys carry a generational tag so reuse of a
//! freed slot does not silently alias an old reference. That tag is an
//! in-process invariant and is intentionally not exposed across the wire.
//!
//! `phux-protocol` ships frames over the network and needs a stable,
//! addressable, `u32`-wide identifier — the server allocates these
//! monotonically and they survive the lifetime of the client connection.
//!
//! The two ID types are therefore deliberately distinct and live in two
//! crates that **must not depend on each other** (ADR boundary: `phux-core`
//! is pure domain; `phux-protocol` is pure wire). This bridge lives only in
//! `phux-server`, the one place that holds both, and is owned by
//! [`IdSpace`](crate::state::IdSpace) alongside the terminal and window id
//! spaces.
//!
//! # Allocation model
//!
//! Wire IDs start at `1` and increase monotonically; `0` is reserved as a
//! sentinel that any future `Option<SessionId>` encoding can use without
//! collision. IDs are never reused for the server's lifetime — once a
//! session is destroyed its wire id is retired (the reverse-lookup returns
//! `None`, the forward map is dropped).
//!
//! `intern()` is idempotent: calling it twice for the same core id returns
//! the same wire id. Callers should treat it as the canonical "get-or-allocate"
//! primitive when constructing an outbound frame that mentions a session.

use std::collections::HashMap;

use phux_core::ids::SessionId as CoreSessionId;
use phux_protocol::ids::SessionId as WireSessionId;

/// Bidirectional `CoreSessionId <-> WireSessionId` map plus a monotonic
/// allocator for fresh wire ids.
///
/// Held inside [`IdSpace`](crate::state::IdSpace), which is itself a field
/// of [`ServerState`](crate::state::ServerState) — reach it through
/// `IdSpace`'s `*_session` methods rather than the field. `IdSpace` also
/// carries the terminal and window id spaces, which are the same shape
/// open-coded; see its module doc for why they are not this type. Not
/// thread-safe on its own; the surrounding `Mutex<ServerState>` provides
/// synchronization.
#[derive(Debug, Default)]
pub struct IdBridge {
    /// Forward: core slotmap key → wire id.
    forward: HashMap<CoreSessionId, WireSessionId>,
    /// Reverse: wire id → core slotmap key. Kept consistent with `forward`
    /// by every mutator.
    reverse: HashMap<WireSessionId, CoreSessionId>,
    /// Next wire id to hand out. Starts at `1`; `0` is reserved.
    next: u32,
}

impl IdBridge {
    /// Build an empty bridge.
    #[must_use]
    pub fn new() -> Self {
        Self {
            forward: HashMap::new(),
            reverse: HashMap::new(),
            next: 1,
        }
    }

    /// Return the wire id for `core`, allocating a fresh one on first call.
    ///
    /// Subsequent calls with the same `core` return the same wire id (the
    /// map is idempotent). Allocation is monotonic from `1`; the first id
    /// handed out by a fresh bridge is `WireSessionId(1)`.
    ///
    /// # Panics
    ///
    /// Panics if more than `u32::MAX - 1` distinct sessions are interned
    /// over the bridge's lifetime — a deliberate fast-fail since at that
    /// point the server has been creating sessions at ~1/ms for 50 days
    /// and reusing freed wire ids would change the contract advertised in
    /// this module's doc.
    #[allow(
        clippy::expect_used,
        reason = "u32 exhaustion is operationally unreachable; fail-fast is the right behavior"
    )]
    pub fn intern(&mut self, core: CoreSessionId) -> WireSessionId {
        if let Some(wire) = self.forward.get(&core) {
            return *wire;
        }
        let raw = self.next;
        let next = self
            .next
            .checked_add(1)
            .expect("IdBridge exhausted u32 wire-id space");
        self.next = next;
        let wire = WireSessionId(raw);
        self.forward.insert(core, wire);
        self.reverse.insert(wire, core);
        wire
    }

    /// Forward lookup without allocating. Returns `None` if `core` has
    /// never been interned.
    #[must_use]
    pub fn wire(&self, core: CoreSessionId) -> Option<WireSessionId> {
        self.forward.get(&core).copied()
    }

    /// Bind a specific `core → wire` mapping recorded in a graceful-upgrade
    /// state blob (ADR-0032), rather than allocating a fresh wire id. Pair
    /// with [`Self::set_next`] so the restored allocator keeps minting ids
    /// above every restored one.
    pub fn bind(&mut self, core: CoreSessionId, wire: WireSessionId) {
        self.forward.insert(core, wire);
        self.reverse.insert(wire, core);
    }

    /// Restore the next-wire-id allocator after a graceful upgrade.
    pub const fn set_next(&mut self, next: u32) {
        self.next = next;
    }

    /// Reverse lookup: which core slotmap key (if any) does `wire`
    /// resolve to? Returns `None` for unknown wire ids — i.e. the client
    /// sent an id the server never allocated, or one that referred to a
    /// since-destroyed session.
    #[must_use]
    pub fn resolve(&self, wire: WireSessionId) -> Option<CoreSessionId> {
        self.reverse.get(&wire).copied()
    }

    /// Drop both directions of the mapping for `core`. Idempotent —
    /// no-op if `core` was never interned.
    ///
    /// Wire ids retired this way are **not** reused; `next` continues
    /// monotonically. This preserves the "wire ids are stable for the
    /// server's lifetime" contract documented above.
    pub fn forget(&mut self, core: CoreSessionId) {
        if let Some(wire) = self.forward.remove(&core) {
            self.reverse.remove(&wire);
        }
    }

    /// Number of currently interned mappings.
    #[must_use]
    pub fn len(&self) -> usize {
        self.forward.len()
    }

    /// True if no mappings are present.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.forward.is_empty()
    }

    /// The next wire id this bridge would allocate. Carried into the
    /// graceful-upgrade state blob so the resumed server keeps minting ids
    /// above every restored one (ADR-0032).
    #[must_use]
    pub const fn next_wire(&self) -> u32 {
        self.next
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_core::registry::Registry;

    fn fresh_core_ids(n: usize) -> (Registry, Vec<CoreSessionId>) {
        let mut reg = Registry::new();
        let ids = (0..n)
            .map(|i| reg.new_session(format!("s{i}")))
            .collect::<Vec<_>>();
        (reg, ids)
    }

    #[test]
    fn intern_is_idempotent() {
        let (_reg, ids) = fresh_core_ids(1);
        let mut bridge = IdBridge::new();
        let first = bridge.intern(ids[0]);
        let again = bridge.intern(ids[0]);
        assert_eq!(first, again);
        assert_eq!(bridge.len(), 1);
    }

    #[test]
    fn intern_allocates_monotonically_from_one() {
        let (_reg, ids) = fresh_core_ids(3);
        let mut bridge = IdBridge::new();
        let a = bridge.intern(ids[0]);
        let b = bridge.intern(ids[1]);
        let c = bridge.intern(ids[2]);
        assert_eq!(a, WireSessionId(1));
        assert_eq!(b, WireSessionId(2));
        assert_eq!(c, WireSessionId(3));
    }

    #[test]
    fn forward_map_is_deterministic() {
        // Same input order → same wire ids. (Not relying on HashMap order
        // because we exercise the allocator, not iteration.)
        let (_reg, ids) = fresh_core_ids(4);

        let mut a = IdBridge::new();
        let a_ws: Vec<_> = ids.iter().map(|c| a.intern(*c)).collect();

        let mut b = IdBridge::new();
        let b_ws: Vec<_> = ids.iter().map(|c| b.intern(*c)).collect();

        assert_eq!(a_ws, b_ws);
    }

    #[test]
    fn resolve_returns_none_for_unknown_wire_id() {
        let bridge = IdBridge::new();
        assert!(bridge.resolve(WireSessionId(1)).is_none());
        assert!(bridge.resolve(WireSessionId(42)).is_none());
        // `0` is reserved as a sentinel and must also resolve to None.
        assert!(bridge.resolve(WireSessionId(0)).is_none());
    }

    #[test]
    fn resolve_returns_none_after_forget() {
        let (_reg, ids) = fresh_core_ids(2);
        let mut bridge = IdBridge::new();
        let w0 = bridge.intern(ids[0]);
        let w1 = bridge.intern(ids[1]);

        bridge.forget(ids[0]);

        assert!(bridge.resolve(w0).is_none());
        assert!(bridge.wire(ids[0]).is_none());
        // Untouched mapping survives.
        assert_eq!(bridge.resolve(w1), Some(ids[1]));
    }

    #[test]
    fn forget_does_not_recycle_wire_ids() {
        let (_reg, ids) = fresh_core_ids(2);
        let mut bridge = IdBridge::new();
        let w0 = bridge.intern(ids[0]);
        bridge.forget(ids[0]);
        let w1 = bridge.intern(ids[1]);
        assert_ne!(w0, w1, "freed wire ids must not be reused");
        assert_eq!(w1, WireSessionId(2));
    }

    #[test]
    fn round_trip_is_stable() {
        let (_reg, ids) = fresh_core_ids(5);
        let mut bridge = IdBridge::new();
        let wires: Vec<_> = ids.iter().map(|c| bridge.intern(*c)).collect();
        for (core, wire) in ids.iter().zip(wires.iter()) {
            assert_eq!(bridge.wire(*core), Some(*wire));
            assert_eq!(bridge.resolve(*wire), Some(*core));
        }
    }

    #[test]
    fn forget_is_idempotent() {
        let (_reg, ids) = fresh_core_ids(1);
        let mut bridge = IdBridge::new();
        bridge.forget(ids[0]); // never interned — must not panic
        let _ = bridge.intern(ids[0]);
        bridge.forget(ids[0]);
        bridge.forget(ids[0]); // double-forget
        assert!(bridge.is_empty());
    }
}
