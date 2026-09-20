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
use phux_client_core::input_replay::{
    INPUT_RETRY_HORIZON, InputReplayJournal, ReplayDisposition, ReplayReport,
};
use phux_client_core::session::{
    HistoryRejectionReason, HistoryUnavailableReason, KernelEffect, KernelSend, KernelStatus,
};
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapCapabilities, BootstrapLimits, BootstrapProfile, ClientCapabilities, Layer, LayerSet,
    ServerFeature, ServerFeatureSet,
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

use crate::engine::{EngineConfig, EngineEvent, EngineHandle, EngineOutcome};
#[cfg(feature = "engine")]
use crate::publication::Publication;

mod events;
pub mod keys;
mod topology;

pub use events::{DeliveryOutcome, Event, Status};
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
    /// A replica generation was invalidated; reconnect for fresh
    /// snapshots.
    #[error("a replica needs a fresh bootstrap")]
    Resync,
    /// The server detached the session at this client's request; do not
    /// reconnect.
    #[error("the session was detached at the client's request")]
    Closed,
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
    AttachTerminal(ResourceId),
    DetachTerminal(ResourceId),
    Kill(ResourceId),
    Close,
    RefreshTopology,
    /// A binding's own command; the reply is surfaced raw.
    Extension,
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
    #[cfg(feature = "engine")]
    publication: Arc<Publication>,
    topology: Option<Topology>,
    /// The target every connection attaches; a `CreateIfMissing` becomes
    /// `ByName` once sent, so a reconnect never re-creates.
    attach_target: Option<AttachTarget>,
    active_attach_id: Option<u32>,
    attach_terminals: HashSet<ResourceId>,
    /// The session the active attach bootstrapped.
    attached_session: Option<u32>,
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
            #[cfg(feature = "engine")]
            publication: Arc::new(Publication::new()),
            topology: None,
            active_attach_id: None,
            attach_terminals: HashSet::new(),
            attached_session: None,
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
        }
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

    /// The session the active attach bootstrapped.
    #[must_use]
    pub const fn attached_session(&self) -> Option<u32> {
        self.attached_session
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
        self.input_replay.connection_lost();
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
        self.fail_pending("the connection failed before the server answered");
        self.set_status(Status::Failed);
    }

    /// The session ends at the consumer's request.
    pub fn close(&mut self) {
        self.handshake_ready = false;
        self.strand_durable("the client closed before the operation completed");
        self.fail_pending("the client closed before the server answered");
        self.set_status(Status::Closed);
    }

    // ----- frames in ------------------------------------------------

    /// Decode exactly one SPEC section 5 frame under the negotiated limits
    /// and feed it.
    pub fn feed_bytes(&mut self, bytes: &[u8]) -> Result<(), ControlError> {
        let decoded = self.decode_limits().map_or_else(
            || FrameKind::decode(bytes),
            |limits| FrameKind::decode_with_limits(bytes, limits),
        );
        let (frame, tail) = decoded
            .map_err(|error| ControlError::Protocol(format!("invalid protocol frame: {error}")))?;
        if !tail.is_empty() {
            return Err(ControlError::Protocol(
                "protocol message contained trailing bytes".to_owned(),
            ));
        }
        self.feed(frame)
    }

    /// Apply one decoded inbound frame.
    pub fn feed(&mut self, frame: FrameKind) -> Result<(), ControlError> {
        if !self.handshake_ready
            && !matches!(
                frame,
                FrameKind::HelloOk { .. }
                    | FrameKind::Error { .. }
                    | FrameKind::Detached { .. }
                    | FrameKind::Ping { .. }
                    | FrameKind::Pong { .. }
            )
        {
            return Err(ControlError::Protocol(
                "server frame arrived before HELLO_OK".to_owned(),
            ));
        }
        match frame {
            FrameKind::HelloOk {
                protocol_major,
                protocol_minor,
                protocol_patch,
                server_caps,
                server_id,
                selected_profile,
                bootstrap_limits,
            } => self.hello_ok(
                (protocol_major, protocol_minor, protocol_patch),
                &server_id,
                server_caps.features,
                server_caps.layers,
                selected_profile,
                bootstrap_limits,
            ),
            FrameKind::Ping { nonce } => {
                self.queue_frame(&FrameKind::Pong { nonce });
                Ok(())
            }
            // The answer to a liveness probe: its arrival was the point.
            FrameKind::Pong { .. } => Ok(()),
            FrameKind::Attached {
                attach_id,
                snapshot,
                ..
            } => self.attached(attach_id, &snapshot),
            FrameKind::AttachReady { attach_id } => self.attach_ready(attach_id),
            FrameKind::Error {
                request_id,
                code,
                message,
            } => self.server_error(request_id, code, message),
            FrameKind::Detached { reason, message } => self.detached(reason, &message),
            FrameKind::Bell { terminal_id } => {
                self.push_event(Event::Bell { terminal_id });
                Ok(())
            }
            FrameKind::ResourceClosed {
                terminal_id,
                exit_status,
                reason,
                signal,
            } => {
                self.apply_engine(EngineEvent::Closed {
                    terminal_id: terminal_id.clone(),
                    exit_status,
                    signal,
                    reason,
                })?;
                self.close_pane(&terminal_id, exit_status, signal, reason);
                Ok(())
            }
            FrameKind::ResourceSpawned { request_id, result } => {
                self.resource_spawned(request_id, &result);
                Ok(())
            }
            FrameKind::CommandResult { request_id, result } => {
                self.command_result(request_id, result)
            }
            FrameKind::Event {
                terminal, event, ..
            } => self.agent_event(terminal, event),
            frame => self.feed_stream_frame(frame),
        }
    }

    fn feed_stream_frame(&mut self, frame: FrameKind) -> Result<(), ControlError> {
        let event = match frame {
            FrameKind::BootstrapBegin {
                terminal_id,
                stream_id,
                bootstrap_id,
                profile,
                cols,
                rows,
                base_seq,
            } => EngineEvent::BootstrapBegin {
                terminal_id,
                stream_id,
                bootstrap_id,
                profile,
                cols,
                rows,
                base_seq,
            },
            FrameKind::BootstrapChunk {
                terminal_id,
                stream_id,
                bootstrap_id,
                chunk_seq,
                payload,
            } => EngineEvent::BootstrapChunk {
                terminal_id,
                stream_id,
                bootstrap_id,
                chunk_seq,
                payload: payload.to_vec(),
            },
            FrameKind::BootstrapReady {
                terminal_id,
                stream_id,
                bootstrap_id,
                history_cursor,
            } => EngineEvent::BootstrapReady {
                terminal_id,
                stream_id,
                bootstrap_id,
                history_cursor: history_cursor.map(|cursor| cursor.to_vec()),
            },
            FrameKind::BootstrapTombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                reason,
                last_valid_seq,
            } => EngineEvent::Tombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                reason,
                last_valid_seq,
            },
            FrameKind::ResourceOutput {
                terminal_id,
                stream_id,
                bootstrap_id,
                seq,
                bytes,
            } => EngineEvent::Output {
                terminal_id,
                stream_id,
                bootstrap_id,
                seq,
                bytes: bytes.to_vec(),
            },
            frame => return self.feed_history_frame(frame),
        };
        self.apply_engine(event)
    }

    fn feed_history_frame(&mut self, frame: FrameKind) -> Result<(), ControlError> {
        let event = match frame {
            FrameKind::HistoryPage {
                terminal_id,
                stream_id,
                bootstrap_id,
                page_seq,
                cursor,
                next_cursor,
                payload,
                rows,
            } => EngineEvent::HistoryPage {
                terminal_id,
                stream_id,
                bootstrap_id,
                page_seq,
                rows,
                cursor: cursor.to_vec(),
                next_cursor: next_cursor.map(|cursor| cursor.to_vec()),
                payload: payload.to_vec(),
            },
            FrameKind::HistoryTombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor,
                reason,
            } => EngineEvent::HistoryTombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor: cursor.to_vec(),
                reason: history_unavailable_reason(reason)?,
            },
            FrameKind::HistoryRejected {
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor,
                reason,
                required_bytes,
                required_rows,
            } => EngineEvent::HistoryRejected {
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor: cursor.to_vec(),
                reason: history_rejection_reason(reason)?,
                required_bytes,
                required_rows,
            },
            other => {
                self.push_event(Event::Frame(Box::new(other)));
                return Ok(());
            }
        };
        self.apply_engine(event)
    }

    // ----- frames out and events out --------------------------------------

    /// Every encoded frame queued since the last take, in send order.
    pub fn take_outbound(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.outbound)
    }

    /// Every event queued since the last drain, in order, preceded by one
    /// [`Event::TerminalChanged`] per terminal that changed.
    pub fn take_events(&mut self) -> Vec<Event> {
        let mut events: Vec<Event> = self
            .damaged
            .drain(..)
            .map(|terminal_id| Event::TerminalChanged { terminal_id })
            .collect();
        events.append(&mut self.events);
        events
    }

    // ----- the common surface --------------------------------------------

    /// Retarget the session every connection attaches. Returns whether a
    /// reconnect is needed to honor it (a live connection is already
    /// attached elsewhere); the driver then resyncs.
    pub fn attach_session(&mut self, target: AttachTarget) -> bool {
        self.attach_target = Some(target);
        self.handshake_ready
    }

    /// The client's viewport changed: the attached session's terminals and
    /// every per-terminal subscription are resized.
    pub fn resize_viewport(&mut self, cols: u16, rows: u16) {
        let viewport = (cols.max(1), rows.max(1));
        if self.options.viewport == viewport {
            return;
        }
        self.options.viewport = viewport;
        if !self.handshake_ready {
            return;
        }
        self.queue_frame(&FrameKind::ViewportResize {
            viewport: ViewportInfo::new(viewport.0, viewport.1),
        });
        // VIEWPORT_RESIZE reaches only the ATTACHed session's terminals; a
        // per-terminal subscription keeps its geometry without the verb.
        let foreign: Vec<ResourceId> = self.terminal_attached.iter().cloned().collect();
        for terminal_id in foreign {
            self.queue_frame(&FrameKind::ResizeTerminal {
                terminal_id,
                cols: viewport.0,
                rows: viewport.1,
            });
        }
    }

    /// Subscribe to one terminal's stream on the live socket
    /// (`Command::AttachResource`); the reply is [`Event::TerminalAttached`]
    /// with the returned correlation.
    pub fn attach_terminal(&mut self, terminal_id: &ResourceId) -> u32 {
        let request_id = self.next_request_id();
        self.pending
            .insert(request_id, Pending::AttachTerminal(terminal_id.clone()));
        self.terminal_attached.insert(terminal_id.clone());
        let (cols, rows) = self.options.viewport;
        self.queue_frame(&FrameKind::Command {
            request_id,
            command: Command::AttachResource {
                terminal_id: terminal_id.clone(),
                role_policy: self.options.attach_role,
            },
        });
        // ATTACH_RESOURCE does not resize; reflow the terminal to this
        // viewport as a session ATTACH would have.
        self.queue_frame(&FrameKind::ResizeTerminal {
            terminal_id: terminal_id.clone(),
            cols,
            rows,
        });
        request_id
    }

    /// Drop a per-terminal subscription (`Command::DetachResource`); the
    /// reply is [`Event::TerminalDetached`]. Never for a terminal of the
    /// attached session: its stream rides the session pumps.
    pub fn detach_terminal(&mut self, terminal_id: &ResourceId) -> u32 {
        let request_id = self.next_request_id();
        self.pending
            .insert(request_id, Pending::DetachTerminal(terminal_id.clone()));
        self.terminal_attached.remove(terminal_id);
        self.queue_frame(&FrameKind::Command {
            request_id,
            command: Command::DetachResource {
                terminal_id: terminal_id.clone(),
            },
        });
        request_id
    }

    /// Make one terminal's stream live again, preferring a per-terminal
    /// re-attach over a reconnect.
    pub fn ensure_stream(&mut self, terminal_id: &ResourceId) -> StreamRecovery {
        if self
            .engine
            .as_ref()
            .is_some_and(|engine| engine.is_closed(terminal_id))
            || !self.stream_recoveries.insert(terminal_id.clone())
        {
            return StreamRecovery::Noop;
        }
        let pane_session = self
            .topology
            .as_ref()
            .and_then(|topology| topology.pane(terminal_id))
            .map(|pane| pane.session_id);
        let foreign_live = self.status == Status::Attached
            && pane_session.is_some()
            && pane_session != self.attached_session;
        if foreign_live {
            self.attach_terminal(terminal_id);
            StreamRecovery::Attached
        } else {
            StreamRecovery::Reconnect
        }
    }

    /// Spawn a terminal (`SPAWN_RESOURCE`); the reply is
    /// [`Event::TerminalSpawned`] with the returned correlation, and the
    /// server pumps the new terminal's output to this client.
    pub fn spawn_terminal(&mut self, request: SpawnRequest) -> u32 {
        let request_id = self.next_request_id();
        // A spawn into the attached (home) session needs no owner; any
        // other session is addressed through one of its existing panes.
        let owner_terminal = request
            .session_id
            .filter(|session| Some(*session) != self.attached_session)
            .and_then(|session| {
                self.topology
                    .as_ref()
                    .and_then(|topology| topology.first_pane_of(session).cloned())
            });
        self.queue_frame(&FrameKind::SpawnResource {
            request_id,
            group: GroupId::new(1),
            command: request.command,
            cwd: request.cwd,
            env: request.env,
            term: None,
            satellite: None,
            owner_terminal,
            agent_session: None,
            resource: None,
            initial_size: Some(request.initial_size.unwrap_or(self.options.viewport)),
        });
        request_id
    }

    /// Terminate a terminal's process (`Command::KillResource`); the reply
    /// is [`Event::TerminalKilled`], and the close itself arrives as
    /// [`Event::TerminalClosed`].
    pub fn kill_terminal(&mut self, terminal_id: &ResourceId) -> u32 {
        let request_id = self.next_request_id();
        self.pending
            .insert(request_id, Pending::Kill(terminal_id.clone()));
        self.queue_frame(&FrameKind::Command {
            request_id,
            command: Command::KillResource {
                terminal_id: terminal_id.clone(),
                operation_id: None,
            },
        });
        request_id
    }

    /// Close a batch of terminals atomically (`CLOSE_TAB_RESOURCES`);
    /// `None` when the server did not advertise it. The reply is
    /// [`Event::TerminalsClosed`].
    pub fn close_terminals(&mut self, ids: Vec<ResourceId>) -> Option<u32> {
        if !self.server_has(ServerFeature::CloseTabResources) {
            return None;
        }
        let request_id = self.next_request_id();
        self.pending.insert(request_id, Pending::Close);
        self.queue_frame(&FrameKind::Command {
            request_id,
            command: Command::CloseTabResources { ids },
        });
        Some(request_id)
    }

    /// Re-read the session graph on the live socket (`GET_STATE`); the
    /// result is [`Event::TopologyChanged`]. `None` before `HELLO_OK`.
    pub fn refresh_topology(&mut self) -> Option<u32> {
        if !self.handshake_ready {
            return None;
        }
        Some(self.queue_refresh_topology())
    }

    /// Send one key on the raw path. `false` when the terminal is fenced
    /// behind an acknowledged input with unknown delivery.
    pub fn send_key(&mut self, terminal_id: &ResourceId, event: KeyEvent) -> bool {
        if self.delivery_fenced(terminal_id) {
            return false;
        }
        self.queue_frame(&FrameKind::InputKey {
            terminal_id: terminal_id.clone(),
            event,
        });
        true
    }

    /// Type `text` as one key event per scalar on the raw path.
    pub fn send_text(&mut self, terminal_id: &ResourceId, text: &str) -> bool {
        if self.delivery_fenced(terminal_id) {
            return false;
        }
        for event in keys::key_events_for_text(text) {
            self.queue_frame(&FrameKind::InputKey {
                terminal_id: terminal_id.clone(),
                event,
            });
        }
        true
    }

    /// Send a paste as one `INPUT_PASTE` frame; the server brackets it per
    /// the terminal's DEC 2004 state and classifies an untrusted payload.
    pub fn send_paste(
        &mut self,
        terminal_id: &ResourceId,
        data: Vec<u8>,
        trust: PasteTrust,
    ) -> bool {
        if self.delivery_fenced(terminal_id) {
            return false;
        }
        self.queue_frame(&FrameKind::InputPaste {
            terminal_id: terminal_id.clone(),
            event: PasteEvent { trust, data },
        });
        true
    }

    /// Send one mouse event.
    pub fn send_mouse(&mut self, terminal_id: &ResourceId, event: MouseEvent) -> bool {
        if self.delivery_fenced(terminal_id) {
            return false;
        }
        self.queue_frame(&FrameKind::InputMouse {
            terminal_id: terminal_id.clone(),
            event,
        });
        true
    }

    /// Report host focus to the terminal.
    pub fn send_focus(&mut self, terminal_id: &ResourceId, event: FocusEvent) -> bool {
        if self.delivery_fenced(terminal_id) {
            return false;
        }
        self.queue_frame(&FrameKind::InputFocus {
            terminal_id: terminal_id.clone(),
            event,
        });
        true
    }

    /// Atomically deliver a composed line and Enter through the
    /// acknowledged path. Returns the correlation [`Event::InputDelivery`]
    /// resolves.
    pub fn apply_line(&mut self, terminal_id: &ResourceId, text: &str) -> u64 {
        if text.len() > MAX_APPLY_INPUT_COMMAND_BODY - APPLY_LINE_OVERHEAD {
            return self.refuse_acknowledged_input("input exceeds the 64 KiB command limit");
        }
        let events = vec![
            InputEvent::Paste(PasteEvent {
                trust: PasteTrust::Trusted,
                data: text.as_bytes().to_vec(),
            }),
            InputEvent::Key(keys::named(phux_protocol::input::key::PhysicalKey::Enter)),
        ];
        self.begin_acknowledged_input(terminal_id, events)
    }

    /// Atomically deliver one untrusted paste through the acknowledged
    /// path, surfacing the server's safety-policy refusal.
    pub fn apply_paste(&mut self, terminal_id: &ResourceId, text: &str) -> u64 {
        if text.len() > MAX_APPLY_INPUT_COMMAND_BODY - APPLY_PASTE_OVERHEAD {
            return self.refuse_acknowledged_input("input exceeds the 64 KiB command limit");
        }
        let events = vec![InputEvent::Paste(PasteEvent {
            trust: PasteTrust::Untrusted,
            data: text.as_bytes().to_vec(),
        })];
        self.begin_acknowledged_input(terminal_id, events)
    }

    /// Atomically flush a draft and press Tab through the acknowledged
    /// path, so a reconnect can neither reorder nor duplicate them.
    pub fn apply_tab_completion(&mut self, terminal_id: &ResourceId, text: &str) -> u64 {
        if text.len() > MAX_APPLY_INPUT_COMMAND_BODY - APPLY_LINE_OVERHEAD {
            return self.refuse_acknowledged_input("input exceeds the 64 KiB command limit");
        }
        let events = vec![
            InputEvent::Paste(PasteEvent {
                trust: PasteTrust::Untrusted,
                data: text.as_bytes().to_vec(),
            }),
            InputEvent::Key(keys::named(phux_protocol::input::key::PhysicalKey::Tab)),
        ];
        self.begin_acknowledged_input(terminal_id, events)
    }

    /// Whether raw input for the terminal would pass the server's gate
    /// right now: attached, a published replica, no delivery fence, and
    /// either the attached session's terminal or a confirmed per-terminal
    /// subscription.
    #[must_use]
    pub fn input_ready(&self, terminal_id: &ResourceId) -> bool {
        if self.status != Status::Attached || self.delivery_fenced(terminal_id) {
            return false;
        }
        if !self
            .engine
            .as_ref()
            .is_some_and(|engine| engine.input_ready(terminal_id))
        {
            return false;
        }
        self.attach_terminals.contains(terminal_id)
            || self.own_spawns.contains(terminal_id)
            || (self.terminal_attached.contains(terminal_id)
                && !self.pending.values().any(
                    |pending| matches!(pending, Pending::AttachTerminal(id) if id == terminal_id),
                ))
    }

    /// Whether the terminal is fenced behind an acknowledged input whose
    /// delivery is unknown.
    #[must_use]
    pub fn delivery_fenced(&self, terminal_id: &ResourceId) -> bool {
        self.input_replay.delivery_fenced(terminal_id)
    }

    /// Subscribe to the server-wide event stream from `after_seq`
    /// (ADR-0123); the cursor is honored only on a server that advertises
    /// `EVENT_JOURNAL`.
    pub fn subscribe_events(&mut self, after_seq: Option<u64>) {
        self.options.event_after_seq = after_seq;
        if self.handshake_ready {
            let after_seq = after_seq.filter(|_| self.server_has(ServerFeature::EventJournal));
            self.queue_frame(&FrameKind::SubscribeEvents {
                terminal: None,
                after_seq,
            });
        }
    }

    /// Ask the server to end the attach; the session closes when
    /// `DETACHED { Requested }` arrives.
    pub fn detach(&mut self) {
        self.detach_requested = true;
        self.queue_frame(&FrameKind::Detach);
    }

    /// Extension point: send any `COMMAND` and receive its reply as
    /// [`Event::CommandResult`].
    pub fn send_command(&mut self, command: Command) -> u32 {
        let request_id = self.next_request_id();
        self.pending.insert(request_id, Pending::Extension);
        self.queue_frame(&FrameKind::Command {
            request_id,
            command,
        });
        request_id
    }

    /// Extension point: the next correlation id, for a binding that builds
    /// a frame of its own and wants its reply through [`Event::Frame`].
    pub fn next_request_id(&mut self) -> u32 {
        let id = self.request_seq;
        self.request_seq = self.request_seq.wrapping_add(1).max(1);
        id
    }

    /// Extension point: queue any frame, encoded.
    pub fn queue_frame(&mut self, frame: &FrameKind) {
        self.outbound.push(encode(frame));
    }

    // ----- durable input expiry ---------------------------------------

    /// When the earliest queued acknowledged input crosses the retry
    /// horizon; `None` while none is queued. The driver sleeps until then
    /// and calls [`Self::expire_inputs`].
    #[must_use]
    pub fn next_input_deadline(&self) -> Option<Instant> {
        self.input_deadlines.values().min().copied()
    }

    /// Resolve every acknowledged input past the retry horizon. Returns
    /// whether any resolved.
    pub fn expire_inputs(&mut self) -> bool {
        let before = self.events.len();
        let now_ms = self.now_ms();
        let mut request_id = self.request_seq;
        let (reports, frames) = self.input_replay.next_frames_at(&mut request_id, now_ms);
        self.request_seq = request_id;
        self.publish_replay_reports(reports, None);
        for frame in &frames {
            self.queue_frame(frame);
        }
        self.events.len() != before
    }

    // ----- handshake --------------------------------------------------

    fn hello_ok(
        &mut self,
        protocol: (u16, u16, u16),
        server_id: &[u8],
        features: ServerFeatureSet,
        layers: LayerSet,
        profile: BootstrapProfile,
        limits: BootstrapLimits,
    ) -> Result<(), ControlError> {
        if self.handshake_ready {
            return Err(ControlError::Protocol(
                "server sent duplicate HELLO_OK".to_owned(),
            ));
        }
        if let Err(error) = validate_hello_ok(
            &self.offered_caps,
            protocol.0,
            protocol.1,
            protocol.2,
            profile,
            limits,
        ) {
            let message = error.to_string();
            self.error = Some(message.clone());
            return Err(ControlError::Refused(message));
        }
        self.server = Some(ServerInfo {
            id: server_id.to_vec(),
            features,
            layers,
            protocol,
            profile,
            limits,
        });
        self.ensure_engine(profile, limits)?;
        self.handshake_ready = true;
        self.set_status(Status::Negotiated);
        let replay_supported = features.contains(ServerFeature::AcknowledgedInput);
        let now_ms = self.now_ms();
        let reports =
            self.input_replay
                .begin_connection_at(Some(server_id), replay_supported, now_ms);
        self.publish_replay_reports(reports, None);
        self.queue_post_handshake();
        self.queue_durable_frames();
        Ok(())
    }

    fn ensure_engine(
        &mut self,
        profile: BootstrapProfile,
        limits: BootstrapLimits,
    ) -> Result<(), ControlError> {
        let config = EngineConfig {
            profile,
            limits,
            scrollback_lines: self.options.scrollback_lines,
        };
        let same = self
            .engine_config
            .as_ref()
            .is_some_and(|current| current.profile == profile && current.limits == limits);
        if self.engine.is_some() && same {
            return Ok(());
        }
        let engine = EngineHandle::start(
            &config,
            #[cfg(feature = "engine")]
            Arc::clone(&self.publication),
        )
        .map_err(|error| ControlError::Protocol(error.to_string()))?;
        self.engine = Some(engine);
        self.engine_config = Some(config);
        Ok(())
    }

    fn queue_post_handshake(&mut self) {
        let after_seq = self
            .options
            .event_after_seq
            .filter(|_| self.server_has(ServerFeature::EventJournal));
        self.queue_frame(&FrameKind::SubscribeEvents {
            terminal: None,
            after_seq,
        });
        let target = self.attach_target.clone();
        match target {
            Some(target) => {
                let attach_id = self.next_request_id();
                self.active_attach_id = Some(attach_id);
                if let AttachTarget::CreateIfMissing { name, .. } = &target {
                    // Creation is a one-shot act; a reconnect re-attaches.
                    self.attach_target = Some(AttachTarget::ByName(name.clone()));
                }
                let (cols, rows) = self.options.viewport;
                self.queue_frame(&FrameKind::Attach {
                    attach_id,
                    target,
                    viewport: ViewportInfo::new(cols, rows),
                    request_scrollback: true,
                    scrollback_limit_lines: self.options.scrollback_lines,
                    role_policy: self.options.attach_role,
                });
            }
            None => {
                self.queue_refresh_topology();
            }
        }
    }

    fn queue_refresh_topology(&mut self) -> u32 {
        let request_id = self.next_request_id();
        self.pending.insert(request_id, Pending::RefreshTopology);
        self.queue_frame(&FrameKind::Command {
            request_id,
            command: Command::GetState {
                scope: StateScope::Server,
            },
        });
        request_id
    }

    fn attached(&mut self, attach_id: u32, snapshot: &SessionSnapshot) -> Result<(), ControlError> {
        if self.active_attach_id != Some(attach_id) {
            return Err(ControlError::Protocol(format!(
                "ATTACHED used unexpected attach id {attach_id}"
            )));
        }
        // Lifecycle events are not replayed on attach; the snapshot is the
        // floor.
        let vanished = self
            .topology
            .as_ref()
            .map(|topology| topology.vanished(snapshot))
            .unwrap_or_default();
        for terminal_id in vanished {
            self.apply_engine(EngineEvent::closed_unknown(terminal_id.clone()))?;
            self.close_pane(
                &terminal_id,
                None,
                None,
                phux_protocol::wire::frame::CloseReason::Unknown,
            );
        }
        // Only the focused session's Terminal-kind resources take part in
        // the attach barrier: the server bootstraps only that session, and
        // an AgentSession paints nothing.
        let focused_windows: HashSet<_> = snapshot
            .windows
            .iter()
            .filter(|window| window.session_id == snapshot.focused_session)
            .map(|window| window.id)
            .collect();
        let terminals: Vec<ResourceId> = topology::terminal_resources(snapshot)
            .filter(|pane| focused_windows.contains(&pane.window_id))
            .map(|pane| pane.id.clone())
            .collect();
        let mut seen = HashSet::new();
        if !terminals.iter().all(|id| seen.insert(id.clone())) {
            return Err(ControlError::Protocol(
                "ATTACHED target session contains duplicate terminal ids".to_owned(),
            ));
        }
        self.attach_terminals = seen;
        self.attached_session = Some(snapshot.focused_session.get());
        self.topology = Some(Topology::from_snapshot(snapshot));
        self.error = None;
        self.apply_engine(EngineEvent::AttachStarted {
            attach_id,
            terminals: terminals.clone(),
        })?;
        self.agent_streams.clear();
        let agent_sessions: Vec<_> = snapshot
            .resources
            .iter()
            .filter(|resource| resource.kind == ResourceKind::AgentSession)
            .filter(|resource| {
                resource
                    .parent
                    .as_ref()
                    .is_some_and(|parent| terminals.contains(parent))
            })
            .cloned()
            .collect();
        for resource in agent_sessions {
            let facet = resource.agent.as_ref();
            self.apply_engine(EngineEvent::AgentSessionDeclared {
                terminal_id: resource.id.clone(),
                parent: resource.parent.clone(),
                provider: facet.map(|facet| facet.provider.clone()),
                native_id: facet.and_then(|facet| facet.native_id.clone()),
                state: facet.map(|facet| facet.state.clone()),
            })?;
            self.agent_streams.insert(resource.id.clone());
        }
        self.push_event(Event::TopologyChanged);
        Ok(())
    }

    fn attach_ready(&mut self, attach_id: u32) -> Result<(), ControlError> {
        self.apply_engine(EngineEvent::AttachReady { attach_id })?;
        if self.active_attach_id != Some(attach_id) {
            return Err(ControlError::Protocol(format!(
                "ATTACH_READY used unexpected attach id {attach_id}"
            )));
        }
        self.attached_once = true;
        self.error = None;
        self.set_status(Status::Attached);
        self.push_event(Event::Attached { attach_id });
        Ok(())
    }

    fn server_error(
        &mut self,
        request_id: Option<u32>,
        code: ErrorCode,
        message: String,
    ) -> Result<(), ControlError> {
        let rendered = format!("server error {code:?}: {message}");
        self.error = Some(rendered.clone());
        if let Some(request_id) = request_id {
            self.resolve_pending(
                request_id,
                CommandResult::Error {
                    code,
                    message: message.clone(),
                },
            )?;
        }
        self.push_event(Event::ServerError {
            code,
            message,
            request_id,
        });
        if code == ErrorCode::VersionIncompatible {
            return Err(ControlError::Refused(rendered));
        }
        Ok(())
    }

    fn detached(
        &mut self,
        reason: Option<DetachReason>,
        message: &str,
    ) -> Result<(), ControlError> {
        self.push_event(Event::Detached {
            reason,
            message: message.to_owned(),
        });
        // A `None` reason is unstated (an older server) and is never read
        // as Requested.
        if reason == Some(DetachReason::Requested) {
            if self.detach_requested {
                self.close();
                return Err(ControlError::Closed);
            }
            return Ok(());
        }
        let detail = match (reason, message) {
            (Some(reason), "") => reason.describe().to_owned(),
            (Some(reason), extra) => format!("{}: {extra}", reason.describe()),
            (None, "") => "the server ended the attach without saying why".to_owned(),
            (None, extra) => extra.to_owned(),
        };
        self.error = Some(detail.clone());
        if reason == Some(DetachReason::ProtocolError) {
            return Err(ControlError::Refused(detail));
        }
        // Other endings: the server closes the socket itself, and the
        // frames it sent ahead of the close still apply.
        Ok(())
    }

    // ----- lifecycle frames ------------------------------------------------

    fn resource_spawned(&mut self, request_id: u32, result: &SpawnResult) {
        let spawned = result.spawned_id().cloned();
        let error = match result {
            SpawnResult::Err(error) => Some(spawn_error_message(error)),
            _ if spawned.is_none() => Some("unrecognized spawn result".to_owned()),
            _ => None,
        };
        if let Some(id) = &spawned {
            // The server pumps a spawned terminal's output to its spawner
            // and answers before it broadcasts the spawn event, so this set
            // is populated before the foreign-pane pickup examines it.
            self.own_spawns.insert(id.clone());
        }
        self.push_event(Event::TerminalSpawned {
            request_id,
            terminal_id: spawned.clone(),
            error,
        });
        // Nothing else announces this client's own spawn: refresh so the
        // topology lists it.
        if spawned.is_some() && self.handshake_ready {
            self.queue_refresh_topology();
        }
    }

    fn command_result(
        &mut self,
        request_id: u32,
        result: CommandResult,
    ) -> Result<(), ControlError> {
        if self.input_replay.owns(request_id) {
            let code = match &result {
                CommandResult::Error { code, .. } => Some(code.as_wire()),
                _ => None,
            };
            let report = self.input_replay.resolve(request_id, &result);
            self.publish_replay_reports(report.into_iter().collect(), code);
            self.queue_durable_frames();
            return Ok(());
        }
        self.resolve_pending(request_id, result)
    }

    fn resolve_pending(
        &mut self,
        request_id: u32,
        result: CommandResult,
    ) -> Result<(), ControlError> {
        let Some(pending) = self.pending.remove(&request_id) else {
            return Ok(());
        };
        let error = command_result_error(&result);
        match pending {
            Pending::AttachTerminal(terminal_id) => {
                if error.is_some() {
                    self.terminal_attached.remove(&terminal_id);
                    self.stream_recoveries.remove(&terminal_id);
                }
                self.push_event(Event::TerminalAttached {
                    request_id,
                    terminal_id,
                    error,
                });
            }
            Pending::DetachTerminal(terminal_id) => {
                if error.is_none()
                    && let Some(engine) = &self.engine
                {
                    engine.detach(terminal_id.clone());
                }
                self.push_event(Event::TerminalDetached {
                    request_id,
                    terminal_id,
                    error,
                });
            }
            Pending::Kill(terminal_id) => {
                self.push_event(Event::TerminalKilled {
                    request_id,
                    terminal_id,
                    error,
                });
            }
            Pending::Close => {
                self.push_event(Event::TerminalsClosed { request_id, error });
            }
            Pending::RefreshTopology => {
                if let CommandResult::OkWith(CommandValue::State(snapshot)) = result {
                    self.apply_topology_refresh(&snapshot)?;
                }
            }
            Pending::Extension => {
                self.push_event(Event::CommandResult { request_id, result });
            }
        }
        self.queue_durable_frames();
        Ok(())
    }

    /// Apply a `GET_STATE` snapshot on a live connection: terminals that
    /// vanished are closed, but a close already processed is never
    /// resurrected by a snapshot the server built before it.
    fn apply_topology_refresh(&mut self, snapshot: &SessionSnapshot) -> Result<(), ControlError> {
        let vanished = self
            .topology
            .as_ref()
            .map(|topology| topology.vanished(snapshot))
            .unwrap_or_default();
        for terminal_id in vanished {
            self.apply_engine(EngineEvent::closed_unknown(terminal_id.clone()))?;
            self.close_pane(
                &terminal_id,
                None,
                None,
                phux_protocol::wire::frame::CloseReason::Unknown,
            );
        }
        let mut topology = Topology::from_snapshot(snapshot);
        if let Some(engine) = &self.engine {
            topology
                .panes
                .retain(|pane| !engine.is_closed(&pane.terminal_id));
        }
        if self.attached_session.is_none() {
            // A browsing connection has no attach barrier; its first
            // topology is the moment it becomes usable.
            self.attached_once = true;
            self.set_status(Status::Attached);
        }
        self.error = None;
        self.topology = Some(topology);
        self.push_event(Event::TopologyChanged);
        Ok(())
    }

    fn agent_event(
        &mut self,
        terminal: Option<ResourceId>,
        event: AgentEvent,
    ) -> Result<(), ControlError> {
        // Server-scoped events carry no terminal; the server always scopes
        // the kinds this build folds.
        let Some(terminal_id) = terminal else {
            return Ok(());
        };
        if let AgentEvent::ResourceSpawned {
            kind: ResourceKind::Terminal,
            ..
        } = &event
            && self.options.auto_attach_foreign_spawns
        {
            self.pick_up_foreign_spawn(&terminal_id);
        }
        // The kernel folds the process-exit bookkeeping (a retained
        // resource, ADR-0124) for terminals it knows; every other kind is
        // projected here directly.
        if self.kernel_knows(&terminal_id)
            && let Some(engine) = &self.engine
        {
            match engine.apply(EngineEvent::Agent {
                terminal_id: terminal_id.clone(),
                event: event.clone(),
            }) {
                Ok(outcome) => self.process_outcome(outcome, false)?,
                Err(error) => return Err(ControlError::Protocol(error.to_string())),
            }
        }
        self.fold_agent_event(terminal_id, event);
        Ok(())
    }

    fn kernel_knows(&self, terminal_id: &ResourceId) -> bool {
        self.attach_terminals.contains(terminal_id)
            || self.own_spawns.contains(terminal_id)
            || self.terminal_attached.contains(terminal_id)
            || self.agent_streams.contains(terminal_id)
    }

    /// A terminal spawned by another client has no output pump on this
    /// connection: attach it on the live socket and refresh the topology
    /// so it gets its window and session context.
    fn pick_up_foreign_spawn(&mut self, terminal_id: &ResourceId) {
        let already_admitted = self.own_spawns.contains(terminal_id)
            || self.terminal_attached.contains(terminal_id)
            || self.engine.as_ref().is_some_and(|engine| {
                engine.has_projection(terminal_id) || engine.is_closed(terminal_id)
            });
        if already_admitted {
            return;
        }
        self.attach_terminal(terminal_id);
        self.queue_refresh_topology();
    }

    fn fold_agent_event(&mut self, terminal_id: ResourceId, event: AgentEvent) {
        match event {
            AgentEvent::Bell => self.push_event(Event::Bell { terminal_id }),
            AgentEvent::TitleChanged { title } => {
                if let Some(pane) = self
                    .topology
                    .as_mut()
                    .and_then(|topology| topology.pane_mut(&terminal_id))
                {
                    pane.title = Some(title.clone());
                }
                self.push_event(Event::TitleChanged { terminal_id, title });
            }
            AgentEvent::ResourceSpawned {
                kind: ResourceKind::Terminal,
                ..
            } => self.push_event(Event::PaneSpawned { terminal_id }),
            AgentEvent::ResourceClosed { exit_status } => self.close_pane(
                &terminal_id,
                exit_status,
                None,
                phux_protocol::wire::frame::CloseReason::Unknown,
            ),
            AgentEvent::Dirty => self.push_event(Event::OutputStarted { terminal_id }),
            AgentEvent::Idle => self.push_event(Event::OutputSettled { terminal_id }),
            AgentEvent::Asked {
                id,
                question,
                suggestions,
                elapsed_seconds,
            } => self.push_event(Event::AgentAsked {
                terminal_id,
                question_id: id,
                text: question,
                suggestions,
                waiting_seconds: elapsed_seconds,
            }),
            AgentEvent::CommandStarted => self.push_event(Event::CommandStarted { terminal_id }),
            AgentEvent::CommandFinished { exit_code } => self.push_event(Event::CommandFinished {
                terminal_id,
                exit_code,
            }),
            AgentEvent::CwdChanged { cwd } => {
                if let Some(pane) = self
                    .topology
                    .as_mut()
                    .and_then(|topology| topology.pane_mut(&terminal_id))
                {
                    pane.cwd = Some(cwd.clone());
                }
                self.push_event(Event::CwdChanged { terminal_id, cwd });
            }
            // Supervisory, unknown, and non-Terminal spawns: forward-compat
            // skip.
            _ => {}
        }
    }

    /// Apply a terminal's closure to the topology and correlations.
    /// Idempotent: the first application removes the entry.
    fn close_pane(
        &mut self,
        terminal_id: &ResourceId,
        exit_status: Option<i32>,
        signal: Option<i32>,
        reason: phux_protocol::wire::frame::CloseReason,
    ) {
        let was_known = self.own_spawns.contains(terminal_id)
            || self.terminal_attached.contains(terminal_id)
            || self
                .topology
                .as_ref()
                .is_some_and(|topology| topology.pane(terminal_id).is_some());
        self.terminal_attached.remove(terminal_id);
        self.stream_recoveries.remove(terminal_id);
        self.own_spawns.remove(terminal_id);
        self.agent_streams.remove(terminal_id);
        if let Some(topology) = self.topology.as_mut() {
            topology
                .panes
                .retain(|pane| &pane.terminal_id != terminal_id);
        }
        let reports = self
            .input_replay
            .retire_terminal(terminal_id, "terminal closed");
        self.publish_replay_reports(reports, None);
        if was_known {
            self.push_event(Event::TerminalClosed {
                terminal_id: terminal_id.clone(),
                exit_status,
                signal,
                reason,
            });
        }
    }

    // ----- the kernel --------------------------------------------------

    fn apply_engine(&mut self, event: EngineEvent) -> Result<(), ControlError> {
        let Some(engine) = &self.engine else {
            return Err(ControlError::Protocol(
                "stateful frame arrived before the session kernel was initialized".to_owned(),
            ));
        };
        // A frame for a terminal the kernel already closed is stale
        // evidence, not an error; only a close itself is idempotent there.
        if !matches!(event, EngineEvent::Closed { .. })
            && event
                .terminal_id()
                .is_some_and(|terminal_id| engine.is_closed(terminal_id))
        {
            return Ok(());
        }
        let outcome = engine
            .apply(event)
            .map_err(|error| ControlError::Protocol(error.to_string()))?;
        self.process_outcome(outcome, true)
    }

    /// Execute every declarative effect before considering the update
    /// result: a codec error can require an acknowledgement and a resync in
    /// the same outcome.
    fn process_outcome(
        &mut self,
        outcome: EngineOutcome,
        strict: bool,
    ) -> Result<(), ControlError> {
        let resync = outcome.resync_required();
        for effect in outcome.effects {
            self.process_effect(effect);
        }
        if resync {
            return Err(ControlError::Resync);
        }
        if let Some(error) = outcome.error {
            if strict {
                return Err(ControlError::Protocol(format!("session kernel: {error}")));
            }
            tracing::debug!(%error, "session kernel ignored an event");
        }
        Ok(())
    }

    fn process_effect(&mut self, effect: KernelEffect) {
        match effect {
            KernelEffect::Send(send) => self.process_send(send),
            KernelEffect::Damage(damage) => {
                if damage.kind != phux_client_core::session::KernelDamageKind::Removed {
                    self.stream_recoveries.remove(&damage.terminal_id);
                    if !self.damaged.contains(&damage.terminal_id) {
                        self.damaged.push(damage.terminal_id);
                    }
                }
            }
            KernelEffect::Status(status) => self.process_status(status),
            KernelEffect::Job(_) => {}
            KernelEffect::AgentRecords {
                terminal_id,
                records,
            } => self.push_event(Event::AgentRecords {
                terminal_id,
                records,
            }),
        }
    }

    fn process_send(&mut self, send: KernelSend) {
        let frame = match send {
            KernelSend::Input { terminal_id, event } => match event {
                InputEvent::Key(event) => FrameKind::InputKey { terminal_id, event },
                InputEvent::Mouse(event) => FrameKind::InputMouse { terminal_id, event },
                InputEvent::Focus(event) => FrameKind::InputFocus { terminal_id, event },
                InputEvent::Paste(event) => FrameKind::InputPaste { terminal_id, event },
                _ => return,
            },
            KernelSend::PtyWrite { terminal_id, bytes } => {
                if !self.server_has(ServerFeature::TerminalReply) {
                    tracing::warn!(
                        "terminal query reply not sent: server lacks terminal-reply support"
                    );
                    return;
                }
                if bytes.is_empty() || bytes.len() > MAX_INPUT_TERMINAL_REPLY_BYTES {
                    tracing::warn!("terminal reply is empty or exceeds the protocol byte limit");
                    return;
                }
                FrameKind::InputTerminalReply {
                    terminal_id,
                    bytes: bytes.into(),
                }
            }
            KernelSend::FrameAck {
                terminal_id,
                stream_id,
                bootstrap_id,
                seq,
            } => FrameKind::FrameAck {
                terminal_id,
                stream_id,
                bootstrap_id,
                seq,
            },
            KernelSend::HistoryRequest {
                key,
                cursor,
                max_bytes,
                max_rows,
            } => FrameKind::HistoryRequest {
                terminal_id: key.terminal_id,
                stream_id: key.stream_id,
                bootstrap_id: key.bootstrap_id,
                cursor: cursor.into(),
                max_bytes,
                max_rows,
            },
            // The kernel asks for the event stream on every attach release;
            // the handshake already subscribed, so this is a harmless
            // re-subscribe carrying the consumer's cursor.
            KernelSend::SubscribeEvents {
                terminal,
                after_seq,
            } => FrameKind::SubscribeEvents {
                terminal,
                after_seq: after_seq
                    .or(self.options.event_after_seq)
                    .filter(|_| self.server_has(ServerFeature::EventJournal)),
            },
        };
        self.queue_frame(&frame);
    }

    fn process_status(&mut self, status: KernelStatus) {
        match status {
            KernelStatus::Engine { key, status } => match status {
                phux_client_core::engine::EngineStatus::Bell => self.push_event(Event::Bell {
                    terminal_id: key.terminal_id,
                }),
                phux_client_core::engine::EngineStatus::Title(title) => {
                    if let Some(pane) = self
                        .topology
                        .as_mut()
                        .and_then(|topology| topology.pane_mut(&key.terminal_id))
                    {
                        pane.title = Some(title.clone());
                    }
                    self.push_event(Event::TitleChanged {
                        terminal_id: key.terminal_id,
                        title,
                    });
                }
            },
            KernelStatus::ResyncRequired {
                terminal_id,
                reason,
                ..
            } => self.push_event(Event::ResyncRequired {
                terminal_id,
                reason,
            }),
            KernelStatus::History { key, status } => self.push_event(Event::History {
                terminal_id: key.terminal_id,
                status,
            }),
            KernelStatus::HistoryUnavailable { key, reason } => {
                self.push_event(Event::HistoryUnavailable {
                    terminal_id: key.terminal_id,
                    reason,
                });
            }
            KernelStatus::Exited {
                terminal_id,
                exit_status,
                signal,
                reason,
            } => self.push_event(Event::Exited {
                terminal_id,
                exit_status,
                signal,
                reason,
            }),
            // Folded from the agent event itself, for every terminal the
            // subscription covers rather than only the kernel's.
            KernelStatus::Cwd { .. }
            | KernelStatus::CommandStarted { .. }
            | KernelStatus::CommandFinished { .. } => {}
        }
    }

    // ----- acknowledged input --------------------------------------------

    fn refuse_acknowledged_input(&mut self, message: &str) -> u64 {
        let delivery_id = self.next_delivery_id();
        self.push_event(Event::InputDelivery {
            delivery_id,
            outcome: DeliveryOutcome::Refused,
            code: Some(ErrorCode::InvalidCommand.as_wire()),
            message: message.to_owned(),
        });
        delivery_id
    }

    fn begin_acknowledged_input(
        &mut self,
        terminal_id: &ResourceId,
        events: Vec<InputEvent>,
    ) -> u64 {
        let delivery_id = self.next_delivery_id();
        if matches!(terminal_id, ResourceId::Satellite { .. }) {
            self.push_event(Event::InputDelivery {
                delivery_id,
                outcome: DeliveryOutcome::Refused,
                code: Some(ErrorCode::UnsupportedSatelliteRoute.as_wire()),
                message: "acknowledged input is local-only".to_owned(),
            });
            return delivery_id;
        }
        let operation_id = new_operation_id();
        let key = operation_id_hex(&operation_id);
        let now_ms = self.now_ms();
        self.input_delivery_ids
            .insert(key.clone(), (delivery_id, terminal_id.clone()));
        match self
            .input_replay
            .submit_at(operation_id, terminal_id.clone(), events, now_ms)
        {
            Ok(()) => {
                self.input_deadlines
                    .insert(key, Instant::now() + INPUT_RETRY_HORIZON);
                self.start_input_replay_if_needed(now_ms);
                self.queue_durable_frames();
            }
            Err(report) => self.publish_replay_reports(vec![report], None),
        }
        delivery_id
    }

    fn start_input_replay_if_needed(&mut self, now_ms: u64) {
        if !self.handshake_ready || self.input_replay.active() {
            return;
        }
        let Some(server) = &self.server else {
            return;
        };
        let acknowledged = server.has(ServerFeature::AcknowledgedInput);
        let server_id = server.id.clone();
        let reports = self
            .input_replay
            .begin_connection_at(Some(&server_id), acknowledged, now_ms);
        self.publish_replay_reports(reports, None);
    }

    /// Build the next serialized `APPLY_INPUT` attempt per terminal.
    fn queue_durable_frames(&mut self) {
        if !self.handshake_ready {
            return;
        }
        let now_ms = self.now_ms();
        let mut request_id = self.request_seq;
        let (reports, frames) = self.input_replay.next_frames_at(&mut request_id, now_ms);
        self.request_seq = request_id;
        self.publish_replay_reports(reports, None);
        for frame in &frames {
            self.queue_frame(frame);
        }
    }

    fn publish_replay_reports(&mut self, reports: Vec<ReplayReport>, code: Option<u16>) {
        for report in reports {
            let Some((delivery_id, terminal_id)) =
                self.input_delivery_ids.remove(&report.operation_id)
            else {
                continue;
            };
            self.input_deadlines.remove(&report.operation_id);
            let outcome = match report.disposition {
                ReplayDisposition::Delivered => DeliveryOutcome::Delivered,
                ReplayDisposition::Refused => DeliveryOutcome::Refused,
                ReplayDisposition::Unknown => {
                    // Damage that predates this ambiguity cannot prove what
                    // the server rendered after it.
                    self.damaged.retain(|id| id != &terminal_id);
                    DeliveryOutcome::Unknown
                }
            };
            self.push_event(Event::InputDelivery {
                delivery_id,
                outcome,
                code,
                message: report.message,
            });
        }
    }

    fn strand_durable(&mut self, message: &str) {
        let reports = self.input_replay.drain_unresolved(message);
        self.publish_replay_reports(reports, None);
    }

    /// Resolve every outstanding correlation as failed: the socket that
    /// carried it is gone. Topology refreshes drop silently, as a fresh
    /// `ATTACHED` is on its way.
    fn fail_pending(&mut self, message: &str) {
        let pending: Vec<(u32, Pending)> = self.pending.drain().collect();
        for (request_id, pending) in pending {
            match pending {
                Pending::AttachTerminal(terminal_id) => {
                    self.push_event(Event::TerminalAttached {
                        request_id,
                        terminal_id,
                        error: Some(message.to_owned()),
                    });
                }
                Pending::DetachTerminal(terminal_id) => {
                    self.push_event(Event::TerminalDetached {
                        request_id,
                        terminal_id,
                        error: Some(message.to_owned()),
                    });
                }
                Pending::Kill(terminal_id) => {
                    self.push_event(Event::TerminalKilled {
                        request_id,
                        terminal_id,
                        error: Some(message.to_owned()),
                    });
                }
                Pending::Close => {
                    self.push_event(Event::TerminalsClosed {
                        request_id,
                        error: Some(message.to_owned()),
                    });
                }
                Pending::RefreshTopology => {}
                Pending::Extension => {
                    self.push_event(Event::CommandResult {
                        request_id,
                        result: CommandResult::Error {
                            code: ErrorCode::InvalidCommand,
                            message: message.to_owned(),
                        },
                    });
                }
            }
        }
    }

    // ----- small helpers -----------------------------------------------

    fn server_has(&self, feature: ServerFeature) -> bool {
        self.server
            .as_ref()
            .is_some_and(|server| server.has(feature))
    }

    fn set_status(&mut self, status: Status) {
        if self.status == status {
            return;
        }
        self.status = status;
        self.push_event(Event::StatusChanged(status));
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.clock_origin.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn next_delivery_id(&mut self) -> u64 {
        let id = self.input_delivery_seq.max(1);
        self.input_delivery_seq = id.wrapping_add(1).max(1);
        id
    }

    fn push_event(&mut self, event: Event) {
        if self.events.len() >= EVENT_QUEUE_CAP {
            self.events.retain(Event::is_lossless);
            self.events.push(Event::TopologyChanged);
        }
        self.events.push(event);
    }
}

