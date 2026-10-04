//! Protocol-0.7 web session over the shared synchronous client kernel.
//!
//! Transport framing stays here; replica lifecycle, generation validation,
//! ordering, READY fences, and input eligibility stay in `phux-client-core`.

use std::collections::HashMap;
use std::rc::Rc;

use crate::panes::{Axis, Layout, PaneRect};
use bytes::{Bytes, BytesMut};
use phux_client_core::engine::{
    BootstrapProgress, CanonicalGeometry, EngineAdapter, EngineDamage, EngineEffect,
    EngineEffectBuffer, EngineStatus, HistoryApplyOutcome,
};
use phux_client_core::history::HistoryCacheConfig;
use phux_client_core::session::{
    AgentSessionDeclaration, EffectBuffer, HistoryRejectionReason as KernelHistoryRejectionReason,
    HistoryUnavailableReason, InputEligibility, KernelAction, KernelEffect, KernelInput,
    KernelSend, KernelStatus, SessionKernel,
};
use phux_protocol::caps::{
    BootstrapCapabilities, BootstrapLimits, BootstrapProfile, BootstrapProfileKind,
    BootstrapProfileSet, BootstrapStreamProfile, ClientCapabilities, EngineCodec, EngineFeatureSet,
    ImageProtocolSet, ServerFeature, ServerFeatureExt,
};
use phux_protocol::ids::{GroupId, ResourceId};
use phux_protocol::input::InputEvent;
use phux_protocol::input::key::KeyEvent;
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::wire::frame::{
    AgentEvent, AttachTarget, Command, CommandResult, FrameKind, HistoryRejectionReason,
    HistoryTombstoneReason, PathQueryResult, PathResults, SpawnResult, ViewportInfo,
};
use phux_protocol::{PROTOCOL_VERSION, ResourceKind};
use phux_vt_web::{Grid, NativeCodecError, NativeDecodeKind, NativeDecoder, Terminal, Vt};

mod resources;

const ATTACH_ID: u32 = 1;
const HISTORY_LINES: u32 = 5_000;

fn history_unavailable_reason(reason: HistoryTombstoneReason) -> Option<HistoryUnavailableReason> {
    Some(match reason {
        HistoryTombstoneReason::Stale => HistoryUnavailableReason::Stale,
        HistoryTombstoneReason::Pruned => HistoryUnavailableReason::Pruned,
        HistoryTombstoneReason::Reset => HistoryUnavailableReason::Reset,
        HistoryTombstoneReason::Resize => HistoryUnavailableReason::Resize,
        HistoryTombstoneReason::Expired => HistoryUnavailableReason::Expired,
        HistoryTombstoneReason::Released => HistoryUnavailableReason::Released,
        HistoryTombstoneReason::Limit => HistoryUnavailableReason::Limit,
        HistoryTombstoneReason::CodecFailure => HistoryUnavailableReason::CodecFailure,
        _ => return None,
    })
}

fn history_rejection_reason(
    reason: HistoryRejectionReason,
) -> Option<KernelHistoryRejectionReason> {
    Some(match reason {
        HistoryRejectionReason::ZeroLimit => KernelHistoryRejectionReason::ZeroLimit,
        HistoryRejectionReason::TooSmall => KernelHistoryRejectionReason::TooSmall,
        HistoryRejectionReason::Busy => KernelHistoryRejectionReason::Busy,
        _ => return None,
    })
}
/// The capability set phux-web advertises in `HELLO`.
///
/// Synthesized compatibility remains unconditional. Native checkpoint v2 is
/// added only after the loaded WASM module reports the canonical immutable
/// codec identity, version, required features, and sufficient record bounds.
#[must_use]
pub fn client_caps(vt: &Vt) -> ClientCapabilities {
    let mut bootstrap = synthesized_bootstrap_caps();
    let required_record_bytes = bootstrap
        .limits
        .max_chunk_bytes()
        .max(bootstrap.limits.max_history_page_bytes()) as usize;
    if vt
        .incremental_capabilities()
        .is_some_and(|capabilities| capabilities.supports_protocol_07(required_record_bytes))
    {
        bootstrap = bootstrap.with_native(
            EngineCodec::LibghosttyCheckpointV2,
            EngineFeatureSet::required_native(),
        );
    }
    ClientCapabilities::new()
        .with_image_protocols(ImageProtocolSet::new())
        .with_bootstrap(bootstrap)
}

fn synthesized_bootstrap_caps() -> BootstrapCapabilities {
    BootstrapCapabilities::new().with_profiles(BootstrapProfileSet::with(&[
        BootstrapProfileKind::SynthesizedVtRaw,
        BootstrapProfileKind::SynthesizedVtStateSync,
    ]))
}

fn synthesized_client_caps() -> ClientCapabilities {
    ClientCapabilities::new()
        .with_image_protocols(ImageProtocolSet::new())
        .with_bootstrap(synthesized_bootstrap_caps())
}

fn validate_hello_ok(
    offered: ClientCapabilities,
    protocol_major: u16,
    protocol_minor: u16,
    protocol_patch: u16,
    selected_profile: BootstrapProfile,
    bootstrap_limits: BootstrapLimits,
) -> Result<(), &'static str> {
    if (protocol_major, protocol_minor, protocol_patch)
        != (
            PROTOCOL_VERSION.major,
            PROTOCOL_VERSION.minor,
            PROTOCOL_VERSION.patch,
        )
    {
        return Err("HELLO_OK selected a different protocol version");
    }
    let profile_offered = match selected_profile {
        BootstrapProfile::NativeState { codec, features } => {
            offered
                .bootstrap
                .profiles
                .contains(BootstrapProfileKind::NativeState)
                && offered.bootstrap.native_codecs.contains(codec)
                && features.supports_native()
                && offered.bootstrap.native_features.intersect(features) == features
        }
        BootstrapProfile::SynthesizedVtRaw => offered
            .bootstrap
            .profiles
            .contains(BootstrapProfileKind::SynthesizedVtRaw),
        BootstrapProfile::SynthesizedVtStateSync => offered
            .bootstrap
            .profiles
            .contains(BootstrapProfileKind::SynthesizedVtStateSync),
        _ => false,
    };
    if !profile_offered {
        return Err("HELLO_OK selected an unadvertised bootstrap profile");
    }
    if bootstrap_limits.intersect(offered.bootstrap.limits) != bootstrap_limits {
        return Err("HELLO_OK selected bootstrap limits above the client offer");
    }
    Ok(())
}

/// One `AgentSession` resource projected for the pane on screen: the badge
/// the DOM shows beside the canvas. Provider and state come from the kernel,
/// which folds the attach snapshot's facet and every stream record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentBadge {
    /// The resource id the server assigned to the agent session.
    pub id: ResourceId,
    /// The terminal the session is bound to, when the server reported one.
    pub parent: Option<ResourceId>,
    /// Provider name, for example `claude`; empty until something names one.
    pub provider: String,
    /// Derived lifecycle state word (`working`, `blocked`, `done`, `idle`,
    /// `ended`, or `unknown`).
    pub state: String,
}

/// The result of handling one incoming frame.
#[derive(Default)]
pub struct Outcome {
    /// Encoded frames for the browser transport to send.
    pub send: Vec<Vec<u8>>,
    /// Whether a published replica changed and should be repainted.
    pub render: bool,
    /// Whether the set of agent badges changed and should be repainted.
    pub badges: bool,
    /// Whether the focused terminal's program rang the bell (BEL).
    pub bell: bool,
    /// Pane inventory, focus, pending operation, or an actionable refusal changed.
    pub panes: bool,
    /// Fatal protocol/kernel failure; the transport must close.
    pub fatal: Option<String>,
}

