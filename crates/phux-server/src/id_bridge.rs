//! Bridge between `phux-core` slotmap keys and `phux-protocol` wire ids.
//!
//! Core keys carry in-process generational tags; wire ids are stable `u32`s.
//! The two crates must not depend on each other (ADR-0011), so the bridge
//! lives here, owned by [`IdSpace`](crate::state::IdSpace). [`IdBridge`] is
//! generic over both, one instance per space; minting differs only through
//! [`WireId`].
//!
//! Wire ids start at 1 (0 is a sentinel), are never reused, and
//! [`IdBridge::intern`] is idempotent. Exhaustion fails fast: minting
//! `u32::MAX` panics (poisoning the state lock) instead of saturating,
//! which would alias every later entity onto one id and misroute I/O.

use std::collections::HashMap;
use std::fmt::Debug;
use std::hash::Hash;

use phux_protocol::ids::{
    ResourceId as WireResourceId, SessionId as WireSessionId, WindowId as WireWindowId,
};

/// A wire id an [`IdBridge`] can mint from a raw `u32` (one impl per space).
pub trait WireId: Clone + Eq + Hash + Debug {
    /// Name of the id space, for the exhaustion panic.
    const SPACE: &'static str;

    /// Mint the wire id for allocator value `raw`.
    fn from_raw(raw: u32) -> Self;
}

impl WireId for WireSessionId {
    const SPACE: &'static str = "session";

    fn from_raw(raw: u32) -> Self {
        Self::new(raw)
    }
}

impl WireId for WireWindowId {
    const SPACE: &'static str = "window";

    fn from_raw(raw: u32) -> Self {
        Self::new(raw)
    }
}

/// Mints only `ResourceId::Local`: satellite terminals are routed off the
/// wire id and never enter a bridge, so [`IdBridge::resolve`] returns
/// `None` for them by design.
impl WireId for WireResourceId {
    const SPACE: &'static str = "terminal";

    fn from_raw(raw: u32) -> Self {
        Self::local(raw)
    }
}

/// Bidirectional core ↔ wire map with a monotonic allocator, reached
/// through [`IdSpace`](crate::state::IdSpace).
#[derive(Debug)]
pub struct IdBridge<C, W> {
    /// Forward: core slotmap key → wire id.
    forward: HashMap<C, W>,
    /// Wire id → core key, kept consistent with `forward`.
    reverse: HashMap<W, C>,
    /// Next wire id to hand out. Starts at `1`; `0` is reserved.
    next: u32,
}

impl<C, W> Default for IdBridge<C, W>
where
    C: Copy + Eq + Hash,
    W: WireId,
{
    /// Same as [`IdBridge::new`] (a derived `Default` would mint the 0
    /// sentinel).
    fn default() -> Self {
        Self::new()
    }
}

