//! C-facing storage over the runtime control plane.
//!
//! This module owns borrowed C views and callback state only. Protocol,
//! request, engine, and document state live in `phux-client-runtime`.

#![allow(
    clippy::redundant_pub_crate,
    reason = "this private module is the shared implementation surface for sibling FFI modules"
)]

use std::collections::HashMap;
use std::ptr;
use std::sync::Arc;

use phux_client_core::history::{HistoryCacheConfig, HistoryLoadState, HistoryStatus};
use phux_client_core::session::ReplicaKey;
use phux_client_runtime::control::{ControlOptions, ControlPlane, Event, Status as RuntimeStatus};
use phux_client_runtime::engine::{
    EngineDocumentPoint, EngineError, MouseMode, Scroll, SelectionGestureEvent,
    SelectionGestureResult,
};
use phux_client_runtime::publication::{GridFrame, Rgb};
use phux_protocol::ResourceKind;
use phux_protocol::caps::{BootstrapLimits, Layer, ServerFeature};
use phux_protocol::ids::{BootstrapId, ResourceId, StreamId};
use phux_protocol::wire::frame::{CloseReason, FrameKind};

use crate::error::BridgeError;
use crate::grid_metadata::GridMetadataCache;
use crate::types::{
    OwnedEffect, PhuxBytes, PhuxClientCallbacks, PhuxClientEffect, PhuxClientState,
    PhuxDocumentAnchor, PhuxDocumentPoint, PhuxResourceId, PhuxSearchResult, PhuxTerminalGridView,
    bytes_out, terminal_id_out,
};

