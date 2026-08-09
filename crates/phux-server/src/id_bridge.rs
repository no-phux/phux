//! Bridge between `phux-core` slotmap keys (in-process registry detail) and
//! the `phux-protocol` identifiers that go on the wire.
//!
//! # Why two id types?
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
//! The two ID namespaces are therefore deliberately distinct and live in two
//! crates that **must not depend on each other** (ADR-0011: `phux-core` is
//! pure domain; `phux-protocol` is pure wire). This bridge lives only in
//! `phux-server`, the one place that holds both, and is owned by
//! [`IdSpace`](crate::state::IdSpace).
//!
//! # One bridge, three spaces
//!
//! [`IdBridge`] is generic over the core key `C` and the wire id `W`, so the
//! session, terminal, and window spaces are all the same type at three
//! instantiations rather than one shared type and two open-coded copies.
//! Minting is the only per-space difference and it lives behind the
//! [`WireId`] trait, which is the single place that decides how a raw `u32`
//! becomes a wire id. That matters most for
//! [`phux_protocol::ids::TerminalId`], which is an enum: see its [`WireId`]
//! impl for the Local-only minting invariant.
//!
//! # Allocation model
//!
//! Wire IDs start at `1` and increase monotonically; `0` is reserved as a
//! sentinel that any future `Option<Id>` encoding can use without collision.
//! IDs are never reused for the server's lifetime — once an entity is
//! destroyed its wire id is retired (the reverse-lookup returns `None`, the
//! forward map is dropped).
//!
//! [`IdBridge::intern`] is idempotent: calling it twice for the same core id
//! returns the same wire id. Callers should treat it as the canonical
//! "get-or-allocate" primitive when constructing an outbound frame.
//!
//! # Exhaustion
//!
//! All three spaces **fail fast**: the call that would mint `u32::MAX`
//! panics instead, so a bridge hands out `1..=u32::MAX - 1` and `u32::MAX` is
//! never a live wire id.
//!
//! The terminal and window spaces used to saturate instead (`saturating_add`
//! on the allocator). That is strictly worse than panicking: saturation mints
//! `u32::MAX` and then keeps minting it, so the reverse map's `insert`
//! overwrites and every terminal after the first aliases onto one wire id.
//! The forward map still points the older terminals at `u32::MAX` while the
//! reverse resolves it to only the newest, which means input and output route
//! to the wrong terminal — silent data corruption in the one table that is
//! the existence oracle for every Terminal-scoped command. A panic is loud,
//! reproducible, and lands roughly four billion allocations away from any
//! real workload.
//!
//! The panic fires while the `ServerState` mutex is held, so it poisons the
//! lock and every subsequent [`SharedState::lock`](crate::state::SharedState)
//! aborts too: the whole server dies, not just the offending client task.
//! That is deliberate and matches the module-level stance on poisoning in
//! [`crate::state`] — an id space that cannot allocate has no consistent
//! state left to serve from.

use std::collections::HashMap;
use std::fmt::Debug;
use std::hash::Hash;

use phux_protocol::ids::{
    SessionId as WireSessionId, TerminalId as WireTerminalId, WindowId as WireWindowId,
};

/// A `phux-protocol` identifier an [`IdBridge`] can mint from a raw `u32`.
///
/// Implemented once per wire id space. The impl owns the *shape* of a minted
/// id, which is the only thing that differs between the three spaces; the
/// allocator, the two maps, and the exhaustion semantic are shared.
pub trait WireId: Clone + Eq + Hash + Debug {
    /// Human-readable name of this id space, used in the exhaustion panic
    /// message (`"session"`, `"terminal"`, `"window"`).
    const SPACE: &'static str;

    /// Mint the wire id for allocator value `raw`.
    ///
    /// Called exactly once per freshly interned core id.
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

/// Load-bearing: `TerminalId` is an enum, not a `u32` newtype, and this impl
/// is the single place that decides which variant a bridge mints.
///
/// It mints [`TerminalId::Local`](phux_protocol::ids::TerminalId::Local) and
/// only that. A satellite terminal
/// ([`TerminalId::Satellite`](phux_protocol::ids::TerminalId::Satellite)) is
/// owned by a federation peer and addressed straight off the wire id by
/// federation routing (ADR-0007); it never enters a bridge's tables. So
/// [`IdBridge::resolve`] returns `None` for a satellite id **by design**, not
/// by omission, and that stays true for as long as this impl calls
/// `TerminalId::local`.
impl WireId for WireTerminalId {
    const SPACE: &'static str = "terminal";

