//! Protocol-0.7 web connection over the shared synchronous client kernel.
//!
//! A [`Connection`] is one attach: one wire connection showing one server
//! session's terminals. The session is the server's record; this type is the
//! browser's view of it.
//!
//! Transport framing stays here; replica lifecycle, generation validation,
//! ordering, READY fences, and input eligibility stay in `phux-client-core`.

use std::collections::HashMap;
use std::rc::Rc;

use crate::panes::{Axis, Layout, PaneRect};
use bytes::{Bytes, BytesMut};
use phux_client_core::engine::{
    BootstrapProgress, CanonicalGeometry, EngineAdapter, EngineDamage, EngineEffect,
    EngineEffectBuffer, EngineStatus, HistoryApplyOutcome,
};
use phux_client_core::handshake::{HelloOkError, validate_hello_ok};
use phux_client_core::history::HistoryCacheConfig;
use phux_client_core::session::{
    AgentSessionDeclaration, EffectBuffer, HistoryRejectionReason as KernelHistoryRejectionReason,
    HistoryUnavailableReason, InputEligibility, KernelAction, KernelEffect, KernelInput,
    KernelSend, KernelStatus, SessionKernel,
};
use phux_protocol::caps::{
    BootstrapCapabilities, BootstrapLimits, BootstrapProfile, BootstrapProfileKind,
    BootstrapProfileSet, BootstrapStreamProfile, ClientCapabilities, EngineCodec, EngineFeatureSet,
    ImageProtocolSet, ServerFeature, ServerFeatureExt,
};
use phux_protocol::ids::{GroupId, OperationId, ResourceId, SessionKey};
use phux_protocol::input::InputEvent;
use phux_protocol::input::key::KeyEvent;
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::wire::frame::{
    AgentEvent, AttachTarget, Command, CommandResult, CommandValue, ErrorCode, FrameKind,
    HistoryRejectionReason, HistoryTombstoneReason, LiveTarget, OrgFilter, OrganizationPage,
    PathQueryResult, PathResults, SpawnResult, TerminateMode, ViewportInfo,
};
use phux_protocol::{PROTOCOL_VERSION, ResourceKind, Version};
use phux_vt_web::{Grid, NativeCodecError, NativeDecoder, Terminal, Vt};

mod attach;
mod engine;
mod frames;
mod pane_replies;
mod panes;
mod path_query;
mod resources;
mod view;

pub use attach::AttachPreference;
use attach::Discovery;
use engine::WebEngine;

const ATTACH_ID: u32 = 1;
const HISTORY_LINES: u32 = 5_000;
/// Staged READY prefix and parser-continuation bound, as native clients use.
const NATIVE_DECODER_LIMIT: usize = 64 * 1024 * 1024;

/// The kernel's reason for a history tombstone. A reason this build does
/// not know (a newer server's) retires the cursor as `Released`.
const fn history_unavailable_reason(reason: HistoryTombstoneReason) -> HistoryUnavailableReason {
    match reason {
        HistoryTombstoneReason::Stale => HistoryUnavailableReason::Stale,
        HistoryTombstoneReason::Pruned => HistoryUnavailableReason::Pruned,
        HistoryTombstoneReason::Reset => HistoryUnavailableReason::Reset,
        HistoryTombstoneReason::Resize => HistoryUnavailableReason::Resize,
        HistoryTombstoneReason::Expired => HistoryUnavailableReason::Expired,
        HistoryTombstoneReason::Limit => HistoryUnavailableReason::Limit,
        HistoryTombstoneReason::CodecFailure => HistoryUnavailableReason::CodecFailure,
        // `Released`, and any reason a newer server added.
        _ => HistoryUnavailableReason::Released,
    }
}

