//! Native libghostty implementation of the session-kernel engine boundary.
//!
//! The checkpoint stream remains opaque here. Fragmentation, authentication,
//! READY transfer, continuation replay, history retention, and FINISH are all
//! delegated to libghostty's safe incremental wrapper.

use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
    marker::PhantomData,
    rc::Rc,
};

use libghostty_vt::{
    Terminal as GhosttyTerminal, TerminalOptions,
    screen::{CellContentTag, CellWide, TrackedGridRef},
    selection::{FormatOptions, Selection},
    snapshot::incremental::{
        AfterReadyStep, DecodeProgress, DecodeStep, Decoder, DecoderOptions, Error as SnapshotError,
    },
    terminal::{Point, PointCoordinate, PointSpace, ScrollViewport},
};
use phux_protocol::{
    BootstrapCapabilities, BootstrapLimits, BootstrapStreamProfile, EngineCodec, EngineFeatureSet,
};
use thiserror::Error;

use super::{
    BootstrapProgress, CanonicalGeometry, DocumentPoint, DocumentSpace, EngineAdapter,
    EngineDamage, EngineDocumentAdapter, EngineDocumentSelection, EngineEffect, EngineEffectBuffer,
    EngineHistoryProjection, EngineProjectionOrigin, EngineProjectionRow, EngineSearchMatch,
    EngineSend, HistoryApplyOutcome,
};
use crate::history::DocumentAnchorId;

const CHECKPOINT_VERSION: u16 = EngineCodec::LibghosttyCheckpointV2 as u8 as u16;
const CHECKPOINT_IDENTITY: &str = "ghostty.snapshot.v1-v2.incremental.v1";
const SYNTH_SCROLLBACK_ROWS: usize = 10_000;

/// Return the client bootstrap capabilities supported by the linked engine.
///
/// Native v2 is added only when every required runtime guarantee and caller
/// bound is reported by libghostty. Any probe failure leaves both synthesized
/// compatibility profiles available.
#[must_use]
pub fn native_bootstrap_capabilities(limits: BootstrapLimits) -> BootstrapCapabilities {
    let capabilities = BootstrapCapabilities::new().with_limits(limits);
    let Ok(native) = libghostty_vt::snapshot::incremental::capabilities() else {
        return capabilities;
    };
    if supports_native(&native, limits) {
        capabilities.with_native(
            EngineCodec::LibghosttyCheckpointV2,
            EngineFeatureSet::required_native(),
        )
    } else {
        capabilities
    }
}

fn supports_native(
    capabilities: &libghostty_vt::snapshot::incremental::Capabilities,
    limits: BootstrapLimits,
) -> bool {
    let required_record_bytes = limits
        .max_chunk_bytes()
        .max(limits.max_history_page_bytes()) as usize;
    capabilities.default_encode_version == CHECKPOINT_VERSION
        && capabilities.min_decode_version <= CHECKPOINT_VERSION
        && capabilities.max_decode_version >= CHECKPOINT_VERSION
        && capabilities.incremental
        && capabilities.ready
        && capabilities.history
        && capabilities.authenticated_tokens
        && capabilities.bounded_records
        && capabilities.bounded_pages
        && capabilities.bounded_units
        && capabilities.max_record_bytes >= required_record_bytes
        && capabilities.max_pages > 0
        && capabilities.max_unit_bytes > 0
        && capabilities.max_rows > 0
        && capabilities.codec_identity == CHECKPOINT_IDENTITY
}

