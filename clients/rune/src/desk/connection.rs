//! One runtime client's request namespace, view leases and workspace.
//!
//! Replacing this state must not reload preferences or reconstruct platform
//! services. A transport reconnect on the same client does not replace it:
//! runtime receipts remain authoritative across those reconnects.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Weak};
use std::time::Instant;

use phux_client_core::layout::LayoutNode;
use phux_client_runtime::control::{PrivateUploadDestination, Status, Topology};
use phux_client_runtime::{Client, ViewId};
use phux_protocol::ids::{ArrangementKey, OperationId, ResourceId, SessionKey};

use super::{
    Fitted, Painted, Placement, Shown, SplitPath, copy_mode, events, find, mouse, palette, popup,
    quick, reorder, resources, screenshot, splits, tabs,
};

pub struct Connection {
    /// Monotonic within this window, even if an embedded client is reused.
    serial: u64,
    pub client: Option<Arc<Client>>,
    /// Why there is no client, or the last failure the user should see.
    pub error: Option<String>,
    pub status: Status,
    pub topology: Option<Topology>,
    pub server: Option<String>,
    /// Transport family of this desktop connection, never an endpoint.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) action_transport: Option<&'static str>,
    pub shown: Option<Shown>,
    pub resources: resources::Resources,
    /// The resource requested by the user, and when its opening began.
    pub wanted: Option<(ResourceId, Instant)>,
    /// A session created here: show its terminal once selected.
    pub(super) pending_session: Option<String>,
    /// The connection incarnation the shown view belongs to.
    pub(super) epoch: u64,
    /// A path search on the focused pane's host.
    pub paths: Option<palette::PathSearch>,
    /// Grid origins in window points, from the last layout.
    pub(crate) origins: HashMap<ResourceId, [f32; 2]>,
    /// Find in the focused pane's leased history.
    pub find: Option<find::Find>,
    pub find_query: rune::TextEdit,
    pub copy_mode: Option<copy_mode::CopyMode>,
    pub changes: crate::features::changes::Changes,
    pub popup: popup::Popup,
    pub tabs: tabs::Tabs,
    pub reorder: reorder::Reorder,
    /// A screenshot being taken for a terminal on this client. Replacing the
    /// connection drops it, which cancels the picker and reaps its worker.
    pub(super) capture: Option<screenshot::Capture>,
    /// Image uploads on their way, and each upload's destination: a private
    /// image also carries the execution it was captured for.
    pub(super) uploads: Vec<(u64, ResourceId, Option<PrivateUploadDestination>)>,
    /// User pastes whose delivery has not settled.
    pub(super) pastes: HashSet<u64>,
    /// The input method's composition at the terminal cursor.
    pub preedit: Option<String>,
    pub(super) pending_spawns: Vec<u32>,
    pub(super) receipts: events::Receipts,
    /// Grid areas in window points, from the last layout.
    pub(super) areas: HashMap<ResourceId, [f32; 2]>,
    /// The shown group's slot tabs, as this window shows them.
    pub slots: Vec<phux_client_core::layout::Slot>,
    /// The tab this window chose in each slot: which tab shows is its own.
    pub(super) slot_choice: Vec<ResourceId>,
    /// The last proposed cells and device-pixel cell size per terminal.
    pub(super) fitted: HashMap<ResourceId, Fitted>,
    /// The active session's split tree, or the focused pane alone.
    pub layout: Option<LayoutNode>,
    /// Retained views of panes besides the focused one.
    pub others: HashMap<ResourceId, Option<ViewId>>,
    pub zoomed: Option<ResourceId>,
    /// Divider drag preview, committed only on release.
    pub(super) dragging: Option<(SplitPath, f32)>,
    /// Spawns waiting for their original session's arrangement.
    pub(super) unplaced_splits: Vec<splits::Unplaced>,
    pub(super) layout_ops: HashSet<OperationId>,
    /// Requested spawns and their captured destination/placement.
    pub(super) pending_splits: HashMap<u32, (Option<SessionKey>, Placement)>,
    pub(super) wheel_pane: Option<ResourceId>,
    pub(super) mouse_capture: Option<(ResourceId, mouse::Press)>,
    pub(super) mouse_cell: Option<(u16, u16)>,
    pub(super) split_sizes: HashMap<SplitPath, [f32; 2]>,
    pub(super) fetched: HashSet<ArrangementKey>,
    pub(super) gesture: u64,
    pub(super) wheel_rows: f32,
    pub(super) sessions_seq: u32,
    /// The exact fenced frame awaiting Rune's drawn receipt.
    pub painted: RefCell<Option<Painted>>,
    /// The terminal last told it has the window's focus.
    pub(super) focus_told: Option<ResourceId>,
    /// One arrangement write at a time, each based on its predecessor.
    pub(super) split_write: Option<splits::SplitWrite>,
    /// A quick-terminal move awaiting the server's receipt.
    pub(super) quick_move: Option<quick::Transfer>,
}

impl Default for Connection {
    fn default() -> Self {
        Self {
            client: None,
            serial: 0,
            error: None,
            status: Status::Idle,
            topology: None,
            server: None,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            action_transport: None,
            shown: None,
            resources: resources::Resources::default(),
            wanted: None,
            pending_session: None,
            epoch: 0,
            paths: None,
            origins: HashMap::new(),
            find: None,
            find_query: rune::TextEdit::new(""),
            copy_mode: None,
            changes: crate::features::changes::Changes::default(),
            popup: popup::Popup::default(),
            tabs: tabs::Tabs::default(),
            reorder: reorder::Reorder::default(),
            capture: None,
            uploads: Vec::new(),
            pastes: HashSet::new(),
            preedit: None,
            pending_spawns: Vec::new(),
            receipts: events::Receipts::default(),
            areas: HashMap::new(),
            slots: Vec::new(),
            slot_choice: Vec::new(),
            fitted: HashMap::new(),
            layout: None,
            others: HashMap::new(),
            zoomed: None,
            dragging: None,
            unplaced_splits: Vec::new(),
            layout_ops: HashSet::new(),
            pending_splits: HashMap::new(),
            wheel_pane: None,
            mouse_capture: None,
            mouse_cell: None,
            split_sizes: HashMap::new(),
            fetched: HashSet::new(),
            gesture: 0,
            wheel_rows: 0.0,
            sessions_seq: 0,
            painted: RefCell::new(None),
            focus_told: None,
            split_write: None,
            quick_move: None,
        }
    }
}

/// A queued platform callback may only address its originating runtime
/// namespace and transport incarnation, even when numeric handles repeat.
pub(super) struct CallbackFence {
    client: Weak<Client>,
    serial: u64,
    epoch: u64,
}

impl Connection {
    pub(super) fn namespace(&self) -> u64 {
        self.serial
    }

    pub(super) fn successor(&self) -> Self {
        Self {
            serial: self.serial + 1,
            ..Self::default()
        }
    }

    pub(super) fn callback_fence(&self) -> Option<CallbackFence> {
        let client = self.client.as_ref()?;
        Some(CallbackFence {
            client: Arc::downgrade(client),
            serial: self.serial,
            epoch: client.connection_epoch(),
        })
    }
}

impl CallbackFence {
    pub(super) fn is_current(&self, connection: &Connection) -> bool {
        connection.serial == self.serial
            && connection.client.as_ref().is_some_and(|client| {
                Weak::ptr_eq(&self.client, &Arc::downgrade(client))
                    && client.connection_epoch() == self.epoch
            })
    }
}
