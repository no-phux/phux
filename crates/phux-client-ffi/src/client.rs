use std::collections::{HashMap, HashSet};
use std::ptr;
use std::time::{Duration, Instant};

use libghostty_vt::fmt::Format;
use libghostty_vt::render::{CellIterator, CursorVisualStyle, RenderState, RowIterator};
use libghostty_vt::screen::{CellContentTag, TrackedGridRef};
use libghostty_vt::selection::{FormatOptions, Selection};
use libghostty_vt::style::{RgbColor, StyleColor};
use libghostty_vt::terminal::{Mode, Point, PointCoordinate, ScrollViewport};
use libghostty_vt::{Terminal, TerminalOptions};
use phux_protocol::TerminalId;
use phux_protocol::input::InputEvent;
use phux_protocol::wire::frame::FrameKind;

use crate::error::BridgeError;
use crate::types::{
    CELL_BLINK, CELL_BOLD, CELL_FAINT, CELL_HYPERLINK, CELL_INVERSE, CELL_INVISIBLE, CELL_ITALIC,
    CELL_OVERLINE, CELL_PROTECTED, CELL_SELECTED, CELL_STRIKETHROUGH, OwnedEffect, PhuxBytes,
    PhuxClientCallbacks, PhuxClientEffect, PhuxClientState, PhuxDocumentAnchor, PhuxDocumentPoint,
    PhuxSearchResult, PhuxTerminalCell, PhuxTerminalGridView, PhuxTerminalId, bytes_out,
    terminal_id_out,
};

const NO_HYPERLINK: (u32, u32) = (0, 0);
const FALLBACK_CELL_PX: (u32, u32) = (8, 16);
const SYNC_OUTPUT_WATCHDOG: Duration = Duration::from_secs(1);

struct RenderCache {
    state: RenderState<'static>,
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
    grid_cells: Vec<PhuxTerminalCell>,
    utf8: Vec<u8>,
    terminal_host: Vec<u8>,
    view: PhuxTerminalGridView,
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

struct Replica {
    terminal: Terminal<'static, 'static>,
    render: RenderCache,
    generation: u64,
    last_seq: u64,
    document_revision: u64,
    sync_output_since: Option<Instant>,
    sync_output_dirty: bool,
}

impl Replica {
    fn new(
        cols: u16,
        rows: u16,
        document_revision: u64,
        generation: u64,
        max_scrollback: usize,
    ) -> Result<Self, BridgeError> {
        let mut terminal = Terminal::new(TerminalOptions {
            cols: cols.max(1),
            rows: rows.max(1),
            max_scrollback,
        })
        .map_err(BridgeError::ghostty)?;
        terminal
            .resize(
                cols.max(1),
                rows.max(1),
                FALLBACK_CELL_PX.0,
                FALLBACK_CELL_PX.1,
            )
            .map_err(BridgeError::ghostty)?;
        Ok(Self {
            terminal,
            render: RenderCache::new()?,
            generation,
            last_seq: 0,
            document_revision,
            sync_output_since: None,
            sync_output_dirty: false,
        })
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent handshake, attachment, detach, and callback guards"
)]
#[allow(
    clippy::redundant_pub_crate,
    reason = "the private bridge module exposes its owning state to the crate-root C exports"
)]
pub(crate) struct Client {
    replicas: HashMap<TerminalId, Replica>,
    participants: HashSet<TerminalId>,
    pending_snapshots: HashSet<TerminalId>,
    anchors: HashMap<u64, (TerminalId, TrackedGridRef)>,
    next_anchor_handle: u64,
    next_document_revision: u64,
    next_replica_generation: u64,
    pub max_scrollback: usize,
    pub outgoing: Vec<Vec<u8>>,
    pub owned_effects: Vec<OwnedEffect>,
    pub effect_views: Vec<PhuxClientEffect>,
    pub selection_buf: Vec<u8>,
    pub search_results: Vec<PhuxSearchResult>,
    pub last_error: Vec<u8>,
    pub protocol_ready: bool,
    pub hello_queued: bool,
    pub callbacks: PhuxClientCallbacks,
    pub in_callback: bool,
    pub attached_notified: bool,
    pub attach_queued: bool,
    pub attached: bool,
    pub detached: bool,
    pub failed: bool,
}

impl Client {
    pub(super) fn new() -> Self {
        Self {
            replicas: HashMap::new(),
            participants: HashSet::new(),
            pending_snapshots: HashSet::new(),
            anchors: HashMap::new(),
            next_anchor_handle: 1,
            next_document_revision: 1,
            next_replica_generation: 1,
            max_scrollback: 0,
            outgoing: Vec::new(),
            owned_effects: Vec::new(),
            effect_views: Vec::new(),
            selection_buf: Vec::new(),
            search_results: Vec::new(),
            last_error: Vec::new(),
            protocol_ready: false,
            hello_queued: false,
            callbacks: PhuxClientCallbacks::default(),
            in_callback: false,
            attached_notified: false,
            attach_queued: false,
            attached: false,
            detached: false,
            failed: false,
        }
    }

    pub(super) fn reset_borrows(&mut self) {
        self.selection_buf.clear();
        self.search_results.clear();
    }

