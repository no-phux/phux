//! The sans-IO control plane (ADR-0133 decision 1).
//!
//! A [`ControlPlane`] is a synchronous state machine over the session
//! kernel: decoded inbound frames go in through [`ControlPlane::feed`] (or
//! raw bytes through [`ControlPlane::feed_bytes`], the model Cockpit drives
//! `phux-client-ffi` with), encoded outbound frames come out of
//! [`ControlPlane::take_outbound`], and owned [`Event`]s out of
//! [`ControlPlane::take_events`]. It never touches a socket or a clock it
//! is not handed, so the same object serves a consumer that owns its socket
//! and the [`connection`](crate::connection) driver.
//!
//! It owns the connection lifecycle (`HELLO` to `HELLO_OK` acceptance
//! through `phux_client_core::handshake`, `ATTACH`, `ATTACH_READY`,
//! `DETACH`), the topology, per-terminal attach/detach/spawn/kill/close,
//! input on the raw path and the acknowledged `APPLY_INPUT` path through
//! `phux_client_core::input_replay`, `SUBSCRIBE_EVENTS` and the folding of
//! agent events, and error and refusal reporting. Terminal state itself is
//! the kernel's, hosted on the [`engine`](crate::engine) owner thread.
//!
//! Extension points for later rungs, so a binding adds a lane without a
//! second state machine: [`ControlPlane::send_command`] correlates any
//! `COMMAND` and answers it as [`Event::CommandResult`],
//! [`ControlPlane::queue_frame`] sends any frame, and every inbound frame
//! the plane does not consume surfaces as [`Event::Frame`].

use std::collections::{HashMap, HashSet};
#[cfg(feature = "engine")]
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::BytesMut;
use phux_client_core::handshake::validate_hello_ok;
use phux_client_core::history::HistoryCacheConfig;
use phux_client_core::input_replay::{
    INPUT_RETRY_HORIZON, InputReplayJournal, ReplayDisposition, ReplayReport,
};
use phux_client_core::session::{
    HistoryRejectionReason, HistoryUnavailableReason, KernelEffect, KernelSend, KernelStatus,
};
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapCapabilities, BootstrapLimits, BootstrapProfile, ClientCapabilities, ImageProtocolSet,
    Layer, LayerSet, ServerFeature, ServerFeatureSet,
};
#[cfg(not(feature = "engine"))]
use phux_protocol::caps::{BootstrapProfileKind, BootstrapProfileSet};
use phux_protocol::ids::{GroupId, InputOperationId, ResourceId, ResourceKind};
use phux_protocol::input::InputEvent;
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::input::key::KeyEvent;
use phux_protocol::input::mouse::MouseEvent;
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::wire::frame::{
    AgentEvent, AttachTarget, Command, CommandResult, CommandValue, DetachReason, ErrorCode,
    FrameKind, HistoryRejectionReason as WireRejection, HistoryTombstoneReason as WireTombstone,
    MAX_APPLY_INPUT_COMMAND_BODY, MAX_INPUT_TERMINAL_REPLY_BYTES, RolePolicy, SpawnResult,
    StateScope, ViewportInfo,
};
use phux_protocol::wire::info::SessionSnapshot;

use crate::engine::{EngineApplyError, EngineConfig, EngineEvent, EngineHandle, EngineOutcome};
#[cfg(feature = "engine")]
use crate::engine::{EngineError, Scroll};
#[cfg(feature = "engine")]
use crate::publication::Publication;

mod agents;
mod commands;
mod events;
mod extensions;
mod frames;
mod input;
mod kernel;
pub mod keys;
mod responses;
mod state;
mod topology;

pub use events::{DeliveryOutcome, Event, Status};
pub use extensions::{
    DirectoryChild, DirectoryFailure, DirectoryListing, FileUploadOutcome, FileUploadReceipt,
    TranscribeOutcome, TranscribeReceipt,
};
pub use topology::{
    AgentSessionDescriptor, PaneDescriptor, SessionDescriptor, Topology, WindowDescriptor,
};

/// Upper bound on undrained events.
///
/// A wedged consumer must not grow the queue without bound. On overflow the
/// droppable events go and a fresh `TopologyChanged` is queued, so a
/// consumer re-derives from the topology, which is authoritative; lossless
/// events (see [`Event::is_lossless`]) stay.
pub const EVENT_QUEUE_CAP: usize = 4096;

/// Exact encoded command-body overhead of an `apply_line` /
/// `apply_tab_completion` batch (a paste plus one key).
const APPLY_LINE_OVERHEAD: usize = 46;
/// Exact encoded command-body overhead of an `apply_paste` batch.
const APPLY_PASTE_OVERHEAD: usize = 30;