struct WebEngine {
    vt: Rc<Vt>,
    limits: BootstrapLimits,
}

struct WebReplica {
    state: WebReplicaState,
    history_budget: Option<(usize, usize)>,
}

enum WebReplicaState {
    Synthesized(Terminal),
    Native {
        decoder: NativeDecoder,
        terminal: Option<Terminal>,
        protocol_finished: bool,
        decoder_finished: bool,
    },
}

impl WebReplica {
    fn terminal(&self) -> Option<&Terminal> {
        match &self.state {
            WebReplicaState::Synthesized(terminal) => Some(terminal),
            WebReplicaState::Native { terminal, .. } => terminal.as_ref(),
        }
    }

    fn apply_history_budget(&self) -> Result<(), WebEngineError> {
        if let (Some(terminal), Some((max_bytes, max_rows))) =
            (self.terminal(), self.history_budget)
        {
            terminal.set_history_budget(max_bytes, max_rows)?;
        }
        Ok(())
    }
}

#[derive(Debug)]
enum WebEngineError {
    UnsupportedProfile(BootstrapStreamProfile),
    Native(NativeCodecError),
    PayloadLimitExceeded { actual: usize, limit: usize },
    InvalidNativeTransition(&'static str),
    InvalidProgress { consumed: usize, available: usize },
    TrailingAfterReady(usize),
    TrailingAfterFinish(usize),
}

impl From<NativeCodecError> for WebEngineError {
    fn from(error: NativeCodecError) -> Self {
        Self::Native(error)
    }
}

impl std::fmt::Display for WebEngineError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedProfile(profile) => {
                write!(formatter, "unsupported web bootstrap profile: {profile:?}")
            }
            Self::Native(error) => write!(formatter, "{error}"),
            Self::PayloadLimitExceeded { actual, limit } => {
                write!(
                    formatter,
                    "engine payload is {actual} bytes; limit is {limit}"
                )
            }
            Self::InvalidNativeTransition(message) => formatter.write_str(message),
            Self::InvalidProgress {
                consumed,
                available,
            } => write!(
                formatter,
                "invalid checkpoint progress: consumed {consumed} of {available}"
            ),
            Self::TrailingAfterReady(bytes) => {
                write!(formatter, "{bytes} trailing bootstrap bytes after READY")
            }
            Self::TrailingAfterFinish(bytes) => {
                write!(formatter, "{bytes} trailing history bytes after FINISH")
            }
        }
    }
}

impl std::error::Error for WebEngineError {}

impl EngineAdapter for WebEngine {
    type Replica = WebReplica;
    type Error = WebEngineError;

    fn start_replica(
        &mut self,
        profile: BootstrapStreamProfile,
        geometry: CanonicalGeometry,
    ) -> Result<Self::Replica, Self::Error> {
        let state = match profile {
            BootstrapStreamProfile::SynthesizedVtRaw
            | BootstrapStreamProfile::SynthesizedVtStateSync => {
                WebReplicaState::Synthesized(self.vt.terminal(geometry.cols, geometry.rows))
            }
            BootstrapStreamProfile::NativeState {
                codec: EngineCodec::LibghosttyCheckpointV2,
            } => {
                let max_record_bytes =
                    self.limits
                        .max_chunk_bytes()
                        .max(self.limits.max_history_page_bytes()) as usize;
                let capabilities = self
                    .vt
                    .incremental_capabilities()
                    .filter(|capabilities| capabilities.supports_protocol_07(max_record_bytes))
                    .ok_or(WebEngineError::UnsupportedProfile(profile))?;
                WebReplicaState::Native {
                    decoder: self.vt.native_decoder(
                        max_record_bytes,
                        max_record_bytes,
                        capabilities.max_pages,
                    )?,
                    terminal: None,
                    protocol_finished: false,
                    decoder_finished: false,
                }
            }
            _ => return Err(WebEngineError::UnsupportedProfile(profile)),
        };
        Ok(WebReplica {
            state,
            history_budget: None,
        })
    }

    fn configure_history_budget(
        &mut self,
        replica: &mut Self::Replica,
        max_bytes: usize,
        max_rows: usize,
    ) -> Result<(), Self::Error> {
        replica.history_budget = Some((max_bytes.max(1), max_rows.max(1)));
        replica.apply_history_budget()
    }

    fn apply_bootstrap_chunk(
        &mut self,
        replica: &mut Self::Replica,
        payload: &[u8],
        effects: &mut EngineEffectBuffer,
    ) -> Result<BootstrapProgress, Self::Error> {
        let limit = self.limits.max_chunk_bytes() as usize;
        if payload.len() > limit {
            return Err(WebEngineError::PayloadLimitExceeded {
                actual: payload.len(),
                limit,
            });
        }
        match &mut replica.state {
            WebReplicaState::Synthesized(terminal) => {
                terminal.write(payload);
                // Replayed state is not the program ringing now.
                let _ = terminal.take_bell();
                effects.push(EngineEffect::Damage(EngineDamage::Full));
                Ok(BootstrapProgress::Pending)
            }
            WebReplicaState::Native {
                decoder,
                terminal,
                protocol_finished,
                decoder_finished,
            } => {
                if *protocol_finished || *decoder_finished {
                    return Err(WebEngineError::InvalidNativeTransition(
                        "bootstrap input arrived after READY",
                    ));
                }
                let mut remaining = payload;
                loop {
                    if remaining.is_empty() {
                        return Ok(BootstrapProgress::Pending);
                    }
                    let event = decoder.push(remaining)?;
                    if event.consumed == 0 || event.consumed > remaining.len() {
                        return Err(WebEngineError::InvalidProgress {
                            consumed: event.consumed,
                            available: remaining.len(),
                        });
                    }
                    remaining = &remaining[event.consumed..];
                    match event.kind {
                        NativeDecodeKind::NeedInput | NativeDecodeKind::Progress => {}
                        NativeDecodeKind::Ready => {
                            *terminal = event.terminal;
                            if terminal.is_none() {
                                return Err(WebEngineError::InvalidNativeTransition(
                                    "READY did not transfer a terminal",
                                ));
                            }
                            if let (Some(terminal), Some((max_bytes, max_rows))) =
                                (terminal.as_ref(), replica.history_budget)
                            {
                                terminal.set_history_budget(max_bytes, max_rows)?;
                            }
                            if !remaining.is_empty() {
                                return Err(WebEngineError::TrailingAfterReady(remaining.len()));
                            }
                            return Ok(BootstrapProgress::Ready);
                        }
                        NativeDecodeKind::HistoryBegin
                        | NativeDecodeKind::HistoryPage
                        | NativeDecodeKind::Finish => {
                            return Err(WebEngineError::InvalidNativeTransition(
                                "history transition arrived before protocol READY",
                            ));
                        }
                    }
                }
            }
        }
    }

    fn finish_bootstrap(
        &mut self,
        replica: &mut Self::Replica,
        effects: &mut EngineEffectBuffer,
    ) -> Result<BootstrapProgress, Self::Error> {
        match &mut replica.state {
            WebReplicaState::Synthesized(_) => {
                effects.push(EngineEffect::Damage(EngineDamage::Full));
            }
            WebReplicaState::Native {
                terminal,
                protocol_finished,
                ..
            } => {
                if terminal.is_none() || std::mem::replace(protocol_finished, true) {
                    return Err(WebEngineError::InvalidNativeTransition(
                        "protocol READY did not follow engine READY exactly once",
                    ));
                }
                effects.push(EngineEffect::Damage(EngineDamage::Full));
            }
        }
        Ok(BootstrapProgress::Finished)
    }

