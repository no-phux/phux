//! Shared fixtures for the dispatcher test suites.

use std::collections::{BTreeMap, HashMap, HashSet};

use phux_protocol::ResourceId;
use phux_protocol::input::InputEvent;

use crate::attach::actions::{PendingSplit, PendingWindow};
use crate::attach::directory_picker::{DirectorySupport, PendingDirectory};
use crate::attach::focus::FocusHistory;
use crate::attach::paint::SidebarReservation;
use crate::attach::pane_state::{AttachKernel, AttentionNavigation, VcsIndex};
use crate::attach::plugin_actions::PluginActionEntry;
use crate::attach::plugin_panes::PluginPaneEntry;
use crate::layout::{SplitDir, Workspace};
use crate::render::chrome::sidebar::SidebarTargets;
use crate::render::chrome::status_bar::{Position, StatusBarPainter};
use crate::render::overlay::OverlayState;
use crate::render::{ChromeBreakpoints, Theme};

use super::ctx::{DispatchCtx, DragGrab};
use super::effects::{PendingSessionRename, ReattachTarget};

/// Ceiling for draining a scripted peer connection whose writer has
/// already been dropped.
///
/// Not load-bearing: the drain ends on the peer's EOF, and the
/// assertions are on the frames collected — never on how fast they
/// arrived. The timeout only stops a peer that never hangs up from
/// wedging the binary. The 5s it replaces was generous on an idle laptop
/// and a measurement of the scheduler on a saturated one (phux-br1f).
pub(super) const PEER_DRAIN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

pub(super) fn tid(id: u32) -> ResourceId {
    ResourceId::local(id)
}

pub(super) fn test_engine_kernel() -> super::super::pane_state::AttachKernel {
    phux_client_core::session::SessionKernel::new(
        phux_client_core::engine::ghostty::GhosttyAdapter::new(
            phux_protocol::BootstrapLimits::default(),
        ),
        phux_protocol::BootstrapProfile::SynthesizedVtRaw,
    )
}

/// Owned backing state for a test [`DispatchCtx`].
///
/// The context is mostly `&mut` borrows of driver-owned state; this
/// fixture owns every one of them so a test sets the fields it cares
/// about, calls [`CtxFixture::ctx`], and reads the results back from the
/// fixture once the context is dropped. Borrows the fixture cannot own
/// sensibly (`control_dial`, `resolver`, `input_replay`, `keybindings`,
/// `plugin_tx`, or state a helper's caller lends in) are left `None` /
/// fixture-backed by `ctx()` and overridden on the returned context.
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
    pub(super) directory_support: DirectorySupport,
    pub(super) pending_directory: Option<PendingDirectory>,
    pub(super) expected_closes: HashSet<ResourceId>,
    pub(super) overlays: OverlayState,
    pub(super) theme: Theme,
    pub(super) sessions: Vec<phux_protocol::wire::info::SessionInfo>,
    pub(super) hosts: Vec<phux_protocol::wire::info::HostInventory>,
    pub(super) host_refresh_request: bool,
    pub(super) foreign_layouts: HashMap<phux_protocol::ids::SessionId, Workspace>,
    pub(super) foreign_agents: HashMap<ResourceId, phux_client::agent_meta::AgentRecord>,
    pub(super) focused_session: Option<phux_protocol::ids::SessionId>,
    pub(super) session_name: String,
    pub(super) rename_pending: Option<PendingSessionRename>,
    pub(super) switch_request: Option<ReattachTarget>,
    pub(super) zoomed: Option<ResourceId>,
    pub(super) sidebar: Option<SidebarReservation>,
    pub(super) sidebar_enabled: bool,
    pub(super) sidebar_width: u16,
    pub(super) chrome: ChromeBreakpoints,
    /// The sidebar's painted click table. `None` derives
    /// `targets(0, workspace.windows.len(), 0)` from [`Self::workspace`]
    /// at [`Self::ctx`] time (phux-k0cw: the strip's shape comes from the
    /// painted target table, not from the workspace).
    pub(super) sidebar_targets: Option<SidebarTargets>,
    pub(super) bar: Option<Position>,
    pub(super) status_bar: Option<StatusBarPainter>,
    pub(super) drag: Option<DragGrab>,
    pub(super) mouse_optout: HashSet<ResourceId>,
    pub(super) attention_navigation: AttentionNavigation,
    pub(super) plugin_actions: Vec<PluginActionEntry>,
    pub(super) plugin_panes: Vec<PluginPaneEntry>,
    pub(super) reload_request: bool,
    pub(super) agent_meta: HashMap<ResourceId, phux_client::agent_meta::AgentRecord>,
    pub(super) vcs: VcsIndex,
    /// The table `ctx()` lends, resolved from [`Self::sidebar_targets`].
    painted_targets: SidebarTargets,
}