/// What a consumer asks the control plane to do on every connection.
#[derive(Debug, Clone)]
pub struct ControlOptions {
    /// The `HELLO` client name.
    pub client_name: String,
    /// Whether the runtime queues its post-handshake attach or topology read.
    ///
    /// Socket-owning embedders set this to `false`: their stable ABI exposes
    /// `HELLO` and `ATTACH` as explicit calls, while this plane still owns all
    /// lifecycle validation and frame application.
    pub automatic_lifecycle: bool,
    /// The viewport `ATTACH` and spawns declare, in cells.
    pub viewport: (u16, u16),
    /// The scrollback depth `ATTACH` requests and the history cache keeps.
    pub scrollback_lines: u32,
    /// The session every connection attaches after `HELLO_OK`; `None`
    /// browses the topology (`GET_STATE`) without attaching.
    pub attach: Option<AttachTarget>,
    /// The role every `ATTACH` declares (ADR-0127); `None` sends nothing.
    pub attach_role: Option<RolePolicy>,
    /// The journal cursor the automatic `SUBSCRIBE_EVENTS` carries on a
    /// server that advertises `EVENT_JOURNAL`; `None` is live-only.
    pub event_after_seq: Option<u64>,
    /// Pick up a terminal another client spawned with a per-terminal
    /// attach on the live socket, so its output reaches this client
    /// without a reconnect.
    pub auto_attach_foreign_spawns: bool,
    /// The bootstrap payload limits offered in `HELLO`.
    pub bootstrap_limits: BootstrapLimits,
}

impl Default for ControlOptions {
    fn default() -> Self {
        Self {
            client_name: "phux-client-runtime".to_owned(),
            automatic_lifecycle: true,
            viewport: (80, 24),
            scrollback_lines: 1000,
            attach: None,
            attach_role: None,
            event_after_seq: None,
            auto_attach_foreign_spawns: true,
            bootstrap_limits: BootstrapLimits::default(),
        }
    }
}

impl ControlOptions {
    /// The capabilities `HELLO` advertises: every bootstrap profile the
    /// hosted engine can consume, and L3 metadata.
    #[must_use]
    #[allow(
        clippy::missing_const_for_fn,
        reason = "the engine offer probes libghostty at runtime; only the headless offer is constant"
    )]
    pub fn client_caps(&self) -> ClientCapabilities {
        ClientCapabilities::new()
            // GridFrame publishes text/style POD cells, not image planes. Do
            // not pay mobile bandwidth for escapes no runtime consumer sees.
            .with_image_protocols(ImageProtocolSet::new())
            .with_layers(LayerSet::with(&[Layer::L3]))
            .with_bootstrap(self.bootstrap_caps())
    }

    #[cfg(feature = "engine")]
    fn bootstrap_caps(&self) -> BootstrapCapabilities {
        phux_client_core::engine::ghostty::native_bootstrap_capabilities(self.bootstrap_limits)
    }

    #[cfg(not(feature = "engine"))]
    const fn bootstrap_caps(&self) -> BootstrapCapabilities {
        BootstrapCapabilities::new()
            .with_profiles(BootstrapProfileSet::with(&[
                BootstrapProfileKind::SynthesizedVtRaw,
            ]))
            .with_limits(self.bootstrap_limits)
    }
}

/// What `HELLO_OK` negotiated.
#[derive(Debug, Clone)]
pub struct ServerInfo {
    /// The server's incarnation identity.
    pub id: Vec<u8>,
    /// The features the server advertised.
    pub features: ServerFeatureSet,
    /// The layers the server advertised.
    pub layers: LayerSet,
    /// The protocol triple the server selected.
    pub protocol: (u16, u16, u16),
    /// The bootstrap profile the server selected.
    pub profile: BootstrapProfile,
    /// The payload limits the server selected.
    pub limits: BootstrapLimits,
}

impl ServerInfo {
    /// Whether the server advertised `feature`.
    #[must_use]
    pub const fn has(&self, feature: ServerFeature) -> bool {
        self.features.contains(feature)
    }
}

/// How a fed frame ended the connection, when it did.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ControlError {
    /// The peer broke the protocol or the kernel could not apply the
    /// frame; the connection should be dropped and redialed for fresh
    /// snapshots.
    #[error("{0}")]
    Protocol(String),
    /// A refusal no retry can satisfy; the session is over.
    #[error("{0}")]
    Refused(String),
    /// A retired, mismatched, or duplicate generation: the C ABI
    /// reports `InvalidState` and the session stays attached.
    #[error("{0}")]
    InvalidState(String),
    /// A replica generation was invalidated; reconnect for fresh
    /// snapshots.
    #[error("a replica needs a fresh bootstrap")]
    Resync,
    /// The server detached the session at this client's request; do not
    /// reconnect.
    #[error("the session was detached at the client's request")]
    Closed,
}

