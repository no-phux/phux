use super::{
    AgentState, ClientTable, HubState, IdSpace, LeaseTable, Lifecycle, MetadataStore,
    ResourceTable, ServerState, SessionTable,
};

impl ServerState {
    /// Build an empty state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            sessions: SessionTable::new(),
            clients: ClientTable::new(),
            resources: ResourceTable::new(),
            leases: LeaseTable::new(),
            satellite_spawns: super::satellite_spawns::SatelliteSpawnLedger::default(),
            idspace: IdSpace::new(),
            metadata: MetadataStore::default(),
            agent: AgentState::new(),
            config: super::ServerConfig::default(),
            hub: HubState::new(),
            hook_dispatcher: None,
            // Mints this process's incarnation and starts the idle clock —
            // see `Lifecycle::new`.
            lifecycle: Lifecycle::new(),
            close_reasons: std::collections::HashMap::new(),
        }
    }
}
