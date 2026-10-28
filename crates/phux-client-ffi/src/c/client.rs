//! C-facing storage over the runtime control plane.
//!
//! This module owns borrowed C views and callback state only. Protocol,
//! request, engine, and document state live in `phux-client-runtime`.

#![allow(
    clippy::redundant_pub_crate,
    reason = "this private module is the shared implementation surface for sibling FFI modules"
)]

mod events;
mod render;
mod terminal;

use std::collections::HashMap;
use std::ptr;
use std::sync::Arc;

use phux_client_core::history::{HistoryCacheConfig, HistoryLoadState, HistoryStatus};
use phux_client_core::session::agent_stream::{AgentPublication, AgentRecordMode};
use phux_client_core::session::{AgentStreamInfo, ReplicaKey};
use phux_client_runtime::control::{
    ControlOptions, Event, InboundDelivery, Status as RuntimeStatus,
};
use phux_client_runtime::engine::{
    EngineDocumentPoint, EngineError, MouseMode, Scroll, SelectionGestureEvent,
    SelectionGestureResult,
};
use phux_client_runtime::publication::GridFrame;
use phux_client_runtime::{Client as RuntimeClient, ControlGuard, Lane, Runtime};
use phux_protocol::ResourceKind;
use phux_protocol::caps::{BootstrapLimits, Layer, ServerFeature};
use phux_protocol::ids::{BootstrapId, ResourceId, StreamId};
use phux_protocol::wire::frame::{CloseReason, FrameKind};

use crate::c::draw_list::DrawCache;
use crate::c::error::BridgeError;
use crate::c::grid_metadata::GridMetadataCache;
use crate::c::types::{
    OwnedEffect, PhuxBytes, PhuxClientCallbacks, PhuxClientEffect, PhuxClientState,
    PhuxDocumentAnchor, PhuxDocumentPoint, PhuxResourceId, PhuxSearchResult, PhuxTerminalGridView,
    bytes_out, terminal_id_out,
};
use crate::projection::event::{Lifecycle, TerminalSignal};
use crate::projection::{event as projected_event, grid};

/// One session from the latest snapshot, enriched with its organization
/// record when the replica holds it. `window_count` is the legacy count the
/// frozen `PhuxSessionInfo` carries, derived from the snapshot's session
/// facets when the server reports them; `PhuxSessionRecord` reads the rest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SessionSummary {
    pub session_id: u32,
    pub name: Vec<u8>,
    pub created_at_unix_secs: i64,
    pub window_count: u16,
    pub attached_client_count: u16,
    pub focused: bool,
    /// The session survives its last member: `lifetime == Persistent`, or
    /// the legacy keep-empty mark from a server without lifetimes.
    pub keep_empty: bool,
    /// The persistent key, from the snapshot or the replica.
    pub key: Option<phux_protocol::ids::SessionKey>,
    /// [`Self::key`] in its full text form; empty without a key.
    pub key_text: Vec<u8>,
    /// Whether it outlives its last member, when the server reports it.
    pub lifetime: Option<phux_protocol::wire::frame::SessionLifetime>,
    /// Members the snapshot lists, of every kind.
    pub snapshot_members: u32,
    /// What the organization replica adds.
    pub org: crate::c::organization::SessionOrg,
}

/// One resource from the latest runtime-owned topology snapshot.
#[derive(Debug)]
pub(crate) struct ResourceSummary {
    pub id: ResourceId,
    pub kind: u32,
    pub parent: Option<ResourceId>,
    pub provider: Vec<u8>,
    pub native_id: Vec<u8>,
    pub state: Vec<u8>,
    /// The live session it belongs to, 0 when the snapshot names none.
    pub session_id: u32,
    /// Its grid size; 0 for kinds without a grid.
    pub cols: u16,
    pub rows: u16,
    /// Its persistent key from the organization replica, all zero when unknown.
    pub key: [u8; 16],
    parent_view: PhuxResourceId,
    parent_host: Vec<u8>,
}

