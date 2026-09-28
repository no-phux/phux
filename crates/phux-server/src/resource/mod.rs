//! The generic resource core and the engines that back it.
//!
//! Every resource has the same backing-agnostic surface: an ordered output
//! stream with a checked `u64` sequence, attached consumers, a semantic
//! event sink ([`event_sink`], ADR-0123), a lifecycle, and a control
//! mailbox. [`ResourceCore`] owns it engine-side; [`ResourceHandle`] is the
//! `Send + Clone` channel set the runtime holds.
//!
//! Per-kind channels are the facet ([`ResourceFacetHandle`]), reached only
//! through accessors such as [`ResourceHandle::terminal`], the single source
//! of [`WrongResourceKind`]. Engines live in submodules and follow ADR-0014:
//! `!Send` state stays in one `spawn_local` task.

use std::sync::atomic::{AtomicUsize, Ordering};

use bytes::Bytes;
use phux_protocol::ClientId;
use phux_protocol::wire::frame::{ControlAction, FrameKind, ReportedAgentState, TerminalSignal};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

pub mod agent_session;
pub mod event_sink;
pub mod terminal;

pub use phux_core::ids::ResourceId;
pub use phux_core::resource::ResourceKind;

/// The wire tag for a kind the server serves (this crate is where the
/// domain and wire kind enums meet).
#[must_use]
pub const fn wire_kind(kind: ResourceKind) -> phux_protocol::ids::ResourceKind {
    match kind {
        ResourceKind::Terminal => phux_protocol::ids::ResourceKind::Terminal,
        ResourceKind::AgentSession => phux_protocol::ids::ResourceKind::AgentSession,
        // A domain kind with no tag yet is reported opaque (255 is
        // unallocated), never mistaken for a Terminal.
        _ => phux_protocol::ids::ResourceKind::Unknown { tag: u8::MAX },
    }
}

/// The domain kind a wire tag names, or `None` for one this build does not
/// serve (a typed refusal, not a decode failure).
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

/// Default capacity of the per-resource output broadcast.
pub const DEFAULT_OUTPUT_BROADCAST: usize = 256;

/// Process-wide override of [`DEFAULT_OUTPUT_BROADCAST`] (never zero).
static OUTPUT_BROADCAST_CAPACITY: AtomicUsize = AtomicUsize::new(DEFAULT_OUTPUT_BROADCAST);

/// Capacity for a new output broadcast; production always gets the
/// default.
#[must_use]
pub fn output_broadcast_capacity() -> usize {
    OUTPUT_BROADCAST_CAPACITY.load(Ordering::Relaxed).max(1)
}

/// Override [`output_broadcast_capacity`] for this process (tests force
/// lag cheaply; nextest isolates processes).
pub fn set_output_broadcast_capacity_for_test(capacity: usize) {
    OUTPUT_BROADCAST_CAPACITY.store(capacity.max(1), Ordering::Relaxed);
}

/// Depth of the core's request mailboxes (event subscription, control).
const CORE_MAILBOX: usize = 64;

// ---- output stream ----------------------------------------------------------

/// Continuity reason carried with an engine-generated full resync.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResyncReason {
    /// Authoritative geometry changed.
    Resize,
    /// A bounded output subscriber observed a sequence gap.
    OutboundGap,
    /// The child exited; this is the final grid, queued before
    /// `RESOURCE_CLOSED`.
    Exit,
}

/// One output pump on a pane: its owning client and wire stream, the same
/// pair control frames are routed by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ResyncTarget {
    /// Server-local client id of the pump's consumer.
    pub owner: u64,
    /// Stream the pump publishes its generations on.
    pub stream_id: phux_protocol::ids::StreamId,
    /// The generation being replaced. Owner and stream alone can collide
    /// across pump kinds, so this keeps a resync from reviving two pumps.
    pub bootstrap_id: phux_protocol::ids::BootstrapId,
}

/// Which pumps a [`PaneOutput::Resync`] is for. A reflow owes everyone; a
/// pump that fell behind is resynced alone so the others keep their
/// generation (resyncs are expensive).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResyncAudience {
    /// Every subscriber replaces its generation.
    Everyone,
    /// Only these pumps (several when they asked in one debounce window).
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

/// Payload of the per-resource output broadcast.
///
/// Each attach pump maps `Live` → `RESOURCE_OUTPUT`, `Resync` →
/// `TERMINAL_SNAPSHOT` (so the client mirror resizes to the carried dims
/// before repainting; raw output could never grow a pane), and `Control` →
/// an ordered control frame for the matching owner.
#[derive(Clone, Debug)]
pub enum PaneOutput {
    /// Live output chunk forwarded as `RESOURCE_OUTPUT`.
    Live {
        /// Resource-global, strictly increasing raw output sequence.
        seq: u64,
        /// Verbatim output bytes for this sequence.
        bytes: Bytes,
        /// When the producer read these bytes; a pump dequeuing long after
        /// has fallen behind in time.
        at: std::time::Instant,
    },
    /// Full post-reflow grid replay (with reset preamble) and the dims the
    /// client mirror must adopt.
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
    /// Ordered native control, sharing the sequence with [`Self::Live`] so a
    /// pump sees all prior output before invalidating a generation.
    Control {
        /// Server-local pump owner; other subscribers ignore this control.
        owner: u64,
        /// Fully-owned tombstone/control frame for the matching pump.
        frame: FrameKind,
    },
}

