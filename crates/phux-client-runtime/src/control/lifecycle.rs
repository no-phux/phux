//! Construction, observation, and the connection lifecycle.

#[cfg(feature = "engine")]
use super::{Arc, EngineError, Publication, Scroll};
use super::{
    BootstrapLimits, ControlOptions, ControlPlane, EngineHandle, Event, HashMap, HashSet,
    HistoryCacheConfig, InputReplayJournal, Instant, ResourceId, RolePolicy, ServerInfo,
    SessionRecovery, Status, Topology, ViewportInfo, agent_delivery, approvals, extensions,
    geometry, global_metadata, org_plane, organization, roster, topology,
};

impl ControlPlane {
    /// A control plane that has opened no connection.
    #[must_use]
    pub fn new(options: ControlOptions) -> Self {
        let offered_caps = options.client_caps();
        Self {
            attach_target: options.attach.clone(),
            viewport: geometry::ViewportIntent {
                desired: ViewportInfo::new(options.viewport.0, options.viewport.1),
                pending: false,
            },
            options,
            offered_caps,
            connection_epoch: 0,
            live_connection_epoch: None,
            inbound: Vec::new(),
            inbound_bytes: 0,
            status: Status::Idle,
            handshake_ready: false,
            attached_once: false,
            detach_requested: false,
            attach_ended: false,
            error: None,
            server: None,
            engine: None,
            engine_config: None,
            history_config: None,
            terminal_theme: None,
            #[cfg(feature = "engine")]
            publication: Arc::new(Publication::new()),
            topology: None,
            listings: topology::Listings::default(),
            active_attach_id: None,
            attach_terminals: HashSet::new(),
            attached_session: None,
            selected_session: None,
            terminal_attached: HashSet::new(),
            preserve_terminal_geometry: HashSet::new(),
            terminal_roles: HashMap::new(),
            geometry_bootstrapped: HashSet::new(),
            stream_recoveries: HashSet::new(),
            own_spawns: HashSet::new(),
            agent_streams: HashSet::new(),
            agent_catalog: Vec::new(),
            agent_subscriptions: HashMap::new(),
            retired_agents: HashSet::new(),
            agent_delivery: agent_delivery::AgentDelivery::default(),
            own_client_id: None,
            pending: HashMap::new(),
            request_seq: 1,
            input_replay: InputReplayJournal::new(),
            delivery_fences: HashMap::new(),
            input_delivery_ids: HashMap::new(),
            live_asks: HashMap::new(),
            command_states: HashMap::new(),
            input_deadlines: HashMap::new(),
            input_delivery_seq: 1,
            clock_origin: Instant::now(),
            outbound: Vec::new(),
            events: Vec::new(),
            clipboard_event_bytes: 0,
            clipboard_focus: None,
            events_dropped: 0,
            damaged: Vec::new(),
            extensions: extensions::Extensions::default(),
            roster: roster::RosterRecovery::default(),
            project_tag_pending: None,
            global_watch: global_metadata::GlobalWatch::default(),
            organization: organization::OrganizationReplica::new(),
            org: org_plane::OrgLane::default(),
            approvals: approvals::ApprovalWatch::default(),
        }
    }

    /// Override the history cache policy before opening the connection.
    /// Bindings with explicit cache and prefetch limits use this builder;
    /// ordinary runtime clients derive policy from [`ControlOptions`].
    #[must_use]
    pub fn with_history_config(mut self, config: HistoryCacheConfig) -> Self {
        self.set_history_config(config);
        self
    }

    /// The same override, for a binding that reaches an already-built plane
    /// through its runtime client rather than a builder.
    pub fn set_history_config(&mut self, config: HistoryCacheConfig) {
        self.history_config = Some(config.normalized());
    }

    /// Install (or clear, with `None`) the client-side terminal theme
    /// (ADR-0157). It applies to every replica now and on every later
    /// connection, and outlives reconnects; an application's own OSC colour
    /// changes still win over it.
    pub fn set_terminal_theme(&mut self, theme: Option<crate::engine::TerminalTheme>) {
        self.terminal_theme = theme;
        if let Some(engine) = &self.engine {
            engine.set_terminal_theme(theme);
        }
    }

    // ----- observation -------------------------------------------------