impl ResourceSummary {
    pub(crate) fn new(
        id: ResourceId,
        kind: u32,
        parent: Option<ResourceId>,
        provider: Vec<u8>,
        native_id: Vec<u8>,
        state: Vec<u8>,
    ) -> Self {
        let parent_host = match &parent {
            Some(ResourceId::Satellite { host, .. }) => host.as_str().as_bytes().to_vec(),
            _ => Vec::new(),
        };
        let mut summary = Self {
            id,
            kind,
            parent,
            provider,
            native_id,
            state,
            session_id: 0,
            cols: 0,
            rows: 0,
            key: [0; 16],
            parent_view: PhuxResourceId::default(),
            parent_host,
        };
        summary.parent_view = summary
            .parent
            .as_ref()
            .map_or_else(PhuxResourceId::default, |parent| {
                resource_id_with_host(parent, &summary.parent_host)
            });
        summary
    }

    pub(crate) const fn parent_ptr(&self) -> *const PhuxResourceId {
        if self.parent.is_some() {
            ptr::from_ref(&self.parent_view)
        } else {
            ptr::null()
        }
    }
}

enum EventProjection {
    Handled(bool),
    Next(Event),
}

/// Retained C view and the immutable runtime frame that backs it.
pub(crate) struct RenderCache {
    pub frame: Arc<GridFrame>,
    pub _terminal_host: Vec<u8>,
    pub view: PhuxTerminalGridView,
    pub metadata: GridMetadataCache,
    /// The draw list last lent for this frame, if one was asked for.
    pub draw: Option<DrawCache>,
}

#[cfg(test)]
thread_local! {
    pub(crate) static RENDER_CACHE_BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(crate) static EFFECT_VIEW_BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    pub bootstrap_chunk: u32,
    pub history_page: u32,
    pub history_page_rows: u32,
    pub history_cache_bytes: usize,
    pub history_materialized_rows: usize,
    pub history_prefetch_rows: usize,
}

#[allow(clippy::struct_excessive_bools)]
pub(crate) struct Client {
    /// The runtime session this bridge projects. The C ABI's historical
    /// lane is `Lane::Embedded`: Cockpit owns the socket and pumps frames.
    /// `phux_client_connect` builds a `Lane::Connected` one instead, where
    /// the runtime's driver dials, reconnects, and pumps.
    pub runtime: RuntimeClient,
    /// Borrow-retained copy of `ControlPlane::take_outbound()`.
    pub outgoing: Vec<Vec<u8>>,
    pub owned_effects: Vec<OwnedEffect>,
    /// Receipts for staged effects; explicit effect clearing accepts them.
    /// Transport retirement drops views without accepting the publications.
    agent_receipts: Vec<Arc<AgentPublication>>,
    pub effect_count: usize,
    pub render: HashMap<ResourceId, RenderCache>,
    pub selection_buf: Vec<u8>,
    pub perf_buf: Vec<u8>,
    pub created_at: std::time::Instant,
    pub search_results: Vec<PhuxSearchResult>,
    pub sessions: Vec<SessionSummary>,
    pub resources: Vec<ResourceSummary>,
    pub agent_streams: HashMap<ResourceId, Option<AgentStreamInfo>>,
    pub operations: crate::c::operations::Operations,
    pub arrangement: crate::c::arrangement::SharedArrangement,
    pub org: crate::c::organization::OrgState,
    pub server_id: Vec<u8>,
    pub last_error: Vec<u8>,
    /// Borrow-retained copy of the runtime's connection failure.
    pub connection_error: Vec<u8>,
    pub limits: Limits,
    pub callbacks: PhuxClientCallbacks,
    pub in_callback: bool,
    pub attached_notified: bool,
    pub protocol_ready: bool,
    pub attached: bool,
    pub attach_queued: bool,
    pub expected_attach_id: Option<u32>,
    pub list_directory: bool,
    pub list_directory_host: bool,
    pub keep_empty_sessions: bool,
    pub conditional_kill: bool,
    pub event_journal: bool,
    pub retain_on_exit: bool,
    pub spawn_idempotency: bool,
    pub close_tab_resources: bool,
    pub l3_metadata: bool,
    pub attach_roles: bool,
    pub attach_role: Option<phux_protocol::wire::frame::RolePolicy>,
    pub event_after_seq: Option<u64>,
    pub directory: crate::c::directory::DirectoryState,
    /// Borrow-retained projected `PATH_RESULTS` batch for the C ABI.
    pub path_answers: Vec<crate::projection::outcome::PathResult>,
    pub session_query: crate::c::session_query::SessionQuery,
    pub session_creates: crate::c::session_create::SessionCreates,
    pub session_rename: crate::c::session_rename::SessionRename,
    pub projection: crate::c::projection::Projection,
    pub detached: bool,
}