    pub(super) fn release_search_results(&mut self) {
        for result in &self.search_results {
            self.anchors.remove(&result.start.opaque_id);
            self.anchors.remove(&result.end.opaque_id);
        }
        self.search_results.clear();
    }

    pub(super) fn state(&self) -> PhuxClientState {
        if self.failed {
            PhuxClientState::Failed
        } else if self.detached {
            PhuxClientState::Detached
        } else if self.attached && self.pending_snapshots.is_empty() {
            PhuxClientState::Attached
        } else if self.protocol_ready {
            PhuxClientState::Negotiated
        } else if self.hello_queued {
            PhuxClientState::HelloQueued
        } else {
            PhuxClientState::New
        }
    }

    pub(super) fn set_error(&mut self, message: impl AsRef<str>) {
        self.last_error.clear();
        self.last_error
            .extend_from_slice(message.as_ref().as_bytes());
    }

    pub(super) fn ensure_attached(&self) -> Result<(), BridgeError> {
        if self.attached && !self.detached {
            Ok(())
        } else {
            Err(BridgeError::state("operation requires an attached client"))
        }
    }

    fn ensure_participant(&self, terminal_id: &TerminalId) -> Result<(), BridgeError> {
        if !self.participants.contains(terminal_id) {
            return Err(BridgeError::protocol(
                "terminal frame targets a terminal outside the active attachment",
            ));
        }
        Ok(())
    }

    pub(super) fn ensure_replica(&self, terminal_id: &TerminalId) -> Result<(), BridgeError> {
        self.ensure_attached()?;
        self.replicas
            .contains_key(terminal_id)
            .then_some(())
            .ok_or_else(|| BridgeError::state("terminal has no authoritative snapshot"))
    }

    pub(super) fn queue_input(
        &mut self,
        terminal_id: TerminalId,
        event: InputEvent,
    ) -> Result<(), BridgeError> {
        self.ensure_replica(&terminal_id)?;
        self.queue_frame(&event.into_frame(terminal_id))
    }

    fn detach(&mut self) {
        self.anchors.clear();
        self.replicas.clear();
        self.participants.clear();
        self.pending_snapshots.clear();
        self.attach_queued = false;
        self.attached = false;
        self.detached = true;
    }

