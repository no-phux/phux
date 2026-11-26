//! A relay session's state: subscribers, pending requests, bootstrap flows, and their budgets.

use super::{
    Arc, AtomicU64, BootstrapId, BootstrapLimits, BootstrapProfile, BootstrapStreamProfile,
    BytesMut, CancellationToken, ClientId, CommandResult, ContentAttach, DirectoryListingResult,
    FrameKind, HashMap, HashSet, Outbound, PathQueryResult, ResourceId, ResourceKindCatalog,
    SatelliteHost, ServerFeatureExtSet, ServerFeatureSet, SpawnResult, StreamId, VecDeque, mpsc,
    oneshot,
};
use futures_util::FutureExt;

/// One proxy subscriber on the return leg.
#[derive(Debug)]
pub(super) struct ProxySubscriber {
    pub(super) client: ClientId,
    pub(super) out_tx: mpsc::Sender<Outbound>,
    pub(super) consumer_cancel: CancellationToken,
    /// Token of the registration held for this `(terminal, client)`.
    pub(super) seq: u64,
    /// Snapshot-ordering gate; see [`SnapshotGate`].
    pub(super) gate: SnapshotGate,
    /// Remove only after the queued `RESOURCE_CLOSED` is delivered.
    pub(super) retire_after_flush: bool,
    /// Latest generation identity, so overflow can emit a terminal-scoped
    /// tombstone.
    pub(super) current_generation: Option<(StreamId, BootstrapId, u64)>,
    /// Its place in line for a mailbox slot, while delivery is blocked.
    pub(super) turn: DeliveryTurn,
    /// The content attach to repeat; `None` for an event-only subscription.
    pub(super) content: Option<ContentAttach>,
}

impl ProxySubscriber {
    /// Deliver one frame, waiting in line behind a full mailbox.
    pub(super) fn deliver(
        &mut self,
        frame: &FrameKind,
    ) -> Result<(), mpsc::error::TrySendError<()>> {
        self.turn.send(&self.out_tx, frame)
    }
}

/// A place in a consumer mailbox's waiting line. A bare `try_send` never wins
/// a slot while the consumer's other producers wait for one, so a few busy
/// panes could keep a relayed bootstrap out for as long as they stayed busy.
/// Polled, never awaited: the link must not stall on one consumer.
// ponytail: one slot per turn; reserve the whole retained run if a slow
// consumer's bootstrap ever takes visibly long.
#[derive(Default)]
pub(super) struct DeliveryTurn(Option<PendingSlot>);

type PendingSlot = std::pin::Pin<
    Box<
        dyn Future<Output = Result<mpsc::OwnedPermit<Outbound>, mpsc::error::SendError<()>>> + Send,
    >,
>;

impl std::fmt::Debug for DeliveryTurn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("DeliveryTurn")
            .field(&self.0.is_some())
            .finish()
    }
}

impl DeliveryTurn {
    fn send(
        &mut self,
        out_tx: &mpsc::Sender<Outbound>,
        frame: &FrameKind,
    ) -> Result<(), mpsc::error::TrySendError<()>> {
        if self.0.is_none() {
            match out_tx.try_reserve() {
                Ok(permit) => {
                    permit.send(Outbound::Frame(frame.clone()));
                    return Ok(());
                }
                Err(mpsc::error::TrySendError::Closed(())) => {
                    return Err(mpsc::error::TrySendError::Closed(()));
                }
                Err(mpsc::error::TrySendError::Full(())) => {}
            }
        }
        let slot = self
            .0
            .get_or_insert_with(|| Box::pin(out_tx.clone().reserve_owned()));
        match slot.now_or_never() {
            None => Err(mpsc::error::TrySendError::Full(())),
            Some(reserved) => {
                self.0 = None;
                let permit = reserved.map_err(|_| mpsc::error::TrySendError::Closed(()))?;
                let _ = permit.send(Outbound::Frame(frame.clone()));
                Ok(())
            }
        }
    }
}

