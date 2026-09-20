//! The engine owner thread (ADR-0133 decision 4).
//!
//! `SessionKernel` and every engine replica live on one dedicated thread.
//! Nothing that is not `Send` ever leaves it: the control plane hands it
//! owned [`EngineEvent`]s over a channel and gets owned [`EngineOutcome`]s
//! back, and the grid it projects reaches consumers only through the
//! `Publication` table. With the `engine` feature the adapter is
//! libghostty's `GhosttyAdapter` and every damaged terminal is re-projected
//! and published before the outcome is answered; without it a bounded
//! `ByteAdapter` keeps the same kernel driving a byte buffer per
//! terminal, so the transport and control-plane lanes build and test with
//! no Zig toolchain (phux-mobile's headless lane).

#[cfg(feature = "engine")]
use std::collections::HashMap;
use std::collections::HashSet;
#[cfg(feature = "engine")]
use std::sync::Arc;
use std::sync::mpsc::{self, Sender};

#[cfg(feature = "engine")]
use libghostty_vt::terminal::ScrollViewport;
#[cfg(feature = "engine")]
use phux_client_core::engine::ghostty::{GhosttyAdapter, GhosttyReplica};
use phux_client_core::engine::{CanonicalGeometry, EngineAdapter};
#[cfg(feature = "engine")]
use phux_client_core::grid::GridProjector;
use phux_client_core::history::HistoryCacheConfig;
#[cfg(feature = "engine")]
use phux_client_core::session::ClosedReplica;
use phux_client_core::session::{
    AgentSessionDeclaration, EffectBuffer, HistoryRejectionReason, HistoryUnavailableReason,
    InputBlockReason, InputEligibility, KernelDamageKind, KernelEffect, KernelInput, KernelStatus,
    SessionKernel,
};
use phux_protocol::caps::{BootstrapLimits, BootstrapProfile, BootstrapStreamProfile};
use phux_protocol::ids::{BootstrapId, ResourceId, StreamId};
use phux_protocol::wire::frame::{AgentEvent, CloseReason, TombstoneReason};

#[cfg(feature = "engine")]
use crate::publication::{FrameColors, GridBuffer, GridFrame, Publication, Rgb, Scrollbar};

#[cfg(feature = "engine")]
type Adapter = GhosttyAdapter;
#[cfg(not(feature = "engine"))]
type Adapter = byte_adapter::ByteAdapter;

/// The headless replica: a bounded byte buffer per terminal.
#[cfg(not(feature = "engine"))]
pub mod byte_adapter {
    use phux_client_core::engine::{
        BootstrapProgress, CanonicalGeometry, EngineAdapter, EngineDamage, EngineEffect,
        EngineEffectBuffer, HistoryApplyOutcome,
    };
    use phux_protocol::caps::BootstrapStreamProfile;

    /// Bytes one headless replica retains before it refuses more.
    pub const HEADLESS_BUFFER_CAP: usize = 1 << 20;

    /// An adapter that keeps the raw synthesized-VT bytes of each replica.
    #[derive(Debug, Default)]
    pub struct ByteAdapter;

    /// One headless replica.
    #[derive(Debug, Default)]
    pub struct ByteReplica {
        /// Every byte applied since the replica started, up to the cap.
        pub bytes: Vec<u8>,
    }

    /// Why the headless adapter refused.
    #[derive(Debug, thiserror::Error)]
    pub enum ByteAdapterError {
        /// Only synthesized VT streams have bytes a headless lane can keep.
        #[error("headless adapter only accepts synthesized VT streams")]
        UnsupportedProfile,
        /// The replica outgrew [`HEADLESS_BUFFER_CAP`].
        #[error("headless projection exceeded its bounded byte budget")]
        BufferOverflow,
    }

    impl ByteAdapter {
        fn append(replica: &mut ByteReplica, payload: &[u8]) -> Result<(), ByteAdapterError> {
            if replica.bytes.len().saturating_add(payload.len()) > HEADLESS_BUFFER_CAP {
                return Err(ByteAdapterError::BufferOverflow);
            }
            replica.bytes.extend_from_slice(payload);
            Ok(())
        }
    }

    impl EngineAdapter for ByteAdapter {
        type Replica = ByteReplica;
        type Error = ByteAdapterError;

        fn start_replica(
            &mut self,
            profile: BootstrapStreamProfile,
            _geometry: CanonicalGeometry,
        ) -> Result<Self::Replica, Self::Error> {
            if !matches!(
                profile,
                BootstrapStreamProfile::SynthesizedVtRaw
                    | BootstrapStreamProfile::SynthesizedVtStateSync
            ) {
                return Err(ByteAdapterError::UnsupportedProfile);
            }
            Ok(ByteReplica::default())
        }

        fn apply_bootstrap_chunk(
            &mut self,
            replica: &mut Self::Replica,
            payload: &[u8],
            _effects: &mut EngineEffectBuffer,
        ) -> Result<BootstrapProgress, Self::Error> {
            Self::append(replica, payload)?;
            Ok(BootstrapProgress::Pending)
        }

        fn bootstrap_staging_bytes(&self, replica: &Self::Replica) -> usize {
            replica.bytes.capacity()
        }

        fn finish_bootstrap(
            &mut self,
            _replica: &mut Self::Replica,
            _effects: &mut EngineEffectBuffer,
        ) -> Result<BootstrapProgress, Self::Error> {
            Ok(BootstrapProgress::Finished)
        }

