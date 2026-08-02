use std::collections::HashMap;
use std::ptr;

use libghostty_vt::render::{CellIterator, CursorVisualStyle, RenderState, RowIterator};
use libghostty_vt::screen::CellContentTag;
use libghostty_vt::selection::Selection;
use libghostty_vt::style::{RgbColor, StyleColor};
use libghostty_vt::terminal::{Point, PointCoordinate, ScrollViewport};
use phux_client_core::engine::ghostty::{GhosttyAdapter, GhosttyReplica};
use phux_client_core::engine::{DocumentPoint, DocumentSpace, EngineDocumentSelection};
use phux_client_core::history::{DocumentAnchorId, HistoryCacheConfig, HistoryLoadState};
use phux_client_core::session::{
    EffectBuffer, KernelDamageKind, KernelEffect, KernelSend, KernelStatus, ReplicaKey,
    SessionKernel,
};
use phux_protocol::BootstrapLimits;
use phux_protocol::TerminalId;
use phux_protocol::wire::frame::FrameKind;

use crate::error::BridgeError;
use crate::types::*;

#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    pub bootstrap_chunk: u32,
    pub history_page: u32,
    pub history_page_rows: u32,
    pub history_cache_bytes: usize,
    pub history_materialized_rows: usize,
    pub history_prefetch_rows: usize,
}

pub(crate) struct RenderCache {
    state: RenderState<'static>,
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
    pub grid_cells: Vec<PhuxTerminalCell>,
    pub utf8: Vec<u8>,
    pub terminal_host: Vec<u8>,
    pub view: PhuxTerminalGridView,
}

impl RenderCache {
    fn new() -> Result<Self, BridgeError> {
        Ok(Self {
            state: RenderState::new().map_err(BridgeError::ghostty)?,
            rows: RowIterator::new().map_err(BridgeError::ghostty)?,
            cells: CellIterator::new().map_err(BridgeError::ghostty)?,
            grid_cells: Vec::new(),
            utf8: Vec::new(),
            terminal_host: Vec::new(),
            view: PhuxTerminalGridView::default(),
        })
    }
}

pub(crate) struct Client {
    pub session: SessionKernel<GhosttyAdapter>,
    pub effects: EffectBuffer,
    pub outgoing: Vec<Vec<u8>>,
    pub owned_effects: Vec<OwnedEffect>,
    pub effect_views: Vec<PhuxClientEffect>,
    pub render: HashMap<TerminalId, RenderCache>,
    pub selection_buf: Vec<u8>,
    pub document_revisions: HashMap<TerminalId, u64>,
    pub next_document_revision: u64,
    pub search_results: Vec<PhuxSearchResult>,
    pub anchors: HashMap<u64, (TerminalId, DocumentAnchorId)>,
    pub next_anchor_handle: u64,
    pub selections: HashMap<TerminalId, EngineDocumentSelection>,
    pub viewport_anchors: HashMap<TerminalId, DocumentAnchorId>,
    pub last_error: Vec<u8>,
    pub limits: Limits,
    pub protocol_ready: bool,
    pub hello_queued: bool,
    pub callbacks: PhuxClientCallbacks,
    pub in_callback: bool,
    pub attached_notified: bool,
    pub attach_queued: bool,
    pub expected_attach_id: Option<u32>,
    pub selected_profile: Option<phux_protocol::BootstrapProfile>,
    pub attached: bool,
    pub terminal_reply: bool,
    pub detached: bool,
}

impl Client {
    pub(crate) fn new(limits: Limits) -> Self {
        let history_config = HistoryCacheConfig {
            max_bytes: limits.history_cache_bytes,
            max_materialized_rows: limits.history_materialized_rows,
            prefetch_rows: limits.history_prefetch_rows,
            request_max_bytes: limits.history_page,
            request_max_rows: limits.history_page_rows,
        };
        Self {
            session: SessionKernel::with_history_config(
                GhosttyAdapter::new(engine_limits(limits)),
                phux_protocol::BootstrapProfile::SynthesizedVtRaw,
                history_config,
            ),
            effects: EffectBuffer::new(),
            outgoing: Vec::new(),
            owned_effects: Vec::new(),
            effect_views: Vec::new(),
            render: HashMap::new(),
            selection_buf: Vec::new(),
            document_revisions: HashMap::new(),
            next_document_revision: 1,
            search_results: Vec::new(),
            last_error: Vec::new(),
            limits,
            anchors: HashMap::new(),
            next_anchor_handle: 1,
            selections: HashMap::new(),
            viewport_anchors: HashMap::new(),
            protocol_ready: false,
            hello_queued: false,
            callbacks: PhuxClientCallbacks::default(),
            in_callback: false,
            attached_notified: false,
            attach_queued: false,
            expected_attach_id: None,
            selected_profile: None,
            attached: false,
            terminal_reply: false,
            detached: false,
        }
    }
    pub(crate) fn install_profile(
        &mut self,
        profile: phux_protocol::BootstrapProfile,
        negotiated_limits: BootstrapLimits,
    ) {
        self.limits.bootstrap_chunk = negotiated_limits.max_chunk_bytes();
        self.limits.history_page = negotiated_limits.max_history_page_bytes();
        let history_config = HistoryCacheConfig {
            max_bytes: self.limits.history_cache_bytes,
            max_materialized_rows: self.limits.history_materialized_rows,
            prefetch_rows: self.limits.history_prefetch_rows,
            request_max_bytes: self.limits.history_page,
            request_max_rows: self.limits.history_page_rows,
        };
        self.session = SessionKernel::with_history_config(
            GhosttyAdapter::new(negotiated_limits),
            profile,
            history_config,
        );
    }