/// Per-subscriber return-leg gate: content deltas flow only after the
/// subscriber's own attach snapshot lands, without stalling the
/// link for one slow consumer.
#[derive(Debug)]
pub(super) enum SnapshotGate {
    /// Attached, but its own snapshot has not been delivered: deltas are
    /// suppressed, so a second attacher never sees the ongoing stream's
    /// output first. The first snapshot moves to `Open` (or `Retained`).
    AwaitingFirst,
    /// Snapshot delivered (or event-only): deltas flow.
    Open,
    /// Bootstrap frames a full mailbox refused, bounded per subscriber and
    /// per connection; exceeding either reaps the subscriber.
    Retained {
        frames: VecDeque<FrameKind>,
        retained_bytes: usize,
        open_after: bool,
    },
    /// Delivery overflowed. The subscriber stays until its resource-scoped
    /// failure is delivered or the deadline passes. Live streams detach
    /// upstream; an already-closed resource does not.
    Retiring {
        failure: FrameKind,
        deadline: std::time::Instant,
        detach_upstream: bool,
    },
}

pub(super) struct DeliveryFailure {
    pub(super) frame: FrameKind,
    pub(super) detach_upstream: bool,
}

#[derive(Clone, Copy)]
pub(super) enum FanOutKind {
    Bootstrap {
        open_after: bool,
        replace_legacy_begin: bool,
    },
    Content,
}

pub(super) struct FanOutFrame<'a> {
    pub(super) host: &'a SatelliteHost,
    pub(super) terminal: u32,
    pub(super) frame: &'a FrameKind,
    pub(super) kind: FanOutKind,
    /// Deliver a generation's frames only to subscribers that got its BEGIN.
    pub(super) begun_only: bool,
}

/// One in-flight relayed command, plus what to roll back on an error reply.
#[derive(Debug)]
pub(super) struct PendingCommand {
    pub(super) reply: oneshot::Sender<CommandResult>,
    pub(super) owner: Option<tokio_util::sync::CancellationToken>,
    /// The terminal an `ATTACH_RESOURCE` opens; with no rider, the hub's own.
    pub(super) attach: Option<u32>,
    pub(super) subscription: Option<PendingProxyRegistration>,
}

#[derive(Debug)]
pub(super) struct PendingProxyRegistration {
    pub(super) terminal: u32,
    pub(super) client: ClientId,
    pub(super) effect: Registration,
    pub(super) seq: u64,
    pub(super) content: bool,
}

/// What registering a subscription rider did, for precise rollback.
#[derive(Debug, Clone, Copy)]
pub(super) enum Registration {
    /// A new `(terminal, client)` subscriber; an error removes it.
    New,
    /// A re-subscribe. If it upgraded an `Open` subscriber to a snapshot
    /// attach it re-gated it, and an error restores `Open` if still
    /// `AwaitingFirst` (a snapshot retained meanwhile stays gated). If it was
    /// the first content attach, an error leaves an event-only subscriber.
    Existing { regated: bool, first_content: bool },
}

/// One in-flight bootstrap generation on the return leg.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RelayBootstrapFlow {
    pub(super) stream_id: StreamId,
    /// The id the satellite gave this generation.
    pub(super) bootstrap_id: BootstrapId,
    /// The id consumers see it under; see [`RelaySession::generations`].
    pub(super) outward: BootstrapId,
    /// When the satellite last sent part of it.
    pub(super) progress_at: std::time::Instant,
    /// Last sequence the satellite sent under it.
    pub(super) last_seq: u64,
    pub(super) profile: BootstrapStreamProfile,
    pub(super) next_chunk_seq: u32,
    pub(super) generation_bytes: u64,
    pub(super) generation_frames: u32,
    pub(super) ready: bool,
}

/// Reject a chunk whose identity drifted from the in-flight generation or
/// that arrives after READY.
pub(super) fn ensure_chunk_identity(
    host: &SatelliteHost,
    terminal: u32,
    flow: &RelayBootstrapFlow,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
) -> Result<(), String> {
    if flow.ready || flow.stream_id != stream_id || flow.bootstrap_id != bootstrap_id {
        return Err(format!(
            "satellite {host} changed or reused bootstrap identity before READY for terminal {terminal}"
        ));
    }
    Ok(())
}

/// Reject a chunk that skipped or repeated the generation's chunk sequence.
pub(super) fn ensure_chunk_sequence(
    host: &SatelliteHost,
    flow: &RelayBootstrapFlow,
    chunk_seq: u32,
) -> Result<(), String> {
    if chunk_seq != flow.next_chunk_seq {
        return Err(format!(
            "satellite {host} sent BOOTSTRAP_CHUNK sequence {chunk_seq}, expected {}",
            flow.next_chunk_seq
        ));
    }
    Ok(())
}