    /// How many connections this plane has opened. Zero before the first
    /// dial; every later value fences the state one connection built.
    #[must_use]
    pub const fn connection_epoch(&self) -> u64 {
        self.connection_epoch
    }

    /// Whether a drained queued batch still belongs to the active transport.
    /// Check while holding the same control guard used to feed its frame.
    #[must_use]
    pub const fn accepts_inbound(&self, epoch: u64) -> bool {
        matches!(self.live_connection_epoch, Some(current) if current == epoch)
    }

    /// The options this plane was built with.
    #[must_use]
    pub const fn options(&self) -> &ControlOptions {
        &self.options
    }

    /// Where the session is.
    #[must_use]
    pub const fn status(&self) -> Status {
        self.status
    }

    /// The last failure message.
    #[must_use]
    pub fn last_error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// What `HELLO_OK` negotiated on the latest connection.
    #[must_use]
    pub const fn server(&self) -> Option<&ServerInfo> {
        self.server.as_ref()
    }

    /// Whether `HELLO_OK` has been accepted on the current connection.
    #[must_use]
    pub const fn handshake_ready(&self) -> bool {
        self.handshake_ready
    }

    /// Whether any connection ever attached: after that, drops reconnect
    /// indefinitely.
    #[must_use]
    pub const fn attached_once(&self) -> bool {
        self.attached_once
    }

    /// The latest session graph.
    #[must_use]
    pub const fn topology(&self) -> Option<&Topology> {
        self.topology.as_ref()
    }

    /// The home session whose pumps the active connection-level attach opened.
    #[must_use]
    pub const fn attached_session(&self) -> Option<u32> {
        self.attached_session
    }

    /// The session currently selected by the consumer.
    #[must_use]
    pub const fn selected_session(&self) -> Option<u32> {
        self.selected_session
    }

    /// The engine host, once `HELLO_OK` selected a profile.
    #[must_use]
    pub const fn engine(&self) -> Option<&EngineHandle> {
        self.engine.as_ref()
    }

    /// The published grid frames.
    #[cfg(feature = "engine")]
    #[must_use]
    pub const fn publication(&self) -> &Arc<Publication> {
        &self.publication
    }

    /// Scroll and publish a terminal while routing any resulting history
    /// request through this control plane.
    #[cfg(feature = "engine")]
    pub fn scroll(&mut self, terminal_id: &ResourceId, scroll: Scroll) -> Result<(), EngineError> {
        let engine = self.engine.clone().ok_or(EngineError::Stopped)?;
        let outcome = engine.scroll(terminal_id, scroll)?;
        self.process_outcome(outcome, true)
            .map_err(|error| EngineError::Engine(error.to_string()))
    }

    /// Scroll an independent presentation and route its history requests.
    #[cfg(feature = "engine")]
    pub fn scroll_view(&mut self, view: crate::ViewId, scroll: Scroll) -> Result<(), EngineError> {
        let engine = self.engine.clone().ok_or(EngineError::Stopped)?;
        let outcome = engine.scroll_view(view, scroll)?;
        self.process_outcome(outcome, true)
            .map_err(|error| EngineError::Engine(error.to_string()))
    }

    /// Pin an independent presentation and route its history requests.
    #[cfg(feature = "engine")]
    pub fn pin_viewport_view(
        &mut self,
        view: crate::ViewId,
        anchor: u64,
    ) -> Result<(), EngineError> {
        let engine = self.engine.clone().ok_or(EngineError::Stopped)?;
        let outcome = engine.pin_view(view, anchor)?;
        self.process_outcome(outcome, true)
            .map_err(|error| EngineError::Engine(error.to_string()))
    }

    /// Pin the viewport at a tracked anchor and route history prefetch.
    #[cfg(feature = "engine")]
    pub fn pin_viewport(
        &mut self,
        terminal_id: &ResourceId,
        anchor: u64,
    ) -> Result<(), EngineError> {
        let engine = self.engine.clone().ok_or(EngineError::Stopped)?;
        let outcome = engine.pin_viewport(terminal_id, anchor)?;
        self.process_outcome(outcome, true)
            .map_err(|error| EngineError::Engine(error.to_string()))
    }