    fn from_raw(raw: u32) -> Self {
        Self::local(raw)
    }
}

/// Bidirectional `core id <-> wire id` map plus a monotonic allocator for
/// fresh wire ids.
///
/// Held inside [`IdSpace`](crate::state::IdSpace), which is itself a field of
/// [`ServerState`](crate::state::ServerState) — reach it through `IdSpace`'s
/// per-space methods rather than the fields. `IdSpace` owns one bridge per id
/// space (sessions, terminals, windows). Not thread-safe on its own; the
/// surrounding `Mutex<ServerState>` provides synchronization.
#[derive(Debug)]
pub struct IdBridge<C, W> {
    /// Forward: core slotmap key → wire id.
    forward: HashMap<C, W>,
    /// Reverse: wire id → core slotmap key. Kept consistent with `forward`
    /// by every mutator.
    reverse: HashMap<W, C>,
    /// Next wire id to hand out. Starts at `1`; `0` is reserved.
    next: u32,
}

impl<C, W> Default for IdBridge<C, W>
where
    C: Copy + Eq + Hash,
    W: WireId,
{
    /// Same as [`IdBridge::new`].
    ///
    /// Hand-written rather than derived: the derived `Default` would set
    /// `next: 0` and mint the reserved sentinel as the first wire id.
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

    /// Return the wire id for `core`, allocating a fresh one on first call.
    ///
    /// Subsequent calls with the same `core` return the same wire id (the
    /// map is idempotent). Allocation is monotonic from `1`; the first id
    /// handed out by a fresh bridge wraps the raw value `1`.
    ///
    /// # Panics
    ///
    /// Panics on the call that would mint `u32::MAX`, i.e. once more than
    /// `u32::MAX - 1` distinct core ids have been interned over the bridge's
    /// lifetime. See the module doc's "Exhaustion" section for why failing
    /// loudly beats saturating.
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

    /// Forward lookup without allocating. Returns `None` if `core` has
    /// never been interned.
    ///
    /// Borrows rather than returning an owned id: not every wire id is
    /// `Copy` (`TerminalId::Satellite` carries a host string). Callers whose
    /// space is `Copy` can `.copied()`.
    #[must_use]
    pub fn wire(&self, core: C) -> Option<&W> {
        self.forward.get(&core)
    }

    /// Bind a specific `core → wire` mapping recorded in a graceful-upgrade
    /// state blob (ADR-0032), rather than allocating a fresh wire id. Pair
    /// with [`Self::set_next`] so the restored allocator keeps minting ids
    /// above every restored one.
    pub fn bind(&mut self, core: C, wire: W) {
        self.forward.insert(core, wire.clone());
        self.reverse.insert(wire, core);
    }

    /// Restore the next-wire-id allocator after a graceful upgrade.
    pub const fn set_next(&mut self, next: u32) {
        self.next = next;
    }

    /// Reverse lookup: which core slotmap key (if any) does `wire`
    /// resolve to? Returns `None` for unknown wire ids — i.e. the client
    /// sent an id the server never allocated, or one that referred to a
    /// since-destroyed entity.
    #[must_use]
    pub fn resolve(&self, wire: &W) -> Option<C> {
        self.reverse.get(wire).copied()
    }

    /// Drop both directions of the mapping for `core` and hand the retired
    /// wire id back. Idempotent — returns `None` if `core` was never
    /// interned.
    ///
    /// Callers that need the retired id have it: the per-terminal L3
    /// metadata scope and the agent-record arbiter are keyed by wire id and
    /// are only reachable while this mapping still exists.
    ///
    /// Wire ids retired this way are **not** reused; `next` continues
    /// monotonically. This preserves the "wire ids are stable for the
    /// server's lifetime" contract documented above.
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
    use phux_core::ids::SessionId as CoreSessionId;
    use phux_core::ids::TerminalId as CoreTerminalId;
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

    fn fresh_core_terminal() -> (Registry, CoreTerminalId) {
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
        // Same input order → same wire ids. (Not relying on HashMap order
        // because we exercise the allocator, not iteration.)
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
        let mut bridge: IdBridge<CoreTerminalId, WireTerminalId> = IdBridge::new();
        let wire = bridge.intern(terminal);
        assert_eq!(wire, WireTerminalId::local(1));
        assert!(wire.is_local(), "a bridge must never mint a Satellite id");
    }

    #[test]
    fn resolve_is_none_for_a_satellite_id() {
        let (_reg, terminal) = fresh_core_terminal();
        let mut bridge: IdBridge<CoreTerminalId, WireTerminalId> = IdBridge::new();
        let wire = bridge.intern(terminal);
        let raw = wire.local_id().expect("minted id is Local");
        // Same raw id, satellite-tagged: federation routing owns it, the
        // bridge never does.
        let satellite = WireTerminalId::satellite("peer", raw);
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
