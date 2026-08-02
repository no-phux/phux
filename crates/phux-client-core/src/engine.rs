//! Frontend-neutral terminal-engine boundary used by the session kernel.

use crate::history::{DocumentAnchorId, HistoryPageId};
use phux_protocol::BootstrapStreamProfile;

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
pub mod ghostty;

/// The canonical live PTY geometry selected by the server.
///
/// Frontends may project historical content at another width, but must never
/// resize a live replica independently of this geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CanonicalGeometry {
    /// Live grid width in terminal cells.
    pub cols: u16,
    /// Live grid height in terminal cells.
    pub rows: u16,
}

impl CanonicalGeometry {
    /// Construct a non-empty canonical geometry.
    #[must_use]
    pub const fn new(cols: u16, rows: u16) -> Option<Self> {
        if cols == 0 || rows == 0 {
            None
        } else {
            Some(Self { cols, rows })
        }
    }
}

/// Progress reported by an engine while consuming a bootstrap transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootstrapProgress {
    /// The engine has not reached its internal READY boundary.
    Pending,
    /// The engine reached its internal READY boundary.
    Ready,
    /// The engine finished the bootstrap transcript and is READY.
    Finished,
}

impl BootstrapProgress {
    pub(crate) const fn is_ready(self) -> bool {
        matches!(self, Self::Ready | Self::Finished)
    }
}

/// Outcome of one authenticated post-READY history unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryApplyOutcome {
    /// Decoder continuation state after consuming the unit.
    pub progress: BootstrapProgress,
    /// Whether the engine retained the imported page under its local limits.
    pub retained: bool,
}

/// A terminal-engine request to write bytes back to its PTY.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineSend {
    /// A protocol reply generated synchronously by terminal parsing.
    PtyWrite(Vec<u8>),
}

/// Engine-reported render damage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineDamage {
    /// The complete canonical grid changed.
    Full,
    /// An inclusive range of canonical grid rows changed.
    Rows {
        /// First damaged row.
        first: u16,
        /// Last damaged row.
        last: u16,
    },
}

/// Engine status that a frontend may present without interpreting terminal data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineStatus {
    /// The terminal emitted a bell.
    Bell,
    /// The engine observed a terminal title change.
    Title(String),
}

/// Cooperative engine work that a host may schedule outside the kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineJob {
    /// Wake the engine again on the owning frontend thread.
    Wakeup,
}

/// A synchronous engine effect captured during an adapter call.
///
/// Adapters append effects here rather than invoking callbacks. The kernel
/// drains the queue only after the adapter call returns, preventing re-entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineEffect {
    /// Send a typed engine response.
    Send(EngineSend),
    /// Report render damage.
    Damage(EngineDamage),
    /// Report frontend-neutral status.
    Status(EngineStatus),
    /// Request cooperative host work.
    Job(EngineJob),
}

/// Reusable queue supplied to every engine adapter operation.
#[derive(Debug, Default)]
pub struct EngineEffectBuffer {
    effects: Vec<EngineEffect>,
}

impl EngineEffectBuffer {
    /// Construct an empty queue with no allocation.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            effects: Vec::new(),
        }
    }

    /// Append one captured engine effect.
    pub fn push(&mut self, effect: EngineEffect) {
        self.effects.push(effect);
    }

    pub(crate) fn clear(&mut self) {
        self.effects.clear();
    }

    pub(crate) fn drain(&mut self) -> std::vec::Drain<'_, EngineEffect> {
        self.effects.drain(..)
    }

    #[cfg(all(test, feature = "native-engine"))]
    pub(crate) fn as_slice(&self) -> &[EngineEffect] {
        &self.effects
    }
}

/// Synchronous terminal-engine operations required by [`SessionKernel`].
///
/// Bootstrap and live payloads are borrowed directly from decoded protocol
/// frames. Implementations own their replica allocation and must append all
/// synchronous side effects to the supplied reusable buffer. Bootstrap
/// `Send` and `Damage` effects are suppressed because replay is not live PTY
/// input and publication emits one full damage; bootstrap `Status` and `Job`
/// effects are generation-bound and buffered until atomic publication. They
/// are discarded if staging is retired. All successful live effects are
/// forwarded with the published replica key.
///
/// Except where an operation documents a stronger guarantee, an error may
/// have partially mutated the replica. The kernel retires that exact
/// generation immediately and never calls the adapter with the same object
/// again; adapters need not provide transactional rollback.
///
/// [`SessionKernel`]: crate::session::SessionKernel
pub trait EngineAdapter {
    /// One engine-owned live terminal replica.
    type Replica;
    /// Adapter failure surfaced by the kernel.
    type Error: std::error::Error + 'static;

    /// Allocate a staging replica for the explicit negotiated stream profile.
    fn start_replica(
        &mut self,
        profile: BootstrapStreamProfile,
        geometry: CanonicalGeometry,
    ) -> Result<Self::Replica, Self::Error>;

    /// Apply one borrowed, contiguous bootstrap fragment.
    fn apply_bootstrap_chunk(
        &mut self,
        replica: &mut Self::Replica,
        payload: &[u8],
        effects: &mut EngineEffectBuffer,
    ) -> Result<BootstrapProgress, Self::Error>;

