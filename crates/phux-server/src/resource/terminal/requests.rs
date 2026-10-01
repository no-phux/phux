//! Request and reply types the Terminal engine serves, and the
//! [`TerminalHandle`] facet. Backing-agnostic channels live in
//! [`crate::resource`] and are re-exported here.

use std::os::fd::OwnedFd;

use crate::grid::SnapshotBytes;
use crate::mailbox::{Outbound, TerminalInput};
use bytes::Bytes;
use phux_protocol::ClientId;
use phux_protocol::ids::{BootstrapId, StreamId};
use phux_protocol::wire::frame::FrameKind;
use tokio::sync::{broadcast, mpsc, oneshot, watch};

pub use crate::resource::{
    ControlRequest, DEFAULT_OUTPUT_BROADCAST, PaneOutput, ResyncAudience, ResyncReason,
    ResyncTarget,
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

/// Default depth of the per-pane input mailbox.
pub const DEFAULT_INPUT_MAILBOX: usize = 64;

/// Input credits per pane (ADR-0144): credited requests in flight between
/// their sender and the end of their `write(2)`.
///
/// Equal to the encoded-input mailbox depth, so a credited `try_send` never
/// finds the mailbox full.
pub const INPUT_CREDITS: usize = DEFAULT_INPUT_MAILBOX;

/// Writer-queue slots that only uncredited terminal replies may use.
const TERMINAL_REPLY_HEADROOM: usize = 16;

/// PTY writer queue depth: every input credit plus the reply headroom, so a
/// credited request forwarded by the actor always finds a slot.
pub const PTY_WRITER_QUEUE: usize = INPUT_CREDITS + TERMINAL_REPLY_HEADROOM;

/// One pane's input credits (ADR-0144). Cloning shares the pool.
#[derive(Debug, Clone)]
pub struct InputCreditPool(std::sync::Arc<tokio::sync::Semaphore>);

impl Default for InputCreditPool {
    fn default() -> Self {
        Self(std::sync::Arc::new(tokio::sync::Semaphore::new(
            INPUT_CREDITS,
        )))
    }
}

impl InputCreditPool {
    /// A credit, if one is free now.
    #[must_use]
    pub fn try_take(&self) -> Option<InputCredit> {
        self.0.try_acquire().ok()?.forget();
        Some(InputCredit { pool: self.clone() })
    }

    /// A credit, waiting until one is returned.
    pub async fn take(&self) -> InputCredit {
        // The pool is never closed, so `acquire` only fails if it were.
        if let Ok(permit) = self.0.acquire().await {
            permit.forget();
        }
        InputCredit { pool: self.clone() }
    }

    /// Credits free right now.
    #[must_use]
    pub fn available(&self) -> usize {
        self.0.available_permits()
    }

    fn is(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
}

/// One held pane input credit, returned to its pool when dropped.
#[derive(Debug)]
pub struct InputCredit {
    pool: InputCreditPool,
}

impl InputCredit {
    /// Whether this credit was drawn from `pool`.
    #[must_use]
    pub fn is_from(&self, pool: &InputCreditPool) -> bool {
        self.pool.is(pool)
    }
}

impl Drop for InputCredit {
    fn drop(&mut self) {
        self.pool.0.add_permits(1);
    }
}

/// Queue `request` for the PTY writer. An uncredited request (a terminal
/// reply, or input from the no-lane path) may not take a slot reserved for
/// credited input, so it is refused as `Full` once only those remain.
///
/// # Errors
///
/// The request back, as `try_send` would return it.
pub(crate) fn try_send_to_writer(
    tx: &mpsc::Sender<EncodedInputRequest>,
    mut request: EncodedInputRequest,
) -> Result<(), mpsc::error::TrySendError<EncodedInputRequest>> {
    let result = if request.credit.is_none() && tx.capacity() <= INPUT_CREDITS {
        Err(mpsc::error::TrySendError::Full(request))
    } else {
        request.writer_queued_at = Some(std::time::Instant::now());
        tx.try_send(request)
    };
    if matches!(&result, Err(mpsc::error::TrySendError::Full(_))) {
        crate::perf::INPUT_WRITER_FULL.incr();
    }
    result
}

/// Final disposition of a PTY write, reported on
/// [`EncodedInputRequest::completion`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WriteCompletion {
    /// `write_all` and `flush` both succeeded.
    Delivered,
    /// Failed after bytes may have landed; delivery is indeterminate.
    Failed,
    /// Refused before writing: the canonical-mode line discipline would have
    /// truncated a line longer than `limit` and possibly wedged the pane.
    CanonicalLimitExceeded { limit: usize },
    /// Never reached a live writer, so `write(2)` was provably not called
    /// (no PTY, full or closed queue). Safe to resubmit.
    NotWritten,
}

/// One-shot report path for an acknowledged write's [`WriteCompletion`],
/// tagging the outcome with its admission-queue ticket.
///
/// Dropping an unfired sink reports [`WriteCompletion::Failed`]; sites that
/// can prove nothing was written report [`WriteCompletion::NotWritten`].
pub(crate) struct WriteCompletionSink {
    notify: Option<Box<dyn FnOnce(WriteCompletion) + Send>>,
}

impl WriteCompletionSink {
    pub(crate) fn new(notify: impl FnOnce(WriteCompletion) + Send + 'static) -> Self {
        Self {
            notify: Some(Box::new(notify)),
        }
    }

    /// Report `outcome` once; consuming the sink disarms the `Drop` fallback.
    pub(crate) fn complete(mut self, outcome: WriteCompletion) {
        if let Some(notify) = self.notify.take() {
            notify(outcome);
        }
    }

    /// A sink backed by a plain channel (tests).
    #[cfg(test)]
    pub(crate) fn channel() -> (Self, std::sync::mpsc::Receiver<WriteCompletion>) {
        let (tx, rx) = std::sync::mpsc::channel();
        (
            Self::new(move |outcome| {
                let _ = tx.send(outcome);
            }),
            rx,
        )
    }
}

impl Drop for WriteCompletionSink {
    fn drop(&mut self) {
        if let Some(notify) = self.notify.take() {
            notify(WriteCompletion::Failed);
        }
    }
}

impl std::fmt::Debug for WriteCompletionSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriteCompletionSink")
            .field("armed", &self.notify.is_some())
            .finish()
    }
}

