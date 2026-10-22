//! The sans-IO control plane (ADR-0133 decision 1).
//!
//! A [`ControlPlane`] is a synchronous state machine over the session
//! kernel: decoded frames go in through [`ControlPlane::feed`] (or bytes
//! through [`ControlPlane::feed_bytes`]), encoded frames come out of
//! [`ControlPlane::take_outbound`] and owned [`Event`]s out of
//! [`ControlPlane::take_events`]. It never touches a socket or a clock it
//! is not handed.
//!
//! It owns the connection lifecycle, topology, per-terminal operations,
//! raw and acknowledged input, and event subscriptions; terminal state is
//! the kernel's, on the [`engine`](crate::engine) owner thread.
//! [`ControlPlane::send_command`], [`ControlPlane::queue_frame`] and
//! [`Event::Frame`] let a binding add a lane without a second state machine.

use std::collections::{HashMap, HashSet};
#[cfg(feature = "engine")]
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::{Bytes, BytesMut};
use phux_client_core::history::HistoryCacheConfig;
use phux_client_core::input_replay::{
    INPUT_RETRY_HORIZON, InputReplayJournal, ReplayDisposition, ReplayReport,
};
use phux_client_core::session::{
    HistoryRejectionReason, HistoryUnavailableReason, KernelEffect, KernelSend, KernelStatus,
};
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, ClientCapabilities, ServerFeature, ServerFeatureExt,
};
use phux_protocol::ids::{ClientId, GroupId, InputOperationId, ResourceId, ResourceKind};
use phux_protocol::input::InputEvent;
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::input::key::KeyEvent;
use phux_protocol::input::mouse::MouseEvent;
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::wire::frame::{
    AgentEvent, AttachTarget, Command, CommandResult, CommandValue, DetachReason, ErrorCode,
    FrameKind, HistoryRejectionReason as WireRejection, HistoryTombstoneReason as WireTombstone,
    MAX_APPLY_INPUT_COMMAND_BODY, RolePolicy, SpawnResult, StateScope, ViewportInfo,
};
use phux_protocol::wire::info::SessionSnapshot;

use crate::engine::{EngineApplyError, EngineConfig, EngineEvent, EngineHandle, EngineOutcome};
#[cfg(feature = "engine")]
use crate::engine::{EngineError, Scroll};
pub use crate::handshake::ServerInfo;
#[cfg(feature = "engine")]
use crate::publication::Publication;

mod agent_delivery;
mod agents;
mod approvals;
mod checkout;
mod commands;
mod error;
mod events;
mod extensions;
mod frames;
mod geometry;
mod global_metadata;
mod input;
mod input_delivery;
mod kernel;
pub mod keys;
mod lifecycle;
mod native_agent;
mod options;
mod org_plane;
pub mod organization;
mod responses;
mod roster;
mod service_http;
mod state;
mod topology;

pub use checkout::{CheckoutFailure, CheckoutOutcome, CheckoutResponse, MAX_CHECKOUT_ANSWERS};
pub use error::ControlError;
pub use events::{
    AgentPublication, AgentRecordMode, AgentSessionInfo, DeliveryOutcome, Event, Observation,
    Status,
};
pub use extensions::{
    DirectoryChild, DirectoryFailure, DirectoryListing, FileUploadOutcome, FileUploadReceipt,
    MAX_PATH_ANSWERS, PathAnswer, PathFailure, PathMatch, PathMatchKind, PathSearchStatus,
    TranscribeOutcome, TranscribeReceipt,
};
pub use geometry::TerminalResizeOutcome;
pub use global_metadata::{GlobalMetadata, MAX_WATCHED_GLOBAL_KEYS};
pub use input::mint_input_operation_id;
pub use options::{
    ControlOptions, InboundDelivery, MAX_QUEUED_INBOUND_BYTES, MAX_QUEUED_INBOUND_FRAMES,
    SessionRecovery,
};
pub use org_plane::{ArrangementSubmitError, mint_operation};
pub use organization::{InboxView, IntakeView, OrganizationReplica, ReplicaAction, ReplicaState};
pub use service_http::{MAX_SERVICE_HTTP_IN_FLIGHT, ServiceHttpCall, ServiceHttpError};
pub use topology::{AgentSessionDescriptor, PaneDescriptor, SessionDescriptor, Topology};

