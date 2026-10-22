//! Declarative kernel effects: sends, damage, status, jobs, and their buffer.

use super::{
    AgentPublication, BootstrapId, CloseReason, EngineJob, EngineStatus, HistoryStatus,
    HistoryUnavailableReason, InputEvent, ReplicaKey, ResourceId, StreamId, TombstoneReason,
};
#[cfg(doc)]
use super::{KernelInput, SessionKernel, agent_stream};

/// A typed transport send requested by the kernel.
#[derive(Debug, Clone, PartialEq)]
pub enum KernelSend {
    /// Send one structured protocol input atom.
    Input {
        /// Explicit target terminal.
        terminal_id: ResourceId,
        /// Owned atom ready for transport framing outside the kernel.
        event: InputEvent,
    },
    /// Acknowledge one successfully applied `StateSync` live frame.
    FrameAck {
        /// Terminal whose reference advanced.
        terminal_id: ResourceId,
        /// Logical `StateSync` subscription.
        stream_id: StreamId,
        /// Published replica generation.
        bootstrap_id: BootstrapId,
        /// Highest contiguous sequence applied.
        seq: u64,
    },
    /// Request one bounded newest-to-oldest history page.
    HistoryRequest {
        /// Exact published generation.
        key: ReplicaKey,
        /// Opaque engine cursor to consume.
        cursor: Vec<u8>,
        /// Negotiated response byte bound.
        max_bytes: u32,
        /// Negotiated response row bound.
        max_rows: u32,
    },
    /// Subscribe to the `AgentEvent` stream (`docs/protocol.md#commands-events-resources`) so
    /// [`KernelInput::Event`] statuses arrive. Emitted on every attach-barrier
    /// release; re-subscribing is a wire no-op.
    SubscribeEvents {
        /// `None` subscribes to every Terminal local to the connected server
        /// (hub-local on a federation hub; satellites need `Some`).
        terminal: Option<ResourceId>,
        /// Journal resume cursor (ADR-0123); `None` is live-only.
        after_seq: Option<u64>,
    },
}

/// Frontend-neutral render invalidation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelDamage {
    /// Damaged terminal.
    pub terminal_id: ResourceId,
    /// Kind of terminal damage.
    pub kind: KernelDamageKind,
}

/// Kind of frontend-neutral terminal damage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelDamageKind {
    /// Reproject the complete canonical live grid.
    Full,
    /// Reproject an inclusive canonical row range.
    Rows {
        /// First damaged row.
        first: u16,
        /// Last damaged row.
        last: u16,
    },
    /// Remove the terminal's prior projection.
    Removed,
}

/// Frontend-neutral session or engine status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KernelStatus {
    /// Status emitted by a terminal engine.
    Engine {
        /// Exact published generation reporting status.
        key: ReplicaKey,
        /// Engine status payload.
        status: EngineStatus,
    },
    /// The named generation was invalidated and requires a fresh bootstrap.
    ResyncRequired {
        /// Terminal requiring replacement.
        terminal_id: ResourceId,
        /// Invalid logical subscription.
        stream_id: StreamId,
        /// Invalid replica generation.
        bootstrap_id: BootstrapId,
        /// Protocol reason recorded in the local tombstone.
        reason: TombstoneReason,
    },
    /// Progressive history loading and cache status.
    History {
        /// Exact published generation.
        key: ReplicaKey,
        /// Frontend presentation state.
        status: HistoryStatus,
    },
    /// One progressive-history boundary became unavailable; live state remains valid.
    HistoryUnavailable {
        /// Exact published generation.
        key: ReplicaKey,
        /// Engine/server reason for ending this cursor chain.
        reason: HistoryUnavailableReason,
    },
    /// The scoped Terminal's working directory changed (`AgentEvent::CwdChanged`).
    Cwd {
        /// The Terminal whose cwd changed.
        terminal_id: ResourceId,
        /// The new working directory (absolute, lossy UTF-8).
        cwd: String,
    },
    /// A shell command began executing in the scoped Terminal
    /// (`AgentEvent::CommandStarted`).
    CommandStarted {
        /// The Terminal the command started in.
        terminal_id: ResourceId,
    },
    /// A shell command finished in the scoped Terminal
    /// (`AgentEvent::CommandFinished`).
    CommandFinished {
        /// The Terminal the command finished in.
        terminal_id: ResourceId,
        /// Exit code reported by the shell's OSC-133 `D` mark, or `None`
        /// when the shell did not include one.
        exit_code: Option<i32>,
    },
    /// The Terminal's process exited, from `TerminalControl { action: Exited }`
    /// (ADR-0124) or `RESOURCE_CLOSED`; reported at most once per terminal.
    Exited {
        /// The Terminal that exited.
        terminal_id: ResourceId,
        /// Process exit code, or `None` for signals / unknown.
        exit_status: Option<i32>,
        /// Terminating signal number, or `None` when the process exited
        /// without one or the cause is unknown. `TerminalControl` never
        /// carries a signal detail (see `SessionKernel::status_for_agent_event`),
        /// so this is always `None` when sourced from that event; a
        /// `RESOURCE_CLOSED`-sourced exit carries the frame's own `signal`.
        signal: Option<i32>,
        /// Why the resource closed ([`CloseReason::Exited`] for `TerminalControl`).
        reason: CloseReason,
    },
}

