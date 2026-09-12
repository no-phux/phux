//! Native libghostty implementation of the session-kernel engine boundary.
//!
//! Native bootstrap is the official GHOSTSNP snapshot codec: the server sends
//! the READY prefix, this adapter reconstructs a live terminal, then history
//! suffix bytes are pulled and applied with [`IncrementalDecoder::next`].

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
    marker::PhantomData,
    rc::Rc,
};

use libghostty_vt::{
    Error as SnapshotError, Terminal as GhosttyTerminal,
    screen::{CellContentTag, CellWide, TrackedGridRef},
    selection::{FormatOptions, Selection},
    snapshot::Decoder,
    terminal::{Point, PointCoordinate, PointSpace, ScrollViewport},
};
use phux_protocol::{
    BootstrapCapabilities, BootstrapLimits, BootstrapStreamProfile, EngineCodec, EngineFeatureSet,
};
use thiserror::Error;

use super::{
    BootstrapProgress, CanonicalGeometry, DocumentPoint, DocumentSpace, EngineAdapter,
    EngineDamage, EngineDocumentAdapter, EngineDocumentSelection, EngineEffect, EngineEffectBuffer,
    EngineHistoryProjection, EnginePresentationAdapter, EngineProjectionOrigin,
    EngineProjectionRow, EngineSearchMatch, EngineSend, HistoryApplyOutcome,
};
use crate::history::DocumentAnchorId;

const SYNTH_SCROLLBACK_ROWS: usize = 10_000;
const CONTINUATION_LIMIT: usize = 64 * 1024 * 1024;

/// Return the client bootstrap capabilities supported by the linked engine.
///
/// Native v2 is advertised when the official snapshot codec can encode a
/// terminal. Probe failure leaves both synthesized compatibility profiles.
#[must_use]
pub fn native_bootstrap_capabilities(limits: BootstrapLimits) -> BootstrapCapabilities {
    let capabilities = BootstrapCapabilities::new().with_limits(limits);
    if official_snapshot_available() {
        capabilities.with_native(
            EngineCodec::LibghosttyCheckpointV2,
            EngineFeatureSet::required_native(),
        )
    } else {
        capabilities
    }
}

