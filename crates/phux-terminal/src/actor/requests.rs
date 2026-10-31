//! Request and reply types the Terminal engine serves, and the
//! [`TerminalHandle`] facet. Backing-agnostic channels live in
//! `crate::resource` and are re-exported here.

use crate::grid::SnapshotBytes;
use crate::{Outbound, TerminalInput};
use phux_protocol::ClientId;
use phux_protocol::ids::{BootstrapId, StreamId};
use tokio::sync::{mpsc, oneshot, watch};

pub use phux_resource::{
    ControlRequest, DEFAULT_OUTPUT_BROADCAST, PaneOutput, ResyncAudience, ResyncReason,
    ResyncTarget,
};

/// Bounded input admission credits and PTY writer handoff.
#[path = "requests_input.rs"]
pub mod input;
pub use input::{
    DEFAULT_INPUT_MAILBOX, INPUT_CREDITS, InputCredit, InputCreditPool, PTY_WRITER_QUEUE,
};
pub use input::{
    EncodedInputRequest, WriteCompletion, WriteCompletionSink, echo_probe_for, try_send_to_writer,
};

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[path = "requests_native.rs"]
mod native;
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
pub use native::{
    NativeBootstrapReply, NativeBootstrapRequest, NativeHistoryReply, NativeHistoryRequest,
    NativePublicationReply, NativePublicationRequest, NativeReleaseRequest,
};

/// Register a consumer with the actor (ADR-0018), sent by the ATTACH path.
///
/// For state-sync consumers the actor synthesizes the bootstrap snapshot,
/// primes the reference from the same terminal cut, and replies with both,
/// with no PTY event in between. Raw consumers only get the lifecycle entry.
#[derive(Debug)]
pub struct ConsumerAttachRequest {
    /// Key for the per-consumer state; must match later detach and ack
    /// routing.
    pub client_id: ClientId,
    /// Per-consumer outbound mailbox for tick-emitted frames.
    pub outbound: mpsc::Sender<Outbound>,
    /// Wire terminal id stamped on every emitted `ResourceOutput`.
    pub wire_terminal_id: u32,
    /// Logical protocol-0.7 subscription identity.
    pub stream_id: StreamId,
    /// Current replica generation for this subscription.
    pub bootstrap_id: BootstrapId,
    /// Whether the consumer negotiated `OutputMode::StateSync`; if so the
    /// tick serves it and the runtime suppresses its broadcast pump.
    pub wants_state_sync: bool,
    /// Scrollback for the state-sync bootstrap (ignored otherwise).
    pub state_sync_scrollback: Option<u32>,
    /// Image protocols supported by the bootstrap receiver.
    pub image_protocols: phux_protocol::caps::ImageProtocolSet,
    /// Maximum snapshot bytes this registration may allocate before replying.
    pub bootstrap_max_bytes: usize,
    /// Maximum bootstrap frames the caller can still retain.
    pub bootstrap_max_frames: usize,
    /// Negotiated maximum bytes per synthesized bootstrap chunk.
    pub bootstrap_chunk_bytes: usize,
    /// Use the loss-tolerant model (ADR-0042): the reference advances on
    /// `FRAME_ACK` instead of on emit, so dropped frames self-heal. Only
    /// meaningful with `wants_state_sync`.
    pub loss_tolerant: bool,
    /// Aggregate-attach gate: while false, no live state-sync frames.
    pub live_gate: watch::Receiver<bool>,
    /// Acknowledges the registration; dropping the receiver is benign.
    pub reply: oneshot::Sender<Result<ConsumerAttachOutcome, ConsumerAttachError>>,
}

/// Snapshot and actor cut produced atomically with state-sync registration.
#[derive(Debug)]
pub struct StateSyncBootstrap {
    /// Synthesized VT snapshot captured from the canonical terminal.
    pub snapshot: SnapshotBytes,
    /// Actor-global raw sequence included by that same cut.
    pub base_seq: u64,
}

/// Outcome of a [`ConsumerAttachRequest`]: whether the tick manages this
/// consumer (so the runtime suppresses its pump), plus any bootstrap.
#[derive(Debug)]
pub struct ConsumerAttachOutcome {
    /// `true` when this actor's tick is the sole live emitter.
    pub tick_managed: bool,
    /// Atomic synthesized bootstrap for a state-sync consumer.
    pub state_sync_bootstrap: Option<StateSyncBootstrap>,
}

