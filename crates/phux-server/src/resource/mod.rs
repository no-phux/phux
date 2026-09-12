//! The generic resource core and the engines that back it.
//!
//! The server serves *resources*. Every resource, whatever its kind, has
//! the same backing-agnostic surface: an ordered output stream with a
//! checked `u64` sequence, a set of consumers attached to that stream, a
//! semantic event fan-out to subscribed clients, a lifecycle (a cancel
//! token in, an exit notification out), and a supervisory control mailbox.
//! [`ResourceCore`] owns that state on the engine side; [`ResourceHandle`]
//! is the `Send + Clone` channel set the runtime holds for it.
//!
//! What differs per kind is the *facet*: the channels only that engine
//! serves. A Terminal answers snapshot, screen, resize, cwd, palette,
//! native-checkpoint, and input requests; another kind does not. The
//! facet lives behind [`ResourceHandle::facet`] as a
//! [`ResourceFacetHandle`], and the runtime reaches it through
//! [`ResourceHandle::terminal`] — the one place that turns "this resource is
//! not a Terminal" into a [`WrongResourceKind`] error. Nothing else in the
//! crate inspects the facet enum.
//!
//! Engines live in submodules: [`terminal`] is the PTY-plus-libghostty
//! engine ([`terminal::TerminalActor`]). An engine embeds a
//! [`ResourceCore`], builds its facet handle, and assembles the
//! [`ResourceHandle`] in its constructor. ADR-0014's placement rule is the
//! engine's to keep: an engine that owns `!Send` state runs as one
//! `spawn_local` task and is the sole borrower of that state; the core adds
//! no shared cells across tasks.

use std::cell::RefCell;

use bytes::Bytes;
use phux_protocol::ClientId;
use phux_protocol::wire::frame::{
    AgentEvent, ControlAction, FrameKind, ReportedAgentState, ResourceEventType, TerminalSignal,
};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::mailbox::Outbound;

pub mod agent_session;
pub mod terminal;

pub use phux_core::ids::ResourceId;
pub use phux_core::resource::ResourceKind;

/// The wire tag for a kind the server serves.
///
/// `phux-core` and `phux-protocol` must not depend on each other, so the
/// domain kind and the wire kind are two enums and this crate — which
/// depends on both — is where they meet, exactly as
/// [`id_bridge`](crate::id_bridge) is where the two id spaces meet.
#[must_use]
pub const fn wire_kind(kind: ResourceKind) -> phux_protocol::ids::ResourceKind {
    match kind {
        ResourceKind::Terminal => phux_protocol::ids::ResourceKind::Terminal,
        ResourceKind::AgentSession => phux_protocol::ids::ResourceKind::AgentSession,
        // The domain enum is `non_exhaustive` for the same forward-compat
        // reason the wire one is, and this crate is where a new kind gets
        // its tag. A domain kind with no tag here is a build that added one
        // side of the pair; reporting it as opaque is the one answer that
        // cannot be mistaken for a Terminal, and 255 is not an allocated
        // tag.
        _ => phux_protocol::ids::ResourceKind::Unknown { tag: u8::MAX },
    }
}

/// The domain kind a wire tag names, or `None` for a tag this build does
/// not serve.
///
/// `None` is not a decode failure — the wire enum is open on purpose, so a
/// newer consumer naming a kind this server has never heard of gets a typed
/// refusal (`SpawnError::UnsupportedKind`) instead of a dropped connection.
#[must_use]
pub const fn core_kind(kind: phux_protocol::ids::ResourceKind) -> Option<ResourceKind> {
    match kind {
        phux_protocol::ids::ResourceKind::Terminal => Some(ResourceKind::Terminal),
        phux_protocol::ids::ResourceKind::AgentSession => Some(ResourceKind::AgentSession),
        _ => None,
    }
}

use agent_session::AgentSessionHandle;
use terminal::{
    ConsumerAckRequest, ConsumerAttachRequest, ConsumerDetachRequest, TerminalHandle,
    UpgradeHandleRequest,
};

/// Default capacity of the per-resource output broadcast channel.
///
/// Bytes fan out to subscribed clients. Sized for "burst tolerance" —
/// a busy resource can emit a few dozen frames in a short window before a
/// slow subscriber falls behind and gets a `RecvError::Lagged`.
pub const DEFAULT_OUTPUT_BROADCAST: usize = 256;

