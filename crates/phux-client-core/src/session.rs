//! Synchronous, transport-neutral session state machine.

use std::collections::{HashMap, HashSet};

use phux_protocol::input::InputEvent;
use phux_protocol::wire::frame::TombstoneReason;
use phux_protocol::{
    BootstrapId, BootstrapProfile, BootstrapStreamProfile, StreamId, TerminalId,
};

use crate::engine::{
    CanonicalGeometry, EngineAdapter, EngineDamage, EngineEffect, EngineEffectBuffer, EngineJob,
    EngineSend, EngineStatus,
};

/// Exact identity of one terminal replica generation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ReplicaKey {
    /// Protocol terminal identifier.
    pub terminal_id: TerminalId,
    /// Connection-scoped logical subscription.
    pub stream_id: StreamId,
    /// Replaceable replica generation.
    pub bootstrap_id: BootstrapId,
    /// Explicit profile repeated by `BOOTSTRAP_BEGIN`.
    pub profile: BootstrapStreamProfile,
}

/// A normalized borrowed input to [`SessionKernel::update`].
#[derive(Debug)]
pub enum KernelInput<'a> {
    /// Start one aggregate attach and its first-damage barrier.
    AttachStarted {
        /// Client-chosen attach correlation identifier.
        attach_id: u32,
        /// Complete ordered terminal inventory for this attach.
        terminals: &'a [TerminalId],
    },
    /// Release the aggregate first-damage barrier.
    AttachReady {
        /// Correlation identifier from `ATTACHED`.
        attach_id: u32,
    },
    /// Begin staging a replacement replica.
    BootstrapBegin {
        /// Target terminal.
        terminal_id: &'a TerminalId,
        /// Logical subscription.
        stream_id: StreamId,
        /// Replacement generation.
        bootstrap_id: BootstrapId,
        /// Explicit stream-local selected profile.
        profile: BootstrapStreamProfile,
        /// Authoritative live geometry.
        geometry: CanonicalGeometry,
        /// Actor cut sequence.
        base_seq: u64,
    },
    /// Apply one borrowed bootstrap fragment.
    BootstrapChunk {
        /// Target terminal.
        terminal_id: &'a TerminalId,
        /// Logical subscription.
        stream_id: StreamId,
        /// Replica generation.
        bootstrap_id: BootstrapId,
        /// Zero-based contiguous chunk sequence.
        chunk_seq: u32,
        /// Borrowed opaque engine or synthesized-VT bytes.
        payload: &'a [u8],
    },
    /// Mark the protocol half of the dual READY fence.
    BootstrapReady {
        /// Target terminal.
        terminal_id: &'a TerminalId,
        /// Logical subscription.
        stream_id: StreamId,
        /// Replica generation.
        bootstrap_id: BootstrapId,
    },
    /// Apply one borrowed live output fragment.
    TerminalOutput {
        /// Target terminal.
        terminal_id: &'a TerminalId,
        /// Logical subscription.
        stream_id: StreamId,
        /// Published replica generation.
        bootstrap_id: BootstrapId,
        /// Exact live sequence.
        seq: u64,
        /// Borrowed raw or profile-selected live bytes.
        payload: &'a [u8],
    },
    /// Permanently retire one replica generation.
    Tombstone {
        /// Target terminal.
        terminal_id: &'a TerminalId,
        /// Logical subscription.
        stream_id: StreamId,
        /// Invalidated generation.
        bootstrap_id: BootstrapId,
        /// Protocol tombstone reason.
        reason: TombstoneReason,
        /// Highest live sequence known valid for the generation.
        last_valid_seq: u64,
    },
    /// Resolve an attach participant by terminal closure.
    TerminalClosed {
        /// Closed terminal.
        terminal_id: &'a TerminalId,
    },
    /// Apply one explicit-terminal user action.
    Action(KernelAction<'a>),
}

/// A normalized borrowed user action.
#[derive(Debug)]
pub enum KernelAction<'a> {
    /// Send one structured input atom to an explicitly eligible terminal.
    Input {
        /// Target terminal selected by the frontend.
        terminal_id: &'a TerminalId,
        /// Borrowed protocol input atom.
        event: &'a InputEvent,
    },
}

/// Why a terminal cannot currently receive user input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputBlockReason {
    /// The terminal is not part of the current kernel state.
    UnknownTerminal,
    /// No dual-READY replica is published yet.
    AwaitingReplica,
    /// The aggregate attach barrier has not been released.
    AwaitingAttachReady,
    /// The terminal was permanently closed.
    Closed,
}