/// Bytes for the PTY writer, optionally with a completion report.
#[derive(Debug)]
pub(crate) struct EncodedInputRequest {
    /// Fully encoded PTY bytes.
    pub(crate) bytes: Bytes,
    /// Whether this may arm the `echo.server` sample (keys and pastes, not
    /// mouse, focus, or terminal replies).
    pub(crate) echo_probe: bool,
    /// See [`WriteCompletionSink`]; dropping it reports indeterminate.
    pub(crate) completion: Option<WriteCompletionSink>,
    /// The pane input credit this request holds (ADR-0144), released when
    /// the request drops: after the writer thread's `write(2)`, or at
    /// whichever step discards it.
    pub(crate) credit: Option<InputCredit>,
    /// Stamped only at the final writer handoff, not the lane/actor queue.
    pub(crate) writer_queued_at: Option<std::time::Instant>,
}

impl EncodedInputRequest {
    pub(crate) fn legacy(bytes: Vec<u8>) -> Self {
        Self::legacy_probe(bytes, true)
    }

    /// [`Self::legacy`] with an explicit echo-probe flag.
    pub(crate) fn legacy_probe(bytes: Vec<u8>, echo_probe: bool) -> Self {
        Self {
            bytes: bytes.into(),
            echo_probe,
            completion: None,
            credit: None,
            writer_queued_at: None,
        }
    }

    /// This request, holding `credit` until it is written or discarded.
    #[must_use]
    pub(crate) fn with_credit(mut self, credit: InputCredit) -> Self {
        self.credit = Some(credit);
        self
    }

    pub(crate) fn acknowledged(bytes: Vec<u8>, completion: WriteCompletionSink) -> Self {
        Self {
            bytes: Bytes::from(bytes),
            echo_probe: true,
            completion: Some(completion),
            credit: None,
            writer_queued_at: None,
        }
    }
}

