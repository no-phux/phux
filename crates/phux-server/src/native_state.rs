//! Native checkpoint hosts over libghostty's official GHOSTSNP snapshot codec.
//!
//! A capture encodes one point-in-time snapshot, splits it at the engine's
//! READY offset, and treats the suffix as pullable history. The live terminal
//! is free to keep taking PTY bytes; clients share an immutable byte copy
//! keyed by a content hash. Phux never parses snapshot records itself.

use bytes::Bytes;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::Cursor,
    marker::PhantomData,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use libghostty_vt::{Error as GhosttyError, Terminal as GhosttyTerminal, snapshot::Decoder};
use phux_protocol::caps::{BootstrapCapabilities, BootstrapLimits, EngineCodec, EngineFeatureSet};
use thiserror::Error;

/// Opaque generation identity: SHA-256 of the encoded snapshot.
pub const TOKEN_LEN: usize = 32;
const CHECKPOINT_VERSION: u16 = EngineCodec::LibghosttyCheckpointV2 as u16;
/// Continuation tracking limit so `encode_snapshot` can capture mid-sequence VT.
const CONTINUATION_LIMIT: usize = 64 * 1024 * 1024;

/// Greatest number of opaque codec records retained before READY publication.
pub(crate) const MAX_NATIVE_PREFIX_CHUNKS: usize = 4_096;
/// Greatest aggregate opaque codec payload retained before READY publication.
pub(crate) const MAX_NATIVE_PREFIX_BYTES: usize = 64 * 1024 * 1024;

/// Typed failures from native capture, history, and generation management.
///
/// Variant names match the previous incremental-wrapper surface so actor and
/// runtime matches stay stable.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum NativeStateError {
    /// The linked engine cannot encode an official snapshot.
    #[error("native checkpoint feature unsupported")]
    UnsupportedFeature,
    /// Snapshot envelope version is not supported.
    #[error("unknown checkpoint version")]
    UnknownVersion,
    /// Authentication or structural validation failed.
    #[error("checkpoint corrupted")]
    Corruption,
    /// End of input arrived before a required checkpoint.
    #[error("checkpoint truncated")]
    Truncated,
    /// A caller-supplied codec or history limit was exceeded.
    #[error("native limit exceeded")]
    LimitExceeded,
    /// The history cursor's snapshot cut is no longer live.
    #[error("history cursor is stale")]
    Stale,
    /// Requested history was pruned after the cut was acquired.
    #[error("requested history was pruned")]
    Pruned,
    /// The requested screen or history generation does not match.
    #[error("history generation mismatch")]
    WrongGeneration,
    /// An operation was attempted with a different terminal.
    #[error("operation used the wrong terminal")]
    WrongTerminal,
    /// A checkpoint, capability, or native handle is invalid.
    #[error("invalid native handle")]
    InvalidHandle,
    /// The destination already has an active history import.
    #[error("history import already active")]
    ImportBusy,
    /// Allocation failed.
    #[error("out of memory")]
    OutOfMemory,
    /// The caller-owned buffer or import budget is too small.
    #[error("out of space: {required_bytes} bytes, {required_rows} rows")]
    OutOfSpace {
        /// Exact required byte count, when the operation can report one.
        required_bytes: usize,
        /// Exact required row count, when the operation can report one.
        required_rows: usize,
    },
    /// The operation is not valid in the current state.
    #[error("invalid native state")]
    InvalidState,
    /// A canonical parser continuation could not be captured or replayed.
    #[error("continuation unavailable")]
    ContinuationUnavailable,
    /// A terminal reset invalidated the history generation.
    #[error("terminal reset invalidated the history generation")]
    Reset,
    /// A terminal resize invalidated the history generation.
    #[error("terminal resize invalidated the history generation")]
    Resize,
}

impl From<GhosttyError> for NativeStateError {
    fn from(error: GhosttyError) -> Self {
        match error {
            GhosttyError::OutOfMemory => Self::OutOfMemory,
            GhosttyError::OutOfSpace { required } => Self::OutOfSpace {
                required_bytes: required,
                required_rows: 0,
            },
            GhosttyError::LimitExceeded => Self::LimitExceeded,
            GhosttyError::IoError => Self::Corruption,
            GhosttyError::InvalidValue => Self::UnsupportedFeature,
        }
    }
}

/// One authenticated engine token carried opaquely by protocol 0.7.
pub type OpaqueHistoryCursor = [u8; TOKEN_LEN];