/// Concrete, current-thread libghostty engine host.
///
/// `limits` are the payload bounds negotiated in `HELLO_OK`; they become hard
/// decoder budgets rather than hints. The `Rc` marker deliberately keeps the
/// host on the same thread as every libghostty object it creates.
///
/// ```compile_fail
/// fn require_thread_safe<T: Send + Sync>() {}
/// require_thread_safe::<phux_client_core::engine::ghostty::GhosttyAdapter>();
/// ```
#[derive(Debug)]
pub struct GhosttyAdapter {
    limits: BootstrapLimits,
    decoder_options: DecoderOptions,
    native_available: bool,
    next_anchor_id: u64,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl GhosttyAdapter {
    /// Construct an adapter for one connection's negotiated bootstrap limits.
    #[must_use]
    pub fn new(limits: BootstrapLimits) -> Self {
        let record_bytes = limits
            .max_chunk_bytes()
            .max(limits.max_history_page_bytes()) as usize;
        let defaults = DecoderOptions::default();
        let native = libghostty_vt::snapshot::incremental::capabilities()
            .ok()
            .filter(|capabilities| supports_native(capabilities, limits));
        let max_pages = native.as_ref().map_or(defaults.max_pages, |capabilities| {
            defaults.max_pages.min(capabilities.max_pages)
        });
        Self {
            limits,
            decoder_options: DecoderOptions {
                max_continuation_bytes: defaults.max_continuation_bytes.min(record_bytes),
                max_record_bytes: record_bytes,
                max_pages,
            },
            native_available: native.is_some(),
            next_anchor_id: 1,
            _not_send_or_sync: PhantomData,
        }
    }

    /// Negotiated payload limits enforced by this adapter.
    #[must_use]
    pub const fn limits(&self) -> BootstrapLimits {
        self.limits
    }
}

/// One adapter-owned libghostty replica.
///
/// ```compile_fail
/// fn require_thread_safe<T: Send + Sync>() {}
/// require_thread_safe::<phux_client_core::engine::ghostty::GhosttyReplica>();
/// ```
#[derive(Debug)]
pub struct GhosttyReplica {
    profile: BootstrapStreamProfile,
    anchors: HashMap<DocumentAnchorId, TrackedGridRef>,
    state: ReplicaState,
    history_max_bytes: Option<usize>,
    history_max_lines: Option<usize>,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl GhosttyReplica {
    /// Exact stream profile used to allocate this replica.
    #[must_use]
    pub const fn profile(&self) -> BootstrapStreamProfile {
        self.profile
    }

    /// Borrow the renderable terminal.
    ///
    /// Native replicas intentionally return `None` until authenticated READY
    /// has transferred the terminal and replayed its parser continuation.
    #[must_use]
    pub fn terminal(&self) -> Option<&GhosttyTerminal<'static, 'static>> {
        match &self.state {
            ReplicaState::Synthesized { terminal, .. } => Some(terminal),
            ReplicaState::Native(native) => native.terminal(),
        }
    }

    fn set_scrollback_max_bytes(&mut self, max: Option<usize>) -> Result<(), GhosttyEngineError> {
        match &mut self.state {
            ReplicaState::Synthesized { terminal, .. } => {
                terminal.set_scrollback_max_bytes(max)?;
            }
            ReplicaState::Native(native) => match &mut native.decoder {
                NativeDecoderState::AfterReady(stream) => {
                    stream.set_scrollback_max_bytes(max)?;
                }
                NativeDecoderState::Finished(terminal)
                | NativeDecoderState::Failed(Some(terminal)) => {
                    terminal.set_scrollback_max_bytes(max)?;
                }
                NativeDecoderState::BeforeReady(_) | NativeDecoderState::Failed(None) => {}
            },
        }
        Ok(())
    }

    fn set_scrollback_max_lines(&mut self, max: Option<usize>) -> Result<(), GhosttyEngineError> {
        match &mut self.state {
            ReplicaState::Synthesized { terminal, .. } => {
                terminal.set_scrollback_max_lines(max)?;
            }
            ReplicaState::Native(native) => match &mut native.decoder {
                NativeDecoderState::AfterReady(stream) => {
                    stream.set_scrollback_max_lines(max)?;
                }
                NativeDecoderState::Finished(terminal)
                | NativeDecoderState::Failed(Some(terminal)) => {
                    terminal.set_scrollback_max_lines(max)?;
                }
                NativeDecoderState::BeforeReady(_) | NativeDecoderState::Failed(None) => {}
            },
        }
        Ok(())
    }

    /// Apply a client-local viewport scroll without exposing mutable terminal ownership.
    pub fn scroll_viewport(&mut self, scroll: ScrollViewport) -> Result<(), GhosttyEngineError> {
        match &mut self.state {
            ReplicaState::Synthesized { terminal, .. } => terminal.scroll_viewport(scroll),
            ReplicaState::Native(native) => match &mut native.decoder {
                NativeDecoderState::AfterReady(stream) => stream.scroll_viewport(scroll),
                NativeDecoderState::Finished(terminal)
                | NativeDecoderState::Failed(Some(terminal)) => terminal.scroll_viewport(scroll),
                NativeDecoderState::BeforeReady(_) => {
                    return Err(GhosttyEngineError::LiveOutputBeforeReady);
                }
                NativeDecoderState::Failed(None) => {
                    return Err(GhosttyEngineError::DecoderFailed);
                }
            },
        }
        Ok(())
    }
}

type PtyResponses = Rc<RefCell<Vec<Vec<u8>>>>;

fn drain_pty_responses(responses: &PtyResponses, effects: &mut EngineEffectBuffer) {
    for bytes in responses.borrow_mut().drain(..) {
        effects.push(EngineEffect::Send(EngineSend::PtyWrite(bytes)));
    }
}

#[derive(Debug)]
enum ReplicaState {
    Synthesized {
        terminal: GhosttyTerminal<'static, 'static>,
        protocol_finished: bool,
        pty_responses: PtyResponses,
    },
    Native(NativeReplica),
}

#[derive(Debug)]
struct NativeReplica {
    decoder: NativeDecoderState,
    protocol_finished: bool,
    pty_responses: PtyResponses,
}

impl NativeReplica {
    fn terminal(&self) -> Option<&GhosttyTerminal<'static, 'static>> {
        match &self.decoder {
            NativeDecoderState::BeforeReady(_) | NativeDecoderState::Failed(None) => None,
            NativeDecoderState::AfterReady(stream) => Some(stream.terminal()),
            NativeDecoderState::Finished(terminal) | NativeDecoderState::Failed(Some(terminal)) => {
                Some(terminal)
            }
        }
    }
}

#[derive(Debug)]
enum NativeDecoderState {
    BeforeReady(Decoder<'static>),
    AfterReady(libghostty_vt::snapshot::incremental::DecodedStream<'static, 'static>),
    Finished(GhosttyTerminal<'static, 'static>),
    Failed(Option<GhosttyTerminal<'static, 'static>>),
}

/// Typed failures from the concrete libghostty engine host.
#[derive(Debug, Error)]
pub enum GhosttyEngineError {
    /// The selected stream profile is unavailable from the linked engine.
    #[error("unsupported bootstrap stream profile: {0:?}")]
    UnsupportedProfile(BootstrapStreamProfile),
    /// A normal terminal allocation or query failed.
    #[error("libghostty terminal operation failed: {0}")]
    Terminal(#[from] libghostty_vt::Error),
    /// The incremental checkpoint wrapper rejected the opaque stream.
    #[error("libghostty checkpoint failed after consuming {consumed} bytes: {source}")]
    Checkpoint {
        /// Exact libghostty status; callers never recover it from text.
        source: SnapshotError,
        /// Exact bytes consumed from the submitted fragment.
        consumed: usize,
    },
    /// An envelope for a different exact native codec was received.
    #[error("wrong checkpoint codec version: expected {expected}, got {actual}")]
    WrongCodecVersion {
        /// Negotiated immutable codec version.
        expected: u16,
        /// Version authenticated by the decoder.
        actual: u16,
    },
    /// One borrowed protocol payload exceeded its negotiated frame bound.
    #[error("engine payload is {actual} bytes; negotiated limit is {limit}")]
    PayloadLimitExceeded {
        /// Borrowed payload length.
        actual: usize,
        /// Negotiated maximum payload length.
        limit: usize,
    },
    /// Compatibility profiles do not accept native history pages.
    #[error("history pages are unsupported for bootstrap stream profile: {0:?}")]
    HistoryUnsupported(BootstrapStreamProfile),
    /// A native bootstrap chunk continued past its authenticated READY record.
    #[error("{trailing} trailing bootstrap bytes after READY")]
    TrailingAfterReady {
        /// Unconsumed bytes after authenticated READY.
        trailing: usize,
    },
    /// A bootstrap chunk arrived after authenticated READY.
    #[error("bootstrap input arrived after READY")]
    InputAfterReady,
    /// A history page arrived before protocol READY published the replica.
    #[error("history page arrived before the native replica was published")]
    HistoryBeforePublication,
    /// Live bytes arrived before the native READY transfer completed.
    #[error("native terminal is not ready for live output")]
    LiveOutputBeforeReady,
    /// Bootstrap bytes or a second protocol finish arrived after FINISH.
    #[error("bootstrap input arrived after FINISH")]
    InputAfterFinish,
    /// FINISH was followed by bytes in the same borrowed fragment.
    #[error("{trailing} trailing bootstrap bytes after FINISH")]
    TrailingAfterFinish {
        /// Unconsumed bytes after the authenticated FINISH record.
        trailing: usize,
    },
    /// The wrapper reported transition accounting that cannot make progress.
    #[error("invalid checkpoint transition accounting: consumed {consumed} of {available}")]
    InvalidProgress {
        /// Wrapper-reported byte consumption.
        consumed: usize,
        /// Bytes offered in this transition.
        available: usize,
    },
    /// The native decoder has already failed and cannot be driven again.
    #[error("native checkpoint decoder is no longer usable")]
    DecoderFailed,
}

impl GhosttyEngineError {
    fn checkpoint(source: SnapshotError, consumed: usize) -> Self {
        Self::Checkpoint { source, consumed }
    }
}

impl EngineAdapter for GhosttyAdapter {
    type Replica = GhosttyReplica;
    type Error = GhosttyEngineError;

    fn start_replica(
        &mut self,
        profile: BootstrapStreamProfile,
        geometry: CanonicalGeometry,
    ) -> Result<Self::Replica, Self::Error> {
        let state = match profile {
            BootstrapStreamProfile::SynthesizedVtRaw
            | BootstrapStreamProfile::SynthesizedVtStateSync => {
                let pty_responses: PtyResponses = Rc::new(RefCell::new(Vec::new()));
                let mut terminal = GhosttyTerminal::new(TerminalOptions {
                    cols: geometry.cols,
                    rows: geometry.rows,
                    max_scrollback: SYNTH_SCROLLBACK_ROWS,
                })?;
                terminal.on_pty_write({
                    let pty_responses = Rc::clone(&pty_responses);
                    move |_terminal, bytes| pty_responses.borrow_mut().push(bytes.to_vec())
                })?;
                ReplicaState::Synthesized {
                    terminal,
                    protocol_finished: false,
                    pty_responses,
                }
            }
            BootstrapStreamProfile::NativeState {
                codec: EngineCodec::LibghosttyCheckpointV2,
            } if self.native_available => {
                let decoder = Decoder::new(self.decoder_options)
                    .map_err(|error| GhosttyEngineError::checkpoint(error, 0))?;
                ReplicaState::Native(NativeReplica {
                    decoder: NativeDecoderState::BeforeReady(decoder),
                    protocol_finished: false,
                    pty_responses: Rc::new(RefCell::new(Vec::new())),
                })
            }
            _ => return Err(GhosttyEngineError::UnsupportedProfile(profile)),
        };
        Ok(GhosttyReplica {
            profile,
            state,
            anchors: HashMap::new(),
            history_max_bytes: None,
            history_max_lines: None,
            _not_send_or_sync: PhantomData,
        })
    }

