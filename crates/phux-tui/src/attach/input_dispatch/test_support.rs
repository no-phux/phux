//! Shared fixtures for the dispatcher test suites.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use phux_protocol::ResourceId;
use phux_protocol::input::InputEvent;
use phux_protocol::input::key::ModSet;
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use phux_protocol::wire::frame::FrameKind;

use crate::attach::actions::{PendingSplit, PendingWindow};
use crate::attach::connection::Connection;
use crate::attach::directory_picker::{DirectorySupport, PendingDirectory};
use crate::attach::focus::FocusHistory;
use crate::attach::input_replay::InputReplayJournal;
use crate::attach::paint::SidebarReservation;
use crate::attach::pane_state::{AttachKernel, AttentionNavigation, PaneSlot, VcsIndex};
use crate::attach::plugin_actions::PluginActionEntry;
use crate::attach::plugin_panes::PluginPaneEntry;
use crate::layout::{SplitDir, Workspace};
use crate::predict::{PredictionState, PredictiveConfig};
use crate::render::chrome::sidebar::SidebarTargets;
use crate::render::chrome::status_bar::{Position, StatusBarPainter};
use crate::render::overlay::OverlayState;
use crate::render::{ChromeBreakpoints, Theme};

use super::ctx::{DispatchCtx, DragGrab};
use super::dispatch::dispatch_input_events;
use super::effects::{ActionEffects, PendingSessionRename, ReattachTarget, apply_action_effects};
use super::run_action::run_action;

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
/// `keybindings`, `plugin_tx`) start `None` on the lent context.
#[allow(
    clippy::struct_excessive_bools,
    reason = "mirrors DispatchCtx's independent driver flags one-to-one"
)]
pub(super) struct CtxFixture {
    pub(super) engine_kernel: AttachKernel,
    pub(super) focus_history: FocusHistory,
    pub(super) workspace: Workspace,
    pub(super) layout_read_complete: bool,
    pub(super) viewport: (u16, u16),
    pub(super) cell_px: (u16, u16),
    pub(super) next_request_id: u32,
    pub(super) spawn_initial_size_supported: bool,
    pub(super) pending_splits: HashMap<u32, PendingSplit>,
    pub(super) pending_windows: HashMap<u32, PendingWindow>,
    pub(super) pending_floating: HashMap<u32, String>,
    pub(super) directory_support: DirectorySupport,
    pub(super) pending_directory: Option<PendingDirectory>,
    pub(super) pending_path: Option<crate::attach::path_picker::PendingPath>,
    pub(super) path_query_supported: bool,
    pub(super) expected_closes: HashSet<ResourceId>,
    pub(super) pending_kills: HashMap<u32, ResourceId>,
    pub(super) overlays: OverlayState,
    pub(super) theme: Theme,
    pub(super) sessions: Vec<phux_protocol::wire::info::SessionInfo>,
    pub(super) hosts: Vec<phux_protocol::wire::info::HostInventory>,
    pub(super) host_refresh_request: bool,
    pub(super) foreign_layouts: HashMap<phux_protocol::ids::SessionId, Workspace>,
    pub(super) foreign_agents: HashMap<ResourceId, phux_client::agent_meta::AgentRecord>,
    pub(super) foreign_attention: HashSet<ResourceId>,
    pub(super) focused_session: Option<phux_protocol::ids::SessionId>,
    pub(super) review: crate::attach::review::ReviewIndex,
    pub(super) session_name: String,
    pub(super) session_mru: Vec<String>,
    pub(super) project_tags: HashMap<phux_protocol::ids::SessionId, String>,
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
    pub(super) agent_meta: HashMap<ResourceId, phux_client::agent_meta::AgentRecord>,
    pub(super) vcs: VcsIndex,
    painted_targets: SidebarTargets,
}

