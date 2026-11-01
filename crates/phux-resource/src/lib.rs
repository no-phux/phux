//! The kind-neutral resource substrate.
//!
//! Every resource has the same backing-agnostic surface: an ordered output
//! stream with a checked `u64` sequence, a semantic event sink
//! ([`event_sink`], ADR-0123), and a lifecycle.
//! [`ResourceCore`] owns it engine-side; [`ResourceHandle`] is the
//! `Send + Clone` channel set the host holds.
//!
//! Per-kind channels are the facet: an opaque [`ResourceFacet`] the host
//! stores without knowing its type, reached only through
//! [`ResourceHandle::facet`], the single source of [`WrongResourceKind`].
//! An app owns its facet type, so this crate names no kind's internals: what
//! the host asks of every kind is [`ResourceOps`], and a surface several
//! kinds could share is a [companion](ResourceHandle::companion).
//! Engines follow ADR-0014: `!Send` state stays in one `spawn_local` task.

#![allow(
    clippy::redundant_pub_crate,
    reason = "items shared inside the crate; pub would widen the app's API"
)]

use std::sync::atomic::{AtomicUsize, Ordering};

use bytes::Bytes;
use phux_protocol::ClientId;
use phux_protocol::wire::frame::{ControlAction, FrameKind};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

pub mod cwd_query;
pub mod event_sink;
pub mod handoff;
pub mod handoff_env;
pub mod mailbox;
pub mod proc_query;

pub use phux_core::ids::ResourceId;
pub use phux_core::resource::ResourceKind;

/// The server's wall clock in Unix milliseconds, `0` if it reads before the
/// epoch.
#[must_use]
pub fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

/// Debounce for the post-resize resync: a window drag fires many resizes,
/// and one snapshot at the settled size replaces per-step snapshots that
/// would land on mismatched mirrors.
pub const RESIZE_RESYNC_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(50);

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

// ---- output stream ----------------------------------------------------------

/// Continuity reason carried with an engine-generated full resync.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResyncReason {
    /// Authoritative geometry changed.
    Resize,
    /// A bounded output subscriber observed a sequence gap.
    OutboundGap,
    /// The backing exited; this is the final state, queued before
    /// `RESOURCE_CLOSED`.
    Exit,
}

/// One output pump on a resource: its owning client and wire stream, the same
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

/// Which pumps a [`ResourceOutput::Resync`] is for. A reflow owes everyone; a
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
/// Each attach pump maps `Live` to `RESOURCE_OUTPUT`, `Resync` to the kind's
/// replacement snapshot (the client mirror adopts the carried dims before
/// repainting, which raw output could never do), and `Control` to an ordered
/// control frame for the matching owner.
#[derive(Clone, Debug)]
pub enum ResourceOutput {
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
    /// A full replacement of the stream's state and the dims the client
    /// mirror must adopt.
    Resync {
        /// Width in cells the client mirror resizes to.
        cols: u16,
        /// Height in cells the client mirror resizes to.
        rows: u16,
        /// Why the prior generation can no longer continue.
        reason: ResyncReason,
        /// Which pumps must replace their generation; the rest ignore it.
        audience: ResyncAudience,
        /// Resource-global raw sequence included by the replacement cut.
        base_seq: u64,
        /// The replacement bytes, including any reset preamble.
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

// ---- input lease ------------------------------------------------------------

/// A change to a resource's input lease (ADR-0033). The lease lives with the
/// host; the engine broadcasts it because it owns the lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LeaseChange {
    /// The client now holding the lease, or `None` if released to `Open`.
    pub input_holder: Option<ClientId>,
    /// `Acquired` / `Seized` / `Released` / `Expired`.
    pub action: ControlAction,
    /// The acting client; `None` for a server-side release (TTL or
    /// revocation).
    pub actor: Option<ClientId>,
}

/// A mailbox send the host may await after releasing its state lock.
pub type Delivery = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;

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

/// The kind-specific channel set of one resource. The app that serves
/// [`Self::KIND`] owns the type; the host stores it opaquely.
pub trait ResourceFacet: ResourceOps + std::any::Any {
    /// The kind whose channels this facet carries.
    const KIND: ResourceKind;
}

/// What the host asks of any resource without knowing its kind. Every kind
/// answers each one: there is no default to inherit by omission.
pub trait ResourceOps: Send + Sync + std::fmt::Debug {
    /// Ask the engine to take `size` in cells, returning whether it accepted.
    fn resize(&self, size: (u16, u16), cell_px: Option<(u16, u16)>) -> bool;