/// Charge one frame against `limit`, or reject a satellite over `budget`.
pub(super) fn charge_frame(
    host: &SatelliteHost,
    budget: &str,
    current: u32,
    limit: u32,
) -> Result<u32, String> {
    current
        .checked_add(1)
        .filter(|count| *count <= limit)
        .ok_or_else(|| format!("satellite {host} exceeded the {budget}"))
}

/// Charge `payload_len` bytes against `limit`, or reject.
pub(super) fn charge_bytes(
    host: &SatelliteHost,
    budget: &str,
    current: u64,
    payload_len: u64,
    limit: u64,
) -> Result<u64, String> {
    current
        .checked_add(payload_len)
        .filter(|bytes| *bytes <= limit)
        .ok_or_else(|| format!("satellite {host} exceeded the {budget}"))
}

/// The one list of return-leg stream frames and where each keeps its terminal
/// scope; `$as` is `as_ref` or `as_mut` to match the borrow of `$frame`.
macro_rules! stream_frame_scope_of {
    ($frame:expr, $as:ident) => {
        match $frame {
            FrameKind::Event { terminal, .. } => terminal.$as(),
            FrameKind::ResourceOutput { terminal_id, .. }
            | FrameKind::BootstrapBegin { terminal_id, .. }
            | FrameKind::BootstrapChunk { terminal_id, .. }
            | FrameKind::BootstrapReady { terminal_id, .. }
            | FrameKind::HistoryPage { terminal_id, .. }
            | FrameKind::BootstrapTombstone { terminal_id, .. }
            | FrameKind::HistoryTombstone { terminal_id, .. }
            | FrameKind::HistoryRejected { terminal_id, .. } => Some(terminal_id),
            _ => None,
        }
    };
}

/// The terminal scope a return-leg stream frame carries, if it has one.
pub(super) const fn stream_frame_scope(frame: &FrameKind) -> Option<&ResourceId> {
    stream_frame_scope_of!(frame, as_ref)
}

/// [`stream_frame_scope`], borrowed for rewriting.
pub(super) const fn stream_frame_scope_mut(frame: &mut FrameKind) -> Option<&mut ResourceId> {
    stream_frame_scope_of!(frame, as_mut)
}

/// The generation a return-leg stream frame names; by reference
/// or for rewriting, as `$frame` is borrowed.
macro_rules! stream_frame_generation_of {
    ($frame:expr) => {
        match $frame {
            FrameKind::ResourceOutput {
                stream_id,
                bootstrap_id,
                ..
            }
            | FrameKind::BootstrapBegin {
                stream_id,
                bootstrap_id,
                ..
            }
            | FrameKind::BootstrapChunk {
                stream_id,
                bootstrap_id,
                ..
            }
            | FrameKind::BootstrapReady {
                stream_id,
                bootstrap_id,
                ..
            }
            | FrameKind::HistoryPage {
                stream_id,
                bootstrap_id,
                ..
            }
            | FrameKind::BootstrapTombstone {
                stream_id,
                bootstrap_id,
                ..
            }
            | FrameKind::HistoryTombstone {
                stream_id,
                bootstrap_id,
                ..
            }
            | FrameKind::HistoryRejected {
                stream_id,
                bootstrap_id,
                ..
            } => Some((stream_id, bootstrap_id)),
            _ => None,
        }
    };
}

/// The generation a return-leg stream frame names, if it names one.
pub(super) const fn stream_frame_generation(frame: &FrameKind) -> Option<(StreamId, BootstrapId)> {
    match stream_frame_generation_of!(frame) {
        Some((stream_id, bootstrap_id)) => Some((*stream_id, *bootstrap_id)),
        None => None,
    }
}

/// The bootstrap id a return-leg stream frame names, borrowed for rewriting.
pub(super) const fn stream_frame_bootstrap_mut(frame: &mut FrameKind) -> Option<&mut BootstrapId> {
    match stream_frame_generation_of!(frame) {
        Some((_, bootstrap_id)) => Some(bootstrap_id),
        None => None,
    }
}

