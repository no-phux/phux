//! The mutable dispatch context (`DispatchCtx`) and the in-flight
//! chrome-drag state (`DragGrab`: dividers, the sidebar edge, window tabs).

use std::collections::{HashMap, HashSet};

use phux_config::plugin::actions::PluginActionEntry;
use phux_protocol::ResourceId;

use crate::attach::actions::{PendingSplit, PendingWindow};
use crate::attach::focus::FocusHistory;
use crate::attach::jobs::Jobs;
use crate::attach::paint::SidebarReservation;
use crate::attach::pane_state::AttentionNavigation;
use crate::attach::plugin_panes::PluginPaneEntry;
use crate::attach::sidebar_zones::PeerInputs;
use crate::layout::{ArrangementDoc, SplitDir};
use crate::render::overlay::OverlayState;
use crate::render::{ChromeBreakpoints, Theme};

use super::effects::ReattachTarget;

/// Driver state the dispatch path reads and mutates, lent per batch.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent negotiated-capability flags lent from the driver; a bitset would obscure every read site"
)]
pub(in crate::attach) struct DispatchCtx<'a> {
    /// Dial for a dedicated request/response connection (the attach stream
    /// interleaves frames).
    pub control_dial: Option<&'a crate::attach::Dial>,
    /// Connection-owned engine replicas used for terminal queries and local scrolling.
    pub engine_kernel: &'a mut crate::attach::pane_state::AttachKernel,
    /// Non-Terminal resources the server's graph named: placeholder leaves.
    pub leaves: &'a crate::attach::surface_leaf::LeafBook,
    /// Keybind resolver; `None` when the config failed to parse, so every
    /// key forwards to the focused pane.
    pub resolver: Option<&'a mut phux_config::keybind::Resolver>,
    /// Client-local focus transition/MRU bookkeeping.
    pub focus_history: FocusHistory,
    /// Client-side multi-window mirror; pane actions operate on the active
    /// window.
    pub workspace: &'a mut ArrangementDoc,
    /// A correlated initial layout GET confirmed valid metadata or absence.
    pub layout_read_complete: bool,
    /// An unloaded arrangement link: a held hidden-view action sets this to
    /// request one retry.
    pub unloaded_arrangement_read: Option<&'a mut bool>,
    /// `close-view` / `reopen-views` pressed before that read, applied once
    /// it completes. `None` drops the keystroke instead.
    pub deferred_arrangement_edits: Option<&'a mut Vec<super::deferred::DeferredArrangementEdit>>,
    /// Set when the arrangement is a record the driver writes after the
    /// batch; a layout broadcast raises it instead of `SET_METADATA`.
    pub arrangement_write_owed: Option<&'a mut bool>,
    /// Outer-viewport `(cols, rows)`.
    pub viewport: (u16, u16),
    /// Host cell size in pixels, derived like the server's; `INPUT_MOUSE` is
    /// scaled by it at the send boundary.
    pub cell_px: (u16, u16),
    /// Monotonic source of request ids.
    pub next_request_id: &'a mut u32,
    /// ADR-0053 replay journal on a remote reconnect lane; routes pastes
    /// through `APPLY_INPUT`. `None` on UDS.
    pub input_replay:
        Option<&'a std::cell::RefCell<crate::attach::input_replay::InputReplayJournal>>,
    /// Whether the server advertises `ORGANIZATION` (ADR-0161).
    pub organization_supported: bool,
    /// Whether a slot tab edit is kept: the arrangement is a record, on a
    /// server that advertises `SLOT_TABS`.
    pub slot_tabs: bool,
    /// Parked `split-pane`s awaiting `RESOURCE_SPAWNED`, by request id.
    pub pending_splits: &'a mut HashMap<u32, PendingSplit>,
    /// Parked `new-window`s awaiting `RESOURCE_SPAWNED`, same id space.
    pub pending_windows: &'a mut HashMap<u32, PendingWindow>,
    /// ADR-0147: floating overlay spawns awaiting `RESOURCE_SPAWNED`, with
    /// the box title.
    pub pending_floating: &'a mut HashMap<u32, String>,
    /// The `LIST_DIRECTORY` the directory picker awaits; newest wins.
    pub pending_directory: &'a mut Option<crate::attach::directory_picker::PendingDirectory>,
    /// Original insertion target and newest in-flight query.
    pub pending_path: &'a mut Option<crate::attach::path_picker::PendingPath>,
    /// Our server-assigned id for input-lease checks.
    pub own_client_id: Option<phux_protocol::ids::ClientId>,
    /// Terminals whose close this client requested (no exit notice).
    pub expected_closes: &'a mut HashSet<ResourceId>,
    /// Terminal-kind members of the attached session, arranged or not.
    pub session_members: &'a HashSet<ResourceId>,
    /// `request_id` -> Terminal for sent kills; a `TerminalNotFound` refusal
    /// folds the dead leaf out.
    pub pending_kills: &'a mut HashMap<u32, ResourceId>,
    /// Overlay stack: when non-empty, keys route to the top overlay.
    pub overlays: &'a mut OverlayState,
    /// The on-disk keybindings, for chord labels; `None` when config load
    /// failed.
    pub keybindings: Option<&'a phux_config::KeybindingsCfg>,
    /// Chrome and overlay theme.
    pub theme: &'a Theme,
    /// The peer-wide view: session graph, host inventory, peer layouts,
    /// agent records, and asks.
    pub peers: PeerInputs<'a>,
    /// Set when an action wants a fresher host inventory.
    pub host_refresh_request: &'a mut bool,
    /// The attached session's name; changed only by a confirmed rename.
    pub session_name: &'a mut String,
    /// In-flight `rename-session` confirmation; `None` when none is
    /// outstanding.
    pub rename_pending: &'a mut Option<super::effects::PendingSessionRename>,
    /// A rename or create the name policy refused, for the status bar.
    pub rename_notice: &'a mut Option<String>,
    /// Out-channel for a committed re-attach.
    pub switch_request: &'a mut Option<ReattachTarget>,
    /// A `DETACH` is in flight, so a second is not sent.
    pub detach_pending: &'a mut bool,
    /// The driver's pane-zoom state.
    pub zoomed: &'a mut Option<ResourceId>,
    /// The active sidebar reservation, `None` when disabled.
    pub sidebar: Option<SidebarReservation>,
    /// The driver's sidebar on/off state.
    pub sidebar_enabled: &'a mut bool,
    /// The sidebar width (whether or not shown); drags resize it in place.
    pub sidebar_width: &'a mut u16,
    /// The attach's `[chrome]` breakpoints.
    pub chrome: ChromeBreakpoints,
    /// The sidebar's click targets for the frame on screen.
    pub sidebar_targets: &'a crate::render::chrome::sidebar::SidebarTargets,
    /// The status bar's row reservation this frame.
    pub bar: Option<crate::render::chrome::status_bar::Position>,
    /// The status-bar painter, for tab hit-tests against the last paint.
    pub status_bar: Option<&'a crate::render::chrome::status_bar::StatusBarPainter>,
    /// The in-flight chrome drag (divider, sidebar edge, or window).
    pub drag: &'a mut Option<DragGrab>,
    /// Panes opted out of client mouse handling (`set-pane mouse off`).
    pub mouse_optout: &'a mut std::collections::HashSet<ResourceId>,
    /// Client-local attention excursion: the first `next-attention` saves an
    /// origin that `return-from-attention` consumes.
    pub attention_navigation: &'a mut AttentionNavigation,
    /// Enabled plugins' manifest `[[actions]]`, listed in the palette.
    pub plugin_actions: &'a [PluginActionEntry],
    /// Origin of the synchronous action currently being dispatched.
    pub plugin_action_source: phux_plugin::ActionSource,
    /// Last loaded key-space identity for the attached arrangement.
    pub plugin_action_placement: Option<&'a crate::attach::arrangement_link::ActionPlacement>,
    /// Enabled plugins' hostable `[[panes]]`.
    pub plugin_panes: &'a [PluginPaneEntry],
    /// The background-job facility (ADR-0172).
    pub jobs: &'a Jobs,
    /// Out-channel for a requested config reload.
    pub reload_request: &'a mut bool,
    /// ADR-0140: out-channel for a dispatched `switch-host`, as
    /// `(host, session)`; the driver owns the terminal it hands over.
    pub host_switch_request: &'a mut Option<(String, String)>,
    /// ADR-0168: out-channel for a dispatched `toggle-project-group`.
    pub project_toggle_request: &'a mut Option<String>,
    /// Out-channel for a dispatched `agents-filter`.
    pub agents_filter_request: &'a mut Option<crate::render::chrome::sidebar::FilterRequest>,
    /// ADR-0040: the driver's decoded `phux.agent/v1` records.
    pub agent_meta: &'a HashMap<ResourceId, phux_client::agent_meta::AgentRecord>,
    /// Pane cwd index and branch memo, for the fleet rows.
    pub vcs: &'a mut crate::attach::pane_state::VcsIndex,
}