/// Probe the linked engine and advertise native checkpoint v2 when snapshots encode.
#[must_use]
pub fn native_bootstrap_capabilities() -> BootstrapCapabilities {
    let requested = BootstrapLimits::default();
    if !official_snapshot_available() {
        return BootstrapCapabilities::new().with_limits(requested);
    }
    BootstrapCapabilities::new()
        .with_limits(requested)
        .with_native(
            EngineCodec::LibghosttyCheckpointV2,
            EngineFeatureSet::required_native(),
        )
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

/// Typed metadata for one complete bootstrap-prefix record.
#[derive(Debug)]
pub enum NativeCheckpointChunkKind {
    /// Envelope or non-boundary active-state record.
    Record,
    /// Authenticated renderable boundary and final bootstrap record.
    Ready,
}

/// One complete native checkpoint record written into a caller-owned buffer.
#[derive(Debug)]
pub struct NativeCheckpointChunk<'buffer> {
    /// Typed publication metadata. The record bytes remain opaque.
    pub kind: NativeCheckpointChunkKind,
    /// Protocol codec version carried on the wire, not the GHOSTSNP envelope.
    pub codec_version: u16,
    /// Complete opaque envelope or record bytes.
    pub bytes: &'buffer [u8],
}

#[derive(Clone, Debug)]
struct SnapshotCut {
    prefix: Bytes,
    suffix: Bytes,
    cursor: OpaqueHistoryCursor,
}

fn cursor_for(bytes: &[u8]) -> OpaqueHistoryCursor {
    Sha256::digest(bytes).into()
}

fn encode_cut(terminal: &mut GhosttyTerminal<'_, '_>) -> Result<SnapshotCut, NativeStateError> {
    let _ = terminal.set_continuation_max_bytes(CONTINUATION_LIMIT);
    let mut encoded = Vec::new();
    terminal.encode_snapshot(&mut encoded)?;
    if encoded.is_empty() {
        return Err(NativeStateError::InvalidState);
    }
    let ready_at = snapshot_ready_offset(&encoded)?;
    if ready_at == 0 || ready_at > encoded.len() {
        return Err(NativeStateError::InvalidState);
    }
    let cursor = cursor_for(&encoded);
    // Keep the full GHOSTSNP blob in the bootstrap prefix so the client can
    // `Decoder::new_buf` locally at READY without retaining a borrowed decoder.
    // History pages stay available as an empty suffix finish record.
    let _ = ready_at;
    Ok(SnapshotCut {
        prefix: Bytes::from(encoded),
        suffix: Bytes::new(),
        cursor,
    })
}

fn snapshot_ready_offset(bytes: &[u8]) -> Result<usize, NativeStateError> {
    let mut reader = Cursor::new(bytes);
    let decoder = Decoder::new(&mut reader)?;
    drop(decoder.ready()?);
    let offset = usize::try_from(reader.position()).unwrap_or(bytes.len());
    Ok(offset.min(bytes.len()))
}

fn chunk_plan(remaining: usize, max_record_bytes: usize) -> Result<usize, NativeStateError> {
    if remaining == 0 || max_record_bytes == 0 {
        return Err(NativeStateError::InvalidState);
    }
    Ok(remaining.min(max_record_bytes))
}

fn prefix_record_bound(
    limits: BootstrapLimits,
    prefix_len: usize,
) -> Result<usize, NativeStateError> {
    let chunk =
        usize::try_from(limits.max_chunk_bytes()).map_err(|_| NativeStateError::LimitExceeded)?;
    if prefix_len > MAX_NATIVE_PREFIX_BYTES {
        return Err(NativeStateError::LimitExceeded);
    }
    let bound = chunk.clamp(1, MAX_NATIVE_PREFIX_BYTES);
    Ok(bound.min(prefix_len.max(1)))
}

/// RAII host for the bounded checkpoint prefix ending at READY.
#[derive(Debug)]
pub struct NativeCheckpointCapture<'terminal> {
    prefix: Bytes,
    suffix: Bytes,
    cursor: OpaqueHistoryCursor,
    pos: usize,
    max_record_bytes: usize,
    ready: bool,
    _terminal: PhantomData<&'terminal mut GhosttyTerminal<'static, 'static>>,
}

impl NativeCheckpointCapture<'_> {
    /// Encode one snapshot and prepare prefix streaming.
    pub fn new(
        terminal: &mut GhosttyTerminal<'_, '_>,
        limits: BootstrapLimits,
    ) -> Result<Self, NativeStateError> {
        let cut = encode_cut(terminal)?;
        let max_record_bytes = prefix_record_bound(limits, cut.prefix.len())?;
        Ok(Self {
            prefix: cut.prefix,
            suffix: cut.suffix,
            cursor: cut.cursor,
            pos: 0,
            max_record_bytes,
            ready: false,
            _terminal: PhantomData,
        })
    }

    /// Emit one prefix slice into `buffer`. The last slice is READY.
    pub fn step<'buffer>(
        &mut self,
        buffer: &'buffer mut [u8],
    ) -> Result<NativeCheckpointChunk<'buffer>, NativeStateError> {
        step_prefix(
            &self.prefix,
            &mut self.pos,
            &mut self.ready,
            self.max_record_bytes,
            buffer,
        )
    }

    /// Maximum bytes required for one complete opaque native record.
    #[must_use]
    pub const fn max_record_bytes(&self) -> usize {
        self.max_record_bytes
    }

    /// Whether this host has emitted its final READY record.
    #[must_use]
    pub const fn is_ready(&self) -> bool {
        self.ready
    }

    /// Release without installing history. The snapshot bytes are dropped.
    #[allow(
        clippy::unnecessary_wraps,
        reason = "callers match Result with other capture APIs"
    )]
    pub fn abort(self) -> Result<(), NativeStateError> {
        let _ = (self.suffix, self.cursor);
        Ok(())
    }
}

