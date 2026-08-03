use std::collections::HashMap;

use phux_core::registry::Registry;
use tokio::task::JoinSet;

use super::{IdSpace, MetadataStore, ServerState};

impl ServerState {
    /// Build an empty state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            server_incarnation: super::ServerIncarnation::random(),
            registry: Registry::new(),
            attached: HashMap::new(),
            viewport_clock: 0,
            terminal_subscribers: HashMap::new(),
            input_leases: HashMap::new(),
            satellite_leases: std::collections::BTreeMap::new(),
            attach_terminal_pumps: HashMap::new(),
            idspace: IdSpace::new(),
            terminals: HashMap::new(),
            terminal_tokens: HashMap::new(),
            terminal_tasks: JoinSet::new(),
            next_client_id: 1,
            session_last_touched: HashMap::new(),
            next_touch_timestamp: 1,
            metadata: MetadataStore::default(),
            session_create_results: HashMap::new(),
            client_layers: HashMap::new(),
            event_subscriptions: HashMap::new(),
            agent_asked: crate::agent_asked::AskedDetector::default(),
            agent_records: crate::agent_state::AgentRecordArbiter::default(),
            config: super::ServerConfig::default(),
            session_root: HashMap::new(),
            window_last_cwd: HashMap::new(),
            has_served_client: false,
            peer_identities: HashMap::new(),
            upgrade_ctx: None,
            hub_table: None,
            hub_link_statuses: None,
            hub_relays: None,
            hook_dispatcher: None,
            live_connections: 0,
            // "Idle since startup": a server nobody ever dialed is the
            // leak shape `--exit-after-idle` exists for, so the clock is
            // already running before the first accept.
            idle_since: Some(std::time::Instant::now()),
        }
    }
}