/// An in-flight pointer drag over client chrome. While one is live no
/// pointer event reaches a pane (ADR-0048).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::attach) enum DragGrab {
    /// A pane divider: motion re-tunes the controlling split's ratio.
    Divider(DividerGrab),
    /// The sidebar's separator rule: motion resizes the strip for the attach
    /// (never written to `config.toml`).
    SidebarEdge,
    /// A window tab or sidebar window row: the release reorders the window
    /// to [`WindowGrab::drop_at`].
    Window(WindowGrab),
    /// A pane picked up by its title: released on another pane, it becomes
    /// the last tab of that pane's slot (`docs/organization.md#slot-tabs`).
    Pane(phux_protocol::ResourceId),
}

impl DispatchCtx<'_> {
    /// Whether `id` is a non-Terminal leaf: a placeholder that takes no
    /// input, resize, signal, or kill.
    pub(super) fn is_surface_leaf(&self, id: &phux_protocol::ResourceId) -> bool {
        crate::attach::surface_leaf::SurfaceLeaves::new(self.engine_kernel, self.leaves)
            .contains(id)
    }

    /// Take the next client request id, advancing the driver's counter.
    pub(super) const fn take_request_id(&mut self) -> u32 {
        let request_id = *self.next_request_id;
        *self.next_request_id = request_id.wrapping_add(1);
        request_id
    }
}