    /// Return the viewport to the live tail and route resulting effects.
    #[cfg(feature = "engine")]
    pub fn follow_live(&mut self, terminal_id: &ResourceId) -> Result<(), EngineError> {
        let engine = self.engine.clone().ok_or(EngineError::Stopped)?;
        let outcome = engine.follow_live(terminal_id)?;
        self.process_outcome(outcome, true)
            .map_err(|error| EngineError::Engine(error.to_string()))
    }

    /// The desired viewport in cells, not the server's applied geometry.
    #[must_use]
    pub const fn viewport(&self) -> (u16, u16) {
        (self.viewport.desired.cols, self.viewport.desired.rows)
    }

    /// Whether this connection's viewport takes part in window-size policy.
    #[must_use]
    pub const fn votes_on_geometry(&self) -> bool {
        self.viewport.desired.cols != 0 && self.viewport.desired.rows != 0
    }

    /// The payload limits inbound frames must be decoded under.
    #[must_use]
    pub fn decode_limits(&self) -> Option<BootstrapLimits> {
        self.server.as_ref().map(|server| server.limits)
    }

    /// Whether frames are waiting to be sent.
    #[must_use]
    pub const fn has_outbound(&self) -> bool {
        !self.outbound.is_empty()
    }

    /// Whether events are waiting to be drained.
    #[must_use]
    pub const fn has_events(&self) -> bool {
        !self.events.is_empty() || !self.damaged.is_empty()
    }

    // ----- connection lifecycle ---------------------------------------

    /// Open a manually driven plane with the embedder's client name.
    /// Returns `false` after the plane has already left `Idle`.
    pub fn open_explicit(&mut self, client_name: String) -> bool {
        if self.options.automatic_lifecycle || self.status != Status::Idle {
            return false;
        }
        self.options.client_name = client_name;
        self.connection_opened();
        true
    }

    /// Whether `terminal_id` belongs to the active session attach.
    #[must_use]
    pub fn active_attach_contains(&self, terminal_id: &ResourceId) -> bool {
        self.attach_terminals.contains(terminal_id)
            && self
                .engine
                .as_ref()
                .is_none_or(|engine| !engine.is_closed(terminal_id))
    }

    /// Whether a terminal belongs to any stream pump admitted by this
    /// control plane, including a manual binding's explicit operations.
    #[must_use]
    pub fn terminal_is_admitted(&self, terminal_id: &ResourceId) -> bool {
        (self.attach_terminals.contains(terminal_id)
            || self.terminal_attached.contains(terminal_id)
            || self.own_spawns.contains(terminal_id))
            && self
                .engine
                .as_ref()
                .is_none_or(|engine| !engine.is_closed(terminal_id))
    }

    /// Reserve a per-terminal pump queued by a manual binding with its role.
    pub fn admit_external_attach(
        &mut self,
        terminal_id: &ResourceId,
        role_policy: Option<RolePolicy>,
    ) -> bool {
        if self.terminal_is_admitted(terminal_id) {
            return false;
        }
        self.terminal_roles
            .insert(terminal_id.clone(), role_policy.unwrap_or_default());
        self.terminal_attached.insert(terminal_id.clone())
    }

    /// Record a server pump created by a manual binding's successful spawn.
    pub fn admit_external_spawn(&mut self, terminal_id: &ResourceId) -> bool {
        if self.terminal_is_admitted(terminal_id) {
            return false;
        }
        self.own_spawns.insert(terminal_id.clone())
    }

    /// Release one explicitly withdrawn terminal from both the attach
    /// inventory and the engine owner.
    pub fn release_terminal(&mut self, terminal_id: &ResourceId) -> bool {
        if self.clipboard_focus.as_ref() == Some(terminal_id) {
            self.clipboard_focus = None;
            self.events
                .retain(|event| !matches!(event, Event::ClipboardWrite { .. }));
            self.clipboard_event_bytes = 0;
        }
        self.preserve_terminal_geometry.remove(terminal_id);
        self.terminal_roles.remove(terminal_id);
        self.geometry_bootstrapped.remove(terminal_id);
        let known = self.attach_terminals.remove(terminal_id)
            | self.terminal_attached.remove(terminal_id)
            | self.own_spawns.remove(terminal_id)
            | self.agent_streams.remove(terminal_id);
        // Detach discards undrained publications. A later
        // `AgentSessionDeclared` clears that fence so the same id can commit
        // again. The stamp is taken here, before the kernel forgets the stream.
        let _ = self.agent_delivery.retire(terminal_id);
        let detached = self
            .engine
            .as_ref()
            .is_some_and(|engine| engine.detach(terminal_id.clone()));
        known || detached
    }