    fn configure_history_budget(
        &mut self,
        replica: &mut Self::Replica,
        max_bytes: usize,
        max_rows: usize,
    ) -> Result<(), Self::Error> {
        replica.history_max_bytes = Some(max_bytes.max(1));
        replica.history_max_lines = Some(max_rows.max(1));
        replica.set_scrollback_max_bytes(replica.history_max_bytes)?;
        replica.set_scrollback_max_lines(replica.history_max_lines)?;
        Ok(())
    }

    fn total_rows(&self, replica: &Self::Replica) -> Result<u64, Self::Error> {
        let Some(terminal) = replica.terminal() else {
            return Ok(0);
        };
        Ok(u64::try_from(terminal.total_rows()?).unwrap_or(u64::MAX))
    }

    fn clear_document_state(&mut self, replica: &mut Self::Replica) {
        replica.anchors.clear();
    }

    fn history_anchor_tail_distance(
        &self,
        replica: &Self::Replica,
        anchor: DocumentAnchorId,
    ) -> Result<Option<u64>, Self::Error> {
        let Some(anchor) = replica.anchors.get(&anchor) else {
            return Ok(None);
        };
        let Some(point) = anchor.point(PointSpace::History)? else {
            return Ok(None);
        };
        let rows = replica
            .terminal()
            .ok_or(GhosttyEngineError::LiveOutputBeforeReady)?
            .scrollback_rows()?;
        let y = point.y as usize;
        if y >= rows {
            return Ok(None);
        }
        Ok(Some(
            u64::try_from(rows.saturating_sub(y).saturating_sub(1)).unwrap_or(u64::MAX),
        ))
    }

    fn apply_bootstrap_chunk(
        &mut self,
        replica: &mut Self::Replica,
        payload: &[u8],
        effects: &mut EngineEffectBuffer,
    ) -> Result<BootstrapProgress, Self::Error> {
        let limit = self.limits.max_chunk_bytes() as usize;
        if payload.len() > limit {
            return Err(GhosttyEngineError::PayloadLimitExceeded {
                actual: payload.len(),
                limit,
            });
        }
        let (progress, pty_responses) = match &mut replica.state {
            ReplicaState::Synthesized {
                terminal,
                protocol_finished,
                pty_responses,
            } => {
                if *protocol_finished {
                    return Err(GhosttyEngineError::InputAfterFinish);
                }
                terminal.vt_write(payload);
                (BootstrapProgress::Pending, &*pty_responses)
            }
            ReplicaState::Native(native) => {
                let progress = push_native(native, payload)?;
                (progress, &native.pty_responses)
            }
        };
        drain_pty_responses(pty_responses, effects);
        enforce_history_budget(replica)?;
        Ok(progress)
    }

    fn finish_bootstrap(
        &mut self,
        replica: &mut Self::Replica,
        effects: &mut EngineEffectBuffer,
    ) -> Result<BootstrapProgress, Self::Error> {
        let (progress, pty_responses) = match &mut replica.state {
            ReplicaState::Synthesized {
                protocol_finished,
                pty_responses,
                ..
            } => {
                if std::mem::replace(protocol_finished, true) {
                    return Err(GhosttyEngineError::InputAfterFinish);
                }
                (BootstrapProgress::Finished, &*pty_responses)
            }
            ReplicaState::Native(native) => {
                let progress = finish_native(native)?;
                (progress, &native.pty_responses)
            }
        };
        drain_pty_responses(pty_responses, effects);
        enforce_history_budget(replica)?;
        Ok(progress)
    }

    fn apply_history_page(
        &mut self,
        replica: &mut Self::Replica,
        payload: &[u8],
        effects: &mut EngineEffectBuffer,
    ) -> Result<HistoryApplyOutcome, Self::Error> {
        let limit = self.limits.max_history_page_bytes() as usize;
        if payload.len() > limit {
            return Err(GhosttyEngineError::PayloadLimitExceeded {
                actual: payload.len(),
                limit,
            });
        }
        let profile = replica.profile;
        let (progress, pty_responses) = match &mut replica.state {
            ReplicaState::Synthesized { .. } => {
                return Err(GhosttyEngineError::HistoryUnsupported(profile));
            }
            ReplicaState::Native(native) => {
                let outcome = push_history(native, payload)?;
                (outcome, &native.pty_responses)
            }
        };
        drain_pty_responses(pty_responses, effects);
        enforce_history_budget(replica)?;
        Ok(progress)
    }

    fn apply_output(
        &mut self,
        replica: &mut Self::Replica,
        payload: &[u8],
        effects: &mut EngineEffectBuffer,
    ) -> Result<(), Self::Error> {
        let pty_responses = match &mut replica.state {
            ReplicaState::Native(native) if !native.protocol_finished => {
                return Err(GhosttyEngineError::LiveOutputBeforeReady);
            }
            ReplicaState::Synthesized {
                terminal,
                pty_responses,
                ..
            } => {
                terminal.vt_write(payload);
                pty_responses
            }
            ReplicaState::Native(native) => {
                match &mut native.decoder {
                    NativeDecoderState::BeforeReady(_) => {
                        return Err(GhosttyEngineError::LiveOutputBeforeReady);
                    }
                    NativeDecoderState::AfterReady(stream) => stream.vt_write(payload),
                    NativeDecoderState::Finished(terminal)
                    | NativeDecoderState::Failed(Some(terminal)) => terminal.vt_write(payload),
                    NativeDecoderState::Failed(None) => {
                        return Err(GhosttyEngineError::DecoderFailed);
                    }
                }
                &native.pty_responses
            }
        };
        drain_pty_responses(pty_responses, effects);
        effects.push(EngineEffect::Damage(EngineDamage::Full));
        Ok(())
    }
}

impl EngineDocumentAdapter for GhosttyAdapter {
    fn project_history(
        &mut self,
        replica: &mut Self::Replica,
        width: u16,
        origin: EngineProjectionOrigin,
        max_rows: usize,
    ) -> Result<EngineHistoryProjection, Self::Error> {
        let terminal = replica
            .terminal()
            .ok_or(GhosttyEngineError::LiveOutputBeforeReady)?;
        let width = width.max(2);
        if max_rows == 0 {
            return Ok(EngineHistoryProjection {
                width,
                rows: Vec::new(),
                has_older: false,
            });
        }
        let history_rows = terminal.scrollback_rows()?;
        let physical_limit = max_rows.saturating_add(1);
        let (mut start, tail) = match origin {
            EngineProjectionOrigin::Tail => (history_rows.saturating_sub(physical_limit), true),
            EngineProjectionOrigin::Anchor(anchor) => {
                let Some(anchor) = replica.anchors.get(&anchor) else {
                    return Ok(EngineHistoryProjection {
                        width,
                        rows: Vec::new(),
                        has_older: true,
                    });
                };
                let Some(point) = anchor.point(PointSpace::History)? else {
                    return Ok(EngineHistoryProjection {
                        width,
                        rows: Vec::new(),
                        has_older: true,
                    });
                };
                (point.y as usize, false)
            }
        };
        while start > 0 && history_row_wrapped(terminal, start - 1)? {
            start -= 1;
        }
        let mut physical_end = history_rows.min(start.saturating_add(physical_limit));
        while physical_end < history_rows
            && physical_end > start
            && history_row_wrapped(terminal, physical_end - 1)?
        {
            physical_end += 1;
        }
        let source = engine_history_rows(terminal, start, physical_end)?;
        let mut rows = rewrap_history_rows(source, width);
        let mut has_older = start > 0;
        if rows.len() > max_rows {
            if tail {
                rows.drain(..rows.len() - max_rows);
            } else {
                rows.truncate(max_rows);
            }
            if tail {
                has_older = true;
            }
        }
        Ok(EngineHistoryProjection {
            width,
            rows,
            has_older,
        })
    }

    fn track_document_anchor(
        &mut self,
        replica: &mut Self::Replica,
        point: DocumentPoint,
    ) -> Result<DocumentAnchorId, Self::Error> {
        let tracked = replica
            .terminal()
            .ok_or(GhosttyEngineError::LiveOutputBeforeReady)?
            .track_grid_ref(to_ghostty_point(point))?;
        let id = DocumentAnchorId::from_raw(self.next_anchor_id);
        self.next_anchor_id = self.next_anchor_id.wrapping_add(1).max(1);
        replica.anchors.insert(id, tracked);
        Ok(id)
    }