    pub(crate) fn reset_borrows(&mut self) {
        self.selection_buf.clear();
        self.search_results.clear();
    }

    pub(crate) fn state(&self) -> PhuxClientState {
        if self.detached {
            PhuxClientState::Detached
        } else if self.attached {
            PhuxClientState::Attached
        } else if self.protocol_ready {
            PhuxClientState::Negotiated
        } else if self.hello_queued {
            PhuxClientState::HelloQueued
        } else {
            PhuxClientState::New
        }
    }

    pub(crate) fn set_error(&mut self, message: impl AsRef<str>) {
        self.last_error.clear();
        self.last_error
            .extend_from_slice(message.as_ref().as_bytes());
    }

    pub(crate) fn ensure_attached(&self) -> Result<(), BridgeError> {
        if self.attached && !self.detached {
            Ok(())
        } else {
            Err(BridgeError::state("operation requires an attached client"))
        }
    }

    pub(crate) fn ensure_participant(&self, terminal_id: &TerminalId) -> Result<(), BridgeError> {
        if self.detached || (!self.attach_queued && !self.attached) {
            return Err(BridgeError::protocol(
                "terminal state frame arrived outside an active ATTACH phase",
            ));
        }
        if self.session.active_attach_contains(terminal_id) {
            Ok(())
        } else {
            Err(BridgeError::protocol(
                "terminal state frame targets a terminal outside the active ATTACH",
            ))
        }
    }

    pub(crate) fn detach(&mut self) {
        self.session.release_active_attach();
        self.effects.clear();
        self.render.clear();
        self.document_revisions.clear();
        self.anchors.clear();
        self.selections.clear();
        self.viewport_anchors.clear();
        self.attach_queued = false;
        self.expected_attach_id = None;
        self.attached = false;
        self.detached = true;
    }

    pub(crate) fn terminal_key(&self, id: &TerminalId) -> Result<ReplicaKey, BridgeError> {
        self.ensure_attached()?;
        self.session
            .published(id)
            .map(|published| published.key().clone())
            .ok_or_else(|| BridgeError::state("terminal has no published READY generation"))
    }

    pub(crate) fn bump_document_revision(
        &mut self,
        terminal_id: &TerminalId,
    ) -> Result<(), BridgeError> {
        let revision = self.next_document_revision;
        self.next_document_revision = self
            .next_document_revision
            .checked_add(1)
            .ok_or_else(|| BridgeError::engine("document revision space exhausted"))?;
        self.document_revisions
            .insert(terminal_id.clone(), revision);
        Ok(())
    }

    fn register_anchor(
        &mut self,
        terminal_id: &TerminalId,
        anchor: DocumentAnchorId,
    ) -> Result<PhuxDocumentAnchor, BridgeError> {
        let handle = self.next_anchor_handle;
        self.next_anchor_handle = self
            .next_anchor_handle
            .checked_add(1)
            .ok_or_else(|| BridgeError::engine("document anchor handle space exhausted"))?;
        self.anchors.insert(handle, (terminal_id.clone(), anchor));
        Ok(PhuxDocumentAnchor { opaque_id: handle })
    }

    fn resolve_anchor(
        &self,
        terminal_id: &TerminalId,
        anchor: PhuxDocumentAnchor,
    ) -> Result<DocumentAnchorId, BridgeError> {
        let Some((owner, engine_anchor)) = self.anchors.get(&anchor.opaque_id) else {
            return Err(BridgeError::state("document anchor is stale or unknown"));
        };
        if owner != terminal_id {
            return Err(BridgeError::state(
                "document anchor belongs to another terminal",
            ));
        }
        Ok(*engine_anchor)
    }

    pub(crate) fn track_anchor(
        &mut self,
        terminal_id: &TerminalId,
        point: PhuxDocumentPoint,
    ) -> Result<PhuxDocumentAnchor, BridgeError> {
        self.ensure_attached()?;
        if point.reserved != 0 {
            return Err(BridgeError::invalid(
                "document point reserved field must be zero",
            ));
        }
        let space = match point.space {
            0 => DocumentSpace::History,
            1 => DocumentSpace::Viewport,
            2 => DocumentSpace::Active,
            _ => return Err(BridgeError::invalid("unknown document point space")),
        };
        let anchor = self
            .session
            .track_document_anchor(
                terminal_id,
                DocumentPoint {
                    space,
                    x: point.column,
                    y: point.row,
                },
            )
            .map_err(|error| BridgeError::engine(error.to_string()))?;
        self.register_anchor(terminal_id, anchor)
    }