fn step_prefix<'buffer>(
    prefix: &Bytes,
    pos: &mut usize,
    ready: &mut bool,
    max_record_bytes: usize,
    buffer: &'buffer mut [u8],
) -> Result<NativeCheckpointChunk<'buffer>, NativeStateError> {
    if *ready {
        return Err(NativeStateError::InvalidState);
    }
    let remaining = prefix.len().saturating_sub(*pos);
    let want = chunk_plan(remaining, max_record_bytes)?;
    if buffer.len() < want {
        return Err(NativeStateError::OutOfSpace {
            required_bytes: want,
            required_rows: 0,
        });
    }
    let end = *pos + want;
    buffer[..want].copy_from_slice(&prefix[*pos..end]);
    *pos = end;
    let last = *pos >= prefix.len();
    if last {
        *ready = true;
    }
    Ok(NativeCheckpointChunk {
        kind: if last {
            NativeCheckpointChunkKind::Ready
        } else {
            NativeCheckpointChunkKind::Record
        },
        codec_version: CHECKPOINT_VERSION,
        bytes: &buffer[..want],
    })
}

/// One bounded result from a retained-history cursor.
#[derive(Debug)]
pub enum NativeHistoryEvent<'buffer> {
    /// One complete authenticated engine history unit.
    Page {
        /// Opaque unit bytes written directly into the caller's buffer.
        bytes: &'buffer [u8],
        /// Number of terminal rows represented by this unit.
        rows: usize,
        /// Whether this unit completes its native source page.
        page_complete: bool,
        /// Same-generation opaque capability for the next request.
        next_cursor: OpaqueHistoryCursor,
    },
    /// No older units remain; the protocol history stream is finished.
    End,
}

/// Owned canonical terminal plus a frozen history suffix from its READY cut.
#[derive(Debug)]
pub struct NativeHistoryCursor<'terminal_alloc, 'cb> {
    terminal: GhosttyTerminal<'terminal_alloc, 'cb>,
    suffix: Bytes,
    cursor: OpaqueHistoryCursor,
    pos: usize,
    max_unit_bytes: usize,
    invalidated: Option<NativeStateError>,
}

impl<'terminal_alloc, 'cb> NativeHistoryCursor<'terminal_alloc, 'cb> {
    /// Consume the canonical terminal and freeze its current snapshot suffix.
    pub fn new(
        mut terminal: GhosttyTerminal<'terminal_alloc, 'cb>,
        limits: BootstrapLimits,
    ) -> Result<Self, NativeStateError> {
        let cut = encode_cut(&mut terminal)?;
        let max_unit_bytes = usize::try_from(limits.max_history_page_bytes())
            .map_err(|_| NativeStateError::LimitExceeded)?
            .max(1);
        Ok(Self {
            terminal,
            suffix: cut.suffix,
            cursor: cut.cursor,
            pos: 0,
            max_unit_bytes,
            invalidated: None,
        })
    }

    /// Opaque checkpoint authenticating this cursor's exact terminal cut.
    #[must_use]
    pub const fn checkpoint(&self) -> &OpaqueHistoryCursor {
        &self.cursor
    }

    /// Opaque capability advertised as `BOOTSTRAP_READY.history_cursor`.
    #[must_use]
    pub const fn cursor(&self) -> &OpaqueHistoryCursor {
        &self.cursor
    }

    /// Borrow the live canonical terminal for read-only engine queries.
    #[must_use]
    pub const fn terminal(&self) -> &GhosttyTerminal<'terminal_alloc, 'cb> {
        &self.terminal
    }

    /// Feed serialized raw PTY bytes to the live canonical terminal.
    pub fn vt_write(&mut self, data: &[u8]) {
        self.terminal.vt_write(data);
    }

    /// Reset the live terminal and invalidate this retained-history generation.
    pub fn reset(&mut self) {
        self.terminal.reset();
        self.invalidated = Some(NativeStateError::Reset);
    }

