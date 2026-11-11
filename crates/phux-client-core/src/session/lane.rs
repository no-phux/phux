//! The per-kind lane registry: every streamed resource lives in one lane.
//!
//! A resource's bootstrap profile picks its lane: a Terminal replica, an
//! `AgentSession` record stream, or a document cut. Each lane keeps its own
//! generation fence and staging through the shared rails in
//! [`staging_rules`](super::staging_rules). The kernel keeps what is
//! connection-wide (the closed set, the ATTACH barrier, the staging
//! ceilings, the clock) and hands a lane one [`LaneCx`] per frame.

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use phux_protocol::{BootstrapProfile, BootstrapStreamProfile, ResourceId, ResourceKind};

use super::{
    AgentStream, CanonicalGeometry, DocumentStream, EffectBuffer, EngineAdapter,
    EngineEffectBuffer, GenerationId, HistoryCacheConfig, KernelError, TerminalState,
    TombstoneReason, TombstoneRecord, document_kind, is_agent_profile,
};
use crate::perf::EchoProbe;

/// The lane a resource's stream belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LaneKind {
    Terminal,
    Agent,
    Document(ResourceKind),
}

impl LaneKind {
    /// The lane a known stream profile selects.
    pub(super) const fn of_profile(profile: BootstrapStreamProfile) -> Self {
        if let Some(kind) = document_kind(profile) {
            Self::Document(kind)
        } else if is_agent_profile(profile) {
            Self::Agent
        } else {
            Self::Terminal
        }
    }

    /// The resource kind this lane implies.
    pub(super) const fn resource_kind(self) -> ResourceKind {
        match self {
            Self::Terminal => ResourceKind::Terminal,
            Self::Agent => ResourceKind::AgentSession,
            Self::Document(kind) => kind,
        }
    }

    /// Checks a begin must pass before any lane state exists for it: only a
    /// Terminal replica is bound to the connection profile and a geometry.
    pub(super) const fn admit_begin<X>(
        self,
        selected: BootstrapProfile,
        frame: &BeginFrame,
    ) -> Result<(), KernelError<X>> {
        match self {
            Self::Terminal => super::terminal_lane::admit_begin(selected, frame),
            Self::Agent | Self::Document(_) => Ok(()),
        }
    }
}

/// What the ATTACH barrier learns from one lane transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum AttachSignal {
    /// Readiness did not change.
    #[default]
    Unchanged,
    /// A generation was published: the participant is ready.
    Resolved,
    /// The published generation was frozen or dropped.
    Unresolved {
        /// A removal must be damaged once the barrier releases.
        pending_removal: bool,
    },
}

/// Connection-wide context one lane transition runs in.
pub(super) struct LaneCx<'a, E: EngineAdapter> {
    pub(super) resource_id: &'a ResourceId,
    pub(super) adapter: &'a mut E,
    pub(super) engine_effects: &'a mut EngineEffectBuffer,
    pub(super) history_config: HistoryCacheConfig,
    pub(super) perf_echo: &'a EchoProbe,
    /// The kernel's monotonic clock, read for staging frames; 0 for live
    /// output, which no lane times.
    pub(super) now_ms: u64,
    /// The resource is held behind an unreleased ATTACH barrier, so its
    /// damage waits for `ATTACH_READY`. Only READY and live output, the
    /// frames that damage, read it; it is `false` for the others.
    pub(super) attach_blocks: bool,
    /// Set by the lane; applied by the kernel even when the frame fails.
    pub(super) attach: AttachSignal,
}

/// `BOOTSTRAP_BEGIN`.
pub(super) struct BeginFrame {
    pub(super) generation: GenerationId,
    pub(super) profile: BootstrapStreamProfile,
    pub(super) geometry: CanonicalGeometry,
    pub(super) base_seq: u64,
}

/// `BOOTSTRAP_CHUNK`, with the connection-wide budget already judged.
pub(super) struct ChunkFrame<'a> {
    pub(super) generation: GenerationId,
    pub(super) chunk_seq: u32,
    pub(super) payload: &'a [u8],
    /// Whether every lane's accepted staging plus this chunk fits the ceilings.
    pub(super) usage_allowed: bool,
}

/// `BOOTSTRAP_READY`.
pub(super) struct ReadyFrame<'a> {
    pub(super) generation: GenerationId,
    pub(super) history_cursor: Option<&'a [u8]>,
}

/// `RESOURCE_OUTPUT`.
pub(super) struct OutputFrame<'a> {
    pub(super) generation: GenerationId,
    pub(super) seq: u64,
    pub(super) payload: &'a [u8],
}

