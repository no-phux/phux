use super::{
    ClientTable, Federation, IdSpace, LeaseTable, Lifecycle, MetadataStore, ResourceTable,
    ServerState, SessionTable, StateConfig,
};

impl ServerState {
    /// Build an empty state with the default configuration.
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(StateConfig::default())
    }

    /// Build an empty state with `config`.
    #[must_use]
    pub(crate) fn with_config(config: StateConfig) -> Self {
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
                config.journal_bounds.entries,
                config.journal_bounds.bytes,
            ),
            resources: ResourceTable::default(),
            leases: LeaseTable::default(),
            delegations: crate::delegation::DelegationTable::default(),
            idspace,
            metadata: MetadataStore::default(),
            federation: Federation::default(),
            uplinks: crate::uplink::Uplinks::default(),
            forward_agent: crate::runtime::forward::agent::Broker::default(),
            hook_dispatcher: None,
            plugin_startup: None,
            standing_mirrors: None,
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
            approvals: super::approvals::ApprovalTable::new(config.approval_limits),
            config,
            credentials: super::credentials::CredentialBroker::default(),
            app_slots: super::app_slots::AppSlots::default(),
        }
    }
}