#[derive(Debug, Clone)]
pub(crate) struct SessionSummary {
    pub session_id: u32,
    pub name: Vec<u8>,
    pub created_at_unix_secs: i64,
    pub window_count: u16,
    pub attached_client_count: u16,
    pub focused: bool,
    pub keep_empty: bool,
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

pub(crate) struct AgentStream {
    pub generation: Option<(u64, u64)>,
    pub retained_pending: bool,
}

/// Retained C view and the immutable runtime frame that backs it.
pub(crate) struct RenderCache {
    pub frame: Arc<GridFrame>,
    pub _terminal_host: Vec<u8>,
    pub view: PhuxTerminalGridView,
    pub metadata: GridMetadataCache,
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
    pub control: ControlPlane,
    /// Borrow-retained copy of `ControlPlane::take_outbound()`.
    pub outgoing: Vec<Vec<u8>>,
    pub owned_effects: Vec<OwnedEffect>,
    pub effect_count: usize,
    pub render: HashMap<ResourceId, RenderCache>,
    pub selection_buf: Vec<u8>,
    pub perf_buf: Vec<u8>,
    pub created_at: std::time::Instant,
    pub search_results: Vec<PhuxSearchResult>,
    pub sessions: Vec<SessionSummary>,
    pub resources: Vec<ResourceSummary>,
    pub agent_streams: HashMap<ResourceId, AgentStream>,
    pub operations: crate::operations::Operations,
    pub workspace: crate::workspace::SharedWorkspace,
    pub server_id: Vec<u8>,
    pub last_error: Vec<u8>,
    pub limits: Limits,
    pub callbacks: PhuxClientCallbacks,
    pub in_callback: bool,
    pub attached_notified: bool,
    pub hello_queued: bool,
    pub protocol_ready: bool,
    pub attached: bool,
    pub attach_queued: bool,
    pub expected_attach_id: Option<u32>,
    #[cfg(test)]
    pub selected_profile: Option<phux_protocol::BootstrapProfile>,
    #[cfg(test)]
    pub offered_caps: Option<phux_protocol::ClientCapabilities>,
    pub terminal_reply: bool,
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
    pub directory: crate::directory::DirectoryState,
    pub session_query: crate::session_query::SessionQuery,
    pub session_creates: crate::session_create::SessionCreates,
    pub session_rename: crate::session_rename::SessionRename,
    pub projection: crate::projection::Projection,
    pub detached: bool,
}

impl Client {
    pub(crate) fn new(limits: Limits) -> Self {
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
            viewport: (80, 24),
            scrollback_lines: u32::try_from(limits.history_materialized_rows).unwrap_or(u32::MAX),
            attach: None,
            attach_role: None,
            event_after_seq: None,
            auto_attach_foreign_spawns: true,
            bootstrap_limits,
        };
        Self {
            control: ControlPlane::new(options).with_history_config(history),
            outgoing: Vec::new(),
            owned_effects: Vec::new(),
            effect_count: 0,
            render: HashMap::new(),
            selection_buf: Vec::new(),
            perf_buf: Vec::new(),
            created_at: std::time::Instant::now(),
            search_results: Vec::new(),
            sessions: Vec::new(),
            resources: Vec::new(),
            agent_streams: HashMap::new(),
            operations: crate::operations::Operations::default(),
            workspace: crate::workspace::SharedWorkspace::default(),
            server_id: Vec::new(),
            last_error: Vec::new(),
            limits,
            callbacks: PhuxClientCallbacks::default(),
            in_callback: false,
            attached_notified: false,
            hello_queued: false,
            protocol_ready: false,
            attached: false,
            attach_queued: false,
            expected_attach_id: None,
            #[cfg(test)]
            selected_profile: None,
            #[cfg(test)]
            offered_caps: None,
            terminal_reply: false,
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
            directory: crate::directory::DirectoryState::default(),
            session_query: crate::session_query::SessionQuery::default(),
            session_creates: crate::session_create::SessionCreates::default(),
            session_rename: crate::session_rename::SessionRename::default(),
            projection: crate::projection::Projection::default(),
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

    #[cfg(test)]
    pub(crate) fn install_profile(
        &mut self,
        profile: phux_protocol::BootstrapProfile,
        limits: BootstrapLimits,
    ) {
        self.limits.bootstrap_chunk = limits.max_chunk_bytes();
        self.limits.history_page = limits.max_history_page_bytes();
        if self.control.status() == phux_client_runtime::control::Status::Idle {
            let _ = self.control.open_explicit("ffi-test".to_owned());
            let _ = self.control.take_outbound();
        }
        let mut features = Vec::new();
        for (enabled, feature) in [
            (self.terminal_reply, ServerFeature::TerminalReply),
            (self.list_directory, ServerFeature::ListDirectory),
            (self.list_directory_host, ServerFeature::ListDirectoryHost),
            (self.keep_empty_sessions, ServerFeature::KeepEmptySessions),
            (self.conditional_kill, ServerFeature::ConditionalKill),
            (self.event_journal, ServerFeature::EventJournal),
            (self.retain_on_exit, ServerFeature::RetainOnExit),
            (self.spawn_idempotency, ServerFeature::SpawnIdempotency),
            (self.close_tab_resources, ServerFeature::CloseTabResources),
            (self.attach_roles, ServerFeature::AttachRoles),
        ] {
            if enabled {
                features.push(feature);
            }
        }
        let layers = if self.l3_metadata {
            phux_protocol::LayerSet::with(&[Layer::L3])
        } else {
            phux_protocol::LayerSet::new()
        };
        let server_caps = phux_protocol::caps::ServerCapabilities::new()
            .with_features(phux_protocol::ServerFeatureSet::with(&features))
            .with_layers(layers);
        self.control
            .feed(FrameKind::HelloOk {
                protocol_major: phux_protocol::PROTOCOL_VERSION.major,
                protocol_minor: phux_protocol::PROTOCOL_VERSION.minor,
                protocol_patch: phux_protocol::PROTOCOL_VERSION.patch,
                server_caps,
                server_id: b"ffi-test-server".to_vec(),
                selected_profile: profile,
                bootstrap_limits: limits,
            })
            .expect("test profile must be accepted by runtime control");
        self.drain_outbound();
    }

    pub(crate) fn is_agent_stream(&self, id: &ResourceId) -> bool {
        self.agent_streams.contains_key(id)
    }

    pub(crate) fn open_agent_generation(
        &mut self,
        id: &ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
    ) {
        if let Some(state) = self.agent_streams.get_mut(id) {
            state.generation = Some((stream_id.get(), bootstrap_id.get()));
            state.retained_pending = true;
        }
    }

    pub(crate) fn reset_borrows(&mut self) {
        for cache in self.render.values_mut() {
            cache.metadata.valid = false;
        }
        self.selection_buf.clear();
        self.search_results.clear();
    }

    pub(crate) const fn state(&self) -> PhuxClientState {
        if self.detached {
            return PhuxClientState::Detached;
        }
        match self.control.status() {
            RuntimeStatus::Idle => PhuxClientState::New,
            RuntimeStatus::Connecting => PhuxClientState::HelloQueued,
            RuntimeStatus::Negotiated => PhuxClientState::Negotiated,
            RuntimeStatus::Attached => PhuxClientState::Attached,
            RuntimeStatus::Closed => PhuxClientState::Detached,
            RuntimeStatus::Failed => PhuxClientState::Failed,
        }
    }

    pub(crate) fn set_error(&mut self, message: impl AsRef<str>) {
        self.last_error.clear();
        self.last_error
            .extend_from_slice(message.as_ref().as_bytes());
    }

    pub(crate) fn ensure_attached(&self) -> Result<(), BridgeError> {
        if (self.control.status() == RuntimeStatus::Attached || cfg!(test) && self.attached)
            && !self.detached
        {
            Ok(())
        } else {
            Err(BridgeError::state("operation requires an attached client"))
        }
    }

    pub(crate) fn ensure_participant(&self, id: &ResourceId) -> Result<(), BridgeError> {
        if self.control.active_attach_contains(id)
            || self.operations.admitted(id)
            || self.is_agent_stream(id)
        {
            Ok(())
        } else {
            Err(BridgeError::protocol(
                "terminal state frame targets a terminal outside the active ATTACH",
            ))
        }
    }

    pub(crate) fn detach(&mut self) {
        self.operations.disconnect();
        self.workspace.disconnect();
        self.directory.disconnect();
        self.session_query.disconnect();
        self.session_creates.disconnect();
        self.session_rename.disconnect();
        self.projection.disconnect();
        self.outgoing.clear();
        self.render.clear();
        self.agent_streams.clear();
        self.control.close();
        self.attach_queued = false;
        self.expected_attach_id = None;
        self.attached = false;
        self.detached = true;
    }

    pub(crate) fn active_attach_contains(&self, id: &ResourceId) -> bool {
        self.control.active_attach_contains(id)
    }

    pub(crate) fn input_ready(&self, id: &ResourceId) -> bool {
        self.control
            .engine()
            .is_some_and(|engine| engine.input_ready(id))
    }

    pub(crate) fn terminal_closed(&self, id: &ResourceId) -> bool {
        self.control
            .engine()
            .is_some_and(|engine| engine.is_closed(id))
    }

    #[cfg(test)]
    pub(crate) fn projection(&self, id: &ResourceId) -> Option<Arc<GridFrame>> {
        self.control.publication().acquire(id)
    }

    pub(crate) fn input_eligibility(
        &self,
        id: &ResourceId,
    ) -> phux_client_core::session::InputEligibility {
        self.control.engine().map_or(
            phux_client_core::session::InputEligibility::Ineligible(
                phux_client_core::session::InputBlockReason::UnknownTerminal,
            ),
            |engine| engine.input_eligibility(id),
        )
    }

    #[cfg(test)]
    pub(crate) fn resource_kind(&self, id: &ResourceId) -> Option<ResourceKind> {
        if self.is_agent_stream(id) {
            return Some(ResourceKind::AgentSession);
        }
        let info = self.control.engine()?.replica_info(id).ok()?;
        Some(match info.profile {
            phux_protocol::BootstrapStreamProfile::AgentEventsJsonlV1 => ResourceKind::AgentSession,
            _ => ResourceKind::Terminal,
        })
    }

    pub(crate) fn has_projection(&self, id: &ResourceId) -> bool {
        self.control
            .engine()
            .is_some_and(|engine| engine.has_projection(id))
    }

    pub(crate) fn release_terminal(&mut self, id: &ResourceId) -> bool {
        self.control.release_terminal(id)
    }

    pub(crate) fn terminal_key(&self, id: &ResourceId) -> Result<ReplicaKey, BridgeError> {
        let info = self.engine()?.replica_info(id).map_err(engine_bridge)?;
        let profile = info.profile;
        Ok(ReplicaKey {
            terminal_id: id.clone(),
            stream_id: StreamId::new(info.stream_id)
                .ok_or_else(|| BridgeError::state("runtime published a zero stream id"))?,
            bootstrap_id: BootstrapId::new(info.bootstrap_id)
                .ok_or_else(|| BridgeError::state("runtime published a zero bootstrap id"))?,
            profile,
        })
    }

    pub(crate) fn mouse_tracking(&self, id: &ResourceId) -> Result<bool, BridgeError> {
        Ok(self.engine()?.mouse_mode(id).map_err(engine_bridge)? != MouseMode::None)
    }

    pub(crate) fn mouse_mode(&self, id: &ResourceId) -> Result<MouseMode, BridgeError> {
        self.engine()?.mouse_mode(id).map_err(engine_bridge)
    }

    pub(crate) fn track_anchor(
        &self,
        id: &ResourceId,
        point: PhuxDocumentPoint,
    ) -> Result<PhuxDocumentAnchor, BridgeError> {
        self.ensure_attached()?;
        if point.reserved != 0 {
            return Err(BridgeError::invalid(
                "document point reserved field must be zero",
            ));
        }
        let handle = self
            .engine()?
            .track_anchor(
                id,
                EngineDocumentPoint {
                    space: point.space,
                    column: point.column,
                    row: point.row,
                },
            )
            .map_err(engine_bridge)?;
        Ok(PhuxDocumentAnchor { opaque_id: handle })
    }

    pub(crate) fn release_anchor(
        &self,
        id: &ResourceId,
        anchor: PhuxDocumentAnchor,
    ) -> Result<(), BridgeError> {
        self.engine()?
            .release_anchor(id, anchor.opaque_id)
            .map_err(engine_bridge)
    }

    pub(crate) fn clear_presentation(
        &self,
        id: &ResourceId,
        stream_id: u64,
        bootstrap_id: u64,
    ) -> Result<(), BridgeError> {
        self.engine()?
            .clear_presentation(id, stream_id, bootstrap_id)
            .map_err(engine_bridge)
    }

    pub(crate) fn pin_viewport(
        &self,
        id: &ResourceId,
        anchor: PhuxDocumentAnchor,
    ) -> Result<(), BridgeError> {
        self.engine()?
            .pin_viewport(id, anchor.opaque_id)
            .map_err(engine_bridge)
    }

    pub(crate) fn follow_live(&self, id: &ResourceId) -> Result<(), BridgeError> {
        self.engine()?.follow_live(id).map_err(engine_bridge)
    }

    pub(crate) fn set_selection(
        &self,
        id: &ResourceId,
        start: PhuxDocumentAnchor,
        end: PhuxDocumentAnchor,
        rectangle: bool,
    ) -> Result<(), BridgeError> {
        self.engine()?
            .set_selection(id, start.opaque_id, end.opaque_id, rectangle)
            .map_err(engine_bridge)
    }

    pub(crate) fn clear_selection(&self, id: &ResourceId) -> Result<(), BridgeError> {
        self.engine()?.clear_selection(id).map_err(engine_bridge)
    }

    pub(crate) fn selection_text(&mut self, id: &ResourceId) -> Result<(), BridgeError> {
        self.selection_buf = self.engine()?.selection_text(id).map_err(engine_bridge)?;
        Ok(())
    }

    pub(crate) fn search(
        &mut self,
        id: &ResourceId,
        query: &[u8],
        case_sensitive: bool,
    ) -> Result<(), BridgeError> {
        let query = std::str::from_utf8(query)
            .map_err(|_| BridgeError::invalid("search query is not UTF-8"))?;
        self.search_results = self
            .engine()?
            .search(id, query.to_owned(), case_sensitive)
            .map_err(engine_bridge)?
            .into_iter()
            .map(|result| PhuxSearchResult {
                start: PhuxDocumentAnchor {
                    opaque_id: result.start,
                },
                end: PhuxDocumentAnchor {
                    opaque_id: result.end,
                },
            })
            .collect();
        Ok(())
    }

    #[allow(
        clippy::unnecessary_wraps,
        reason = "keeps the mutation adapter uniform and explicitly idempotent"
    )]
    pub(crate) fn release_search_results(&mut self) -> Result<(), BridgeError> {
        let results = std::mem::take(&mut self.search_results);
        let ids: Vec<ResourceId> = self.control.publication().terminals();
        let mut first = None;
        for result in results {
            for anchor in [result.start, result.end] {
                let released = ids.iter().any(|id| {
                    self.engine()
                        .and_then(|engine| {
                            engine
                                .release_anchor(id, anchor.opaque_id)
                                .map_err(engine_bridge)
                        })
                        .is_ok()
                });
                if !released && first.is_none() {
                    first = Some(BridgeError::state("document anchor is stale or unknown"));
                }
            }
        }
        // Result handles are transient C borrows. A result may already have
        // been invalidated by another viewport mutation; releasing the set is
        // intentionally idempotent, as the pre-runtime shim was.
        let _ = first;
        Ok(())
    }