/// Errors from registering a consumer.
#[derive(Debug, thiserror::Error)]
pub enum ConsumerAttachError {
    /// libghostty could not allocate the one-shot `RenderState`.
    #[error("libghostty allocation failed: {0}")]
    Ghostty(#[from] libghostty_vt::Error),
    /// Priming the per-consumer reference failed.
    #[error("reference priming failed: {0}")]
    Synth(#[from] crate::grid::SynthesisError),
    /// The actor-global sequence cannot represent a post-bootstrap frame.
    #[error("state-sync sequence exhausted")]
    SequenceExhausted,
}

/// Drop the per-consumer state for `client_id` (detach or EOF cleanup);
/// idempotent.
#[derive(Debug)]
pub struct ConsumerDetachRequest {
    /// Identifier whose [`super::ConsumerSyncState`] entry to remove.
    pub client_id: ClientId,
    /// Fired once the entry is gone; dropping the receiver is benign.
    pub reply: oneshot::Sender<()>,
}

/// Inbound `FRAME_ACK` for the state-sync loop, forwarded by the runtime
/// (which strips the terminal id). Unknown clients are ignored; no reply.
#[derive(Debug)]
pub struct ConsumerAckRequest {
    /// Identifier whose [`super::ConsumerSyncState`]'s dirty cache to evict.
    pub client_id: ClientId,
    /// Logical subscription being acknowledged.
    pub stream_id: StreamId,
    /// Replica generation being acknowledged.
    pub bootstrap_id: BootstrapId,
    /// Cumulative ack `seq` (SPEC §12.2); stale acks are dropped.
    pub seq: u64,
}

/// Request the pane's replay snapshot (ATTACH).
#[derive(Debug)]
pub struct SnapshotRequest {
    /// Scrollback: `None` viewport only, `Some(0)` all, `Some(n)` last `n`.
    pub scrollback: Option<u32>,
    /// Image protocols the receiver can replay; omitted before charging the source budget.
    pub image_protocols: phux_protocol::caps::ImageProtocolSet,
    /// Maximum aggregate snapshot bytes the actor may allocate.
    pub max_bytes: usize,
    /// Maximum frames the caller can still retain for this snapshot.
    pub max_frames: usize,
    /// Negotiated maximum bytes per synthesized bootstrap chunk.
    pub chunk_bytes: usize,
    /// Snapshot plus its raw cut; dropping the receiver is benign.
    pub reply: oneshot::Sender<Result<(SnapshotBytes, u64), crate::grid::SynthesisError>>,
}

/// Install an interactive client's default palette; later OSC 10/11
/// queries see it once acknowledged.
#[derive(Debug)]
pub struct SetDefaultColorsRequest {
    /// Outer terminal defaults to install on the canonical emulator.
    pub colors: phux_protocol::caps::TerminalDefaultColors,
    /// Completion acknowledgement.
    pub reply: oneshot::Sender<()>,
}

/// Project the pane's screen (`GET_SCREEN`, ADR-0022); read-only.
#[derive(Debug)]
pub struct ScreenRequest {
    /// Wire-local pane id to stamp into the projected `ScreenState`.
    pub pane: u32,
    /// Scrollback: `None`, `Some(0)` all, or `Some(n)` rows.
    pub scrollback: Option<u32>,
    /// Populate [`phux_core::screen::ScreenState::cells`].
    pub cells: bool,
    /// Formatter rendering: `0` none, `1` HTML, `2` VT (others refused).
    pub format: u8,
    /// Reply channel; dropping the receiver is benign.
    pub reply: oneshot::Sender<ScreenReply>,
}

/// Reply to a [`ScreenRequest`]: the projection (render failures ride in
/// `rendered_error`), or [`Self::TooLarge`] when the rendering would exceed
/// the budget and must be refused as `RESOURCE_EXHAUSTED`.
#[derive(Debug)]
pub enum ScreenReply {
    /// The projection, boxed to keep the enum small.
    Projection(Box<phux_core::screen::ScreenState>),
    /// The requested rendering exceeds the per-read budget.
    TooLarge {
        /// The Formatter's measured byte count.
        required_bytes: usize,
        /// The server's per-read budget the request exceeded.
        budget_bytes: usize,
    },
}

pub use phux_resource::handoff::{PaneUpgradeHandle, UpgradeHandleRequest};

/// Request the pane's live cwd from the kernel.
///
/// Serves `defaults.cwd-inheritance = inherit-focused`. libghostty's
/// `Terminal::pwd` never populates from OSC 7, so it is not used. `None`
/// when there is no PTY, no pid, or the query fails.
#[derive(Debug)]
pub struct PwdRequest {
    /// Reply channel (`None`: no resolvable cwd).
    pub reply: oneshot::Sender<Option<String>>,
}

/// Request the typed process facet (`GET_TERMINAL_STATE`): child and start
/// time, foreground group, cwd, prompt state, and exit. Served after EOF
/// too; unobtainable facts are `None`.
#[derive(Debug)]
pub struct ProcessFacetRequest {
    /// Reply channel; dropping the receiver is benign.
    pub reply: oneshot::Sender<phux_core::process::TerminalProcessState>,
}

#[path = "requests_resize.rs"]
mod resize;
pub use resize::*;

/// The Terminal facet of a [`ResourceHandle`](phux_resource::ResourceHandle).
///
/// `Send + Clone`; obtained only via [`TerminalResource::terminal`], so
/// Terminal-only requests are refused for other kinds at one seam.
#[derive(Debug, Clone)]
pub struct TerminalHandle {
    /// Register a consumer before its bootstrap goes out (ADR-0018).
    pub consumer_attach: mpsc::Sender<ConsumerAttachRequest>,
    /// Drop a consumer (detach or EOF cleanup); idempotent.
    pub consumer_detach: mpsc::Sender<ConsumerDetachRequest>,
    /// Inbound `FRAME_ACK`s for this resource.
    pub consumer_ack: mpsc::Sender<ConsumerAckRequest>,
    /// Graceful-upgrade handoff requests (ADR-0032).
    pub upgrade: mpsc::Sender<UpgradeHandleRequest>,
    /// Input events, encoded by the actor and written to the PTY.
    pub input: mpsc::Sender<TerminalInput>,
    /// Bytes pre-encoded by the input lane (`try_send`, never blocks).
    pub encoded_input: mpsc::Sender<EncodedInputRequest>,
    /// Credits for `encoded_input` (ADR-0144): each request sent there holds
    /// one until written, so that `try_send` never finds the mailbox full.
    pub input_credits: InputCreditPool,
    /// Latest input-encoder state, captured after every terminal mutation.
    pub input_snapshot: tokio::sync::watch::Receiver<crate::input::InputEncoderSnapshot>,
    /// Snapshot requests (ATTACH).
    pub snapshot: mpsc::Sender<SnapshotRequest>,
    /// Actor-serialized native checkpoint capture.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub native_bootstrap: mpsc::Sender<NativeBootstrapRequest>,
    /// Actor-serialized transition from staged READY to replay/live delivery.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub native_publication: mpsc::Sender<NativePublicationRequest>,
    /// Actor-serialized native history paging.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub native_history: mpsc::Sender<NativeHistoryRequest>,
    /// Actor-serialized detach cleanup for retained native cuts.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub native_release: mpsc::Sender<NativeReleaseRequest>,
    /// Host-palette updates; the latest reporting client wins.
    pub set_default_colors: mpsc::Sender<SetDefaultColorsRequest>,
    /// Structured screen reads (`GET_SCREEN`).
    pub screen: mpsc::Sender<ScreenRequest>,
    /// Live cwd reads (see [`PwdRequest`]).
    pub pwd: mpsc::Sender<PwdRequest>,
    /// Process-facet reads (see [`ProcessFacetRequest`]).
    pub process: mpsc::Sender<ProcessFacetRequest>,
    /// Resize requests (see [`ResizeRequest`]).
    pub resize: ResizeSender,
    /// Pane viewport width in cells at construction time.
    pub cols: u16,
    /// Pane viewport height in cells at construction time.
    pub rows: u16,
    /// Present when this pane has a live PTY writer. The receive task writes
    /// a legacy printable key here instead of waking the input lane. Weak, so
    /// a retained pane whose PTY was released holds no master descriptor.
    pub fast_pty: Option<std::sync::Weak<super::fast_pty::PtyFastPath>>,
}

impl phux_resource::ResourceFacet for TerminalHandle {
    const KIND: phux_resource::ResourceKind = phux_resource::ResourceKind::Terminal;
}

/// The Terminal facet of a [`phux_resource::ResourceHandle`].
pub trait TerminalResource {
    /// The Terminal facet.
    ///
    /// # Errors
    ///
    /// [`phux_resource::WrongResourceKind`] when this resource is not a
    /// Terminal.
    fn terminal(&self) -> Result<&TerminalHandle, phux_resource::WrongResourceKind>;
}

impl TerminalResource for phux_resource::ResourceHandle {
    fn terminal(&self) -> Result<&TerminalHandle, phux_resource::WrongResourceKind> {
        self.facet::<TerminalHandle>()
    }
}

impl TerminalHandle {
    /// A facet whose senders are all disconnected (tests).
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn detached_for_test(cols: u16, rows: u16) -> Self {
        Self {
            consumer_attach: mpsc::channel(1).0,
            consumer_detach: mpsc::channel(1).0,
            consumer_ack: mpsc::channel(1).0,
            upgrade: mpsc::channel(1).0,
            input: mpsc::channel(1).0,
            encoded_input: mpsc::channel(1).0,
            input_credits: InputCreditPool::default(),
            input_snapshot: watch::channel(crate::input::InputEncoderSnapshot::default()).1,
            snapshot: mpsc::channel(1).0,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            native_bootstrap: mpsc::channel(1).0,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            native_publication: mpsc::channel(1).0,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            native_history: mpsc::channel(1).0,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            native_release: mpsc::channel(1).0,
            set_default_colors: mpsc::channel(1).0,
            screen: mpsc::channel(1).0,
            pwd: mpsc::channel(1).0,
            process: mpsc::channel(1).0,
            resize: ResizeSender::channel(1).0,
            cols,
            rows,
            fast_pty: None,
        }
    }
}