/// Depth of the core's request mailboxes (event subscription, control).
///
/// Small on purpose: these are supervisory requests the server drains in
/// the same event loop. A backed-up mailbox here means the engine has
/// stalled, which is its own bug to investigate.
const CORE_MAILBOX: usize = 64;

// ---- output stream ----------------------------------------------------------

/// Continuity reason carried with an engine-generated full resync.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResyncReason {
    /// Authoritative geometry changed.
    Resize,
    /// A bounded output subscriber observed a sequence gap.
    OutboundGap,
}

/// One output pump on a pane: the server-local client that owns it and the
/// wire stream it publishes on.
///
/// The same pair a [`PaneOutput::Control`] frame is routed by (`owner`, then
/// the frame's own `stream_id`), so a resync can be addressed as precisely
/// as a tombstone already is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ResyncTarget {
    /// Server-local client id of the pump's consumer.
    pub owner: u64,
    /// Stream the pump publishes its generations on.
    pub stream_id: phux_protocol::ids::StreamId,
    /// The generation the resync replaces: the one the pump was fenced on, or
    /// the one a reflow tombstoned. Owner and stream alone can collide — an
    /// `ATTACH` pump's stream id comes from its attach id and an
    /// `ATTACH_RESOURCE` pump's from its client id — so without the generation
    /// one addressed resync could revive two pumps, and their native captures
    /// would race for the owner-keyed binding.
    pub bootstrap_id: phux_protocol::ids::BootstrapId,
}

/// Which output pumps a [`PaneOutput::Resync`] replaces the generation of.
///
/// A resync is expensive for every pump that takes it: a tombstone, a full
/// bootstrap on the wire, and for a native consumer a checkpoint capture on
/// the actor. It is owed to *every* consumer only when the grid itself
/// changed under them (a resize reflow). A pump that fell behind (a
/// broadcast `Lagged`, or a chunk past the pump's staleness budget) lost
/// frames nobody else lost, so its resync is addressed to it alone and every
/// other pump on the pane keeps its generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResyncAudience {
    /// Every subscriber replaces its generation.
    Everyone,
    /// Only the named pumps replace their generation; every other subscriber
    /// ignores this resync. More than one when several pumps asked inside
    /// the same debounce window: they share one synthesis.
    Only(std::sync::Arc<[ResyncTarget]>),
}

impl ResyncAudience {
    /// Is `target` one of the pumps this resync is for?
    #[must_use]
    pub fn includes(&self, target: ResyncTarget) -> bool {
        match self {
            Self::Everyone => true,
            Self::Only(targets) => targets.contains(&target),
        }
    }
}

/// Payload of the per-resource output broadcast ([`ResourceHandle::output`]).
///
/// Subscribers (the per-attach output pumps in `runtime::attach`) map each
/// variant to a distinct wire frame:
///
/// * [`PaneOutput::Live`] → `RESOURCE_OUTPUT` — a post-snapshot byte delta.
/// * [`PaneOutput::Resync`] → `TERMINAL_SNAPSHOT` — the full post-reflow
///   grid, carrying the new `(cols, rows)` so the client mirror RESIZES to
///   them and repaints from authoritative state.
/// * [`PaneOutput::Control`] → an ordered generation/history control frame
///   routed only by the pump whose server-local owner matches.
///
/// Routing the resize-resync as a `TERMINAL_SNAPSHOT` (rather than raw
/// output) is load-bearing. The client resizes its libghostty mirror ONLY
/// on `TERMINAL_SNAPSHOT` (ADR-0013 / phux-wurs: the mirror's grid size is
/// server-authoritative and never guessed from a client-side rect). A
/// resync delivered as `RESOURCE_OUTPUT` would `vt_write` into a mirror
/// still at its old size, so a resize that GROWS a pane — kill-pane reflow
/// promoting the survivor, or enlarging the outer window — could never fill
/// the freed space (phux-3ns5). The snapshot path resizes first, then
/// applies the synthesized grid, so grow and shrink both reconverge.
#[derive(Clone, Debug)]
pub enum PaneOutput {
    /// Live output chunk forwarded as `RESOURCE_OUTPUT`.
    Live {
        /// Resource-global, strictly increasing raw output sequence.
        seq: u64,
        /// Verbatim output bytes for this sequence.
        bytes: Bytes,
        /// When the producer read these bytes (for a Terminal, when the PTY
        /// `read(2)` returned). A pump that dequeues a chunk long after this
        /// is a consumer that has fallen behind in time, whatever the
        /// broadcast's slot count says.
        at: std::time::Instant,
    },
    /// Post-resize grid resync forwarded as `TERMINAL_SNAPSHOT` at the
    /// carried dims (phux-8v1 reconverge mechanism + phux-3ns5 mirror
    /// resize). `bytes` is the synthesized grid replay (with its
    /// `DECSTR + ED2 + home` reset preamble); `cols`/`rows` are the
    /// post-reflow grid size the client mirror must adopt.
    Resync {
        /// Post-reflow grid width the client mirror resizes to.
        cols: u16,
        /// Post-reflow grid height the client mirror resizes to.
        rows: u16,
        /// Why the prior generation can no longer continue.
        reason: ResyncReason,
        /// Which pumps must replace their generation; the rest ignore it.
        audience: ResyncAudience,
        /// Resource-global raw sequence included by the replacement cut.
        base_seq: u64,
        /// Synthesized grid replay (with reset preamble) for `vt_write`.
        bytes: Bytes,
    },
    /// Ordered native control. It shares the broadcast sequence with
    /// [`Self::Live`] so a pump observes every prior raw sequence before
    /// invalidating the matching generation or history cursor.
    Control {
        /// Server-local pump owner; other subscribers ignore this control.
        owner: u64,
        /// Fully-owned tombstone/control frame for the matching pump.
        frame: FrameKind,
    },
}