/// Whether this input kind arms the `echo.server` sample.
pub(crate) const fn echo_probe_for(input: &TerminalInput) -> bool {
    matches!(input, TerminalInput::Key(_) | TerminalInput::Paste(_))
}

/// Request the pane's replay snapshot (ATTACH).
#[derive(Debug)]
pub struct SnapshotRequest {
    /// Scrollback: `None` viewport only, `Some(0)` all, `Some(n)` last `n`.
    pub scrollback: Option<u32>,
    /// Maximum aggregate snapshot bytes the actor may allocate.
    pub max_bytes: usize,
    /// Maximum frames the caller can still retain for this snapshot.
    pub max_frames: usize,
    /// Negotiated maximum bytes per synthesized bootstrap chunk.
    pub chunk_bytes: usize,
    /// Snapshot plus its raw cut; dropping the receiver is benign.
    pub reply: oneshot::Sender<Result<(SnapshotBytes, u64), crate::grid::SynthesisError>>,
}

/// Fully-owned native prefix captured atomically by the terminal actor.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[derive(Debug)]
pub struct NativeBootstrapReply {
    /// Bounded BEGIN/CHUNK/READY sequence for the per-client pump to publish.
    pub frames: Vec<FrameKind>,
    /// Heap retained by the opaque payloads, charged to the connection's
    /// bootstrap staging budget.
    pub retained_bytes: usize,
    /// Actor-global raw output cut included by the checkpoint.
    pub base_seq: u64,
    /// Actor-private generation key used only for the publication fence.
    pub(crate) publication_cursor: crate::native_state::OpaqueHistoryCursor,
}

/// Live bytes after a native checkpoint fence, plus the receiver installed
/// atomically after the replay cut.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[derive(Debug)]
pub struct NativePublicationReply {
    pub(crate) replay: Vec<(u64, Bytes)>,
    pub(crate) live: broadcast::Receiver<PaneOutput>,
}

/// Complete the `READY`/`ATTACH_READY` publication fence for one owner.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[derive(Debug)]
pub struct NativePublicationRequest {
    /// Server-local lease owner completing publication.
    pub owner: u64,
    /// Wire terminal identity for the published replica.
    pub terminal_id: phux_protocol::ids::ResourceId,
    /// Logical stream identity for the published replica.
    pub stream_id: StreamId,
    /// Replica generation whose READY frame was just sent.
    pub bootstrap_id: BootstrapId,
    /// Authenticated actor-private generation cursor returned with READY.
    pub cursor: crate::native_state::OpaqueHistoryCursor,
    /// Completion carrying replay bytes and the post-replay live receiver.
    pub reply:
        oneshot::Sender<Result<NativePublicationReply, crate::native_state::NativeStateError>>,
}

/// Native checkpoint capture request serialized by the terminal actor.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[derive(Debug)]
pub struct NativeBootstrapRequest {
    /// Server-local owner identity used for detach and TTL cleanup.
    pub owner: u64,
    /// Wire terminal identity for this subscription.
    pub terminal_id: phux_protocol::ids::ResourceId,
    /// Logical stream identity.
    pub stream_id: StreamId,
    /// Replica generation identity.
    pub bootstrap_id: BootstrapId,
    /// Negotiated payload limits.
    pub limits: phux_protocol::caps::BootstrapLimits,
    /// Remaining connection-wide opaque byte budget.
    pub max_bytes: usize,
    /// Remaining connection-wide frame budget.
    pub max_frames: usize,
    /// Atomic capture result. The pump alone publishes the returned frames.
    pub reply: oneshot::Sender<Result<NativeBootstrapReply, crate::native_state::NativeStateError>>,
}

/// Actor result paired with the caller's still-owned outbound permit.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[derive(Debug)]
pub struct NativeHistoryReply {
    /// Permit proving mailbox capacity was reserved before actor advancement.
    pub permit: mpsc::OwnedPermit<Outbound>,
    /// Fully-owned response frame, or an invalid request/host failure.
    pub result: Result<FrameKind, crate::native_state::NativeStateError>,
}