/// Explicit per-terminal input eligibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEligibility {
    /// Input targets this published generation.
    Eligible {
        /// Published logical subscription.
        stream_id: StreamId,
        /// Published replica generation.
        bootstrap_id: BootstrapId,
    },
    /// Input is currently rejected for the stated reason.
    Ineligible(InputBlockReason),
}

/// A typed transport send requested by the kernel.
#[derive(Debug, Clone, PartialEq)]
pub enum KernelSend {
    /// Send one structured protocol input atom.
    Input {
        /// Explicit target terminal.
        terminal_id: TerminalId,
        /// Owned atom ready for transport framing outside the kernel.
        event: InputEvent,
    },
    /// Write a terminal-engine response to the owning PTY.
    PtyWrite {
        /// Terminal whose engine generated the response.
        terminal_id: TerminalId,
        /// One response payload; batching and encoding remain outside.
        bytes: Vec<u8>,
    },
}

/// Frontend-neutral render invalidation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelDamage {
    /// Damaged terminal.
    pub terminal_id: TerminalId,
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

/// Frontend-neutral terminal status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelStatus {
    /// Terminal reporting status.
    pub terminal_id: TerminalId,
    /// Engine status payload.
    pub status: EngineStatus,
}

/// Cooperative work handed to the host executor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelJob {
    /// Terminal requesting work.
    pub terminal_id: TerminalId,
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
    pub fn len(&self) -> usize {
        self.effects.len()
    }

    /// Whether the last update produced no effects.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.effects.is_empty()
    }

    /// Current reusable allocation capacity.
    #[must_use]
    pub fn capacity(&self) -> usize {
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

    fn push(&mut self, effect: KernelEffect) {
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

/// Borrowed access to an atomically published engine replica.
pub struct PublishedReplica<'a, E: EngineAdapter> {
    key: &'a ReplicaKey,
    geometry: CanonicalGeometry,
    last_seq: u64,
    engine: &'a E::Replica,
}

impl<'a, E: EngineAdapter> PublishedReplica<'a, E> {
    /// Exact protocol identity and profile.
    #[must_use]
    pub const fn key(&self) -> &ReplicaKey {
        self.key
    }

    /// Canonical live PTY geometry.
    #[must_use]
    pub const fn geometry(&self) -> CanonicalGeometry {
        self.geometry
    }

    /// Highest contiguous live sequence applied to this replica.
    #[must_use]
    pub const fn last_seq(&self) -> u64 {
        self.last_seq
    }

    /// Borrow the adapter-owned live state for frontend projection.
    #[must_use]
    pub const fn engine(&self) -> &E::Replica {
        self.engine
    }
}

/// Borrowed staging state, exposed for diagnostics without publication.
pub struct StagingReplica<'a, E: EngineAdapter> {
    key: &'a ReplicaKey,
    geometry: CanonicalGeometry,
    engine_ready: bool,
    protocol_ready: bool,
    engine: &'a E::Replica,
}

impl<'a, E: EngineAdapter> StagingReplica<'a, E> {
    /// Exact protocol identity and profile.
    #[must_use]
    pub const fn key(&self) -> &ReplicaKey {
        self.key
    }

    /// Canonical live PTY geometry.
    #[must_use]
    pub const fn geometry(&self) -> CanonicalGeometry {
        self.geometry
    }

    /// Whether the engine reported its internal READY boundary.
    #[must_use]
    pub const fn engine_ready(&self) -> bool {
        self.engine_ready
    }

    /// Whether protocol `BOOTSTRAP_READY` arrived.
    #[must_use]
    pub const fn protocol_ready(&self) -> bool {
        self.protocol_ready
    }

    /// Borrow the adapter-owned staging state.
    #[must_use]
    pub const fn engine(&self) -> &E::Replica {
        self.engine
    }
}