    fn release_document_anchor(&mut self, replica: &mut Self::Replica, anchor: DocumentAnchorId) {
        replica.anchors.remove(&anchor);
    }

    fn document_anchor_point(
        &self,
        replica: &Self::Replica,
        anchor: DocumentAnchorId,
        space: DocumentSpace,
    ) -> Result<Option<DocumentPoint>, Self::Error> {
        let Some(anchor) = replica.anchors.get(&anchor) else {
            return Ok(None);
        };
        let Some(point) = anchor.point(to_ghostty_space(space))? else {
            return Ok(None);
        };
        Ok(Some(DocumentPoint {
            space,
            x: point.x,
            y: point.y,
        }))
    }

    fn search_loaded(
        &mut self,
        replica: &mut Self::Replica,
        needle: &str,
        max_matches: usize,
    ) -> Result<Vec<EngineSearchMatch>, Self::Error> {
        if needle.is_empty() || max_matches == 0 {
            return Ok(Vec::new());
        }
        let needle: Vec<char> = needle.chars().collect();
        let mut ranges = Vec::new();
        let mut window = VecDeque::with_capacity(needle.len());
        let mut push_scalar = |scalar: char, point: DocumentPoint| {
            window.push_back((scalar, point));
            if window.len() > needle.len() {
                window.pop_front();
            }
            if window.len() == needle.len()
                && window
                    .iter()
                    .map(|(value, _)| *value)
                    .eq(needle.iter().copied())
            {
                ranges.push((
                    window.front().expect("non-empty search").1,
                    window.back().expect("non-empty search").1,
                ));
            }
            ranges.len() == max_matches
        };
        {
            let terminal = replica
                .terminal()
                .ok_or(GhosttyEngineError::LiveOutputBeforeReady)?;
            let cols = terminal.cols()?;
            let mut y = 0_u32;
            'history: loop {
                let first = match terminal.grid_ref(Point::History(PointCoordinate { x: 0, y })) {
                    Ok(value) => value,
                    Err(libghostty_vt::Error::InvalidValue) => break,
                    Err(error) => return Err(error.into()),
                };
                let wrapped = first.row()?.is_wrapped()?;
                for x in 0..cols {
                    let grid_ref = terminal.grid_ref(Point::History(PointCoordinate { x, y }))?;
                    for scalar in grid_ref_graphemes(&grid_ref)? {
                        if push_scalar(
                            scalar,
                            DocumentPoint {
                                space: DocumentSpace::History,
                                x,
                                y,
                            },
                        ) {
                            break 'history;
                        }
                    }
                }
                if !wrapped
                    && push_scalar(
                        '\n',
                        DocumentPoint {
                            space: DocumentSpace::History,
                            x: cols.saturating_sub(1),
                            y,
                        },
                    )
                {
                    break;
                }
                y = match y.checked_add(1) {
                    Some(value) => value,
                    None => break,
                };
            }
        }
        let mut matches: Vec<EngineSearchMatch> = Vec::with_capacity(ranges.len());
        for (start, end) in ranges {
            let start = match self.track_document_anchor(replica, start) {
                Ok(anchor) => anchor,
                Err(error) => {
                    for found in matches.drain(..) {
                        self.release_document_anchor(replica, found.start);
                        self.release_document_anchor(replica, found.end);
                    }
                    return Err(error);
                }
            };
            let end = match self.track_document_anchor(replica, end) {
                Ok(anchor) => anchor,
                Err(error) => {
                    self.release_document_anchor(replica, start);
                    for found in matches.drain(..) {
                        self.release_document_anchor(replica, found.start);
                        self.release_document_anchor(replica, found.end);
                    }
                    return Err(error);
                }
            };
            matches.push(EngineSearchMatch { start, end });
        }
        Ok(matches)
    }

