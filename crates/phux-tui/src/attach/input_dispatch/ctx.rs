//! The mutable dispatch context (`DispatchCtx`) and the in-flight
//! chrome-drag state (`DragGrab`: dividers, the sidebar edge, window tabs).

use std::collections::{HashMap, HashSet};

use phux_protocol::ResourceId;

use crate::attach::actions::{PendingSplit, PendingWindow};
use crate::attach::focus::FocusHistory;
use crate::attach::paint::SidebarReservation;
use crate::attach::pane_state::AttentionNavigation;
use crate::attach::plugin_actions::{PluginActionEntry, PluginRunResult};
use crate::attach::plugin_panes::PluginPaneEntry;
use crate::attach::sidebar_zones::PeerInputs;
use crate::layout::{SplitDir, Workspace};
use crate::render::overlay::OverlayState;
use crate::render::{ChromeBreakpoints, Theme};

use super::effects::ReattachTarget;

/// Driver state the dispatch path reads and mutates, lent per batch.
pub(in crate::attach) struct DispatchCtx<'a> {
    /// Dial used to open a dedicated request/response connection for actions
    /// that cannot safely consume interleaved frames from the attach stream.
    pub control_dial: Option<&'a crate::attach::Dial>,
    /// Connection-owned engine replicas used for terminal queries and local scrolling.
    pub engine_kernel: &'a mut crate::attach::pane_state::AttachKernel,
    /// Keybind resolver state. `None` when the on-disk config failed
    /// to parse; the dispatcher then forwards every key to the focused
    /// pane unchanged.
    pub resolver: Option<&'a mut phux_config::keybind::Resolver>,
    /// Client-local focus transition/MRU bookkeeping.
    pub focus_history: FocusHistory,
    /// Client-side multi-window mirror. Pane actions operate on the
    /// active window ([`Workspace::active_window_mut`]); the whole
    /// workspace is what gets serialized to L3 on a `SET_METADATA`.
    pub workspace: &'a mut Workspace,
    /// A correlated initial layout GET confirmed valid metadata or absence.
    pub layout_read_complete: bool,
    /// Outer-viewport `(cols, rows)`. Used by `apply_resize` to convert
    /// `amount` (cells) to a ratio delta.
    pub viewport: (u16, u16),
    /// Host cell size in pixels, derived like the server's (`pixel / cells`,
    /// 8x16 fallback, never zero); `INPUT_MOUSE` is scaled by it at the send
    /// boundary (SPEC input.md §3.1).
    pub cell_px: (u16, u16),
    /// Monotonic source of request ids.
    pub next_request_id: &'a mut u32,
    /// ADR-0053 replay journal on a remote reconnect lane; routes pastes
    /// through `APPLY_INPUT`. `None` on UDS.
    pub input_replay:
        Option<&'a std::cell::RefCell<crate::attach::input_replay::InputReplayJournal>>,
    /// Whether the server supports `SPAWN_RESOURCE.initial_size`.
    pub spawn_initial_size_supported: bool,
    /// Parked split actions awaiting their
    /// `RESOURCE_SPAWNED` reply. `run_action` inserts;
    /// `handle_server_frame` removes.
    pub pending_splits: &'a mut HashMap<u32, PendingSplit>,
    /// Parked `new-window` actions awaiting their
    /// `RESOURCE_SPAWNED` reply. Same lifecycle as `pending_splits`,
    /// keyed in the same request-id space.
    pub pending_windows: &'a mut HashMap<u32, PendingWindow>,
    /// What `go-to-directory` can list on this server.
    pub directory_support: crate::attach::directory_picker::DirectorySupport,
    /// The `LIST_DIRECTORY` the directory picker is waiting on, with the
    /// host it reads. Newest request wins: a reply carrying any other id is
    /// stale (the user already navigated on) and is dropped.
    pub pending_directory: &'a mut Option<crate::attach::directory_picker::PendingDirectory>,
    /// Terminals whose close this client requested (no exit notice).
    pub expected_closes: &'a mut HashSet<ResourceId>,
    /// `request_id` -> Terminal for sent kills; a `TerminalNotFound` refusal
    /// folds the dead leaf out.
    pub pending_kills: &'a mut HashMap<u32, ResourceId>,
    /// Overlay stack. When non-empty the dispatcher routes
    /// key events to the active overlay (no resolver, no predict, no
    /// pane forwarding) and discovery actions push onto it.
    pub overlays: &'a mut OverlayState,
    /// Snapshot of the on-disk keybindings, captured at driver start.
    /// The action finder uses it to show each live chord. `None` when
    /// config load failed (rows then show as unbound).
    pub keybindings: Option<&'a phux_config::KeybindingsCfg>,
    /// Chrome and overlay theme.
    pub theme: &'a Theme,
    /// The peer-wide view: the server's session graph (and which session is
    /// ours), the federation host inventory, and the peer layouts, agent
    /// records, and asks the pickers and the fleet list.
    pub peers: PeerInputs<'a>,
    /// Set when an action wants a fresher host inventory.
    pub host_refresh_request: &'a mut bool,
    /// The attached session's name; changed only by a confirmed rename.
    pub session_name: &'a mut String,
    /// In-flight `rename-session` confirmation, parked by
    /// [`apply_action_effects`] until the driver consumes the `GET_STATE`
    /// barrier. `None` when no rename is outstanding.
    pub rename_pending: &'a mut Option<super::effects::PendingSessionRename>,
    /// A rename the shared policy refused before any write. The driver
    /// surfaces it on the status bar after this batch. `None` when the
    /// batch did not refuse a rename.
    pub rename_notice: &'a mut Option<String>,
    /// Out-channel for a committed re-attach.
    pub switch_request: &'a mut Option<ReattachTarget>,
    /// A `DETACH` is in flight; set by the first detach so a second is not
    /// re-sent.
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
    /// Driver-owned, client-local attention excursion state.
    /// The first `next-attention` saves an origin; later cycles preserve it,
    /// and `return-from-attention` consumes it. Never serialized or shared.
    pub attention_navigation: &'a mut AttentionNavigation,
    /// Enabled plugins' manifest `[[actions]]`, snapshotted at
    /// driver start (same lifecycle as `keybindings`). The command palette
    /// appends one namespaced row per entry under a "Plugin" header.
    pub plugin_actions: &'a [PluginActionEntry],
    /// Enabled plugins' hostable `[[panes]]`.
    pub plugin_panes: &'a [PluginPaneEntry],
    /// Plugin-run completion channel; `None` in tests.
    pub plugin_tx: Option<&'a tokio::sync::mpsc::UnboundedSender<PluginRunResult>>,
    /// Out-channel for a requested config reload (dispatch borrows exactly
    /// the state a reload replaces).
    pub reload_request: &'a mut bool,
    /// ADR-0140: out-channel for a dispatched `switch-host`, as
    /// `(host, session)`. The driver reads it after the batch, detaches, and
    /// hands the terminal to `phux attach` on that machine; dispatch cannot
    /// do it because the terminal it would restore belongs to the driver.
    pub host_switch_request: &'a mut Option<(String, String)>,
    /// phux-foz.7 / ADR-0040: the driver's decoded `phux.agent/v1` records
    /// (`AgentMetaIndex::records`), kept live by the per-pane metadata
    /// subscriptions. The `agent-fleet` action projects them into the
    /// dashboard rows.
    pub agent_meta: &'a HashMap<ResourceId, phux_client::agent_meta::AgentRecord>,
    /// The driver's pane-cwd index + memoized
    /// branch cache. The fleet rows resolve each pane's branch through it
    /// (mut only for the memo).
    pub vcs: &'a mut crate::attach::pane_state::VcsIndex,
}

