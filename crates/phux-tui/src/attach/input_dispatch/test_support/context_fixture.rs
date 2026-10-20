/// Ceiling for draining a peer whose writer has been dropped. The drain ends
/// on EOF; this only stops a peer that never hangs up from wedging the test.
const PEER_DRAIN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

pub(super) fn tid(id: u32) -> ResourceId {
    ResourceId::local(id)
}

pub(super) fn test_engine_kernel() -> AttachKernel {
    phux_client_core::session::SessionKernel::new(
        phux_client_core::engine::ghostty::GhosttyAdapter::new(
            phux_protocol::BootstrapLimits::default(),
        ),
        phux_protocol::BootstrapProfile::SynthesizedVtRaw,
    )
}

/// Owned backing state for a test [`DispatchCtx`]: set the fields a test
/// cares about, lend it with [`CtxFixture::ctx`], read results back after.
/// Borrow-only fields (`control_dial`, `resolver`, `input_replay`,
/// `keybindings`) start `None` on the lent context.
#[allow(
    clippy::struct_excessive_bools,
    reason = "mirrors DispatchCtx's independent driver flags one-to-one"
)]
pub(super) struct CtxFixture {
    pub(super) engine_kernel: AttachKernel,
    pub(super) focus_history: FocusHistory,
    pub(super) workspace: ArrangementDoc,
    pub(super) layout_read_complete: bool,
    pub(super) unloaded_arrangement_read: Option<bool>,
    pub(super) deferred_arrangement_edits: Vec<DeferredArrangementEdit>,
    pub(super) viewport: (u16, u16),
    pub(super) cell_px: (u16, u16),
    pub(super) next_request_id: u32,
    pub(super) spawn_initial_size_supported: bool,
    pub(super) organization_supported: bool,
    pub(super) pending_splits: HashMap<u32, PendingSplit>,
    pub(super) pending_windows: HashMap<u32, PendingWindow>,
    pub(super) pending_floating: HashMap<u32, String>,
    pub(super) directory_support: DirectorySupport,
    pub(super) pending_directory: Option<PendingDirectory>,
    pub(super) pending_path: Option<crate::attach::path_picker::PendingPath>,
    pub(super) path_query_supported: bool,
    pub(super) expected_closes: HashSet<ResourceId>,
    pub(super) session_members: HashSet<ResourceId>,
    pub(super) pending_kills: HashMap<u32, ResourceId>,
    pub(super) overlays: OverlayState,
    pub(super) theme: Theme,
    pub(super) sessions: Vec<phux_protocol::wire::info::SessionInfo>,
    pub(super) hosts: Vec<phux_protocol::wire::info::HostInventory>,
    pub(super) host_refresh_request: bool,
    pub(super) foreign_layouts: HashMap<phux_protocol::ids::SessionId, ArrangementDoc>,
    pub(super) foreign_agents: HashMap<ResourceId, phux_client::agent_meta::AgentRecord>,
    pub(super) foreign_attention: HashSet<ResourceId>,
    pub(super) focused_session: Option<phux_protocol::ids::SessionId>,
    pub(super) review: crate::attach::review::ReviewIndex,
    pub(super) session_name: String,
    pub(super) project_tags: HashMap<phux_protocol::ids::SessionId, String>,
    pub(super) collapsed_projects: std::collections::BTreeSet<String>,
    pub(super) rename_pending: Option<PendingSessionRename>,
    pub(super) rename_notice: Option<String>,
    pub(super) switch_request: Option<ReattachTarget>,
    pub(super) detach_pending: bool,
    pub(super) zoomed: Option<ResourceId>,
    pub(super) sidebar: Option<SidebarReservation>,
    pub(super) sidebar_enabled: bool,
    pub(super) sidebar_width: u16,
    pub(super) chrome: ChromeBreakpoints,
    /// The painted click table; `None` derives `targets(0, windows, 0)`
    /// from the workspace at `ctx()` time.
    pub(super) sidebar_targets: Option<SidebarTargets>,
    pub(super) bar: Option<Position>,
    pub(super) status_bar: Option<StatusBarPainter>,
    pub(super) drag: Option<DragGrab>,
    pub(super) mouse_optout: HashSet<ResourceId>,
    pub(super) attention_navigation: AttentionNavigation,
    pub(super) plugin_actions: Vec<PluginActionEntry>,
    pub(super) plugin_panes: Vec<PluginPaneEntry>,
    pub(super) reload_request: bool,
    pub(super) host_switch_request: Option<(String, String)>,
    pub(super) project_toggle_request: Option<String>,
    pub(super) agents_filter_request: Option<crate::render::chrome::sidebar::FilterRequest>,
    pub(super) agent_meta: HashMap<ResourceId, phux_client::agent_meta::AgentRecord>,
    pub(super) vcs: VcsIndex,
    /// The fixture's own job facility; await `jobs.next()` for results.
    pub(super) jobs: crate::attach::jobs::Jobs,
    painted_targets: SidebarTargets,
}

