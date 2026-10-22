//! Replica identity and borrowed views of published, closed, and staging replicas.

use super::{
    BootstrapId, BootstrapStreamProfile, CanonicalGeometry, EngineAdapter, EngineSearchMatch,
    HistoryCache, ResourceId, StreamId,
};

/// Registered matches from one loaded-history search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedHistorySearch {
    /// Matches in document order, each endpoint registered with the history.
    pub matches: Vec<EngineSearchMatch>,
    /// The caller's bound or the anchor budget hid at least one more match.
    pub truncated: bool,
}

/// Exact identity of one terminal replica generation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ReplicaKey {
    /// Protocol terminal identifier.
    pub terminal_id: ResourceId,
    /// Connection-scoped logical subscription.
    pub stream_id: StreamId,
    /// Replaceable replica generation.
    pub bootstrap_id: BootstrapId,
    /// Explicit profile repeated by `BOOTSTRAP_BEGIN`.
    pub profile: BootstrapStreamProfile,
}

impl ReplicaKey {
    /// Lossless 128-bit `(stream_id, bootstrap_id)` generation token, so a
    /// pooled render cache is dropped when the published replica is replaced
    /// at unchanged geometry.
    #[must_use]
    pub fn generation_token(&self) -> u128 {
        u128::from(self.stream_id.get()) << 64 | u128::from(self.bootstrap_id.get())
    }
}

/// Borrowed access to an atomically published engine replica.
pub struct PublishedReplica<'a, E: EngineAdapter> {
    pub(super) key: &'a ReplicaKey,
    pub(super) geometry: CanonicalGeometry,
    pub(super) last_seq: u64,
    pub(super) engine: &'a E::Replica,
    pub(super) history: &'a HistoryCache,
}
impl<E: EngineAdapter> std::fmt::Debug for PublishedReplica<'_, E> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PublishedReplica")
            .field("key", &self.key)
            .field("geometry", &self.geometry)
            .field("last_seq", &self.last_seq)
            .finish_non_exhaustive()
    }
}

impl<'a, E: EngineAdapter> PublishedReplica<'a, E> {
    /// Exact protocol identity and profile.
    #[must_use]
    pub const fn key(&self) -> &'a ReplicaKey {
        self.key
    }

    /// Canonical live PTY geometry.
    #[must_use]
    pub const fn geometry(&self) -> CanonicalGeometry {
        self.geometry
    }

    /// Highest contiguous live sequence applied to this replica.
    #[must_use]
    pub const fn last_seq(&self) -> u64 {
        self.last_seq
    }

    /// Borrow this client's generation-scoped progressive history.
    #[must_use]
    pub const fn history(&self) -> &'a HistoryCache {
        self.history
    }

    /// Borrow the adapter-owned live state for frontend projection.
    ///
    /// The borrow carries the kernel's lifetime (`'a`), not this view's, so
    /// a caller can return the engine replica (or state borrowed from it)
    /// while letting the `PublishedReplica` view itself go out of scope.
    #[must_use]
    pub const fn engine(&self) -> &'a E::Replica {
        self.engine
    }
}

/// An owned final replica transferred out of the kernel after terminal close.
///
/// The close transition still makes future stream input ineligible immediately,
/// but a native frontend may opt in before close and take this value to preserve
/// scrollable finished-pane history without retaining a second raw-byte copy.
pub struct ClosedReplica<E: EngineAdapter> {
    pub(super) key: ReplicaKey,
    pub(super) geometry: CanonicalGeometry,
    pub(super) last_seq: u64,
    pub(super) engine: E::Replica,
    pub(super) history: HistoryCache,
}

impl<E: EngineAdapter> std::fmt::Debug for ClosedReplica<E> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClosedReplica")
            .field("key", &self.key)
            .field("geometry", &self.geometry)
            .field("last_seq", &self.last_seq)
            .finish_non_exhaustive()
    }
}

impl<E: EngineAdapter> ClosedReplica<E> {
    /// Exact protocol identity and profile of the final published generation.
    #[must_use]
    pub const fn key(&self) -> &ReplicaKey {
        &self.key
    }

    /// Final canonical PTY geometry.
    #[must_use]
    pub const fn geometry(&self) -> CanonicalGeometry {
        self.geometry
    }

    /// Highest contiguous live sequence applied before close.
    #[must_use]
    pub const fn last_seq(&self) -> u64 {
        self.last_seq
    }

    /// Borrow the final progressive-history metadata.
    #[must_use]
    pub const fn history(&self) -> &HistoryCache {
        &self.history
    }

    /// Borrow the adapter-owned final replica for projection.
    #[must_use]
    pub const fn engine(&self) -> &E::Replica {
        &self.engine
    }

    /// Mutably borrow the final replica for frontend-local viewport controls.
    #[must_use]
    pub const fn engine_mut(&mut self) -> &mut E::Replica {
        &mut self.engine
    }
}

/// Borrowed staging state, exposed for diagnostics without publication.
pub struct StagingReplica<'a, E: EngineAdapter> {
    pub(super) key: &'a ReplicaKey,
    pub(super) geometry: CanonicalGeometry,
    pub(super) engine_ready: bool,
    pub(super) protocol_ready: bool,
    pub(super) engine: &'a E::Replica,
}
impl<E: EngineAdapter> std::fmt::Debug for StagingReplica<'_, E> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StagingReplica")
            .field("key", &self.key)
            .field("geometry", &self.geometry)
            .field("engine_ready", &self.engine_ready)
            .field("protocol_ready", &self.protocol_ready)
            .finish_non_exhaustive()
    }
}

impl<E: EngineAdapter> StagingReplica<'_, E> {
    /// Exact protocol identity and profile.
    #[must_use]
    pub const fn key(&self) -> &ReplicaKey {
        self.key
    }

    /// Canonical live PTY geometry.
    #[must_use]
    pub const fn geometry(&self) -> CanonicalGeometry {
        self.geometry
    }

    /// Whether the engine reported its internal READY boundary.
    #[must_use]
    pub const fn engine_ready(&self) -> bool {
        self.engine_ready
    }

    /// Whether protocol `BOOTSTRAP_READY` arrived.
    #[must_use]
    pub const fn protocol_ready(&self) -> bool {
        self.protocol_ready
    }

    /// Borrow the adapter-owned staging state.
    #[must_use]
    pub const fn engine(&self) -> &E::Replica {
        self.engine
    }
}