    /// Resize the live terminal and invalidate this retained-history generation.
    pub fn resize(
        &mut self,
        cols: u16,
        rows: u16,
        cell_width_px: u32,
        cell_height_px: u32,
    ) -> libghostty_vt::error::Result<()> {
        let result = self
            .terminal
            .resize(cols, rows, cell_width_px, cell_height_px);
        if result.is_ok() {
            self.invalidated = Some(NativeStateError::Resize);
        }
        result
    }

    /// Emit one authenticated history unit within both negotiated bounds.
    pub fn next<'buffer>(
        &mut self,
        max_bytes: u32,
        buffer: &'buffer mut [u8],
    ) -> Result<NativeHistoryEvent<'buffer>, NativeStateError> {
        if let Some(error) = self.invalidated {
            return Err(error);
        }
        let requested = usize::try_from(max_bytes).map_err(|_| NativeStateError::LimitExceeded)?;
        if requested == 0 {
            return Err(NativeStateError::LimitExceeded);
        }
        let remaining = self.suffix.len().saturating_sub(self.pos);
        if remaining == 0 {
            return Ok(NativeHistoryEvent::End);
        }
        let want = remaining.min(self.max_unit_bytes).min(requested);
        if buffer.len() < want {
            return Err(NativeStateError::OutOfSpace {
                required_bytes: want,
                required_rows: 0,
            });
        }
        let end = self.pos + want;
        buffer[..want].copy_from_slice(&self.suffix[self.pos..end]);
        self.pos = end;
        Ok(NativeHistoryEvent::Page {
            bytes: &buffer[..want],
            rows: 0,
            page_complete: self.pos >= self.suffix.len(),
            next_cursor: self.cursor,
        })
    }

    /// Release cursor state before returning the live terminal.
    #[must_use]
    pub fn into_terminal(self) -> GhosttyTerminal<'terminal_alloc, 'cb> {
        self.terminal
    }
}

/// Failure from [`NativeTerminalManager::new`] that returns the terminal.
#[derive(Debug)]
pub(crate) struct NativeManagerInitFailure {
    pub(crate) error: NativeStateError,
    pub(crate) terminal: GhosttyTerminal<'static, 'static>,
}

/// Fixed bounds for one shared retained-history generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    clippy::struct_field_names,
    reason = "every field IS a maximum; dropping the prefix would leave `rows`/`records` reading as counts rather than caps"
)]
pub(crate) struct NativeGenerationBounds {
    pub(crate) max_record_bytes: usize,
    pub(crate) max_rows: usize,
    pub(crate) max_records: usize,
    pub(crate) max_total_bytes: usize,
}

impl NativeGenerationBounds {
    /// Retained bytes needed for the exact fixed record table and payload budget.
    pub(crate) fn required_reserved_bytes(self) -> Result<usize, NativeStateError> {
        let record_table_bytes = self
            .max_records
            .checked_mul(std::mem::size_of::<Option<CachedNativeHistoryRecord>>())
            .ok_or(NativeStateError::LimitExceeded)?;
        record_table_bytes
            .checked_add(self.max_total_bytes)
            .ok_or(NativeStateError::LimitExceeded)
    }
}

/// The one native continuation produced by a capture that reached READY.
#[derive(Debug)]
pub(crate) struct NativeGenerationSeed {
    suffix: Bytes,
    bounds: NativeGenerationBounds,
}

impl NativeGenerationSeed {
    pub(crate) const fn bounds(&self) -> NativeGenerationBounds {
        self.bounds
    }
}

/// One immutable record in a shared generation's append-only history.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CachedNativeHistoryRecord {
    pub(crate) bytes: Bytes,
    pub(crate) rows: usize,
    pub(crate) finish: bool,
}

impl CachedNativeHistoryRecord {
    fn for_request(&self, max_bytes: usize, max_rows: usize) -> Result<Self, NativeStateError> {
        if self.bytes.len() > max_bytes || self.rows > max_rows {
            return Err(NativeStateError::OutOfSpace {
                required_bytes: self.bytes.len(),
                required_rows: self.rows,
            });
        }
        Ok(self.clone())
    }
}

#[derive(Debug, Default)]
struct NativeGenerationCharge {
    live_payloads: AtomicUsize,
    live_payload_bytes: AtomicUsize,
}

#[derive(Debug)]
struct ChargedNativePayload {
    bytes: Box<[u8]>,
    charge: Arc<NativeGenerationCharge>,
}

impl ChargedNativePayload {
    fn new(bytes: Box<[u8]>, charge: Arc<NativeGenerationCharge>) -> Self {
        charge.live_payloads.fetch_add(1, Ordering::Relaxed);
        charge
            .live_payload_bytes
            .fetch_add(bytes.len(), Ordering::Relaxed);
        Self { bytes, charge }
    }
}