    fn apply_history_page(
        &mut self,
        replica: &mut Self::Replica,
        payload: &[u8],
        declared_rows: u32,
        _effects: &mut EngineEffectBuffer,
    ) -> Result<HistoryApplyOutcome, Self::Error> {
        let limit = self.limits.max_history_page_bytes() as usize;
        if payload.len() > limit {
            return Err(WebEngineError::PayloadLimitExceeded {
                actual: payload.len(),
                limit,
            });
        }
        let WebReplicaState::Native {
            decoder,
            protocol_finished,
            decoder_finished,
            ..
        } = &mut replica.state
        else {
            return Ok(HistoryApplyOutcome {
                progress: BootstrapProgress::Finished,
                retained_rows: declared_rows as usize,
                authenticated_rows: declared_rows as usize,
            });
        };
        if !*protocol_finished {
            return Err(WebEngineError::InvalidNativeTransition(
                "history arrived before protocol READY",
            ));
        }
        if *decoder_finished {
            return Err(WebEngineError::InvalidNativeTransition(
                "history arrived after FINISH",
            ));
        }
        let mut retained = true;
        let mut remaining = payload;
        loop {
            if remaining.is_empty() {
                return Ok(HistoryApplyOutcome {
                    progress: BootstrapProgress::Ready,
                    retained_rows: if retained { declared_rows as usize } else { 0 },
                    authenticated_rows: declared_rows as usize,
                });
            }
            let event = decoder.push(remaining)?;
            if event.consumed == 0 || event.consumed > remaining.len() {
                return Err(WebEngineError::InvalidProgress {
                    consumed: event.consumed,
                    available: remaining.len(),
                });
            }
            remaining = &remaining[event.consumed..];
            match event.kind {
                NativeDecodeKind::NeedInput
                | NativeDecodeKind::Progress
                | NativeDecodeKind::HistoryBegin => {}
                NativeDecodeKind::HistoryPage => retained &= event.retained,
                NativeDecodeKind::Finish => {
                    *decoder_finished = true;
                    if !remaining.is_empty() {
                        return Err(WebEngineError::TrailingAfterFinish(remaining.len()));
                    }
                    return Ok(HistoryApplyOutcome {
                        progress: BootstrapProgress::Finished,
                        retained_rows: if retained { declared_rows as usize } else { 0 },
                        authenticated_rows: declared_rows as usize,
                    });
                }
                NativeDecodeKind::Ready => {
                    return Err(WebEngineError::InvalidNativeTransition(
                        "second READY arrived in history",
                    ));
                }
            }
        }
    }

    fn apply_output(
        &mut self,
        replica: &mut Self::Replica,
        payload: &[u8],
        effects: &mut EngineEffectBuffer,
    ) -> Result<(), Self::Error> {
        let terminal = replica
            .terminal()
            .ok_or(WebEngineError::InvalidNativeTransition(
                "live output arrived before READY",
            ))?;
        terminal.write(payload);
        effects.push(EngineEffect::Damage(EngineDamage::Full));
        if terminal.take_bell() {
            effects.push(EngineEffect::Status(EngineStatus::Bell));
        }
        Ok(())
    }
}

struct PendingSplit {
    request_id: u32,
    target: ResourceId,
    axis: Axis,
    resource: Option<ResourceId>,
    acknowledged: bool,
}

/// A wire session whose terminal replicas are owned by [`SessionKernel`].
pub struct Session {
    vt: Rc<Vt>,
    blank: Terminal,
    offered_caps: ClientCapabilities,
    kernel: Option<SessionKernel<WebEngine>>,
    effects: EffectBuffer,
    cols: u16,
    rows: u16,
    /// The pixel size of one cell as the client draws it, when known.
    cell_px: Option<(u16, u16)>,
    focused_terminal: Option<ResourceId>,
    terminal_order: Vec<ResourceId>,
    bootstrap_limits: Option<BootstrapLimits>,
    selected_profile: Option<BootstrapProfile>,
    terminal_reply_supported: bool,
    path_query_supported: bool,
    path_request_id: u32,
    path_pending: Option<u32>,
    path_target: Option<ResourceId>,
    path_results: Option<PathResults>,
    path_error: Option<String>,
    failed: bool,
    render_visible: bool,
    attach_ready: bool,
    layout: Option<Layout>,
    /// Last requested geometry, including resizes not yet published by the server.
    pane_sizes: HashMap<ResourceId, CanonicalGeometry>,
    /// Failed new resources remain isolated until their queued close arrives.
    retiring_panes: Vec<ResourceId>,
    pending_split: Option<PendingSplit>,
    pending_close: Option<(u32, ResourceId)>,
    /// Correlate optional child-stream attach refusals without failing its terminal.
    pending_agents: HashMap<u32, ResourceId>,
    pane_request_id: u32,
    pane_error: Option<String>,
    spawn_initial_size: bool,
}

impl Session {
    /// Open a session with a blank fallback grid of `cols`×`rows`.
    #[must_use]
    pub fn new(vt: &Rc<Vt>, cols: u16, rows: u16) -> Self {
        Self::with_caps(vt, cols, rows, client_caps(vt))
    }

    /// Open an explicit synthesized-only compatibility session.
    ///
    /// This is a fail-closed diagnostic path for drift/unavailability tests;
    /// normal browser connections use [`Self::new`] and prefer exact native v2.
    #[must_use]
    pub fn new_synthesized_compat(vt: &Rc<Vt>, cols: u16, rows: u16) -> Self {
        Self::with_caps(vt, cols, rows, synthesized_client_caps())
    }

    fn with_caps(vt: &Rc<Vt>, cols: u16, rows: u16, offered_caps: ClientCapabilities) -> Self {
        Self {
            vt: Rc::clone(vt),
            blank: vt.terminal(cols, rows),
            offered_caps,
            kernel: None,
            effects: EffectBuffer::new(),
            cols,
            rows,
            cell_px: None,
            focused_terminal: None,
            terminal_order: Vec::new(),
            bootstrap_limits: None,
            selected_profile: None,
            terminal_reply_supported: false,
            path_query_supported: false,
            path_request_id: 0,
            path_pending: None,
            path_target: None,
            path_results: None,
            path_error: None,
            failed: false,
            render_visible: false,
            attach_ready: false,
            layout: None,
            pane_sizes: HashMap::new(),
            retiring_panes: Vec::new(),
            pending_split: None,
            pending_close: None,
            pending_agents: HashMap::new(),
            pane_request_id: 0x1000,
            pane_error: None,
            spawn_initial_size: false,
        }
    }

    /// Overall canvas dimensions, independent of the focused terminal.
    #[must_use]
    pub const fn canvas_dims(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }

    /// Visible terminals and their local rectangles (at most four).
    #[must_use]
    pub fn pane_rects(&self) -> Vec<(ResourceId, PaneRect)> {
        let mut panes = Vec::with_capacity(4);
        if let Some(layout) = &self.layout {
            layout.rects(
                PaneRect {
                    x: 0,
                    y: 0,
                    cols: self.cols,
                    rows: self.rows,
                },
                &mut panes,
            );
        }
        panes
    }

    /// Current keyboard target, only after its replica has been published.
    #[must_use]
    pub fn focused_pane(&self) -> Option<ResourceId> {
        self.first_published_terminal()
    }

