use super::{
    AgentState, ClientTable, HubState, IdSpace, LeaseTable, Lifecycle, MetadataStore, ServerState,
    SessionTable, TerminalTable,
};

impl ServerState {
    /// Build an empty state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            sessions: SessionTable::new(),
            clients: ClientTable::new(),
            terminal_table: TerminalTable::new(),
            leases: LeaseTable::new(),
            idspace: IdSpace::new(),
            metadata: MetadataStore::default(),
            agent: AgentState::new(),
            config: super::ServerConfig::default(),
            hub: HubState::new(),
            hook_dispatcher: None,
            // Mints this process's incarnation and starts the idle clock —
            // see `Lifecycle::new`.
            lifecycle: Lifecycle::new(),
        }
    }
}