impl ControlError {
    const fn precedence(&self) -> u8 {
        match self {
            Self::InvalidState(_) => 0,
            Self::Protocol(_) => 1,
            Self::Resync => 2,
            Self::Refused(_) | Self::Closed => 3,
        }
    }

    fn prefer(current: Option<Self>, candidate: Self) -> Self {
        match current {
            Some(error) if error.precedence() >= candidate.precedence() => error,
            _ => candidate,
        }
    }
}

#[cfg(test)]
mod control_error_tests {
    use super::ControlError;

    #[test]
    fn fatal_batch_errors_outrank_nonfatal_stale_generation_errors() {
        let stale = ControlError::InvalidState("stale".to_owned());
        let protocol = ControlError::Protocol("gap".to_owned());
        let resync = ControlError::Resync;

        let selected = ControlError::prefer(Some(stale), protocol);
        assert!(matches!(selected, ControlError::Protocol(_)));
        let selected = ControlError::prefer(Some(selected), resync);
        assert!(matches!(selected, ControlError::Resync));
    }
}

/// A spawn a consumer asks for.
#[derive(Debug, Clone, Default)]
pub struct SpawnRequest {
    /// The command, or the server's default shell.
    pub command: Option<Vec<String>>,
    /// The working directory, or the server's default.
    pub cwd: Option<String>,
    /// Extra environment.
    pub env: Option<Vec<(String, String)>>,
    /// The session to spawn into, by id; `None` is the attached session.
    pub session_id: Option<u32>,
    /// The pane geometry; `None` is the viewport.
    pub initial_size: Option<(u16, u16)>,
}

/// How `ensure_stream` decided to make a terminal's stream live again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamRecovery {
    /// A per-terminal attach was queued on the live socket.
    Attached,
    /// Only a reconnect (fresh snapshots) can help; the driver resyncs.
    Reconnect,
    /// Nothing to do: closed, or recovery already in flight.
    Noop,
}

/// What one outstanding `COMMAND` asked for, so its reply resolves to the
/// right event.
#[derive(Debug, Clone)]
enum Pending {
    Spawn,
    AttachTerminal(ResourceId),
    DetachTerminal(ResourceId),
    Kill(ResourceId),
    Close,
    RefreshTopology,
    /// A binding's own command; the reply is surfaced raw.
    Extension,
    /// One chunk of a runtime-owned durable upload.
    PutFile(u64),
    /// A one-shot transcription request for a completed upload.
    Transcribe(u64),
}

/// The sans-IO control plane. See the module docs.
#[derive(Debug)]
pub struct ControlPlane {
    options: ControlOptions,
    offered_caps: ClientCapabilities,
    status: Status,
    handshake_ready: bool,
    attached_once: bool,
    detach_requested: bool,
    error: Option<String>,
    server: Option<ServerInfo>,
    engine: Option<EngineHandle>,
    engine_config: Option<EngineConfig>,
    history_config: Option<HistoryCacheConfig>,
    #[cfg(feature = "engine")]
    publication: Arc<Publication>,
    topology: Option<Topology>,
    /// The target every connection attaches; a `CreateIfMissing` becomes
    /// `ByName` once sent, so a reconnect never re-creates.
    attach_target: Option<AttachTarget>,
    active_attach_id: Option<u32>,
    attach_terminals: HashSet<ResourceId>,
    /// The home session whose pumps the connection-level `ATTACH` opened.
    attached_session: Option<u32>,
    /// The session the consumer is currently viewing. This may differ from
    /// `attached_session` after a live, per-terminal session switch.
    selected_session: Option<u32>,
    /// Terminals subscribed with a per-terminal attach on this connection.
    terminal_attached: HashSet<ResourceId>,
    /// Terminals whose replacement snapshot is requested but not published.
    stream_recoveries: HashSet<ResourceId>,
    /// Terminals this client's own spawns created on this connection.
    own_spawns: HashSet<ResourceId>,
    /// `AgentSession` resources declared to the kernel.
    agent_streams: HashSet<ResourceId>,
    pending: HashMap<u32, Pending>,
    request_seq: u32,
    input_replay: InputReplayJournal,
    input_delivery_ids: HashMap<String, (u64, ResourceId)>,
    input_deadlines: HashMap<String, Instant>,
    input_delivery_seq: u64,
    clock_origin: Instant,
    outbound: Vec<Vec<u8>>,
    events: Vec<Event>,
    damaged: Vec<ResourceId>,
    extensions: extensions::Extensions,
}