impl AsRef<[u8]> for ChargedNativePayload {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

impl Drop for ChargedNativePayload {
    fn drop(&mut self) {
        self.charge
            .live_payload_bytes
            .fetch_sub(self.bytes.len(), Ordering::Release);
        self.charge.live_payloads.fetch_sub(1, Ordering::Release);
    }
}

#[derive(Debug)]
struct NativeRecordTable {
    slots: Box<[Option<CachedNativeHistoryRecord>]>,
    len: usize,
}

impl NativeRecordTable {
    fn new(max_records: usize) -> Result<Self, NativeStateError> {
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(max_records)
            .map_err(|_| NativeStateError::OutOfMemory)?;
        slots.resize_with(max_records, || None);
        Ok(Self {
            slots: slots.into_boxed_slice(),
            len: 0,
        })
    }

    fn get(&self, index: usize) -> Option<&CachedNativeHistoryRecord> {
        self.slots.get(index).and_then(Option::as_ref)
    }
}

#[derive(Debug)]
struct NativeCheckpointGeneration {
    records: NativeRecordTable,
    #[allow(
        dead_code,
        reason = "retained for install-time equality checks and debug"
    )]
    bounds: NativeGenerationBounds,
    owners: usize,
    charge: Arc<NativeGenerationCharge>,
}

fn generation_bounds_for(suffix_len: usize) -> Result<NativeGenerationBounds, NativeStateError> {
    let max_record_bytes = usize::try_from(phux_protocol::MAX_HISTORY_PAGE_BYTES)
        .map_err(|_| NativeStateError::LimitExceeded)?
        .max(1);
    let max_rows = usize::try_from(phux_protocol::MAX_HISTORY_PAGE_ROWS)
        .map_err(|_| NativeStateError::LimitExceeded)?
        .max(1);
    let max_records = suffix_len.div_ceil(max_record_bytes).max(1);
    Ok(NativeGenerationBounds {
        max_record_bytes,
        max_rows,
        max_records,
        max_total_bytes: suffix_len.max(1),
    })
}

fn chunk_suffix(
    suffix: &Bytes,
    bounds: NativeGenerationBounds,
    charge: &Arc<NativeGenerationCharge>,
) -> Result<NativeRecordTable, NativeStateError> {
    let mut table = NativeRecordTable::new(bounds.max_records)?;
    if suffix.is_empty() {
        let payload = ChargedNativePayload::new(Box::from([]), Arc::clone(charge));
        table.slots[0] = Some(CachedNativeHistoryRecord {
            bytes: Bytes::from_owner(payload),
            rows: 0,
            finish: true,
        });
        table.len = 1;
        return Ok(table);
    }
    let mut offset = 0;
    let mut index = 0;
    while offset < suffix.len() {
        let end = (offset + bounds.max_record_bytes).min(suffix.len());
        let finish = end >= suffix.len();
        let payload = ChargedNativePayload::new(suffix[offset..end].into(), Arc::clone(charge));
        table.slots[index] = Some(CachedNativeHistoryRecord {
            bytes: Bytes::from_owner(payload),
            rows: 0,
            finish,
        });
        table.len = index + 1;
        offset = end;
        index += 1;
    }
    Ok(table)
}

/// Actor-owned terminal and bounded concurrent native history cuts.
#[derive(Debug)]
pub(crate) struct NativeTerminalManager {
    terminal: GhosttyTerminal<'static, 'static>,
    generations: HashMap<OpaqueHistoryCursor, NativeCheckpointGeneration>,
    retired_generation_charges: Vec<Arc<NativeGenerationCharge>>,
    capacity: usize,
    capture_active: bool,
}

impl NativeTerminalManager {
    pub(crate) fn new(
        mut terminal: GhosttyTerminal<'static, 'static>,
        capacity: usize,
    ) -> Result<Self, NativeManagerInitFailure> {
        if !official_snapshot_available() {
            return Err(NativeManagerInitFailure {
                error: NativeStateError::UnsupportedFeature,
                terminal,
            });
        }
        if capacity == 0 {
            return Err(NativeManagerInitFailure {
                error: NativeStateError::LimitExceeded,
                terminal,
            });
        }
        if terminal
            .set_continuation_max_bytes(CONTINUATION_LIMIT)
            .is_err()
        {
            return Err(NativeManagerInitFailure {
                error: NativeStateError::ContinuationUnavailable,
                terminal,
            });
        }
        let mut generations = HashMap::new();
        let mut retired_generation_charges = Vec::new();
        if generations.try_reserve(capacity).is_err()
            || retired_generation_charges
                .try_reserve_exact(capacity)
                .is_err()
        {
            return Err(NativeManagerInitFailure {
                error: NativeStateError::OutOfMemory,
                terminal,
            });
        }
        Ok(Self {
            terminal,
            generations,
            retired_generation_charges,
            capacity,
            capture_active: false,
        })
    }