    /// Whether a server-authoritative split or close is outstanding.
    #[must_use]
    pub fn pane_pending(&self) -> bool {
        self.pending_split.is_some() || self.pending_close.is_some()
    }

    /// Last actionable pane refusal; does not fail the connection.
    #[must_use]
    pub fn pane_error(&self) -> Option<&str> {
        self.pane_error.as_deref()
    }

    pub(crate) fn set_pane_error(&mut self, message: String) {
        self.pane_error = Some(message);
    }

    /// Published engine for a resource.
    #[must_use]
    pub fn pane_terminal(&self, id: &ResourceId) -> Option<&Terminal> {
        self.kernel.as_ref()?.published_engine(id)?.terminal()
    }

    /// Focus one published, visible terminal.
    ///
    /// # Errors
    /// Refuses missing or bootstrapping terminals.
    pub fn focus_pane(&mut self, id: &ResourceId) -> Result<(), String> {
        if self.failed
            || !self.pane_rects().iter().any(|(pane, _)| pane == id)
            || self.pane_terminal(id).is_none()
        {
            return Err("That pane is not ready for input yet.".to_owned());
        }
        self.focused_terminal = Some(id.clone());
        self.cancel_path_query();
        self.pane_error = None;
        Ok(())
    }

    /// Cycle keyboard focus through visible, published terminals.
    ///
    /// # Errors
    /// Refuses when no terminal is ready.
    pub fn focus_next_pane(&mut self) -> Result<(), String> {
        let panes: Vec<_> = self
            .pane_rects()
            .into_iter()
            .map(|(id, _)| id)
            .filter(|id| self.pane_terminal(id).is_some())
            .collect();
        if panes.is_empty() {
            return Err("No terminal is ready for input.".to_owned());
        }
        let index = panes
            .iter()
            .position(|id| Some(id) == self.focused_terminal.as_ref())
            .map_or(0, |index| (index + 1) % panes.len());
        self.focus_pane(&panes[index])
    }

    fn next_pane_request(&mut self) -> u32 {
        self.pane_request_id = self.pane_request_id.wrapping_add(1).max(0x1000);
        self.pane_request_id
    }

    fn pane_operation_ready(&self) -> Result<(), String> {
        if self.failed || !self.attach_ready {
            return Err("Reconnect the session before changing panes.".to_owned());
        }
        if self.pane_pending() {
            return Err("Wait for the current pane operation to finish.".to_owned());
        }
        Ok(())
    }

    /// Request a real terminal spawn; commit layout only after bootstrap and ack.
    ///
    /// # Errors
    /// Refuses invalid axes, unavailable sessions, small panes, or a fifth pane.
    pub fn split_pane_frame(&mut self, axis: &str) -> Result<Vec<u8>, String> {
        let axis = Axis::parse(axis)?;
        self.pane_operation_ready()?;
        let panes = self.pane_rects();
        if panes.len() + self.retiring_panes.len() >= 4 {
            return Err("Four terminals are open or closing. Wait for cleanup or close a pane before splitting.".to_owned());
        }
        let target = self
            .focused_pane()
            .ok_or("No terminal is ready to split.")?;
        let rect = panes
            .iter()
            .find(|(id, _)| id == &target)
            .map(|(_, rect)| *rect)
            .ok_or("The focused terminal is not visible.")?;
        let (_, new_rect) = rect
            .split(axis)
            .ok_or("This pane is too small to split. Enlarge the terminal first.")?;
        let request_id = self.next_pane_request();
        self.pending_split = Some(PendingSplit {
            request_id,
            target: target.clone(),
            axis,
            resource: None,
            acknowledged: false,
        });
        self.pane_error = None;
        Ok(encode(&FrameKind::SpawnResource {
            request_id,
            group: GroupId::new(1),
            command: None,
            cwd: None,
            env: None,
            term: None,
            satellite: target.host().cloned(),
            owner_terminal: Some(target),
            agent_session: None,
            initial_size: self
                .spawn_initial_size
                .then_some((new_rect.cols, new_rect.rows)),
            resource: None,
        }))
    }

    /// Request closure; keep the pane visible until ResourceClosed.
    ///
    /// # Errors
    /// Refuses the last pane or another pending operation.
    pub fn close_pane_frame(&mut self) -> Result<Vec<u8>, String> {
        self.pane_operation_ready()?;
        if self.pane_rects().len() <= 1 {
            return Err(
                "Cannot close the last pane. Use Release Session to end the session.".to_owned(),
            );
        }
        let terminal_id = self
            .focused_pane()
            .ok_or("No terminal is ready to close.")?;
        let request_id = self.next_pane_request();
        self.pending_close = Some((request_id, terminal_id.clone()));
        self.pane_error = None;
        Ok(encode(&FrameKind::Command {
            request_id,
            command: Command::KillResource {
                terminal_id,
                operation_id: None,
            },
        }))
    }

    fn restore_layout(&mut self) {
        if let Some(focused) = &self.focused_terminal
            && let Some(index) = self.terminal_order.iter().position(|id| id == focused)
            && index >= 4
        {
            self.terminal_order.swap(0, index);
        }
        self.terminal_order.truncate(4);
        let mut ids = self.terminal_order.iter().take(4);
        self.layout = ids.next().cloned().map(Layout::Leaf);
        let mut previous = self.terminal_order.first().cloned();
        for id in ids {
            if let (Some(layout), Some(target)) = (&mut self.layout, &previous) {
                layout.insert(target, id.clone(), Axis::Vertical);
            }
            previous = Some(id.clone());
        }
    }

    fn pane_resize_frames(&mut self) -> Vec<Vec<u8>> {
        self.pane_rects()
            .into_iter()
            .filter_map(|(terminal_id, rect)| {
                let geometry = CanonicalGeometry {
                    cols: rect.cols,
                    rows: rect.rows,
                };
                let previous = self
                    .pane_sizes
                    .insert(terminal_id.clone(), geometry)
                    .or_else(|| {
                        self.kernel
                            .as_ref()
                            .and_then(|kernel| kernel.published(&terminal_id))
                            .map(|replica| replica.geometry())
                    });
                if previous == Some(geometry) {
                    return None;
                }
                Some(encode(&FrameKind::ResizeTerminal {
                    terminal_id,
                    cols: rect.cols,
                    rows: rect.rows,
                }))
            })
            .collect()
    }

    /// Resize every visible resource to its local pane rectangle.
    pub fn resize_panes(&mut self, cols: u16, rows: u16) -> Vec<Vec<u8>> {
        if self.layout.is_none() {
            return self.resize_frame(cols, rows).into_iter().collect();
        }
        let (cols, rows) = (cols.max(1), rows.max(1));
        if self.failed || (cols, rows) == (self.cols, self.rows) {
            return Vec::new();
        }
        self.cols = cols;
        self.rows = rows;
        self.pane_resize_frames()
    }

    fn pane_refusal(&mut self, message: String) -> Outcome {
        let resource = self
            .pending_split
            .take()
            .and_then(|pending| pending.resource);
        self.pending_close = None;
        self.pane_error = Some(message);
        let send = resource
            .into_iter()
            .map(|terminal_id| {
                self.retiring_panes.push(terminal_id.clone());
                let request_id = self.next_pane_request();
                encode(&FrameKind::Command {
                    request_id,
                    command: Command::KillResource {
                        terminal_id,
                        operation_id: None,
                    },
                })
            })
            .collect();
        Outcome {
            send,
            panes: true,
            ..Outcome::default()
        }
    }