// ---- supervisory control ----------------------------------------------------

/// A supervisory request to a resource's engine (ADR-0033).
///
/// The input lease lives in `ServerState`; the engine emits `TerminalControl`
/// because it owns the lifecycle. Engines answer inapplicable variants with an
/// error where a reply exists.
#[derive(Debug)]
pub enum ControlRequest {
    /// The input lease changed; broadcast the new holder and action.
    LeaseChanged {
        /// The client now holding the lease, or `None` if released to `Open`.
        input_holder: Option<ClientId>,
        /// `Acquired` / `Seized` / `Released` / `Expired`.
        action: ControlAction,
        /// The acting client; `None` for a server-side release (TTL or
        /// revocation).
        actor: Option<ClientId>,
    },
    /// Something else wrote `phux.agent/v1` (ADR-0046 §E): clear the
    /// detector's edge filter so it republishes once. No-op without a
    /// detector.
    AgentRecordInvalidated,
    /// Bind the producer channel of an `AgentSession` child just spawned
    /// here (ADR-0103 §6). No reply.
    BindAgentSession {
        /// The child engine's append channel.
        append: mpsc::Sender<agent_session::AppendRequest>,
    },
    /// Feed an `AgentSession` child's state into the detector at the
    /// `Stream` rank (ADR-0103 §5); `None` is the `session_end` retraction.
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
    /// Append hook-reported state as a synthesized record on this
    /// Terminal's live `AgentSession` child (ADR-0103 §6); used only when a
    /// live child exists.
    SynthesizeAgentStateRecord {
        /// Hook-reported state to append.
        state: ReportedAgentState,
        /// Whether the record was accepted.
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Signal the pane's process group, update the lifecycle, and broadcast
    /// `TerminalControl`; `reply` carries delivery or an error.
    Signal {
        /// The signal to deliver.
        signal: TerminalSignal,
        /// The lease holder at the time of the signal, for the broadcast.
        input_holder: Option<ClientId>,
        /// The client requesting the signal.
        by: ClientId,
        /// The signal's `operation_id` (L1 §5.1.1).
        operation_id: Option<phux_protocol::ids::IdempotencyKey>,
        /// Delivery acknowledgement.
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// The child exited and the pane is retained (ADR-0124): report
    /// `Exited`, stop the detector, refuse input. No reply.
    Retire,
    /// Replace the exited child with a fresh default shell in place. Reply
    /// is the next-exit receiver.
    ReplaceChild {
        /// Default-shell command for the replacement child.
        command: ReplacementCommand,
        /// The next PTY-EOF receiver, or why the replacement could not start.
        reply: oneshot::Sender<Result<oneshot::Receiver<phux_core::process::ExitOutcome>, String>>,
    },
}

/// `CommandBuilder` wrapper so [`ControlRequest`] can stay `Debug`.
pub struct ReplacementCommand(pub portable_pty::CommandBuilder);

impl std::fmt::Debug for ReplacementCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ReplacementCommand")
    }
}

// ---- handle -----------------------------------------------------------------

/// A facet operation requested of a resource of another kind (wire:
/// `WRONG_RESOURCE_KIND`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("resource is {actual}, operation requires {required}")]
pub struct WrongResourceKind {
    /// The kind the operation needs.
    pub required: ResourceKind,
    /// The kind the resource actually is.
    pub actual: ResourceKind,
}

/// The kind-specific channel set of one resource, read only through
/// [`ResourceHandle`]'s accessors.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum ResourceFacetHandle {
    /// Terminal engine channels.
    Terminal(TerminalHandle),
    /// Agent-session engine channels plus provider and native id.
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

/// `Send + Clone` handle to one resource's engine (ADR-0014). Every field
/// but `facet` exists for every kind.
#[derive(Debug, Clone)]
pub struct ResourceHandle {
    /// Which engine is behind this handle.
    pub kind: ResourceKind,
    /// The resource this one is bound to, if any (immutable).
    pub parent: Option<ResourceId>,
    /// Output broadcast: live chunks, resyncs, and ordered control.
    pub output: broadcast::Sender<PaneOutput>,
    /// Register a consumer before its bootstrap goes out (ADR-0018).
    pub consumer_attach: mpsc::Sender<ConsumerAttachRequest>,
    /// Drop a consumer (detach or EOF cleanup); idempotent.
    pub consumer_detach: mpsc::Sender<ConsumerDetachRequest>,
    /// Inbound `FRAME_ACK`s for this resource.
    pub consumer_ack: mpsc::Sender<ConsumerAckRequest>,
    /// Graceful-upgrade handoff requests (ADR-0032).
    pub upgrade: mpsc::Sender<UpgradeHandleRequest>,
    /// Supervisory control (ADR-0033).
    pub control: mpsc::Sender<ControlRequest>,
    /// The kind-specific channels. Reach them through [`Self::terminal`].
    pub facet: ResourceFacetHandle,
}