// ---- semantic event fan-out -------------------------------------------------

/// A client subscribed to semantic events for a single resource.
/// Holds the client's outbound mailbox and event type filter.
#[derive(Clone, Debug)]
pub struct ResourceEventSubscriber {
    /// Client's outbound frame channel (where Event frames are sent).
    pub outbound: mpsc::Sender<Outbound>,
    /// Event type filter (empty = all types). Only events matching a type
    /// in this list are forwarded; if empty, all events are sent.
    pub event_types: Vec<ResourceEventType>,
}

/// Request to subscribe to a resource's semantic events.
#[derive(Debug)]
pub struct SubscribeToEventsRequest {
    /// The new subscriber to register.
    pub subscriber: ResourceEventSubscriber,
    /// Wire-level resource id for Event frames (SPEC §7.1).
    /// The runtime passes this when registering.
    pub wire_terminal_id: u32,
}

/// Request to unsubscribe from a resource's semantic events.
#[derive(Debug)]
pub struct UnsubscribeFromEventsRequest {
    /// Address of the subscriber's outbound mailbox, used for identity
    /// comparison against the registered subscribers.
    ///
    /// A `usize` address rather than a `*const Sender<Outbound>`: a raw
    /// pointer is `!Send`, which would make [`ResourceHandle`] — and through
    /// it the entire [`ServerState`](crate::state::ServerState) — `!Send`,
    /// blocking the dedicated input lane (phux-51n6.2, ADR-0044) that routes
    /// input from a separate thread. The identity semantics are unchanged:
    /// the core compares this against `&raw const sub.outbound as usize`.
    pub outbound_addr: usize,
}

// ---- supervisory control ----------------------------------------------------