    #[allow(
        clippy::too_many_lines,
        reason = "ordered frame validation and mutation stay in one auditable dispatcher"
    )]
    pub(super) fn feed(&mut self, frame: FrameKind) -> Result<bool, BridgeError> {
        if self.failed {
            return Err(BridgeError::state(
                "server frame arrived after client failure",
            ));
        }
        if !self.protocol_ready
            && !matches!(&frame, FrameKind::HelloOk { .. } | FrameKind::Error { .. })
        {
            return Err(BridgeError::state("server frame arrived before HELLO_OK"));
        }
        if self.detached {
            return Err(BridgeError::protocol("server frame arrived after DETACHED"));
        }

        let mut notify_attached = false;
        match frame {
            FrameKind::HelloOk {
                protocol_major,
                protocol_minor,
                ..
            } => {
                if protocol_major != phux_protocol::PROTOCOL_VERSION.major
                    || protocol_minor != phux_protocol::PROTOCOL_VERSION.minor
                {
                    self.failed = true;
                    return Err(BridgeError::protocol(
                        "server selected an unsupported protocol version",
                    ));
                }
                if !self.hello_queued || self.protocol_ready {
                    return Err(BridgeError::protocol("unsolicited or duplicate HELLO_OK"));
                }
                self.protocol_ready = true;
            }
            FrameKind::Ping { nonce } => self.queue_frame(&FrameKind::Pong { nonce })?,
            FrameKind::Pong { .. } => {}
            FrameKind::Attached { snapshot, .. } => {
                if !self.attach_queued || self.attached {
                    return Err(BridgeError::protocol("unsolicited or duplicate ATTACHED"));
                }
                self.participants = snapshot
                    .panes
                    .iter()
                    .filter(|pane| {
                        snapshot.windows.iter().any(|window| {
                            window.id == pane.window_id
                                && window.session_id == snapshot.focused_session
                        })
                    })
                    .map(|pane| pane.id.clone())
                    .collect();
                if self.participants.is_empty() {
                    self.participants.insert(snapshot.focused_pane);
                }
                self.pending_snapshots.clone_from(&self.participants);
                self.attach_queued = false;
                self.attached = true;
                notify_attached = self.pending_snapshots.is_empty();
            }
            FrameKind::TerminalSnapshot {
                terminal_id,
                cols,
                rows,
                vt_replay_bytes,
                scrollback_bytes,
            } => {
                self.ensure_participant(&terminal_id)?;
                if cols == 0 || rows == 0 {
                    return Err(BridgeError::protocol("terminal snapshot geometry is zero"));
                }
                self.invalidate_terminal_handles(&terminal_id);
                let preserve_sync = self.replicas.get(&terminal_id).and_then(|replica| {
                    replica
                        .sync_output_dirty
                        .then_some(replica.sync_output_since)
                        .flatten()
                });
                let generation = self.next_generation()?;
                let revision = self.next_revision()?;
                let mut replica =
                    Replica::new(cols, rows, revision, generation, self.max_scrollback)?;
                if let Some(scrollback) = scrollback_bytes {
                    replica.terminal.vt_write(&scrollback);
                }
                replica.terminal.vt_write(&vt_replay_bytes);
                let title = replica.terminal.title().unwrap_or_default().to_owned();
                let sync_active = replica.terminal.mode(Mode::SYNC_OUTPUT).unwrap_or(false);
                replica.sync_output_since =
                    preserve_sync.or_else(|| sync_active.then(Instant::now));
                replica.sync_output_dirty = preserve_sync.is_some() || sync_active;
                let publish_damage = !replica.sync_output_dirty;
                self.replicas.insert(terminal_id.clone(), replica);
                if publish_damage {
                    let mut damage = OwnedEffect::simple(1, 1, terminal_id.clone());
                    damage.stream_id = generation;
                    self.owned_effects.push(damage);
                } else {
                    let mut present = OwnedEffect::simple(1, 4, terminal_id.clone());
                    present.stream_id = generation;
                    self.owned_effects.push(present);
                }
                if !title.is_empty() {
                    let mut effect = OwnedEffect::simple(2, 2, terminal_id.clone());
                    effect.stream_id = generation;
                    effect.bytes = title.into_bytes();
                    self.owned_effects.push(effect);
                }
                self.pending_snapshots.remove(&terminal_id);
                notify_attached = self.pending_snapshots.is_empty() && !self.attached_notified;
                self.rebuild_effect_views();
            }
            FrameKind::TerminalOutput {
                terminal_id,
                seq,
                bytes,
            } => {
                self.ensure_participant(&terminal_id)?;
                let replica = self.replicas.get(&terminal_id).ok_or_else(|| {
                    BridgeError::protocol("terminal output arrived before snapshot")
                })?;
                if seq != 0 && seq <= replica.last_seq {
                    self.queue_frame(&FrameKind::FrameAck { terminal_id, seq })?;
                    return Ok(false);
                }
                let revision = self.next_revision()?;
                let (old_title, new_title, publish_damage, generation) = {
                    let replica = self.replicas.get_mut(&terminal_id).ok_or_else(|| {
                        BridgeError::protocol("terminal output arrived before snapshot")
                    })?;
                    let old_title = replica.terminal.title().unwrap_or_default().to_owned();
                    replica.terminal.vt_write(&bytes);
                    replica.last_seq = seq;
                    replica.document_revision = revision;
                    let new_title = replica.terminal.title().unwrap_or_default().to_owned();
                    let sync_active = replica.terminal.mode(Mode::SYNC_OUTPUT).unwrap_or(false);
                    let publish_damage = if sync_active {
                        replica.sync_output_since.get_or_insert_with(Instant::now);
                        replica.sync_output_dirty = true;
                        false
                    } else {
                        replica.sync_output_since = None;
                        let was_dirty = replica.sync_output_dirty;
                        replica.sync_output_dirty = false;
                        was_dirty || !bytes.is_empty()
                    };
                    (old_title, new_title, publish_damage, replica.generation)
                };
                if seq != 0 {
                    self.queue_frame(&FrameKind::FrameAck {
                        terminal_id: terminal_id.clone(),
                        seq,
                    })?;
                }
                if publish_damage {
                    let mut damage = OwnedEffect::simple(1, 1, terminal_id.clone());
                    damage.stream_id = generation;
                    damage.seq = seq;
                    self.owned_effects.push(damage);
                }
                if old_title != new_title {
                    let mut effect = OwnedEffect::simple(2, 2, terminal_id.clone());
                    effect.stream_id = generation;
                    effect.bytes = new_title.into_bytes();
                    self.owned_effects.push(effect);
                }
                self.rebuild_effect_views();
            }
            FrameKind::TerminalClosed { terminal_id, .. } => {
                self.ensure_participant(&terminal_id)?;
                self.invalidate_terminal_handles(&terminal_id);
                let generation = self
                    .replicas
                    .remove(&terminal_id)
                    .map_or(0, |replica| replica.generation);
                self.participants.remove(&terminal_id);
                self.pending_snapshots.remove(&terminal_id);
                let mut removed = OwnedEffect::simple(1, 3, terminal_id);
                removed.stream_id = generation;
                self.owned_effects.push(removed);
                notify_attached = self.pending_snapshots.is_empty() && !self.attached_notified;
                self.rebuild_effect_views();
            }
            FrameKind::Bell { terminal_id } => {
                self.ensure_participant(&terminal_id)?;
                let generation = self
                    .replicas
                    .get(&terminal_id)
                    .map_or(0, |replica| replica.generation);
                let mut bell = OwnedEffect::simple(2, 1, terminal_id);
                bell.stream_id = generation;
                self.owned_effects.push(bell);
                self.rebuild_effect_views();
            }
            FrameKind::Error { code, message, .. } => {
                if !self.protocol_ready {
                    self.hello_queued = false;
                    self.failed = true;
                    self.set_error(&message);
                } else if self.attach_queued {
                    self.attach_queued = false;
                }
                let mut effect = OwnedEffect::simple(2, 4, TerminalId::local(0));
                effect.bytes = format!("{code:?}: {message}").into_bytes();
                self.owned_effects.push(effect);
                self.rebuild_effect_views();
            }
            FrameKind::Detached => {
                if !self.attach_queued && !self.attached {
                    return Err(BridgeError::protocol(
                        "DETACHED arrived outside an active attachment",
                    ));
                }
                self.detach();
                self.owned_effects
                    .push(OwnedEffect::simple(2, 5, TerminalId::local(0)));
                self.rebuild_effect_views();
            }
            _ => {
                return Err(BridgeError::protocol(
                    "server sent a frame outside the native projection contract",
                ));
            }
        }
        Ok(notify_attached)
    }

    pub(super) fn maintenance(&mut self) {
        self.maintenance_at(Instant::now());
    }

    pub(super) fn maintenance_pending(&self) -> bool {
        self.replicas
            .values()
            .any(|replica| replica.sync_output_dirty && replica.sync_output_since.is_some())
    }

    fn maintenance_at(&mut self, now: Instant) {
        let mut expired = Vec::new();
        for (terminal_id, replica) in &mut self.replicas {
            if replica.sync_output_dirty
                && replica.sync_output_since.is_some_and(|since| {
                    now.saturating_duration_since(since) >= SYNC_OUTPUT_WATCHDOG
                })
            {
                replica.sync_output_since = None;
                replica.sync_output_dirty = false;
                expired.push((terminal_id.clone(), replica.generation, replica.last_seq));
            }
        }
        if expired.is_empty() {
            return;
        }
        self.owned_effects
            .extend(expired.into_iter().map(|(terminal_id, generation, seq)| {
                let mut damage = OwnedEffect::simple(1, 1, terminal_id);
                damage.stream_id = generation;
                damage.seq = seq;
                damage
            }));
        self.rebuild_effect_views();
    }

    fn next_revision(&mut self) -> Result<u64, BridgeError> {
        let revision = self.next_document_revision;
        self.next_document_revision = self
            .next_document_revision
            .checked_add(1)
            .ok_or_else(|| BridgeError::engine("document revision space exhausted"))?;
        Ok(revision)
    }

    fn next_generation(&mut self) -> Result<u64, BridgeError> {
        let generation = self.next_replica_generation;
        self.next_replica_generation = self
            .next_replica_generation
            .checked_add(1)
            .ok_or_else(|| BridgeError::engine("replica generation space exhausted"))?;
        Ok(generation)
    }

    fn publish_local_damage(&mut self, terminal_id: &TerminalId) -> Result<(), BridgeError> {
        let revision = self.next_revision()?;
        let replica = self
            .replicas
            .get_mut(terminal_id)
            .ok_or_else(|| BridgeError::state("terminal has no authoritative snapshot"))?;
        replica.document_revision = revision;
        let mut damage = OwnedEffect::simple(1, 1, terminal_id.clone());
        damage.stream_id = replica.generation;
        damage.seq = replica.last_seq;
        self.owned_effects.push(damage);
        self.rebuild_effect_views();
        Ok(())
    }

    pub(super) fn queue_frame(&mut self, kind: &FrameKind) -> Result<(), BridgeError> {
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

    pub(super) fn rebuild_effect_views(&mut self) {
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

    #[allow(
        clippy::too_many_lines,
        reason = "building one borrowed dense grid keeps render state and arenas cohesive"
    )]
    pub(super) fn build_grid(
        &mut self,
        terminal_id: &TerminalId,
    ) -> Result<*const PhuxTerminalGridView, BridgeError> {
        self.ensure_replica(terminal_id)?;
        let replica = self
            .replicas
            .get_mut(terminal_id)
            .ok_or_else(|| BridgeError::state("terminal has no authoritative snapshot"))?;
        let terminal = &replica.terminal;
        let cache = &mut replica.render;
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
                            let ch = char::from_u32(cp)
                                .ok_or_else(|| BridgeError::engine("invalid terminal codepoint"))?;
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
                        colors.palette
                            [usize::from(raw.bg_color_palette().map_err(BridgeError::ghostty)?.0)]
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
                    semantic_content: raw.semantic_content().map_err(BridgeError::ghostty)? as u8,
                    flags,
                    foreground_r: fg.r,
                    foreground_g: fg.g,
                    foreground_b: fg.b,
                    background_r: bg.r,
                    background_g: bg.g,
                    background_b: bg.b,
                    underline: style.underline as u8,
                    underline_r: underline_color.r,
                    underline_g: underline_color.g,
                    underline_b: underline_color.b,
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
            stream_id: replica.generation,
            bootstrap_id: 0,
            last_seq: replica.last_seq,
            document_revision: replica.document_revision,
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
            history_pages_loaded: 0,
            history_unread_rows: 0,
            history_bytes_loaded: 0,
            history_loading: false,
            history_has_more: false,
            top_anchor: PhuxDocumentAnchor::default(),
        };
        Ok(ptr::from_ref(&cache.view))
    }

    pub(super) fn mouse_tracking(&self, terminal_id: &TerminalId) -> Result<bool, BridgeError> {
        self.ensure_replica(terminal_id)?;
        let terminal = &self
            .replicas
            .get(terminal_id)
            .ok_or_else(|| BridgeError::state("terminal has no authoritative snapshot"))?
            .terminal;
        Ok(terminal_wants_mouse_tracking(terminal))
    }

    pub(super) fn scroll(
        &mut self,
        terminal_id: &TerminalId,
        kind: u32,
        value: i64,
    ) -> Result<(), BridgeError> {
        self.ensure_replica(terminal_id)?;
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
        self.replicas
            .get_mut(terminal_id)
            .ok_or_else(|| BridgeError::state("terminal has no authoritative snapshot"))?
            .terminal
            .scroll_viewport(scroll);
        self.publish_local_damage(terminal_id)
    }

    pub(super) fn track_anchor(
        &mut self,
        terminal_id: &TerminalId,
        point: PhuxDocumentPoint,
    ) -> Result<PhuxDocumentAnchor, BridgeError> {
        self.ensure_replica(terminal_id)?;
        if point.reserved != 0 {
            return Err(BridgeError::invalid(
                "document point reserved field must be zero",
            ));
        }
        let coordinate = PointCoordinate {
            x: point.column,
            y: point.row,
        };
        let point = match point.space {
            0 => Point::History(coordinate),
            1 => Point::Viewport(coordinate),
            2 => Point::Active(coordinate),
            _ => return Err(BridgeError::invalid("unknown document point space")),
        };
        let tracked = self
            .replicas
            .get(terminal_id)
            .ok_or_else(|| BridgeError::state("terminal has no authoritative snapshot"))?
            .terminal
            .track_grid_ref(point)
            .map_err(BridgeError::ghostty)?;
        let handle = self.next_anchor_handle;
        self.next_anchor_handle = self
            .next_anchor_handle
            .checked_add(1)
            .ok_or_else(|| BridgeError::engine("document anchor handle space exhausted"))?;
        self.anchors.insert(handle, (terminal_id.clone(), tracked));
        Ok(PhuxDocumentAnchor { opaque_id: handle })
    }

    pub(super) fn release_anchor(
        &mut self,
        terminal_id: &TerminalId,
        anchor: PhuxDocumentAnchor,
    ) -> Result<(), BridgeError> {
        self.ensure_attached()?;
        let Some((owner, _)) = self.anchors.get(&anchor.opaque_id) else {
            return Err(BridgeError::state("document anchor is stale or unknown"));
        };
        if owner != terminal_id {
            return Err(BridgeError::state(
                "document anchor belongs to another terminal",
            ));
        }
        self.anchors.remove(&anchor.opaque_id);
        Ok(())
    }

    pub(super) fn pin_viewport(
        &mut self,
        terminal_id: &TerminalId,
        anchor: PhuxDocumentAnchor,
    ) -> Result<(), BridgeError> {
        self.ensure_replica(terminal_id)?;
        let (owner, tracked) = self
            .anchors
            .get(&anchor.opaque_id)
            .ok_or_else(|| BridgeError::state("document anchor is stale or unknown"))?;
        if owner != terminal_id {
            return Err(BridgeError::state(
                "document anchor belongs to another terminal",
            ));
        }
        let row = tracked
            .point(libghostty_vt::terminal::PointSpace::Screen)
            .map_err(BridgeError::ghostty)?
            .ok_or_else(|| BridgeError::state("document anchor no longer resolves"))?
            .y;
        self.scroll(terminal_id, 3, i64::from(row))
    }

    pub(super) fn follow_live(&mut self, terminal_id: &TerminalId) -> Result<(), BridgeError> {
        self.scroll(terminal_id, 1, 0)
    }

    pub(super) fn set_selection(
        &mut self,
        terminal_id: &TerminalId,
        start: PhuxDocumentAnchor,
        end: PhuxDocumentAnchor,
        rectangular: bool,
    ) -> Result<(), BridgeError> {
        self.ensure_replica(terminal_id)?;
        let replica = self
            .replicas
            .get(terminal_id)
            .ok_or_else(|| BridgeError::state("terminal has no authoritative snapshot"))?;
        let start = self.resolve_anchor(terminal_id, start)?;
        let end = self.resolve_anchor(terminal_id, end)?;
        let start = start
            .snapshot(&replica.terminal)
            .map_err(BridgeError::ghostty)?
            .ok_or_else(|| BridgeError::state("selection start no longer resolves"))?;
        let end = end
            .snapshot(&replica.terminal)
            .map_err(BridgeError::ghostty)?
            .ok_or_else(|| BridgeError::state("selection end no longer resolves"))?;
        replica
            .terminal
            .set_selection(Some(&Selection::new(start, end, rectangular)))
            .map_err(BridgeError::ghostty)?;
        self.publish_local_damage(terminal_id)
    }

    pub(super) fn clear_selection(&mut self, terminal_id: &TerminalId) -> Result<(), BridgeError> {
        self.ensure_replica(terminal_id)?;
        self.replicas
            .get(terminal_id)
            .ok_or_else(|| BridgeError::state("terminal has no authoritative snapshot"))?
            .terminal
            .set_selection(None)
            .map_err(BridgeError::ghostty)?;
        self.publish_local_damage(terminal_id)
    }

    pub(super) fn selection_text(&mut self, terminal_id: &TerminalId) -> Result<(), BridgeError> {
        self.ensure_replica(terminal_id)?;
        let terminal = &self
            .replicas
            .get(terminal_id)
            .ok_or_else(|| BridgeError::state("terminal has no authoritative snapshot"))?
            .terminal;
        let text = terminal
            .format_selection_alloc(
                None,
                FormatOptions::new()
                    .with_emit_format(Format::Plain)
                    .with_unwrap(true)
                    .with_trim(true),
            )
            .map_err(BridgeError::ghostty)?
            .ok_or_else(|| BridgeError::state("terminal has no active selection"))?;
        self.selection_buf.extend_from_slice(text.as_ref());
        Ok(())
    }

    pub(super) fn search(
        &self,
        terminal_id: &TerminalId,
        query: &[u8],
        _case_sensitive: bool,
    ) -> Result<(), BridgeError> {
        self.ensure_replica(terminal_id)?;
        let query = std::str::from_utf8(query)
            .map_err(|_| BridgeError::invalid("search query is not UTF-8"))?;
        if query.is_empty() {
            return Err(BridgeError::invalid("search query is empty"));
        }
        Err(BridgeError::state(
            "search is unavailable in the protocol-0.6 projection bridge",
        ))
    }

    fn resolve_anchor(
        &self,
        terminal_id: &TerminalId,
        anchor: PhuxDocumentAnchor,
    ) -> Result<&TrackedGridRef, BridgeError> {
        let Some((owner, tracked)) = self.anchors.get(&anchor.opaque_id) else {
            return Err(BridgeError::state("document anchor is stale or unknown"));
        };
        if owner != terminal_id {
            return Err(BridgeError::state(
                "document anchor belongs to another terminal",
            ));
        }
        Ok(tracked)
    }

    fn invalidate_terminal_handles(&mut self, terminal_id: &TerminalId) {
        self.anchors.retain(|_, (owner, _)| owner != terminal_id);
    }
}