    fn reduce_pane_frame(&mut self, frame: &FrameKind) -> Option<Outcome> {
        match frame {
            FrameKind::ResourceSpawned { request_id, result }
                if self
                    .pending_split
                    .as_ref()
                    .is_some_and(|pending| pending.request_id == *request_id) =>
            {
                Some(self.accept_spawn(result))
            }
            FrameKind::CommandResult { request_id, result } => {
                self.accept_pane_command(*request_id, result)
            }
            FrameKind::Error {
                request_id,
                message,
                ..
            } => {
                let correlated = request_id.is_some_and(|id| {
                    self.pending_split
                        .as_ref()
                        .is_some_and(|pending| pending.request_id == id)
                        || self
                            .pending_close
                            .as_ref()
                            .is_some_and(|(pending, _)| *pending == id)
                });
                if correlated {
                    Some(self.pane_refusal(message.clone()))
                } else {
                    self.pane_error = Some(message.clone());
                    Some(Outcome {
                        panes: true,
                        ..Outcome::default()
                    })
                }
            }
            _ => None,
        }
    }

    fn accept_spawn(&mut self, result: &SpawnResult) -> Outcome {
        let Some(terminal_id) = result.spawned_id().cloned() else {
            return self.pane_refusal(format!(
                "Could not split this pane: {result:?}. Try again or close another pane."
            ));
        };
        let request_id = self.next_pane_request();
        if let Some(pending) = self.pending_split.as_mut() {
            pending.resource = Some(terminal_id.clone());
            pending.request_id = request_id;
        }
        Outcome {
            send: vec![encode(&FrameKind::Command {
                request_id,
                command: Command::AttachResource {
                    terminal_id,
                    role_policy: None,
                },
            })],
            panes: true,
            ..Outcome::default()
        }
    }

    fn accept_pane_command(&mut self, request_id: u32, result: &CommandResult) -> Option<Outcome> {
        let split = self
            .pending_split
            .as_ref()
            .is_some_and(|pending| pending.request_id == request_id);
        let close = self
            .pending_close
            .as_ref()
            .is_some_and(|(id, _)| *id == request_id);
        if !split && !close {
            return None;
        }
        if let CommandResult::Error { message, .. } = result {
            return Some(self.pane_refusal(format!("Pane operation refused: {message}")));
        }
        if split && let Some(pending) = self.pending_split.as_mut() {
            pending.acknowledged = true;
        }
        Some(Outcome {
            panes: true,
            ..Outcome::default()
        })
    }

    fn finish_split(&mut self, outcome: &mut Outcome) {
        let Some(pending) = self.pending_split.as_ref() else {
            return;
        };
        let Some(id) = pending.resource.as_ref() else {
            return;
        };
        if !pending.acknowledged || self.pane_terminal(id).is_none() {
            return;
        }
        let id = id.clone();
        if let Some(layout) = self.layout.as_mut() {
            if !layout.insert(&pending.target, id.clone(), pending.axis)
                && let Some(target) = self.terminal_order.first()
            {
                // The requested source may naturally exit while spawn is in flight.
                layout.insert(target, id.clone(), pending.axis);
            }
        } else {
            self.layout = Some(Layout::Leaf(id.clone()));
        }
        self.terminal_order.push(id.clone());
        self.focused_terminal = Some(id);
        self.cancel_path_query();
        self.pending_split = None;
        outcome.panes = true;
        outcome.badges = true;
        outcome.render = true;
        outcome.send.extend(self.pane_resize_frames());
    }
    /// Negotiated decode limits after `HELLO_OK`.
    #[must_use]
    pub const fn bootstrap_limits(&self) -> Option<BootstrapLimits> {
        self.bootstrap_limits
    }

    /// Exact profile selected by a validated `HELLO_OK`, if negotiation finished.
    #[must_use]
    pub const fn selected_profile(&self) -> Option<BootstrapProfile> {
        self.selected_profile
    }

    /// Capabilities frozen into this session's outbound `HELLO`.
    #[must_use]
    pub const fn advertised_capabilities(&self) -> ClientCapabilities {
        self.offered_caps
    }

    /// Whether this session has entered its terminal protocol-failure state.
    #[must_use]
    pub const fn is_failed(&self) -> bool {
        self.failed
    }

    /// Whether the aggregate attach and every terminal bootstrap reached READY.
    #[must_use]
    pub const fn is_attach_ready(&self) -> bool {
        self.attach_ready
    }

    /// Whether this peer explicitly offers host-side path discovery.
    #[must_use]
    pub const fn path_query_supported(&self) -> bool {
        self.path_query_supported
    }

    /// Results for the latest query, if it completed for the current pane.
    #[must_use]
    pub fn path_results(&self) -> Option<&PathResults> {
        self.path_results.as_ref()
    }

    /// Display-only refusal for the latest query.
    #[must_use]
    pub fn path_error(&self) -> Option<&str> {
        self.path_error.as_deref()
    }

    /// True while awaiting a reply from the selected host.
    #[must_use]
    pub const fn path_pending(&self) -> bool {
        self.path_pending.is_some()
    }

    /// Cancel the UI's interest in the outstanding request. The server may still reply.
    pub fn cancel_path_query(&mut self) {
        self.path_pending = None;
        self.path_target = None;
        self.path_results = None;
        self.path_error = None;
    }

    /// Start one host query, replacing any prior query. No browser filesystem is read.
    #[must_use]
    pub fn path_query_frame(
        &mut self,
        root: &str,
        query: &str,
        recursive: bool,
    ) -> Option<Vec<u8>> {
        if !self.path_query_supported || !self.attach_ready || self.failed {
            return None;
        }
        let target = self.first_published_terminal()?;
        let host = target.host().cloned();
        self.path_request_id = self.path_request_id.wrapping_add(1).max(1);
        self.path_pending = Some(self.path_request_id);
        self.path_target = Some(target);
        self.path_results = None;
        self.path_error = None;
        Some(encode(&FrameKind::PathQuery {
            request_id: self.path_request_id,
            root: root.to_owned(),
            query: query.to_owned(),
            recursive,
            host,
        }))
    }

    /// Insert a selected, original server path as editable POSIX shell text.
    /// The kernel checks the pane's current input lease before emitting a paste.
    #[must_use]
    pub fn paste_path_row(&mut self, index: usize) -> Option<Vec<u8>> {
        let target = self.eligible_path_target()?;
        let path = &self.path_results.as_ref()?.rows.get(index)?.path;
        let text = shell_quote_path(path)?;
        let (outcome, applied) = self.apply_kernel(KernelInput::Action(KernelAction::Input {
            terminal_id: &target,
            event: &InputEvent::Paste(PasteEvent {
                trust: PasteTrust::Untrusted,
                data: text.into_bytes(),
            }),
        }));
        if !applied {
            return None;
        }
        let frame = outcome.send.into_iter().next();
        if frame.is_some() {
            self.cancel_path_query();
        }
        frame
    }

    fn eligible_path_target(&self) -> Option<ResourceId> {
        let target = self.path_target.as_ref()?;
        if self.path_pending.is_some() || self.first_published_terminal().as_ref() != Some(target) {
            return None;
        }
        let kernel = self.kernel.as_ref()?;
        matches!(
            kernel.input_eligibility(target),
            InputEligibility::Eligible { .. }
        )
        .then(|| target.clone())
    }

    /// Permanently fail this session after a transport or framing violation.
    pub fn fail_protocol(&mut self, _message: &str) {
        self.failed = true;
        self.cancel_path_query();
    }