impl<C, W> IdBridge<C, W>
where
    C: Copy + Eq + Hash,
    W: WireId,
{
    /// Build an empty bridge with the allocator at `1`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            forward: HashMap::new(),
            reverse: HashMap::new(),
            next: 1,
        }
    }

    /// The wire id for `core`, allocating on first call (idempotent,
    /// monotonic from 1).
    ///
    /// # Panics
    ///
    /// On the call that would mint `u32::MAX`.
    #[allow(
        clippy::panic,
        reason = "u32 exhaustion is operationally unreachable; fail-fast beats aliasing wire ids"
    )]
    pub fn intern(&mut self, core: C) -> W {
        if let Some(wire) = self.forward.get(&core) {
            return wire.clone();
        }
        let raw = self.next;
        let Some(next) = self.next.checked_add(1) else {
            panic!("{} wire-id space exhausted at u32::MAX", W::SPACE)
        };
        self.next = next;
        let wire = W::from_raw(raw);
        self.forward.insert(core, wire.clone());
        self.reverse.insert(wire.clone(), core);
        wire
    }

    /// Forward lookup without allocating (borrowed: not every wire id is
    /// `Copy`).
    #[must_use]
    pub fn wire(&self, core: C) -> Option<&W> {
        self.forward.get(&core)
    }

    /// Bind a mapping from an upgrade blob (ADR-0032); pair with
    /// [`Self::set_next`].
    pub fn bind(&mut self, core: C, wire: W) {
        self.forward.insert(core, wire.clone());
        self.reverse.insert(wire, core);
    }

    /// Restore the next-wire-id allocator after a graceful upgrade.
    pub const fn set_next(&mut self, next: u32) {
        self.next = next;
    }

    /// Reverse lookup; `None` for ids never allocated or since retired.
    #[must_use]
    pub fn resolve(&self, wire: &W) -> Option<C> {
        self.reverse.get(wire).copied()
    }

    /// Drop both directions for `core`, returning the retired wire id (never
    /// reused). Idempotent.
    pub fn forget(&mut self, core: C) -> Option<W> {
        let wire = self.forward.remove(&core)?;
        self.reverse.remove(&wire);
        Some(wire)
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

    /// The next wire id to allocate (carried in the upgrade blob).
    #[must_use]
    pub const fn next_wire(&self) -> u32 {
        self.next
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_core::ids::ResourceId as CoreResourceId;
    use phux_core::ids::SessionId as CoreSessionId;
    use phux_core::registry::Registry;

    /// The session-space instantiation, which these tests exercise.
    type SessionBridge = IdBridge<CoreSessionId, WireSessionId>;

    fn fresh_core_ids(n: usize) -> (Registry, Vec<CoreSessionId>) {
        let mut reg = Registry::new();
        let ids = (0..n)
            .map(|i| reg.new_session(format!("s{i}")))
            .collect::<Vec<_>>();
        (reg, ids)
    }

    fn fresh_core_terminal() -> (Registry, CoreResourceId) {
        let mut reg = Registry::new();
        let session = reg.new_session("s".to_owned());
        let window = reg.new_window(session).expect("new window");
        let terminal = reg.new_terminal(window).expect("new terminal");
        (reg, terminal)
    }

    #[test]
    fn intern_is_idempotent() {
        let (_reg, ids) = fresh_core_ids(1);
        let mut bridge = SessionBridge::new();
        let first = bridge.intern(ids[0]);
        let again = bridge.intern(ids[0]);
        assert_eq!(first, again);
        assert_eq!(bridge.len(), 1);
    }

    #[test]
    fn intern_allocates_monotonically_from_one() {
        let (_reg, ids) = fresh_core_ids(3);
        let mut bridge = SessionBridge::new();
        let a = bridge.intern(ids[0]);
        let b = bridge.intern(ids[1]);
        let c = bridge.intern(ids[2]);
        assert_eq!(a, WireSessionId(1));
        assert_eq!(b, WireSessionId(2));
        assert_eq!(c, WireSessionId(3));
    }

    #[test]
    fn default_starts_at_one_like_new() {
        // The derived `Default` would start at `0`, the reserved sentinel.
        let bridge = SessionBridge::default();
        assert_eq!(bridge.next_wire(), SessionBridge::new().next_wire());
        assert_eq!(bridge.next_wire(), 1);
    }

    #[test]
    fn forward_map_is_deterministic() {
        let (_reg, ids) = fresh_core_ids(4);

        let mut a = SessionBridge::new();
        let a_ws: Vec<_> = ids.iter().map(|c| a.intern(*c)).collect();

        let mut b = SessionBridge::new();
        let b_ws: Vec<_> = ids.iter().map(|c| b.intern(*c)).collect();

        assert_eq!(a_ws, b_ws);
    }

    #[test]
    fn resolve_returns_none_for_unknown_wire_id() {
        let bridge = SessionBridge::new();
        assert!(bridge.resolve(&WireSessionId(1)).is_none());
        assert!(bridge.resolve(&WireSessionId(42)).is_none());
        // `0` is reserved as a sentinel and must also resolve to None.
        assert!(bridge.resolve(&WireSessionId(0)).is_none());
    }

    #[test]
    fn resolve_returns_none_after_forget() {
        let (_reg, ids) = fresh_core_ids(2);
        let mut bridge = SessionBridge::new();
        let w0 = bridge.intern(ids[0]);
        let w1 = bridge.intern(ids[1]);

        assert_eq!(bridge.forget(ids[0]), Some(w0));

        assert!(bridge.resolve(&w0).is_none());
        assert!(bridge.wire(ids[0]).is_none());
        // Untouched mapping survives.
        assert_eq!(bridge.resolve(&w1), Some(ids[1]));
    }

    #[test]
    fn forget_does_not_recycle_wire_ids() {
        let (_reg, ids) = fresh_core_ids(2);
        let mut bridge = SessionBridge::new();
        let w0 = bridge.intern(ids[0]);
        let _ = bridge.forget(ids[0]);
        let w1 = bridge.intern(ids[1]);
        assert_ne!(w0, w1, "freed wire ids must not be reused");
        assert_eq!(w1, WireSessionId(2));
    }

    #[test]
    fn round_trip_is_stable() {
        let (_reg, ids) = fresh_core_ids(5);
        let mut bridge = SessionBridge::new();
        let wires: Vec<_> = ids.iter().map(|c| bridge.intern(*c)).collect();
        for (core, wire) in ids.iter().zip(wires.iter()) {
            assert_eq!(bridge.wire(*core), Some(wire));
            assert_eq!(bridge.resolve(wire), Some(*core));
        }
    }

    #[test]
    fn forget_is_idempotent() {
        let (_reg, ids) = fresh_core_ids(1);
        let mut bridge = SessionBridge::new();
        assert_eq!(bridge.forget(ids[0]), None); // never interned
        let _ = bridge.intern(ids[0]);
        let _ = bridge.forget(ids[0]);
        assert_eq!(bridge.forget(ids[0]), None); // double-forget
        assert!(bridge.is_empty());
    }

    #[test]
    fn terminal_bridge_mints_only_local_ids() {
        let (_reg, terminal) = fresh_core_terminal();
        let mut bridge: IdBridge<CoreResourceId, WireResourceId> = IdBridge::new();
        let wire = bridge.intern(terminal);
        assert_eq!(wire, WireResourceId::local(1));
        assert!(wire.is_local(), "a bridge must never mint a Satellite id");
    }

    #[test]
    fn resolve_is_none_for_a_satellite_id() {
        let (_reg, terminal) = fresh_core_terminal();
        let mut bridge: IdBridge<CoreResourceId, WireResourceId> = IdBridge::new();
        let wire = bridge.intern(terminal);
        let raw = wire.local_id().expect("minted id is Local");
        // Satellite-tagged: never a bridge's.
        let satellite = WireResourceId::satellite("peer", raw);
        assert!(bridge.resolve(&satellite).is_none());
        assert_eq!(bridge.resolve(&wire), Some(terminal));
    }

    #[test]
    #[should_panic(expected = "session wire-id space exhausted")]
    fn intern_panics_on_the_call_that_would_mint_u32_max() {
        let (_reg, ids) = fresh_core_ids(2);
        let mut bridge = SessionBridge::new();
        bridge.set_next(u32::MAX - 1);
        let _ = bridge.intern(ids[0]);
        let _ = bridge.intern(ids[1]);
    }

    #[test]
    fn last_mintable_wire_id_is_u32_max_minus_one() {
        let (_reg, ids) = fresh_core_ids(1);
        let mut bridge = SessionBridge::new();
        bridge.set_next(u32::MAX - 1);
        assert_eq!(bridge.intern(ids[0]), WireSessionId(u32::MAX - 1));
    }
}
