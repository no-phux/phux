//! Building a relay session and configuring it from the negotiated link.

#[cfg(test)]
use super::ServerFeature;
use super::{
    Arc, BootstrapLimits, BootstrapProfile, BytesMut, HashMap, HashSet, RelaySession,
    ResourceKindCatalog, SatelliteHost, ServerFeatureExtSet, ServerFeatureSet, first_generation,
};

impl RelaySession {
    /// Fresh session state for one established connection to `host`.
    #[cfg(test)]
    pub(crate) fn new(host: SatelliteHost, bootstrap_limits: BootstrapLimits) -> Self {
        let mut session = Self::new_negotiated(
            host,
            bootstrap_limits,
            BootstrapProfile::SynthesizedVtRaw,
            ServerFeatureSet::with(&[ServerFeature::ListDirectory]),
        );
        session.enforce_bootstrap_flow = false;
        session
    }

    /// Fresh session state pinned to the connection's exact negotiated profile.
    pub(crate) fn new_negotiated(
        host: SatelliteHost,
        bootstrap_limits: BootstrapLimits,
        bootstrap_profile: BootstrapProfile,
        satellite_features: ServerFeatureSet,
    ) -> Self {
        Self {
            host,
            epoch: {
                static NEXT_EPOCH: super::AtomicU64 = super::AtomicU64::new(1);
                NEXT_EPOCH.fetch_add(1, super::Ordering::Relaxed)
            },
            bootstrap_limits,
            bootstrap_profile,
            next_request_id: 1,
            pending: HashMap::new(),
            pending_spawns: HashMap::new(),
            pending_listings: HashMap::new(),
            pending_paths: HashMap::new(),
            satellite_features,
            satellite_features_ext: ServerFeatureExtSet::new(),
            satellite_features_ext2: phux_protocol::caps::ServerFeatureExt2Set::new(),
            pending_detaches: HashMap::new(),
            mirrored: HashSet::new(),
            pending_mirror_gets: HashMap::new(),
            enforce_bootstrap_flow: true,
            subscribers: HashMap::new(),
            explicit_content: HashSet::new(),
            legacy_events: HashSet::new(),
            bootstrap_flows: HashMap::new(),
            generations: first_generation(),
            retained_bytes: 0,
            retained_frames: 0,
            orphaned_by_delivery: HashSet::new(),
            inflight_generation_bytes: 0,
            inflight_generation_frames: 0,
            encode_buf: BytesMut::with_capacity(1024),
            journal: None,
            incarnation: None,
            operations: crate::hub::operation_fence::OperationFence::default(),
            resource_kinds: None,
        }
    }

    /// Re-stamp relayed events from `journal`, the hub's own state.
    pub(crate) fn set_journal(&mut self, journal: Option<crate::state::SharedState>) {
        if let Some(journal) = &journal {
            journal.with_mut(|s| {
                s.interrupt_satellite_host(&self.host);
                s.federation_mut().begin_satellite_link(
                    self.host.clone(),
                    self.epoch,
                    self.satellite_features,
                );
            });
        }
        self.journal = journal;
    }

    /// Record the incarnation this connection's satellite announced.
    pub(crate) const fn set_incarnation(
        &mut self,
        incarnation: crate::hub::operation_fence::Incarnation,
    ) {
        self.incarnation = incarnation;
    }

    /// The incarnation this connection's satellite announced.
    pub(crate) const fn incarnation(&self) -> crate::hub::operation_fence::Incarnation {
        self.incarnation
    }

    /// Share the link's incarnation fence, which outlives this connection.
    pub(crate) fn set_operation_fence(
        &mut self,
        operations: crate::hub::operation_fence::OperationFence,
    ) {
        self.operations = operations;
    }

    pub(crate) const fn set_satellite_features_ext(&mut self, features: ServerFeatureExtSet) {
        self.satellite_features_ext = features;
    }

    pub(crate) const fn set_satellite_features_ext2(
        &mut self,
        features: phux_protocol::caps::ServerFeatureExt2Set,
    ) {
        self.satellite_features_ext2 = features;
    }

    pub(crate) fn set_resource_kinds(&mut self, resource_kinds: ResourceKindCatalog) {
        {
            let mut catalog = resource_kinds
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            catalog.kinds.clear();
            catalog.controls.clear();
            catalog.terminal_profile = Some(self.bootstrap_profile);
            catalog.epoch = self.epoch;
            // Outward ids outlive every epoch: a consumer never sees one twice.
            self.generations = Arc::clone(&catalog.generations);
        }
        self.resource_kinds = Some(resource_kinds);
    }
}