        fn apply_history_page(
            &mut self,
            replica: &mut Self::Replica,
            payload: &[u8],
            declared_rows: u32,
            _effects: &mut EngineEffectBuffer,
        ) -> Result<HistoryApplyOutcome, Self::Error> {
            Self::append(replica, payload)?;
            Ok(HistoryApplyOutcome {
                progress: BootstrapProgress::Ready,
                retained_rows: declared_rows as usize,
                authenticated_rows: declared_rows as usize,
            })
        }

        fn apply_output(
            &mut self,
            replica: &mut Self::Replica,
            payload: &[u8],
            effects: &mut EngineEffectBuffer,
        ) -> Result<(), Self::Error> {
            Self::append(replica, payload)?;
            effects.push(EngineEffect::Damage(EngineDamage::Full));
            Ok(())
        }
    }
}

/// One owned kernel input, as the control plane decodes it from a frame.
#[derive(Debug, Clone)]
#[allow(
    missing_docs,
    reason = "each field mirrors the `KernelInput` field of the same name"
)]
pub enum EngineEvent {
    /// `ATTACHED`: the aggregate attach and its terminal inventory.
    AttachStarted {
        attach_id: u32,
        terminals: Vec<ResourceId>,
    },
    /// `ATTACH_READY`.
    AttachReady { attach_id: u32 },
    /// `BOOTSTRAP_BEGIN`.
    BootstrapBegin {
        terminal_id: ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        profile: BootstrapStreamProfile,
        cols: u16,
        rows: u16,
        base_seq: u64,
    },
    /// `BOOTSTRAP_CHUNK`.
    BootstrapChunk {
        terminal_id: ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        chunk_seq: u32,
        payload: Vec<u8>,
    },
    /// `BOOTSTRAP_READY`.
    BootstrapReady {
        terminal_id: ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        history_cursor: Option<Vec<u8>>,
    },
    /// `HISTORY_PAGE`.
    HistoryPage {
        terminal_id: ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        page_seq: u64,
        rows: u32,
        cursor: Vec<u8>,
        next_cursor: Option<Vec<u8>>,
        payload: Vec<u8>,
    },
    /// `HISTORY_TOMBSTONE`.
    HistoryTombstone {
        terminal_id: ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        cursor: Vec<u8>,
        reason: HistoryUnavailableReason,
    },
    /// `HISTORY_REJECTED`.
    HistoryRejected {
        terminal_id: ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        cursor: Vec<u8>,
        reason: HistoryRejectionReason,
        required_bytes: u32,
        required_rows: u32,
    },
    /// `RESOURCE_OUTPUT`.
    Output {
        terminal_id: ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        seq: u64,
        bytes: Vec<u8>,
    },
    /// `BOOTSTRAP_TOMBSTONE`.
    Tombstone {
        terminal_id: ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        reason: TombstoneReason,
        last_valid_seq: u64,
    },
    /// `RESOURCE_CLOSED`, or a close inferred from a topology that no
    /// longer lists the terminal.
    Closed {
        terminal_id: ResourceId,
        exit_status: Option<i32>,
        signal: Option<i32>,
        reason: CloseReason,
    },
    /// One subscribed `EVENT` scoped to a terminal.
    Agent {
        terminal_id: ResourceId,
        event: AgentEvent,
    },
    /// An `AgentSession` resource the attach snapshot declared.
    AgentSessionDeclared {
        terminal_id: ResourceId,
        parent: Option<ResourceId>,
        provider: Option<String>,
        native_id: Option<String>,
        state: Option<String>,
    },
}

impl EngineEvent {
    /// A close the control plane inferred (a pane missing from a fresh
    /// topology) rather than received.
    #[must_use]
    pub const fn closed_unknown(terminal_id: ResourceId) -> Self {
        Self::Closed {
            terminal_id,
            exit_status: None,
            signal: None,
            reason: CloseReason::Unknown,
        }
    }

    /// The terminal the event concerns, if it is terminal-scoped.
    #[must_use]
    pub const fn terminal_id(&self) -> Option<&ResourceId> {
        match self {
            Self::AttachStarted { .. } | Self::AttachReady { .. } => None,
            Self::BootstrapBegin { terminal_id, .. }
            | Self::BootstrapChunk { terminal_id, .. }
            | Self::BootstrapReady { terminal_id, .. }
            | Self::HistoryPage { terminal_id, .. }
            | Self::HistoryTombstone { terminal_id, .. }
            | Self::HistoryRejected { terminal_id, .. }
            | Self::Output { terminal_id, .. }
            | Self::Tombstone { terminal_id, .. }
            | Self::Closed { terminal_id, .. }
            | Self::Agent { terminal_id, .. }
            | Self::AgentSessionDeclared { terminal_id, .. } => Some(terminal_id),
        }
    }
}

/// What one applied event produced: the kernel's declarative effects, in
/// order, and the update error if the kernel refused.
///
/// Effects are handed back even when the update failed: a codec error can
/// require an acknowledgement and a resync in the same outcome, and
/// dropping them would break the kernel contract.
#[derive(Debug, Default)]
pub struct EngineOutcome {
    /// The kernel effects this event produced.
    pub effects: Vec<KernelEffect>,
    /// The kernel's refusal, if any.
    pub error: Option<String>,
}