    pub(crate) const fn terminal(&self) -> &GhosttyTerminal<'static, 'static> {
        &self.terminal
    }

    pub(crate) fn vt_write(&mut self, bytes: &[u8]) {
        debug_assert!(!self.capture_active);
        self.terminal.vt_write(bytes);
    }

    pub(crate) fn resize(
        &mut self,
        cols: u16,
        rows: u16,
        cell_width_px: u32,
        cell_height_px: u32,
    ) -> libghostty_vt::error::Result<()> {
        debug_assert!(!self.capture_active);
        self.retire_all_generations();
        self.terminal
            .resize(cols, rows, cell_width_px, cell_height_px)
    }

    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "kept for capture-budget tests that are not yet ported"
    )]
    pub(crate) fn capture(
        &mut self,
        limits: BootstrapLimits,
    ) -> Result<NativeManagedCapture<'static>, NativeStateError> {
        self.begin_generation_capture(limits, MAX_NATIVE_PREFIX_BYTES, MAX_NATIVE_PREFIX_CHUNKS)
    }

    #[cfg(test)]
    pub(crate) fn capture_generation_bounded(
        &mut self,
        limits: BootstrapLimits,
        max_prefix_bytes: usize,
        max_prefix_chunks: usize,
    ) -> Result<NativeManagedCapture<'static>, NativeStateError> {
        self.begin_generation_capture(limits, max_prefix_bytes, max_prefix_chunks)
    }

    pub(crate) fn begin_generation_capture(
        &mut self,
        limits: BootstrapLimits,
        max_prefix_bytes: usize,
        max_prefix_chunks: usize,
    ) -> Result<NativeManagedCapture<'static>, NativeStateError> {
        if self.capture_active || max_prefix_chunks == 0 || max_prefix_bytes == 0 {
            return Err(NativeStateError::InvalidState);
        }
        let cut = encode_cut(&mut self.terminal)?;
        if cut.prefix.len() > max_prefix_bytes {
            return Err(NativeStateError::LimitExceeded);
        }
        let max_record_bytes = prefix_record_bound(limits, cut.prefix.len())?;
        let bounds = generation_bounds_for(cut.suffix.len())?;
        self.capture_active = true;
        Ok(NativeManagedCapture {
            prefix: cut.prefix,
            suffix: cut.suffix,
            cursor: cut.cursor,
            pos: 0,
            max_record_bytes,
            bounds,
            ready: false,
            _marker: PhantomData,
        })
    }

    pub(crate) fn finish_generation_capture(
        &mut self,
        capture: NativeManagedCapture<'static>,
    ) -> Result<(OpaqueHistoryCursor, NativeGenerationSeed), NativeStateError> {
        if !self.capture_active {
            return Err(NativeStateError::InvalidState);
        }
        let result = capture.detach_generation_ready();
        self.capture_active = false;
        result
    }

    pub(crate) fn abort_generation_capture(&mut self, capture: NativeManagedCapture<'static>) {
        drop(capture);
        self.capture_active = false;
    }

    pub(crate) fn has_generation(&self, cursor: &OpaqueHistoryCursor) -> bool {
        self.generations.contains_key(cursor)
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "seed is the owned generation payload"
    )]
    pub(crate) fn install_generation(
        &mut self,
        cursor: OpaqueHistoryCursor,
        seed: NativeGenerationSeed,
        bounds: NativeGenerationBounds,
        reserved_bytes: usize,
    ) -> Result<(), NativeStateError> {
        let required_reserved_bytes = bounds.required_reserved_bytes()?;
        if self.retained_generation_count()? >= self.capacity
            || self.generations.contains_key(&cursor)
            || bounds != seed.bounds
            || reserved_bytes < required_reserved_bytes
        {
            return Err(NativeStateError::LimitExceeded);
        }
        let charge = Arc::new(NativeGenerationCharge::default());
        let records = chunk_suffix(&seed.suffix, bounds, &charge)?;
        self.generations
            .try_reserve(1)
            .map_err(|_| NativeStateError::OutOfMemory)?;
        self.generations.insert(
            cursor,
            NativeCheckpointGeneration {
                records,
                bounds,
                owners: 1,
                charge,
            },
        );
        Ok(())
    }

    pub(crate) fn retain_generation(
        &mut self,
        cursor: &OpaqueHistoryCursor,
    ) -> Result<(), NativeStateError> {
        let generation = self
            .generations
            .get_mut(cursor)
            .ok_or(NativeStateError::InvalidHandle)?;
        generation.owners = generation
            .owners
            .checked_add(1)
            .ok_or(NativeStateError::LimitExceeded)?;
        Ok(())
    }

    pub(crate) fn history_record_at(
        &mut self,
        cursor: &OpaqueHistoryCursor,
        index: usize,
        requested_max_bytes: u32,
        requested_max_rows: u32,
    ) -> Result<CachedNativeHistoryRecord, NativeStateError> {
        let (requested_bytes, requested_rows) =
            requested_record_window(requested_max_bytes, requested_max_rows)?;
        let generation = self
            .generations
            .get_mut(cursor)
            .ok_or(NativeStateError::InvalidHandle)?;
        let record = generation
            .records
            .get(index)
            .ok_or(NativeStateError::InvalidHandle)?;
        record.for_request(requested_bytes, requested_rows)
    }

    pub(crate) fn release_generation(
        &mut self,
        cursor: &OpaqueHistoryCursor,
    ) -> Result<(), NativeStateError> {
        let remove = {
            let generation = self
                .generations
                .get_mut(cursor)
                .ok_or(NativeStateError::InvalidHandle)?;
            generation.owners = generation
                .owners
                .checked_sub(1)
                .ok_or(NativeStateError::InvalidHandle)?;
            generation.owners == 0
        };
        if remove {
            let generation = self
                .generations
                .remove(cursor)
                .ok_or(NativeStateError::InvalidHandle)?;
            if let Some(charge) = Self::released_generation_charge(generation) {
                self.retired_generation_charges.push(charge);
            }
        }
        Ok(())
    }

    fn retire_all_generations(&mut self) {
        let generations = std::mem::take(&mut self.generations);
        for (_, generation) in generations {
            if let Some(charge) = Self::released_generation_charge(generation) {
                self.retired_generation_charges.push(charge);
            }
        }
        self.reap_retired_generation_charges();
    }

    fn released_generation_charge(
        generation: NativeCheckpointGeneration,
    ) -> Option<Arc<NativeGenerationCharge>> {
        let charge = Arc::clone(&generation.charge);
        drop(generation);
        (charge.live_payloads.load(Ordering::Acquire) != 0).then_some(charge)
    }

    fn reap_retired_generation_charges(&mut self) {
        self.retired_generation_charges
            .retain(|charge| charge.live_payloads.load(Ordering::Acquire) != 0);
    }

    fn retained_generation_count(&mut self) -> Result<usize, NativeStateError> {
        self.reap_retired_generation_charges();
        self.generations
            .len()
            .checked_add(self.retired_generation_charges.len())
            .ok_or(NativeStateError::LimitExceeded)
    }
}