    pub(crate) fn release_anchor(
        &mut self,
        terminal_id: &TerminalId,
        anchor: PhuxDocumentAnchor,
    ) -> Result<(), BridgeError> {
        self.ensure_attached()?;
        let engine_anchor = self.resolve_anchor(terminal_id, anchor)?;
        self.session
            .release_document_anchor(terminal_id, engine_anchor)
            .map_err(|error| BridgeError::engine(error.to_string()))?;
        self.anchors.remove(&anchor.opaque_id);
        Ok(())
    }

    pub(crate) fn pin_viewport(
        &mut self,
        terminal_id: &TerminalId,
        anchor: PhuxDocumentAnchor,
    ) -> Result<(), BridgeError> {
        self.ensure_attached()?;
        let engine_anchor = self.resolve_anchor(terminal_id, anchor)?;
        self.session
            .pin_history_viewport(terminal_id, engine_anchor)
            .map_err(|error| BridgeError::engine(error.to_string()))
    }

    pub(crate) fn follow_live(&mut self, terminal_id: &TerminalId) -> Result<(), BridgeError> {
        self.ensure_attached()?;
        self.session
            .follow_history_tail(terminal_id)
            .map_err(|error| BridgeError::engine(error.to_string()))?;
        if let Some(old) = self.viewport_anchors.remove(terminal_id) {
            self.session
                .release_document_anchor(terminal_id, old)
                .map_err(|error| BridgeError::engine(error.to_string()))?;
        }
        Ok(())
    }

    pub(crate) fn invalidate_terminal_handles(&mut self, terminal_id: &TerminalId) {
        self.anchors.retain(|_, (owner, _)| owner != terminal_id);
        self.selections.remove(terminal_id);
        self.viewport_anchors.remove(terminal_id);
    }
    fn document_revision(&self, terminal_id: &TerminalId) -> Result<u64, BridgeError> {
        self.document_revisions
            .get(terminal_id)
            .copied()
            .ok_or_else(|| BridgeError::state("terminal has no published document revision"))
    }

    pub(crate) fn terminal(
        &self,
        id: &TerminalId,
    ) -> Result<&libghostty_vt::Terminal<'static, 'static>, BridgeError> {
        self.ensure_attached()?;
        self.session
            .published_engine(id)
            .and_then(GhosttyReplica::terminal)
            .ok_or_else(|| BridgeError::state("terminal has no renderable engine"))
    }

    pub(crate) fn process_effects(&mut self) -> Result<(), BridgeError> {
        let mut effects = self.effects.take();
        let result: Result<(), BridgeError> = (|| {
            for effect in effects.drain(..) {
                match effect {
                    KernelEffect::Send(send) => self.process_send(send)?,
                    KernelEffect::Damage(damage) => {
                        let mut out = OwnedEffect::simple(1, 0, damage.terminal_id);
                        match damage.kind {
                            KernelDamageKind::Full => out.detail = 1,
                            KernelDamageKind::Rows { first, last } => {
                                out.detail = 2;
                                out.first_row = first;
                                out.last_row = last;
                            }
                            KernelDamageKind::Removed => out.detail = 3,
                        }
                        self.owned_effects.push(out);
                    }
                    KernelEffect::Status(status) => self.process_status(status),
                    KernelEffect::Job(job) => {
                        let detail = match job.job {
                            phux_client_core::engine::EngineJob::Wakeup => 1,
                        };
                        let mut out = OwnedEffect::simple(3, detail, job.key.terminal_id);
                        out.stream_id = u64::from(job.key.stream_id.get());
                        out.bootstrap_id = u64::from(job.key.bootstrap_id.get());
                        self.owned_effects.push(out);
                    }
                }
            }
            Ok(())
        })();
        effects.clear();
        self.effects.restore_allocation(effects);
        result?;
        self.rebuild_effect_views();
        Ok(())
    }

    fn process_send(&mut self, send: KernelSend) -> Result<(), BridgeError> {
        match send {
            KernelSend::Input { terminal_id, event } => {
                let frame = match event {
                    phux_protocol::input::InputEvent::Key(event) => {
                        FrameKind::InputKey { terminal_id, event }
                    }
                    phux_protocol::input::InputEvent::Mouse(event) => {
                        FrameKind::InputMouse { terminal_id, event }
                    }
                    phux_protocol::input::InputEvent::Focus(event) => {
                        FrameKind::InputFocus { terminal_id, event }
                    }
                    phux_protocol::input::InputEvent::Paste(event) => {
                        FrameKind::InputPaste { terminal_id, event }
                    }
                    _ => {
                        return Err(BridgeError::engine(
                            "engine emitted an unsupported input event",
                        ));
                    }
                };
                self.queue_frame(frame)?;
            }
            KernelSend::PtyWrite { terminal_id, bytes } => {
                if !self.terminal_reply {
                    return Err(BridgeError::engine(
                        "terminal generated a PTY reply but HELLO_OK did not advertise TERMINAL_REPLY",
                    ));
                }
                if bytes.is_empty()
                    || bytes.len() > phux_protocol::wire::frame::MAX_INPUT_TERMINAL_REPLY_BYTES
                {
                    return Err(BridgeError::engine(
                        "terminal reply is empty or exceeds the protocol byte limit",
                    ));
                }
                self.queue_frame(FrameKind::InputTerminalReply {
                    terminal_id,
                    bytes: bytes.into(),
                })?;
            }
            KernelSend::FrameAck {
                terminal_id,
                stream_id,
                bootstrap_id,
                seq,
            } => {
                self.queue_frame(FrameKind::FrameAck {
                    terminal_id,
                    stream_id,
                    bootstrap_id,
                    seq,
                })?;
            }
            KernelSend::HistoryRequest {
                key,
                cursor,
                max_bytes,
                max_rows,
            } => {
                self.queue_frame(FrameKind::HistoryRequest {
                    terminal_id: key.terminal_id,
                    stream_id: key.stream_id,
                    bootstrap_id: key.bootstrap_id,
                    cursor: cursor.into(),
                    max_bytes,
                    max_rows,
                })?;
            }
        }
        Ok(())
    }