/// A relayed `LIST_DIRECTORY` awaiting its `DIRECTORY_LISTING`.
#[derive(Debug)]
pub(super) struct PendingListing {
    /// The requested path, for a refusal that has to name it.
    pub(super) path: String,
    pub(super) reply: oneshot::Sender<DirectoryListingResult>,
}

#[derive(Debug)]
pub(super) struct PendingPathQuery {
    pub(super) root: String,
    pub(super) reply: oneshot::Sender<PathQueryResult>,
}

#[derive(Debug)]
pub(super) struct PendingDetach {
    pub(super) terminal: u32,
    pub(super) deadline: tokio::time::Instant,
}

/// Per-connection relay state driven by [`crate::hub::link::run_link`].
///
/// Owns link-side request ids, pending replies, and the proxy registry.
/// Session-scoped: [`Self::teardown`] fails pending work so a reconnect
/// starts clean.
#[derive(Debug)]
pub(crate) struct RelaySession {
    pub(super) host: SatelliteHost,
    pub(crate) epoch: u64,
    pub(super) bootstrap_limits: BootstrapLimits,
    pub(super) bootstrap_profile: BootstrapProfile,
    pub(super) next_request_id: u32,
    pub(super) pending: HashMap<u32, PendingCommand>,
    pub(super) enforce_bootstrap_flow: bool,
    /// Relayed spawns awaiting `RESOURCE_SPAWNED`, in the shared id space.
    pub(super) pending_spawns: HashMap<
        u32,
        (
            oneshot::Sender<SpawnResult>,
            phux_protocol::ids::ResourceKind,
        ),
    >,
    /// Relayed listings awaiting `DIRECTORY_LISTING`, in the same id space.
    pub(super) pending_listings: HashMap<u32, PendingListing>,
    pub(super) pending_paths: HashMap<u32, PendingPathQuery>,
    /// The satellite's `HELLO_OK` features; listings need `LIST_DIRECTORY`.
    pub(super) satellite_features: ServerFeatureSet,
    pub(super) satellite_features_ext: ServerFeatureExtSet,
    pub(super) satellite_features_ext2: phux_protocol::caps::ServerFeatureExt2Set,
    /// Upstream detach barriers: old frames may arrive until the reply.
    pub(super) pending_detaches: HashMap<u32, PendingDetach>,
    /// Terminals whose metadata allowlist is already mirrored (ADR-0136).
    pub(super) mirrored: HashSet<u32>,
    /// Link-side `GET_METADATA` ids for that allowlist: terminal and key.
    pub(super) pending_mirror_gets: HashMap<u32, (u32, String)>,
    pub(super) subscribers: HashMap<u32, Vec<ProxySubscriber>>,
    /// Terminals with an explicit content attach since the last upstream
    /// detach; a refused attach cannot resurrect the retired SPAWN
    /// generation.
    pub(super) explicit_content: HashSet<u32>,
    /// Legacy `SUBSCRIBE_EVENTS` terminals, re-subscribed after internal
    /// detaches (which remove them upstream).
    pub(super) legacy_events: HashSet<u32>,
    pub(super) bootstrap_flows: HashMap<u32, RelayBootstrapFlow>,
    /// Next bootstrap id consumers see. The link's `stream_id` is the same for
    /// every consumer and outlives their connections, and a satellite that
    /// restarts (or predates id blocks) repeats its ids; a consumer draws no
    /// generation it retired, so the hub names each one itself. Shared with
    /// the link's other sessions.
    pub(super) generations: Arc<AtomicU64>,
    pub(super) retained_bytes: usize,
    pub(super) retained_frames: usize,
    /// Terminals whose last subscriber was removed by delivery failure,
    /// drained by the link into upstream detaches.
    pub(super) orphaned_by_delivery: HashSet<u32>,
    pub(super) inflight_generation_bytes: u64,
    pub(super) inflight_generation_frames: u32,
    pub(super) encode_buf: BytesMut,
    /// Hub state for re-stamping relayed events; `None` keeps the satellite's.
    pub(super) journal: Option<crate::state::SharedState>,
    /// The satellite's `HELLO_OK.server_id` on this connection.
    pub(super) incarnation: crate::hub::operation_fence::Incarnation,
    /// Incarnation fence shared with the link's other sessions.
    pub(super) operations: crate::hub::operation_fence::OperationFence,
    pub(super) resource_kinds: Option<ResourceKindCatalog>,
}
