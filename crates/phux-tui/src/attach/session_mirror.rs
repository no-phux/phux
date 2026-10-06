//! The client-side mirror of the attached session that server frames fold
//! into.
//!
//! **TL;DR.** [`SessionMirror`] owns the state the frame dispatcher
//! (`server_frame::handle_server_frame`) reads and writes: the session
//! kernel, the pane slots, the layout mirror, focus and zoom, the session
//! name, the predictor, and the request-correlated bookkeeping for spawns,
//! closes, and agent records. The live loop (`SessionLoop`), the headless
//! composite, and the dispatcher's test rigs each own one, so a new piece of
//! mirrored state is one field here plus [`SessionMirror::new`], not an edit
//! to every call site (phux-jx39.3).

use std::collections::{HashMap, HashSet};

use phux_client_core::session::EffectBuffer as KernelEffectBuffer;
use phux_protocol::ids::ResourceId;

use super::actions::{PendingSplit, PendingWindow};
use super::pane_state::{AttachKernel, PaneSlot};
use super::server_frame::AgentMetaIndex;
use crate::layout::Workspace;
use crate::predict::PredictionState;

/// The attached session as this client mirrors it.
pub(in crate::attach) struct SessionMirror {
    /// The client-side libghostty session kernel the frames feed.
    pub(in crate::attach) engine_kernel: AttachKernel,
    /// Effects the kernel emits per ingested frame.
    pub(in crate::attach) kernel_effects: KernelEffectBuffer,
    /// Client-local pane slots, keyed by Terminal.
    pub(in crate::attach) panes: HashMap<ResourceId, PaneSlot>,
    /// The multi-window layout mirror.
    pub(in crate::attach) workspace: Workspace,
    /// The focused leaf, when there is one.
    pub(in crate::attach) focused_resource: Option<ResourceId>,
    /// The leaf zoomed to fill its window. Render and reflow geometry go
    /// through `Workspace::render_window(zoomed)`; a split un-zooms.
    pub(in crate::attach) zoomed: Option<ResourceId>,
    /// The attached session's name; changed only by a confirmed rename.
    pub(in crate::attach) session_name: String,
    /// ADR-0105: whether the attached session is keep-empty.
    pub(in crate::attach) keep_empty_session: bool,
    /// Local-echo predictions for the focused pane.
    pub(in crate::attach) predict: PredictionState,
    /// Parked split actions awaiting their `RESOURCE_SPAWNED` reply.
    pub(in crate::attach) pending_splits: HashMap<u32, PendingSplit>,
    /// Parked `new-window` actions awaiting their `RESOURCE_SPAWNED` reply.
    pub(in crate::attach) pending_windows: HashMap<u32, PendingWindow>,
    /// ADR-0147: floating plugin overlay spawns awaiting their reply, by
    /// request id, with the box title.
    pub(in crate::attach) pending_floating: HashMap<u32, String>,
    /// Closes this client requested; their pane-exit notice is suppressed.
    pub(in crate::attach) expected_closes: HashSet<ResourceId>,
    /// `request_id` -> Terminal for commands whose `TERMINAL_NOT_FOUND` is the
    /// only evidence the resource is gone, so a stale leaf can fold out.
    pub(in crate::attach) pending_resource_ops: HashMap<u32, ResourceId>,
    /// ADR-0040 `phux.agent/v1` index, read when composing window labels.
    pub(in crate::attach) agent_meta: AgentMetaIndex,
}

impl SessionMirror {
    /// The pane that owns paint focus: the floating overlay while one is
    /// open (ADR-0147), else the focused leaf.
    pub(in crate::attach) fn paint_focus(&self) -> Option<ResourceId> {
        super::floating::paint_focus(&self.panes, self.focused_resource.as_ref()).cloned()
    }

    /// An empty mirror around `engine_kernel`: no panes, an empty layout,
    /// nothing focused or in flight.
    pub(in crate::attach) fn new(engine_kernel: AttachKernel, predict: PredictionState) -> Self {
        Self {
            engine_kernel,
            kernel_effects: KernelEffectBuffer::new(),
            panes: HashMap::new(),
            workspace: Workspace::default(),
            focused_resource: None,
            zoomed: None,
            session_name: String::new(),
            keep_empty_session: false,
            predict,
            pending_splits: HashMap::new(),
            pending_windows: HashMap::new(),
            pending_floating: HashMap::new(),
            expected_closes: HashSet::new(),
            pending_resource_ops: HashMap::new(),
            agent_meta: AgentMetaIndex::default(),
        }
    }
}