/// One bounded native history step routed to the owning terminal actor.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[derive(Debug)]
pub struct NativeHistoryRequest {
    /// Reserved client-mailbox capacity. The actor returns but never consumes it.
    pub permit: mpsc::OwnedPermit<Outbound>,
    /// Server-local owner identity authenticated against the retained cut.
    pub owner: u64,
    /// Wire terminal identity for this subscription.
    pub terminal_id: phux_protocol::ids::ResourceId,
    /// Logical stream identity.
    pub stream_id: StreamId,
    /// Replica generation identity.
    pub bootstrap_id: BootstrapId,
    /// Stable opaque capability echoed by the client.
    pub cursor: Bytes,
    /// Requested non-zero response byte bound.
    pub max_bytes: u32,
    /// Requested non-zero history row bound.
    pub max_rows: u32,
    /// Negotiated connection bounds.
    pub limits: phux_protocol::caps::BootstrapLimits,
    /// Actor result; the pump consumes the returned permit.
    pub reply: oneshot::Sender<NativeHistoryReply>,
}
/// Release every retained native history cut owned by one detached client.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[derive(Debug)]
pub struct NativeReleaseRequest {
    /// Server-local owner identity.
    pub owner: u64,
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

/// Request a pane's graceful-upgrade handoff (ADR-0032); read-only.
#[derive(Debug)]
pub struct UpgradeHandleRequest {
    /// Reply channel; dropping the receiver is benign.
    pub reply: oneshot::Sender<PaneUpgradeHandle>,
}

/// One pane's upgrade handoff: PTY descriptors to re-adopt plus the replay
/// snapshot. `master_fd`/`child_pid` are `None` without a PTY (skipped).
#[derive(Debug)]
pub struct PaneUpgradeHandle {
    /// Owned duplicate of the master fd, taken under the master lock.
    pub master_fd: Option<OwnedFd>,
    /// Child PID on the slave side, re-adopted via `waitpid` after the exec.
    pub child_pid: Option<i32>,
    /// Current grid width in cells.
    pub cols: u16,
    /// Current grid height in cells.
    pub rows: u16,
    /// Cell size in pixels, if a client reported one.
    pub cell_px: Option<(u16, u16)>,
    /// Current pane title, if the child set one.
    pub title: Option<String>,
    /// Live cwd, falling back to the last known one.
    pub cwd: Option<String>,
    /// Replayable viewport snapshot.
    pub vt_replay_bytes: Vec<u8>,
    /// Replayable scrollback that precedes the viewport, or empty.
    pub scrollback_bytes: Vec<u8>,
}

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

/// A resize request.
///
/// `resync_clients` re-broadcasts a snapshot after a live reflow (clients
/// reflow independently and may diverge); it is `false` for the attach-time
/// resize, whose handshake snapshot already covers it.
#[derive(Debug, Clone, Copy)]
pub struct ResizeRequest {
    /// New grid width in cells.
    pub cols: u16,
    /// New grid height in cells.
    pub rows: u16,
    /// Donor cell size in pixels. `None` keeps the last-known size, so a
    /// pixel-less resize cannot zero established geometry.
    pub cell_px: Option<(u16, u16)>,
    /// Re-broadcast a snapshot after the reflow.
    pub resync_clients: bool,
    /// Skip the resize and only schedule a resync (a lagged pump's
    /// request); geometry fields are ignored.
    pub resync_only: bool,
    /// With `resync_only`, the one pump owed the resync; `None` means
    /// everyone.
    pub resync_for: Option<ResyncTarget>,
}

/// Latest geometry plus a bounded, lossless-under-backpressure recovery queue.
///
/// Geometry is state, not a work item: replacing it must never lose the final
/// size, pixel donor, or an already-owed live resync. Gap recovery remains
/// ordered and bounded independently, including its target generation.
#[derive(Debug, Clone)]
pub struct ResizeSender {
    geometry: std::sync::Arc<std::sync::Mutex<Option<ResizeRequest>>>,
    wake: mpsc::Sender<()>,
    recovery: mpsc::Sender<ResizeRequest>,
}

/// Actor-side receiver for [`ResizeSender`].
#[derive(Debug)]
pub struct ResizeReceiver {
    geometry: std::sync::Arc<std::sync::Mutex<Option<ResizeRequest>>>,
    wake: mpsc::Receiver<()>,
    recovery: mpsc::Receiver<ResizeRequest>,
}

impl ResizeSender {
    /// Keep at most one pending geometry and `capacity` recovery requests.
    #[must_use]
    pub fn channel(capacity: usize) -> (Self, ResizeReceiver) {
        let geometry = std::sync::Arc::new(std::sync::Mutex::new(None));
        let (wake, wake_rx) = mpsc::channel(1);
        let (recovery, recovery_rx) = mpsc::channel(capacity);
        (
            Self {
                geometry: geometry.clone(),
                wake,
                recovery,
            },
            ResizeReceiver {
                geometry,
                wake: wake_rx,
                recovery: recovery_rx,
            },
        )
    }