    pub(crate) fn scroll(&self, id: &ResourceId, kind: u32, value: i64) -> Result<(), BridgeError> {
        let scroll = match kind {
            0 => Scroll::Top,
            1 => Scroll::Bottom,
            2 => Scroll::Delta(value),
            3 => Scroll::Row(
                u64::try_from(value)
                    .map_err(|_| BridgeError::invalid("scroll row must be non-negative"))?,
            ),
            _ => return Err(BridgeError::invalid("unknown viewport scroll kind")),
        };
        self.engine()?.scroll(id, scroll).map_err(engine_bridge)
    }

    pub(crate) fn selection_gesture(
        &self,
        id: &ResourceId,
        event: SelectionGestureEvent,
    ) -> Result<SelectionGestureResult, BridgeError> {
        self.engine()?
            .selection_gesture(id, event)
            .map_err(engine_bridge)
    }

    pub(crate) fn process_send(
        &mut self,
        send: phux_client_core::session::KernelSend,
    ) -> Result<(), BridgeError> {
        use phux_client_core::session::KernelSend;
        let send = match send {
            KernelSend::SubscribeEvents {
                terminal,
                after_seq: None,
            } => KernelSend::SubscribeEvents {
                terminal,
                after_seq: self.event_after_seq,
            },
            send => send,
        };
        if self.operations.fence_send(&send) {
            return Ok(());
        }
        let frame = crate::operations::kernel_send_frame(send)?;
        self.queue_frame(&frame)
    }