    fn process_status(&mut self, status: KernelStatus) {
        match status {
            KernelStatus::Engine { key, status } => {
                let mut out = OwnedEffect::simple(2, 0, key.terminal_id);
                out.stream_id = u64::from(key.stream_id.get());
                out.bootstrap_id = u64::from(key.bootstrap_id.get());
                match status {
                    phux_client_core::engine::EngineStatus::Bell => out.detail = 1,
                    phux_client_core::engine::EngineStatus::Title(title) => {
                        out.detail = 2;
                        out.bytes = title.into_bytes();
                    }
                }
                self.owned_effects.push(out);
            }
            KernelStatus::ResyncRequired {
                terminal_id,
                stream_id,
                bootstrap_id,
                reason,
            } => {
                let mut out = OwnedEffect::simple(2, 3, terminal_id);
                out.stream_id = u64::from(stream_id.get());
                out.bootstrap_id = u64::from(bootstrap_id.get());
                out.status_code = u32::from(reason.as_wire());
                self.owned_effects.push(out);
            }
            KernelStatus::History { key, status } => {
                let mut out = OwnedEffect::simple(2, 6, key.terminal_id);
                out.stream_id = u64::from(key.stream_id.get());
                out.bootstrap_id = u64::from(key.bootstrap_id.get());
                out.status_code = match status.state {
                    HistoryLoadState::Idle => 0,
                    HistoryLoadState::Loading => 1,
                    HistoryLoadState::Complete => 2,
                    HistoryLoadState::Gap => 3,
                    HistoryLoadState::Stale => 4,
                    HistoryLoadState::Pruned => 5,
                    HistoryLoadState::Tombstoned => 6,
                };
                self.owned_effects.push(out);
            }
            KernelStatus::HistoryUnavailable { key, reason } => {
                let mut out = OwnedEffect::simple(2, 7, key.terminal_id);
                out.stream_id = u64::from(key.stream_id.get());
                out.bootstrap_id = u64::from(key.bootstrap_id.get());
                out.status_code = match reason {
                    phux_client_core::session::HistoryUnavailableReason::Stale => 0,
                    phux_client_core::session::HistoryUnavailableReason::Pruned => 1,
                    phux_client_core::session::HistoryUnavailableReason::Reset => 2,
                    phux_client_core::session::HistoryUnavailableReason::Resize => 3,
                    phux_client_core::session::HistoryUnavailableReason::Expired => 4,
                    phux_client_core::session::HistoryUnavailableReason::Released => 5,
                    phux_client_core::session::HistoryUnavailableReason::Limit => 6,
                    phux_client_core::session::HistoryUnavailableReason::CodecFailure => 7,
                };
                self.owned_effects.push(out);
            }
        }
    }

    pub(crate) fn queue_frame(&mut self, kind: FrameKind) -> Result<(), BridgeError> {
        let mut encoded = bytes::BytesMut::new();
        kind.encode(&mut encoded);
        let body_len = encoded
            .len()
            .checked_sub(4)
            .ok_or_else(|| BridgeError::engine("protocol encoder produced a truncated frame"))?;
        if body_len > phux_protocol::wire::frame::MAX_FRAME_LEN as usize {
            return Err(BridgeError::invalid(
                "outbound frame exceeds the protocol length limit",
            ));
        }
        self.outgoing.push(encoded.to_vec());
        Ok(())
    }

    pub(crate) fn rebuild_effect_views(&mut self) {
        self.effect_views.clear();
        self.effect_views
            .extend(self.owned_effects.iter().map(|effect| PhuxClientEffect {
                kind: effect.kind,
                detail: effect.detail,
                status_code: effect.status_code,
                terminal_id: terminal_id_out(&effect.terminal_id),
                stream_id: effect.stream_id,
                bootstrap_id: effect.bootstrap_id,
                seq: effect.seq,
                first_row: effect.first_row,
                last_row: effect.last_row,
                bytes: bytes_out(&effect.bytes),
            }));
    }