/// A deterministic protocol or adapter failure.
#[derive(Debug, thiserror::Error)]
pub enum KernelError<E: std::error::Error + 'static> {
    /// The stream profile differs from the profile selected by `HELLO_OK`.
    #[error("bootstrap profile {incoming:?} does not match selected profile {selected:?}")]
    ProfileMismatch {
        /// Connection-selected profile.
        selected: BootstrapProfile,
        /// Stream-local profile from `BOOTSTRAP_BEGIN`.
        incoming: BootstrapStreamProfile,
    },
    /// The authoritative live geometry contained an empty axis.
    #[error("invalid canonical geometry {cols}x{rows}")]
    InvalidGeometry {
        /// Received cell width.
        cols: u16,
        /// Received cell height.
        rows: u16,
    },
    /// An attach was restarted before its barrier completed.
    #[error("attach {active_attach_id} is still waiting for ATTACH_READY")]
    AttachInProgress {
        /// Active attach correlation identifier.
        active_attach_id: u32,
    },
    /// The attach inventory named a terminal more than once.
    #[error("attach inventory contains duplicate terminal {0}")]
    DuplicateAttachTerminal(TerminalId),
    /// An event named a permanently closed terminal.
    #[error("terminal {0} is closed")]
    ClosedTerminal(TerminalId),
    /// `ATTACH_READY` did not match the active attach.
    #[error("ATTACH_READY {actual} does not match active attach {expected:?}")]
    AttachIdMismatch {
        /// Active attach identifier, if any.
        expected: Option<u32>,
        /// Received attach identifier.
        actual: u32,
    },
    /// `ATTACH_READY` arrived before every pane was READY or closed.
    #[error("ATTACH_READY arrived with {remaining} unresolved terminals")]
    AttachNotReady {
        /// Number of unresolved attach participants.
        remaining: usize,
    },
    /// No state exists for the target terminal.
    #[error("unknown terminal {0}")]
    UnknownTerminal(TerminalId),
    /// The exact stream/bootstrap generation is not current.
    #[error("generation ({stream_id}, {bootstrap_id}) is not current for {terminal_id}")]
    GenerationMismatch {
        /// Target terminal.
        terminal_id: TerminalId,
        /// Received logical subscription.
        stream_id: StreamId,
        /// Received generation.
        bootstrap_id: BootstrapId,
    },
    /// The exact stream/bootstrap generation was permanently retired.
    #[error("generation ({stream_id}, {bootstrap_id}) is retired for {terminal_id}")]
    RetiredGeneration {
        /// Target terminal.
        terminal_id: TerminalId,
        /// Retired logical subscription.
        stream_id: StreamId,
        /// Retired generation.
        bootstrap_id: BootstrapId,
    },
    /// The same generation began more than once.
    #[error("generation ({stream_id}, {bootstrap_id}) already exists for {terminal_id}")]
    DuplicateGeneration {
        /// Target terminal.
        terminal_id: TerminalId,
        /// Logical subscription.
        stream_id: StreamId,
        /// Replica generation.
        bootstrap_id: BootstrapId,
    },
    /// A publication fence was reached without its staging replica.
    #[error("terminal {0} has no staging replica to publish")]
    MissingStaging(TerminalId),
    /// A bootstrap chunk repeated or moved backward.
    #[error("duplicate bootstrap chunk {actual}; expected {expected}")]
    DuplicateChunk {
        /// Next required chunk sequence.
        expected: u32,
        /// Received chunk sequence.
        actual: u32,
    },
    /// A bootstrap chunk skipped forward.
    #[error("bootstrap chunk gap at {actual}; expected {expected}")]
    ChunkGap {
        /// Next required chunk sequence.
        expected: u32,
        /// Received chunk sequence.
        actual: u32,
    },
    /// No chunk sequence remains representable.
    #[error("bootstrap chunk sequence exhausted")]
    ChunkSequenceExhausted,
    /// A chunk arrived after the engine's READY boundary.
    #[error("bootstrap chunk arrived after engine READY")]
    ChunkAfterEngineReady,
    /// Protocol READY was duplicated.
    #[error("duplicate protocol BOOTSTRAP_READY")]
    DuplicateProtocolReady,
    /// Protocol READY arrived but the engine decoder did not become READY.
    #[error("protocol BOOTSTRAP_READY did not reach engine READY")]
    EngineNotReady,
    /// A live sequence repeated or moved backward.
    #[error("duplicate live sequence {actual}; expected {expected}")]
    DuplicateSequence {
        /// Next required live sequence.
        expected: u64,
        /// Received live sequence.
        actual: u64,
    },
    /// A live sequence skipped forward.
    #[error("live sequence gap at {actual}; expected {expected}")]
    SequenceGap {
        /// Next required live sequence.
        expected: u64,
        /// Received live sequence.
        actual: u64,
    },
    /// No live sequence remains representable.
    #[error("live sequence exhausted")]
    SequenceExhausted,
    /// The frontend attempted input while its target was ineligible.
    #[error("input is not eligible for {terminal_id}: {reason:?}")]
    InputIneligible {
        /// Explicit input target.
        terminal_id: TerminalId,
        /// Deterministic gate reason.
        reason: InputBlockReason,
    },
    /// The terminal engine rejected an operation.
    #[error("terminal engine failed: {0}")]
    Engine(#[source] E),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct GenerationId {
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
}

struct Replica<R> {
    key: ReplicaKey,
    geometry: CanonicalGeometry,
    last_seq: u64,
    next_seq: Option<u64>,
    engine: R,
}