/// The kernel's reason for a history rejection. A reason this build does not
/// know (a newer server's) reads as `ZeroLimit`: the fetch is cancelled
/// without a retry or a tombstone.
const fn history_rejection_reason(reason: HistoryRejectionReason) -> KernelHistoryRejectionReason {
    match reason {
        HistoryRejectionReason::TooSmall => KernelHistoryRejectionReason::TooSmall,
        HistoryRejectionReason::Busy => KernelHistoryRejectionReason::Busy,
        // `ZeroLimit`, and any reason a newer server added.
        _ => KernelHistoryRejectionReason::ZeroLimit,
    }
}
/// The capability set phux-web advertises in `HELLO`.
///
/// Synthesized compatibility remains unconditional. The official snapshot
/// codec (`LibghosttySnapshotV1`) is added only after the loaded engine
/// decoded a snapshot through the browser's reader at load.
#[must_use]
pub fn client_caps(vt: &Vt) -> ClientCapabilities {
    let mut bootstrap = synthesized_bootstrap_caps();
    if vt.native_snapshot_available() {
        bootstrap = bootstrap.with_native(
            EngineCodec::LibghosttySnapshotV1,
            EngineFeatureSet::required_native(),
        );
    }
    ClientCapabilities::new()
        .with_image_protocols(ImageProtocolSet::new())
        .with_bootstrap(bootstrap)
}

fn synthesized_bootstrap_caps() -> BootstrapCapabilities {
    BootstrapCapabilities::new().with_profiles(BootstrapProfileSet::with(&[
        BootstrapProfileKind::SynthesizedVtRaw,
        BootstrapProfileKind::SynthesizedVtStateSync,
    ]))
}

fn synthesized_client_caps() -> ClientCapabilities {
    ClientCapabilities::new()
        .with_image_protocols(ImageProtocolSet::new())
        .with_bootstrap(synthesized_bootstrap_caps())
}

/// Leads every failure message for a protocol-major refusal, so a redial
/// loop that sees only the message can tell it from a transient loss.
pub const VERSION_REFUSAL: &str = "incompatible server protocol";

/// Whether a connection failure `message` is a protocol-major refusal.
#[must_use]
pub fn is_version_refusal(message: &str) -> bool {
    message.starts_with(VERSION_REFUSAL)
}

/// One `AgentSession` resource projected for the pane on screen: the badge
/// the DOM shows beside the canvas. Provider and state come from the kernel,
/// which folds the attach snapshot's facet and every stream record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentBadge {
    /// The resource id the server assigned to the agent session.
    pub id: ResourceId,
    /// The terminal the session is bound to, when the server reported one.
    pub parent: Option<ResourceId>,
    /// Provider name, for example `claude`; empty until something names one.
    pub provider: String,
    /// Derived lifecycle state word (`working`, `blocked`, `done`, `idle`,
    /// `ended`, or `unknown`).
    pub state: String,
}

/// The result of handling one incoming frame.
#[derive(Default)]
pub struct Outcome {
    /// Encoded frames for the browser transport to send.
    pub send: Vec<Vec<u8>>,
    /// Whether a published replica changed and should be repainted.
    pub render: bool,
    /// Whether the set of agent badges changed and should be repainted.
    pub badges: bool,
    /// Whether the focused terminal's program rang the bell (BEL).
    pub bell: bool,
    /// Pane inventory, focus, pending operation, or an actionable refusal changed.
    pub panes: bool,
    /// Fatal protocol/kernel failure; the transport must close.
    pub fatal: Option<String>,
}

/// An explicit kill in flight. Against an `ORGANIZATION` server it first
/// reads the session's page to learn the target's current activation.
struct PendingKill {
    request_id: u32,
    target: ResourceId,
    /// The session being read for the activation; `None` once the
    /// terminate (or legacy kill) is on the wire.
    resolving: Option<SessionKey>,
}

struct PendingSplit {
    request_id: u32,
    target: ResourceId,
    axis: Axis,
    resource: Option<ResourceId>,
    acknowledged: bool,
}