impl Default for CtxFixture {
    /// A single-window workspace on an 80x24 viewport with 1x1 cells, no
    /// sidebar or bar, a confirmed layout read, and every server feature.
    fn default() -> Self {
        Self {
            engine_kernel: test_engine_kernel(),
            focus_history: FocusHistory::default(),
            workspace: ArrangementDoc::single(tid(1)),
            layout_read_complete: true,
            unloaded_arrangement_read: None,
            deferred_arrangement_edits: Vec::new(),
            viewport: (80, 24),
            cell_px: (1, 1),
            next_request_id: 1,
            spawn_initial_size_supported: true,
            organization_supported: false,
            pending_splits: HashMap::new(),
            pending_windows: HashMap::new(),
            pending_floating: HashMap::new(),
            directory_support: DirectorySupport::HostAware,
            pending_directory: None,
            pending_path: None,
            path_query_supported: true,
            expected_closes: HashSet::new(),
            session_members: HashSet::new(),
            pending_kills: HashMap::new(),
            overlays: OverlayState::new(),
            theme: Theme::default(),
            sessions: Vec::new(),
            hosts: Vec::new(),
            host_refresh_request: false,
            foreign_layouts: HashMap::new(),
            foreign_agents: HashMap::new(),
            foreign_attention: HashSet::new(),
            focused_session: None,
            review: crate::attach::review::ReviewIndex::new(),
            session_name: String::new(),
            project_tags: HashMap::new(),
            collapsed_projects: std::collections::BTreeSet::new(),
            rename_pending: None,
            rename_notice: None,
            switch_request: None,
            detach_pending: false,
            zoomed: None,
            sidebar: None,
            sidebar_enabled: false,
            sidebar_width: 20,
            chrome: ChromeBreakpoints::default(),
            sidebar_targets: None,
            bar: None,
            status_bar: None,
            drag: None,
            mouse_optout: HashSet::new(),
            attention_navigation: AttentionNavigation::default(),
            plugin_actions: Vec::new(),
            plugin_panes: Vec::new(),
            reload_request: false,
            host_switch_request: None,
            project_toggle_request: None,
            agents_filter_request: None,
            agent_meta: HashMap::new(),
            vcs: VcsIndex::default(),
            jobs: crate::attach::jobs::Jobs::new(),
            painted_targets: SidebarTargets::default(),
        }
    }
}