struct Staging<R> {
    key: ReplicaKey,
    geometry: CanonicalGeometry,
    base_seq: u64,
    next_chunk_seq: Option<u32>,
    engine_ready: bool,
    protocol_ready: bool,
    engine: R,
}

struct TerminalState<R> {
    published: Option<Replica<R>>,
    staging: Option<Staging<R>>,
    retired: HashMap<GenerationId, TombstoneRecord>,
}

impl<R> Default for TerminalState<R> {
    fn default() -> Self {
        Self {
            published: None,
            staging: None,
            retired: HashMap::new(),
        }
    }
}

struct AttachParticipant {
    terminal_id: TerminalId,
    resolved: bool,
}

struct AttachState {
    attach_id: u32,
    released: bool,
    terminals: Vec<AttachParticipant>,
}

/// Frontend-neutral synchronous client session kernel.
pub struct SessionKernel<E: EngineAdapter> {
    adapter: E,
    selected_profile: BootstrapProfile,
    terminals: HashMap<TerminalId, TerminalState<E::Replica>>,
    closed: HashSet<TerminalId>,
    attach: Option<AttachState>,
    engine_effects: EngineEffectBuffer,
}

impl<E: EngineAdapter> SessionKernel<E> {
    /// Construct a kernel bound to the exact profile selected by `HELLO_OK`.
    #[must_use]
    pub fn new(adapter: E, selected_profile: BootstrapProfile) -> Self {
        Self {
            adapter,
            selected_profile,
            terminals: HashMap::new(),
            closed: HashSet::new(),
            attach: None,
            engine_effects: EngineEffectBuffer::new(),
        }
    }

    /// The immutable profile selected for this connection.
    #[must_use]
    pub const fn selected_profile(&self) -> BootstrapProfile {
        self.selected_profile
    }

    /// Borrow the engine adapter for diagnostics or frontend-owned projection helpers.
    #[must_use]
    pub const fn adapter(&self) -> &E {
        &self.adapter
    }

    /// Mutably borrow the engine adapter outside an update.
    #[must_use]
    pub const fn adapter_mut(&mut self) -> &mut E {
        &mut self.adapter
    }