impl Client {
    /// The plane options and history policy both lanes are built with.
    ///
    /// `deliver_inbound` is [`InboundDelivery::Queued`] in either lane: this
    /// ABI's per-frame hooks (retired-close suppression, bootstrap-profile
    /// validation) read arrangement-subscription state that only the owning
    /// thread may touch, so the decode point
    /// stays here even when the runtime owns the socket.
    pub(crate) fn control_options(limits: &Limits) -> (ControlOptions, HistoryCacheConfig) {
        let bootstrap_limits =
            BootstrapLimits::new(limits.bootstrap_chunk, limits.history_page).unwrap_or_default();
        let history = HistoryCacheConfig {
            max_bytes: limits.history_cache_bytes,
            max_materialized_rows: limits.history_materialized_rows,
            prefetch_rows: limits.history_prefetch_rows,
            request_max_bytes: limits.history_page,
            request_max_rows: limits.history_page_rows,
        };
        let options = ControlOptions {
            client_name: String::new(),
            automatic_lifecycle: false,
            session_recovery: phux_client_runtime::control::SessionRecovery::Automatic,
            viewport: (80, 24),
            scrollback_lines: u32::try_from(limits.history_materialized_rows).unwrap_or(u32::MAX),
            attach: None,
            attach_role: None,
            event_after_seq: None,
            // The ABI arrangement subscription owns foreign-pane admission;
            // enabling the generic runtime path as well would issue a second
            // ATTACH_RESOURCE for the same spawn.
            auto_attach_foreign_spawns: false,
            // Arrangement refresh subscribes AgentSession streams itself
            // (`crate::c::arrangement::resources`).
            subscribe_agent_sessions: false,
            bootstrap_limits,
            // The draw-list ABI exposes text, not image planes.
            image_protocols: phux_protocol::caps::ImageProtocolSet::new(),
            deliver_inbound: InboundDelivery::Queued,
            prediction_style: phux_client_runtime::control::PredictionStyle::Underline,
        };
        (options, history)
    }

    pub(crate) fn new(limits: Limits) -> Self {
        let (options, history) = Self::control_options(&limits);
        Self::from_runtime(limits, Runtime::embedded(options), history)
    }

    pub(crate) fn from_runtime(
        limits: Limits,
        runtime: RuntimeClient,
        history: HistoryCacheConfig,
    ) -> Self {
        runtime.control().set_history_config(history);
        Self {
            runtime,
            outgoing: Vec::new(),
            owned_effects: Vec::new(),
            agent_receipts: Vec::new(),
            effect_count: 0,
            render: HashMap::new(),
            selection_buf: Vec::new(),
            perf_buf: Vec::new(),
            created_at: std::time::Instant::now(),
            search_results: Vec::new(),
            sessions: Vec::new(),
            resources: Vec::new(),
            agent_streams: HashMap::new(),
            operations: crate::c::operations::Operations::default(),
            arrangement: crate::c::arrangement::SharedArrangement::default(),
            org: crate::c::organization::OrgState::default(),
            server_id: Vec::new(),
            last_error: Vec::new(),
            connection_error: Vec::new(),
            limits,
            callbacks: PhuxClientCallbacks::default(),
            in_callback: false,
            attached_notified: false,
            protocol_ready: false,
            attached: false,
            attach_queued: false,
            expected_attach_id: None,
            list_directory: false,
            list_directory_host: false,
            keep_empty_sessions: false,
            conditional_kill: false,
            event_journal: false,
            retain_on_exit: false,
            spawn_idempotency: false,
            close_tab_resources: false,
            l3_metadata: false,
            attach_roles: false,
            attach_role: None,
            event_after_seq: None,
            directory: crate::c::directory::DirectoryState::default(),
            path_answers: Vec::new(),
            session_query: crate::c::session_query::SessionQuery::default(),
            session_creates: crate::c::session_create::SessionCreates::default(),
            session_rename: crate::c::session_rename::SessionRename::default(),
            projection: crate::c::projection::Projection::default(),
            detached: false,
        }
    }

    pub(crate) fn next_attach_role(&mut self) -> Option<phux_protocol::wire::frame::RolePolicy> {
        let role = self.attach_role;
        if role.is_some_and(phux_protocol::wire::frame::RolePolicy::takes_over) {
            self.attach_role = None;
        }
        role
    }