impl EngineOutcome {
    /// Whether an effect asked for a fresh bootstrap.
    #[must_use]
    pub fn resync_required(&self) -> bool {
        self.effects.iter().any(|effect| {
            matches!(
                effect,
                KernelEffect::Status(KernelStatus::ResyncRequired { .. })
            )
        })
    }
}

/// A viewport scroll a consumer asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scroll {
    /// The top of the scrollback.
    Top,
    /// The live tail.
    Bottom,
    /// Relative rows; negative scrolls toward the top.
    Delta(i64),
    /// An absolute row offset from the top of the scrollable area.
    Row(u64),
}

/// Why an engine request failed.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// The owner thread is gone; the runtime is unusable.
    #[error("the engine owner thread stopped")]
    Stopped,
    /// The owner thread could not be spawned.
    #[error("could not start the engine owner thread: {0}")]
    Spawn(std::io::Error),
    /// The engine refused a render or a viewport operation.
    #[error("engine: {0}")]
    Engine(String),
}

/// What the owner thread needs to build its kernel.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// The bootstrap profile `HELLO_OK` selected.
    pub profile: BootstrapProfile,
    /// The payload limits `HELLO_OK` selected.
    pub limits: BootstrapLimits,
    /// The scrollback depth the attach requested; bounds the history cache.
    pub scrollback_lines: u32,
}

impl EngineConfig {
    fn history(&self) -> HistoryCacheConfig {
        HistoryCacheConfig {
            max_materialized_rows: self.scrollback_lines as usize,
            request_max_bytes: self.limits.max_history_page_bytes(),
            request_max_rows: self.scrollback_lines,
            ..HistoryCacheConfig::default()
        }
    }
}

enum Command {
    Apply(EngineEvent, Sender<EngineOutcome>),
    Lifecycle(Lifecycle),
    Query(Query),
}

enum Lifecycle {
    Detach(ResourceId),
    #[cfg(feature = "engine")]
    Retain(ResourceId, bool),
    #[cfg(feature = "engine")]
    Release(ResourceId),
}

enum Query {
    HasProjection(ResourceId, Sender<bool>),
    IsClosed(ResourceId, Sender<bool>),
    InputReady(ResourceId, Sender<bool>),
    #[cfg(feature = "engine")]
    Scroll(ResourceId, Scroll, Sender<Result<(), EngineError>>),
    #[cfg(feature = "engine")]
    IsAltScreen(ResourceId, Sender<bool>),
    #[cfg(feature = "engine")]
    Republish(ResourceId, Sender<Result<bool, EngineError>>),
    #[cfg(not(feature = "engine"))]
    TakeOutput(ResourceId, Sender<Vec<u8>>),
}

/// The thread-safe handle on the owner thread. Cloning it shares the
/// thread; dropping the last clone stops it.
#[derive(Clone, Debug)]
pub struct EngineHandle {
    commands: Sender<Command>,
}

impl EngineHandle {
    /// Spawn the owner thread with a kernel built from `config`; with the
    /// `engine` feature it publishes every render into `publication`.
    pub(crate) fn start(
        config: &EngineConfig,
        #[cfg(feature = "engine")] publication: Arc<Publication>,
    ) -> Result<Self, EngineError> {
        let (commands, receiver) = mpsc::channel();
        let history = config.history();
        let profile = config.profile;
        #[cfg(feature = "engine")]
        let limits = config.limits;
        std::thread::Builder::new()
            .name("phux-engine-owner".to_owned())
            .spawn(move || {
                #[cfg(feature = "engine")]
                let adapter = GhosttyAdapter::new(limits);
                #[cfg(not(feature = "engine"))]
                let adapter = byte_adapter::ByteAdapter;
                let kernel = SessionKernel::with_history_config(adapter, profile, history);
                Owner::new(
                    kernel,
                    #[cfg(feature = "engine")]
                    publication,
                )
                .run(&receiver);
            })
            .map_err(EngineError::Spawn)?;
        Ok(Self { commands })
    }

    /// Apply one event and wait for its outcome.
    pub(crate) fn apply(&self, event: EngineEvent) -> Result<EngineOutcome, EngineError> {
        self.request(|reply| Command::Apply(event, reply))
    }

    /// Forget a terminal and its projection.
    pub(crate) fn detach(&self, terminal_id: ResourceId) {
        let _ = self
            .commands
            .send(Command::Lifecycle(Lifecycle::Detach(terminal_id)));
    }

    /// Whether the terminal has a live (or explicitly retained) replica.
    #[must_use]
    pub fn has_projection(&self, terminal_id: &ResourceId) -> bool {
        self.request(|reply| Command::Query(Query::HasProjection(terminal_id.clone(), reply)))
            .unwrap_or(false)
    }

    /// Whether the kernel has permanently closed the terminal.
    #[must_use]
    pub fn is_closed(&self, terminal_id: &ResourceId) -> bool {
        self.request(|reply| Command::Query(Query::IsClosed(terminal_id.clone(), reply)))
            .unwrap_or(false)
    }

    /// Whether the kernel would accept input for the terminal right now.
    #[must_use]
    pub fn input_ready(&self, terminal_id: &ResourceId) -> bool {
        self.request(|reply| Command::Query(Query::InputReady(terminal_id.clone(), reply)))
            .unwrap_or(false)
    }