    /// Apply the client-local decoded scrollback memory budget.
    fn configure_history_budget(
        &mut self,
        _replica: &mut Self::Replica,
        _max_bytes: usize,
        _max_rows: usize,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Release all engine-owned tracked document state.
    fn clear_document_state(&mut self, _replica: &mut Self::Replica) {}

    /// Resolve an engine-owned history anchor to its current distance from tail.
    ///
    /// `None` means the anchor was pruned or otherwise became invalid.
    fn history_anchor_tail_distance(
        &self,
        _replica: &Self::Replica,
        _anchor: DocumentAnchorId,
    ) -> Result<Option<u64>, Self::Error> {
        Ok(None)
    }

    /// Finish the bootstrap transcript at the protocol READY boundary.
    fn finish_bootstrap(
        &mut self,
        replica: &mut Self::Replica,
        effects: &mut EngineEffectBuffer,
    ) -> Result<BootstrapProgress, Self::Error>;

    /// Apply one borrowed opaque history page after the replica is published.
    ///
    /// History delivery is generation-bound but independent of live output
    /// sequencing. On error, the replica MUST remain renderable at its last
    /// successfully imported state: the kernel freezes and retains it while
    /// requesting a replacement. Native adapters retain their post-READY
    /// decoder across successful calls; compatibility adapters reject history.
    fn apply_history_page(
        &mut self,
        replica: &mut Self::Replica,
        payload: &[u8],
        effects: &mut EngineEffectBuffer,
    ) -> Result<HistoryApplyOutcome, Self::Error>;

    /// Apply one borrowed, exactly sequenced live payload.
    fn apply_output(
        &mut self,
        replica: &mut Self::Replica,
        payload: &[u8],
        effects: &mut EngineEffectBuffer,
    ) -> Result<(), Self::Error>;
}

/// Coordinate space for engine-owned document anchors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentSpace {
    /// Scrollback history before the active area.
    History,
    /// Current visible viewport.
    Viewport,
    /// Active PTY area.
    Active,
}

/// A point interpreted only by the selected terminal engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocumentPoint {
    /// Coordinate space.
    pub space: DocumentSpace,
    /// Cell column.
    pub x: u16,
    /// Physical row in the selected space.
    pub y: u32,
}

/// Adapter-produced semantic row for frontend projection.
///
/// This is derived from imported engine state, never from opaque history bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineProjectionRow {
    /// Engine-formatted Unicode text.
    pub text: String,
    /// Whether the following physical row continues this logical line.
    pub soft_wrapped: bool,
    /// Cached opaque page backing this row when known.
    pub page: Option<HistoryPageId>,
}

/// Cooperative client-local history projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineHistoryProjection {
    /// Frontend-local width used for wrapping.
    pub width: u16,
    /// Oldest-to-newest semantic rows returned within the caller's bound.
    pub rows: Vec<EngineProjectionRow>,
    /// Whether an older uncached/unmaterialized row remains.
    pub has_older: bool,
}

/// Engine-owned selection endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineDocumentSelection {
    /// Inclusive start anchor.
    pub start: DocumentAnchorId,
    /// Inclusive end anchor.
    pub end: DocumentAnchorId,
    /// Rectangular rather than linear selection.
    pub rectangle: bool,
}

/// One engine search result represented by stable tracked anchors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineSearchMatch {
    /// Inclusive first match scalar.
    pub start: DocumentAnchorId,
    /// Inclusive last match scalar.
    pub end: DocumentAnchorId,
}

/// Where an engine projection begins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineProjectionOrigin {
    /// Project the newest loaded tail.
    Tail,
    /// Project forward from a resolved tracked anchor.
    Anchor(DocumentAnchorId),
}
/// Optional semantic document operations for adapters with an engine-owned grid.
pub trait EngineDocumentAdapter: EngineAdapter {
    /// Cooperatively project imported history without resizing canonical PTY state.
    fn project_history(
        &mut self,
        replica: &mut Self::Replica,
        width: u16,
        origin: EngineProjectionOrigin,
        max_rows: usize,
    ) -> Result<EngineHistoryProjection, Self::Error>;

    /// Create one tracked anchor owned by the replica.
    fn track_document_anchor(
        &mut self,
        replica: &mut Self::Replica,
        point: DocumentPoint,
    ) -> Result<DocumentAnchorId, Self::Error>;

    /// Release one tracked anchor.
    fn release_document_anchor(&mut self, replica: &mut Self::Replica, anchor: DocumentAnchorId);

    /// Resolve an anchor immediately before projection.
    fn document_anchor_point(
        &self,
        replica: &Self::Replica,
        anchor: DocumentAnchorId,
        space: DocumentSpace,
    ) -> Result<Option<DocumentPoint>, Self::Error>;

    /// Search only engine state already imported into this replica.
    fn search_loaded(
        &mut self,
        replica: &mut Self::Replica,
        needle: &str,
        max_matches: usize,
    ) -> Result<Vec<EngineSearchMatch>, Self::Error>;

    /// Format a selection with engine wrap and grapheme semantics.
    fn format_selection(
        &self,
        replica: &Self::Replica,
        selection: EngineDocumentSelection,
    ) -> Result<Option<String>, Self::Error>;
}