    pub(crate) fn build_grid(
        &mut self,
        terminal_id: &TerminalId,
    ) -> Result<*const PhuxTerminalGridView, BridgeError> {
        let key = self.terminal_key(terminal_id)?;
        let revision = self.document_revision(terminal_id)?;
        let history = self
            .session
            .history_cache(terminal_id)
            .map(|cache| cache.status());
        let top_anchor = self.track_anchor(
            terminal_id,
            PhuxDocumentPoint {
                space: 1,
                row: 0,
                column: 0,
                reserved: 0,
            },
        )?;
        let result = (|| {
            let terminal =
                self.terminal(terminal_id)? as *const libghostty_vt::Terminal<'static, 'static>;
            let cache = self
                .render
                .entry(terminal_id.clone())
                .or_insert(RenderCache::new()?);
            // SAFETY: terminal is owned by session; render cache is disjoint bridge state and no
            // session mutation occurs until this method returns.
            let terminal = unsafe { &*terminal };
            let snapshot = cache.state.update(terminal).map_err(BridgeError::ghostty)?;
            let cols = snapshot.cols().map_err(BridgeError::ghostty)?;
            let rows = snapshot.rows().map_err(BridgeError::ghostty)?;
            let colors = snapshot.colors().map_err(BridgeError::ghostty)?;
            cache.grid_cells.clear();
            cache.utf8.clear();
            cache
                .grid_cells
                .reserve(usize::from(cols) * usize::from(rows));
            let mut graphemes = String::new();
            let mut hyperlink = vec![0_u8; 64];
            let mut row_index = 0_u32;
            let mut row_iter = cache.rows.update(&snapshot).map_err(BridgeError::ghostty)?;
            while let Some(row) = row_iter.next() {
                let mut column_index = 0_u16;
                let mut cell_iter = cache.cells.update(row).map_err(BridgeError::ghostty)?;
                while let Some(cell) = cell_iter.next() {
                    let raw = cell.raw_cell().map_err(BridgeError::ghostty)?;
                    let style = cell.style().map_err(BridgeError::ghostty)?;
                    let content_tag = raw.content_tag().map_err(BridgeError::ghostty)?;
                    let start = cache.utf8.len();
                    match content_tag {
                        CellContentTag::Codepoint => {
                            let cp = raw.codepoint().map_err(BridgeError::ghostty)?;
                            if cp != 0 {
                                let ch = char::from_u32(cp).ok_or_else(|| {
                                    BridgeError::engine("invalid terminal codepoint")
                                })?;
                                let mut encoded = [0_u8; 4];
                                cache
                                    .utf8
                                    .extend_from_slice(ch.encode_utf8(&mut encoded).as_bytes());
                            }
                        }
                        CellContentTag::CodepointGrapheme => {
                            graphemes.clear();
                            cell.graphemes_utf8(&mut graphemes)
                                .map_err(BridgeError::ghostty)?;
                            cache.utf8.extend_from_slice(graphemes.as_bytes());
                        }
                        CellContentTag::BgColorPalette | CellContentTag::BgColorRgb => {}
                    }
                    let cell_utf8_len = cache.utf8.len() - start;
                    const NO_HYPERLINK: (u32, u32) = (0, 0);
                    let has_hyperlink = raw.has_hyperlink().map_err(BridgeError::ghostty)?;
                    let (hyperlink_offset, hyperlink_len) = if has_hyperlink {
                        let reference = terminal
                            .grid_ref(Point::Viewport(PointCoordinate {
                                x: column_index,
                                y: row_index,
                            }))
                            .map_err(BridgeError::ghostty)?;
                        let len = loop {
                            match reference.hyperlink_uri(&mut hyperlink) {
                                Ok(len) => break len,
                                Err(libghostty_vt::Error::OutOfSpace { required })
                                    if required > hyperlink.len() =>
                                {
                                    hyperlink.resize(required, 0);
                                }
                                Err(error) => return Err(BridgeError::ghostty(error)),
                            }
                        };
                        let offset = u32::try_from(cache.utf8.len())
                            .map_err(|_| BridgeError::engine("cell UTF-8 arena exceeds u32"))?;
                        cache.utf8.extend_from_slice(&hyperlink[..len]);
                        (
                            offset,
                            u32::try_from(len)
                                .map_err(|_| BridgeError::engine("hyperlink URI exceeds u32"))?,
                        )
                    } else {
                        NO_HYPERLINK
                    };
                    let fg = resolve_color(style.fg_color, colors.foreground, &colors.palette);
                    let mut bg = resolve_color(style.bg_color, colors.background, &colors.palette);
                    let underline_color = resolve_color(style.underline_color, fg, &colors.palette);
                    bg = match content_tag {
                        CellContentTag::BgColorPalette => {
                            colors.palette[usize::from(
                                raw.bg_color_palette().map_err(BridgeError::ghostty)?.0,
                            )]
                        }
                        CellContentTag::BgColorRgb => {
                            raw.bg_color_rgb().map_err(BridgeError::ghostty)?
                        }
                        _ => bg,
                    };
                    let mut flags = 0;
                    if style.bold {
                        flags |= CELL_BOLD;
                    }
                    if style.italic {
                        flags |= CELL_ITALIC;
                    }
                    if style.faint {
                        flags |= CELL_FAINT;
                    }
                    if style.blink {
                        flags |= CELL_BLINK;
                    }
                    if style.inverse {
                        flags |= CELL_INVERSE;
                    }
                    if style.invisible {
                        flags |= CELL_INVISIBLE;
                    }
                    if style.strikethrough {
                        flags |= CELL_STRIKETHROUGH;
                    }
                    if style.overline {
                        flags |= CELL_OVERLINE;
                    }
                    if cell.is_selected().map_err(BridgeError::ghostty)? {
                        flags |= CELL_SELECTED;
                    }
                    if raw.is_protected().map_err(BridgeError::ghostty)? {
                        flags |= CELL_PROTECTED;
                    }
                    if has_hyperlink {
                        flags |= CELL_HYPERLINK;
                    }
                    cache.grid_cells.push(PhuxTerminalCell {
                        utf8_offset: u32::try_from(start)
                            .map_err(|_| BridgeError::engine("cell UTF-8 arena exceeds u32"))?,
                        utf8_len: u16::try_from(cell_utf8_len)
                            .map_err(|_| BridgeError::engine("cell grapheme exceeds u16"))?,
                        hyperlink_offset,
                        hyperlink_len,
                        content_tag: content_tag as u16,
                        wide: raw.wide().map_err(BridgeError::ghostty)? as u8,
                        semantic_content: raw.semantic_content().map_err(BridgeError::ghostty)?
                            as u8,
                        flags,
                        foreground_r: fg.r,
                        foreground_g: fg.g,
                        foreground_b: fg.b,
                        background_r: bg.r,
                        background_g: bg.g,
                        underline_r: underline_color.r,
                        underline_g: underline_color.g,
                        underline_b: underline_color.b,
                        background_b: bg.b,
                        underline: style.underline as u8,
                        reserved: 0,
                    });
                    column_index = column_index
                        .checked_add(1)
                        .ok_or_else(|| BridgeError::engine("render column exceeds u16"))?;
                }
                row_index = row_index
                    .checked_add(1)
                    .ok_or_else(|| BridgeError::engine("render row exceeds u32"))?;
            }
            let expected_cells = usize::from(cols)
                .checked_mul(usize::from(rows))
                .ok_or_else(|| BridgeError::engine("render grid dimensions overflow usize"))?;
            if cache.grid_cells.len() != expected_cells {
                return Err(BridgeError::engine(
                    "libghostty render iterator did not produce a dense viewport",
                ));
            }
            let cursor = snapshot.cursor_viewport().map_err(BridgeError::ghostty)?;
            let scrollbar = terminal.scrollbar().map_err(BridgeError::ghostty)?;
            let (pages, bytes, unread_rows, loading, has_more) =
                history.map_or((0, 0, 0, false, false), |status| {
                    (
                        status.loaded_pages as u64,
                        status.loaded_bytes as u64,
                        status.unread_rows,
                        status.state == HistoryLoadState::Loading,
                        status.next_cursor.is_some(),
                    )
                });
            cache.terminal_host.clear();
            let view_terminal_id = match terminal_id {
                TerminalId::Local { id } => PhuxTerminalId {
                    kind: 0,
                    id: *id,
                    host: PhuxBytes::default(),
                },
                TerminalId::Satellite { host, id } => {
                    cache
                        .terminal_host
                        .extend_from_slice(host.as_str().as_bytes());
                    PhuxTerminalId {
                        kind: 1,
                        id: *id,
                        host: bytes_out(&cache.terminal_host),
                    }
                }
            };
            cache.view = PhuxTerminalGridView {
                terminal_id: view_terminal_id,
                stream_id: u64::from(key.stream_id.get()),
                bootstrap_id: u64::from(key.bootstrap_id.get()),
                last_seq: self
                    .session
                    .published(terminal_id)
                    .map_or(0, |p| p.last_seq()),
                document_revision: revision,
                cols,
                rows,
                cells: if cache.grid_cells.is_empty() {
                    ptr::null()
                } else {
                    cache.grid_cells.as_ptr()
                },
                cell_count: cache.grid_cells.len(),
                utf8: bytes_out(&cache.utf8),
                cursor_visible: snapshot.cursor_visible().map_err(BridgeError::ghostty)?
                    && cursor.is_some(),
                cursor_col: cursor.map_or(0, |value| value.x),
                cursor_row: cursor.map_or(0, |value| value.y),
                cursor_style: cursor_style(
                    snapshot
                        .cursor_visual_style()
                        .map_err(BridgeError::ghostty)?,
                ),
                history_total_rows: scrollbar.total,
                history_viewport_offset: scrollbar.offset,
                history_visible_rows: scrollbar.len,
                history_pages_loaded: pages,
                history_unread_rows: unread_rows,
                history_bytes_loaded: bytes,
                history_loading: loading,
                history_has_more: has_more,
                top_anchor,
            };
            Ok(ptr::from_ref(&cache.view))
        })();
        if result.is_err() {
            let _ = self.release_anchor(terminal_id, top_anchor);
        }
        result
    }