    /// Keep the final replica of a terminal after it closes, so a consumer
    /// can still scroll and read it until [`Self::release`].
    #[cfg(feature = "engine")]
    pub fn set_retain_on_close(&self, terminal_id: &ResourceId, retain: bool) {
        let _ = self.commands.send(Command::Lifecycle(Lifecycle::Retain(
            terminal_id.clone(),
            retain,
        )));
    }

    /// Release a retained closed replica and its projection.
    #[cfg(feature = "engine")]
    pub fn release(&self, terminal_id: &ResourceId) {
        let _ = self
            .commands
            .send(Command::Lifecycle(Lifecycle::Release(terminal_id.clone())));
    }

    /// Scroll the terminal's viewport and publish the result before
    /// returning.
    #[cfg(feature = "engine")]
    pub fn scroll(&self, terminal_id: &ResourceId, scroll: Scroll) -> Result<(), EngineError> {
        self.request(|reply| Command::Query(Query::Scroll(terminal_id.clone(), scroll, reply)))?
    }

    /// Whether the terminal's alternate screen is active.
    #[cfg(feature = "engine")]
    #[must_use]
    pub fn is_alt_screen(&self, terminal_id: &ResourceId) -> bool {
        self.request(|reply| Command::Query(Query::IsAltScreen(terminal_id.clone(), reply)))
            .unwrap_or(false)
    }

    /// Re-project and publish the terminal now, whether or not the kernel
    /// reported damage. Returns whether a frame was published.
    #[cfg(feature = "engine")]
    pub fn republish(&self, terminal_id: &ResourceId) -> Result<bool, EngineError> {
        self.request(|reply| Command::Query(Query::Republish(terminal_id.clone(), reply)))?
    }

    /// Drain the bytes the headless replica retained since the last take.
    #[cfg(not(feature = "engine"))]
    #[must_use]
    pub fn take_output(&self, terminal_id: &ResourceId) -> Vec<u8> {
        self.request(|reply| Command::Query(Query::TakeOutput(terminal_id.clone(), reply)))
            .unwrap_or_default()
    }

    fn request<T>(&self, command: impl FnOnce(Sender<T>) -> Command) -> Result<T, EngineError> {
        let (reply, response) = mpsc::channel();
        self.commands
            .send(command(reply))
            .map_err(|_| EngineError::Stopped)?;
        response.recv().map_err(|_| EngineError::Stopped)
    }
}

#[cfg(feature = "engine")]
struct ProjectorSlot {
    token: u128,
    projector: GridProjector,
    /// The buffer recycled from the frame the last publish replaced.
    spare: Option<GridBuffer>,
}

struct Owner {
    kernel: SessionKernel<Adapter>,
    effects: EffectBuffer,
    /// Terminals with a published, non-removed projection.
    visible: HashSet<ResourceId>,
    #[cfg(feature = "engine")]
    publication: Arc<Publication>,
    #[cfg(feature = "engine")]
    projectors: HashMap<ResourceId, ProjectorSlot>,
    #[cfg(feature = "engine")]
    closed: HashMap<ResourceId, ClosedReplica<GhosttyAdapter>>,
    #[cfg(feature = "engine")]
    pending_releases: HashSet<ResourceId>,
}

impl Owner {
    fn new(
        kernel: SessionKernel<Adapter>,
        #[cfg(feature = "engine")] publication: Arc<Publication>,
    ) -> Self {
        Self {
            kernel,
            effects: EffectBuffer::new(),
            visible: HashSet::new(),
            #[cfg(feature = "engine")]
            publication,
            #[cfg(feature = "engine")]
            projectors: HashMap::new(),
            #[cfg(feature = "engine")]
            closed: HashMap::new(),
            #[cfg(feature = "engine")]
            pending_releases: HashSet::new(),
        }
    }

    fn run(mut self, commands: &mpsc::Receiver<Command>) {
        while let Ok(command) = commands.recv() {
            match command {
                Command::Apply(event, reply) => {
                    let _ = reply.send(self.apply(event));
                }
                Command::Lifecycle(lifecycle) => self.lifecycle(lifecycle),
                Command::Query(query) => self.query(query),
            }
        }
    }

    fn lifecycle(&mut self, lifecycle: Lifecycle) {
        match lifecycle {
            Lifecycle::Detach(id) => {
                let _ = self.kernel.detach_terminal(&id);
                self.visible.remove(&id);
                #[cfg(feature = "engine")]
                {
                    self.closed.remove(&id);
                    self.pending_releases.remove(&id);
                    self.projectors.remove(&id);
                    self.publication.remove(&id);
                }
            }
            #[cfg(feature = "engine")]
            Lifecycle::Retain(id, retain) => {
                self.kernel.set_retain_replica_on_close(&id, retain);
            }
            #[cfg(feature = "engine")]
            Lifecycle::Release(id) => {
                self.kernel.set_retain_replica_on_close(&id, false);
                if self.closed.contains_key(&id) {
                    self.pending_releases.insert(id.clone());
                    self.release_closed(&id);
                }
                self.projectors.remove(&id);
                self.publication.remove(&id);
            }
        }
    }