/// Actor-owned prefix capture that no longer borrows the live terminal.
#[derive(Debug)]
pub(crate) struct NativeManagedCapture<'manager> {
    prefix: Bytes,
    suffix: Bytes,
    cursor: OpaqueHistoryCursor,
    pos: usize,
    max_record_bytes: usize,
    bounds: NativeGenerationBounds,
    ready: bool,
    _marker: PhantomData<&'manager ()>,
}

impl NativeManagedCapture<'_> {
    pub(crate) const fn max_record_bytes(&self) -> usize {
        self.max_record_bytes
    }

    pub(crate) fn step<'buffer>(
        &mut self,
        buffer: &'buffer mut [u8],
    ) -> Result<NativeCheckpointChunk<'buffer>, NativeStateError> {
        step_prefix(
            &self.prefix,
            &mut self.pos,
            &mut self.ready,
            self.max_record_bytes,
            buffer,
        )
    }

    pub(crate) fn detach_generation_ready(
        self,
    ) -> Result<(OpaqueHistoryCursor, NativeGenerationSeed), NativeStateError> {
        if !self.ready {
            return Err(NativeStateError::InvalidState);
        }
        Ok((
            self.cursor,
            NativeGenerationSeed {
                suffix: self.suffix,
                bounds: self.bounds,
            },
        ))
    }
}

