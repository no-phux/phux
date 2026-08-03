use std::collections::{BTreeMap, VecDeque};
use std::convert::Infallible;

use phux_protocol::input::InputEvent;
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::wire::frame::TombstoneReason;
use phux_protocol::{BootstrapId, BootstrapProfile, BootstrapStreamProfile, StreamId, TerminalId};

use super::{
    EffectBuffer, HistoryRejectionReason, HistoryUnavailableReason, InputEligibility, KernelAction,
    KernelEffect, KernelInput, KernelSend, ReplicaKey, SessionKernel, TombstoneRecord,
};
use crate::engine::{
    BootstrapProgress, CanonicalGeometry, DocumentPoint, DocumentSpace, EngineAdapter,
    EngineDocumentAdapter, EngineDocumentSelection, EngineEffectBuffer, EngineHistoryProjection,
    EngineProjectionOrigin, EngineProjectionRow, EngineSearchMatch, HistoryApplyOutcome,
};
use crate::history::{DocumentAnchorId, HistoryCacheConfig, HistoryLoadState, ViewportAnchor};

pub(super) const HISTORY_MAX_BYTES: usize = 256;
pub(super) const HISTORY_MAX_ROWS: usize = 24;

#[derive(Debug, Clone)]
pub(super) enum RigEvent {
    Negotiate {
        state_sync: bool,
    },
    AttachStarted {
        attach_id: u32,
    },
    AttachReady {
        attach_id: u32,
    },
    Disconnect,
    Begin {
        stream: u8,
        generation: u8,
        cols: u16,
        rows: u16,
        base_seq: u64,
        exact_profile: bool,
    },
    Publish {
        stream: u8,
        generation: u8,
        cols: u16,
        rows: u16,
        base_seq: u64,
        history_cursor: Option<u8>,
        bootstrap_text: String,
    },
    Chunk {
        stream: u8,
        generation: u8,
        chunk_seq: u32,
        text: String,
    },
    Ready {
        stream: u8,
        generation: u8,
        history_cursor: Option<u8>,
    },
    Output {
        stream: u8,
        generation: u8,
        seq: u64,
        text: String,
    },
    Resume {
        stream: u8,
        generation: u8,
        seq: u64,
        text: String,
    },
    Paste {
        text: String,
    },
    Prefetch {
        rows_from_oldest: usize,
    },
    HistoryPage {
        stream: u8,
        generation: u8,
        page_seq: u64,
        rows: u32,
        cursor: u8,
        next_cursor: Option<u8>,
        text: String,
    },
    HistoryTombstone {
        stream: u8,
        generation: u8,
        cursor: u8,
        reason: HistoryUnavailableReason,
    },
    HistoryRejected {
        stream: u8,
        generation: u8,
        cursor: u8,
        reason: HistoryRejectionReason,
        required_bytes: u32,
        required_rows: u32,
    },
    Tombstone {
        stream: u8,
        generation: u8,
        last_valid_seq: u64,
    },
    Project {
        width: u16,
        max_rows: usize,
    },
    Track {
        x: u16,
        y: u32,
    },
    Pin {
        anchor_slot: usize,
    },
    FollowTail,
    Select {
        start_slot: usize,
        end_slot: usize,
        rectangle: bool,
    },
    InvalidateAnchors,
    Close,
}

impl RigEvent {
    pub(super) const fn replayable_wire_event(&self) -> bool {
        matches!(
            self,
            Self::Begin { .. }
                | Self::Chunk { .. }
                | Self::Ready { .. }
                | Self::Output { .. }
                | Self::Resume { .. }
                | Self::HistoryPage { .. }
                | Self::HistoryTombstone { .. }
                | Self::Tombstone { .. }
                | Self::Close
        )
    }

    pub(super) const fn resets_connection(&self) -> bool {
        matches!(self, Self::Negotiate { .. })
    }

    pub(super) const fn explicitly_invalidates_anchors(&self) -> bool {
        matches!(
            self,
            Self::InvalidateAnchors
                | Self::HistoryTombstone { .. }
                | Self::HistoryPage { .. }
                | Self::HistoryRejected { .. }
        )
    }
}