fn official_snapshot_available() -> bool {
    let Ok(mut terminal) = GhosttyTerminal::new(2, 2) else {
        return false;
    };
    if terminal
        .set_continuation_max_bytes(CONTINUATION_LIMIT)
        .is_err()
    {
        return false;
    }
    terminal.vt_write(b"ok");
    let mut encoded = Vec::new();
    terminal.encode_snapshot(&mut encoded).is_ok() && !encoded.is_empty()
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
    native_available: bool,
    next_anchor_id: u64,
    search_case_sensitive: bool,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl GhosttyAdapter {
    /// Construct an adapter for one connection's negotiated bootstrap limits.
    #[must_use]
    pub fn new(limits: BootstrapLimits) -> Self {
        Self {
            limits,
            native_available: official_snapshot_available(),
            next_anchor_id: 1,
            search_case_sensitive: true,
            _not_send_or_sync: PhantomData,
        }
    }

    /// Negotiated payload limits enforced by this adapter.
    #[must_use]
    pub const fn limits(&self) -> BootstrapLimits {
        self.limits
    }

    /// Configure loaded-history matching. Insensitive matching folds ASCII only,
    /// matching Ghostty's native scrollback search without altering text/anchors.
    pub const fn set_search_case_sensitive(&mut self, case_sensitive: bool) {
        self.search_case_sensitive = case_sensitive;
    }

    /// Convert scanned history ranges into tracked anchor pairs.
    ///
    /// Any failure releases every anchor already tracked for this search, so a
    /// partially tracked result never leaks tracked grid refs into the replica.
    fn track_search_matches(
        &mut self,
        replica: &mut GhosttyReplica,
        ranges: Vec<(DocumentPoint, DocumentPoint)>,
    ) -> Result<Vec<EngineSearchMatch>, GhosttyEngineError> {
        let mut matches: Vec<EngineSearchMatch> = Vec::with_capacity(ranges.len());
        for (start, end) in ranges {
            match self.track_match_range(replica, start, end) {
                Ok(found) => matches.push(found),
                Err(error) => {
                    self.release_matches(replica, std::mem::take(&mut matches));
                    return Err(error);
                }
            }
        }
        Ok(matches)
    }

    /// Track both endpoints of one match, releasing the start if the end fails.
    fn track_match_range(
        &mut self,
        replica: &mut GhosttyReplica,
        start: DocumentPoint,
        end: DocumentPoint,
    ) -> Result<EngineSearchMatch, GhosttyEngineError> {
        let start = self.track_document_anchor(replica, start)?;
        let end = match self.track_document_anchor(replica, end) {
            Ok(anchor) => anchor,
            Err(error) => {
                self.release_document_anchor(replica, start);
                return Err(error);
            }
        };
        Ok(EngineSearchMatch { start, end })
    }

    /// Release both anchors of every already-tracked match.
    fn release_matches(&mut self, replica: &mut GhosttyReplica, matches: Vec<EngineSearchMatch>) {
        for found in matches {
            self.release_document_anchor(replica, found.start);
            self.release_document_anchor(replica, found.end);
        }
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
    bell_pending: Rc<Cell<bool>>,
    profile: BootstrapStreamProfile,
    reported_title: Option<String>,
    anchors: HashMap<DocumentAnchorId, TrackedGridRef>,
    state: ReplicaState,
    history_max_bytes: Option<usize>,
    history_max_lines: Option<usize>,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl GhosttyReplica {
    fn publish_title(
        &mut self,
        effects: &mut EngineEffectBuffer,
    ) -> Result<(), GhosttyEngineError> {
        let Some(terminal) = self.terminal() else {
            return Ok(());
        };
        let title = terminal.title()?;
        if Some(title) == self.reported_title.as_deref() {
            return Ok(());
        }
        let title = title.to_owned();
        self.reported_title = Some(title.clone());
        effects.push(EngineEffect::Status(super::EngineStatus::Title(title)));
        Ok(())
    }

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
    pub const fn terminal(&self) -> Option<&GhosttyTerminal<'static, 'static>> {
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
                NativeDecoderState::Finished(terminal) => {
                    terminal.set_scrollback_max_bytes(max)?;
                }
                NativeDecoderState::Collecting => {}
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
                NativeDecoderState::Finished(terminal) => {
                    terminal.set_scrollback_max_lines(max)?;
                }
                NativeDecoderState::Collecting => {}
            },
        }
        Ok(())
    }

    /// Apply a client-local viewport scroll without exposing mutable terminal ownership.
    pub fn scroll_viewport(&mut self, scroll: ScrollViewport) -> Result<(), GhosttyEngineError> {
        match &mut self.state {
            ReplicaState::Synthesized { terminal, .. } => terminal.scroll_viewport(scroll),
            ReplicaState::Native(native) => match &mut native.decoder {
                NativeDecoderState::Finished(terminal) => terminal.scroll_viewport(scroll),
                NativeDecoderState::Collecting => {
                    return Err(GhosttyEngineError::LiveOutputBeforeReady);
                }
            },
        }
        Ok(())
    }

    /// Mutate screen/history directly; the live parser may be mid-sequence.
    fn clear_presentation(&mut self) -> Result<(), GhosttyEngineError> {
        match &mut self.state {
            ReplicaState::Synthesized {
                terminal,
                protocol_finished,
                ..
            } => {
                if !*protocol_finished {
                    return Err(GhosttyEngineError::LiveOutputBeforeReady);
                }
                clear_terminal_presentation(terminal);
            }
            ReplicaState::Native(native) => native.clear_presentation()?,
        }
        Ok(())
    }
}

fn clear_terminal_presentation(terminal: &mut GhosttyTerminal<'_, '_>) {
    // Drive CUP/ED/selection on the grid directly. vt_write of those
    // sequences would complete a pending CSI/OSC/DCS or flush U+FFFD for
    // unfinished UTF-8; decoded snapshots also disable continuation tracking
    // so those bytes cannot be extracted and replayed after RIS.
    terminal.clear_presentation();
}

type PtyResponses = Rc<RefCell<Vec<Vec<u8>>>>;

fn drain_pty_responses(responses: &PtyResponses, effects: &mut EngineEffectBuffer) {
    for bytes in responses.take() {
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
struct SnapshotFeed {
    data: Vec<u8>,
}

#[derive(Debug)]
struct NativeReplica {
    bell_pending: Rc<Cell<bool>>,
    /// Decoder first so it drops before the heap `feed` its callbacks point at.
    decoder: NativeDecoderState,
    feed: Box<SnapshotFeed>,
    protocol_finished: bool,
    pty_responses: PtyResponses,
}

impl NativeReplica {
    fn clear_presentation(&mut self) -> Result<(), GhosttyEngineError> {
        if !self.protocol_finished {
            return Err(GhosttyEngineError::LiveOutputBeforeReady);
        }
        match &mut self.decoder {
            NativeDecoderState::Finished(terminal) => {
                clear_terminal_presentation(terminal);
            }
            NativeDecoderState::Collecting => {
                return Err(GhosttyEngineError::LiveOutputBeforeReady);
            }
        }
        Ok(())
    }

    const fn terminal(&self) -> Option<&GhosttyTerminal<'static, 'static>> {
        match &self.decoder {
            NativeDecoderState::Collecting => None,
            NativeDecoderState::Finished(terminal) => Some(terminal),
        }
    }
}

/// Decoder first so it drops before the heap `feed` it points at.
#[derive(Debug)]
enum NativeDecoderState {
    Collecting,
    Finished(GhosttyTerminal<'static, 'static>),
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
    const fn checkpoint(source: SnapshotError, consumed: usize) -> Self {
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
        let bell_pending = Rc::new(Cell::new(false));
        let state = match profile {
            BootstrapStreamProfile::SynthesizedVtRaw
            | BootstrapStreamProfile::SynthesizedVtStateSync => {
                let pty_responses: PtyResponses = Rc::new(RefCell::new(Vec::new()));
                let mut terminal = GhosttyTerminal::new(geometry.cols, geometry.rows)?;
                terminal.set_scrollback_max_lines(Some(SYNTH_SCROLLBACK_ROWS))?;
                let _ = terminal.set_continuation_max_bytes(CONTINUATION_LIMIT);
                terminal.on_pty_write({
                    let pty_responses = Rc::clone(&pty_responses);
                    move |_terminal, bytes| pty_responses.borrow_mut().push(bytes.to_vec())
                })?;
                terminal.on_bell({
                    let bell_pending = Rc::clone(&bell_pending);
                    move |_terminal| bell_pending.set(true)
                })?;
                ReplicaState::Synthesized {
                    terminal,
                    protocol_finished: false,
                    pty_responses,
                }
            }
            BootstrapStreamProfile::NativeState {
                codec: EngineCodec::LibghosttyCheckpointV2,
            } if self.native_available => ReplicaState::Native(NativeReplica {
                bell_pending: Rc::clone(&bell_pending),
                feed: Box::new(SnapshotFeed { data: Vec::new() }),
                decoder: NativeDecoderState::Collecting,
                protocol_finished: false,
                pty_responses: Rc::new(RefCell::new(Vec::new())),
            }),
            _ => return Err(GhosttyEngineError::UnsupportedProfile(profile)),
        };
        Ok(GhosttyReplica {
            bell_pending,
            reported_title: None,
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
        replica.publish_title(effects)?;
        // Synthesized bootstrap bytes are history, not new attention events.
        replica.bell_pending.set(false);
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
                    NativeDecoderState::Collecting => {
                        return Err(GhosttyEngineError::LiveOutputBeforeReady);
                    }
                    NativeDecoderState::Finished(terminal) => terminal.vt_write(payload),
                }
                &native.pty_responses
            }
        };
        drain_pty_responses(pty_responses, effects);
        replica.publish_title(effects)?;
        if replica.bell_pending.replace(false) {
            effects.push(EngineEffect::Status(super::EngineStatus::Bell));
        }
        effects.push(EngineEffect::Damage(EngineDamage::Full));
        Ok(())
    }
}

impl EnginePresentationAdapter for GhosttyAdapter {
    fn clear_presentation(&mut self, replica: &mut Self::Replica) -> Result<(), Self::Error> {
        replica.clear_presentation()?;
        self.clear_document_state(replica);
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
            return Ok(empty_projection(width, false));
        }
        let history_rows = terminal.scrollback_rows()?;
        let physical_limit = max_rows.saturating_add(1);
        let Some(window) = history_window(replica, terminal, origin, history_rows, physical_limit)?
        else {
            return Ok(empty_projection(width, true));
        };
        let source = engine_history_rows(terminal, window.start, window.end)?;
        let mut rows = rewrap_history_rows(source, width);
        let trimmed_older = trim_projection_rows(&mut rows, max_rows, window.tail);
        Ok(EngineHistoryProjection {
            width,
            rows,
            has_older: window.start > 0 || trimmed_older,
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
        let ranges = {
            let terminal = replica
                .terminal()
                .ok_or(GhosttyEngineError::LiveOutputBeforeReady)?;
            scan_history_for_needle(terminal, needle, max_matches, self.search_case_sensitive)?
        };
        self.track_search_matches(replica, ranges)
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
        let formatted = terminal.format_selection_alloc(
            None,
            FormatOptions::new()
                .with_selection(&selection)
                .with_unwrap(true)
                .with_trim(true),
        )?;
        Ok(formatted.map(|bytes| String::from_utf8_lossy(&bytes).into_owned()))
    }
}

/// Rolling window of scalars compared against the search needle.
struct NeedleScan<'needle> {
    needle: &'needle [char],
    max_matches: usize,
    window: VecDeque<(char, DocumentPoint)>,
    ranges: Vec<(DocumentPoint, DocumentPoint)>,
    case_sensitive: bool,
}

impl<'needle> NeedleScan<'needle> {
    fn new(needle: &'needle [char], max_matches: usize, case_sensitive: bool) -> Self {
        Self {
            needle,
            max_matches,
            window: VecDeque::with_capacity(needle.len()),
            ranges: Vec::new(),
            case_sensitive,
        }
    }

    /// Feed one scalar; reports whether every requested match has been collected.
    fn push(&mut self, scalar: char, point: DocumentPoint) -> bool {
        self.window.push_back((scalar, point));
        if self.window.len() > self.needle.len() {
            self.window.pop_front();
        }
        if self.window.len() == self.needle.len()
            && self
                .window
                .iter()
                .zip(self.needle)
                .all(|((value, _), expected)| self.scalar_matches(*value, *expected))
        {
            let (Some((_, start)), Some((_, end))) = (self.window.front(), self.window.back())
            else {
                unreachable!("a matched search window is non-empty");
            };
            self.ranges.push((*start, *end));
        }
        self.ranges.len() == self.max_matches
    }

    fn into_ranges(self) -> Vec<(DocumentPoint, DocumentPoint)> {
        self.ranges
    }

    const fn scalar_matches(&self, value: char, expected: char) -> bool {
        if self.case_sensitive {
            value == expected
        } else {
            value.eq_ignore_ascii_case(&expected)
        }
    }
}

/// A history-space document point at one grid coordinate.
const fn history_point(x: u16, y: u32) -> DocumentPoint {
    DocumentPoint {
        space: DocumentSpace::History,
        x,
        y,
    }
}

/// Read one history row's soft-wrap flag, or `None` once history runs out.
fn history_row_wrapped_at(
    terminal: &GhosttyTerminal<'_, '_>,
    y: u32,
) -> Result<Option<bool>, GhosttyEngineError> {
    let first = match terminal.grid_ref(Point::History(PointCoordinate { x: 0, y })) {
        Ok(value) => value,
        Err(libghostty_vt::Error::InvalidValue) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    Ok(Some(first.row()?.is_wrapped()?))
}

/// Feed every scalar of one history row to the scan; reports whether it filled.
fn scan_history_row(
    terminal: &GhosttyTerminal<'_, '_>,
    scan: &mut NeedleScan<'_>,
    cols: u16,
    y: u32,
) -> Result<bool, GhosttyEngineError> {
    for x in 0..cols {
        let grid_ref = terminal.grid_ref(Point::History(PointCoordinate { x, y }))?;
        for scalar in grid_ref_graphemes(&grid_ref)? {
            if scan.push(scalar, history_point(x, y)) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Scan loaded history oldest-first for `needle`, stopping at `max_matches` ranges.
///
/// A row that is not soft-wrapped ends a logical line, so a newline scalar is fed
/// at its end; the needle therefore matches across the engine's own row wrapping.
fn scan_history_for_needle(
    terminal: &GhosttyTerminal<'_, '_>,
    needle: &str,
    max_matches: usize,
    case_sensitive: bool,
) -> Result<Vec<(DocumentPoint, DocumentPoint)>, GhosttyEngineError> {
    let needle: Vec<char> = needle.chars().collect();
    let mut scan = NeedleScan::new(&needle, max_matches, case_sensitive);
    let cols = terminal.cols()?;
    let mut y = 0_u32;
    while let Some(wrapped) = history_row_wrapped_at(terminal, y)? {
        if scan_history_row(terminal, &mut scan, cols, y)? {
            break;
        }
        if !wrapped && scan.push('\n', history_point(cols.saturating_sub(1), y)) {
            break;
        }
        let Some(next) = y.checked_add(1) else {
            break;
        };
        y = next;
    }
    Ok(scan.into_ranges())
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

const fn to_ghostty_point(point: DocumentPoint) -> Point {
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

const fn to_ghostty_space(space: DocumentSpace) -> PointSpace {
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
        rows.push(engine_history_row(terminal, row, cols)?);
    }
    Ok(rows)
}

/// Project one physical history row into its cells plus its soft-wrap flag.
fn engine_history_row(
    terminal: &GhosttyTerminal<'_, '_>,
    row: usize,
    cols: u16,
) -> Result<(Vec<ProjectedCell>, bool), GhosttyEngineError> {
    let y = u32::try_from(row).unwrap_or(u32::MAX);
    let first = terminal.grid_ref(Point::History(PointCoordinate { x: 0, y }))?;
    let wrapped = first.row()?.is_wrapped()?;
    let mut cells = Vec::with_capacity(usize::from(cols));
    for x in 0..cols {
        if let Some(cell) = project_history_cell(terminal, x, y)? {
            cells.push(cell);
        }
    }
    Ok((cells, wrapped))
}

/// Project one history cell, or `None` for a wide-cell spacer that owns no text.
fn project_history_cell(
    terminal: &GhosttyTerminal<'_, '_>,
    x: u16,
    y: u32,
) -> Result<Option<ProjectedCell>, GhosttyEngineError> {
    let grid_ref = terminal.grid_ref(Point::History(PointCoordinate { x, y }))?;
    let cell = grid_ref.cell()?;
    let wide = cell.wide()?;
    if matches!(wide, CellWide::SpacerTail | CellWide::SpacerHead) {
        return Ok(None);
    }
    let text: String = grid_ref_graphemes(&grid_ref)?.into_iter().collect();
    Ok(Some(ProjectedCell {
        empty_default: is_empty_default_cell(cell, &text)?,
        text,
        width: if wide == CellWide::Wide { 2 } else { 1 },
    }))
}

/// Whether a cell carries nothing but untouched default codepoint content.
fn is_empty_default_cell(
    cell: libghostty_vt::screen::Cell,
    text: &str,
) -> Result<bool, GhosttyEngineError> {
    Ok(text.is_empty()
        && cell.codepoint()? == 0
        && cell.content_tag()? == CellContentTag::Codepoint)
}

/// Physical history rows to project, and whether the tail pinned the window.
#[derive(Debug, Clone, Copy)]
struct HistoryWindow {
    start: usize,
    end: usize,
    tail: bool,
}

/// An empty projection at the caller's width.
const fn empty_projection(width: u16, has_older: bool) -> EngineHistoryProjection {
    EngineHistoryProjection {
        width,
        rows: Vec::new(),
        has_older,
    }
}

/// Resolve the physical history rows one projection origin selects.
///
/// Returns `None` when an anchored origin no longer resolves to a history
/// point; the caller then reports an empty projection that still has older rows.
fn history_window(
    replica: &GhosttyReplica,
    terminal: &GhosttyTerminal<'_, '_>,
    origin: EngineProjectionOrigin,
    history_rows: usize,
    physical_limit: usize,
) -> Result<Option<HistoryWindow>, GhosttyEngineError> {
    let Some((start, tail)) = window_origin(replica, origin, history_rows, physical_limit)? else {
        return Ok(None);
    };
    let start = extend_start_to_logical_line(terminal, start)?;
    let end = extend_end_to_logical_line(terminal, start, history_rows, physical_limit)?;
    Ok(Some(HistoryWindow { start, end, tail }))
}

/// First physical row for one origin, and whether that origin is the tail.
fn window_origin(
    replica: &GhosttyReplica,
    origin: EngineProjectionOrigin,
    history_rows: usize,
    physical_limit: usize,
) -> Result<Option<(usize, bool)>, GhosttyEngineError> {
    match origin {
        EngineProjectionOrigin::Tail => {
            Ok(Some((history_rows.saturating_sub(physical_limit), true)))
        }
        EngineProjectionOrigin::Anchor(anchor) => {
            let Some(anchor) = replica.anchors.get(&anchor) else {
                return Ok(None);
            };
            let Some(point) = anchor.point(PointSpace::History)? else {
                return Ok(None);
            };
            Ok(Some((point.y as usize, false)))
        }
    }
}

/// Walk the window start back over soft-wrapped rows onto a logical line boundary.
fn extend_start_to_logical_line(
    terminal: &GhosttyTerminal<'_, '_>,
    mut start: usize,
) -> Result<usize, GhosttyEngineError> {
    while start > 0 && history_row_wrapped(terminal, start - 1)? {
        start -= 1;
    }
    Ok(start)
}

/// Extend the window end past soft-wrapped rows so it closes on a logical line.
fn extend_end_to_logical_line(
    terminal: &GhosttyTerminal<'_, '_>,
    start: usize,
    history_rows: usize,
    physical_limit: usize,
) -> Result<usize, GhosttyEngineError> {
    let mut end = history_rows.min(start.saturating_add(physical_limit));
    while end < history_rows && end > start && history_row_wrapped(terminal, end - 1)? {
        end += 1;
    }
    Ok(end)
}

/// Trim a rewrapped projection to `max_rows`, dropping the end the origin did not pin.
///
/// Returns whether trimming dropped rows off the front, which leaves older rows behind.
fn trim_projection_rows(rows: &mut Vec<EngineProjectionRow>, max_rows: usize, tail: bool) -> bool {
    if rows.len() <= max_rows {
        return false;
    }
    if tail {
        rows.drain(..rows.len() - max_rows);
        return true;
    }
    rows.truncate(max_rows);
    false
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
    for cell in std::mem::take(logical) {
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
    input: &[u8],
) -> Result<BootstrapProgress, GhosttyEngineError> {
    if native.protocol_finished {
        return Err(GhosttyEngineError::InputAfterReady);
    }
    match native.decoder {
        NativeDecoderState::Collecting => {}
        NativeDecoderState::Finished(_) => return Err(GhosttyEngineError::InputAfterFinish),
    }
    native.feed.data.extend_from_slice(input);
    Ok(BootstrapProgress::Pending)
}

fn attach_native_callbacks(
    native: &NativeReplica,
    terminal: &mut GhosttyTerminal<'static, 'static>,
) -> Result<(), GhosttyEngineError> {
    terminal.on_pty_write({
        let pty_responses = Rc::clone(&native.pty_responses);
        move |_terminal, bytes| pty_responses.borrow_mut().push(bytes.to_vec())
    })?;
    terminal.on_bell({
        let bell_pending = Rc::clone(&native.bell_pending);
        move |_terminal| bell_pending.set(true)
    })?;
    Ok(())
}

fn decode_collected_snapshot(
    native: &NativeReplica,
) -> Result<GhosttyTerminal<'static, 'static>, GhosttyEngineError> {
    let decoder = Decoder::new_buf(&native.feed.data)
        .map_err(|error| GhosttyEngineError::checkpoint(error, 0))?;
    let mut inc = decoder
        .ready()
        .map_err(|error| GhosttyEngineError::checkpoint(error, native.feed.data.len()))?;
    loop {
        match inc.next() {
            Ok(Some(_)) => {}
            Ok(None) | Err(libghostty_vt::Error::InvalidValue) => break,
            Err(error) => {
                return Err(GhosttyEngineError::checkpoint(
                    error,
                    native.feed.data.len(),
                ));
            }
        }
    }
    Ok(inc.into_terminal())
}

fn finish_native(native: &mut NativeReplica) -> Result<BootstrapProgress, GhosttyEngineError> {
    if native.protocol_finished {
        return Err(GhosttyEngineError::InputAfterFinish);
    }
    match native.decoder {
        NativeDecoderState::Collecting => {}
        NativeDecoderState::Finished(_) => {
            native.protocol_finished = true;
            return Ok(BootstrapProgress::Finished);
        }
    }
    let mut terminal = decode_collected_snapshot(native)?;
    let _ = terminal.set_continuation_max_bytes(CONTINUATION_LIMIT);
    attach_native_callbacks(native, &mut terminal)?;
    native.decoder = NativeDecoderState::Finished(terminal);
    native.protocol_finished = true;
    Ok(BootstrapProgress::Finished)
}

const fn push_history(
    native: &NativeReplica,
    _input: &[u8],
) -> Result<HistoryApplyOutcome, GhosttyEngineError> {
    if !native.protocol_finished {
        return Err(GhosttyEngineError::HistoryBeforePublication);
    }
    if matches!(native.decoder, NativeDecoderState::Finished(_)) {
        return Ok(HistoryApplyOutcome {
            progress: BootstrapProgress::Finished,
            retained: true,
        });
    }
    Err(GhosttyEngineError::HistoryBeforePublication)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn geometry() -> CanonicalGeometry {
        CanonicalGeometry::new(80, 4).expect("valid geometry")
    }

    fn native_profile() -> BootstrapStreamProfile {
        BootstrapStreamProfile::NativeState {
            codec: EngineCodec::LibghosttyCheckpointV2,
        }
    }

    fn capture_snapshot() -> (Vec<u8>, Vec<u8>) {
        let mut source = GhosttyTerminal::new(80, 4).expect("source terminal");
        source
            .set_scrollback_max_lines(Some(100))
            .expect("scrollback");
        source
            .set_continuation_max_bytes(CONTINUATION_LIMIT)
            .expect("continuation");
        for line in 0..40 {
            source.vt_write(format!("line {line}\r\n").as_bytes());
        }
        source.vt_write(b"\x1b]2;checkpoint-title\x07");
        let mut encoded = Vec::new();
        source.encode_snapshot(&mut encoded).expect("encode");
        let mut reader = Cursor::new(encoded.as_slice());
        let decoder = Decoder::new(&mut reader).expect("decoder");
        drop(decoder.ready().expect("ready"));
        let offset = usize::try_from(reader.position()).expect("offset");
        let bootstrap = encoded[..offset].to_vec();
        let history = encoded[offset..].to_vec();
        (bootstrap, history)
    }

    fn native_adapter() -> GhosttyAdapter {
        GhosttyAdapter::new(BootstrapLimits::default())
    }

    #[test]
    fn unpublished_synthesized_clear_is_rejected() {
        let mut adapter = native_adapter();
        let mut replica = adapter
            .start_replica(BootstrapStreamProfile::SynthesizedVtRaw, geometry())
            .unwrap();
        assert!(matches!(
            adapter.clear_presentation(&mut replica),
            Err(GhosttyEngineError::LiveOutputBeforeReady)
        ));
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
        assert!(matches!(
            effects.as_slice(),
            [EngineEffect::Status(super::super::EngineStatus::Title(title))] if title.is_empty()
        ));
        effects.clear();
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
    fn live_bells_reach_effects_in_synthesized_and_native_replicas() {
        let (bootstrap, history) = capture_snapshot();
        for profile in [BootstrapStreamProfile::SynthesizedVtRaw, native_profile()] {
            let mut adapter = native_adapter();
            let mut replica = adapter.start_replica(profile, geometry()).unwrap();
            let mut effects = EngineEffectBuffer::new();
            let bytes = if profile == BootstrapStreamProfile::SynthesizedVtRaw {
                b"historical bell\x07".as_slice()
            } else {
                &bootstrap
            };
            adapter
                .apply_bootstrap_chunk(&mut replica, bytes, &mut effects)
                .unwrap();
            adapter
                .finish_bootstrap(&mut replica, &mut effects)
                .unwrap();
            assert!(!effects.as_slice().iter().any(|effect| matches!(
                effect,
                EngineEffect::Status(super::super::EngineStatus::Bell)
            )));
            assert_live_bell(&mut adapter, &mut replica, &mut effects);
            if profile != BootstrapStreamProfile::SynthesizedVtRaw {
                adapter
                    .apply_history_page(&mut replica, &history, &mut effects)
                    .unwrap();
                assert_live_bell(&mut adapter, &mut replica, &mut effects);
            }
        }
    }

    fn assert_live_bell(
        adapter: &mut GhosttyAdapter,
        replica: &mut GhosttyReplica,
        effects: &mut EngineEffectBuffer,
    ) {
        effects.clear();
        adapter
            .apply_output(replica, b"\x1b]2;title\x07", effects)
            .unwrap();
        assert!(!effects.as_slice().iter().any(|effect| matches!(
            effect,
            EngineEffect::Status(super::super::EngineStatus::Bell)
        )));
        effects.clear();
        adapter.apply_output(replica, b"\x07", effects).unwrap();
        assert_eq!(
            effects
                .as_slice()
                .iter()
                .filter(|effect| matches!(
                    effect,
                    EngineEffect::Status(super::super::EngineStatus::Bell)
                ))
                .count(),
            1
        );
        effects.clear();
        adapter.apply_output(replica, b"quiet", effects).unwrap();
        assert!(!effects.as_slice().iter().any(|effect| matches!(
            effect,
            EngineEffect::Status(super::super::EngineStatus::Bell)
        )));
    }

    #[test]
    fn native_decoder_accepts_arbitrary_fragment_cuts_and_multiple_records() {
        let (bootstrap, history) = capture_snapshot();
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
            assert_eq!(bootstrap_progress, BootstrapProgress::Pending);
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
        let (bootstrap, _) = capture_snapshot();
        let mut adapter = native_adapter();
        let mut replica = adapter
            .start_replica(native_profile(), geometry())
            .expect("native replica");
        let mut effects = EngineEffectBuffer::new();
        assert!(matches!(
            adapter.apply_output(&mut replica, b"too early", &mut effects),
            Err(GhosttyEngineError::LiveOutputBeforeReady)
        ));
        assert_eq!(
            adapter
                .apply_bootstrap_chunk(&mut replica, &bootstrap, &mut effects)
                .expect("prefix bytes"),
            BootstrapProgress::Pending
        );
        assert!(replica.terminal().is_none());
        assert!(matches!(
            adapter.apply_output(&mut replica, b"not published", &mut effects),
            Err(GhosttyEngineError::LiveOutputBeforeReady)
        ));
        assert!(matches!(
            adapter.clear_presentation(&mut replica),
            Err(GhosttyEngineError::LiveOutputBeforeReady)
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
        let (bootstrap, history) = capture_snapshot();
        let mut adapter = native_adapter();
        let mut replica = adapter
            .start_replica(native_profile(), geometry())
            .expect("native replica");
        let mut effects = EngineEffectBuffer::new();
        adapter
            .apply_bootstrap_chunk(&mut replica, &bootstrap, &mut effects)
            .expect("checkpoint bootstrap record");
        adapter
            .finish_bootstrap(&mut replica, &mut effects)
            .expect("protocol READY");
        adapter
            .apply_output(
                &mut replica,
                b"\x1b]2;live-during-history\x07",
                &mut effects,
            )
            .expect("live output after READY");
        if !history.is_empty() {
            adapter
                .apply_history_page(&mut replica, &history, &mut effects)
                .expect("history page");
        }
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
        let (bootstrap, _) = capture_snapshot();
        let mut effects = EngineEffectBuffer::new();

        let mut adapter = native_adapter();
        let mut early = adapter
            .start_replica(native_profile(), geometry())
            .expect("native replica");
        assert!(matches!(
            adapter.finish_bootstrap(&mut early, &mut effects),
            Err(GhosttyEngineError::Checkpoint { .. })
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
            Err(GhosttyEngineError::Checkpoint { .. })
        ));

        let mut corrupt = bootstrap.clone();
        corrupt[0] ^= 0xff;
        let mut adapter = native_adapter();
        let mut replica = adapter
            .start_replica(native_profile(), geometry())
            .expect("native replica");
        adapter
            .apply_bootstrap_chunk(&mut replica, &corrupt, &mut effects)
            .expect("corrupt bytes are buffered until READY");
        assert!(matches!(
            adapter.finish_bootstrap(&mut replica, &mut effects),
            Err(GhosttyEngineError::Checkpoint { .. })
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
        assert!(limit_error.is_none());
        assert!(matches!(
            adapter.finish_bootstrap(&mut replica, &mut effects),
            Ok(BootstrapProgress::Finished) | Err(GhosttyEngineError::Checkpoint { .. })
        ));
    }

    #[test]
    fn native_history_finish_rejects_trailing_and_post_finish_pages() {
        let (bootstrap, mut history) = capture_snapshot();
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
        // Full GHOSTSNP is already reconstructed at READY; extra history is a
        // no-op that must leave the live terminal renderable.
        adapter
            .apply_history_page(&mut replica, &history, &mut effects)
            .expect("history after full snapshot decode is ignored");
        assert!(replica.terminal().is_some());
        adapter
            .apply_history_page(&mut replica, b"again", &mut effects)
            .expect("repeat history after decode is ignored");
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
    fn native_history_bounds_every_projection_under_page_granular_storage() {
        let (bootstrap, history) = capture_snapshot();
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
        adapter
            .configure_history_budget(&mut replica, 64 * 1024, 2)
            .expect("engine history limits");
        let mut physical_high_water = 0;
        if !history.is_empty() {
            adapter
                .apply_history_page(&mut replica, &history, &mut effects)
                .expect("bounded history unit");
            physical_high_water = physical_high_water.max(
                replica
                    .terminal()
                    .expect("live terminal")
                    .scrollback_rows()
                    .expect("scrollback rows"),
            );
            let projection = adapter
                .project_history(&mut replica, 80, EngineProjectionOrigin::Tail, 2)
                .expect("bounded projection");
            assert!(projection.rows.len() <= 2);
        }
        assert!(physical_high_water <= crate::history::MAX_HISTORY_PAGE_ROWS as usize);
    }

    #[test]
    fn search_case_policy_preserves_unicode_and_original_document_points() {
        let needle: Vec<char> = "éx".chars().collect();
        let start = history_point(7, 4);
        let end = history_point(8, 4);
        let mut insensitive = NeedleScan::new(&needle, 2, false);
        insensitive.push('é', start);
        insensitive.push('X', end);
        insensitive.push('É', history_point(9, 4));
        insensitive.push('x', history_point(10, 4));
        assert_eq!(insensitive.into_ranges(), vec![(start, end)]);

        let mut sensitive = NeedleScan::new(&needle, 2, true);
        sensitive.push('é', start);
        sensitive.push('X', end);
        assert!(sensitive.into_ranges().is_empty());
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn native_projection_search_and_anchors_remain_engine_owned() {
        let (bootstrap, history) = capture_snapshot();
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
        assert!(
            narrow
                .rows
                .iter()
                .all(|row| row.text.chars().count() <= usize::from(narrow.width))
        );

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
        let full_scrollback = replica
            .terminal()
            .expect("live terminal")
            .scrollback_rows()
            .expect("scrollback rows");
        adapter
            .configure_history_budget(&mut replica, 64 * 1024, full_scrollback)
            .expect("full scrollback cap");
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
        adapter
            .apply_output(&mut replica, b"\x1b]2;control-only\x07", &mut effects)
            .expect("control-only output");
        assert_eq!(
            adapter
                .history_anchor_tail_distance(&replica, found[0].start)
                .expect("anchor distance")
                .expect("retained anchor"),
            distance_after
        );
        adapter
            .apply_output(
                &mut replica,
                b"coalesced-one\r\ncoalesced-two\r\n",
                &mut effects,
            )
            .expect("coalesced output");
        assert_eq!(
            adapter
                .history_anchor_tail_distance(&replica, found[0].start)
                .expect("anchor distance")
                .expect("retained anchor"),
            distance_after + 2
        );
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