fn requested_record_window(
    requested_max_bytes: u32,
    requested_max_rows: u32,
) -> Result<(usize, usize), NativeStateError> {
    let requested_bytes =
        usize::try_from(requested_max_bytes).map_err(|_| NativeStateError::LimitExceeded)?;
    let requested_rows =
        usize::try_from(requested_max_rows).map_err(|_| NativeStateError::LimitExceeded)?;
    if requested_bytes == 0 || requested_rows == 0 {
        return Err(NativeStateError::LimitExceeded);
    }
    Ok((requested_bytes, requested_rows))
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_protocol::caps::BootstrapProfileKind;

    fn terminal(cols: u16, rows: u16) -> GhosttyTerminal<'static, 'static> {
        let mut terminal = GhosttyTerminal::new(cols, rows).expect("canonical terminal");
        terminal
            .set_scrollback_max_lines(Some(1000))
            .expect("scrollback");
        terminal
            .set_continuation_max_bytes(CONTINUATION_LIMIT)
            .expect("continuation");
        terminal
    }

    fn history_terminal() -> GhosttyTerminal<'static, 'static> {
        let mut terminal = terminal(20, 4);
        for row in 0..300 {
            terminal.vt_write(format!("history-{row:03}\r\n").as_bytes());
        }
        terminal
    }

    fn drain_ready(capture: &mut NativeManagedCapture<'_>) {
        loop {
            let required = match capture.step(&mut []) {
                Err(NativeStateError::OutOfSpace { required_bytes, .. }) => required_bytes,
                other => panic!("probe: {other:?}"),
            };
            let mut exact = vec![0; required];
            if matches!(
                capture.step(&mut exact).expect("record").kind,
                NativeCheckpointChunkKind::Ready
            ) {
                break;
            }
        }
    }

    #[test]
    fn advertises_official_snapshot_native_v2() {
        assert!(official_snapshot_available());
        let advertised = native_bootstrap_capabilities();
        assert!(
            advertised
                .profiles
                .contains(BootstrapProfileKind::NativeState)
        );
        assert!(
            advertised
                .native_codecs
                .contains(EngineCodec::LibghosttyCheckpointV2)
        );
        assert_eq!(
            advertised.native_features,
            EngineFeatureSet::required_native()
        );
    }

    #[test]
    fn capture_then_live_write_then_history() {
        let limits = BootstrapLimits::new(phux_protocol::DEFAULT_BOOTSTRAP_CHUNK_BYTES, 64 * 1024)
            .expect("test limits");
        let mut source = history_terminal();
        let mut capture = NativeCheckpointCapture::new(&mut source, limits).expect("capture");
        loop {
            let required = match capture.step(&mut []) {
                Err(NativeStateError::OutOfSpace { required_bytes, .. }) => required_bytes,
                other => panic!("probe: {other:?}"),
            };
            let mut exact = vec![0; required];
            if matches!(
                capture.step(&mut exact).expect("record").kind,
                NativeCheckpointChunkKind::Ready
            ) {
                break;
            }
        }
        capture.abort().expect("abort");
        let mut history = NativeHistoryCursor::new(source, limits).expect("cursor");
        history.vt_write(b"live after READY\r\n");
        let mut pages = 0usize;
        loop {
            match history.next(limits.max_history_page_bytes(), &mut []) {
                Ok(NativeHistoryEvent::End) => break,
                Err(NativeStateError::OutOfSpace { required_bytes, .. }) => {
                    let mut exact = vec![0; required_bytes];
                    let _ = history
                        .next(limits.max_history_page_bytes(), &mut exact)
                        .expect("page");
                    pages += 1;
                }
                other => panic!("{other:?}"),
            }
        }
        let _ = pages;
        let mut source = history.into_terminal();
        NativeCheckpointCapture::new(&mut source, limits).expect("still capturable");
    }

    #[test]
    fn reset_and_resize_invalidate_cursor() {
        let limits = BootstrapLimits::default();
        let mut buffer = vec![0; 64];
        let mut reset = NativeHistoryCursor::new(history_terminal(), limits).expect("reset");
        reset.reset();
        assert_eq!(
            reset
                .next(limits.max_history_page_bytes(), &mut buffer)
                .unwrap_err(),
            NativeStateError::Reset
        );
        let mut resized = NativeHistoryCursor::new(history_terminal(), limits).expect("resize");
        resized.resize(21, 4, 8, 16).expect("resize");
        assert_eq!(
            resized
                .next(limits.max_history_page_bytes(), &mut buffer)
                .unwrap_err(),
            NativeStateError::Resize
        );
    }

    #[test]
    fn shared_generation_is_content_addressed() {
        let limits = BootstrapLimits::new(phux_protocol::DEFAULT_BOOTSTRAP_CHUNK_BYTES, 64 * 1024)
            .expect("limits");
        let mut manager = NativeTerminalManager::new(history_terminal(), 1).expect("manager");
        let mut capture = manager
            .capture_generation_bounded(limits, MAX_NATIVE_PREFIX_BYTES, MAX_NATIVE_PREFIX_CHUNKS)
            .expect("capture");
        drain_ready(&mut capture);
        let (cursor, seed) = manager.finish_generation_capture(capture).expect("finish");
        let bounds = seed.bounds();
        manager
            .install_generation(
                cursor,
                seed,
                bounds,
                bounds.required_reserved_bytes().expect("res"),
            )
            .expect("install");
        manager.retain_generation(&cursor).expect("retain");
        let first = manager
            .history_record_at(&cursor, 0, limits.max_history_page_bytes(), 1)
            .expect("first");
        let again = manager
            .history_record_at(&cursor, 0, limits.max_history_page_bytes(), 1)
            .expect("again");
        assert_eq!(first.bytes.as_ptr(), again.bytes.as_ptr());
        manager.release_generation(&cursor).expect("r1");
        manager.release_generation(&cursor).expect("r2");
        assert!(!manager.generations.contains_key(&cursor));
    }
}