impl ResourceHandle {
    /// The Terminal facet.
    ///
    /// # Errors
    ///
    /// [`WrongResourceKind`] when this resource is not a Terminal.
    pub const fn terminal(&self) -> Result<&TerminalHandle, WrongResourceKind> {
        match &self.facet {
            ResourceFacetHandle::Terminal(handle) => Ok(handle),
            _ => Err(WrongResourceKind {
                required: ResourceKind::Terminal,
                actual: self.facet.kind(),
            }),
        }
    }

    /// The `AgentSession` facet.
    ///
    /// # Errors
    ///
    /// [`WrongResourceKind`] when this resource is not an agent session.
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

/// The engine-side half of a resource: output sequence and broadcast,
/// lifecycle, and the control receiver, owned by one engine task.
pub struct ResourceCore {
    /// Which engine owns this core.
    pub(super) kind: ResourceKind,
    /// The resource this one is bound to, if any.
    pub(super) parent: Option<ResourceId>,
    /// Resource-global raw output sequence; advanced only by
    /// [`Self::next_seq`].
    pub(super) seq: u64,
    /// Output sender; the seed receiver was dropped, so `receiver_count()`
    /// counts live subscribers.
    pub(super) output_tx: broadcast::Sender<PaneOutput>,
    /// Fired once when the backing exits.
    pub(super) exit_notify: Option<oneshot::Sender<phux_core::process::ExitOutcome>>,
    /// Cancellation token the engine watches (dropping does not cancel).
    pub(super) token: CancellationToken,
    /// Supervisory control mailbox (ADR-0033).
    pub(super) control_rx: mpsc::Receiver<ControlRequest>,
}

/// Sender halves paired with a fresh [`ResourceCore`].
#[derive(Debug)]
pub struct ResourceCoreChannels {
    /// Output broadcast sender, cloned into the handle.
    pub output: broadcast::Sender<PaneOutput>,
    /// Supervisory control sender.
    pub control: mpsc::Sender<ControlRequest>,
    /// Fires with the exit outcome.
    pub exit_notify: oneshot::Receiver<phux_core::process::ExitOutcome>,
}

impl ResourceCore {
    /// Build a core for `kind` bound to `parent`, watching `token`.
    #[must_use]
    pub fn new(
        kind: ResourceKind,
        parent: Option<ResourceId>,
        token: CancellationToken,
        output_capacity: usize,
    ) -> (Self, ResourceCoreChannels) {
        let (output_tx, _seed_rx) = broadcast::channel(output_capacity);
        let (control_tx, control_rx) = mpsc::channel(CORE_MAILBOX);
        let (exit_tx, exit_rx) = oneshot::channel();
        let core = Self {
            kind,
            parent,
            seq: 0,
            output_tx: output_tx.clone(),
            exit_notify: Some(exit_tx),
            token,
            control_rx,
        };
        let channels = ResourceCoreChannels {
            output: output_tx,
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

    /// Advance the output sequence; `None` when exhausted (never wraps).
    pub const fn next_seq(&mut self) -> Option<u64> {
        match self.seq.checked_add(1) {
            Some(seq) => {
                self.seq = seq;
                Some(seq)
            }
            None => None,
        }
    }

    /// Report the backing's exit once.
    pub fn notify_exit(&mut self, outcome: phux_core::process::ExitOutcome) {
        if let Some(tx) = self.exit_notify.take() {
            let _ = tx.send(outcome);
        }
    }

    /// Arm a new exit oneshot after a child replacement.
    pub fn arm_exit_notify(&mut self) -> oneshot::Receiver<phux_core::process::ExitOutcome> {
        let (tx, rx) = oneshot::channel();
        self.exit_notify = Some(tx);
        rx
    }
}

impl std::fmt::Debug for ResourceCore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResourceCore")
            .field("kind", &self.kind)
            .field("parent", &self.parent)
            .field("seq", &self.seq)
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
        core.notify_exit(phux_core::process::ExitOutcome::exited(3));
        core.notify_exit(phux_core::process::ExitOutcome::signaled(9));
        assert_eq!(
            channels.exit_notify.blocking_recv(),
            Ok(phux_core::process::ExitOutcome::exited(3))
        );
    }
}