    /// Borrow the published replica for one terminal.
    #[must_use]
    pub fn published(&self, terminal_id: &TerminalId) -> Option<PublishedReplica<'_, E>> {
        let replica = self.terminals.get(terminal_id)?.published.as_ref()?;
        Some(PublishedReplica {
            key: &replica.key,
            geometry: replica.geometry,
            last_seq: replica.last_seq,
            engine: &replica.engine,
        })
    }

    /// Borrow the unpublished staging replica for one terminal.
    #[must_use]
    pub fn staging(&self, terminal_id: &TerminalId) -> Option<StagingReplica<'_, E>> {
        let staging = self.terminals.get(terminal_id)?.staging.as_ref()?;
        Some(StagingReplica {
            key: &staging.key,
            geometry: staging.geometry,
            engine_ready: staging.engine_ready,
            protocol_ready: staging.protocol_ready,
            engine: &staging.engine,
        })
    }

    /// Return a retained tombstone, if this exact generation is retired.
    #[must_use]
    pub fn tombstone(
        &self,
        terminal_id: &TerminalId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
    ) -> Option<TombstoneRecord> {
        self.terminals
            .get(terminal_id)?
            .retired
            .get(&GenerationId {
                stream_id,
                bootstrap_id,
            })
            .copied()
    }

    /// Compute explicit input eligibility without producing effects.
    #[must_use]
    pub fn input_eligibility(&self, terminal_id: &TerminalId) -> InputEligibility {
        if self.closed.contains(terminal_id) {
            return InputEligibility::Ineligible(InputBlockReason::Closed);
        }
        let Some(state) = self.terminals.get(terminal_id) else {
            return InputEligibility::Ineligible(InputBlockReason::UnknownTerminal);
        };
        let Some(replica) = state.published.as_ref() else {
            return InputEligibility::Ineligible(InputBlockReason::AwaitingReplica);
        };
        if self.attach_blocks(terminal_id) {
            return InputEligibility::Ineligible(InputBlockReason::AwaitingAttachReady);
        }
        InputEligibility::Eligible {
            stream_id: replica.key.stream_id,
            bootstrap_id: replica.key.bootstrap_id,
        }
    }

    /// Apply one normalized input and replace `effects` with its declarative result.
    pub fn update(
        &mut self,
        input: KernelInput<'_>,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        effects.clear();
        match input {
            KernelInput::AttachStarted {
                attach_id,
                terminals,
            } => self.attach_started(attach_id, terminals),
            KernelInput::AttachReady { attach_id } => self.attach_ready(attach_id, effects),
            KernelInput::BootstrapBegin {
                terminal_id,
                stream_id,
                bootstrap_id,
                profile,
                geometry,
                base_seq,
            } => self.bootstrap_begin(
                terminal_id,
                stream_id,
                bootstrap_id,
                profile,
                geometry,
                base_seq,
            ),
            KernelInput::BootstrapChunk {
                terminal_id,
                stream_id,
                bootstrap_id,
                chunk_seq,
                payload,
            } => self.bootstrap_chunk(
                terminal_id,
                stream_id,
                bootstrap_id,
                chunk_seq,
                payload,
            ),
            KernelInput::BootstrapReady {
                terminal_id,
                stream_id,
                bootstrap_id,
            } => self.bootstrap_ready(terminal_id, stream_id, bootstrap_id, effects),
            KernelInput::TerminalOutput {
                terminal_id,
                stream_id,
                bootstrap_id,
                seq,
                payload,
            } => self.terminal_output(
                terminal_id,
                stream_id,
                bootstrap_id,
                seq,
                payload,
                effects,
            ),
            KernelInput::Tombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                reason,
                last_valid_seq,
            } => {
                self.tombstone_generation(
                    terminal_id,
                    stream_id,
                    bootstrap_id,
                    TombstoneRecord {
                        reason,
                        last_valid_seq,
                    },
                    effects,
                );
                Ok(())
            }
            KernelInput::TerminalClosed { terminal_id } => {
                self.terminal_closed(terminal_id, effects);
                Ok(())
            }
            KernelInput::Action(action) => self.action(action, effects),
        }
    }

    fn attach_started(
        &mut self,
        attach_id: u32,
        terminal_ids: &[TerminalId],
    ) -> Result<(), KernelError<E::Error>> {
        if let Some(attach) = self.attach.as_ref() {
            if !attach.released {
                return Err(KernelError::AttachInProgress {
                    active_attach_id: attach.attach_id,
                });
            }
        }
        for (index, terminal_id) in terminal_ids.iter().enumerate() {
            if self.closed.contains(terminal_id) {
                return Err(KernelError::ClosedTerminal(terminal_id.clone()));
            }
            if terminal_ids[..index].contains(terminal_id) {
                return Err(KernelError::DuplicateAttachTerminal(terminal_id.clone()));
            }
        }

        self.terminals.reserve(terminal_ids.len());
        for terminal_id in terminal_ids {
            self.terminals.entry(terminal_id.clone()).or_default();
        }

        let attach = self.attach.get_or_insert_with(|| AttachState {
            attach_id,
            released: false,
            terminals: Vec::with_capacity(terminal_ids.len()),
        });
        attach.attach_id = attach_id;
        attach.released = false;
        attach.terminals.clear();
        attach.terminals.reserve(terminal_ids.len());
        attach
            .terminals
            .extend(terminal_ids.iter().cloned().map(|terminal_id| AttachParticipant {
                terminal_id,
                resolved: false,
            }));
        Ok(())
    }

    fn attach_ready(
        &mut self,
        attach_id: u32,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        let Some(attach) = self.attach.as_mut() else {
            return Err(KernelError::AttachIdMismatch {
                expected: None,
                actual: attach_id,
            });
        };
        if attach.attach_id != attach_id {
            return Err(KernelError::AttachIdMismatch {
                expected: Some(attach.attach_id),
                actual: attach_id,
            });
        }
        let remaining = attach
            .terminals
            .iter()
            .filter(|participant| !participant.resolved)
            .count();
        if remaining != 0 {
            return Err(KernelError::AttachNotReady { remaining });
        }
        if attach.released {
            return Err(KernelError::AttachIdMismatch {
                expected: None,
                actual: attach_id,
            });
        }

        attach.released = true;
        for participant in &attach.terminals {
            let Some(replica) = self
                .terminals
                .get_mut(&participant.terminal_id)
                .and_then(|state| state.published.as_mut())
            else {
                continue;
            };
            effects.push(KernelEffect::Damage(KernelDamage {
                terminal_id: participant.terminal_id.clone(),
                kind: KernelDamageKind::Full,
            }));
        }
        Ok(())
    }

    fn bootstrap_begin(
        &mut self,
        terminal_id: &TerminalId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        profile: BootstrapStreamProfile,
        geometry: CanonicalGeometry,
        base_seq: u64,
    ) -> Result<(), KernelError<E::Error>> {
        if self.closed.contains(terminal_id) {
            return Err(KernelError::ClosedTerminal(terminal_id.clone()));
        }
        if !profile_matches(self.selected_profile, profile) {
            return Err(KernelError::ProfileMismatch {
                selected: self.selected_profile,
                incoming: profile,
            });
        }
        if geometry.cols == 0 || geometry.rows == 0 {
            return Err(KernelError::InvalidGeometry {
                cols: geometry.cols,
                rows: geometry.rows,
            });
        }

        let generation = GenerationId {
            stream_id,
            bootstrap_id,
        };
        let state = self.terminals.entry(terminal_id.clone()).or_default();
        if state.retired.contains_key(&generation) {
            return Err(KernelError::RetiredGeneration {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
            });
        }
        if state
            .published
            .as_ref()
            .is_some_and(|replica| generation_of(&replica.key) == generation)
            || state
                .staging
                .as_ref()
                .is_some_and(|staging| generation_of(&staging.key) == generation)
        {
            return Err(KernelError::DuplicateGeneration {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
            });
        }

        self.engine_effects.clear();
        let engine = match self.adapter.start_replica(profile, geometry) {
            Ok(engine) => engine,
            Err(error) => return Err(KernelError::Engine(error)),
        };
        if let Some(old_staging) = state.staging.replace(Staging {
            key: ReplicaKey {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                profile,
            },
            geometry,
            base_seq,
            next_chunk_seq: Some(0),
            engine_ready: false,
            protocol_ready: false,
            engine,
        }) {
            state.retired.insert(
                generation_of(&old_staging.key),
                TombstoneRecord {
                    reason: TombstoneReason::ExplicitReattach,
                    last_valid_seq: old_staging.base_seq,
                },
            );
        }
        Ok(())
    }

    fn bootstrap_chunk(
        &mut self,
        terminal_id: &TerminalId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        chunk_seq: u32,
        payload: &[u8],
    ) -> Result<(), KernelError<E::Error>> {
        self.ensure_open(terminal_id)?;
        let generation = GenerationId {
            stream_id,
            bootstrap_id,
        };
        let state = self
            .terminals
            .get_mut(terminal_id)
            .ok_or_else(|| KernelError::UnknownTerminal(terminal_id.clone()))?;
        if state.retired.contains_key(&generation) {
            return Err(retired_error(terminal_id, generation));
        }
        let staging = state
            .staging
            .as_mut()
            .filter(|staging| generation_of(&staging.key) == generation)
            .ok_or_else(|| mismatch_error(terminal_id, generation))?;
        if staging.engine_ready {
            return Err(KernelError::ChunkAfterEngineReady);
        }
        let Some(expected) = staging.next_chunk_seq else {
            return Err(KernelError::ChunkSequenceExhausted);
        };
        if chunk_seq < expected {
            return Err(KernelError::DuplicateChunk {
                expected,
                actual: chunk_seq,
            });
        }
        if chunk_seq > expected {
            return Err(KernelError::ChunkGap {
                expected,
                actual: chunk_seq,
            });
        }

        self.engine_effects.clear();
        let progress = match self.adapter.apply_bootstrap_chunk(
            &mut staging.engine,
            payload,
            &mut self.engine_effects,
        ) {
            Ok(progress) => progress,
            Err(error) => {
                self.engine_effects.clear();
                return Err(KernelError::Engine(error));
            }
        };
        staging.next_chunk_seq = chunk_seq.checked_add(1);
        staging.engine_ready |= progress.is_ready();
        let mut captured = std::mem::take(&mut self.engine_effects);
        captured.clear();
        self.engine_effects = captured;
        Ok(())
    }

    fn bootstrap_ready(
        &mut self,
        terminal_id: &TerminalId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        self.ensure_open(terminal_id)?;
        let generation = GenerationId {
            stream_id,
            bootstrap_id,
        };
        let state = self
            .terminals
            .get_mut(terminal_id)
            .ok_or_else(|| KernelError::UnknownTerminal(terminal_id.clone()))?;
        if state.retired.contains_key(&generation) {
            return Err(retired_error(terminal_id, generation));
        }
        let staging = state
            .staging
            .as_mut()
            .filter(|staging| generation_of(&staging.key) == generation)
            .ok_or_else(|| mismatch_error(terminal_id, generation))?;
        if staging.protocol_ready {
            return Err(KernelError::DuplicateProtocolReady);
        }
        staging.protocol_ready = true;

        self.engine_effects.clear();
        let progress = match self.adapter.finish_bootstrap(
            &mut staging.engine,
            &mut self.engine_effects,
        ) {
            Ok(progress) => progress,
            Err(error) => {
                self.engine_effects.clear();
                return Err(KernelError::Engine(error));
            }
        };
        staging.engine_ready |= progress.is_ready();
        let mut captured = std::mem::take(&mut self.engine_effects);
        captured.clear();
        self.engine_effects = captured;
        if !staging.engine_ready {
            return Err(KernelError::EngineNotReady);
        }

        self.publish(terminal_id, effects)
    }

    fn publish(
        &mut self,
        terminal_id: &TerminalId,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        let state = self
            .terminals
            .get_mut(terminal_id)
            .ok_or_else(|| KernelError::UnknownTerminal(terminal_id.clone()))?;
        let staging = state
            .staging
            .take()
            .ok_or_else(|| KernelError::MissingStaging(terminal_id.clone()))?;
        debug_assert!(staging.engine_ready && staging.protocol_ready);
        let replacement = Replica {
            key: staging.key,
            geometry: staging.geometry,
            last_seq: staging.base_seq,
            next_seq: staging.base_seq.checked_add(1),
            engine: staging.engine,
        };
        if let Some(old) = state.published.replace(replacement) {
            state.retired.insert(
                generation_of(&old.key),
                TombstoneRecord {
                    reason: TombstoneReason::ExplicitReattach,
                    last_valid_seq: old.last_seq,
                },
            );
        }
        self.mark_attach_resolved(terminal_id);
        if !self.attach_blocks(terminal_id) {
            effects.push(KernelEffect::Damage(KernelDamage {
                terminal_id: terminal_id.clone(),
                kind: KernelDamageKind::Full,
            }));
        }
        Ok(())
    }

    fn terminal_output(
        &mut self,
        terminal_id: &TerminalId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        seq: u64,
        payload: &[u8],
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        self.ensure_open(terminal_id)?;
        let generation = GenerationId {
            stream_id,
            bootstrap_id,
        };
        let state = self
            .terminals
            .get_mut(terminal_id)
            .ok_or_else(|| KernelError::UnknownTerminal(terminal_id.clone()))?;
        if state.retired.contains_key(&generation) {
            return Err(retired_error(terminal_id, generation));
        }
        let replica = state
            .published
            .as_mut()
            .filter(|replica| generation_of(&replica.key) == generation)
            .ok_or_else(|| mismatch_error(terminal_id, generation))?;
        let Some(expected) = replica.next_seq else {
            return Err(KernelError::SequenceExhausted);
        };
        if seq < expected {
            return Err(KernelError::DuplicateSequence {
                expected,
                actual: seq,
            });
        }
        if seq > expected {
            return Err(KernelError::SequenceGap {
                expected,
                actual: seq,
            });
        }

        self.engine_effects.clear();
        if let Err(error) = self.adapter.apply_output(
            &mut replica.engine,
            payload,
            &mut self.engine_effects,
        ) {
            self.engine_effects.clear();
            return Err(KernelError::Engine(error));
        }
        replica.last_seq = seq;
        replica.next_seq = seq.checked_add(1);

        let damage_allowed = !self.attach_blocks(terminal_id);
        let mut captured = std::mem::take(&mut self.engine_effects);
        for effect in captured.drain() {
            self.translate_engine_effect(terminal_id, effect, damage_allowed, effects);
        }
        self.engine_effects = captured;
        Ok(())
    }

    fn translate_engine_effect(
        &mut self,
        terminal_id: &TerminalId,
        effect: EngineEffect,
        damage_allowed: bool,
        effects: &mut EffectBuffer,
    ) {
        match effect {
            EngineEffect::Send(EngineSend::PtyWrite(bytes)) => {
                effects.push(KernelEffect::Send(KernelSend::PtyWrite {
                    terminal_id: terminal_id.clone(),
                    bytes,
                }));
            }
            EngineEffect::Damage(damage) if damage_allowed => {
                effects.push(KernelEffect::Damage(KernelDamage {
                    terminal_id: terminal_id.clone(),
                    kind: match damage {
                        EngineDamage::Full => KernelDamageKind::Full,
                        EngineDamage::Rows { first, last } => {
                            KernelDamageKind::Rows { first, last }
                        }
                    },
                }));
            }
            EngineEffect::Damage(_) => {}
            EngineEffect::Status(status) => {
                effects.push(KernelEffect::Status(KernelStatus {
                    terminal_id: terminal_id.clone(),
                    status,
                }));
            }
            EngineEffect::Job(job) => {
                effects.push(KernelEffect::Job(KernelJob {
                    terminal_id: terminal_id.clone(),
                    job,
                }));
            }
        }
    }

    fn tombstone_generation(
        &mut self,
        terminal_id: &TerminalId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        record: TombstoneRecord,
        effects: &mut EffectBuffer,
    ) {
        let generation = GenerationId {
            stream_id,
            bootstrap_id,
        };
        let state = self.terminals.entry(terminal_id.clone()).or_default();
        state.retired.entry(generation).or_insert(record);
        if state
            .staging
            .as_ref()
            .is_some_and(|staging| generation_of(&staging.key) == generation)
        {
            state.staging = None;
        }
        let removed_published = state
            .published
            .as_ref()
            .is_some_and(|replica| generation_of(&replica.key) == generation);
        if removed_published {
            state.published = None;
            self.mark_attach_unresolved(terminal_id);
            if !self.attach_blocks(terminal_id) {
                effects.push(KernelEffect::Damage(KernelDamage {
                    terminal_id: terminal_id.clone(),
                    kind: KernelDamageKind::Removed,
                }));
            }
        }
    }

    fn terminal_closed(&mut self, terminal_id: &TerminalId, effects: &mut EffectBuffer) {
        let had_published = self
            .terminals
            .remove(terminal_id)
            .is_some_and(|state| state.published.is_some());
        self.closed.insert(terminal_id.clone());
        self.mark_attach_resolved(terminal_id);
        if had_published && !self.attach_blocks(terminal_id) {
            effects.push(KernelEffect::Damage(KernelDamage {
                terminal_id: terminal_id.clone(),
                kind: KernelDamageKind::Removed,
            }));
        }
    }

    fn action(
        &mut self,
        action: KernelAction<'_>,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        match action {
            KernelAction::Input { terminal_id, event } => {
                let eligibility = self.input_eligibility(terminal_id);
                if let InputEligibility::Ineligible(reason) = eligibility {
                    return Err(KernelError::InputIneligible {
                        terminal_id: terminal_id.clone(),
                        reason,
                    });
                }
                effects.push(KernelEffect::Send(KernelSend::Input {
                    terminal_id: terminal_id.clone(),
                    event: event.clone(),
                }));
                Ok(())
            }
        }
    }

    fn ensure_open(&self, terminal_id: &TerminalId) -> Result<(), KernelError<E::Error>> {
        if self.closed.contains(terminal_id) {
            Err(KernelError::ClosedTerminal(terminal_id.clone()))
        } else {
            Ok(())
        }
    }

    fn attach_blocks(&self, terminal_id: &TerminalId) -> bool {
        self.attach.as_ref().is_some_and(|attach| {
            !attach.released
                && attach
                    .terminals
                    .iter()
                    .any(|participant| &participant.terminal_id == terminal_id)
        })
    }

    fn mark_attach_resolved(&mut self, terminal_id: &TerminalId) {
        if let Some(participant) = self.attach.as_mut().and_then(|attach| {
            attach
                .terminals
                .iter_mut()
                .find(|participant| &participant.terminal_id == terminal_id)
        }) {
            participant.resolved = true;
        }
    }

    fn mark_attach_unresolved(&mut self, terminal_id: &TerminalId) {
        if let Some(participant) = self.attach.as_mut().and_then(|attach| {
            attach
                .terminals
                .iter_mut()
                .find(|participant| &participant.terminal_id == terminal_id)
        }) {
            participant.resolved = false;
        }
    }
}