    #[allow(
        clippy::unnecessary_wraps,
        reason = "preserves the fallible binding queue seam used by all extension modules"
    )]
    pub(crate) fn queue_frame(&mut self, frame: &FrameKind) -> Result<(), BridgeError> {
        self.control.queue_frame(frame);
        self.drain_outbound();
        Ok(())
    }

    pub(crate) fn drain_outbound(&mut self) {
        self.outgoing.extend(self.control.take_outbound());
    }

    pub(crate) fn process_runtime_events(&mut self) -> Result<bool, BridgeError> {
        let mut attached = false;
        for event in self.control.take_events() {
            attached |= self.process_runtime_event(event)?;
        }
        self.sync_server_features();
        self.publish_effects();
        self.drain_outbound();
        Ok(attached)
    }

    pub(crate) fn process_runtime_event(&mut self, event: Event) -> Result<bool, BridgeError> {
        let event = match self.process_topology_event(event)? {
            EventProjection::Handled(attached) => return Ok(attached),
            EventProjection::Next(event) => event,
        };
        let event = match self.process_effect_event(event) {
            EventProjection::Handled(attached) => return Ok(attached),
            EventProjection::Next(event) => event,
        };
        let event = match self.process_engine_bridge_event(event)? {
            EventProjection::Handled(attached) => return Ok(attached),
            EventProjection::Next(event) => event,
        };
        self.process_lifecycle_event(event);
        Ok(false)
    }

    fn process_topology_event(&mut self, event: Event) -> Result<EventProjection, BridgeError> {
        match event {
            Event::TopologySnapshot {
                attach_id,
                snapshot,
            } => {
                self.resources = snapshot.resources.iter().map(resource_summary).collect();
                for resource in &snapshot.resources {
                    if resource.kind == ResourceKind::AgentSession {
                        self.agent_streams
                            .entry(resource.id.clone())
                            .or_insert(AgentStream {
                                generation: None,
                                retained_pending: false,
                            });
                    }
                }
                self.sessions = crate::workspace::session_summaries(snapshot.clone())?;
                if attach_id.is_some() {
                    crate::workspace::attached(self, snapshot);
                }
            }
            Event::TerminalChanged { terminal_id } => self.damage_effect(&terminal_id),
            Event::TerminalClosed { terminal_id, .. } => {
                self.finish_terminal_close(&terminal_id)?;
            }
            Event::Attached { .. } => {
                self.attach_queued = false;
                self.expected_attach_id = None;
                self.attached = true;
                crate::workspace::initial_read(self);
                return Ok(EventProjection::Handled(true));
            }
            event => return Ok(EventProjection::Next(event)),
        }
        Ok(EventProjection::Handled(false))
    }

    fn process_effect_event(&mut self, event: Event) -> EventProjection {
        match event {
            Event::Bell { terminal_id } => {
                self.owned_effects
                    .push(OwnedEffect::simple(2, 1, terminal_id));
            }
            Event::TitleChanged { terminal_id, title } => {
                let mut effect = OwnedEffect::simple(2, 2, terminal_id);
                effect.bytes = title.into_bytes();
                self.owned_effects.push(effect);
            }
            Event::ResyncRequired {
                terminal_id,
                reason,
            } => {
                let mut effect = OwnedEffect::simple(2, 3, terminal_id);
                effect.status_code = u32::from(reason.as_wire());
                self.owned_effects.push(effect);
            }
            Event::History {
                terminal_id,
                status,
            } => {
                let mut effect = OwnedEffect::simple(2, 6, terminal_id);
                effect.status_code = history_state_code(status.state);
                self.owned_effects.push(effect);
            }
            Event::HistoryUnavailable {
                terminal_id,
                reason,
            } => {
                let mut effect = OwnedEffect::simple(2, 7, terminal_id);
                effect.status_code = history_unavailable_code(reason);
                self.owned_effects.push(effect);
            }
            Event::CwdChanged { terminal_id, cwd } => {
                let mut effect = OwnedEffect::simple(2, 8, terminal_id);
                effect.bytes = cwd.into_bytes();
                self.owned_effects.push(effect);
            }
            Event::CommandStarted { terminal_id } => {
                self.owned_effects
                    .push(OwnedEffect::simple(2, 9, terminal_id));
            }
            Event::CommandFinished {
                terminal_id,
                exit_code,
            } => {
                let mut effect = OwnedEffect::simple(2, 10, terminal_id);
                encode_optional_i32(&mut effect.stream_id, &mut effect.bootstrap_id, exit_code);
                self.owned_effects.push(effect);
            }
            event => return EventProjection::Next(event),
        }
        EventProjection::Handled(false)
    }

    fn process_engine_bridge_event(
        &mut self,
        event: Event,
    ) -> Result<EventProjection, BridgeError> {
        match event {
            Event::Exited {
                terminal_id,
                exit_status,
                signal,
                reason,
            } => {
                if !self.is_agent_stream(&terminal_id) {
                    self.exit_effect(terminal_id, exit_status, signal, reason);
                }
            }
            Event::AgentRecords {
                terminal_id,
                records,
            } => self.agent_records_effect(terminal_id, &records)?,
            Event::KernelSend(send) => self.process_send(send)?,
            event => return Ok(EventProjection::Next(event)),
        }
        Ok(EventProjection::Handled(false))
    }

    fn process_lifecycle_event(&mut self, event: Event) {
        match event {
            Event::Detached { reason, message } => {
                let mut effect = OwnedEffect::simple(2, 5, ResourceId::local(0));
                effect.status_code = reason
                    .map_or(crate::types::DETACH_REASON_UNSTATED, |reason| {
                        u32::from(reason.as_wire())
                    });
                effect.bytes = message.into_bytes();
                self.detach();
                self.owned_effects.push(effect);
            }
            Event::ServerError { code, message, .. } => {
                self.set_error(&message);
                let mut effect = OwnedEffect::simple(2, 4, ResourceId::local(0));
                effect.bytes = format!("{code:?}: {message}").into_bytes();
                self.owned_effects.push(effect);
            }
            _ => {}
        }
    }

    pub(crate) fn finish_terminal_close(&mut self, id: &ResourceId) -> Result<(), BridgeError> {
        self.workspace.subscriptions.cancel(id);
        self.workspace.subscriptions.mark_closed(id);
        if let Some(stream) = self.agent_streams.remove(id) {
            let (stream_id, bootstrap_id) = stream.generation.unwrap_or((0, 0));
            let mut effect = OwnedEffect::simple(
                crate::types::EFFECT_AGENT_RECORDS,
                crate::types::AGENT_RECORDS_CLOSED,
                id.clone(),
            );
            effect.stream_id = stream_id;
            effect.bootstrap_id = bootstrap_id;
            self.owned_effects.push(effect);
        } else {
            crate::operations::release_terminal(self, id)?;
            self.operations.observe_resource_closed(id);
            self.owned_effects
                .push(OwnedEffect::simple(1, 3, id.clone()));
        }
        self.render.remove(id);
        self.resources.retain(|resource| &resource.id != id);
        Ok(())
    }

    fn damage_effect(&mut self, id: &ResourceId) {
        self.render.remove(id);
        let Some(frame) = self.control.publication().acquire(id) else {
            self.owned_effects
                .push(OwnedEffect::simple(1, 3, id.clone()));
            return;
        };
        let mut effect = OwnedEffect::simple(1, 0, id.clone());
        match frame.damage {
            phux_client_core::grid::GridDamage::Full => effect.detail = 1,
            phux_client_core::grid::GridDamage::Rows => {
                effect.detail = 2;
                let mut rows = frame.dirty_rows();
                let Some(first) = rows.next() else { return };
                effect.first_row = first;
                effect.last_row = rows.last().unwrap_or(first);
            }
            phux_client_core::grid::GridDamage::Clean => return,
        }
        self.owned_effects.push(effect);
    }

    fn exit_effect(
        &mut self,
        id: ResourceId,
        exit_status: Option<i32>,
        signal: Option<i32>,
        reason: CloseReason,
    ) {
        let mut effect = OwnedEffect::simple(2, 11, id);
        effect.status_code = u32::from(reason.as_wire());
        encode_optional_i32(&mut effect.stream_id, &mut effect.bootstrap_id, exit_status);
        effect.first_row = signal
            .and_then(|value| u16::try_from(value).ok())
            .unwrap_or(0);
        self.owned_effects.push(effect);
    }

    fn agent_records_effect(
        &mut self,
        id: ResourceId,
        records: &[phux_client_core::session::agent_stream::AgentEventRecord],
    ) -> Result<(), BridgeError> {
        let (generation, retained) =
            self.agent_streams
                .get_mut(&id)
                .map_or((None, false), |state| {
                    (
                        state.generation,
                        std::mem::replace(&mut state.retained_pending, false),
                    )
                });
        let mut bytes = Vec::new();
        for record in records {
            let line = serde_json::json!({
                "seq": record.seq,
                "ts_ms": record.ts_ms,
                "type": record.kind.as_str(),
                "data": record.data,
            });
            serde_json::to_writer(&mut bytes, &line)
                .map_err(|error| BridgeError::engine(error.to_string()))?;
            bytes.push(b'\n');
        }
        let detail = if retained {
            crate::types::AGENT_RECORDS_RETAINED
        } else {
            crate::types::AGENT_RECORDS_LIVE
        };
        let mut effect = OwnedEffect::simple(crate::types::EFFECT_AGENT_RECORDS, detail, id);
        let (stream_id, bootstrap_id) = generation.unwrap_or((0, 0));
        effect.stream_id = stream_id;
        effect.bootstrap_id = bootstrap_id;
        effect.seq = records.last().map_or(0, |record| record.seq);
        effect.bytes = bytes;
        self.owned_effects.push(effect);
        Ok(())
    }

    fn sync_server_features(&mut self) {
        let Some(server) = self.control.server() else {
            return;
        };
        self.protocol_ready = true;
        self.server_id.clone_from(&server.id);
        self.terminal_reply = negotiated_test_feature(
            self.terminal_reply,
            server.has(ServerFeature::TerminalReply),
        );
        self.list_directory = negotiated_test_feature(
            self.list_directory,
            server.has(ServerFeature::ListDirectory),
        );
        self.list_directory_host = negotiated_test_feature(
            self.list_directory_host,
            server.has(ServerFeature::ListDirectoryHost),
        );
        self.keep_empty_sessions = negotiated_test_feature(
            self.keep_empty_sessions,
            server.has(ServerFeature::KeepEmptySessions),
        );
        self.conditional_kill = negotiated_test_feature(
            self.conditional_kill,
            server.has(ServerFeature::ConditionalKill),
        );
        self.event_journal =
            negotiated_test_feature(self.event_journal, server.has(ServerFeature::EventJournal));
        self.retain_on_exit =
            negotiated_test_feature(self.retain_on_exit, server.has(ServerFeature::RetainOnExit));
        self.spawn_idempotency = negotiated_test_feature(
            self.spawn_idempotency,
            server.has(ServerFeature::SpawnIdempotency),
        );
        self.close_tab_resources = negotiated_test_feature(
            self.close_tab_resources,
            server.has(ServerFeature::CloseTabResources),
        );
        self.attach_roles =
            negotiated_test_feature(self.attach_roles, server.has(ServerFeature::AttachRoles));
        self.l3_metadata =
            negotiated_test_feature(self.l3_metadata, server.layers.contains(Layer::L3));
    }

    fn engine(&self) -> Result<&phux_client_runtime::engine::EngineHandle, BridgeError> {
        self.control
            .engine()
            .ok_or_else(|| BridgeError::state("terminal engine is not negotiated"))
    }

    pub(crate) const fn publish_effects(&mut self) {
        self.effect_count = self.owned_effects.len();
    }

    pub(crate) fn effect_view(&self, index: usize) -> Option<PhuxClientEffect> {
        self.owned_effects[..self.effect_count]
            .get(index)
            .map(|effect| {
                #[cfg(test)]
                EFFECT_VIEW_BUILDS.set(EFFECT_VIEW_BUILDS.get() + 1);
                PhuxClientEffect {
                    kind: effect.kind,
                    detail: effect.detail,
                    status_code: effect.status_code,
                    terminal_id: terminal_id_out(&effect.terminal_id),
                    stream_id: effect.stream_id,
                    bootstrap_id: effect.bootstrap_id,
                    seq: effect.seq,
                    first_row: effect.first_row,
                    last_row: effect.last_row,
                    bytes: bytes_out(&effect.bytes),
                }
            })
    }

    pub(crate) fn build_grid(
        &mut self,
        id: &ResourceId,
    ) -> Result<*const PhuxTerminalGridView, BridgeError> {
        let frame = self
            .control
            .publication()
            .acquire(id)
            .ok_or_else(|| BridgeError::state("terminal has no published grid"))?;
        let info = self.engine()?.replica_info(id).map_err(engine_bridge)?;
        let history = history_counters(info.history.as_ref());
        if self
            .render
            .get(id)
            .is_some_and(|cache| render_cache_matches(cache, &frame, &info, &history))
        {
            let top_anchor = self.track_anchor(
                id,
                PhuxDocumentPoint {
                    space: 1,
                    row: 0,
                    column: 0,
                    reserved: 0,
                },
            )?;
            let Some(cache) = self.render.get_mut(id) else {
                return Err(BridgeError::state("render cache disappeared"));
            };
            cache.view.top_anchor = top_anchor;
            cache.metadata.valid = true;
            return Ok(ptr::from_ref(&cache.view));
        }
        #[cfg(test)]
        RENDER_CACHE_BUILDS.set(RENDER_CACHE_BUILDS.get() + 1);
        let top_anchor = self.track_anchor(
            id,
            PhuxDocumentPoint {
                space: 1,
                row: 0,
                column: 0,
                reserved: 0,
            },
        )?;
        let mut host = Vec::new();
        let terminal_id = view_terminal_id(id, &mut host);
        let view = PhuxTerminalGridView {
            terminal_id,
            stream_id: frame.stream_id,
            bootstrap_id: frame.bootstrap_id,
            last_seq: frame.last_seq,
            document_revision: info.document_revision,
            cols: frame.cols,
            rows: frame.rows,
            cells: if frame.buffer.cells.is_empty() {
                ptr::null()
            } else {
                frame.buffer.cells.as_ptr()
            },
            cell_count: frame.buffer.cells.len(),
            utf8: bytes_out(&frame.buffer.utf8),
            cursor_visible: frame.cursor.visible,
            cursor_col: frame.cursor.col,
            cursor_row: frame.cursor.row,
            cursor_style: frame.cursor.style as u32,
            history_total_rows: frame.scrollbar.total,
            history_viewport_offset: frame.scrollbar.offset,
            history_visible_rows: frame.scrollbar.len,
            history_pages_loaded: history.pages,
            history_unread_rows: history.unread_rows,
            history_bytes_loaded: history.bytes,
            history_loading: history.loading,
            history_has_more: history.has_more,
            top_anchor,
        };
        let metadata = GridMetadataCache::from_frame(&frame, info.document_revision);
        self.render.insert(
            id.clone(),
            RenderCache {
                frame,
                _terminal_host: host,
                view,
                metadata,
            },
        );
        let Some(cache) = self.render.get(id) else {
            return Err(BridgeError::state("render cache insertion failed"));
        };
        Ok(ptr::from_ref(&cache.view))
    }

    pub(crate) fn perf_json(&mut self) {
        self.perf_buf = phux_client_core::perf::report(self.created_at.elapsed())
            .to_json()
            .into_bytes();
    }
}