/// Cooperative work handed to the host executor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelJob {
    /// Exact published generation requesting work.
    pub key: ReplicaKey,
    /// Engine-owned-thread work request.
    pub job: EngineJob,
}

/// One declarative effect produced by a synchronous kernel update.
#[derive(Debug, Clone, PartialEq)]
pub enum KernelEffect {
    /// A typed send for the transport or PTY executor.
    Send(KernelSend),
    /// Render damage for frontend projection.
    Damage(KernelDamage),
    /// Status for optional frontend presentation.
    Status(KernelStatus),
    /// Cooperative work for a host scheduler.
    Job(KernelJob),
    /// One committed [`AgentPublication`].
    ///
    /// Successful READY always emits one, including an empty resume. Live
    /// output uses [`agent_stream::AgentRecordMode::Append`]. [`AgentPublication::stream`]
    /// is the commit watermark; record payloads are not an identity source.
    /// Invalid staging never emits this effect. Raw stream bytes never reach
    /// a frontend.
    AgentPublication(AgentPublication),
}

/// Reusable high-water-mark effect queue.
///
/// [`SessionKernel::update`] clears the logical contents before each update but
/// retains the vector allocation. Callers must execute or copy effects before
/// the next update.
#[derive(Debug, Default)]
pub struct EffectBuffer {
    effects: Vec<KernelEffect>,
}

impl EffectBuffer {
    /// Construct an empty allocation-free buffer.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            effects: Vec::new(),
        }
    }

    /// Construct an empty buffer with reusable capacity.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            effects: Vec::with_capacity(capacity),
        }
    }

    /// Number of effects produced by the last update.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.effects.len()
    }

    /// Whether the last update produced no effects.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.effects.is_empty()
    }

    /// Current reusable allocation capacity.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.effects.capacity()
    }

    /// Effects produced by the last update.
    #[must_use]
    pub fn as_slice(&self) -> &[KernelEffect] {
        &self.effects
    }

    /// Remove all logical effects while retaining capacity.
    pub fn clear(&mut self) {
        self.effects.clear();
    }

    /// Take every pending effect, leaving this buffer logically empty.
    ///
    /// Frontends that process effects outside [`SessionKernel::update`] must
    /// consume rather than repeatedly borrow the previous update's effects.
    pub fn take(&mut self) -> Vec<KernelEffect> {
        std::mem::take(&mut self.effects)
    }

    pub(super) fn push(&mut self, effect: KernelEffect) {
        self.effects.push(effect);
    }
}

/// Metadata retained for a permanently invalid generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TombstoneRecord {
    /// Why continuity ended.
    pub reason: TombstoneReason,
    /// Highest live sequence known valid.
    pub last_valid_seq: u64,
}