    fn query(&mut self, query: Query) {
        match query {
            Query::HasProjection(id, reply) => {
                let _ = reply.send(self.has_replica(&id));
            }
            Query::IsClosed(id, reply) => {
                let _ = reply.send(self.is_closed(&id));
            }
            Query::InputReady(id, reply) => {
                let ready = matches!(
                    self.kernel.input_eligibility(&id),
                    InputEligibility::Eligible { .. }
                );
                let _ = reply.send(ready);
            }
            #[cfg(feature = "engine")]
            Query::Scroll(id, scroll, reply) => {
                let _ = reply.send(self.scroll(&id, scroll));
            }
            #[cfg(feature = "engine")]
            Query::IsAltScreen(id, reply) => {
                let alt = self
                    .replica(&id)
                    .and_then(GhosttyReplica::terminal)
                    .is_some_and(|terminal| {
                        matches!(
                            terminal.active_screen(),
                            Ok(libghostty_vt::screen::Screen::Alternate)
                        )
                    });
                let _ = reply.send(alt);
            }
            #[cfg(feature = "engine")]
            Query::Republish(id, reply) => {
                let _ = reply.send(self.render_and_publish(&id));
            }
            #[cfg(not(feature = "engine"))]
            Query::TakeOutput(id, reply) => {
                let bytes = if self.visible.contains(&id) {
                    self.kernel
                        .published_engine_mut(&id)
                        .map(|replica| std::mem::take(&mut replica.bytes))
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };
                let _ = reply.send(bytes);
            }
        }
    }

    fn is_closed(&self, id: &ResourceId) -> bool {
        matches!(
            self.kernel.input_eligibility(id),
            InputEligibility::Ineligible(InputBlockReason::Closed)
        )
    }

    fn apply(&mut self, event: EngineEvent) -> EngineOutcome {
        if let EngineEvent::Closed { terminal_id, .. } = &event
            && self.is_closed(terminal_id)
        {
            return EngineOutcome::default();
        }
        if let EngineEvent::AttachStarted { terminals, .. } = &event {
            self.prepare_attach(terminals);
        }
        #[cfg(feature = "engine")]
        let closing = match &event {
            EngineEvent::Closed { terminal_id, .. } => Some(terminal_id.clone()),
            _ => None,
        };
        let result = apply_event(&mut self.kernel, event, &mut self.effects);
        let effects = self.effects.take();
        // A retained close moves the final replica aside before the damage
        // walk sees the kernel's `Removed`, so the walk re-publishes it
        // from there instead of dropping its slot.
        #[cfg(feature = "engine")]
        if let Some(id) = closing {
            self.capture_closed(id);
        }
        let damaged = self.note_damage(&effects);
        #[cfg(feature = "engine")]
        {
            self.release_pending();
            for id in damaged {
                if let Err(error) = self.render_and_publish(&id) {
                    tracing::warn!(terminal = %id, %error, "grid projection failed");
                }
            }
        }
        #[cfg(not(feature = "engine"))]
        drop(damaged);
        EngineOutcome {
            effects,
            error: result.err().map(|error| error.to_string()),
        }
    }

    /// Record which terminals gained or lost a projection, and return the
    /// ones to re-project, in effect order without duplicates.
    fn note_damage(&mut self, effects: &[KernelEffect]) -> Vec<ResourceId> {
        let mut damaged = Vec::new();
        for effect in effects {
            let KernelEffect::Damage(damage) = effect else {
                continue;
            };
            let id = &damage.terminal_id;
            if damage.kind == KernelDamageKind::Removed {
                self.visible.remove(id);
                damaged.retain(|damaged| damaged != id);
                #[cfg(feature = "engine")]
                if self.closed.contains_key(id) {
                    // Retained: the final frame is projected from the
                    // closed replica.
                    damaged.push(id.clone());
                } else {
                    self.projectors.remove(id);
                    self.publication.remove(id);
                }
            } else {
                self.visible.insert(id.clone());
                if !damaged.contains(id) {
                    damaged.push(id.clone());
                }
            }
        }
        damaged
    }

    fn prepare_attach(&mut self, terminals: &[ResourceId]) {
        self.kernel.release_active_attach();
        for terminal_id in terminals {
            if !self.is_closed(terminal_id) {
                continue;
            }
            self.visible.remove(terminal_id);
            #[cfg(feature = "engine")]
            {
                self.closed.remove(terminal_id);
                self.pending_releases.remove(terminal_id);
            }
            let _ = self.kernel.release_terminal(terminal_id);
        }
    }

    #[cfg(feature = "engine")]
    fn has_replica(&self, id: &ResourceId) -> bool {
        self.replica(id).is_some()
    }

    #[cfg(not(feature = "engine"))]
    fn has_replica(&self, id: &ResourceId) -> bool {
        self.visible.contains(id) && self.kernel.published_engine(id).is_some()
    }

    #[cfg(feature = "engine")]
    fn capture_closed(&mut self, id: ResourceId) {
        if let Some(replica) = self.kernel.take_closed_replica(&id) {
            self.closed.insert(id, replica);
        } else {
            self.projectors.remove(&id);
            self.publication.remove(&id);
        }
    }

    #[cfg(feature = "engine")]
    fn release_closed(&mut self, id: &ResourceId) {
        if self.kernel.detach_terminal(id) {
            self.closed.remove(id);
            self.pending_releases.remove(id);
        }
    }

    #[cfg(feature = "engine")]
    fn release_pending(&mut self) {
        for id in self.pending_releases.clone() {
            self.release_closed(&id);
        }
    }