impl Default for CtxFixture {
    /// A single-window workspace on an 80x24 viewport with 1x1 cells, no
    /// sidebar or bar, a confirmed layout read, and every server feature.
    fn default() -> Self {
        Self {
            engine_kernel: test_engine_kernel(),
            focus_history: FocusHistory::default(),
            workspace: Workspace::single(tid(1)),
            layout_read_complete: true,
            viewport: (80, 24),
            cell_px: (1, 1),
            next_request_id: 1,
            spawn_initial_size_supported: true,
            pending_splits: HashMap::new(),
            pending_windows: HashMap::new(),
            pending_floating: HashMap::new(),
            directory_support: DirectorySupport::HostAware,
            pending_directory: None,
            pending_path: None,
            path_query_supported: true,
            expected_closes: HashSet::new(),
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
            session_mru: Vec::new(),
            project_tags: HashMap::new(),
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
            agent_meta: HashMap::new(),
            vcs: VcsIndex::default(),
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
            .unwrap_or_else(|| targets(0, self.workspace.windows.len(), 0));
        DispatchCtx {
            control_dial: None,
            engine_kernel: &mut self.engine_kernel,
            resolver: None,
            focus_history: self.focus_history.clone(),
            workspace: &mut self.workspace,
            layout_read_complete: self.layout_read_complete,
            viewport: self.viewport,
            cell_px: self.cell_px,
            next_request_id: &mut self.next_request_id,
            input_replay: None,
            spawn_initial_size_supported: self.spawn_initial_size_supported,
            pending_splits: &mut self.pending_splits,
            pending_windows: &mut self.pending_windows,
            pending_floating: &mut self.pending_floating,
            directory_support: self.directory_support,
            pending_directory: &mut self.pending_directory,
            pending_path: &mut self.pending_path,
            path_query_supported: self.path_query_supported,
            own_client_id: None,
            expected_closes: &mut self.expected_closes,
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
                review: &self.review,
            },
            host_refresh_request: &mut self.host_refresh_request,
            session_name: &mut self.session_name,
            session_mru: &mut self.session_mru,
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
            plugin_tx: None,
            reload_request: &mut self.reload_request,
            host_switch_request: &mut self.host_switch_request,
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
        let focused = self.workspace.active_window().and_then(|w| w.focus.clone());
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

/// A fixture over `workspace` with request ids starting at 100.
pub(super) fn fx(workspace: Workspace) -> CtxFixture {
    CtxFixture {
        workspace,
        next_request_id: 100,
        ..CtxFixture::default()
    }
}

/// Every frame the peer received until the writer's EOF.
#[allow(clippy::future_not_send, reason = "current-thread test state")]
async fn drain(mut peer: Connection) -> Vec<FrameKind> {
    let mut received = Vec::new();
    while let Ok(frame) = tokio::time::timeout(PEER_DRAIN_DEADLINE, peer.recv())
        .await
        .expect("timed out draining the peer connection")
    {
        received.push(frame);
    }
    received
}

/// What one dispatched batch did.
pub(super) struct Sent {
    pub(super) frames: Vec<FrameKind>,
    pub(super) detach: bool,
    pub(super) repainted: bool,
}

/// Everything `dispatch_input_events` threads besides the context.
pub(super) struct Env<'a> {
    pub(super) fx: CtxFixture,
    pub(super) panes: HashMap<ResourceId, PaneSlot>,
    pub(super) focused: Option<ResourceId>,
    pub(super) predict: PredictionState,
    pub(super) resolver: Option<phux_config::keybind::Resolver>,
    pub(super) keybindings: Option<phux_config::KeybindingsCfg>,
    pub(super) journal: Option<&'a RefCell<InputReplayJournal>>,
}

impl Env<'_> {
    /// Focus on pane 1, predictor disabled, no resolver.
    pub(super) fn new(fx: CtxFixture) -> Self {
        Self {
            fx,
            panes: HashMap::new(),
            focused: Some(tid(1)),
            predict: PredictionState::new(PredictiveConfig::disabled(), 80, 24),
            resolver: None,
            keybindings: None,
            journal: None,
        }
    }

    /// Replace the kernel and pane slots with published replicas of
    /// `(id, cols, rows, vt)`.
    pub(super) fn published(mut self, entries: &[(&ResourceId, u16, u16, &[u8])]) -> Self {
        let (kernel, _, panes) = crate::attach::pane_state::published_test_state(entries);
        self.fx.engine_kernel = kernel;
        self.panes = panes;
        self
    }

    /// Install the default config's resolver and keybindings.
    pub(super) fn with_default_bindings(mut self) -> Self {
        let cfg = default_cfg();
        self.resolver =
            Some(phux_config::keybind::Resolver::new(&cfg.keybindings).expect("resolver"));
        self.keybindings = Some(cfg.keybindings);
        self
    }

    #[allow(clippy::future_not_send, reason = "current-thread test state")]
    pub(super) async fn dispatch(&mut self, mut events: Vec<InputEvent>) -> Sent {
        let (a, b) = tokio::net::UnixStream::pair().expect("uds pair");
        let mut conn = Connection::from_stream(a);
        let repainted = {
            let mut ctx = self.fx.ctx();
            ctx.resolver = self.resolver.as_mut();
            ctx.keybindings = self.keybindings.as_ref();
            ctx.input_replay = self.journal;
            dispatch_input_events(
                &mut Vec::new(),
                &mut conn,
                &mut events,
                &mut self.focused,
                &mut self.predict,
                &mut self.panes,
                &mut ctx,
            )
            .await
            .expect("dispatch")
        };
        drop(conn);
        // Per batch, as the driver's flag is per attach: the next starts clear.
        let detach = std::mem::take(&mut self.fx.detach_pending);
        Sent {
            frames: drain(Connection::from_stream(b)).await,
            detach,
            repainted,
        }
    }
}

pub(super) fn default_cfg() -> phux_config::Config {
    phux_config::parse_str(
        phux_config::DEFAULT_CONFIG_TOML,
        std::path::Path::new("default.toml"),
    )
    .expect("default config parses")
}

/// A [`ResolvedAction`] named `name` with `args`.
pub(super) fn act(
    name: &str,
    args: &[(&str, toml::Value)],
) -> phux_config::keybind::ResolvedAction {
    phux_config::keybind::ResolvedAction {
        action: name.to_owned(),
        args: args
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect(),
    }
}

/// Two leaves split side by side at `ratio`, focus on the left.
pub(super) fn two_pane_workspace_at(ratio: f32) -> Workspace {
    use crate::layout::{LayoutNode, LayoutState, WindowState, split_at};
    let tree = split_at(
        &LayoutNode::Leaf(tid(1)),
        &tid(1),
        &tid(2),
        SplitDir::Horizontal,
        ratio,
    )
    .expect("split");
    Workspace {
        windows: vec![WindowState::new(
            "1".to_owned(),
            LayoutState {
                tree: Some(tree),
                focus: Some(tid(1)),
            },
        )],
        active: 0,
    }
}

pub(super) fn two_pane_workspace() -> Workspace {
    two_pane_workspace_at(0.5)
}

pub(super) fn press(key: phux_protocol::input::key::PhysicalKey, text: Option<&str>) -> InputEvent {
    use phux_protocol::input::key::{KeyAction, KeyEvent};
    InputEvent::Key(KeyEvent {
        action: KeyAction::Press,
        key,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: text.map(ToOwned::to_owned),
        unshifted_codepoint: text.and_then(|t| t.chars().next()).map(u32::from),
    })
}

pub(super) fn mev(action: MouseAction, button: MouseButton, x: f64, y: f64) -> MouseEvent {
    MouseEvent {
        action,
        button,
        mods: ModSet::empty(),
        x,
        y,
    }
}

pub(super) fn mouse(action: MouseAction, button: MouseButton, x: f64, y: f64) -> InputEvent {
    InputEvent::Mouse(mev(action, button, x, y))
}

/// A left-button event at cell `(x, y)`.
pub(super) fn left(action: MouseAction, x: u16, y: u16) -> InputEvent {
    mouse(action, MouseButton::Left, f64::from(x), f64::from(y))
}

/// A right press at cell `(x, y)`.
pub(super) fn right_press(x: u16, y: u16) -> InputEvent {
    mouse(
        MouseAction::Press,
        MouseButton::Right,
        f64::from(x),
        f64::from(y),
    )
}

pub(super) fn targets(needs_you: usize, windows: usize, roster: usize) -> SidebarTargets {
    use crate::render::chrome::sidebar::{SessionRosterTarget, SidebarCounts, SidebarTarget};
    // Window-only fixtures still represent one current session.
    let roster = roster.max(usize::from(windows > 0));
    SidebarTargets {
        counts: SidebarCounts {
            needs_you,
            windows,
            roster,
            active_session: (windows > 0 && roster > 0).then_some(0),
            host_starts: (0..roster.min(128)).fold(0, |mask, j| mask | (1u128 << j)),
            rule: crate::render::chrome::sidebar::SidebarRule::Trailing,
            plugin: crate::render::chrome::sidebar_sections::PluginShape::default(),
        },
        // Row 0 is local; the rest are peers, so one fixture covers both.
        needs_you: (0..needs_you)
            .map(|j| {
                if j == 0 {
                    SidebarTarget::Window(1)
                } else {
                    SidebarTarget::Session {
                        name: format!("peer-{j}"),
                        id: None,
                        window: Some(2),
                        pane: Some(3),
                        resource: None,
                    }
                }
            })
            .collect(),
        roster: (0..roster)
            .map(|j| {
                Some(SessionRosterTarget {
                    name: format!("space-{j}"),
                    id: None,
                    host: None,
                    switch_host: None,
                })
            })
            .collect(),
        plugin: Vec::new(),
    }
}