    /// Geometry never fails for lack of capacity. Recovery callers retain
    /// the ordinary bounded-mailbox contract.
    pub fn try_send(
        &self,
        mut request: ResizeRequest,
    ) -> Result<(), mpsc::error::TrySendError<ResizeRequest>> {
        if request.resync_only {
            return self.recovery.try_send(request);
        }
        let mut pending = self
            .geometry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.wake.try_send(()) == Err(mpsc::error::TrySendError::Closed(())) {
            return Err(mpsc::error::TrySendError::Closed(request));
        }
        if let Some(previous) = pending.as_ref() {
            request.cell_px = request.cell_px.or(previous.cell_px);
            request.resync_clients |= previous.resync_clients;
        }
        *pending = Some(request);
        drop(pending);
        Ok(())
    }

    /// Await room only for recovery; live geometry is coalesced immediately.
    pub async fn send(
        &self,
        request: ResizeRequest,
    ) -> Result<(), mpsc::error::SendError<ResizeRequest>> {
        if request.resync_only {
            return self.recovery.send(request).await;
        }
        self.try_send(request)
            .map_err(|error| mpsc::error::SendError(error.into_inner()))
    }

    /// Whether the actor has dropped its receiver.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.wake.is_closed()
    }
}

impl ResizeReceiver {
    fn take_geometry(&self) -> Option<ResizeRequest> {
        self.geometry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    /// Geometry precedes recovery so the replacement cut has the latest size.
    pub async fn recv(&mut self) -> Option<ResizeRequest> {
        loop {
            tokio::select! {
                biased;
                Some(()) = self.wake.recv() => {
                    if let Some(request) = self.take_geometry() {
                        return Some(request);
                    }
                }
                Some(request) = self.recovery.recv() => return Some(request),
                else => return None,
            }
        }
    }

    /// Drain geometry first, retaining every queued targeted recovery.
    pub fn try_recv(&mut self) -> Result<ResizeRequest, mpsc::error::TryRecvError> {
        while self.wake.try_recv().is_ok() {
            if let Some(request) = self.take_geometry() {
                return Ok(request);
            }
        }
        self.recovery.try_recv()
    }
}

/// The Terminal facet of a [`ResourceHandle`](crate::resource::ResourceHandle).
///
/// `Send + Clone`; obtained only via
/// [`ResourceHandle::terminal`](crate::resource::ResourceHandle::terminal),
/// so Terminal-only requests are refused for other kinds at one seam.
#[derive(Debug, Clone)]
pub struct TerminalHandle {
    /// Input events, encoded by the actor and written to the PTY.
    pub input: mpsc::Sender<TerminalInput>,
    /// Bytes pre-encoded by the input lane (`try_send`, never blocks).
    pub(crate) encoded_input: mpsc::Sender<EncodedInputRequest>,
    /// Credits for `encoded_input` (ADR-0144): each request sent there holds
    /// one until written, so that `try_send` never finds the mailbox full.
    pub(crate) input_credits: InputCreditPool,
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
}

impl TerminalHandle {
    /// A facet whose senders are all disconnected (tests).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn detached_for_test(cols: u16, rows: u16) -> Self {
        Self {
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
        }
    }
}