    /// Frame sent when the transport opens. Stateful frames wait for `HELLO_OK`.
    #[must_use]
    pub fn handshake(&self) -> Vec<Vec<u8>> {
        vec![encode(&FrameKind::Hello {
            client_name: "phux-web".to_owned(),
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            client_caps: self.offered_caps,
        })]
    }

    /// Agent badges for the pane on screen: every `AgentSession` the kernel
    /// holds whose parent is that terminal, in resource-id order. Empty when
    /// the server reported none or every parent is another pane.
    #[must_use]
    pub fn agent_badges(&self) -> Vec<AgentBadge> {
        let Some(kernel) = self.kernel.as_ref() else {
            return Vec::new();
        };
        let focused = self
            .first_published_terminal()
            .or_else(|| self.focused_terminal.clone());
        let mut badges: Vec<AgentBadge> = kernel
            .agent_sessions()
            .filter(|view| view.parent.is_none() || view.parent == focused.as_ref())
            .map(|view| AgentBadge {
                id: view.terminal_id.clone(),
                parent: view.parent.cloned(),
                provider: view.state.provider.clone().unwrap_or_default(),
                state: view.state.status.as_str().to_owned(),
            })
            .collect();
        badges.sort_by(|left, right| left.id.cmp(&right.id));
        badges
    }

    /// Whether the kernel knows `terminal_id` as an `AgentSession` resource.
    fn is_agent_session(&self, terminal_id: &ResourceId) -> bool {
        self.kernel.as_ref().is_some_and(|kernel| {
            kernel.resource_kind(terminal_id) == Some(ResourceKind::AgentSession)
        })
    }

    /// Reduce one decoded server frame through the shared kernel.
    ///
    /// A fault on an `AgentSession` stream (the kernel retires that generation
    /// and asks for a resync) never fails the session: the browser cannot
    /// reopen an agent stream, and a badge is not worth the terminal.
    pub fn on_frame(&mut self, frame: FrameKind) -> Outcome {
        if self.failed {
            return Outcome::default();
        }
        if frame_resource_id(&frame).is_some_and(|id| self.retiring_panes.contains(id))
            && !matches!(&frame, FrameKind::ResourceClosed { .. })
        {
            return Outcome::default();
        }
        if let FrameKind::HelloOk { server_caps, .. } = &frame {
            self.path_query_supported = server_caps
                .features_ext
                .contains(ServerFeatureExt::PathQuery);
            self.spawn_initial_size = server_caps
                .features
                .contains(ServerFeature::SpawnInitialSize);
        }
        if let FrameKind::PathResults { request_id, result } = frame {
            self.accept_path_results(request_id, result);
            return Outcome::default();
        }
        let agent_frame = frame_resource_id(&frame).is_some_and(|id| self.is_agent_session(id));
        let split_frame = frame_resource_id(&frame).is_some_and(|id| {
            self.pending_split
                .as_ref()
                .and_then(|pending| pending.resource.as_ref())
                == Some(id)
        });
        let mut outcome = self.reduce_frame(frame);
        if agent_frame && outcome.fatal.is_some() {
            self.failed = false;
            outcome.fatal = None;
        }
        if split_frame && let Some(message) = outcome.fatal.take() {
            self.failed = false;
            return self.pane_refusal(format!(
                "New pane bootstrap failed: {message}. The other panes are still available."
            ));
        }
        self.finish_split(&mut outcome);
        outcome
    }

    fn accept_path_results(&mut self, request_id: u32, result: PathQueryResult) {
        if !self.path_query_supported || self.path_pending != Some(request_id) {
            return;
        }
        if self.path_target.as_ref() != self.first_published_terminal().as_ref() {
            self.cancel_path_query();
            return;
        }
        self.path_pending = None;
        match result {
            Ok(results) => self.path_results = Some(results),
            Err(error) => self.path_error = Some(error.message),
        }
    }