    /// Update the cursor carried by runtime-generated event subscriptions.
    pub const fn set_event_after_seq(&mut self, after_seq: Option<u64>) {
        self.options.event_after_seq = after_seq;
    }

    /// A transport is up: reset every per-connection correlation and queue
    /// `HELLO`. Frames still queued from the previous connection are
    /// discarded, as they were built against per-connection state.
    pub fn connection_opened(&mut self) {
        self.park_agent_delivery();
        if self.options.session_recovery == SessionRecovery::CallerValidated {
            self.attach_target = None;
            self.preserve_terminal_geometry.clear();
        }
        self.roster = roster::RosterRecovery::default();
        self.geometry_bootstrapped.clear();
        self.connection_epoch = self.connection_epoch.saturating_add(1);
        self.live_connection_epoch = Some(self.connection_epoch);
        self.outbound.clear();
        // Anything the consumer has not drained belongs to the connection
        // that just ended, and feeding it against fresh per-connection
        // state would be a protocol error.
        let _ = self.take_inbound();
        self.terminal_attached.clear();
        self.terminal_roles.clear();
        self.stream_recoveries.clear();
        self.own_spawns.clear();
        self.command_states.clear();
        self.handshake_ready = false;
        self.attach_ended = false;
        self.active_attach_id = None;
        self.attach_terminals.clear();
        self.attached_session = None;
        self.selected_session = None;
        self.input_replay.connection_lost();
        self.org_connection_lost();
        self.approvals_connection_ended();
        self.reset_extension_correlations("the connection ended before the server answered");
        self.fail_pending("connection replaced before the server answered", true);
        self.push_event(Event::ConnectionOpened {
            connection_epoch: self.connection_epoch,
        });
        self.set_status(Status::Connecting);
        self.queue_frame(&crate::handshake::hello(
            self.options.client_name.clone(),
            self.offered_caps,
        ));
    }

    /// The transport dropped; the driver will walk the ladder.
    pub fn connection_lost(&mut self, message: Option<String>) {
        self.park_agent_delivery();
        self.live_connection_epoch = None;
        let _ = self.take_inbound();
        self.handshake_ready = false;
        self.input_replay.connection_lost();
        self.org_connection_lost();
        self.approvals_connection_ended();
        self.reset_extension_correlations("the connection ended before the server answered");
        self.fail_native_pending("the connection ended before the server answered");
        if let Some(message) = &message {
            self.error = Some(message.clone());
        }
        self.push_event(Event::ConnectionLost { message });
        if !self.status.is_terminal() {
            self.set_status(Status::Connecting);
        }
    }

    /// The session ends in failure: no retry can help.
    pub fn fail(&mut self, message: impl Into<String>) {
        self.live_connection_epoch = None;
        let _ = self.take_inbound();
        self.handshake_ready = false;
        self.error = Some(message.into());
        self.strand_durable("the connection failed before the operation completed");
        self.org_strand("the connection failed before the operation completed");
        self.strand_extensions("the connection failed before the operation completed");
        self.approvals_connection_ended();
        self.fail_pending("the connection failed before the server answered", false);
        self.set_status(Status::Failed);
    }

    /// The session ends at the consumer's request.
    pub fn close(&mut self) {
        self.discard_agent_delivery();
        self.live_connection_epoch = None;
        self.handshake_ready = false;
        let _ = self.take_inbound();
        self.strand_durable("the client closed before the operation completed");
        self.org_strand("the client closed before the operation completed");
        self.strand_extensions("the client closed before the operation completed");
        self.fail_pending("the client closed before the server answered", false);
        if let Some(engine) = &self.engine {
            engine.reset_connection();
        }
        self.active_attach_id = None;
        self.attach_terminals.clear();
        self.attached_session = None;
        self.selected_session = None;
        self.terminal_attached.clear();
        self.terminal_roles.clear();
        self.agent_streams.clear();
        self.command_states.clear();
        self.preserve_terminal_geometry.clear();
        self.geometry_bootstrapped.clear();
        self.set_status(Status::Closed);
    }
}