impl ControlPlane {
    /// A control plane that has opened no connection.
    #[must_use]
    pub fn new(options: ControlOptions) -> Self {
        let offered_caps = options.client_caps();
        Self {
            attach_target: options.attach.clone(),
            options,
            offered_caps,
            status: Status::Idle,
            handshake_ready: false,
            attached_once: false,
            detach_requested: false,
            error: None,
            server: None,
            engine: None,
            engine_config: None,
            history_config: None,
            #[cfg(feature = "engine")]
            publication: Arc::new(Publication::new()),
            topology: None,
            active_attach_id: None,
            attach_terminals: HashSet::new(),
            attached_session: None,
            selected_session: None,
            terminal_attached: HashSet::new(),
            stream_recoveries: HashSet::new(),
            own_spawns: HashSet::new(),
            agent_streams: HashSet::new(),
            pending: HashMap::new(),
            request_seq: 1,
            input_replay: InputReplayJournal::new(),
            input_delivery_ids: HashMap::new(),
            input_deadlines: HashMap::new(),
            input_delivery_seq: 1,
            clock_origin: Instant::now(),
            outbound: Vec::new(),
            events: Vec::new(),
            damaged: Vec::new(),
            extensions: extensions::Extensions::default(),
        }
    }

    /// Override the history cache policy before opening the connection.
    /// Bindings with explicit cache and prefetch limits use this builder;
    /// ordinary runtime clients derive policy from [`ControlOptions`].
    #[must_use]
    pub fn with_history_config(mut self, config: HistoryCacheConfig) -> Self {
        self.history_config = Some(config.normalized());
        self
    }

    // ----- observation -------------------------------------------------

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

    /// The current viewport.
    #[must_use]
    pub const fn viewport(&self) -> (u16, u16) {
        self.options.viewport
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

    /// Reserve a per-terminal pump queued by a manual binding.
    pub fn admit_external_attach(&mut self, terminal_id: &ResourceId) -> bool {
        if self.terminal_is_admitted(terminal_id) {
            return false;
        }
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
        let known = self.attach_terminals.remove(terminal_id)
            | self.terminal_attached.remove(terminal_id)
            | self.own_spawns.remove(terminal_id)
            | self.agent_streams.remove(terminal_id);
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
        self.outbound.clear();
        self.terminal_attached.clear();
        self.stream_recoveries.clear();
        self.own_spawns.clear();
        self.handshake_ready = false;
        self.active_attach_id = None;
        self.attach_terminals.clear();
        self.attached_session = None;
        self.selected_session = None;
        self.input_replay.connection_lost();
        self.reset_extension_correlations("the connection ended before the server answered");
        self.fail_pending("connection replaced before the server answered");
        self.set_status(Status::Connecting);
        self.queue_frame(&FrameKind::Hello {
            client_name: self.options.client_name.clone(),
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            client_caps: self.offered_caps,
        });
    }

    /// The transport dropped; the driver will walk the ladder.
    pub fn connection_lost(&mut self, message: Option<String>) {
        self.handshake_ready = false;
        self.input_replay.connection_lost();
        self.reset_extension_correlations("the connection ended before the server answered");
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
        self.handshake_ready = false;
        self.error = Some(message.into());
        self.strand_durable("the connection failed before the operation completed");
        self.strand_extensions("the connection failed before the operation completed");
        self.fail_pending("the connection failed before the server answered");
        self.set_status(Status::Failed);
    }

    /// The session ends at the consumer's request.
    pub fn close(&mut self) {
        self.handshake_ready = false;
        self.strand_durable("the client closed before the operation completed");
        self.strand_extensions("the client closed before the operation completed");
        self.fail_pending("the client closed before the server answered");
        if let Some(engine) = &self.engine {
            engine.reset_connection();
        }
        self.active_attach_id = None;
        self.attach_terminals.clear();
        self.attached_session = None;
        self.selected_session = None;
        self.terminal_attached.clear();
        self.agent_streams.clear();
        self.set_status(Status::Closed);
    }
}

/// Encode one frame as SPEC section 5 bytes.
#[must_use]
pub fn encode(frame: &FrameKind) -> Vec<u8> {
    let mut encoded = BytesMut::new();
    frame.encode(&mut encoded);
    encoded.to_vec()
}

/// The retry-horizon bound a driver may sleep against when nothing is
/// queued, so a wake never waits longer than one horizon.
#[must_use]
pub const fn max_expiry_wait() -> Duration {
    INPUT_RETRY_HORIZON
}