    #[cfg(feature = "engine")]
    fn replica(&self, id: &ResourceId) -> Option<&GhosttyReplica> {
        if self.pending_releases.contains(id) {
            return None;
        }
        if !self.visible.contains(id) {
            return self.closed.get(id).map(ClosedReplica::engine);
        }
        self.kernel
            .published_engine(id)
            .or_else(|| self.closed.get(id).map(ClosedReplica::engine))
    }

    /// The generation token, geometry, key, and last sequence of the replica
    /// a render of `id` reads.
    #[cfg(feature = "engine")]
    fn replica_identity(
        &self,
        id: &ResourceId,
    ) -> Option<(u128, CanonicalGeometry, u64, u64, u64)> {
        if self.pending_releases.contains(id) {
            return None;
        }
        let from_closed = || {
            self.closed.get(id).map(|replica| {
                let key = replica.key();
                (
                    key.generation_token(),
                    replica.geometry(),
                    key.stream_id.get(),
                    key.bootstrap_id.get(),
                    replica.last_seq(),
                )
            })
        };
        if !self.visible.contains(id) {
            return from_closed();
        }
        self.kernel
            .published(id)
            .map(|replica| {
                let key = replica.key();
                (
                    key.generation_token(),
                    replica.geometry(),
                    key.stream_id.get(),
                    key.bootstrap_id.get(),
                    replica.last_seq(),
                )
            })
            .or_else(from_closed)
    }

    /// Project `id`'s replica into the back buffer and publish it. `Ok(false)`
    /// means there is nothing renderable yet (a native replica before READY).
    #[cfg(feature = "engine")]
    fn render_and_publish(&mut self, id: &ResourceId) -> Result<bool, EngineError> {
        let Some((token, _geometry, stream_id, bootstrap_id, last_seq)) = self.replica_identity(id)
        else {
            return Ok(false);
        };
        let replace = self
            .projectors
            .get(id)
            .is_none_or(|slot| slot.token != token);
        if replace {
            let projector = GridProjector::new().map_err(|error| {
                EngineError::Engine(format!("render state allocation failed: {error}"))
            })?;
            self.projectors.insert(
                id.clone(),
                ProjectorSlot {
                    token,
                    projector,
                    spare: None,
                },
            );
        }
        let Some(mut slot) = self.projectors.remove(id) else {
            return Ok(false);
        };
        let Some(terminal) = self.replica(id).and_then(GhosttyReplica::terminal) else {
            self.projectors.insert(id.clone(), slot);
            return Ok(false);
        };
        let projected = slot
            .projector
            .project(terminal)
            .map(|snapshot| {
                (
                    snapshot.cols,
                    snapshot.rows,
                    snapshot.cursor,
                    frame_colors(&snapshot.colors),
                    snapshot.damage,
                )
            })
            .map_err(|error| EngineError::Engine(error.to_string()));
        let scrollbar = terminal.scrollbar().map(|bar| Scrollbar {
            total: bar.total,
            offset: bar.offset,
            len: bar.len,
        });
        let outcome = projected.and_then(|(cols, rows, cursor, colors, damage)| {
            let scrollbar =
                scrollbar.map_err(|error| EngineError::Engine(format!("scrollbar: {error}")))?;
            let mut buffer = slot.spare.take().unwrap_or_default();
            slot.projector.swap_buffer(&mut buffer);
            let frame = GridFrame {
                terminal_id: id.clone(),
                generation: 0,
                stream_id,
                bootstrap_id,
                last_seq,
                cols,
                rows,
                cursor,
                scrollbar,
                colors,
                damage,
                buffer,
            };
            if let Some(previous) = self.publication.publish(id, frame)
                && let Ok(previous) = Arc::try_unwrap(previous)
            {
                slot.spare = Some(previous.buffer);
            }
            Ok(true)
        });
        self.projectors.insert(id.clone(), slot);
        outcome
    }

    #[cfg(feature = "engine")]
    fn scroll(&mut self, id: &ResourceId, scroll: Scroll) -> Result<(), EngineError> {
        let viewport = match scroll {
            Scroll::Top => ScrollViewport::Top,
            Scroll::Bottom => ScrollViewport::Bottom,
            Scroll::Delta(delta) => ScrollViewport::Delta(
                isize::try_from(delta)
                    .map_err(|_| EngineError::Engine("scroll delta exceeds isize".to_owned()))?,
            ),
            Scroll::Row(row) => ScrollViewport::Row(
                usize::try_from(row)
                    .map_err(|_| EngineError::Engine("scroll row exceeds usize".to_owned()))?,
            ),
        };
        let scrolled = if let Some(replica) = self.kernel.published_engine_mut(id) {
            replica.scroll_viewport(viewport)
        } else if let Some(replica) = self.closed.get_mut(id) {
            replica.engine_mut().scroll_viewport(viewport)
        } else {
            return Err(EngineError::Engine(
                "terminal has no scrollable replica".to_owned(),
            ));
        };
        scrolled.map_err(|error| EngineError::Engine(error.to_string()))?;
        self.render_and_publish(id).map(|_| ())
    }
}