#[derive(Debug)]
pub(super) struct RigReplica {
    geometry: CanonicalGeometry,
    bootstrap: Vec<u8>,
    live: Vec<u8>,
    history: VecDeque<Vec<u8>>,
    history_bytes: usize,
    history_budget: usize,
    history_row_budget: usize,
    anchors: BTreeMap<DocumentAnchorId, RigAnchor>,
    tail_rows: u64,
    saw_bootstrap: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RigAnchor {
    point: DocumentPoint,
    tail_at_creation: u64,
}

#[derive(Debug, Default)]
pub(super) struct RigAdapter {
    next_anchor: u64,
}

impl RigAdapter {
    fn allocate_anchor(
        &mut self,
        replica: &mut RigReplica,
        point: DocumentPoint,
    ) -> DocumentAnchorId {
        self.next_anchor = self.next_anchor.saturating_add(1).max(1);
        let anchor = DocumentAnchorId::from_raw(self.next_anchor);
        replica.anchors.insert(
            anchor,
            RigAnchor {
                point,
                tail_at_creation: replica.tail_rows,
            },
        );
        anchor
    }
}

impl EngineAdapter for RigAdapter {
    type Replica = RigReplica;
    type Error = Infallible;

    fn start_replica(
        &mut self,
        _profile: BootstrapStreamProfile,
        geometry: CanonicalGeometry,
    ) -> Result<Self::Replica, Self::Error> {
        Ok(RigReplica {
            geometry,
            bootstrap: Vec::new(),
            live: Vec::new(),
            history: VecDeque::new(),
            history_bytes: 0,
            history_budget: usize::MAX,
            history_row_budget: usize::MAX,
            anchors: BTreeMap::new(),
            tail_rows: 0,
            saw_bootstrap: false,
        })
    }

    fn apply_bootstrap_chunk(
        &mut self,
        replica: &mut Self::Replica,
        payload: &[u8],
        _effects: &mut EngineEffectBuffer,
    ) -> Result<BootstrapProgress, Self::Error> {
        replica.bootstrap.extend_from_slice(payload);
        replica.saw_bootstrap = true;
        Ok(BootstrapProgress::Ready)
    }

    fn configure_history_budget(
        &mut self,
        replica: &mut Self::Replica,
        max_bytes: usize,
        max_rows: usize,
    ) -> Result<(), Self::Error> {
        replica.history_budget = max_bytes;
        replica.history_row_budget = max_rows;
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
        Ok(replica
            .anchors
            .get(&anchor)
            .map(|tracked| replica.tail_rows.saturating_sub(tracked.tail_at_creation)))
    }

    fn finish_bootstrap(
        &mut self,
        replica: &mut Self::Replica,
        _effects: &mut EngineEffectBuffer,
    ) -> Result<BootstrapProgress, Self::Error> {
        Ok(if replica.saw_bootstrap {
            BootstrapProgress::Finished
        } else {
            BootstrapProgress::Pending
        })
    }

    fn apply_history_page(
        &mut self,
        replica: &mut Self::Replica,
        payload: &[u8],
        _effects: &mut EngineEffectBuffer,
    ) -> Result<HistoryApplyOutcome, Self::Error> {
        replica.history.push_back(payload.to_vec());
        replica.history_bytes = replica.history_bytes.saturating_add(payload.len());
        while replica.history_bytes > replica.history_budget {
            let Some(removed) = replica.history.pop_front() else {
                break;
            };
            replica.history_bytes = replica.history_bytes.saturating_sub(removed.len());
        }
        Ok(HistoryApplyOutcome {
            progress: BootstrapProgress::Ready,
            retained: true,
        })
    }

    #[allow(
        clippy::naive_bytecount,
        reason = "the test-only rig does one delimiter count; another dev dependency is disproportionate"
    )]
    fn apply_output(
        &mut self,
        replica: &mut Self::Replica,
        payload: &[u8],
        _effects: &mut EngineEffectBuffer,
    ) -> Result<(), Self::Error> {
        replica.live.extend_from_slice(payload);
        let rows = payload.iter().filter(|byte| **byte == b'\n').count().max(1);
        replica.tail_rows = replica
            .tail_rows
            .saturating_add(u64::try_from(rows).unwrap_or(u64::MAX));
        Ok(())
    }
}

impl EngineDocumentAdapter for RigAdapter {
    fn project_history(
        &mut self,
        replica: &mut Self::Replica,
        width: u16,
        _origin: EngineProjectionOrigin,
        max_rows: usize,
    ) -> Result<EngineHistoryProjection, Self::Error> {
        let rows = replica
            .history
            .iter()
            .rev()
            .take(max_rows.min(replica.history_row_budget))
            .rev()
            .map(|payload| EngineProjectionRow {
                text: String::from_utf8_lossy(payload).into_owned(),
                soft_wrapped: false,
                page: None,
            })
            .collect();
        Ok(EngineHistoryProjection {
            width,
            rows,
            has_older: false,
        })
    }