/// Upper bound on undrained events.
///
/// A wedged consumer must not grow the queue without bound. On overflow the
/// droppable events go and a fresh `TopologyChanged` is queued, so a
/// consumer re-derives from the topology, which is authoritative; lossless
/// events (see [`Event::is_lossless`]) stay.
pub const EVENT_QUEUE_CAP: usize = 4096;

/// Maximum retained clipboard payload capacity in the undrained event queue.
/// Clipboard events are droppable, never replayed to recover an overflow.
pub const CLIPBOARD_QUEUE_BYTES: usize = 4 * 1024 * 1024;

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

/// Identifies the exact unknown-delivery fence a presentation can acknowledge.
///
/// Capture this with the authoritative projection being handed to the user;
/// acknowledging an older token cannot clear a newer ambiguity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectionFence {
    /// Connection epoch of the presented projection.
    pub connection_epoch: u64,
    /// Correlation of the latest unknown-delivery receipt for this terminal.
    pub delivery_id: u64,
}

/// What one outstanding `COMMAND` asked for, so its reply resolves to the
/// right event.
#[derive(Debug, Clone)]
enum Pending {
    Spawn {
        replay: Option<FrameKind>,
        needs_replay: bool,
    },
    AttachTerminal(ResourceId),
    DetachTerminal {
        terminal_id: ResourceId,
        /// Admission withdrawn eagerly, but restored if the server refuses.
        was_attached: bool,
    },
    Kill(ResourceId),
    Close,
    NativeAgent {
        agent_session_id: ResourceId,
        operation_id: u64,
    },
    RefreshTopology,
    /// A binding's own command; the reply is surfaced raw.
    Extension,
    /// One chunk of a runtime-owned durable upload.
    PutFile(u64),
    /// A one-shot transcription request for a completed upload.
    Transcribe(u64),
    /// A checkout method on this Terminal's checkout.
    Checkout(ResourceId),
    /// A host-service HTTP command; its answer resolves a waiting call.
    ServiceHttp,
    /// The runtime's own subscription to an `AgentSession` stream.
    AgentSubscription(ResourceId),
    /// The runtime's release of an `AgentSession` stream whose pane it
    /// stopped streaming; the answer changes nothing.
    AgentRelease,
    /// One `GET_ORGANIZATION` page of a seed or a session refetch.
    OrgPage(phux_protocol::wire::frame::OrgFilter),
    /// One `ARRANGEMENT_GET` into the replica.
    OrgArrangement(phux_protocol::ids::ArrangementKey),
}