#[cfg(feature = "engine")]
fn frame_colors(colors: &libghostty_vt::render::Colors) -> FrameColors {
    let rgb = |color: libghostty_vt::style::RgbColor| Rgb {
        r: color.r,
        g: color.g,
        b: color.b,
    };
    FrameColors {
        background: rgb(colors.background),
        foreground: rgb(colors.foreground),
        cursor: colors.cursor.map(rgb),
        palette: Box::new(colors.palette.map(rgb)),
    }
}

type KernelResult =
    Result<(), phux_client_core::session::KernelError<<Adapter as EngineAdapter>::Error>>;

fn apply_event(
    kernel: &mut SessionKernel<Adapter>,
    event: EngineEvent,
    effects: &mut EffectBuffer,
) -> KernelResult {
    match event {
        EngineEvent::AttachStarted {
            attach_id,
            terminals,
        } => kernel.update(
            KernelInput::AttachStarted {
                attach_id,
                terminals: &terminals,
            },
            effects,
        ),
        EngineEvent::AttachReady { attach_id } => {
            kernel.update(KernelInput::AttachReady { attach_id }, effects)
        }
        EngineEvent::BootstrapBegin {
            terminal_id,
            stream_id,
            bootstrap_id,
            profile,
            cols,
            rows,
            base_seq,
        } => kernel.update(
            KernelInput::BootstrapBegin {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                profile,
                geometry: CanonicalGeometry { cols, rows },
                base_seq,
            },
            effects,
        ),
        EngineEvent::BootstrapChunk {
            terminal_id,
            stream_id,
            bootstrap_id,
            chunk_seq,
            payload,
        } => kernel.update(
            KernelInput::BootstrapChunk {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                chunk_seq,
                payload: &payload,
            },
            effects,
        ),
        EngineEvent::BootstrapReady {
            terminal_id,
            stream_id,
            bootstrap_id,
            history_cursor,
        } => kernel.update(
            KernelInput::BootstrapReady {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                history_cursor: history_cursor.as_deref(),
            },
            effects,
        ),
        event => apply_stream_event(kernel, event, effects),
    }
}

fn apply_stream_event(
    kernel: &mut SessionKernel<Adapter>,
    event: EngineEvent,
    effects: &mut EffectBuffer,
) -> KernelResult {
    match event {
        EngineEvent::HistoryPage {
            terminal_id,
            stream_id,
            bootstrap_id,
            page_seq,
            rows,
            cursor,
            next_cursor,
            payload,
        } => kernel.update(
            KernelInput::HistoryPage {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                page_seq,
                rows,
                payload: &payload,
                cursor: &cursor,
                next_cursor: next_cursor.as_deref(),
            },
            effects,
        ),
        EngineEvent::HistoryTombstone {
            terminal_id,
            stream_id,
            bootstrap_id,
            cursor,
            reason,
        } => kernel.update(
            KernelInput::HistoryTombstone {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                cursor: &cursor,
                reason,
            },
            effects,
        ),
        EngineEvent::HistoryRejected {
            terminal_id,
            stream_id,
            bootstrap_id,
            cursor,
            reason,
            required_bytes,
            required_rows,
        } => kernel.update(
            KernelInput::HistoryRejected {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                cursor: &cursor,
                reason,
                required_bytes,
                required_rows,
            },
            effects,
        ),
        EngineEvent::Output {
            terminal_id,
            stream_id,
            bootstrap_id,
            seq,
            bytes,
        } => kernel.update(
            KernelInput::ResourceOutput {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                seq,
                payload: &bytes,
            },
            effects,
        ),
        event => apply_lifecycle_event(kernel, event, effects),
    }
}