/// One attach connection whose terminal replicas are owned by [`SessionKernel`].
pub struct Connection {
    vt: Rc<Vt>,
    blank: Terminal,
    offered_caps: ClientCapabilities,
    kernel: Option<SessionKernel<WebEngine>>,
    effects: EffectBuffer,
    cols: u16,
    rows: u16,
    /// The pixel size of one cell as the client draws it, when known.
    cell_px: Option<(u16, u16)>,
    focused_terminal: Option<ResourceId>,
    terminal_order: Vec<ResourceId>,
    bootstrap_limits: Option<BootstrapLimits>,
    selected_profile: Option<BootstrapProfile>,
    /// The server's protocol from `HELLO_OK`: same major, any minor/patch.
    server_protocol: Option<Version>,
    path_query_supported: bool,
    path_request_id: u32,
    path_pending: Option<u32>,
    path_target: Option<ResourceId>,
    path_results: Option<PathResults>,
    path_error: Option<String>,
    failed: bool,
    /// The server refused this client's protocol major (or answered on
    /// another); no redial can succeed until one side is updated.
    version_refused: bool,
    render_visible: bool,
    attach_ready: bool,
    layout: Option<Layout>,
    /// Last requested geometry, including resizes not yet published by the server.
    pane_sizes: HashMap<ResourceId, CanonicalGeometry>,
    /// Failed new resources remain isolated until their queued close arrives.
    retiring_panes: Vec<ResourceId>,
    pending_split: Option<PendingSplit>,
    pending_kill: Option<PendingKill>,
    /// Correlate optional child-stream attach refusals without failing its terminal.
    pending_agents: HashMap<u32, ResourceId>,
    /// Bounded announcements observed before an initial or split parent is admitted.
    agent_discoveries: HashMap<ResourceId, resources::AgentDiscovery>,
    awaiting_agent_inventory: bool,
    pane_request_id: u32,
    pane_error: Option<String>,
    spawn_initial_size: bool,
    /// The server advertised `ORGANIZATION` in `HELLO_OK`.
    organization: bool,
    attach_preference: AttachPreference,
    discovery: Discovery,
    /// The attached session's persistent key, when the server reports one.
    session_key: Option<SessionKey>,
    operation_counter: u64,
}

impl Connection {
    /// Open a connection with a blank fallback grid of `cols`×`rows`.
    #[must_use]
    pub fn new(vt: &Rc<Vt>, cols: u16, rows: u16) -> Self {
        Self::with_caps(vt, cols, rows, client_caps(vt))
    }

    /// Open an explicit synthesized-only compatibility connection.
    ///
    /// This is a fail-closed diagnostic path for drift/unavailability tests;
    /// normal browser connections use [`Self::new`] and prefer exact native v2.
    #[must_use]
    pub fn new_synthesized_compat(vt: &Rc<Vt>, cols: u16, rows: u16) -> Self {
        Self::with_caps(vt, cols, rows, synthesized_client_caps())
    }

    fn with_caps(vt: &Rc<Vt>, cols: u16, rows: u16, offered_caps: ClientCapabilities) -> Self {
        Self {
            vt: Rc::clone(vt),
            blank: vt.terminal(cols, rows),
            offered_caps,
            kernel: None,
            effects: EffectBuffer::new(),
            cols,
            rows,
            cell_px: None,
            focused_terminal: None,
            terminal_order: Vec::new(),
            bootstrap_limits: None,
            selected_profile: None,
            server_protocol: None,
            path_query_supported: false,
            path_request_id: 0,
            path_pending: None,
            path_target: None,
            path_results: None,
            path_error: None,
            failed: false,
            version_refused: false,
            render_visible: false,
            attach_ready: false,
            layout: None,
            pane_sizes: HashMap::new(),
            retiring_panes: Vec::new(),
            pending_split: None,
            pending_kill: None,
            pending_agents: HashMap::new(),
            agent_discoveries: HashMap::new(),
            awaiting_agent_inventory: true,
            pane_request_id: 0x1000,
            pane_error: None,
            spawn_initial_size: false,
            organization: false,
            attach_preference: AttachPreference::default(),
            discovery: Discovery::Idle,
            session_key: None,
            operation_counter: 0,
        }
    }

    /// Negotiated decode limits after `HELLO_OK`.
    #[must_use]
    pub const fn bootstrap_limits(&self) -> Option<BootstrapLimits> {
        self.bootstrap_limits
    }

    /// Exact profile selected by a validated `HELLO_OK`, if negotiation finished.
    #[must_use]
    pub const fn selected_profile(&self) -> Option<BootstrapProfile> {
        self.selected_profile
    }

    /// Capabilities frozen into this connection's outbound `HELLO`.
    #[must_use]
    pub const fn advertised_capabilities(&self) -> ClientCapabilities {
        self.offered_caps
    }

    /// Whether this connection has entered its terminal protocol-failure state.
    #[must_use]
    pub const fn is_failed(&self) -> bool {
        self.failed
    }

    /// Whether the failure was a protocol-major refusal, which a redial
    /// cannot fix: the page should stop and say so instead of looping.
    #[must_use]
    pub const fn is_version_refused(&self) -> bool {
        self.version_refused
    }