    pub(crate) fn scroll(
        &mut self,
        terminal_id: &TerminalId,
        kind: u32,
        value: i64,
    ) -> Result<(), BridgeError> {
        self.ensure_attached()?;
        let scroll = match kind {
            0 => ScrollViewport::Top,
            1 => ScrollViewport::Bottom,
            2 => ScrollViewport::Delta(
                isize::try_from(value)
                    .map_err(|_| BridgeError::invalid("scroll delta exceeds isize"))?,
            ),
            3 => ScrollViewport::Row(
                usize::try_from(value)
                    .map_err(|_| BridgeError::invalid("scroll row must be non-negative"))?,
            ),
            _ => return Err(BridgeError::invalid("unknown viewport scroll kind")),
        };
        self.session
            .published_engine_mut(terminal_id)
            .ok_or_else(|| BridgeError::state("terminal has no published READY generation"))?
            .scroll_viewport(scroll)
            .map_err(|error| BridgeError::engine(error.to_string()))?;
        let scrollbar = self
            .terminal(terminal_id)?
            .scrollbar()
            .map_err(BridgeError::ghostty)?;
        let at_tail = scrollbar.offset.saturating_add(scrollbar.len) >= scrollbar.total;
        if at_tail {
            self.session
                .follow_history_tail(terminal_id)
                .map_err(|error| BridgeError::engine(error.to_string()))?;
            if let Some(old) = self.viewport_anchors.remove(terminal_id) {
                self.session
                    .release_document_anchor(terminal_id, old)
                    .map_err(|error| BridgeError::engine(error.to_string()))?;
            }
        } else {
            let anchor = self
                .session
                .track_document_anchor(
                    terminal_id,
                    DocumentPoint {
                        space: DocumentSpace::Viewport,
                        x: 0,
                        y: 0,
                    },
                )
                .map_err(|error| BridgeError::engine(error.to_string()))?;
            self.session
                .pin_history_viewport(terminal_id, anchor)
                .map_err(|error| BridgeError::engine(error.to_string()))?;
            if let Some(old) = self.viewport_anchors.insert(terminal_id.clone(), anchor) {
                self.session
                    .release_document_anchor(terminal_id, old)
                    .map_err(|error| BridgeError::engine(error.to_string()))?;
            }
        }
        let rows_from_oldest = usize::try_from(scrollbar.offset)
            .map_err(|_| BridgeError::engine("history viewport offset exceeds usize"))?;
        self.effects.clear();
        if self
            .session
            .prefetch_history(terminal_id, rows_from_oldest, &mut self.effects)
        {
            self.process_effects()?;
        }
        Ok(())
    }

