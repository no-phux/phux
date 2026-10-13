use super::{
    AgentState, ClientTable, HubState, IdSpace, LeaseTable, Lifecycle, MetadataStore,
    ResourceTable, ServerState, SessionTable,
};

impl ServerState {
    /// Build an empty state.
    #[must_use]
    pub fn new() -> Self {
        let idspace = IdSpace::new();
        let org = super::org::fresh_org(
            phux_core::InstanceId::from_bytes(*idspace.instance().as_bytes()),
            super::org::now_stamp(),
        );
        Self {
            sessions: SessionTable::new(phux_core::Registry::new(org)),
            org: super::org::OrgSide::memory(),
            clients: ClientTable::new(),
            journal: super::journal::Journal::new(
                usize::try_from(phux_config::DEFAULT_EVENT_JOURNAL_ENTRIES).unwrap_or(usize::MAX),
                usize::try_from(phux_config::DEFAULT_EVENT_JOURNAL_BYTES).unwrap_or(usize::MAX),
            ),
            resources: ResourceTable::new(),
            leases: LeaseTable::new(),
            delegations: crate::delegation::DelegationTable::default(),
            satellite_spawns: super::satellite_spawns::SatelliteSpawnLedger::default(),
            idspace,
            metadata: MetadataStore::default(),
            agent: AgentState::new(),
            config: super::ServerConfig::default(),
            hub: HubState::new(),
            hook_dispatcher: None,
            push_grants: crate::push::PushGrants::default(),
            // Mints this process's incarnation and starts the idle clock —
            // see `Lifecycle::new`.
            lifecycle: Lifecycle::new(),
            close_reasons: std::collections::HashMap::new(),
            close_attributions: std::collections::HashMap::new(),
            exit_watches: std::collections::HashMap::new(),
            retained: super::retained::RetainedTable::default(),
            remote_listeners: phux_protocol::wire::RemoteListenersReport::new(),
            operation_dedupe: crate::runtime::operation_dedupe::OperationDedupe::default(),
            approvals: super::approvals::ApprovalTable::default(),
        }
    }
}