/// One stream frame routed to a lane.
pub(super) enum StreamFrame<'a> {
    Begin(BeginFrame),
    Chunk(ChunkFrame<'a>),
    Ready(ReadyFrame<'a>),
    Output(OutputFrame<'a>),
}

/// The rails every lane keeps alike: the generation fence and staging.
pub(super) trait LaneRails {
    /// Record an external tombstone at `now_ms`; staging for that
    /// generation is dropped.
    fn on_tombstone(
        &mut self,
        generation: GenerationId,
        record: TombstoneRecord,
        now_ms: u64,
    ) -> AttachSignal;
    /// Bytes and chunks accepted by the open staging generation.
    fn staged_accounting(&self) -> Option<(usize, usize)>;
    /// The staging generation its clock has expired, if any.
    fn expired_staging(&self, now_ms: u64) -> Option<GenerationId>;
    /// A fenced generation whose expected recut never began, reported once.
    fn take_overdue_recut(&mut self, _now_ms: u64) -> Option<(GenerationId, TombstoneReason)> {
        None
    }
    /// The first millisecond a staging cut or an awaited recut is overdue.
    fn deadline_ms(&self) -> Option<u64>;
    /// Fence `generation` for exceeding a staging limit and ask for a recut.
    fn retire_for_limit(
        &mut self,
        resource_id: &ResourceId,
        generation: GenerationId,
        effects: &mut EffectBuffer,
    );
}

/// One kind's codec: how its lane stages, publishes and applies live frames.
pub(super) trait KindLane<E: EngineAdapter>: LaneRails {
    fn on_begin(
        &mut self,
        cx: &mut LaneCx<'_, E>,
        frame: &BeginFrame,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>>;
    fn on_chunk(
        &mut self,
        cx: &mut LaneCx<'_, E>,
        frame: &ChunkFrame<'_>,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>>;
    fn on_ready(
        &mut self,
        cx: &mut LaneCx<'_, E>,
        frame: &ReadyFrame<'_>,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>>;
    fn on_output(
        &mut self,
        cx: &mut LaneCx<'_, E>,
        frame: &OutputFrame<'_>,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>>;
    /// Staged bytes held beyond the accepted payload (engine state, buffered
    /// effects), counted after a chunk against the byte ceiling.
    fn retained_staging_bytes(&self, _adapter: &E) -> usize {
        0
    }
}

/// One resource's lane.
pub(super) enum ResourceLane<R> {
    Terminal(TerminalState<R>),
    Agent(AgentStream),
    Document(DocumentStream),
}

macro_rules! each_lane {
    ($lane:expr, $inner:ident => $body:expr) => {
        match $lane {
            ResourceLane::Terminal($inner) => $body,
            ResourceLane::Agent($inner) => $body,
            ResourceLane::Document($inner) => $body,
        }
    };
}

impl<R> ResourceLane<R> {
    fn new(kind: LaneKind) -> Self {
        match kind {
            LaneKind::Terminal => Self::Terminal(TerminalState::default()),
            LaneKind::Agent => Self::Agent(AgentStream::new()),
            LaneKind::Document(kind) => Self::Document(DocumentStream::new(kind)),
        }
    }

    const fn is(&self, kind: LaneKind) -> bool {
        matches!(
            (self, kind),
            (Self::Terminal(_), LaneKind::Terminal)
                | (Self::Agent(_), LaneKind::Agent)
                | (Self::Document(_), LaneKind::Document(_))
        )
    }
}

impl<R> LaneRails for ResourceLane<R> {
    fn on_tombstone(
        &mut self,
        generation: GenerationId,
        record: TombstoneRecord,
        now_ms: u64,
    ) -> AttachSignal {
        each_lane!(self, lane => lane.on_tombstone(generation, record, now_ms))
    }

    fn staged_accounting(&self) -> Option<(usize, usize)> {
        each_lane!(self, lane => lane.staged_accounting())
    }

    fn expired_staging(&self, now_ms: u64) -> Option<GenerationId> {
        each_lane!(self, lane => lane.expired_staging(now_ms))
    }

    fn take_overdue_recut(&mut self, now_ms: u64) -> Option<(GenerationId, TombstoneReason)> {
        each_lane!(self, lane => lane.take_overdue_recut(now_ms))
    }

    fn deadline_ms(&self) -> Option<u64> {
        each_lane!(self, lane => lane.deadline_ms())
    }

    fn retire_for_limit(
        &mut self,
        resource_id: &ResourceId,
        generation: GenerationId,
        effects: &mut EffectBuffer,
    ) {
        each_lane!(self, lane => lane.retire_for_limit(resource_id, generation, effects));
    }
}

impl<E: EngineAdapter> KindLane<E> for ResourceLane<E::Replica> {
    fn on_begin(
        &mut self,
        cx: &mut LaneCx<'_, E>,
        frame: &BeginFrame,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        each_lane!(self, lane => KindLane::<E>::on_begin(lane, cx, frame, effects))
    }

    fn on_chunk(
        &mut self,
        cx: &mut LaneCx<'_, E>,
        frame: &ChunkFrame<'_>,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        each_lane!(self, lane => KindLane::<E>::on_chunk(lane, cx, frame, effects))
    }

    fn on_ready(
        &mut self,
        cx: &mut LaneCx<'_, E>,
        frame: &ReadyFrame<'_>,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        each_lane!(self, lane => KindLane::<E>::on_ready(lane, cx, frame, effects))
    }

    fn on_output(
        &mut self,
        cx: &mut LaneCx<'_, E>,
        frame: &OutputFrame<'_>,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        each_lane!(self, lane => KindLane::<E>::on_output(lane, cx, frame, effects))
    }

    fn retained_staging_bytes(&self, adapter: &E) -> usize {
        each_lane!(self, lane => KindLane::<E>::retained_staging_bytes(lane, adapter))
    }
}

/// Every resource's lane, keyed by resource id.
pub(super) struct LaneRegistry<R> {
    lanes: HashMap<ResourceId, ResourceLane<R>>,
}

impl<R> Default for LaneRegistry<R> {
    fn default() -> Self {
        Self {
            lanes: HashMap::new(),
        }
    }
}

impl<R> LaneRegistry<R> {
    pub(super) fn len(&self) -> usize {
        self.lanes.len()
    }

    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.lanes.is_empty()
    }

    pub(super) fn get(&self, resource_id: &ResourceId) -> Option<&ResourceLane<R>> {
        self.lanes.get(resource_id)
    }

    pub(super) fn get_mut(&mut self, resource_id: &ResourceId) -> Option<&mut ResourceLane<R>> {
        self.lanes.get_mut(resource_id)
    }

    pub(super) fn remove(&mut self, resource_id: &ResourceId) -> Option<ResourceLane<R>> {
        self.lanes.remove(resource_id)
    }

    pub(super) fn iter_mut(&mut self) -> impl Iterator<Item = (&ResourceId, &mut ResourceLane<R>)> {
        self.lanes.iter_mut()
    }

    pub(super) fn values(&self) -> impl Iterator<Item = &ResourceLane<R>> {
        self.lanes.values()
    }

    pub(super) fn reserve(&mut self, additional: usize) {
        self.lanes.reserve(additional);
    }

    /// The lane of `kind` for `resource_id`, opened on first use. A lane of
    /// another kind is a fence-only Terminal placeholder (a tombstone that
    /// arrived before any begin named the kind), so it is replaced: callers
    /// have already checked the declared kind.
    pub(super) fn open(
        &mut self,
        resource_id: &ResourceId,
        kind: LaneKind,
    ) -> &mut ResourceLane<R> {
        match self.lanes.entry(resource_id.clone()) {
            Entry::Occupied(slot) => {
                let lane = slot.into_mut();
                if !lane.is(kind) {
                    *lane = ResourceLane::new(kind);
                }
                lane
            }
            Entry::Vacant(slot) => slot.insert(ResourceLane::new(kind)),
        }
    }

    /// The lane a tombstone lands in: an unknown resource gets a Terminal
    /// lane holding only the fence.
    pub(super) fn fence_slot(&mut self, resource_id: &ResourceId) -> &mut ResourceLane<R> {
        self.lanes
            .entry(resource_id.clone())
            .or_insert_with(|| ResourceLane::Terminal(TerminalState::default()))
    }

    pub(super) fn terminal(&self, resource_id: &ResourceId) -> Option<&TerminalState<R>> {
        match self.lanes.get(resource_id)? {
            ResourceLane::Terminal(state) => Some(state),
            ResourceLane::Agent(_) | ResourceLane::Document(_) => None,
        }
    }

    pub(super) fn terminal_mut(
        &mut self,
        resource_id: &ResourceId,
    ) -> Option<&mut TerminalState<R>> {
        match self.lanes.get_mut(resource_id)? {
            ResourceLane::Terminal(state) => Some(state),
            ResourceLane::Agent(_) | ResourceLane::Document(_) => None,
        }
    }

    pub(super) fn agent(&self, resource_id: &ResourceId) -> Option<&AgentStream> {
        self.agent_entry(resource_id).map(|(_, stream)| stream)
    }

    pub(super) fn agent_entry(
        &self,
        resource_id: &ResourceId,
    ) -> Option<(&ResourceId, &AgentStream)> {
        match self.lanes.get_key_value(resource_id)? {
            (resource_id, ResourceLane::Agent(stream)) => Some((resource_id, stream)),
            (_, ResourceLane::Terminal(_) | ResourceLane::Document(_)) => None,
        }
    }

    pub(super) fn agents(&self) -> impl Iterator<Item = (&ResourceId, &AgentStream)> {
        self.lanes
            .iter()
            .filter_map(|(resource_id, lane)| match lane {
                ResourceLane::Agent(stream) => Some((resource_id, stream)),
                ResourceLane::Terminal(_) | ResourceLane::Document(_) => None,
            })
    }

    pub(super) fn agent_count(&self) -> usize {
        self.agents().count()
    }
}