fn render_cache_matches(
    cache: &RenderCache,
    frame: &Arc<GridFrame>,
    info: &phux_client_runtime::engine::ReplicaInfo,
    history: &HistoryCounters,
) -> bool {
    Arc::ptr_eq(&cache.frame, frame)
        && cache.view.document_revision == info.document_revision
        && cache.view.history_pages_loaded == history.pages
        && cache.view.history_unread_rows == history.unread_rows
        && cache.view.history_bytes_loaded == history.bytes
        && cache.view.history_loading == history.loading
        && cache.view.history_has_more == history.has_more
}

const fn negotiated_test_feature(current: bool, negotiated: bool) -> bool {
    #[cfg(test)]
    {
        current || negotiated
    }
    #[cfg(not(test))]
    {
        let _ = current;
        negotiated
    }
}

fn engine_bridge(error: EngineError) -> BridgeError {
    match error {
        EngineError::Engine(message) => BridgeError::state(message),
        EngineError::Stopped => BridgeError::engine("the engine owner thread stopped"),
        EngineError::Spawn(error) => BridgeError::engine(error.to_string()),
    }
}

pub(crate) fn resource_summary(
    resource: &phux_protocol::wire::info::ResourceInfo,
) -> ResourceSummary {
    let agent = resource.agent.as_ref();
    ResourceSummary::new(
        resource.id.clone(),
        u32::from(resource.kind.as_wire()),
        resource.parent.clone(),
        agent.map_or_else(Vec::new, |facet| facet.provider.as_bytes().to_vec()),
        agent
            .and_then(|facet| facet.native_id.as_ref())
            .map_or_else(Vec::new, |value| value.as_bytes().to_vec()),
        agent.map_or_else(Vec::new, |facet| facet.state.as_bytes().to_vec()),
    )
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

fn view_terminal_id(id: &ResourceId, host: &mut Vec<u8>) -> PhuxResourceId {
    if let ResourceId::Satellite {
        host: satellite, ..
    } = id
    {
        host.extend_from_slice(satellite.as_str().as_bytes());
    }
    resource_id_with_host(id, host)
}

struct HistoryCounters {
    pages: u64,
    bytes: u64,
    unread_rows: u64,
    loading: bool,
    has_more: bool,
}

fn history_counters(status: Option<&HistoryStatus>) -> HistoryCounters {
    status.map_or(
        HistoryCounters {
            pages: 0,
            bytes: 0,
            unread_rows: 0,
            loading: false,
            has_more: false,
        },
        |status| HistoryCounters {
            pages: status.loaded_pages as u64,
            bytes: status.loaded_bytes as u64,
            unread_rows: status.unread_rows,
            loading: status.state == HistoryLoadState::Loading,
            has_more: status.next_cursor.is_some(),
        },
    )
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

#[allow(dead_code)]
const fn rgb(value: Rgb) -> crate::grid_metadata::PhuxGridRgb {
    crate::grid_metadata::PhuxGridRgb {
        r: value.r,
        g: value.g,
        b: value.b,
    }
}
