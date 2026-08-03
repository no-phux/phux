//! Protocol-0.7 web session over the shared synchronous client kernel.
//!
//! Transport framing stays here; replica lifecycle, generation validation,
//! ordering, READY fences, and input eligibility stay in `phux-client-core`.

use std::rc::Rc;

use bytes::{Bytes, BytesMut};
use phux_client_core::engine::{
    BootstrapProgress, CanonicalGeometry, EngineAdapter, EngineDamage, EngineEffect,
    EngineEffectBuffer, HistoryApplyOutcome,
};
use phux_client_core::history::HistoryCacheConfig;
use phux_client_core::session::{
    EffectBuffer, HistoryRejectionReason as KernelHistoryRejectionReason, HistoryUnavailableReason,
    InputEligibility, KernelAction, KernelEffect, KernelInput, KernelSend, SessionKernel,
};
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapCapabilities, BootstrapLimits, BootstrapProfile, BootstrapProfileKind,
    BootstrapProfileSet, BootstrapStreamProfile, ClientCapabilities, EngineCodec, EngineFeatureSet,
    ImageProtocolSet, ServerFeature,
};
use phux_protocol::ids::TerminalId;
use phux_protocol::input::InputEvent;
use phux_protocol::input::key::KeyEvent;
use phux_protocol::wire::frame::{
    AttachTarget, FrameKind, HistoryRejectionReason, HistoryTombstoneReason, ViewportInfo,
};
use phux_vt_web::{Grid, NativeCodecError, NativeDecodeKind, NativeDecoder, Terminal, Vt};

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

/// The result of handling one incoming frame.
#[derive(Default)]
pub struct Outcome {
    /// Encoded frames for the browser transport to send.
    pub send: Vec<Vec<u8>>,
    /// Whether a published replica changed and should be repainted.
    pub render: bool,
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
                retained: true,
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
                    retained,
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
                        retained,
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
        Ok(())
    }
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
    focused_terminal: Option<TerminalId>,
    terminal_order: Vec<TerminalId>,
    bootstrap_limits: Option<BootstrapLimits>,
    selected_profile: Option<BootstrapProfile>,
    terminal_reply_supported: bool,
    failed: bool,
    render_visible: bool,
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
            focused_terminal: None,
            terminal_order: Vec::new(),
            bootstrap_limits: None,
            selected_profile: None,
            terminal_reply_supported: false,
            failed: false,
            render_visible: false,
        }
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

    /// Active engine scrollback byte/row budgets after atomic publication.
    #[must_use]
    pub fn active_history_budget(&self) -> Option<(usize, usize)> {
        self.published_terminal()?.history_budget().ok()
    }

    /// Whether this session has entered its terminal protocol-failure state.
    #[must_use]
    pub const fn is_failed(&self) -> bool {
        self.failed
    }

    /// Permanently fail this session after a transport or framing violation.
    pub fn fail_protocol(&mut self, _message: &str) {
        self.failed = true;
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

    /// Reduce one decoded server frame through the shared kernel.
    pub fn on_frame(&mut self, frame: FrameKind) -> Outcome {
        if self.failed {
            return Outcome::default();
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
                    send: vec![encode(&FrameKind::Attach {
                        attach_id: ATTACH_ID,
                        target: AttachTarget::CreateIfMissing {
                            name: "default".to_owned(),
                            command: None,
                            cwd: None,
                        },
                        viewport: ViewportInfo::new(self.cols, self.rows),
                        request_scrollback: true,
                        scrollback_limit_lines: HISTORY_LINES,
                    })],
                    render: false,
                    fatal: None,
                }
            }
            FrameKind::Attached {
                attach_id,
                snapshot,
                ..
            } => {
                if attach_id != ATTACH_ID {
                    return self.protocol_failure("ATTACHED used the wrong attach identifier");
                }
                let terminal_ids: Vec<_> =
                    snapshot.panes.iter().map(|pane| pane.id.clone()).collect();
                let focused_terminal = snapshot.focused_pane;
                let (outcome, applied) = self.apply_kernel(KernelInput::AttachStarted {
                    attach_id,
                    terminals: &terminal_ids,
                });
                if applied {
                    self.focused_terminal = Some(focused_terminal);
                    self.terminal_order = terminal_ids;
                    self.render_visible = false;
                }
                outcome
            }
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
                if applied {
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
            FrameKind::TerminalOutput {
                terminal_id,
                stream_id,
                bootstrap_id,
                seq,
                bytes,
            } => {
                self.apply_kernel(KernelInput::TerminalOutput {
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
            FrameKind::TerminalClosed { terminal_id, .. } => {
                let was_focused = self.focused_terminal.as_ref() == Some(&terminal_id);
                let (outcome, applied) = self.apply_kernel(KernelInput::TerminalClosed {
                    terminal_id: &terminal_id,
                });
                if applied && was_focused {
                    self.focused_terminal = self.first_published_terminal();
                }
                outcome
            }
            FrameKind::AttachReady { attach_id } => {
                self.apply_kernel(KernelInput::AttachReady { attach_id }).0
            }
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

    /// Encode an eligible structured key event for the focused published pane.
    #[must_use]
    pub fn key_frame(&mut self, event: KeyEvent) -> Option<Vec<u8>> {
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
            event: &InputEvent::Key(event),
        }));
        if applied {
            outcome.send.into_iter().next()
        } else {
            None
        }
    }

    fn apply_kernel(&mut self, input: KernelInput<'_>) -> (Outcome, bool) {
        let Some(kernel) = self.kernel.as_mut() else {
            self.effects.clear();
            return (
                self.protocol_failure("stateful frame arrived before HELLO_OK"),
                false,
            );
        };
        let result = kernel.update(input, &mut self.effects);
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
                KernelEffect::Damage(damage) => {
                    if focused == Some(&damage.terminal_id) || focused.is_none() {
                        outcome.render = true;
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

    fn protocol_failure(&mut self, message: &str) -> Outcome {
        self.fail_protocol(message);
        Outcome {
            fatal: Some(message.to_owned()),
            ..Outcome::default()
        }
    }

    fn first_published_terminal(&self) -> Option<TerminalId> {
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

fn encode(frame: &FrameKind) -> Vec<u8> {
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    buf.to_vec()
}