    pub(crate) fn set_selection(
        &mut self,
        terminal_id: &TerminalId,
        start: PhuxDocumentAnchor,
        end: PhuxDocumentAnchor,
        rectangular: bool,
    ) -> Result<(), BridgeError> {
        self.ensure_attached()?;
        let start = self.resolve_anchor(terminal_id, start)?;
        let end = self.resolve_anchor(terminal_id, end)?;
        let selection = EngineDocumentSelection {
            start,
            end,
            rectangle: rectangular,
        };
        let start_point = self
            .session
            .document_anchor_point(terminal_id, start, DocumentSpace::Viewport)
            .map_err(|error| BridgeError::engine(error.to_string()))?;
        let end_point = self
            .session
            .document_anchor_point(terminal_id, end, DocumentSpace::Viewport)
            .map_err(|error| BridgeError::engine(error.to_string()))?;
        let terminal = self.terminal(terminal_id)?;
        if let (Some(start_point), Some(end_point)) = (start_point, end_point) {
            let start = terminal
                .grid_ref(Point::Viewport(PointCoordinate {
                    x: start_point.x,
                    y: start_point.y,
                }))
                .map_err(BridgeError::ghostty)?;
            let end = terminal
                .grid_ref(Point::Viewport(PointCoordinate {
                    x: end_point.x,
                    y: end_point.y,
                }))
                .map_err(BridgeError::ghostty)?;
            terminal
                .set_selection(Some(&Selection::new(start, end, rectangular)))
                .map_err(BridgeError::ghostty)?;
        } else {
            terminal.set_selection(None).map_err(BridgeError::ghostty)?;
        }
        self.selections.insert(terminal_id.clone(), selection);
        Ok(())
    }

    pub(crate) fn clear_selection(&mut self, terminal_id: &TerminalId) -> Result<(), BridgeError> {
        self.terminal(terminal_id)?
            .set_selection(None)
            .map_err(BridgeError::ghostty)?;
        self.selections.remove(terminal_id);
        Ok(())
    }

    pub(crate) fn selection_text(&mut self, terminal_id: &TerminalId) -> Result<(), BridgeError> {
        self.ensure_attached()?;
        let selection = self
            .selections
            .get(terminal_id)
            .copied()
            .ok_or_else(|| BridgeError::state("terminal has no active selection"))?;
        let text = self
            .session
            .format_document_selection(terminal_id, selection)
            .map_err(|error| BridgeError::engine(error.to_string()))?
            .unwrap_or_default();
        self.selection_buf = text.into_bytes();
        Ok(())
    }