    fn reduce_frame(&mut self, frame: FrameKind) -> Outcome {
        if let Some(outcome) = self.reduce_agent_reply(&frame) {
            return outcome;
        }
        if let Some(outcome) = self.reduce_pane_frame(&frame) {
            return outcome;
        }
        match frame {
            FrameKind::HelloOk {
                protocol_major,
                protocol_minor,
                protocol_patch,
                server_caps,
                selected_profile,
                bootstrap_limits,
                ..
            } => {
                if self.kernel.is_some() {
                    return self.protocol_failure("server sent duplicate HELLO_OK");
                }
                if let Err(message) = validate_hello_ok(
                    self.offered_caps,
                    protocol_major,
                    protocol_minor,
                    protocol_patch,
                    selected_profile,
                    bootstrap_limits,
                ) {
                    return self.protocol_failure(message);
                }
                self.bootstrap_limits = Some(bootstrap_limits);
                self.selected_profile = Some(selected_profile);
                self.terminal_reply_supported =
                    server_caps.features.contains(ServerFeature::TerminalReply);
                let history_config = HistoryCacheConfig {
                    request_max_bytes: bootstrap_limits.max_history_page_bytes(),
                    ..HistoryCacheConfig::default()
                };
                self.kernel = Some(SessionKernel::with_history_config(
                    WebEngine {
                        vt: Rc::clone(&self.vt),
                        limits: bootstrap_limits,
                    },
                    selected_profile,
                    history_config,
                ));
                Outcome {
                    // Subscribe before taking the attach snapshot: a child
                    // created during bootstrap must not fall into a discovery gap.
                    // The kernel's post-ready subscription is idempotent.
                    send: vec![
                        encode(&FrameKind::SubscribeEvents {
                            terminal: None,
                            after_seq: None,
                        }),
                        encode(&FrameKind::Attach {
                            attach_id: ATTACH_ID,
                            target: AttachTarget::CreateIfMissing {
                                name: "default".to_owned(),
                                command: None,
                                cwd: None,
                            },
                            viewport: self.viewport(),
                            request_scrollback: true,
                            scrollback_limit_lines: HISTORY_LINES,
                            role_policy: None,
                        }),
                    ],
                    ..Outcome::default()
                }
            }
            FrameKind::Attached {
                attach_id,
                snapshot,
                ..
            } => self.accept_snapshot(attach_id, snapshot),
            FrameKind::BootstrapBegin {
                terminal_id,
                stream_id,
                bootstrap_id,
                profile,
                cols,
                rows,
                base_seq,
            } => {
                let (outcome, applied) = self.apply_kernel(KernelInput::BootstrapBegin {
                    terminal_id: &terminal_id,
                    stream_id,
                    bootstrap_id,
                    profile,
                    geometry: CanonicalGeometry { cols, rows },
                    base_seq,
                });
                if applied && !self.is_agent_session(&terminal_id) {
                    self.focused_terminal
                        .get_or_insert_with(|| terminal_id.clone());
                }
                outcome
            }
            FrameKind::BootstrapChunk {
                terminal_id,
                stream_id,
                bootstrap_id,
                chunk_seq,
                payload,
            } => {
                self.apply_kernel(KernelInput::BootstrapChunk {
                    terminal_id: &terminal_id,
                    stream_id,
                    bootstrap_id,
                    chunk_seq,
                    payload: &payload,
                })
                .0
            }
            FrameKind::BootstrapReady {
                terminal_id,
                stream_id,
                bootstrap_id,
                history_cursor,
            } => {
                self.apply_kernel(KernelInput::BootstrapReady {
                    terminal_id: &terminal_id,
                    stream_id,
                    bootstrap_id,
                    history_cursor: history_cursor.as_deref(),
                })
                .0
            }
            FrameKind::HistoryPage {
                terminal_id,
                stream_id,
                bootstrap_id,
                page_seq,
                rows,
                cursor,
                next_cursor,
                payload,
            } => {
                self.apply_kernel(KernelInput::HistoryPage {
                    terminal_id: &terminal_id,
                    stream_id,
                    bootstrap_id,
                    page_seq,
                    rows,
                    payload: &payload,
                    cursor: &cursor,
                    next_cursor: next_cursor.as_deref(),
                })
                .0
            }
            FrameKind::HistoryTombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor,
                reason,
            } => {
                let Some(reason) = history_unavailable_reason(reason) else {
                    return self.protocol_failure("unsupported history tombstone reason");
                };
                self.apply_kernel(KernelInput::HistoryTombstone {
                    terminal_id: &terminal_id,
                    stream_id,
                    bootstrap_id,
                    cursor: &cursor,
                    reason,
                })
                .0
            }
            FrameKind::HistoryRejected {
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor,
                reason,
                required_bytes,
                required_rows,
            } => {
                let Some(reason) = history_rejection_reason(reason) else {
                    return self.protocol_failure("unsupported history rejection reason");
                };
                self.apply_kernel(KernelInput::HistoryRejected {
                    terminal_id: &terminal_id,
                    stream_id,
                    bootstrap_id,
                    cursor: &cursor,
                    reason,
                    required_bytes,
                    required_rows,
                })
                .0
            }
            FrameKind::ResourceOutput {
                terminal_id,
                stream_id,
                bootstrap_id,
                seq,
                bytes,
            } => {
                self.apply_kernel(KernelInput::ResourceOutput {
                    terminal_id: &terminal_id,
                    stream_id,
                    bootstrap_id,
                    seq,
                    payload: &bytes,
                })
                .0
            }
            FrameKind::BootstrapTombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                reason,
                last_valid_seq,
            } => {
                self.apply_kernel(KernelInput::Tombstone {
                    terminal_id: &terminal_id,
                    stream_id,
                    bootstrap_id,
                    reason,
                    last_valid_seq,
                })
                .0
            }
            FrameKind::ResourceClosed {
                terminal_id,
                exit_status,
                reason,
                signal,
            } => self.close_resource(terminal_id, exit_status, reason, signal),
            FrameKind::AttachReady { attach_id } => self.finish_attach(attach_id),
            FrameKind::Event {
                terminal: Some(id),
                event,
                ..
            } => self.reduce_resource_event(&id, &event),
            _ => Outcome::default(),
        }
    }

    /// Current styled grid from a published replica, or the initial blank grid.
    #[must_use]
    pub fn grid(&self) -> Grid {
        self.published_terminal()
            .map_or_else(|| self.blank.grid(), |terminal| terminal.grid())
    }

    /// Whether canvas paint is allowed past the aggregate first-damage barrier.
    #[must_use]
    pub const fn render_visible(&self) -> bool {
        self.render_visible
    }

    /// Current published grid dimensions in cells.
    #[must_use]
    pub fn dims(&self) -> (u16, u16) {
        self.published_geometry()
            .map_or((self.cols, self.rows), |geometry| {
                (geometry.cols, geometry.rows)
            })
    }

    /// Scroll the focused replica's viewport `rows` rows (negative is up,
    /// into scrollback). Local only: the server's pane never moves. Returns
    /// whether a published replica took the scroll.
    pub fn scroll_viewport(&self, rows: i32) -> bool {
        let Some(terminal) = self.published_terminal() else {
            return false;
        };
        terminal.scroll_viewport(rows);
        true
    }

    /// Return a scrolled-back viewport to the live screen. Returns whether
    /// it moved (and so needs a repaint).
    pub fn scroll_to_bottom(&self) -> bool {
        match self.published_terminal() {
            Some(terminal) if terminal.viewport_scrolled() => {
                terminal.scroll_to_bottom();
                true
            }
            _ => false,
        }
    }

    /// The title the focused replica's program set (OSC 0/2), empty if none.
    #[must_use]
    pub fn title(&self) -> String {
        self.published_terminal()
            .map(Terminal::title)
            .unwrap_or_default()
    }

    /// Whether the focused replica's viewport is scrolled back.
    #[must_use]
    pub fn viewport_scrolled(&self) -> bool {
        self.published_terminal()
            .is_some_and(Terminal::viewport_scrolled)
    }

    /// Record a new viewport and, once the handshake is done, encode the
    /// `VIEWPORT_RESIZE` announcing it. Before `HELLO_OK` the `ATTACH`
    /// carries the new size instead, so no frame is needed; an unchanged
    /// size sends nothing. The server resizes the pane (subject to its
    /// multi-client size policy) and the replica follows its re-bootstrap.
    pub fn resize_frame(&mut self, cols: u16, rows: u16) -> Option<Vec<u8>> {
        let (cols, rows) = (cols.max(1), rows.max(1));
        if self.failed || (cols, rows) == (self.cols, self.rows) {
            return None;
        }
        self.cols = cols;
        self.rows = rows;
        self.kernel.as_ref()?;
        Some(encode(&FrameKind::ViewportResize {
            viewport: self.viewport(),
        }))
    }

    /// Set the pixel size of one cell as the client draws it and reports
    /// pointer positions in. The next `ATTACH` or `VIEWPORT_RESIZE` reports
    /// the viewport's size in those pixels, from which the server sizes the
    /// cells its mouse encoder divides positions by (SPEC L1 §9.2.1);
    /// without it the server uses another client's cells or its 8x16
    /// default.
    pub fn set_cell_size(&mut self, width: u16, height: u16) {
        self.cell_px = Some((width.max(1), height.max(1)));
    }

    /// The viewport as `ATTACH` and `VIEWPORT_RESIZE` report it: cells, and
    /// pixels when the cell size is known and the grid fits the wire's u16.
    fn viewport(&self) -> ViewportInfo {
        let pixels = self.cell_px.and_then(|(width, height)| {
            Some((
                self.cols.checked_mul(width)?,
                self.rows.checked_mul(height)?,
            ))
        });
        ViewportInfo::new(self.cols, self.rows).with_pixels(
            pixels.map(|(width, _)| width),
            pixels.map(|(_, height)| height),
        )
    }

    /// Encode an eligible structured key event for the focused published pane.
    #[must_use]
    pub fn key_frame(&mut self, event: KeyEvent) -> Option<Vec<u8>> {
        self.input_frame(InputEvent::Key(event))
    }

    /// Encode an eligible input atom (key, paste, ...) for the focused
    /// published pane; `None` while no pane is eligible for input.
    #[must_use]
    pub fn input_frame(&mut self, event: InputEvent) -> Option<Vec<u8>> {
        if self.failed {
            return None;
        }
        let terminal_id = self.first_published_terminal()?;
        let kernel = self.kernel.as_ref()?;
        if !matches!(
            kernel.input_eligibility(&terminal_id),
            InputEligibility::Eligible { .. }
        ) {
            return None;
        }
        let (outcome, applied) = self.apply_kernel(KernelInput::Action(KernelAction::Input {
            terminal_id: &terminal_id,
            event: &event,
        }));
        if applied {
            outcome.send.into_iter().next()
        } else {
            None
        }
    }

    pub(crate) fn pane_focus_frames(&mut self, previous: Option<ResourceId>) -> Vec<Vec<u8>> {
        use phux_protocol::input::focus::FocusEvent;
        let mut frames = Vec::new();
        for (id, focus) in [
            (previous, FocusEvent::Lost),
            (self.focused_pane(), FocusEvent::Gained),
        ] {
            let Some(id) = id else {
                continue;
            };
            if !self
                .pane_terminal(&id)
                .is_some_and(|terminal| terminal.dec_mode(1004))
            {
                continue;
            }
            let event = InputEvent::Focus(focus);
            let (outcome, _) = self.apply_kernel(KernelInput::Action(KernelAction::Input {
                terminal_id: &id,
                event: &event,
            }));
            frames.extend(outcome.send);
        }
        frames
    }

    fn apply_kernel(&mut self, input: KernelInput<'_>) -> (Outcome, bool) {
        let Some(kernel) = self.kernel.as_mut() else {
            self.effects.clear();
            return (
                self.protocol_failure("stateful frame arrived before HELLO_OK"),
                false,
            );
        };
        let result = kernel.update_at(browser_monotonic_ms(), input, &mut self.effects);
        let focused = self.focused_terminal.as_ref();
        let mut outcome = Outcome::default();
        for effect in self.effects.as_slice() {
            match effect {
                KernelEffect::Send(KernelSend::Input { terminal_id, event }) => {
                    outcome
                        .send
                        .push(encode(&(*event).clone().into_frame(terminal_id.clone())));
                }
                KernelEffect::Send(KernelSend::FrameAck {
                    terminal_id,
                    stream_id,
                    bootstrap_id,
                    seq,
                }) => outcome.send.push(encode(&FrameKind::FrameAck {
                    terminal_id: terminal_id.clone(),
                    stream_id: *stream_id,
                    bootstrap_id: *bootstrap_id,
                    seq: *seq,
                })),
                KernelEffect::Send(KernelSend::HistoryRequest {
                    key,
                    cursor,
                    max_bytes,
                    max_rows,
                }) => {
                    outcome.send.push(encode(&FrameKind::HistoryRequest {
                        terminal_id: key.terminal_id.clone(),
                        stream_id: key.stream_id,
                        bootstrap_id: key.bootstrap_id,
                        cursor: Bytes::copy_from_slice(cursor),
                        max_bytes: *max_bytes,
                        max_rows: *max_rows,
                    }));
                }
                KernelEffect::Send(KernelSend::PtyWrite { terminal_id, bytes }) => {
                    if self.terminal_reply_supported {
                        outcome.send.push(encode(&FrameKind::InputTerminalReply {
                            terminal_id: terminal_id.clone(),
                            bytes: Bytes::copy_from_slice(bytes),
                        }));
                    } else {
                        outcome.fatal = Some(
                            "terminal query reply not sent: server lacks terminal-reply support"
                                .to_owned(),
                        );
                    }
                }
                KernelEffect::Send(KernelSend::SubscribeEvents {
                    terminal,
                    after_seq,
                }) => {
                    outcome.send.push(encode(&FrameKind::SubscribeEvents {
                        terminal: terminal.clone(),
                        after_seq: *after_seq,
                    }));
                }
                KernelEffect::Damage(_) => outcome.render = true,
                KernelEffect::AgentRecords { .. } => outcome.badges = true,
                KernelEffect::Status(KernelStatus::Engine {
                    key,
                    status: EngineStatus::Bell,
                }) => {
                    if focused == Some(&key.terminal_id) || focused.is_none() {
                        outcome.bell = true;
                    }
                }
                KernelEffect::Status(_) | KernelEffect::Job(_) => {}
            }
        }
        if outcome.render {
            self.render_visible = true;
        }
        if outcome.fatal.is_some() {
            self.failed = true;
            return (outcome, false);
        }
        match result {
            Ok(()) => (outcome, true),
            Err(error) => {
                self.failed = true;
                outcome.fatal = Some(error.to_string());
                (outcome, false)
            }
        }
    }

    /// Retire expired staging; a failed new split never disconnects its siblings.
    pub fn expire_bootstrap_staging(&mut self) -> Outcome {
        let Some(kernel) = self.kernel.as_mut() else {
            self.effects.clear();
            return Outcome::default();
        };
        let expired = kernel.expire_bootstrap_staging(browser_monotonic_ms(), &mut self.effects);
        if expired == 0 {
            self.effects.clear();
            return Outcome::default();
        }
        let split = self
            .pending_split
            .as_ref()
            .and_then(|pending| pending.resource.as_ref());
        let only_split = self.effects.as_slice().iter().all(|effect| {
            !matches!(effect, KernelEffect::Status(KernelStatus::ResyncRequired { terminal_id, .. })
                if Some(terminal_id) != split)
        });
        self.effects.clear();
        if split.is_some() && only_split {
            self.pane_refusal("New pane bootstrap timed out. The other panes are still available; try splitting again.".to_owned())
        } else {
            self.protocol_failure("terminal bootstrap staging timed out")
        }
    }

    fn protocol_failure(&mut self, message: &str) -> Outcome {
        self.fail_protocol(message);
        Outcome {
            fatal: Some(message.to_owned()),
            ..Outcome::default()
        }
    }

    fn first_published_terminal(&self) -> Option<ResourceId> {
        let kernel = self.kernel.as_ref()?;
        if let Some(focused) = self.focused_terminal.as_ref()
            && kernel.published(focused).is_some()
        {
            return Some(focused.clone());
        }
        self.terminal_order
            .iter()
            .find(|terminal_id| kernel.published(terminal_id).is_some())
            .cloned()
    }

    /// The focused pane's published replica: what the canvas shows, and
    /// what search, copy, links, and mouse-mode checks read.
    #[must_use]
    pub fn terminal(&self) -> Option<&Terminal> {
        self.published_terminal()
    }

    /// The engine instance every replica of this session runs on.
    #[must_use]
    pub fn vt(&self) -> &Rc<Vt> {
        &self.vt
    }

    fn published_terminal(&self) -> Option<&Terminal> {
        let terminal_id = self.first_published_terminal()?;
        let kernel = self.kernel.as_ref()?;
        kernel.published_engine(&terminal_id)?.terminal()
    }

    fn published_geometry(&self) -> Option<CanonicalGeometry> {
        let terminal_id = self.first_published_terminal()?;
        self.kernel
            .as_ref()?
            .published(&terminal_id)
            .map(|replica| replica.geometry())
    }
}