/// Encode one frame as SPEC section 5 bytes.
#[must_use]
pub fn encode(frame: &FrameKind) -> Vec<u8> {
    let mut encoded = BytesMut::new();
    frame.encode(&mut encoded);
    encoded.to_vec()
}

fn new_operation_id() -> InputOperationId {
    loop {
        if let Some(id) = InputOperationId::new(uuid::Uuid::new_v4().into_bytes()) {
            return id;
        }
    }
}

fn operation_id_hex(operation_id: &InputOperationId) -> String {
    use std::fmt::Write as _;
    operation_id
        .as_bytes()
        .iter()
        .fold(String::with_capacity(32), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

fn command_result_error(result: &CommandResult) -> Option<String> {
    match result {
        CommandResult::Ok | CommandResult::OkWith(_) => None,
        CommandResult::Error { code, message } => Some(format!("{code:?}: {message}")),
        _ => Some("unrecognized command result".to_owned()),
    }
}

fn spawn_error_message(error: &phux_protocol::wire::frame::SpawnError) -> String {
    use phux_protocol::wire::frame::SpawnError;
    match error {
        SpawnError::GroupNotFound => "the server has no such group".to_owned(),
        SpawnError::SpawnFailed(message) | SpawnError::SatelliteUnreachable(message) => {
            message.clone()
        }
        SpawnError::UnsupportedSatelliteRoute => {
            "the server cannot route the spawn to that satellite".to_owned()
        }
        _ => "spawn failed".to_owned(),
    }
}

fn history_unavailable_reason(
    reason: WireTombstone,
) -> Result<HistoryUnavailableReason, ControlError> {
    Ok(match reason {
        WireTombstone::Stale => HistoryUnavailableReason::Stale,
        WireTombstone::Pruned => HistoryUnavailableReason::Pruned,
        WireTombstone::Reset => HistoryUnavailableReason::Reset,
        WireTombstone::Resize => HistoryUnavailableReason::Resize,
        WireTombstone::Expired => HistoryUnavailableReason::Expired,
        WireTombstone::Released => HistoryUnavailableReason::Released,
        WireTombstone::Limit => HistoryUnavailableReason::Limit,
        WireTombstone::CodecFailure => HistoryUnavailableReason::CodecFailure,
        _ => {
            return Err(ControlError::Protocol(
                "unsupported history tombstone reason".to_owned(),
            ));
        }
    })
}

fn history_rejection_reason(reason: WireRejection) -> Result<HistoryRejectionReason, ControlError> {
    Ok(match reason {
        WireRejection::ZeroLimit => HistoryRejectionReason::ZeroLimit,
        WireRejection::TooSmall => HistoryRejectionReason::TooSmall,
        WireRejection::Busy => HistoryRejectionReason::Busy,
        _ => {
            return Err(ControlError::Protocol(
                "unsupported history rejection reason".to_owned(),
            ));
        }
    })
}

/// The retry-horizon bound a driver may sleep against when nothing is
/// queued, so a wake never waits longer than one horizon.
#[must_use]
pub const fn max_expiry_wait() -> Duration {
    INPUT_RETRY_HORIZON
}