    fn track_document_anchor(
        &mut self,
        replica: &mut Self::Replica,
        point: DocumentPoint,
    ) -> Result<DocumentAnchorId, Self::Error> {
        Ok(self.allocate_anchor(replica, point))
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
        Ok(replica
            .anchors
            .get(&anchor)
            .map(|tracked| tracked.point)
            .filter(|point| point.space == space))
    }

    fn search_loaded(
        &mut self,
        replica: &mut Self::Replica,
        needle: &str,
        max_matches: usize,
    ) -> Result<Vec<EngineSearchMatch>, Self::Error> {
        if max_matches == 0
            || !replica
                .history
                .iter()
                .any(|payload| String::from_utf8_lossy(payload).contains(needle))
        {
            return Ok(Vec::new());
        }
        let point = DocumentPoint {
            space: DocumentSpace::History,
            x: 0,
            y: 0,
        };
        let start = self.allocate_anchor(replica, point);
        let end = self.allocate_anchor(replica, point);
        Ok(vec![EngineSearchMatch { start, end }])
    }

    fn format_selection(
        &self,
        replica: &Self::Replica,
        selection: EngineDocumentSelection,
    ) -> Result<Option<String>, Self::Error> {
        if !replica.anchors.contains_key(&selection.start)
            || !replica.anchors.contains_key(&selection.end)
        {
            return Ok(None);
        }
        let mut text = String::new();
        for page in &replica.history {
            text.push_str(&String::from_utf8_lossy(page));
        }
        text.push_str(&String::from_utf8_lossy(&replica.live));
        Ok(Some(text))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HistoryObservation {
    pub(super) state: HistoryLoadState,
    pub(super) loaded_pages: usize,
    pub(super) retained_payload_bytes: usize,
    pub(super) materialized_rows: usize,
    pub(super) unread_rows: u64,
    pub(super) viewport: ViewportAnchor,
    pub(super) projection_width: u16,
    pub(super) next_cursor: Option<Vec<u8>>,
    pub(super) next_page_seq: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReplicaObservation {
    pub(super) key: ReplicaKey,
    pub(super) geometry: CanonicalGeometry,
    pub(super) engine_geometry: CanonicalGeometry,
    pub(super) last_seq: u64,
    pub(super) bootstrap: Vec<u8>,
    pub(super) live: Vec<u8>,
    pub(super) imported_history_bytes: usize,
    pub(super) anchors: Vec<(DocumentAnchorId, DocumentPoint)>,
    pub(super) history: HistoryObservation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StagingObservation {
    pub(super) key: ReplicaKey,
    pub(super) geometry: CanonicalGeometry,
    pub(super) engine_geometry: CanonicalGeometry,
    pub(super) engine_ready: bool,
    pub(super) protocol_ready: bool,
    pub(super) bootstrap: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RigSnapshot {
    pub(super) selected_profile: BootstrapProfile,
    pub(super) published: Option<ReplicaObservation>,
    pub(super) staging: Option<StagingObservation>,
    pub(super) tombstones: Vec<((u8, u8), TombstoneRecord)>,
    pub(super) eligibility: InputEligibility,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ResumeCheckpoint {
    pub(super) stream: u8,
    pub(super) generation: u8,
    pub(super) next_seq: u64,
}

pub(super) struct KernelRig {
    kernel: SessionKernel<RigAdapter>,
    effects: EffectBuffer,
    terminal_id: TerminalId,
    anchors: Vec<DocumentAnchorId>,
}

impl KernelRig {
    pub(super) fn new(state_sync: bool) -> Self {
        let terminal_id = TerminalId::local(55);
        Self {
            kernel: Self::new_kernel(state_sync),
            effects: EffectBuffer::with_capacity(8),
            terminal_id,
            anchors: Vec::new(),
        }
    }

    fn new_kernel(state_sync: bool) -> SessionKernel<RigAdapter> {
        let profile = if state_sync {
            BootstrapProfile::SynthesizedVtStateSync
        } else {
            BootstrapProfile::SynthesizedVtRaw
        };
        SessionKernel::with_history_config(
            RigAdapter::default(),
            profile,
            HistoryCacheConfig {
                max_bytes: HISTORY_MAX_BYTES,
                max_materialized_rows: HISTORY_MAX_ROWS,
                prefetch_rows: 4,
                request_max_bytes: 64,
                request_max_rows: 8,
            },
        )
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn apply(&mut self, event: &RigEvent) -> bool {
        match event {
            RigEvent::Negotiate { state_sync } => {
                self.kernel = Self::new_kernel(*state_sync);
                self.effects.clear();
                self.anchors.clear();
                true
            }
            RigEvent::AttachStarted { attach_id } => {
                let terminals = [self.terminal_id.clone()];
                self.update(KernelInput::AttachStarted {
                    attach_id: *attach_id,
                    terminals: &terminals,
                })
            }
            RigEvent::AttachReady { attach_id } => self.update(KernelInput::AttachReady {
                attach_id: *attach_id,
            }),
            RigEvent::Disconnect => {
                self.kernel.release_active_attach();
                self.effects.clear();
                true
            }
            RigEvent::Begin {
                stream,
                generation,
                cols,
                rows,
                base_seq,
                exact_profile,
            } => self.begin(
                *stream,
                *generation,
                *cols,
                *rows,
                *base_seq,
                *exact_profile,
            ),
            RigEvent::Publish {
                stream,
                generation,
                cols,
                rows,
                base_seq,
                history_cursor,
                bootstrap_text,
            } => {
                if !self.begin(*stream, *generation, *cols, *rows, *base_seq, true) {
                    return false;
                }
                if !self.chunk(*stream, *generation, 0, bootstrap_text) {
                    return false;
                }
                self.ready(*stream, *generation, *history_cursor)
            }
            RigEvent::Chunk {
                stream,
                generation,
                chunk_seq,
                text,
            } => self.chunk(*stream, *generation, *chunk_seq, text),
            RigEvent::Ready {
                stream,
                generation,
                history_cursor,
            } => self.ready(*stream, *generation, *history_cursor),
            RigEvent::Output {
                stream,
                generation,
                seq,
                text,
            }
            | RigEvent::Resume {
                stream,
                generation,
                seq,
                text,
            } => {
                let terminal_id = self.terminal_id.clone();
                self.update(KernelInput::TerminalOutput {
                    terminal_id: &terminal_id,
                    stream_id: stream_id(*stream),
                    bootstrap_id: bootstrap_id(*generation),
                    seq: *seq,
                    payload: text.as_bytes(),
                })
            }
            RigEvent::Paste { text } => {
                let input = InputEvent::Paste(PasteEvent {
                    trust: PasteTrust::Trusted,
                    data: text.as_bytes().to_vec(),
                });
                let terminal_id = self.terminal_id.clone();
                self.update(KernelInput::Action(KernelAction::Input {
                    terminal_id: &terminal_id,
                    event: &input,
                }))
            }
            RigEvent::Prefetch { rows_from_oldest } => self.kernel.prefetch_history(
                &self.terminal_id,
                *rows_from_oldest,
                &mut self.effects,
            ),
            RigEvent::HistoryPage {
                stream,
                generation,
                page_seq,
                rows,
                cursor,
                next_cursor,
                text,
            } => {
                let cursor = cursor_bytes(*cursor);
                let next = next_cursor.map(cursor_bytes);
                let terminal_id = self.terminal_id.clone();
                self.update(KernelInput::HistoryPage {
                    terminal_id: &terminal_id,
                    stream_id: stream_id(*stream),
                    bootstrap_id: bootstrap_id(*generation),
                    page_seq: *page_seq,
                    rows: *rows,
                    payload: text.as_bytes(),
                    cursor: &cursor,
                    next_cursor: next.as_deref(),
                })
            }
            RigEvent::HistoryTombstone {
                stream,
                generation,
                cursor,
                reason,
            } => {
                let cursor = cursor_bytes(*cursor);
                let terminal_id = self.terminal_id.clone();
                self.update(KernelInput::HistoryTombstone {
                    terminal_id: &terminal_id,
                    stream_id: stream_id(*stream),
                    bootstrap_id: bootstrap_id(*generation),
                    cursor: &cursor,
                    reason: *reason,
                })
            }
            RigEvent::HistoryRejected {
                stream,
                generation,
                cursor,
                reason,
                required_bytes,
                required_rows,
            } => {
                let cursor = cursor_bytes(*cursor);
                let terminal_id = self.terminal_id.clone();
                self.update(KernelInput::HistoryRejected {
                    terminal_id: &terminal_id,
                    stream_id: stream_id(*stream),
                    bootstrap_id: bootstrap_id(*generation),
                    cursor: &cursor,
                    reason: *reason,
                    required_bytes: *required_bytes,
                    required_rows: *required_rows,
                })
            }
            RigEvent::Tombstone {
                stream,
                generation,
                last_valid_seq,
            } => {
                let terminal_id = self.terminal_id.clone();
                self.update(KernelInput::Tombstone {
                    terminal_id: &terminal_id,
                    stream_id: stream_id(*stream),
                    bootstrap_id: bootstrap_id(*generation),
                    reason: TombstoneReason::OutboundGap,
                    last_valid_seq: *last_valid_seq,
                })
            }
            RigEvent::Project { width, max_rows } => self
                .kernel
                .project_history(&self.terminal_id, *width, *max_rows)
                .is_ok(),
            RigEvent::Track { x, y } => {
                let tracked = self.kernel.track_document_anchor(
                    &self.terminal_id,
                    DocumentPoint {
                        space: DocumentSpace::History,
                        x: *x,
                        y: *y,
                    },
                );
                match tracked {
                    Ok(anchor) => {
                        self.anchors.push(anchor);
                        true
                    }
                    Err(_) => false,
                }
            }
            RigEvent::Pin { anchor_slot } => {
                self.selected_anchor(*anchor_slot).is_some_and(|anchor| {
                    self.kernel
                        .pin_history_viewport(&self.terminal_id, anchor)
                        .is_ok()
                })
            }
            RigEvent::FollowTail => self.kernel.follow_history_tail(&self.terminal_id).is_ok(),
            RigEvent::Select {
                start_slot,
                end_slot,
                rectangle,
            } => {
                let Some(start) = self.selected_anchor(*start_slot) else {
                    return false;
                };
                let Some(end) = self.selected_anchor(*end_slot) else {
                    return false;
                };
                self.kernel
                    .format_document_selection(
                        &self.terminal_id,
                        EngineDocumentSelection {
                            start,
                            end,
                            rectangle: *rectangle,
                        },
                    )
                    .is_ok()
            }
            RigEvent::InvalidateAnchors => {
                if let Some(engine) = self.kernel.published_engine_mut(&self.terminal_id) {
                    engine.anchors.clear();
                    let _ = self.kernel.project_history(&self.terminal_id, 80, 8);
                    true
                } else {
                    false
                }
            }
            RigEvent::Close => {
                let terminal_id = self.terminal_id.clone();
                self.update(KernelInput::TerminalClosed {
                    terminal_id: &terminal_id,
                })
            }
        }
    }

    fn update(&mut self, input: KernelInput<'_>) -> bool {
        self.kernel.update(input, &mut self.effects).is_ok()
    }

    fn begin(
        &mut self,
        stream: u8,
        generation: u8,
        cols: u16,
        rows: u16,
        base_seq: u64,
        exact_profile: bool,
    ) -> bool {
        let selected = self.kernel.selected_profile();
        let matching = match selected {
            BootstrapProfile::SynthesizedVtRaw => BootstrapStreamProfile::SynthesizedVtRaw,
            BootstrapProfile::SynthesizedVtStateSync => {
                BootstrapStreamProfile::SynthesizedVtStateSync
            }
            _ => unreachable!("rig negotiates synthesized profiles"),
        };
        let profile = if exact_profile {
            matching
        } else {
            match matching {
                BootstrapStreamProfile::SynthesizedVtRaw => {
                    BootstrapStreamProfile::SynthesizedVtStateSync
                }
                BootstrapStreamProfile::SynthesizedVtStateSync => {
                    BootstrapStreamProfile::SynthesizedVtRaw
                }
                _ => unreachable!("rig negotiates synthesized profiles"),
            }
        };
        let Some(geometry) = CanonicalGeometry::new(cols, rows) else {
            return false;
        };
        let terminal_id = self.terminal_id.clone();
        self.update(KernelInput::BootstrapBegin {
            terminal_id: &terminal_id,
            stream_id: stream_id(stream),
            bootstrap_id: bootstrap_id(generation),
            profile,
            geometry,
            base_seq,
        })
    }

    fn chunk(&mut self, stream: u8, generation: u8, chunk_seq: u32, text: &str) -> bool {
        let terminal_id = self.terminal_id.clone();
        self.update(KernelInput::BootstrapChunk {
            terminal_id: &terminal_id,
            stream_id: stream_id(stream),
            bootstrap_id: bootstrap_id(generation),
            chunk_seq,
            payload: text.as_bytes(),
        })
    }

    fn ready(&mut self, stream: u8, generation: u8, cursor: Option<u8>) -> bool {
        let cursor = cursor.map(cursor_bytes);
        let terminal_id = self.terminal_id.clone();
        self.update(KernelInput::BootstrapReady {
            terminal_id: &terminal_id,
            stream_id: stream_id(stream),
            bootstrap_id: bootstrap_id(generation),
            history_cursor: cursor.as_deref(),
        })
    }

    fn selected_anchor(&self, slot: usize) -> Option<DocumentAnchorId> {
        self.anchors
            .get(slot.checked_rem(self.anchors.len())?)
            .copied()
    }

    pub(super) fn selection_text(&self, start_slot: usize, end_slot: usize) -> Option<String> {
        let start = self.selected_anchor(start_slot)?;
        let end = self.selected_anchor(end_slot)?;
        self.kernel
            .format_document_selection(
                &self.terminal_id,
                EngineDocumentSelection {
                    start,
                    end,
                    rectangle: false,
                },
            )
            .ok()
            .flatten()
    }

    pub(super) fn last_paste_payload(&self) -> Option<&[u8]> {
        self.effects.as_slice().iter().find_map(|effect| {
            let KernelEffect::Send(KernelSend::Input {
                event: InputEvent::Paste(paste),
                ..
            }) = effect
            else {
                return None;
            };
            Some(paste.data.as_slice())
        })
    }

    pub(super) fn resume_checkpoint(&self) -> Option<ResumeCheckpoint> {
        let InputEligibility::Eligible {
            stream_id,
            bootstrap_id,
        } = self.kernel.input_eligibility(&self.terminal_id)
        else {
            return None;
        };
        let published = self.kernel.published(&self.terminal_id)?;
        Some(ResumeCheckpoint {
            stream: u8::try_from(stream_id.get()).ok()?,
            generation: u8::try_from(bootstrap_id.get()).ok()?,
            next_seq: published.last_seq().checked_add(1)?,
        })
    }

    pub(super) fn snapshot(&self) -> RigSnapshot {
        let published = self.kernel.published(&self.terminal_id).map(|published| {
            let status = published.history().status();
            ReplicaObservation {
                key: published.key().clone(),
                geometry: published.geometry(),
                engine_geometry: published.engine().geometry,
                last_seq: published.last_seq(),
                bootstrap: published.engine().bootstrap.clone(),
                live: published.engine().live.clone(),
                imported_history_bytes: published.engine().history_bytes,
                anchors: published
                    .engine()
                    .anchors
                    .iter()
                    .map(|(anchor, tracked)| (*anchor, tracked.point))
                    .collect(),
                history: HistoryObservation {
                    state: status.state,
                    loaded_pages: status.loaded_pages,
                    retained_payload_bytes: published.history().retained_payload_bytes_for_tests(),
                    materialized_rows: status.materialized_rows,
                    unread_rows: status.unread_rows,
                    viewport: published.history().viewport_anchor(),
                    projection_width: published.history().projection_width(),
                    next_cursor: status
                        .next_cursor
                        .as_ref()
                        .map(|cursor| cursor.as_bytes().to_vec()),
                    next_page_seq: status.next_page_seq,
                },
            }
        });
        let staging = self
            .kernel
            .staging(&self.terminal_id)
            .map(|staging| StagingObservation {
                key: staging.key().clone(),
                geometry: staging.geometry(),
                engine_geometry: staging.engine().geometry,
                engine_ready: staging.engine_ready(),
                protocol_ready: staging.protocol_ready(),
                bootstrap: staging.engine().bootstrap.clone(),
            });
        let mut tombstones = Vec::new();
        for stream in 1..=3 {
            for generation in 1..=3 {
                if let Some(record) = self.kernel.tombstone(
                    &self.terminal_id,
                    stream_id(stream),
                    bootstrap_id(generation),
                ) {
                    tombstones.push(((stream, generation), record));
                }
            }
        }
        RigSnapshot {
            selected_profile: self.kernel.selected_profile(),
            published,
            staging,
            tombstones,
            eligibility: self.kernel.input_eligibility(&self.terminal_id),
        }
    }
}

fn stream_id(raw: u8) -> StreamId {
    StreamId::new(u64::from(raw.max(1))).expect("rig stream identifiers are non-zero")
}

fn bootstrap_id(raw: u8) -> BootstrapId {
    BootstrapId::new(u64::from(raw.max(1))).expect("rig bootstrap identifiers are non-zero")
}

fn cursor_bytes(raw: u8) -> Vec<u8> {
    vec![b'c', raw]
}