    fn format_selection(
        &self,
        replica: &Self::Replica,
        selection: EngineDocumentSelection,
    ) -> Result<Option<String>, Self::Error> {
        let terminal = replica
            .terminal()
            .ok_or(GhosttyEngineError::LiveOutputBeforeReady)?;
        let Some(start) = replica.anchors.get(&selection.start) else {
            return Ok(None);
        };
        let Some(end) = replica.anchors.get(&selection.end) else {
            return Ok(None);
        };
        let Some(start) = start.snapshot(terminal)? else {
            return Ok(None);
        };
        let Some(end) = end.snapshot(terminal)? else {
            return Ok(None);
        };
        let selection = Selection::new(start, end, selection.rectangle);
        let formatted = terminal
            .format_selection_alloc(None, FormatOptions::new().with_selection(&selection))?;
        Ok(formatted.map(|bytes| String::from_utf8_lossy(&bytes).into_owned()))
    }
}

fn enforce_history_budget(replica: &mut GhosttyReplica) -> Result<(), GhosttyEngineError> {
    if replica.history_max_bytes.is_some() {
        replica.set_scrollback_max_bytes(replica.history_max_bytes)?;
    }
    if replica.history_max_lines.is_some() {
        replica.set_scrollback_max_lines(replica.history_max_lines)?;
    }
    Ok(())
}

fn to_ghostty_point(point: DocumentPoint) -> Point {
    let coordinate = PointCoordinate {
        x: point.x,
        y: point.y,
    };
    match point.space {
        DocumentSpace::History => Point::History(coordinate),
        DocumentSpace::Viewport => Point::Viewport(coordinate),
        DocumentSpace::Active => Point::Active(coordinate),
    }
}

fn to_ghostty_space(space: DocumentSpace) -> PointSpace {
    match space {
        DocumentSpace::History => PointSpace::History,
        DocumentSpace::Viewport => PointSpace::Viewport,
        DocumentSpace::Active => PointSpace::Active,
    }
}

fn history_row_wrapped(
    terminal: &GhosttyTerminal<'_, '_>,
    row: usize,
) -> Result<bool, GhosttyEngineError> {
    let y = u32::try_from(row).unwrap_or(u32::MAX);
    Ok(terminal
        .grid_ref(Point::History(PointCoordinate { x: 0, y }))?
        .row()?
        .is_wrapped()?)
}

fn grid_ref_graphemes(
    grid_ref: &libghostty_vt::screen::GridRef<'_>,
) -> Result<Vec<char>, GhosttyEngineError> {
    let mut inline = ['\0'; 8];
    match grid_ref.graphemes(&mut inline) {
        Ok(len) => Ok(inline[..len].to_vec()),
        Err(libghostty_vt::Error::OutOfSpace { required }) => {
            let mut values = vec!['\0'; required];
            let len = grid_ref.graphemes(&mut values)?;
            values.truncate(len);
            Ok(values)
        }
        Err(error) => Err(error.into()),
    }
}
#[derive(Debug)]
struct ProjectedCell {
    text: String,
    width: usize,
    empty_default: bool,
}

fn engine_history_rows(
    terminal: &GhosttyTerminal<'_, '_>,
    start: usize,
    end: usize,
) -> Result<Vec<(Vec<ProjectedCell>, bool)>, GhosttyEngineError> {
    let cols = terminal.cols()?;
    let mut rows = Vec::with_capacity(end.saturating_sub(start));
    for row in start..end {
        let y = u32::try_from(row).unwrap_or(u32::MAX);
        let first = terminal.grid_ref(Point::History(PointCoordinate { x: 0, y }))?;
        let wrapped = first.row()?.is_wrapped()?;
        let mut cells = Vec::with_capacity(usize::from(cols));
        for x in 0..cols {
            let grid_ref = terminal.grid_ref(Point::History(PointCoordinate { x, y }))?;
            let cell = grid_ref.cell()?;
            let wide = cell.wide()?;
            if matches!(wide, CellWide::SpacerTail | CellWide::SpacerHead) {
                continue;
            }
            let text: String = grid_ref_graphemes(&grid_ref)?.into_iter().collect();
            cells.push(ProjectedCell {
                empty_default: text.is_empty()
                    && cell.codepoint()? == 0
                    && cell.content_tag()? == CellContentTag::Codepoint,
                text,
                width: if wide == CellWide::Wide { 2 } else { 1 },
            });
        }
        rows.push((cells, wrapped));
    }
    Ok(rows)
}

fn rewrap_history_rows(
    source: Vec<(Vec<ProjectedCell>, bool)>,
    width: u16,
) -> Vec<EngineProjectionRow> {
    let width = usize::from(width);
    let mut result = Vec::new();
    let mut logical = Vec::new();
    for (mut cells, wrapped) in source {
        logical.append(&mut cells);
        if !wrapped {
            append_rewrapped_line(&mut result, &mut logical, width);
        }
    }
    if !logical.is_empty() {
        append_rewrapped_line(&mut result, &mut logical, width);
    }
    result
}

fn append_rewrapped_line(
    result: &mut Vec<EngineProjectionRow>,
    logical: &mut Vec<ProjectedCell>,
    width: usize,
) {
    while logical.last().is_some_and(|cell| cell.empty_default) {
        logical.pop();
    }
    if logical.is_empty() {
        result.push(EngineProjectionRow {
            text: String::new(),
            soft_wrapped: false,
            page: None,
        });
        return;
    }
    let mut text = String::new();
    let mut cells: usize = 0;
    for cell in logical.drain(..) {
        if cells > 0 && cells.saturating_add(cell.width) > width {
            result.push(EngineProjectionRow {
                text: std::mem::take(&mut text),
                soft_wrapped: true,
                page: None,
            });
            cells = 0;
        }
        if cell.text.is_empty() {
            text.push(' ');
        } else {
            text.push_str(&cell.text);
        }
        cells = cells.saturating_add(cell.width);
    }
    result.push(EngineProjectionRow {
        text,
        soft_wrapped: false,
        page: None,
    });
}

fn push_native(
    native: &mut NativeReplica,
    mut input: &[u8],
) -> Result<BootstrapProgress, GhosttyEngineError> {
    if native.protocol_finished {
        return Err(GhosttyEngineError::InputAfterReady);
    }
    if input.is_empty() {
        return match native.decoder {
            NativeDecoderState::BeforeReady(_) => Ok(BootstrapProgress::Pending),
            NativeDecoderState::AfterReady(_) => Err(GhosttyEngineError::InputAfterReady),
            NativeDecoderState::Finished(_) => Err(GhosttyEngineError::InputAfterFinish),
            NativeDecoderState::Failed(_) => Err(GhosttyEngineError::DecoderFailed),
        };
    }

    loop {
        let state = std::mem::replace(&mut native.decoder, NativeDecoderState::Failed(None));
        match state {
            NativeDecoderState::BeforeReady(decoder) => match decoder.push(input) {
                Err(failure) => {
                    return Err(GhosttyEngineError::checkpoint(
                        failure.error,
                        failure.consumed,
                    ));
                }
                Ok(DecodeStep::NeedInput { decoder, progress })
                | Ok(DecodeStep::Progress { decoder, progress }) => {
                    check_version(progress)?;
                    native.decoder = NativeDecoderState::BeforeReady(decoder);
                    input = remaining(input, progress)?;
                    if input.is_empty() {
                        return Ok(BootstrapProgress::Pending);
                    }
                }
                Ok(DecodeStep::Ready { decoder, progress }) => {
                    check_version(progress)?;
                    let continuation = decoder.take_terminal().map_err(|error| {
                        GhosttyEngineError::checkpoint(error, progress.consumed)
                    })?;
                    let mut stream = continuation.replay().map_err(|failure| {
                        GhosttyEngineError::checkpoint(failure.error, progress.consumed)
                    })?;
                    stream.on_pty_write({
                        let pty_responses = Rc::clone(&native.pty_responses);
                        move |_terminal, bytes| pty_responses.borrow_mut().push(bytes.to_vec())
                    })?;
                    let trailing_result =
                        remaining(input, progress).map(|remaining| remaining.len());
                    native.decoder = NativeDecoderState::AfterReady(stream);
                    let trailing = trailing_result?;
                    if trailing != 0 {
                        return Err(GhosttyEngineError::TrailingAfterReady { trailing });
                    }
                    return Ok(BootstrapProgress::Ready);
                }
            },
            NativeDecoderState::AfterReady(stream) => {
                native.decoder = NativeDecoderState::AfterReady(stream);
                return Err(GhosttyEngineError::InputAfterReady);
            }
            NativeDecoderState::Finished(terminal) => {
                native.decoder = NativeDecoderState::Finished(terminal);
                return Err(GhosttyEngineError::InputAfterFinish);
            }
            NativeDecoderState::Failed(terminal) => {
                native.decoder = NativeDecoderState::Failed(terminal);
                return Err(GhosttyEngineError::DecoderFailed);
            }
        }
    }
}

fn finish_native(native: &mut NativeReplica) -> Result<BootstrapProgress, GhosttyEngineError> {
    if native.protocol_finished {
        return Err(GhosttyEngineError::InputAfterFinish);
    }
    let state = std::mem::replace(&mut native.decoder, NativeDecoderState::Failed(None));
    match state {
        NativeDecoderState::BeforeReady(decoder) => match decoder.end_input() {
            Err(failure) => Err(GhosttyEngineError::checkpoint(
                failure.error,
                failure.consumed,
            )),
            Ok(_) => Err(GhosttyEngineError::checkpoint(
                SnapshotError::InvalidState,
                0,
            )),
        },
        NativeDecoderState::AfterReady(stream) => {
            native.decoder = NativeDecoderState::AfterReady(stream);
            native.protocol_finished = true;
            Ok(BootstrapProgress::Finished)
        }
        NativeDecoderState::Finished(terminal) => {
            native.decoder = NativeDecoderState::Finished(terminal);
            Err(GhosttyEngineError::InputAfterFinish)
        }
        NativeDecoderState::Failed(terminal) => {
            native.decoder = NativeDecoderState::Failed(terminal);
            Err(GhosttyEngineError::DecoderFailed)
        }
    }
}

fn push_history(
    native: &mut NativeReplica,
    mut input: &[u8],
    ) -> Result<HistoryApplyOutcome, GhosttyEngineError> {
    if !native.protocol_finished {
        return Err(GhosttyEngineError::HistoryBeforePublication);
    }
    if input.is_empty() {
        return match native.decoder {
            NativeDecoderState::AfterReady(_) => Ok(HistoryApplyOutcome {
                progress: BootstrapProgress::Ready,
                retained: true,
            }),
            NativeDecoderState::Finished(_) => Err(GhosttyEngineError::InputAfterFinish),
            NativeDecoderState::BeforeReady(_) => Err(GhosttyEngineError::HistoryBeforePublication),
            NativeDecoderState::Failed(_) => Err(GhosttyEngineError::DecoderFailed),
        };
    }

    let mut retained = true;
    loop {
        let state = std::mem::replace(&mut native.decoder, NativeDecoderState::Failed(None));
        let stream = match state {
            NativeDecoderState::AfterReady(stream) => stream,
            NativeDecoderState::Finished(terminal) => {
                native.decoder = NativeDecoderState::Finished(terminal);
                return Err(GhosttyEngineError::InputAfterFinish);
            }
            NativeDecoderState::BeforeReady(decoder) => {
                native.decoder = NativeDecoderState::BeforeReady(decoder);
                return Err(GhosttyEngineError::HistoryBeforePublication);
            }
            NativeDecoderState::Failed(terminal) => {
                native.decoder = NativeDecoderState::Failed(terminal);
                return Err(GhosttyEngineError::DecoderFailed);
            }
        };
        match stream.push(input) {
            Err(failure) => {
                native.decoder = NativeDecoderState::Failed(Some(failure.terminal));
                return Err(GhosttyEngineError::checkpoint(
                    failure.error,
                    failure.consumed,
                ));
            }
            Ok(AfterReadyStep::NeedInput { decoder, progress })
            | Ok(AfterReadyStep::Progress { decoder, progress })
            | Ok(AfterReadyStep::HistoryBegin {
                decoder, progress, ..
            }) => {
                let version = check_version(progress);
                native.decoder = NativeDecoderState::AfterReady(decoder);
                version?;
                input = remaining(input, progress)?;
                if input.is_empty() {
                    return Ok(HistoryApplyOutcome {
                        progress: BootstrapProgress::Ready,
                        retained,
                    });
                }
            }
            Ok(AfterReadyStep::HistoryPage {
                decoder,
                progress,
                retained: page_retained,
            }) => {
                retained &= page_retained;
                let version = check_version(progress);
                native.decoder = NativeDecoderState::AfterReady(decoder);
                version?;
                input = remaining(input, progress)?;
                if input.is_empty() {
                    return Ok(HistoryApplyOutcome {
                        progress: BootstrapProgress::Ready,
                        retained,
                    });
                }
            }
            Ok(AfterReadyStep::Finish(finished)) => {
                let version = check_version(finished.progress);
                let codec_version = finished.codec_version;
                let trailing_result =
                    remaining(input, finished.progress).map(|remaining| remaining.len());
                native.decoder = NativeDecoderState::Finished(finished.terminal);
                version?;
                if codec_version != CHECKPOINT_VERSION {
                    return Err(GhosttyEngineError::WrongCodecVersion {
                        expected: CHECKPOINT_VERSION,
                        actual: codec_version,
                    });
                }
                let trailing = trailing_result?;
                if trailing != 0 {
                    return Err(GhosttyEngineError::TrailingAfterFinish { trailing });
                }
                return Ok(HistoryApplyOutcome {
                    progress: BootstrapProgress::Finished,
                    retained,
                });
            }
        }
    }
}

fn check_version(progress: DecodeProgress) -> Result<(), GhosttyEngineError> {
    if progress.codec_version != 0 && progress.codec_version != CHECKPOINT_VERSION {
        return Err(GhosttyEngineError::WrongCodecVersion {
            expected: CHECKPOINT_VERSION,
            actual: progress.codec_version,
        });
    }
    Ok(())
}

fn remaining<'a>(
    input: &'a [u8],
    progress: DecodeProgress,
) -> Result<&'a [u8], GhosttyEngineError> {
    if progress.consumed == 0 || progress.consumed > input.len() {
        return Err(GhosttyEngineError::InvalidProgress {
            consumed: progress.consumed,
            available: input.len(),
        });
    }
    Ok(&input[progress.consumed..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use libghostty_vt::snapshot::incremental::{
        CaptureEventKind, CaptureOptions, Error as SnapshotError,
    };

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum RecordKind {
        Other,
        Ready,
        History,
        Finish,
    }

    fn geometry() -> CanonicalGeometry {
        CanonicalGeometry::new(80, 4).expect("valid geometry")
    }

    fn native_profile() -> BootstrapStreamProfile {
        BootstrapStreamProfile::NativeState {
            codec: EngineCodec::LibghosttyCheckpointV2,
        }
    }

    fn capture_records() -> Vec<(RecordKind, Vec<u8>)> {
        let mut source = GhosttyTerminal::new(TerminalOptions {
            cols: 80,
            rows: 4,
            max_scrollback: 100,
        })
        .expect("source terminal");
        for line in 0..40 {
            source.vt_write(format!("line {line}\r\n").as_bytes());
        }
        source.vt_write(b"\x1b]2;checkpoint-title\x07");

        let mut capture = source
            .capture(CaptureOptions::default())
            .expect("incremental capture");
        let mut records = Vec::new();
        loop {
            let required = match capture.next(&mut []) {
                Err(SnapshotError::OutOfSpace {
                    required_bytes,
                    required_rows: 0,
                }) => required_bytes,
                other => panic!("capture size probe: {other:?}"),
            };
            let mut bytes = vec![0; required];
            let event = capture.next(&mut bytes).expect("capture record");
            let kind = match event.kind {
                CaptureEventKind::Ready { .. } => RecordKind::Ready,
                CaptureEventKind::HistoryBegin { .. } | CaptureEventKind::HistoryPage { .. } => {
                    RecordKind::History
                }
                CaptureEventKind::Finish => RecordKind::Finish,
                CaptureEventKind::Record => RecordKind::Other,
            };
            records.push((kind, bytes));
            if kind == RecordKind::Finish {
                return records;
            }
        }
    }

    fn native_adapter() -> GhosttyAdapter {
        GhosttyAdapter::new(BootstrapLimits::default())
    }

    fn split_capture(records: &[(RecordKind, Vec<u8>)]) -> (Vec<u8>, Vec<u8>) {
        let ready = records
            .iter()
            .position(|(kind, _)| *kind == RecordKind::Ready)
            .expect("READY record");
        let bootstrap = records[..=ready]
            .iter()
            .flat_map(|(_, bytes)| bytes.iter().copied())
            .collect();
        let history = records[ready + 1..]
            .iter()
            .flat_map(|(_, bytes)| bytes.iter().copied())
            .collect();
        (bootstrap, history)
    }

    #[test]
    fn synthesized_profiles_write_borrowed_bytes_and_reject_history() {
        for profile in [
            BootstrapStreamProfile::SynthesizedVtRaw,
            BootstrapStreamProfile::SynthesizedVtStateSync,
        ] {
            let mut adapter = native_adapter();
            let mut replica = adapter
                .start_replica(profile, geometry())
                .expect("synth replica");
            let mut effects = EngineEffectBuffer::new();
            assert_eq!(
                adapter
                    .apply_bootstrap_chunk(&mut replica, b"\x1b]2;synth-title\x07", &mut effects)
                    .expect("bootstrap bytes"),
                BootstrapProgress::Pending
            );
            assert_eq!(
                adapter
                    .finish_bootstrap(&mut replica, &mut effects)
                    .expect("protocol finish"),
                BootstrapProgress::Finished
            );
            assert_eq!(
                replica.terminal().expect("synth terminal").title().unwrap(),
                "synth-title"
            );
            assert!(matches!(
                adapter.apply_history_page(&mut replica, b"history", &mut effects),
                Err(GhosttyEngineError::HistoryUnsupported(actual)) if actual == profile
            ));
            assert!(matches!(
                adapter.apply_bootstrap_chunk(&mut replica, b"late", &mut effects),
                Err(GhosttyEngineError::InputAfterFinish)
            ));
        }
    }

    #[test]
    fn synthesized_terminal_queries_emit_exact_pty_write_effects() {
        let mut adapter = native_adapter();
        let mut replica = adapter
            .start_replica(BootstrapStreamProfile::SynthesizedVtRaw, geometry())
            .expect("synth replica");
        let mut effects = EngineEffectBuffer::new();
        adapter
            .apply_bootstrap_chunk(&mut replica, b"\x1b[5n", &mut effects)
            .expect("bootstrap DSR query");
        assert!(matches!(
            effects.as_slice(),
            [EngineEffect::Send(EngineSend::PtyWrite(bytes))] if bytes == b"\x1b[0n"
        ));
        effects.clear();
        adapter
            .finish_bootstrap(&mut replica, &mut effects)
            .expect("publish synthesized terminal");
        adapter
            .apply_output(&mut replica, b"\x1b[5n", &mut effects)
            .expect("live DSR query");
        assert!(matches!(
            effects.as_slice(),
            [
                EngineEffect::Send(EngineSend::PtyWrite(bytes)),
                EngineEffect::Damage(EngineDamage::Full),
            ] if bytes == b"\x1b[0n"
        ));
    }

    #[test]
    fn native_decoder_accepts_arbitrary_fragment_cuts_and_multiple_records() {
        let records = capture_records();
        let (bootstrap, history) = split_capture(&records);
        for width in [1, 2, 3, 7, 31, bootstrap.len().max(history.len())] {
            let mut adapter = native_adapter();
            let mut replica = adapter
                .start_replica(native_profile(), geometry())
                .expect("native replica");
            let mut effects = EngineEffectBuffer::new();
            let mut bootstrap_progress = BootstrapProgress::Pending;
            for fragment in bootstrap.chunks(width) {
                bootstrap_progress = adapter
                    .apply_bootstrap_chunk(&mut replica, fragment, &mut effects)
                    .expect("arbitrary bootstrap fragment");
            }
            assert_eq!(bootstrap_progress, BootstrapProgress::Ready);
            assert_eq!(
                adapter
                    .finish_bootstrap(&mut replica, &mut effects)
                    .expect("protocol READY"),
                BootstrapProgress::Finished
            );
            let mut history_progress = BootstrapProgress::Ready;
            for fragment in history.chunks(width) {
                history_progress = adapter
                    .apply_history_page(&mut replica, fragment, &mut effects)
                    .expect("arbitrary history fragment")
                    .progress;
            }
            assert_eq!(history_progress, BootstrapProgress::Finished);
            assert_eq!(
                replica
                    .terminal()
                    .expect("finished terminal")
                    .title()
                    .unwrap(),
                "checkpoint-title"
            );
        }
    }

    #[test]
    fn publication_requires_authenticated_ready_and_one_shot_continuation_replay() {
        let records = capture_records();
        let ready = records
            .iter()
            .position(|(kind, _)| *kind == RecordKind::Ready)
            .expect("READY record");
        let mut adapter = native_adapter();
        let mut replica = adapter
            .start_replica(native_profile(), geometry())
            .expect("native replica");
        let mut effects = EngineEffectBuffer::new();
        assert!(matches!(
            adapter.apply_output(&mut replica, b"too early", &mut effects),
            Err(GhosttyEngineError::LiveOutputBeforeReady)
        ));
        for (_, record) in &records[..ready] {
            assert_eq!(
                adapter
                    .apply_bootstrap_chunk(&mut replica, record, &mut effects)
                    .expect("pre-READY record"),
                BootstrapProgress::Pending
            );
            assert!(replica.terminal().is_none());
        }
        assert_eq!(
            adapter
                .apply_bootstrap_chunk(&mut replica, &records[ready].1, &mut effects)
                .expect("READY record"),
            BootstrapProgress::Ready
        );
        assert!(replica.terminal().is_some());
        assert!(matches!(
            adapter.apply_output(&mut replica, b"not published", &mut effects),
            Err(GhosttyEngineError::LiveOutputBeforeReady)
        ));
        assert!(matches!(
            adapter.apply_bootstrap_chunk(&mut replica, b"late chunk", &mut effects),
            Err(GhosttyEngineError::InputAfterReady)
        ));
        assert_eq!(
            adapter
                .finish_bootstrap(&mut replica, &mut effects)
                .expect("protocol READY validates native READY"),
            BootstrapProgress::Finished
        );
        adapter
            .apply_output(&mut replica, b"\x1b]2;published-live\x07", &mut effects)
            .expect("published live output");
        assert_eq!(
            replica.terminal().unwrap().title().unwrap(),
            "published-live"
        );
        effects.clear();
        adapter
            .apply_output(&mut replica, b"\x1b[5n", &mut effects)
            .expect("native DSR query after READY publication");
        assert!(matches!(
            effects.as_slice(),
            [
                EngineEffect::Send(EngineSend::PtyWrite(bytes)),
                EngineEffect::Damage(EngineDamage::Full),
            ] if bytes == b"\x1b[0n"
        ));
    }

    #[test]
    fn live_output_is_applied_between_later_history_pages() {
        let records = capture_records();
        let ready = records
            .iter()
            .position(|(kind, _)| *kind == RecordKind::Ready)
            .expect("READY record");
        assert!(records.iter().any(|(kind, _)| *kind == RecordKind::History));
        let mut adapter = native_adapter();
        let mut replica = adapter
            .start_replica(native_profile(), geometry())
            .expect("native replica");
        let mut effects = EngineEffectBuffer::new();
        for (_, record) in &records[..=ready] {
            adapter
                .apply_bootstrap_chunk(&mut replica, record, &mut effects)
                .expect("checkpoint bootstrap record");
        }
        adapter
            .finish_bootstrap(&mut replica, &mut effects)
            .expect("protocol READY");

        let mut wrote_live = false;
        for (kind, record) in &records[ready + 1..] {
            adapter
                .apply_history_page(&mut replica, record, &mut effects)
                .expect("history page");
            if !wrote_live && *kind == RecordKind::History {
                adapter
                    .apply_output(
                        &mut replica,
                        b"\x1b]2;live-during-history\x07",
                        &mut effects,
                    )
                    .expect("live output between history pages");
                wrote_live = true;
            }
        }
        assert!(wrote_live);
        assert_eq!(
            replica
                .terminal()
                .expect("finished terminal")
                .title()
                .unwrap(),
            "live-during-history"
        );
    }

    #[test]
    fn native_truncation_corruption_and_limits_are_typed() {
        let records = capture_records();
        let (bootstrap, _) = split_capture(&records);
        let mut effects = EngineEffectBuffer::new();

        let mut adapter = native_adapter();
        let mut early = adapter
            .start_replica(native_profile(), geometry())
            .expect("native replica");
        assert!(matches!(
            adapter.finish_bootstrap(&mut early, &mut effects),
            Err(GhosttyEngineError::Checkpoint {
                source: SnapshotError::Truncated,
                ..
            })
        ));

        let mut adapter = native_adapter();
        let mut truncated = adapter
            .start_replica(native_profile(), geometry())
            .expect("native replica");
        adapter
            .apply_bootstrap_chunk(
                &mut truncated,
                &bootstrap[..bootstrap.len() - 1],
                &mut effects,
            )
            .expect("truncated fragment is buffered");
        assert!(matches!(
            adapter.finish_bootstrap(&mut truncated, &mut effects),
            Err(GhosttyEngineError::Checkpoint {
                source: SnapshotError::Truncated,
                ..
            })
        ));

        let mut corrupt = bootstrap.clone();
        corrupt[0] ^= 0xff;
        let mut adapter = native_adapter();
        let mut replica = adapter
            .start_replica(native_profile(), geometry())
            .expect("native replica");
        assert!(matches!(
            adapter.apply_bootstrap_chunk(&mut replica, &corrupt, &mut effects),
            Err(GhosttyEngineError::Checkpoint { .. })
                | Err(GhosttyEngineError::WrongCodecVersion { .. })
        ));

        let tiny = BootstrapLimits::new(1, 1).expect("tiny valid limits");
        let mut adapter = GhosttyAdapter::new(tiny);
        let mut replica = adapter
            .start_replica(native_profile(), geometry())
            .expect("native replica");
        assert!(matches!(
            adapter.apply_bootstrap_chunk(&mut replica, &bootstrap, &mut effects),
            Err(GhosttyEngineError::PayloadLimitExceeded { limit: 1, .. })
        ));

        let mut adapter = GhosttyAdapter::new(tiny);
        let mut replica = adapter
            .start_replica(native_profile(), geometry())
            .expect("native replica");
        let mut limit_error = None;
        for byte in &bootstrap {
            if let Err(error) = adapter.apply_bootstrap_chunk(
                &mut replica,
                std::slice::from_ref(byte),
                &mut effects,
            ) {
                limit_error = Some(error);
                break;
            }
        }
        assert!(matches!(
            limit_error,
            Some(GhosttyEngineError::Checkpoint {
                source: SnapshotError::LimitExceeded,
                ..
            })
        ));
    }

    #[test]
    fn native_history_finish_rejects_trailing_and_post_finish_pages() {
        let records = capture_records();
        let (bootstrap, mut history) = split_capture(&records);
        history.extend_from_slice(b"trailing");
        let mut adapter = native_adapter();
        let mut replica = adapter
            .start_replica(native_profile(), geometry())
            .expect("native replica");
        let mut effects = EngineEffectBuffer::new();
        adapter
            .apply_bootstrap_chunk(&mut replica, &bootstrap, &mut effects)
            .expect("checkpoint through READY");
        adapter
            .finish_bootstrap(&mut replica, &mut effects)
            .expect("protocol READY");
        assert!(matches!(
            adapter.apply_history_page(&mut replica, &history, &mut effects),
            Err(GhosttyEngineError::TrailingAfterFinish { trailing: 8 })
        ));
        assert!(
            replica.terminal().is_some(),
            "a rejected post-READY record must leave the last terminal renderable"
        );
        assert!(matches!(
            adapter.apply_history_page(&mut replica, b"again", &mut effects),
            Err(GhosttyEngineError::InputAfterFinish)
        ));
    }

    #[test]
    fn native_profile_is_rejected_when_the_runtime_contract_is_unavailable() {
        let mut adapter = native_adapter();
        adapter.native_available = false;
        assert!(matches!(
            adapter.start_replica(native_profile(), geometry()),
            Err(GhosttyEngineError::UnsupportedProfile(
                BootstrapStreamProfile::NativeState {
                    codec: EngineCodec::LibghosttyCheckpointV2,
                }
            ))
        ));
    }

    #[test]
    fn native_capabilities_require_the_exact_runtime_contract() {
        let limits = BootstrapLimits::default();
        let capabilities = native_bootstrap_capabilities(limits);
        assert!(
            capabilities
                .native_codecs
                .contains(EngineCodec::LibghosttyCheckpointV2)
        );
        assert_eq!(
            capabilities.native_features,
            EngineFeatureSet::required_native()
        );
        assert_eq!(capabilities.limits, limits);
    }

    #[test]
    fn native_projection_search_and_anchors_remain_engine_owned() {
        let records = capture_records();
        let (bootstrap, history) = split_capture(&records);
        let mut adapter = native_adapter();
        let mut replica = adapter
            .start_replica(native_profile(), geometry())
            .expect("native replica");
        let mut effects = EngineEffectBuffer::new();
        adapter
            .apply_bootstrap_chunk(&mut replica, &bootstrap, &mut effects)
            .expect("checkpoint through READY");
        adapter
            .finish_bootstrap(&mut replica, &mut effects)
            .expect("protocol READY");
        assert_eq!(
            adapter
                .apply_history_page(&mut replica, &history, &mut effects)
                .expect("complete history")
                .progress,
            BootstrapProgress::Finished
        );

        let projection = adapter
            .project_history(&mut replica, 12, EngineProjectionOrigin::Tail, 3)
            .expect("bounded projection");
        assert_eq!(projection.width, 12);
        assert_eq!(projection.rows.len(), 3);
        assert!(projection.has_older);
        let narrow = adapter
            .project_history(&mut replica, 1, EngineProjectionOrigin::Tail, 3)
            .expect("minimum-width projection");
        assert_eq!(narrow.width, 2);
        assert!(narrow
            .rows
            .iter()
            .all(|row| row.text.chars().count() <= usize::from(narrow.width)));

        let found = adapter
            .search_loaded(&mut replica, "line 10", 1)
            .expect("engine search");
        assert_eq!(found.len(), 1);
        let selection = EngineDocumentSelection {
            start: found[0].start,
            end: found[0].end,
            rectangle: false,
        };
        assert_eq!(
            adapter
                .format_selection(&replica, selection)
                .expect("engine formatting")
                .as_deref(),
            Some("line 10")
        );
        let distance_before = adapter
            .history_anchor_tail_distance(&replica, found[0].start)
            .expect("anchor distance")
            .expect("live anchor");
        adapter
            .apply_output(&mut replica, b"newer-live-output\r\n", &mut effects)
            .expect("live append");
        let distance_after = adapter
            .history_anchor_tail_distance(&replica, found[0].start)
            .expect("anchor distance")
            .expect("retained anchor");
        assert_eq!(distance_after, distance_before + 1);
        assert!(
            adapter
                .document_anchor_point(&replica, found[0].start, DocumentSpace::History)
                .expect("tracked point")
                .is_some()
        );

        let other = adapter
            .start_replica(BootstrapStreamProfile::SynthesizedVtRaw, geometry())
            .expect("other replica");
        assert_eq!(
            adapter
                .document_anchor_point(&other, found[0].start, DocumentSpace::History)
                .expect("wrong replica lookup"),
            None
        );
        adapter.release_document_anchor(&mut replica, found[0].start);
        adapter.release_document_anchor(&mut replica, found[0].end);
        assert!(replica.anchors.is_empty());
        assert!(other.anchors.is_empty());
    }

    #[test]
    fn projection_extends_anchor_to_complete_soft_wrapped_line() {
        let mut adapter = native_adapter();
        let mut replica = adapter
            .start_replica(BootstrapStreamProfile::SynthesizedVtRaw, geometry())
            .expect("synthesized replica");
        let mut effects = EngineEffectBuffer::new();
        let mut output = vec![b'x'; 320];
        output.extend_from_slice(b"\r\none\r\ntwo\r\nthree\r\nfour\r\n");
        adapter
            .apply_output(&mut replica, &output, &mut effects)
            .expect("wrapped output");
        let anchor = adapter
            .track_document_anchor(
                &mut replica,
                DocumentPoint {
                    space: DocumentSpace::History,
                    x: 1,
                    y: 2,
                },
            )
            .expect("anchor inside wrapped line");
        let projection = adapter
            .project_history(&mut replica, 320, EngineProjectionOrigin::Anchor(anchor), 1)
            .expect("complete logical line");
        assert_eq!(projection.width, 320);
        assert_eq!(projection.rows.len(), 1);
        assert_eq!(projection.rows[0].text, "x".repeat(320));
        assert!(!projection.has_older);
    }

    fn projected(text: &str, width: usize) -> ProjectedCell {
        ProjectedCell {
            text: text.to_owned(),
            width,
            empty_default: false,
        }
    }

    #[test]
    fn projection_trims_only_trailing_default_cells() {
        let rows = rewrap_history_rows(
            vec![(
                vec![
                    projected("a", 1),
                    ProjectedCell {
                        text: String::new(),
                        width: 1,
                        empty_default: true,
                    },
                    projected("b", 1),
                    ProjectedCell {
                        text: String::new(),
                        width: 1,
                        empty_default: true,
                    },
                ],
                false,
            )],
            8,
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text, "a b");
        assert!(!rows[0].soft_wrapped);
    }

    #[test]
    fn projection_joins_soft_rows_and_keeps_wide_graphemes_atomic() {
        let rows = rewrap_history_rows(
            vec![
                (
                    vec![
                        projected("a", 1),
                        projected("e\u{301}", 1),
                        projected("界", 2),
                    ],
                    true,
                ),
                (vec![projected("z", 1)], false),
            ],
            3,
        );
        assert_eq!(
            rows.iter()
                .map(|row| (row.text.as_str(), row.soft_wrapped))
                .collect::<Vec<_>>(),
            vec![("ae\u{301}", true), ("界z", false)]
        );
    }

    #[test]
    fn minimum_projection_width_keeps_leading_cjk_within_row() {
        let rows = rewrap_history_rows(
            vec![(vec![projected("界", 2), projected("a", 1)], false)],
            2,
        );
        assert_eq!(
            rows.iter()
                .map(|row| (row.text.as_str(), row.soft_wrapped))
                .collect::<Vec<_>>(),
            vec![("界", true), ("a", false)]
        );
    }
}