fn resolve_color(color: StyleColor, fallback: RgbColor, palette: &[RgbColor; 256]) -> RgbColor {
    match color {
        StyleColor::None => fallback,
        StyleColor::Palette(index) => palette[usize::from(index.0)],
        StyleColor::Rgb(rgb) => rgb,
    }
}

fn terminal_wants_mouse_tracking(terminal: &Terminal<'_, '_>) -> bool {
    [
        Mode::X10_MOUSE,
        Mode::NORMAL_MOUSE,
        Mode::BUTTON_MOUSE,
        Mode::ANY_MOUSE,
    ]
    .into_iter()
    .any(|mode| terminal.mode(mode).unwrap_or(false))
}

const fn cursor_style(style: CursorVisualStyle) -> u32 {
    match style {
        CursorVisualStyle::Block => 1,
        CursorVisualStyle::Underline => 2,
        CursorVisualStyle::BlockHollow => 3,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_protocol::caps::ServerCapabilities;
    use phux_protocol::ids::{ClientId, SessionId, WindowId};
    use phux_protocol::wire::frame::ErrorCode;
    use phux_protocol::wire::info::{SessionSnapshot, TerminalInfo, WindowInfo};

    fn attached_client() -> Client {
        let terminal_id = TerminalId::local(7);
        let session_id = SessionId::new(1);
        let window_id = WindowId::new(2);
        let snapshot = SessionSnapshot::new(session_id, window_id, terminal_id.clone())
            .with_windows(vec![WindowInfo::new(window_id, session_id, "main")])
            .with_panes(vec![TerminalInfo::new(terminal_id, window_id, 8, 3)]);
        let mut client = Client::new();
        client.hello_queued = true;
        client
            .feed(FrameKind::HelloOk {
                protocol_major: phux_protocol::PROTOCOL_VERSION.major,
                protocol_minor: phux_protocol::PROTOCOL_VERSION.minor,
                protocol_patch: phux_protocol::PROTOCOL_VERSION.patch,
                server_caps: ServerCapabilities::new(),
                server_id: Vec::new(),
            })
            .expect("HELLO_OK");
        client.attach_queued = true;
        client
            .feed(FrameKind::Attached {
                snapshot,
                initial_client_id: ClientId::new(3),
            })
            .expect("ATTACHED");
        client
    }

    #[test]
    fn mouse_tracking_follows_decset_1000() {
        let mut terminal = Terminal::new(TerminalOptions {
            cols: 80,
            rows: 24,
            max_scrollback: 100,
        })
        .expect("terminal");
        assert!(!terminal_wants_mouse_tracking(&terminal));
        terminal.vt_write(b"\x1b[?1000h");
        assert!(terminal_wants_mouse_tracking(&terminal));
        terminal.vt_write(b"\x1b[?1000l");
        assert!(!terminal_wants_mouse_tracking(&terminal));
    }

    #[test]
    fn snapshot_publishes_dense_grid_and_finishes_attach() {
        let mut client = attached_client();
        assert!(client.attached);
        assert!(!client.attached_notified);
        assert_eq!(client.state(), PhuxClientState::Negotiated);
        assert!(
            client
                .feed(FrameKind::TerminalSnapshot {
                    terminal_id: TerminalId::local(7),
                    cols: 8,
                    rows: 3,
                    vt_replay_bytes: b"hello".to_vec(),
                    scrollback_bytes: None,
                })
                .expect("snapshot")
        );

        let view = client
            .build_grid(&TerminalId::local(7))
            .expect("dense grid");
        let view = unsafe { &*view };
        assert_eq!((view.cols, view.rows), (8, 3));
        assert_eq!(view.cell_count, 24);
        let utf8 = unsafe { std::slice::from_raw_parts(view.utf8.data, view.utf8.len) };
        assert!(utf8.starts_with(b"hello"));
        assert_eq!(client.state(), PhuxClientState::Attached);
        assert_eq!(view.stream_id, 1);
        assert_eq!(view.bootstrap_id, 0);
        assert_eq!(client.effect_views[0].stream_id, 1);
    }

    #[test]
    fn local_scroll_publishes_damage_without_changing_replica_generation() {
        let mut client = attached_client();
        client
            .feed(FrameKind::TerminalSnapshot {
                terminal_id: TerminalId::local(7),
                cols: 8,
                rows: 3,
                vt_replay_bytes: b"one\r\ntwo\r\nthree\r\nfour".to_vec(),
                scrollback_bytes: None,
            })
            .expect("snapshot");
        client.owned_effects.clear();
        client.rebuild_effect_views();
        let revision = client.replicas[&TerminalId::local(7)].document_revision;

        client
            .scroll(&TerminalId::local(7), 0, 0)
            .expect("scroll to top");

        assert_eq!(client.owned_effects.len(), 1);
        assert_eq!(client.effect_views[0].stream_id, 1);
        assert!(client.replicas[&TerminalId::local(7)].document_revision > revision);
    }

    #[test]
    fn output_is_applied_once_and_acknowledged() {
        let mut client = attached_client();
        client
            .feed(FrameKind::TerminalSnapshot {
                terminal_id: TerminalId::local(7),
                cols: 8,
                rows: 3,
                vt_replay_bytes: Vec::new(),
                scrollback_bytes: None,
            })
            .expect("snapshot");
        client.owned_effects.clear();
        client.rebuild_effect_views();

        let output = FrameKind::TerminalOutput {
            terminal_id: TerminalId::local(7),
            seq: 9,
            bytes: bytes::Bytes::from_static(b"once"),
        };
        client.feed(output.clone()).expect("first output");
        assert_eq!(client.owned_effects.len(), 1);
        assert_eq!(client.outgoing.len(), 1);
        let (ack, remaining) = FrameKind::decode(&client.outgoing[0]).expect("ACK decodes");
        assert!(remaining.is_empty());
        assert!(matches!(
            ack,
            FrameKind::FrameAck { terminal_id, seq }
                if terminal_id == TerminalId::local(7) && seq == 9
        ));

        client.feed(output).expect("duplicate output is harmless");
        assert_eq!(client.owned_effects.len(), 1);
        assert_eq!(client.outgoing.len(), 2);
        let (duplicate_ack, remaining) =
            FrameKind::decode(&client.outgoing[1]).expect("duplicate ACK decodes");
        assert!(remaining.is_empty());
        assert!(matches!(
            duplicate_ack,
            FrameKind::FrameAck { terminal_id, seq }
                if terminal_id == TerminalId::local(7) && seq == 9
        ));
        let view = client
            .build_grid(&TerminalId::local(7))
            .expect("dense grid");
        assert_eq!(unsafe { (*view).last_seq }, 9);
    }

    #[test]
    fn replacement_snapshot_preserves_history_and_advances_generation() {
        let mut client = attached_client();
        let terminal_id = TerminalId::local(7);
        client
            .feed(FrameKind::TerminalSnapshot {
                terminal_id: terminal_id.clone(),
                cols: 8,
                rows: 3,
                vt_replay_bytes: b"old".to_vec(),
                scrollback_bytes: None,
            })
            .expect("initial snapshot");
        let anchor = client
            .track_anchor(
                &terminal_id,
                PhuxDocumentPoint {
                    space: 1,
                    row: 0,
                    column: 0,
                    reserved: 0,
                },
            )
            .expect("anchor");

        client
            .feed(FrameKind::TerminalSnapshot {
                terminal_id: terminal_id.clone(),
                cols: 10,
                rows: 4,
                vt_replay_bytes: b"new".to_vec(),
                scrollback_bytes: Some(b"one\r\ntwo\r\nthree\r\nfour\r\n".to_vec()),
            })
            .expect("replacement snapshot");

        let view = unsafe { &*client.build_grid(&terminal_id).expect("replacement grid") };
        assert_eq!(view.stream_id, 2);
        assert!(view.history_total_rows > view.history_visible_rows);
        assert!(client.release_anchor(&terminal_id, anchor).is_err());
    }

    #[test]
    fn output_before_snapshot_is_rejected() {
        let mut client = attached_client();
        let error = client
            .feed(FrameKind::TerminalOutput {
                terminal_id: TerminalId::local(7),
                seq: 1,
                bytes: bytes::Bytes::from_static(b"early"),
            })
            .expect_err("output must wait for authoritative snapshot");
        assert_eq!(error.result, crate::types::PhuxClientResult::ProtocolError);
        assert!(client.outgoing.is_empty());
    }

    #[test]
    fn synchronized_output_defers_damage_until_the_transaction_closes() {
        let mut client = attached_client();
        client
            .feed(FrameKind::TerminalSnapshot {
                terminal_id: TerminalId::local(7),
                cols: 8,
                rows: 3,
                vt_replay_bytes: Vec::new(),
                scrollback_bytes: None,
            })
            .expect("snapshot");
        client.owned_effects.clear();
        client.rebuild_effect_views();

        for (seq, bytes) in [
            (1, bytes::Bytes::from_static(b"\x1b[?2026hA")),
            (2, bytes::Bytes::from_static(b"B")),
        ] {
            client
                .feed(FrameKind::TerminalOutput {
                    terminal_id: TerminalId::local(7),
                    seq,
                    bytes,
                })
                .expect("synchronized output");
        }
        assert!(client.owned_effects.is_empty());
        assert_eq!(client.outgoing.len(), 2);

        client
            .feed(FrameKind::TerminalOutput {
                terminal_id: TerminalId::local(7),
                seq: 3,
                bytes: bytes::Bytes::from_static(b"\x1b[?2026l"),
            })
            .expect("synchronized output close");
        assert_eq!(client.owned_effects.len(), 1);
        assert_eq!(client.owned_effects[0].kind, 1);
        assert_eq!(client.owned_effects[0].seq, 3);
        assert_eq!(client.outgoing.len(), 3);
    }

    #[test]
    fn synchronized_output_watchdog_publishes_stalled_damage() {
        let mut client = attached_client();
        client
            .feed(FrameKind::TerminalSnapshot {
                terminal_id: TerminalId::local(7),
                cols: 8,
                rows: 3,
                vt_replay_bytes: Vec::new(),
                scrollback_bytes: None,
            })
            .expect("snapshot");
        client.owned_effects.clear();
        client.rebuild_effect_views();
        client
            .feed(FrameKind::TerminalOutput {
                terminal_id: TerminalId::local(7),
                seq: 1,
                bytes: bytes::Bytes::from_static(b"\x1b[?2026hstalled"),
            })
            .expect("synchronized output");
        assert!(client.owned_effects.is_empty());
        assert!(client.maintenance_pending());

        let started = client.replicas[&TerminalId::local(7)]
            .sync_output_since
            .expect("watchdog start");
        client.maintenance_at(started + SYNC_OUTPUT_WATCHDOG);

        assert_eq!(client.owned_effects.len(), 1);
        assert_eq!(client.owned_effects[0].kind, 1);
        assert_eq!(client.owned_effects[0].seq, 1);
        assert!(!client.maintenance_pending());
    }

    #[test]
    fn handshake_error_enters_failed_state() {
        let mut client = Client::new();
        client.hello_queued = true;
        client
            .feed(FrameKind::Error {
                request_id: None,
                code: ErrorCode::VersionIncompatible,
                message: "upgrade required".to_owned(),
            })
            .expect("handshake refusal is a legal server frame");
        assert_eq!(client.state(), PhuxClientState::Failed);
        assert_eq!(client.owned_effects.len(), 1);
    }

    #[test]
    fn attach_error_allows_another_target_to_be_queued() {
        let mut client = Client::new();
        client.hello_queued = true;
        client
            .feed(FrameKind::HelloOk {
                protocol_major: phux_protocol::PROTOCOL_VERSION.major,
                protocol_minor: phux_protocol::PROTOCOL_VERSION.minor,
                protocol_patch: phux_protocol::PROTOCOL_VERSION.patch,
                server_caps: ServerCapabilities::new(),
                server_id: Vec::new(),
            })
            .expect("HELLO_OK");
        client.attach_queued = true;
        client
            .feed(FrameKind::Error {
                request_id: None,
                code: ErrorCode::SessionNotFound,
                message: "missing".to_owned(),
            })
            .expect("attach refusal");
        assert!(!client.attach_queued);
        assert_eq!(client.state(), PhuxClientState::Negotiated);
    }

    #[test]
    fn detached_after_snapshot_barrier_error_aborts_partial_attachment() {
        let mut client = Client::new();
        client.protocol_ready = true;
        client.attached = true;
        client.participants.insert(TerminalId::local(7));
        client.pending_snapshots.insert(TerminalId::local(7));

        client
            .feed(FrameKind::Error {
                request_id: None,
                code: ErrorCode::InternalError,
                message: "snapshot failed".to_owned(),
            })
            .expect("partial attachment failure is a legal server frame");
        assert!(
            client.attached,
            "an unrelated error cannot abort attachment"
        );
        client
            .feed(FrameKind::Detached)
            .expect("server explicitly rolls back the attachment");

        assert!(client.participants.is_empty());
        assert!(client.pending_snapshots.is_empty());
        assert_eq!(client.state(), PhuxClientState::Detached);
    }
}