const fn generation_of(key: &ReplicaKey) -> GenerationId {
    GenerationId {
        stream_id: key.stream_id,
        bootstrap_id: key.bootstrap_id,
    }
}

fn mismatch_error<E: std::error::Error + 'static>(
    terminal_id: &TerminalId,
    generation: GenerationId,
) -> KernelError<E> {
    KernelError::GenerationMismatch {
        terminal_id: terminal_id.clone(),
        stream_id: generation.stream_id,
        bootstrap_id: generation.bootstrap_id,
    }
}

fn retired_error<E: std::error::Error + 'static>(
    terminal_id: &TerminalId,
    generation: GenerationId,
) -> KernelError<E> {
    KernelError::RetiredGeneration {
        terminal_id: terminal_id.clone(),
        stream_id: generation.stream_id,
        bootstrap_id: generation.bootstrap_id,
    }
}

const fn profile_matches(
    selected: BootstrapProfile,
    incoming: BootstrapStreamProfile,
) -> bool {
    match (selected, incoming) {
        (
            BootstrapProfile::NativeState { codec: selected, .. },
            BootstrapStreamProfile::NativeState { codec: incoming },
        ) => selected as u8 == incoming as u8,
        (BootstrapProfile::SynthesizedVtRaw, BootstrapStreamProfile::SynthesizedVtRaw)
        | (
            BootstrapProfile::SynthesizedVtStateSync,
            BootstrapStreamProfile::SynthesizedVtStateSync,
        ) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests;