    pub(crate) fn search(
        &mut self,
        terminal_id: &TerminalId,
        query: &[u8],
        case_sensitive: bool,
    ) -> Result<(), BridgeError> {
        self.ensure_attached()?;
        let query = std::str::from_utf8(query)
            .map_err(|_| BridgeError::invalid("search query is not UTF-8"))?;
        if query.is_empty() {
            return Err(BridgeError::invalid("search query is empty"));
        }
        if !case_sensitive {
            return Err(BridgeError::invalid(
                "case-insensitive native search is unsupported",
            ));
        }
        let matches = self
            .session
            .search_loaded_history(terminal_id, query, 4096)
            .map_err(|error| BridgeError::engine(error.to_string()))?;
        let mut found = Vec::with_capacity(matches.len());
        for matched in matches {
            let start = self.register_anchor(terminal_id, matched.start)?;
            let end = self.register_anchor(terminal_id, matched.end)?;
            found.push(PhuxSearchResult { start, end });
        }
        self.search_results = found;
        Ok(())
    }
}

fn engine_limits(limits: Limits) -> BootstrapLimits {
    BootstrapLimits::new(limits.bootstrap_chunk, limits.history_page)
        .expect("client limits were validated at construction")
}

fn resolve_color(color: StyleColor, fallback: RgbColor, palette: &[RgbColor; 256]) -> RgbColor {
    match color {
        StyleColor::None => fallback,
        StyleColor::Palette(index) => palette[usize::from(index.0)],
        StyleColor::Rgb(rgb) => rgb,
    }
}

fn cursor_style(style: CursorVisualStyle) -> u32 {
    match style {
        CursorVisualStyle::Bar => 0,
        CursorVisualStyle::Block => 1,
        CursorVisualStyle::Underline => 2,
        CursorVisualStyle::BlockHollow => 3,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_reply_requires_explicit_hello_ok_feature() {
        let mut client = Client::new(Limits {
            bootstrap_chunk: 1024,
            history_page: 1024,
            history_page_rows: 128,
            history_cache_bytes: 4096,
            history_materialized_rows: 1024,
            history_prefetch_rows: 64,
        });
        let error = client
            .process_send(KernelSend::PtyWrite {
                terminal_id: TerminalId::local(7),
                bytes: b"\x1b[1;1R".to_vec(),
            })
            .expect_err("old protocol-0.7 peers must reject terminal replies");
        assert_eq!(error.result, PhuxClientResult::EngineError);
        assert!(client.outgoing.is_empty());
    }

    #[test]
    fn terminal_reply_is_exactly_encoded_when_negotiated() {
        let mut client = Client::new(Limits {
            bootstrap_chunk: 1024,
            history_page: 1024,
            history_page_rows: 128,
            history_cache_bytes: 4096,
            history_materialized_rows: 1024,
            history_prefetch_rows: 64,
        });
        client.terminal_reply = true;
        client
            .process_send(KernelSend::PtyWrite {
                terminal_id: TerminalId::local(7),
                bytes: b"\x1b[1;1R".to_vec(),
            })
            .expect("explicit TERMINAL_REPLY feature");

        let (decoded, remaining) =
            FrameKind::decode(&client.outgoing[0]).expect("decode generated frame");
        assert!(remaining.is_empty());
        assert!(matches!(
            decoded,
            FrameKind::InputTerminalReply {
                terminal_id,
                bytes,
            } if terminal_id == TerminalId::local(7) && bytes.as_ref() == b"\x1b[1;1R"
        ));
    }

    #[test]
    fn terminal_reply_enforces_exact_payload_bounds_before_queueing() {
        let limits = Limits {
            bootstrap_chunk: 1024,
            history_page: 1024,
            history_page_rows: 128,
            history_cache_bytes: 4096,
            history_materialized_rows: 1024,
            history_prefetch_rows: 64,
        };
        let mut client = Client::new(limits);
        client.terminal_reply = true;
        let cap = phux_protocol::wire::frame::MAX_INPUT_TERMINAL_REPLY_BYTES;
        client
            .process_send(KernelSend::PtyWrite {
                terminal_id: TerminalId::local(7),
                bytes: vec![b'x'; cap],
            })
            .expect("exact-cap terminal reply");
        let (decoded, remaining) =
            FrameKind::decode(&client.outgoing[0]).expect("exact-cap reply decodes");
        assert!(remaining.is_empty());
        assert!(matches!(
            decoded,
            FrameKind::InputTerminalReply { bytes, .. } if bytes.len() == cap
        ));

        client.outgoing.clear();
        for bytes in [Vec::new(), vec![b'x'; cap + 1]] {
            let error = client
                .process_send(KernelSend::PtyWrite {
                    terminal_id: TerminalId::local(7),
                    bytes,
                })
                .expect_err("invalid terminal reply payload");
            assert_eq!(error.result, PhuxClientResult::EngineError);
            assert!(client.outgoing.is_empty());
        }
    }

    #[test]
    fn underline_color_falls_back_to_resolved_cell_foreground() {
        let palette = [RgbColor { r: 0, g: 0, b: 0 }; 256];
        let cell_foreground = RgbColor {
            r: 0x12,
            g: 0x34,
            b: 0x56,
        };
        assert_eq!(
            resolve_color(StyleColor::None, cell_foreground, &palette),
            cell_foreground
        );
        let explicit = RgbColor {
            r: 0x65,
            g: 0x43,
            b: 0x21,
        };
        assert_eq!(
            resolve_color(StyleColor::Rgb(explicit), cell_foreground, &palette),
            explicit
        );
    }
}