impl Default for CtxFixture {
    /// A single-window workspace on an 80x24 viewport with 1x1 cells, no
    /// sidebar, no bar, a confirmed initial layout read, and every
    /// server feature the context gates on.
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
            directory_support: DirectorySupport::HostAware,
            pending_directory: None,
            expected_closes: HashSet::new(),
            overlays: OverlayState::new(),
            theme: Theme::default(),
            sessions: Vec::new(),
            hosts: Vec::new(),
            host_refresh_request: false,
            foreign_layouts: HashMap::new(),
            foreign_agents: HashMap::new(),
            focused_session: None,
            session_name: String::new(),
            rename_pending: None,
            switch_request: None,
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
            agent_meta: HashMap::new(),
            vcs: VcsIndex::default(),
            painted_targets: SidebarTargets::default(),
        }
    }
}

impl CtxFixture {
    /// Lend every owned field as a [`DispatchCtx`]. The borrowed-only
    /// fields (`control_dial`, `resolver`, `input_replay`, `keybindings`,
    /// `plugin_tx`) start `None`; set them on the returned context.
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
            directory_support: self.directory_support,
            pending_directory: &mut self.pending_directory,
            expected_closes: &mut self.expected_closes,
            overlays: &mut self.overlays,
            keybindings: None,
            theme: &self.theme,
            sessions: &self.sessions,
            hosts: &self.hosts,
            host_refresh_request: &mut self.host_refresh_request,
            foreign_layouts: &self.foreign_layouts,
            foreign_agents: &self.foreign_agents,
            focused_session: self.focused_session,
            session_name: &mut self.session_name,
            rename_pending: &mut self.rename_pending,
            switch_request: &mut self.switch_request,
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
            agent_meta: &self.agent_meta,
            vcs: &mut self.vcs,
        }
    }
}

/// Build a [`ResolvedAction`] with no args.
pub(super) fn bare_action(name: &str) -> phux_config::keybind::ResolvedAction {
    phux_config::keybind::ResolvedAction {
        action: name.to_owned(),
        args: BTreeMap::new(),
    }
}

/// A two-pane Horizontal split with focus on the left leaf, root
/// ratio 0.5 — the fixture the `resize-pane` dispatch tests mutate.
pub(super) fn two_pane_workspace() -> Workspace {
    use crate::layout::{LayoutState, WindowState, split_at};
    let tree = split_at(
        &crate::layout::LayoutNode::Leaf(tid(1)),
        &tid(1),
        &tid(2),
        SplitDir::Horizontal,
        0.5,
    )
    .unwrap();
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

pub(super) fn press(key: phux_protocol::input::key::PhysicalKey, text: Option<&str>) -> InputEvent {
    use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet};
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

pub(super) fn targets(
    needs_you: usize,
    windows: usize,
    roster: usize,
) -> crate::render::chrome::sidebar::SidebarTargets {
    use crate::render::chrome::sidebar::{
        SessionRosterTarget, SidebarCounts, SidebarTarget, SidebarTargets,
    };
    // Window-only fixtures still represent one current session.
    let roster = roster.max(usize::from(windows > 0));
    SidebarTargets {
        counts: SidebarCounts {
            needs_you,
            windows,
            roster,
            active_session: (windows > 0 && roster > 0).then_some(0),
            rule: crate::render::chrome::sidebar::SidebarRule::Trailing,
        },
        needs_you: (0..needs_you)
            .map(|j| {
                // Row 0 is local; the rest are peers, so one fixture
                // exercises both commit shapes.
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
                })
            })
            .collect(),
    }
}
