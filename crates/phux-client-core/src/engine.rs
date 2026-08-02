//! Frontend-neutral terminal-engine boundary used by the session kernel.

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
    ) -> Result<BootstrapProgress, Self::Error>;

    /// Apply one borrowed, exactly sequenced live payload.
    fn apply_output(
        &mut self,
        replica: &mut Self::Replica,
        payload: &[u8],
        effects: &mut EngineEffectBuffer,
    ) -> Result<(), Self::Error>;
}