/// The sans-IO control plane. See the module docs.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent per-connection lifecycle facts (handshake, first attach, detach asked, attach ended), not one state machine"
)]
#[derive(Debug)]
pub struct ControlPlane {
    options: ControlOptions,
    viewport: geometry::ViewportIntent,
    /// Bumped by every `connection_opened`. A consumer that fences
    /// per-connection state on its own client's identity uses this instead
    /// once the runtime owns the reconnect.
    connection_epoch: u64,
    /// The epoch authorized to deliver inbound frames; absent after loss.
    live_connection_epoch: Option<u64>,
    /// Frames read off the socket and not yet drained, under
    /// [`InboundDelivery::Queued`].
    inbound: Vec<Bytes>,
    inbound_bytes: usize,
    offered_caps: ClientCapabilities,
    status: Status,
    handshake_ready: bool,
    attached_once: bool,
    detach_requested: bool,
    /// The server sent `DETACHED` on this connection: the attach is over,
    /// whatever the transport does next, so no input is admitted until a new
    /// attach or connection.
    attach_ended: bool,
    error: Option<String>,
    server: Option<ServerInfo>,
    engine: Option<EngineHandle>,
    engine_config: Option<EngineConfig>,
    history_config: Option<HistoryCacheConfig>,
    terminal_theme: Option<crate::engine::TerminalTheme>,
    #[cfg(feature = "engine")]
    publication: Arc<Publication>,
    topology: Option<Topology>,
    /// The Terminals each snapshot view last listed.
    listings: topology::Listings,
    /// The target every connection attaches; a `CreateIfMissing` becomes
    /// `ByName` once sent, then incarnation-scoped `ById` once confirmed,
    /// so reconnect follows renames and never re-creates.
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
    /// Opt-in subscription intent retained across transport reconnects.
    preserve_terminal_geometry: HashSet<ResourceId>,
    /// Roles declared by manual per-terminal attaches, overriding the session role.
    terminal_roles: HashMap<ResourceId, RolePolicy>,
    /// Resources bootstrapped on this connection, excluding retained old frames.
    geometry_bootstrapped: HashSet<ResourceId>,
    /// Terminals whose replacement snapshot is requested but not published.
    stream_recoveries: HashSet<ResourceId>,
    /// Terminals this client's own spawns created on this connection.
    own_spawns: HashSet<ResourceId>,
    /// `AgentSession` resources declared to the kernel.
    agent_streams: HashSet<ResourceId>,
    /// The `AgentSession` resources the last topology listed, kept for
    /// subscribing one whose parent this connection admits later.
    agent_catalog: Vec<phux_protocol::wire::info::ResourceInfo>,
    /// The `AgentSession` streams `subscribe_agent_sessions` follows, kept
    /// across reconnects until the server proves them closed.
    agent_subscriptions: HashMap<ResourceId, agents::AgentSubscription>,
    /// `AgentSession` streams that ended on this server incarnation: a
    /// later close the server sends for one is not a pane closing. Ids are
    /// unique within an incarnation, so a restart clears it.
    retired_agents: HashSet<ResourceId>,
    /// Undrained committed agent publications and close stamps.
    agent_delivery: agent_delivery::AgentDelivery,
    /// This connection's identity on the server, from `ATTACHED`; what an
    /// input lease's `holder` is compared against.
    own_client_id: Option<ClientId>,
    pending: HashMap<u32, Pending>,
    request_seq: u32,
    input_replay: InputReplayJournal,
    delivery_fences: HashMap<ResourceId, u64>,
    input_delivery_ids: HashMap<String, (u64, ResourceId)>,
    input_deadlines: HashMap<String, Instant>,
    input_delivery_seq: u64,
    /// The question id each pane is asking per the event stream: set by
    /// `AgentAsked`, cleared by the `asked: false` level and by pane close.
    /// The fallback [`ControlPlane::apply_answer`] guards with when the
    /// pane's title carries no ADR-0035 sentinel (a hook-reported ask).
    live_asks: HashMap<ResourceId, String>,
    /// Each terminal's command state as this connection's shell events
    /// report it: `true` after a start (OSC 133 C), `false` after a finish
    /// (OSC 133 D) or the shell's exit. Absent means no event yet, so the
    /// replica's marks answer instead; see [`ControlPlane::command_state`].
    command_states: HashMap<ResourceId, bool>,
    clock_origin: Instant,
    outbound: Vec<Vec<u8>>,
    events: Vec<Event>,
    clipboard_event_bytes: usize,
    clipboard_focus: Option<ResourceId>,
    /// How many events the queue cap dropped since the last drain.
    events_dropped: usize,
    damaged: Vec<ResourceId>,
    extensions: extensions::Extensions,
    roster: roster::RosterRecovery,
    /// In-flight `GET` of `phux.session.project/v1`.
    project_tag_pending: Option<u32>,
    /// `Global` keys a binding asked to watch.
    global_watch: global_metadata::GlobalWatch,
    /// The organization replica (`ORGANIZATION` servers only).
    organization: organization::OrganizationReplica,
    /// The organization operation journal and close batches.
    org: org_plane::OrgLane,
    /// The opt-in projection of server-held approvals (ADR-0128).
    approvals: approvals::ApprovalWatch,
}

impl Drop for ControlPlane {
    fn drop(&mut self) {
        if let Some(engine) = &self.engine {
            engine.stop();
        }
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