    /// Where this resource answers a graceful-upgrade handoff, if it has a
    /// process to hand off.
    fn upgrade_sender(&self) -> Option<mpsc::Sender<handoff::UpgradeHandleRequest>>;

    /// Tell the engine its input lease changed, waiting for mailbox room.
    fn lease_changed(&self, change: LeaseChange) -> Delivery;

    /// [`Self::lease_changed`] for a caller that may not wait; a full
    /// mailbox drops the notice.
    fn try_lease_changed(&self, change: LeaseChange);

    /// The backing exited and the resource is retained (ADR-0124): report
    /// it, stop any background work, refuse input.
    fn retire(&self) -> Delivery;
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
    pub output: broadcast::Sender<ResourceOutput>,
    /// The kind-specific channels. Reach them through [`Self::facet`].
    facet: std::sync::Arc<dyn std::any::Any + Send + Sync>,
    /// The same facet, as the operations every kind answers.
    ops: std::sync::Arc<dyn ResourceOps>,
    /// Surfaces beyond the facet, newest first (see [`Self::companion`]).
    companions: std::sync::Arc<[Companion]>,
}

/// One value in [`ResourceHandle`]'s companion list.
type Companion = std::sync::Arc<dyn std::any::Any + Send + Sync>;

impl ResourceHandle {
    /// A handle for a resource of `F::KIND`, carrying `facet`.
    #[must_use]
    pub fn new<F: ResourceFacet>(
        parent: Option<ResourceId>,
        output: broadcast::Sender<ResourceOutput>,
        facet: F,
    ) -> Self {
        let facet = std::sync::Arc::new(facet);
        Self {
            kind: F::KIND,
            parent,
            output,
            ops: facet.clone(),
            facet,
            companions: std::sync::Arc::from([]),
        }
    }

    /// This handle with `companion` added; a later one of the same type
    /// shadows an earlier one.
    #[must_use]
    pub fn with_companion<C: std::any::Any + Send + Sync>(mut self, companion: C) -> Self {
        let added: Companion = std::sync::Arc::new(companion);
        self.companions = std::iter::once(added)
            .chain(self.companions.iter().cloned())
            .collect();
        self
    }

    /// A surface of type `C` this resource offers beyond its facet, for
    /// callers that need it without knowing the resource's kind. The crate
    /// that owns `C` defines what it means.
    #[must_use]
    pub fn companion<C: std::any::Any>(&self) -> Option<&C> {
        self.companions
            .iter()
            .find_map(|companion| companion.downcast_ref::<C>())
    }

    /// The operations every kind answers.
    #[must_use]
    pub fn ops(&self) -> &dyn ResourceOps {
        self.ops.as_ref()
    }

    /// This resource's facet of type `F`.
    ///
    /// # Errors
    ///
    /// [`WrongResourceKind`] when this resource is not of `F::KIND`.
    pub fn facet<F: ResourceFacet>(&self) -> Result<&F, WrongResourceKind> {
        self.facet.downcast_ref::<F>().ok_or(WrongResourceKind {
            required: F::KIND,
            actual: self.kind,
        })
    }
}

// ---- engine-side core -------------------------------------------------------

/// The engine-side half of a resource: output sequence and broadcast and
/// lifecycle, owned by one engine task.
pub struct ResourceCore {
    /// Which engine owns this core.
    pub kind: ResourceKind,
    /// The resource this one is bound to, if any.
    pub parent: Option<ResourceId>,
    /// Resource-global raw output sequence; advanced only by
    /// [`Self::next_seq`].
    pub seq: u64,
    /// Output sender; the seed receiver was dropped, so `receiver_count()`
    /// counts live subscribers.
    pub output_tx: broadcast::Sender<ResourceOutput>,
    /// Fired once when the backing exits.
    pub exit_notify: Option<oneshot::Sender<phux_core::process::ExitOutcome>>,
    /// Cancellation token the engine watches (dropping does not cancel).
    pub token: CancellationToken,
}

/// Sender halves paired with a fresh [`ResourceCore`].
#[derive(Debug)]
pub struct ResourceCoreChannels {
    /// Output broadcast sender, cloned into the handle.
    pub output: broadcast::Sender<ResourceOutput>,
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
        let (exit_tx, exit_rx) = oneshot::channel();
        let core = Self {
            kind,
            parent,
            seq: 0,
            output_tx: output_tx.clone(),
            exit_notify: Some(exit_tx),
            token,
        };
        let channels = ResourceCoreChannels {
            output: output_tx,
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
mod tests;
