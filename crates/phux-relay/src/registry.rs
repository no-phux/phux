//! Live connector-tunnel registry, keyed by route name.
//!
//! Claim policy is **last-writer-wins** (ADR-0052 rotation semantics,
//! ADR-0051 Decision 2's one-tunnel-per-route shape): a redialing
//! connector must never be locked out by a half-dead incumbent the idle
//! timeout has not yet reaped, so a new valid claim closes the old tunnel
//! with [`crate::RECLAIMED_CODE`] and takes the route. The warn log on
//! replacement is the operator's theft-detection surface (a stolen token
//! can evict a live connector; accepted for the single-tenant reference
//! relay).
//!
//! Each claim gets a monotonic epoch so a replaced tunnel's cleanup can
//! never evict its successor.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// What the registry needs from a tunnel connection handle. Abstracted so
/// the replace/epoch semantics are unit-testable without QUIC endpoints;
/// production uses [`quinn::Connection`].
pub(crate) trait TunnelHandle: Clone {
    /// Close the tunnel because a newer claim on the same route
    /// superseded it.
    fn close_reclaimed(&self);
}

impl TunnelHandle for quinn::Connection {
    fn close_reclaimed(&self) {
        self.close(
            crate::RECLAIMED_CODE.into(),
            b"superseded by a newer tunnel claim",
        );
    }
}

/// One registered tunnel: the connection and the epoch of its claim.
struct Entry<C> {
    conn: C,
    epoch: u64,
}

/// Registry state behind one mutex: the route map plus the epoch counter.
struct Inner<C> {
    tunnels: HashMap<String, Entry<C>>,
    next_epoch: u64,
}

/// Shared registry of live tunnels. Cheap to clone (an `Arc`); the mutex
/// is never held across an await.
pub(crate) struct TunnelRegistry<C> {
    inner: Arc<Mutex<Inner<C>>>,
}

impl<C> Clone for TunnelRegistry<C> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<C> std::fmt::Debug for TunnelRegistry<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TunnelRegistry").finish_non_exhaustive()
    }
}

impl<C: TunnelHandle> TunnelRegistry<C> {
    /// An empty registry.
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                tunnels: HashMap::new(),
                next_epoch: 1,
            })),
        }
    }

    /// Register `conn` as the live tunnel for `route`, returning the
    /// claim's epoch. Last-writer-wins: an existing entry is replaced and
    /// its connection closed with [`crate::RECLAIMED_CODE`].
    pub(crate) fn claim(&self, route: &str, conn: C) -> u64 {
        let mut inner = self.lock();
        let epoch = inner.next_epoch;
        inner.next_epoch += 1;
        let replaced = inner
            .tunnels
            .insert(route.to_owned(), Entry { conn, epoch });
        // Close the evicted tunnel outside the lock.
        drop(inner);
        if let Some(old) = replaced {
            tracing::warn!(
                route,
                "tunnel replaced: a newer claim superseded the live tunnel"
            );
            old.conn.close_reclaimed();
        }
        epoch
    }

    /// The live tunnel for `route`, if any, cloned for the caller's use.
    pub(crate) fn get(&self, route: &str) -> Option<C> {
        self.lock().tunnels.get(route).map(|e| e.conn.clone())
    }

    /// Remove `route`'s entry, but only if it still belongs to the claim
    /// identified by `epoch` — a replaced tunnel's cleanup must not evict
    /// its successor. Returns whether an entry was removed.
    pub(crate) fn remove_if_current(&self, route: &str, epoch: u64) -> bool {
        let mut inner = self.lock();
        if inner.tunnels.get(route).is_some_and(|e| e.epoch == epoch) {
            inner.tunnels.remove(route);
            true
        } else {
            false
        }
    }

    /// Lock the registry, recovering from a poisoned mutex (no invariant
    /// here can be broken mid-update in a way a panic would corrupt).
    fn lock(&self) -> MutexGuard<'_, Inner<C>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A mock tunnel handle counting how many times it was reclaimed.
    #[derive(Clone)]
    struct MockConn {
        id: u32,
        reclaims: Arc<AtomicUsize>,
    }

    impl MockConn {
        fn new(id: u32) -> Self {
            Self {
                id,
                reclaims: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn reclaimed(&self) -> usize {
            self.reclaims.load(Ordering::SeqCst)
        }
    }

    impl TunnelHandle for MockConn {
        fn close_reclaimed(&self) {
            self.reclaims.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn claim_registers_and_get_returns_the_tunnel() {
        let registry = TunnelRegistry::new();
        let conn = MockConn::new(1);
        let epoch = registry.claim("alpha", conn.clone());
        assert!(epoch > 0);
        assert_eq!(registry.get("alpha").map(|c| c.id), Some(1));
        assert!(registry.get("beta").is_none());
        assert_eq!(conn.reclaimed(), 0);
    }

    #[test]
    fn reclaim_replaces_and_closes_the_incumbent() {
        let registry = TunnelRegistry::new();
        let old = MockConn::new(1);
        let new = MockConn::new(2);
        let old_epoch = registry.claim("alpha", old.clone());
        let new_epoch = registry.claim("alpha", new.clone());

        assert_ne!(old_epoch, new_epoch, "each claim gets its own epoch");
        assert_eq!(old.reclaimed(), 1, "incumbent closed with RECLAIMED");
        assert_eq!(new.reclaimed(), 0);
        assert_eq!(
            registry.get("alpha").map(|c| c.id),
            Some(2),
            "the route serves the newest claim (last-writer-wins)"
        );
    }

    #[test]
    fn stale_epoch_cleanup_cannot_evict_the_successor() {
        let registry = TunnelRegistry::new();
        let old_epoch = registry.claim("alpha", MockConn::new(1));
        let new_epoch = registry.claim("alpha", MockConn::new(2));

        // The replaced tunnel's task runs its cleanup late: no-op.
        assert!(!registry.remove_if_current("alpha", old_epoch));
        assert_eq!(registry.get("alpha").map(|c| c.id), Some(2));

        // The live claim's own cleanup removes the route.
        assert!(registry.remove_if_current("alpha", new_epoch));
        assert!(registry.get("alpha").is_none());
    }

    #[test]
    fn routes_are_independent() {
        let registry = TunnelRegistry::new();
        let alpha = MockConn::new(1);
        registry.claim("alpha", alpha.clone());
        let beta_epoch = registry.claim("beta", MockConn::new(2));

        assert_eq!(alpha.reclaimed(), 0, "a claim on beta never touches alpha");
        assert!(registry.remove_if_current("beta", beta_epoch));
        assert_eq!(registry.get("alpha").map(|c| c.id), Some(1));
    }
}