impl DragGrab {
    /// Insertion index on the status-bar tab strip, when this grab is a
    /// live tab drag over a tab.
    #[must_use]
    pub(in crate::attach) const fn tab_drop_at(&self) -> Option<usize> {
        match self {
            Self::Window(grab) if matches!(grab.strip, WindowStrip::Tabs) => grab.drop_at,
            _ => None,
        }
    }

    /// Insertion index on the sidebar window rows, when this grab is a
    /// live sidebar-row drag over a window row.
    #[must_use]
    pub(in crate::attach) const fn sidebar_drop_at(&self) -> Option<usize> {
        match self {
            Self::Window(grab) if matches!(grab.strip, WindowStrip::Sidebar) => grab.drop_at,
            _ => None,
        }
    }
}

/// An active divider drag (ADR-0048), keyed by split identity so a fast drag
/// still re-tunes the right split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::attach) struct DividerGrab {
    /// Path to the grabbed [`crate::layout::LayoutNode::Split`].
    pub node_path: crate::layout::NodePath,
    /// The grabbed split's axis (drives x vs y of the pointer).
    pub axis: SplitDir,
}

/// A window picked up from one of the two window strips.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::attach) struct WindowGrab {
    /// The grabbed window's durable layout id, re-resolved at the drop so a
    /// mid-drag change cannot redirect the move.
    pub window: [u8; 16],
    /// The strip it was picked up from; the drop resolves against it.
    pub strip: WindowStrip,
    /// Slot under the pointer on [`Self::strip`]; `None` off the strip.
    pub drop_at: Option<usize>,
}

/// The two chrome surfaces that list windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::attach) enum WindowStrip {
    /// The status bar's window tabs.
    Tabs,
    /// The sidebar's window rows.
    Sidebar,
}