impl CtxFixture {
    /// Lend every owned field as a [`DispatchCtx`].
    pub(super) fn ctx(&mut self) -> DispatchCtx<'_> {
        self.painted_targets = self
            .sidebar_targets
            .clone()
            .unwrap_or_else(|| targets(0, self.workspace.groups.len(), 0));
        DispatchCtx {
            control_dial: None,
            engine_kernel: &mut self.engine_kernel,
            resolver: None,
            focus_history: self.focus_history.clone(),
            workspace: &mut self.workspace,
            layout_read_complete: self.layout_read_complete,
            unloaded_arrangement_read: self.unloaded_arrangement_read.as_mut(),
            deferred_arrangement_edits: Some(&mut self.deferred_arrangement_edits),
            arrangement_write_owed: None,
            viewport: self.viewport,
            cell_px: self.cell_px,
            next_request_id: &mut self.next_request_id,
            input_replay: None,
            spawn_initial_size_supported: self.spawn_initial_size_supported,
            organization_supported: self.organization_supported,
            pending_splits: &mut self.pending_splits,
            pending_windows: &mut self.pending_windows,
            pending_floating: &mut self.pending_floating,
            directory_support: self.directory_support,
            pending_directory: &mut self.pending_directory,
            pending_path: &mut self.pending_path,
            path_query_supported: self.path_query_supported,
            own_client_id: None,
            expected_closes: &mut self.expected_closes,
            session_members: &self.session_members,
            pending_kills: &mut self.pending_kills,
            overlays: &mut self.overlays,
            keybindings: None,
            theme: &self.theme,
            peers: crate::attach::sidebar_zones::PeerInputs {
                serving_host: None,
                origin: None,
                remote_hosts: &[],
                hosts: &self.hosts,
                sessions: &self.sessions,
                focused_session: self.focused_session,
                windows: &[],
                resources: &[],
                foreign_layouts: &self.foreign_layouts,
                foreign_agents: &self.foreign_agents,
                foreign_attention: &self.foreign_attention,
                project_tags: &self.project_tags,
                collapsed_projects: &self.collapsed_projects,
                review: &self.review,
            },
            host_refresh_request: &mut self.host_refresh_request,
            session_name: &mut self.session_name,
            rename_pending: &mut self.rename_pending,
            rename_notice: &mut self.rename_notice,
            switch_request: &mut self.switch_request,
            detach_pending: &mut self.detach_pending,
            zoomed: &mut self.zoomed,
            sidebar: self.sidebar,
            sidebar_enabled: &mut self.sidebar_enabled,
            sidebar_width: &mut self.sidebar_width,
            chrome: self.chrome,
            sidebar_targets: &self.painted_targets,
            bar: self.bar,
            status_bar: self.status_bar.as_ref(),
            drag: &mut self.drag,
            mouse_optout: &mut self.mouse_optout,
            attention_navigation: &mut self.attention_navigation,
            plugin_actions: &self.plugin_actions,
            plugin_panes: &self.plugin_panes,
            jobs: &self.jobs,
            reload_request: &mut self.reload_request,
            host_switch_request: &mut self.host_switch_request,
            project_toggle_request: &mut self.project_toggle_request,
            agents_filter_request: &mut self.agents_filter_request,
            agent_meta: &self.agent_meta,
            vcs: &mut self.vcs,
        }
    }

    /// `run_action` against the fixture's workspace, focused on its active
    /// window's focus.
    pub(super) fn run(&mut self, action: &phux_config::keybind::ResolvedAction) -> ActionEffects {
        self.run_in(action, &HashMap::new())
    }

    /// [`Self::run`] with the dispatcher's pane slots.
    pub(super) fn run_in(
        &mut self,
        action: &phux_config::keybind::ResolvedAction,
        panes: &HashMap<ResourceId, PaneSlot>,
    ) -> ActionEffects {
        let focused = self.workspace.active_group().and_then(|w| w.focus.clone());
        let mut ctx = self.ctx();
        run_action(action, &mut ctx, focused.as_ref(), panes)
    }

    /// `apply_action_effects` over a socket pair; returns what the peer got.
    #[allow(clippy::future_not_send, reason = "current-thread test state")]
    pub(super) async fn apply(&mut self, effects: ActionEffects) -> Vec<FrameKind> {
        let (a, b) = tokio::net::UnixStream::pair().expect("uds pair");
        let mut conn = Connection::from_stream(a);
        let mut predict = PredictionState::new(PredictiveConfig::disabled(), 80, 24);
        {
            let mut ctx = self.ctx();
            apply_action_effects(
                effects,
                &mut Vec::new(),
                &mut conn,
                &mut ctx,
                &mut None,
                &mut predict,
                &HashMap::new(),
            )
            .await
            .expect("apply effects");
        }
        drop(conn);
        // Each applied batch starts with no detach in flight.
        self.detach_pending = false;
        drain(Connection::from_stream(b)).await
    }
}