    /// The protocol the server answered `HELLO_OK` with, once negotiated.
    /// It may be older or newer than this build's on the same major.
    #[must_use]
    pub const fn server_protocol(&self) -> Option<Version> {
        self.server_protocol
    }

    /// Whether the aggregate attach and every terminal bootstrap reached READY.
    #[must_use]
    pub const fn is_attach_ready(&self) -> bool {
        self.attach_ready
    }

    /// Permanently fail this connection after a transport or framing violation.
    pub fn fail_protocol(&mut self, _message: &str) {
        self.failed = true;
        self.cancel_path_query();
    }

    /// Frame sent when the transport opens. Stateful frames wait for `HELLO_OK`.
    #[must_use]
    pub fn handshake(&self) -> Vec<Vec<u8>> {
        vec![encode(&FrameKind::Hello {
            client_name: "phux-web".to_owned(),
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            client_caps: self.offered_caps,
        })]
    }

    /// Agent badges for the pane on screen: every `AgentSession` the kernel
    /// holds whose parent is that terminal, in resource-id order. Empty when
    /// the server reported none or every parent is another pane.
    #[must_use]
    pub fn agent_badges(&self) -> Vec<AgentBadge> {
        let Some(kernel) = self.kernel.as_ref() else {
            return Vec::new();
        };
        let focused = self
            .first_published_terminal()
            .or_else(|| self.focused_terminal.clone());
        let mut badges: Vec<AgentBadge> = kernel
            .agent_sessions()
            .filter(|view| view.parent.is_none() || view.parent == focused.as_ref())
            .map(|view| AgentBadge {
                id: view.terminal_id.clone(),
                parent: view.parent.cloned(),
                provider: view.state.provider.clone().unwrap_or_default(),
                state: view.state.status.as_str().to_owned(),
            })
            .collect();
        badges.sort_by(|left, right| left.id.cmp(&right.id));
        badges
    }

    /// Whether the kernel knows `terminal_id` as an `AgentSession` resource.
    fn is_agent_session(&self, terminal_id: &ResourceId) -> bool {
        self.kernel.as_ref().is_some_and(|kernel| {
            kernel.resource_kind(terminal_id) == Some(ResourceKind::AgentSession)
        })
    }
}

/// Single quotes keep shell metacharacters inert, including embedded apostrophes.
/// Reject terminal controls rather than pasting an executable newline or escape.
fn shell_quote_path(path: &str) -> Option<String> {
    if !path.starts_with('/') || path.chars().any(char::is_control) {
        return None;
    }
    Some(format!("'{}'", path.replace('\'', "'\\''")))
}

fn browser_monotonic_ms() -> u64 {
    web_sys::window()
        .and_then(|window| window.performance())
        .map_or(0, |performance| performance.now().max(0.0) as u64)
}

fn encode(frame: &FrameKind) -> Vec<u8> {
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    buf.to_vec()
}

/// The encoded `HISTORY_REQUEST` for a kernel history request.
fn history_request_frame(send: &KernelSend) -> Option<Vec<u8>> {
    let KernelSend::HistoryRequest {
        key,
        cursor,
        max_bytes,
        max_rows,
    } = send
    else {
        return None;
    };
    Some(encode(&FrameKind::HistoryRequest {
        terminal_id: key.terminal_id.clone(),
        stream_id: key.stream_id,
        bootstrap_id: key.bootstrap_id,
        cursor: Bytes::copy_from_slice(cursor),
        max_bytes: *max_bytes,
        max_rows: *max_rows,
    }))
}

/// The resource a terminal-stream frame addresses, when it addresses one.
fn frame_resource_id(frame: &FrameKind) -> Option<&ResourceId> {
    match frame {
        FrameKind::BootstrapBegin { terminal_id, .. }
        | FrameKind::BootstrapChunk { terminal_id, .. }
        | FrameKind::BootstrapReady { terminal_id, .. }
        | FrameKind::BootstrapTombstone { terminal_id, .. }
        | FrameKind::HistoryPage { terminal_id, .. }
        | FrameKind::HistoryTombstone { terminal_id, .. }
        | FrameKind::HistoryRejected { terminal_id, .. }
        | FrameKind::ResourceOutput { terminal_id, .. }
        | FrameKind::ResourceClosed { terminal_id, .. } => Some(terminal_id),
        _ => None,
    }
}