/// A supervisory control request delivered to a resource's engine over its
/// `control` mailbox (ADR-0033, "take the wheel + kill").
///
/// The input *lease* itself lives in [`crate::state::ServerState`] (the input
/// gate runs there, under the state lock, where the originating `ClientId` is
/// known). The engine is the emitter of the
/// [`AgentEvent::TerminalControl`] broadcast because it owns both the
/// event-subscriber list and the process lifecycle — so the handler forwards
/// the *fact* of a change and lets the engine stamp its current lifecycle and
/// fan the event out.
///
/// The variant set is the union the runtime issues; an engine handles the
/// variants that apply to its kind and answers the rest with an error reply
/// where one exists.
#[derive(Debug)]
pub enum ControlRequest {
    /// The input lease changed in `ServerState`; emit a `TerminalControl`
    /// broadcast reflecting the new holder and the action that produced it.
    LeaseChanged {
        /// The client now holding the lease, or `None` if released to `Open`.
        input_holder: Option<ClientId>,
        /// What just happened (`Acquired` / `Seized` / `Released`).
        action: ControlAction,
        /// The client that performed the action.
        actor: ClientId,
    },
    /// Something other than the detector wrote this pane's `phux.agent/v1`
    /// record (an explicit `SET_METADATA` or `DELETE_METADATA`), so the
    /// detector's edge filter — a model of its own emissions — is now a model
    /// of a store that no longer exists (ADR-0046 §E).
    ///
    /// The engine clears the filter, re-arming exactly one republish on the
    /// next tick. This is what makes `DELETE` mean "the detector resumes"
    /// rather than "the record vanishes until the agent's state happens to
    /// change" — which, for an agent sitting idle waiting on a human, is
    /// never. No-op on a resource with no detector.
    AgentRecordInvalidated,
    /// Bind the producer channel of an `AgentSession` child that just
    /// spawned under this Terminal (ADR-0103 §6), so a hook report becomes
    /// a record on that child's stream rather than a second opinion beside
    /// it. No reply: the binding is a fact, not a request.
    BindAgentSession {
        /// The child engine's append channel.
        append: mpsc::Sender<agent_session::AppendRequest>,
    },
    /// Feed an `AgentSession` child's derived state into this Terminal's
    /// detector at the `Stream` rank (ADR-0103 §5).
    ///
    /// The rank is what distinguishes this from
    /// [`Self::ReportAgentState`]: while a session is producing records its
    /// stream outranks the hook edge, because the stream carries the same
    /// facts in order and with a replayable log behind them. `None` is the
    /// retraction a `session_end` produces — the stream stops asserting a
    /// state, and lower-ranked evidence resumes.
    ReportStreamState {
        /// The derived state, or `None` to withdraw the stream's claim.
        state: Option<ReportedAgentState>,
        /// Whether the detector accepted the evidence.
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Feed lifecycle-hook evidence into the pane's detector.
    ReportAgentState {
        /// Hook-reported state.
        state: ReportedAgentState,
        /// Whether the detector accepted the evidence.
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Turn hook-reported state into a synthesized `state` record on this
    /// Terminal's live `AgentSession` child, so one source of truth - the
    /// stream - feeds the arbiter (ADR-0103 decision 6).
    ///
    /// Routed here instead of [`Self::ReportAgentState`] only when the
    /// binding graph says a live child exists; otherwise `REPORT_AGENT_STATE`
    /// takes the ADR-0085 path unchanged. Two requests rather than a flag on
    /// one, because the two do genuinely different things to different
    /// resources and a boolean would hide that at every call site.
    SynthesizeAgentStateRecord {
        /// Hook-reported state, to be appended as
        /// `{"type":"state","data":{"state":...,"source":"hook"}}`.
        state: ReportedAgentState,
        /// Whether the record was accepted.
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Deliver `signal` to the pane's process group, update the lifecycle
    /// (`Freeze` → `Frozen`, `Resume` → `Running`), and broadcast a
    /// `TerminalControl`. `reply` carries `Ok(())` on delivery or a
    /// human-readable error (no PTY / no pid / `killpg` failed).
    Signal {
        /// The signal to deliver.
        signal: TerminalSignal,
        /// The lease holder at the time of the signal, for the broadcast.
        input_holder: Option<ClientId>,
        /// The client requesting the signal.
        by: ClientId,
        /// Delivery acknowledgement.
        reply: oneshot::Sender<Result<(), String>>,
    },
}

// ---- handle -----------------------------------------------------------------

/// A facet operation was requested of a resource of another kind.
///
/// Produced only by [`ResourceHandle::terminal`]; every runtime path that
/// needs a Terminal-only channel goes through that accessor, so this is the
/// single origin of the "wrong kind" condition inside the server. The
/// protocol's `WRONG_RESOURCE_KIND` error code is the wire form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("resource is {actual}, operation requires {required}")]
pub struct WrongResourceKind {
    /// The kind the operation needs.
    pub required: ResourceKind,
    /// The kind the resource actually is.
    pub actual: ResourceKind,
}

/// The kind-specific channel set of one resource.
///
/// One variant per engine. Read only through the accessors on
/// [`ResourceHandle`]; runtime code never matches on this enum directly.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum ResourceFacetHandle {
    /// Terminal engine: input, snapshot, screen, resize, cwd, palette, and
    /// native-checkpoint channels plus the construction-time grid size.
    Terminal(TerminalHandle),
    /// Agent-session engine: producer appends, bootstrap cuts, and the
    /// session's immutable provider and native id.
    AgentSession(AgentSessionHandle),
}

impl ResourceFacetHandle {
    /// The kind whose channels this facet carries.
    #[must_use]
    pub const fn kind(&self) -> ResourceKind {
        match self {
            Self::Terminal(_) => ResourceKind::Terminal,
            Self::AgentSession(_) => ResourceKind::AgentSession,
        }
    }
}

/// Cross-task handle to one resource's engine.
///
/// `ResourceHandle` is `Send + Clone`: per-client tasks clone it freely to
/// subscribe to the output broadcast, attach consumers, subscribe to events,
/// or send control. The engine itself (which may own `!Send` state, as the
/// Terminal engine does) lives on the `LocalSet` and never crosses a thread
/// boundary (ADR-0014).
///
/// Every field but `facet` exists for every kind. The payload types of
/// `consumer_attach`, `consumer_detach`, `consumer_ack`, and `upgrade` are
/// defined by the Terminal engine; the channels themselves are part of the
/// generic surface.
#[derive(Debug, Clone)]
pub struct ResourceHandle {
    /// Which engine is behind this handle.
    pub kind: ResourceKind,
    /// The resource this one is bound to, if any. Immutable for the
    /// resource's lifetime.
    pub parent: Option<ResourceId>,
    /// Output broadcast channel; subscribers receive every output chunk
    /// ([`PaneOutput::Live`]) plus engine-generated resyncs
    /// ([`PaneOutput::Resync`]) and ordered control ([`PaneOutput::Control`]).
    pub output: broadcast::Sender<PaneOutput>,
    /// ADR-0018 per-consumer lifecycle (phux-q0e.2). The runtime sends a
    /// [`ConsumerAttachRequest`] on each successful ATTACH so the engine
    /// registers the consumer before the bootstrap goes out.
    pub consumer_attach: mpsc::Sender<ConsumerAttachRequest>,
    /// Counterpart to [`Self::consumer_attach`]. The runtime sends this on
    /// DETACH (and on the EOF cleanup path). Silent no-op if the consumer
    /// was never attached.
    pub consumer_detach: mpsc::Sender<ConsumerDetachRequest>,
    /// ADR-0018 inbound `FRAME_ACK` channel (phux-q0e.4). One
    /// [`ConsumerAckRequest`] per decoded `FRAME_ACK` whose `terminal_id`
    /// resolved to this resource. Silent no-op if the consumer is not
    /// currently registered.
    pub consumer_ack: mpsc::Sender<ConsumerAckRequest>,
    /// Subscribe to semantic events for this resource. The runtime sends a
    /// [`SubscribeToEventsRequest`] when a client subscribes; the core
    /// registers the subscriber and begins fanning matching events out.
    pub subscribe_to_events: mpsc::Sender<SubscribeToEventsRequest>,
    /// Unsubscribe from semantic events. The runtime sends an
    /// [`UnsubscribeFromEventsRequest`] when a client detaches; the core
    /// removes the subscriber from its list (idempotent).
    pub unsubscribe_from_events: mpsc::Sender<UnsubscribeFromEventsRequest>,
    /// Graceful-upgrade handoff channel (ADR-0032). The upgrade producer
    /// sends an [`UpgradeHandleRequest`] per resource to collect what the
    /// re-exec'd image needs to re-adopt it.
    pub upgrade: mpsc::Sender<UpgradeHandleRequest>,
    /// Supervisory control channel (ADR-0033). The runtime sends a
    /// [`ControlRequest`] when a client takes the wheel, releases it, or
    /// signals the resource. The engine broadcasts `TerminalControl` events
    /// (it owns the event-subscriber list) and delivers what applies to it.
    pub control: mpsc::Sender<ControlRequest>,
    /// The kind-specific channels. Reach them through [`Self::terminal`].
    pub facet: ResourceFacetHandle,
}

impl ResourceHandle {
    /// The Terminal facet: the only route from runtime code to a
    /// Terminal-only channel (snapshot, screen, resize, cwd, palette,
    /// native, input, grid size).
    ///
    /// # Errors
    ///
    /// [`WrongResourceKind`] when this resource is not a Terminal. This is
    /// the sole producer of that error in the crate; callers map it into
    /// their own reply shape.
    pub const fn terminal(&self) -> Result<&TerminalHandle, WrongResourceKind> {
        match &self.facet {
            ResourceFacetHandle::Terminal(handle) => Ok(handle),
            _ => Err(WrongResourceKind {
                required: ResourceKind::Terminal,
                actual: self.facet.kind(),
            }),
        }
    }

    /// The `AgentSession` facet: the only route from runtime code to a
    /// session-only channel (producer append, bootstrap cut) and to the
    /// session's provider and native id.
    ///
    /// # Errors
    ///
    /// [`WrongResourceKind`] when this resource is not an agent session —
    /// the reply `APPEND_RESOURCE_OUTPUT` on a Terminal earns.
    pub const fn agent_session(&self) -> Result<&AgentSessionHandle, WrongResourceKind> {
        match &self.facet {
            ResourceFacetHandle::AgentSession(handle) => Ok(handle),
            _ => Err(WrongResourceKind {
                required: ResourceKind::AgentSession,
                actual: self.facet.kind(),
            }),
        }
    }
}

// ---- engine-side core -------------------------------------------------------

/// The generic, engine-side half of a resource.
///
/// Owned by exactly one engine task. Holds the checked output sequence, the
/// output broadcast sender, the semantic-event subscriber registry and the
/// fan-out over it, the lifecycle (cancel token in, exit notification out),
/// and the receiving ends of the core mailboxes. An engine polls the
/// receivers in its own `select!` and calls back in for the bookkeeping.
///
/// No shared interior mutability crosses a task boundary: the
/// `RefCell` around the subscriber list exists so an engine's `select!`
/// arms can fan events out from `&self` while another arm holds a
/// disjoint `&mut` field, and it is only ever touched from the owning
/// task (ADR-0014).
pub struct ResourceCore {
    /// Which engine owns this core.
    pub(super) kind: ResourceKind,
    /// The resource this one is bound to, if any.
    pub(super) parent: Option<ResourceId>,
    /// Wire-level resource id stamped on Event frames. `0` until the first
    /// event subscriber registers and supplies it.
    pub(super) wire_id: u32,
    /// Resource-global raw output sequence; never resets across bootstrap
    /// generations. Advances only through [`Self::next_seq`].
    pub(super) seq: u64,
    /// Output broadcast sender. The seed receiver is dropped at
    /// construction, so `receiver_count()` is the live-subscriber count.
    pub(super) output_tx: broadcast::Sender<PaneOutput>,
    /// Semantic-event subscribers. Added by [`Self::subscribe_events`],
    /// removed by [`Self::unsubscribe_events`].
    pub(super) event_subscribers: RefCell<Vec<ResourceEventSubscriber>>,
    /// One-shot fired when the engine observes its backing exit. `Option`
    /// so it can be `take()`n after firing — sending on a `oneshot::Sender`
    /// is a by-value move. `None` after the first fire.
    pub(super) exit_notify: Option<oneshot::Sender<Option<i32>>>,
    /// Cancellation token the engine's loop watches. Cancel to ask the
    /// engine to shut down cleanly. Dropping the token does NOT cancel —
    /// cancellation is always explicit.
    pub(super) token: CancellationToken,
    /// Inbound event-subscription requests.
    pub(super) subscribe_to_events_rx: mpsc::Receiver<SubscribeToEventsRequest>,
    /// Inbound event-unsubscription requests.
    pub(super) unsubscribe_from_events_rx: mpsc::Receiver<UnsubscribeFromEventsRequest>,
    /// Supervisory control mailbox (ADR-0033).
    pub(super) control_rx: mpsc::Receiver<ControlRequest>,
}

/// The sender halves paired with a fresh [`ResourceCore`], for the engine
/// to fold into its [`ResourceHandle`] and bundle.
#[derive(Debug)]
pub struct ResourceCoreChannels {
    /// Output broadcast sender, cloned into the handle.
    pub output: broadcast::Sender<PaneOutput>,
    /// Event-subscription sender.
    pub subscribe_to_events: mpsc::Sender<SubscribeToEventsRequest>,
    /// Event-unsubscription sender.
    pub unsubscribe_from_events: mpsc::Sender<UnsubscribeFromEventsRequest>,
    /// Supervisory control sender.
    pub control: mpsc::Sender<ControlRequest>,
    /// Fires with the exit status when the engine observes its backing
    /// exit.
    pub exit_notify: oneshot::Receiver<Option<i32>>,
}

impl ResourceCore {
    /// Build a core for a resource of `kind` bound to `parent`, whose
    /// engine loop watches `token`, with an output broadcast of
    /// `output_capacity` frames.
    #[must_use]
    pub fn new(
        kind: ResourceKind,
        parent: Option<ResourceId>,
        token: CancellationToken,
        output_capacity: usize,
    ) -> (Self, ResourceCoreChannels) {
        let (output_tx, _seed_rx) = broadcast::channel(output_capacity);
        let (subscribe_tx, subscribe_to_events_rx) = mpsc::channel(CORE_MAILBOX);
        let (unsubscribe_tx, unsubscribe_from_events_rx) = mpsc::channel(CORE_MAILBOX);
        let (control_tx, control_rx) = mpsc::channel(CORE_MAILBOX);
        let (exit_tx, exit_rx) = oneshot::channel();
        let core = Self {
            kind,
            parent,
            wire_id: 0,
            seq: 0,
            output_tx: output_tx.clone(),
            event_subscribers: RefCell::new(Vec::new()),
            exit_notify: Some(exit_tx),
            token,
            subscribe_to_events_rx,
            unsubscribe_from_events_rx,
            control_rx,
        };
        let channels = ResourceCoreChannels {
            output: output_tx,
            subscribe_to_events: subscribe_tx,
            unsubscribe_from_events: unsubscribe_tx,
            control: control_tx,
            exit_notify: exit_rx,
        };
        (core, channels)
    }

    /// Which engine owns this core.
    #[must_use]
    pub const fn kind(&self) -> ResourceKind {
        self.kind
    }

    /// The resource this one is bound to, if any.
    #[must_use]
    pub const fn parent(&self) -> Option<ResourceId> {
        self.parent
    }

    /// The last output sequence issued (`0` before the first).
    #[must_use]
    pub const fn seq(&self) -> u64 {
        self.seq
    }

    /// Advance and return the next output sequence, or `None` when the
    /// `u64` is exhausted — the engine must then stop rather than wrap.
    pub const fn next_seq(&mut self) -> Option<u64> {
        match self.seq.checked_add(1) {
            Some(seq) => {
                self.seq = seq;
                Some(seq)
            }
            None => None,
        }
    }

    /// Register an event subscriber and adopt the wire id it was
    /// registered under for Event frames.
    pub fn subscribe_events(&mut self, request: SubscribeToEventsRequest) {
        self.wire_id = request.wire_terminal_id;
        self.event_subscribers.borrow_mut().push(request.subscriber);
    }

    /// Remove the subscriber whose outbound mailbox address matches.
    /// Silent no-op if none does.
    pub fn unsubscribe_events(&self, request: &UnsubscribeFromEventsRequest) {
        let mut subs = self.event_subscribers.borrow_mut();
        subs.retain(|sub| (&raw const sub.outbound) as usize != request.outbound_addr);
    }

    /// `true` when no client is subscribed to semantic events.
    #[must_use]
    pub fn has_no_event_subscribers(&self) -> bool {
        self.event_subscribers.borrow().is_empty()
    }

    /// Fan `event` out to every subscriber whose type filter admits it.
    /// `try_send`: a full mailbox drops the event rather than stalling the
    /// engine — the event stream is an accelerator, never a guarantee.
    pub fn fan_out_event(&self, event: &AgentEvent) {
        let subs = self.event_subscribers.borrow();
        for subscriber in subs.iter() {
            // Map AgentEvent variants to ResourceEventType for filtering.
            let event_type = match event {
                AgentEvent::CommandStarted => Some(ResourceEventType::CommandStarted),
                AgentEvent::CommandFinished { .. } => Some(ResourceEventType::CommandEnded),
                AgentEvent::CwdChanged { .. } => Some(ResourceEventType::CwdChanged),
                AgentEvent::Dirty => Some(ResourceEventType::GridChanged),
                AgentEvent::Idle => Some(ResourceEventType::OutputReceived),
                // Other event types don't map to semantic filters yet
                _ => None,
            };

            // Supervisory control events (ADR-0033) bypass the semantic-type
            // filter: "who has the wheel" and "frozen" are not grid activity,
            // and every subscriber needs them to render an honest state.
            let interested = matches!(event, AgentEvent::TerminalControl { .. })
                || event_type.is_some_and(|et| {
                    subscriber.event_types.is_empty() || subscriber.event_types.contains(&et)
                });

            if interested {
                let frame = FrameKind::Event {
                    terminal: if self.wire_id == 0 {
                        None
                    } else {
                        Some(phux_protocol::ids::ResourceId::local(self.wire_id))
                    },
                    event: event.clone(),
                };
                let _ = subscriber.outbound.try_send(Outbound::Frame(frame));
            }
        }
    }

    /// Report the backing's exit status to whoever holds the bundle's
    /// receiver. Fires at most once; later calls are no-ops.
    pub fn notify_exit(&mut self, status: Option<i32>) {
        if let Some(tx) = self.exit_notify.take() {
            let _ = tx.send(status);
        }
    }
}

impl std::fmt::Debug for ResourceCore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResourceCore")
            .field("kind", &self.kind)
            .field("parent", &self.parent)
            .field("wire_id", &self.wire_id)
            .field("seq", &self.seq)
            .field("event_subscribers", &self.event_subscribers.borrow().len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal_handle_stub() -> ResourceHandle {
        let (core, channels) = ResourceCore::new(
            ResourceKind::Terminal,
            None,
            CancellationToken::new(),
            DEFAULT_OUTPUT_BROADCAST,
        );
        drop(core);
        ResourceHandle {
            kind: ResourceKind::Terminal,
            parent: None,
            output: channels.output,
            consumer_attach: mpsc::channel(1).0,
            consumer_detach: mpsc::channel(1).0,
            consumer_ack: mpsc::channel(1).0,
            subscribe_to_events: channels.subscribe_to_events,
            unsubscribe_from_events: channels.unsubscribe_from_events,
            upgrade: mpsc::channel(1).0,
            control: channels.control,
            facet: ResourceFacetHandle::Terminal(TerminalHandle::detached_for_test(80, 24)),
        }
    }

    #[test]
    fn terminal_facet_resolves_for_a_terminal() {
        let handle = terminal_handle_stub();
        let facet = handle.terminal().expect("terminal facet");
        assert_eq!((facet.cols, facet.rows), (80, 24));
        assert_eq!(handle.facet.kind(), ResourceKind::Terminal);
    }

    #[test]
    fn next_seq_is_checked() {
        let (mut core, _channels) = ResourceCore::new(
            ResourceKind::Terminal,
            None,
            CancellationToken::new(),
            DEFAULT_OUTPUT_BROADCAST,
        );
        assert_eq!(core.next_seq(), Some(1));
        assert_eq!(core.seq(), 1);
        core.seq = u64::MAX;
        assert_eq!(core.next_seq(), None, "an exhausted sequence never wraps");
        assert_eq!(core.seq(), u64::MAX);
    }

    #[test]
    fn exit_notify_fires_once() {
        let (mut core, channels) = ResourceCore::new(
            ResourceKind::Terminal,
            None,
            CancellationToken::new(),
            DEFAULT_OUTPUT_BROADCAST,
        );
        core.notify_exit(Some(3));
        core.notify_exit(Some(4));
        assert_eq!(channels.exit_notify.blocking_recv(), Ok(Some(3)));
    }

    #[test]
    fn event_fan_out_respects_type_filter_and_wire_id() {
        let (mut core, _channels) = ResourceCore::new(
            ResourceKind::Terminal,
            None,
            CancellationToken::new(),
            DEFAULT_OUTPUT_BROADCAST,
        );
        let (all_tx, mut all_rx) = mpsc::channel(4);
        let (cwd_tx, mut cwd_rx) = mpsc::channel(4);
        core.subscribe_events(SubscribeToEventsRequest {
            subscriber: ResourceEventSubscriber {
                outbound: all_tx,
                event_types: Vec::new(),
            },
            wire_terminal_id: 7,
        });
        core.subscribe_events(SubscribeToEventsRequest {
            subscriber: ResourceEventSubscriber {
                outbound: cwd_tx.clone(),
                event_types: vec![ResourceEventType::CwdChanged],
            },
            wire_terminal_id: 7,
        });

        core.fan_out_event(&AgentEvent::Dirty);
        core.fan_out_event(&AgentEvent::CwdChanged {
            cwd: "/tmp".to_owned(),
        });

        let mut all = Vec::new();
        while let Ok(out) = all_rx.try_recv() {
            all.push(out);
        }
        assert_eq!(all.len(), 2, "unfiltered subscriber sees both");
        match &all[0] {
            Outbound::Frame(FrameKind::Event { terminal, .. }) => {
                assert_eq!(*terminal, Some(phux_protocol::ids::ResourceId::local(7)));
            }
            other => panic!("expected an Event frame, got {other:?}"),
        }
        let mut cwd_only = Vec::new();
        while let Ok(out) = cwd_rx.try_recv() {
            cwd_only.push(out);
        }
        assert_eq!(
            cwd_only.len(),
            1,
            "filtered subscriber sees only CwdChanged"
        );

        core.unsubscribe_events(&UnsubscribeFromEventsRequest {
            outbound_addr: {
                let subs = core.event_subscribers.borrow();
                (&raw const subs[1].outbound) as usize
            },
        });
        assert_eq!(core.event_subscribers.borrow().len(), 1);
        drop(cwd_tx);
    }
}