fn apply_lifecycle_event(
    kernel: &mut SessionKernel<Adapter>,
    event: EngineEvent,
    effects: &mut EffectBuffer,
) -> KernelResult {
    match event {
        EngineEvent::Tombstone {
            terminal_id,
            stream_id,
            bootstrap_id,
            reason,
            last_valid_seq,
        } => kernel.update(
            KernelInput::Tombstone {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                reason,
                last_valid_seq,
            },
            effects,
        ),
        EngineEvent::Closed {
            terminal_id,
            exit_status,
            signal,
            reason,
        } => kernel.update(
            KernelInput::ResourceClosed {
                terminal_id: &terminal_id,
                exit_status,
                signal,
                reason,
            },
            effects,
        ),
        EngineEvent::Agent { terminal_id, event } => kernel.update(
            KernelInput::Event {
                terminal_id: &terminal_id,
                event: &event,
            },
            effects,
        ),
        EngineEvent::AgentSessionDeclared {
            terminal_id,
            parent,
            provider,
            native_id,
            state,
        } => kernel.update(
            KernelInput::AgentSessionDeclared(AgentSessionDeclaration {
                terminal_id: &terminal_id,
                parent: parent.as_ref(),
                provider: provider.as_deref(),
                native_id: native_id.as_deref(),
                state: state.as_deref(),
            }),
            effects,
        ),
        EngineEvent::AttachStarted { .. }
        | EngineEvent::AttachReady { .. }
        | EngineEvent::BootstrapBegin { .. }
        | EngineEvent::BootstrapChunk { .. }
        | EngineEvent::BootstrapReady { .. }
        | EngineEvent::HistoryPage { .. }
        | EngineEvent::HistoryTombstone { .. }
        | EngineEvent::HistoryRejected { .. }
        | EngineEvent::Output { .. } => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "engine")]
    use crate::publication::GridDamage;

    fn id(value: u32) -> ResourceId {
        ResourceId::local(value)
    }

    fn stream(value: u64) -> StreamId {
        StreamId::new(value).expect("nonzero")
    }

    fn bootstrap(value: u64) -> BootstrapId {
        BootstrapId::new(value).expect("nonzero")
    }

    fn config() -> EngineConfig {
        EngineConfig {
            profile: BootstrapProfile::SynthesizedVtRaw,
            limits: BootstrapLimits::default(),
            scrollback_lines: 100,
        }
    }

    #[cfg(feature = "engine")]
    fn owner() -> (EngineHandle, Arc<Publication>) {
        let publication = Arc::new(Publication::new());
        let handle = EngineHandle::start(&config(), Arc::clone(&publication)).expect("owner");
        (handle, publication)
    }

    #[cfg(not(feature = "engine"))]
    fn owner() -> EngineHandle {
        EngineHandle::start(&config()).expect("owner")
    }

    fn apply_ok(owner: &EngineHandle, event: EngineEvent) -> EngineOutcome {
        let outcome = owner.apply(event).expect("owner response");
        assert_eq!(outcome.error, None);
        assert!(!outcome.resync_required());
        outcome
    }

    fn attach(owner: &EngineHandle, terminal_id: &ResourceId, bytes: &[u8]) {
        apply_ok(
            owner,
            EngineEvent::AttachStarted {
                attach_id: 7,
                terminals: vec![terminal_id.clone()],
            },
        );
        apply_ok(
            owner,
            EngineEvent::BootstrapBegin {
                terminal_id: terminal_id.clone(),
                stream_id: stream(1),
                bootstrap_id: bootstrap(1),
                profile: BootstrapStreamProfile::SynthesizedVtRaw,
                cols: 20,
                rows: 4,
                base_seq: 0,
            },
        );
        apply_ok(
            owner,
            EngineEvent::BootstrapChunk {
                terminal_id: terminal_id.clone(),
                stream_id: stream(1),
                bootstrap_id: bootstrap(1),
                chunk_seq: 0,
                payload: bytes.to_vec(),
            },
        );
        apply_ok(
            owner,
            EngineEvent::BootstrapReady {
                terminal_id: terminal_id.clone(),
                stream_id: stream(1),
                bootstrap_id: bootstrap(1),
                history_cursor: None,
            },
        );
        apply_ok(owner, EngineEvent::AttachReady { attach_id: 7 });
    }

    #[cfg(feature = "engine")]
    #[test]
    fn a_published_replica_is_projected_and_generations_advance_on_output() {
        let (owner, publication) = owner();
        let terminal = id(1);
        assert!(!owner.has_projection(&terminal));
        attach(&owner, &terminal, b"ready");
        assert!(owner.has_projection(&terminal));
        assert!(owner.input_ready(&terminal));
        let first = publication.acquire(&terminal).expect("published");
        assert_eq!(first.generation, 1);
        assert_eq!(first.text(), "ready");
        assert_eq!(first.damage, GridDamage::Full);
        assert_eq!(first.dirty_rows().count(), 4, "a first frame is all dirty");

        // Nothing applied: the generation does not move.
        assert_eq!(publication.generation(&terminal), Some(1));

        apply_ok(
            &owner,
            EngineEvent::Output {
                terminal_id: terminal.clone(),
                stream_id: stream(1),
                bootstrap_id: bootstrap(1),
                seq: 1,
                bytes: b"\x1b[3;1Hthird".to_vec(),
            },
        );
        let second = publication.acquire(&terminal).expect("published");
        assert_eq!(second.generation, 2);
        assert_eq!(second.row_text(2), "third");
        assert!(second.is_row_dirty(2));
        assert_eq!(first.text(), "ready", "the held frame is untouched");

        // A scroll re-publishes even when the grid did not change.
        owner.scroll(&terminal, Scroll::Bottom).expect("scroll");
        assert_eq!(publication.generation(&terminal), Some(3));
    }

    #[cfg(feature = "engine")]
    #[test]
    fn a_closed_terminal_loses_its_projection_unless_retained() {
        let (owner, publication) = owner();
        let terminal = id(2);
        attach(&owner, &terminal, b"final");
        owner.set_retain_on_close(&terminal, true);
        apply_ok(&owner, EngineEvent::closed_unknown(terminal.clone()));
        assert!(owner.is_closed(&terminal));
        assert!(owner.has_projection(&terminal), "retained after close");
        assert!(publication.acquire(&terminal).is_some());
        owner.release(&terminal);
        assert!(!owner.has_projection(&terminal));
        assert!(publication.acquire(&terminal).is_none());
    }

    #[cfg(not(feature = "engine"))]
    #[test]
    fn the_headless_replica_keeps_bytes_until_taken() {
        let owner = owner();
        let terminal = id(3);
        attach(&owner, &terminal, b"ready");
        assert!(owner.has_projection(&terminal));
        assert_eq!(owner.take_output(&terminal), b"ready");
        apply_ok(
            &owner,
            EngineEvent::Output {
                terminal_id: terminal.clone(),
                stream_id: stream(1),
                bootstrap_id: bootstrap(1),
                seq: 1,
                bytes: b"more".to_vec(),
            },
        );
        assert_eq!(owner.take_output(&terminal), b"more");
        assert!(owner.take_output(&terminal).is_empty());
    }
}