/// An in-flight pointer drag over client chrome. While one is live no
/// pointer event reaches a pane (ADR-0048).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::attach) enum DragGrab {
    /// A pane divider: motion re-tunes the controlling split's ratio.
    Divider(DividerGrab),
    /// The left-docked sidebar's separator rule: motion resizes the strip. The width
    /// is runtime chrome like `toggle-sidebar`: it lasts for the attach and
    /// is never written to `config.toml` (ADR-0101 decision 2).
    SidebarEdge,
    /// A window tab or sidebar window row: motion paints an insertion
    /// marker at [`WindowGrab::drop_at`]; the release reorders the
    /// window to the slot it was dropped on.
    Window(WindowGrab),
}

impl DispatchCtx<'_> {
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
    /// The grabbed window's durable layout id. The drop re-resolves its
    /// position, so a reorder, close, or peer layout that lands mid-drag
    /// cannot redirect the move onto a different window.
    pub window: [u8; 16],
    /// Which strip it was picked up from; the drop resolves against the
    /// same strip, so a tab dropped on the sidebar is a no-op.
    pub strip: WindowStrip,
    /// Slot under the pointer on [`Self::strip`], painted as the live
    /// insertion marker. `None` when the pointer is off that strip (the
    /// drop would be a no-op).
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