/// Single quotes keep shell metacharacters inert, including embedded apostrophes.
/// Reject terminal controls rather than pasting an executable newline or escape.
fn shell_quote_path(path: &str) -> Option<String> {
    if !path.starts_with('/') || path.chars().any(char::is_control) {
        return None;
    }
    Some(format!("'{}'", path.replace('\'', "'\\''")))
}

fn browser_monotonic_ms() -> u64 {
    web_sys::window()
        .and_then(|window| window.performance())
        .map_or(0, |performance| performance.now().max(0.0) as u64)
}

fn encode(frame: &FrameKind) -> Vec<u8> {
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    buf.to_vec()
}

/// The resource a terminal-stream frame addresses, when it addresses one.
fn frame_resource_id(frame: &FrameKind) -> Option<&ResourceId> {
    match frame {
        FrameKind::BootstrapBegin { terminal_id, .. }
        | FrameKind::BootstrapChunk { terminal_id, .. }
        | FrameKind::BootstrapReady { terminal_id, .. }
        | FrameKind::BootstrapTombstone { terminal_id, .. }
        | FrameKind::HistoryPage { terminal_id, .. }
        | FrameKind::HistoryTombstone { terminal_id, .. }
        | FrameKind::HistoryRejected { terminal_id, .. }
        | FrameKind::ResourceOutput { terminal_id, .. }
        | FrameKind::ResourceClosed { terminal_id, .. } => Some(terminal_id),
        _ => None,
    }
}