    pub(crate) fn is_agent_stream(&self, id: &ResourceId) -> bool {
        self.agent_streams.contains_key(id)
    }

    pub(crate) fn reset_borrows(&mut self) {
        for cache in self.render.values_mut() {
            cache.metadata.valid = false;
        }
        self.selection_buf.clear();
        self.search_results.clear();
    }

    fn engine(&self) -> Result<phux_client_runtime::engine::EngineHandle, BridgeError> {
        self.control()
            .engine()
            .cloned()
            .ok_or_else(|| BridgeError::state("terminal engine is not negotiated"))
    }

    /// Borrow the runtime's control plane. Releasing the guard notifies a
    /// connected lane's driver about anything the borrow queued.
    pub(crate) fn control(&self) -> ControlGuard<'_> {
        self.runtime.control()
    }
}

fn engine_bridge(error: EngineError) -> BridgeError {
    match error {
        EngineError::Engine(message) | EngineError::AnchorUnavailable(message) => {
            BridgeError::state(message)
        }
        EngineError::ProjectionUnavailable => BridgeError::state("no projection for this terminal"),
        EngineError::Stopped => BridgeError::engine("the engine owner thread stopped"),
        EngineError::Spawn(error) => BridgeError::engine(error.to_string()),
    }
}

pub(crate) fn resource_summary(
    snapshot: &phux_protocol::wire::info::SessionSnapshot,
    resource: &phux_protocol::wire::info::ResourceInfo,
) -> ResourceSummary {
    let agent = resource.agent.as_ref();
    let mut summary = ResourceSummary::new(
        resource.id.clone(),
        u32::from(resource.kind.as_wire()),
        resource.parent.clone(),
        agent.map_or_else(Vec::new, |facet| facet.provider.as_bytes().to_vec()),
        agent
            .and_then(|facet| facet.native_id.as_ref())
            .map_or_else(Vec::new, |value| value.as_bytes().to_vec()),
        agent.map_or_else(Vec::new, |facet| facet.state.as_bytes().to_vec()),
    );
    summary.session_id = resource_session(snapshot, resource);
    summary.cols = resource.cols;
    summary.rows = resource.rows;
    summary
}

/// The live session a snapshot resource belongs to: its `session` facet,
/// or from a server that predates it, the session of its legacy window; 0
/// when neither names one.
pub(crate) fn resource_session(
    snapshot: &phux_protocol::wire::info::SessionSnapshot,
    resource: &phux_protocol::wire::info::ResourceInfo,
) -> u32 {
    resource
        .session
        .or_else(|| {
            snapshot
                .windows
                .iter()
                .find(|window| window.id == resource.window_id)
                .map(|window| window.session_id)
        })
        .map_or(0, phux_protocol::SessionId::get)
}

fn resource_id_with_host(id: &ResourceId, host: &[u8]) -> PhuxResourceId {
    match id {
        ResourceId::Local { id } => PhuxResourceId {
            kind: 0,
            id: *id,
            host: PhuxBytes::default(),
        },
        ResourceId::Satellite { id, .. } => PhuxResourceId {
            kind: 1,
            id: *id,
            host: bytes_out(host),
        },
    }
}

fn encode_optional_i32(presence: &mut u64, payload: &mut u64, value: Option<i32>) {
    if let Some(value) = value {
        *presence = 1;
        *payload = u64::from(value.cast_unsigned());
    }
}

const fn history_state_code(state: HistoryLoadState) -> u32 {
    match state {
        HistoryLoadState::Idle => 0,
        HistoryLoadState::Loading => 1,
        HistoryLoadState::Complete => 2,
        HistoryLoadState::Gap => 3,
        HistoryLoadState::Stale => 4,
        HistoryLoadState::Pruned => 5,
        HistoryLoadState::Tombstoned => 6,
        HistoryLoadState::Cleared => 7,
    }
}

const fn history_unavailable_code(
    reason: phux_client_core::session::HistoryUnavailableReason,
) -> u32 {
    use phux_client_core::session::HistoryUnavailableReason as Reason;
    match reason {
        Reason::Stale => 0,
        Reason::Pruned => 1,
        Reason::Reset => 2,
        Reason::Resize => 3,
        Reason::Expired => 4,
        Reason::Released => 5,
        Reason::Limit => 6,
        Reason::CodecFailure => 7,
    }
}

#[cfg(test)]
mod search_error_tests;
