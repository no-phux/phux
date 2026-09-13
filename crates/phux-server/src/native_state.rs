//! Native checkpoint hosts over libghostty's official GHOSTSNP snapshot codec.
//!
//! Prefix capture advances one bounded engine record at a time through READY.
//! Detaching READY is O(1): the engine registers a history cut and encodes
//! nothing. Each later `HISTORY_REQUEST` borrows the live terminal for one
//! bounded scan or record step, so live PTY bytes continue between client
//! pulls. Phux forwards exact engine records and typed metadata without
//! decoding terminal contents.

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

use libghostty_vt::{
    Error as GhosttyError, Terminal as GhosttyTerminal,
    snapshot::{
        CaptureEvent, CaptureInvalidation, CaptureOptions, Decoder, HistoryCapture, OwnedCapture,
    },
};
use phux_protocol::caps::{BootstrapCapabilities, BootstrapLimits, EngineCodec, EngineFeatureSet};
use thiserror::Error;

/// Opaque manager-local generation identity: SHA-256 of READY bytes and nonce.
pub const TOKEN_LEN: usize = 32;
const LEGACY_CHECKPOINT_VERSION: u16 = EngineCodec::LibghosttyCheckpointV2 as u16;
const PROGRESSIVE_CHECKPOINT_VERSION: u16 = EngineCodec::LibghosttySnapshotV1 as u16;
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

/// Probe the linked engine and advertise official progressive snapshot v1.
#[must_use]
pub fn native_bootstrap_capabilities() -> BootstrapCapabilities {
    let requested = BootstrapLimits::default();
    if !official_snapshot_available() {
        return BootstrapCapabilities::new().with_limits(requested);
    }
    BootstrapCapabilities::new()
        .with_limits(requested)
        .with_native(
            EngineCodec::LibghosttySnapshotV1,
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
    // Prefix is the full GHOSTSNP blob so a `#![forbid(unsafe_code)]` client
    // can `Decoder::new_buf` at READY without holding IncrementalDecoder.
    // Suffix is the post-READY tail of the same buffer, served as history
    // pages to pullers. `Bytes::slice` shares the allocation.
    let encoded = Bytes::from(encoded);
    let suffix = encoded.slice(ready_at..);
    Ok(SnapshotCut {
        prefix: encoded,
        suffix,
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
        codec_version: LEGACY_CHECKPOINT_VERSION,
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
///
/// The continuation is leased, not encoded: [`OwnedCapture::detach`]
/// registers engine lease state (tracked pins and the history generation at
/// READY) and copies no page. It must be installed back into the actor-local
/// manager rather than sent to another thread.
#[derive(Debug)]
pub(crate) struct NativeGenerationSeed {
    capture: HistoryCapture<'static>,
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
    capture: Option<HistoryCapture<'static>>,
    /// Why the cut was spent before FINISH, when it was. A prune, mutation,
    /// reset, or engine failure at the frontier is kept so every later
    /// request from any owner gets that reason instead of `InvalidHandle`.
    failure: Option<NativeStateError>,
    scratch: Vec<u8>,
    pending: Vec<u8>,
    retained_bytes: usize,
    #[allow(
        dead_code,
        reason = "retained for install-time equality checks and debug"
    )]
    bounds: NativeGenerationBounds,
    owners: usize,
    charge: Arc<NativeGenerationCharge>,
}

impl NativeCheckpointGeneration {
    /// Drop a cut that cannot resume, keeping the reason for every later
    /// request at this frontier.
    fn spend(&mut self, error: NativeStateError) -> NativeStateError {
        self.capture = None;
        self.failure = Some(error);
        error
    }
}

/// Actor-owned terminal and bounded concurrent native history cuts.
///
/// Generations hold detached engine cuts whose pins live in `terminal`'s
/// page list. [`Drop`] releases those cuts first so a live lease cannot
/// outlive the terminal it tracks.
#[derive(Debug)]
pub(crate) struct NativeTerminalManager {
    terminal: Option<GhosttyTerminal<'static, 'static>>,
    generations: HashMap<OpaqueHistoryCursor, NativeCheckpointGeneration>,
    retired_generation_charges: Vec<Arc<NativeGenerationCharge>>,
    capacity: usize,
    capture_active: bool,
    next_generation: u64,
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
            terminal: Some(terminal),
            generations,
            retired_generation_charges,
            capacity,
            capture_active: false,
            next_generation: 1,
        })
    }

    pub(crate) fn terminal(&self) -> &GhosttyTerminal<'static, 'static> {
        self.terminal
            .as_ref()
            .unwrap_or_else(|| unreachable!("terminal unavailable only during prefix capture"))
    }

    pub(crate) fn vt_write(&mut self, bytes: &[u8]) {
        debug_assert!(!self.capture_active);
        match self.terminal.as_mut() {
            Some(terminal) => terminal.vt_write(bytes),
            None => unreachable!("terminal available outside prefix capture"),
        }
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
        self.terminal.as_mut().map_or_else(
            || unreachable!("terminal available outside prefix capture"),
            |terminal| terminal.resize(cols, rows, cell_width_px, cell_height_px),
        )
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
        let max_record_bytes = usize::try_from(limits.max_history_page_bytes())
            .map_err(|_| NativeStateError::LimitExceeded)?
            .min(max_prefix_bytes)
            .max(1);
        let max_records = max_prefix_chunks.min(MAX_NATIVE_PREFIX_CHUNKS);
        let generation = self.next_generation;
        let next_generation = generation
            .checked_add(1)
            .ok_or(NativeStateError::LimitExceeded)?;
        let terminal = self.terminal.take().ok_or(NativeStateError::InvalidState)?;
        let capture = match terminal.into_snapshot_capture(CaptureOptions {
            max_record_bytes,
            max_pages: max_records,
        }) {
            Ok(capture) => capture,
            Err(failure) => {
                self.terminal = Some(failure.terminal);
                return Err(failure.error.into());
            }
        };
        self.next_generation = next_generation;
        self.capture_active = true;
        Ok(NativeManagedCapture {
            capture,
            digest: Sha256::new(),
            generation,
            max_record_bytes,
            bounds: NativeGenerationBounds {
                max_record_bytes,
                max_rows: usize::try_from(phux_protocol::MAX_HISTORY_PAGE_ROWS)
                    .map_err(|_| NativeStateError::LimitExceeded)?,
                max_records: max_records.saturating_add(1),
                max_total_bytes: MAX_NATIVE_PREFIX_BYTES,
            },
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
        match capture.detach_generation_ready() {
            Ok((terminal, cursor, seed)) => {
                self.terminal = Some(terminal);
                self.capture_active = false;
                Ok((cursor, seed))
            }
            Err(failure) => {
                self.terminal = Some(failure.terminal);
                self.capture_active = false;
                Err(failure.error)
            }
        }
    }

    pub(crate) fn abort_generation_capture(&mut self, capture: NativeManagedCapture<'static>) {
        self.terminal = Some(capture.into_terminal());
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
        let records = NativeRecordTable::new(bounds.max_records)?;
        self.generations
            .try_reserve(1)
            .map_err(|_| NativeStateError::OutOfMemory)?;
        self.generations.insert(
            cursor,
            NativeCheckpointGeneration {
                records,
                capture: Some(seed.capture),
                failure: None,
                scratch: Vec::new(),
                pending: Vec::new(),
                retained_bytes: 0,
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
        self.capture_history_through(cursor, index)?;
        let generation = self
            .generations
            .get(cursor)
            .ok_or(NativeStateError::InvalidHandle)?;
        let record = generation
            .records
            .get(index)
            .ok_or(NativeStateError::InvalidHandle)?;
        record.for_request(requested_bytes, requested_rows)
    }

    fn capture_history_through(
        &mut self,
        cursor: &OpaqueHistoryCursor,
        index: usize,
    ) -> Result<(), NativeStateError> {
        {
            let generation = self
                .generations
                .get(cursor)
                .ok_or(NativeStateError::InvalidHandle)?;
            if generation.records.get(index).is_some() {
                return Ok(());
            }
            if let Some(failure) = generation.failure {
                return Err(failure);
            }
            if generation.capture.is_none() {
                return Err(NativeStateError::InvalidHandle);
            }
        }
        let terminal = self.terminal.as_mut().ok_or(NativeStateError::ImportBusy)?;
        let generation = self
            .generations
            .get_mut(cursor)
            .ok_or(NativeStateError::InvalidHandle)?;
        Self::capture_one_history_step(terminal, generation)?;
        if generation.records.get(index).is_none() {
            if let Some(failure) = generation.failure {
                return Err(failure);
            }
            return Err(NativeStateError::ImportBusy);
        }
        Ok(())
    }

    fn capture_one_history_step(
        terminal: &mut GhosttyTerminal<'static, 'static>,
        generation: &mut NativeCheckpointGeneration,
    ) -> Result<(), NativeStateError> {
        let required = match Self::probe_history_capture(terminal, generation)? {
            HistoryProbe::Scan => return Ok(()),
            HistoryProbe::Event(event) => return Self::store_history_event(generation, event),
            HistoryProbe::Required(required) => required,
        };
        if required > generation.bounds.max_record_bytes {
            return Err(NativeStateError::LimitExceeded);
        }
        if generation.scratch.capacity() < required {
            generation
                .scratch
                .try_reserve_exact(required.saturating_sub(generation.scratch.len()))
                .map_err(|_| NativeStateError::OutOfMemory)?;
        }
        generation.scratch.resize(required, 0);
        let event = Self::read_history_capture(terminal, generation)?;
        let written = event.written();
        generation
            .pending
            .try_reserve(written)
            .map_err(|_| NativeStateError::OutOfMemory)?;
        generation
            .pending
            .extend_from_slice(&generation.scratch[..written]);
        if generation
            .retained_bytes
            .checked_add(generation.pending.capacity())
            .and_then(|bytes| bytes.checked_add(generation.scratch.capacity()))
            .is_none_or(|bytes| bytes > generation.bounds.max_total_bytes)
        {
            return Err(NativeStateError::LimitExceeded);
        }
        Self::store_history_event(generation, event)
    }

    fn probe_history_capture(
        terminal: &mut GhosttyTerminal<'static, 'static>,
        generation: &mut NativeCheckpointGeneration,
    ) -> Result<HistoryProbe, NativeStateError> {
        let result = {
            let capture = generation
                .capture
                .as_mut()
                .ok_or(NativeStateError::InvalidHandle)?;
            capture.next(terminal, &mut [])
        };
        match result {
            Err(GhosttyError::OutOfSpace { required }) => Ok(HistoryProbe::Required(required)),
            Ok(CaptureEvent::Scan) => Ok(HistoryProbe::Scan),
            Ok(event) if event.written() == 0 => Ok(HistoryProbe::Event(event)),
            Ok(_) => Err(generation.spend(NativeStateError::InvalidState)),
            Err(error) => Err(generation.spend(error.into())),
        }
    }

    fn read_history_capture(
        terminal: &mut GhosttyTerminal<'static, 'static>,
        generation: &mut NativeCheckpointGeneration,
    ) -> Result<CaptureEvent, NativeStateError> {
        let result = {
            let NativeCheckpointGeneration {
                capture, scratch, ..
            } = generation;
            let capture = capture.as_mut().ok_or(NativeStateError::InvalidHandle)?;
            capture.next(terminal, scratch)
        };
        match result {
            Ok(event) => Ok(event),
            Err(GhosttyError::OutOfSpace { required }) => Err(NativeStateError::OutOfSpace {
                required_bytes: required,
                required_rows: 0,
            }),
            Err(error) => Err(generation.spend(error.into())),
        }
    }

    fn store_history_event(
        generation: &mut NativeCheckpointGeneration,
        event: CaptureEvent,
    ) -> Result<(), NativeStateError> {
        match event {
            CaptureEvent::Scan | CaptureEvent::Record { .. } => Ok(()),
            CaptureEvent::HistoryPage { rows, .. } => {
                Self::store_history_record(generation, rows, false)
            }
            CaptureEvent::Finish { .. } => {
                Self::store_history_record(generation, 0, true)?;
                generation.capture = None;
                Ok(())
            }
            CaptureEvent::Invalidated(reason) => Err(generation.spend(invalidation_error(reason))),
            CaptureEvent::Ready { .. } => Err(generation.spend(NativeStateError::InvalidState)),
        }
    }

    fn store_history_record(
        generation: &mut NativeCheckpointGeneration,
        rows: usize,
        finish: bool,
    ) -> Result<(), NativeStateError> {
        let next_bytes = generation
            .retained_bytes
            .checked_add(generation.pending.len())
            .filter(|bytes| *bytes <= generation.bounds.max_total_bytes)
            .ok_or(NativeStateError::LimitExceeded)?;
        if generation.pending.len() > generation.bounds.max_record_bytes
            || rows > generation.bounds.max_rows
            || generation.records.len >= generation.records.slots.len()
        {
            return Err(NativeStateError::LimitExceeded);
        }
        let payload = ChargedNativePayload::new(
            std::mem::take(&mut generation.pending).into_boxed_slice(),
            Arc::clone(&generation.charge),
        );
        generation.records.slots[generation.records.len] = Some(CachedNativeHistoryRecord {
            bytes: Bytes::from_owner(payload),
            rows,
            finish,
        });
        generation.records.len += 1;
        generation.retained_bytes = next_bytes;
        Ok(())
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

impl Drop for NativeTerminalManager {
    fn drop(&mut self) {
        // Detached cuts untrack pins from the terminal's page list when
        // released, so they must go first. Cached payloads that already
        // escaped through `Bytes` own their own allocations.
        self.generations.clear();
    }
}

/// First `HistoryCapture::next` against an empty buffer: a scan, a
/// zero-byte event, or the exact byte count the next record needs.
enum HistoryProbe {
    Scan,
    Event(CaptureEvent),
    Required(usize),
}

/// Actor-owned prefix capture that no longer borrows the live terminal.
#[derive(Debug)]
pub(crate) struct NativeManagedCapture<'manager> {
    capture: OwnedCapture<'static, 'static>,
    digest: Sha256,
    generation: u64,
    max_record_bytes: usize,
    bounds: NativeGenerationBounds,
    ready: bool,
    _marker: PhantomData<&'manager ()>,
}

struct NativeManagedCaptureFailure {
    error: NativeStateError,
    terminal: GhosttyTerminal<'static, 'static>,
}

impl NativeManagedCapture<'_> {
    pub(crate) const fn max_record_bytes(&self) -> usize {
        self.max_record_bytes
    }

    pub(crate) fn step<'buffer>(
        &mut self,
        buffer: &'buffer mut [u8],
    ) -> Result<NativeCheckpointChunk<'buffer>, NativeStateError> {
        if self.ready {
            return Err(NativeStateError::InvalidState);
        }
        let event = self.capture.next(buffer)?;
        self.digest.update(&buffer[..event.written()]);
        let kind = match event {
            CaptureEvent::Ready { .. } => {
                self.ready = true;
                NativeCheckpointChunkKind::Ready
            }
            CaptureEvent::Record { .. } => NativeCheckpointChunkKind::Record,
            _ => return Err(NativeStateError::InvalidState),
        };
        Ok(NativeCheckpointChunk {
            kind,
            codec_version: PROGRESSIVE_CHECKPOINT_VERSION,
            bytes: &buffer[..event.written()],
        })
    }

    fn detach_generation_ready(
        self,
    ) -> Result<
        (
            GhosttyTerminal<'static, 'static>,
            OpaqueHistoryCursor,
            NativeGenerationSeed,
        ),
        NativeManagedCaptureFailure,
    > {
        if !self.ready {
            return Err(NativeManagedCaptureFailure {
                error: NativeStateError::InvalidState,
                terminal: self.capture.into_terminal(),
            });
        }
        let mut digest = self.digest;
        digest.update(self.generation.to_le_bytes());
        let cursor = digest.finalize().into();
        // O(1): the engine registers a history lease and encodes nothing.
        // Each HISTORY_REQUEST later encodes one page from the live cut.
        let (terminal, capture) =
            self.capture
                .detach()
                .map_err(|failure| NativeManagedCaptureFailure {
                    error: failure.error.into(),
                    terminal: failure.terminal,
                })?;
        Ok((
            terminal,
            cursor,
            NativeGenerationSeed {
                capture,
                bounds: self.bounds,
            },
        ))
    }

    pub(crate) fn into_terminal(self) -> GhosttyTerminal<'static, 'static> {
        self.capture.into_terminal()
    }
}

const fn invalidation_error(reason: CaptureInvalidation) -> NativeStateError {
    match reason {
        CaptureInvalidation::Reset => NativeStateError::Reset,
        CaptureInvalidation::Resize => NativeStateError::Resize,
        CaptureInvalidation::WrongTerminal => NativeStateError::WrongTerminal,
        CaptureInvalidation::Mutation => NativeStateError::Stale,
        CaptureInvalidation::Evicted => NativeStateError::Pruned,
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

    /// Wide enough that retained scrollback is a HISTORY suffix, not only
    /// the READY active-area pages a 20x4 grid can fold into the prefix.
    fn paged_history_terminal() -> GhosttyTerminal<'static, 'static> {
        let mut terminal = GhosttyTerminal::new(80, 24).expect("paged history terminal");
        terminal
            .set_scrollback_max_lines(Some(100_000))
            .expect("history rows");
        terminal
            .set_scrollback_max_bytes(None)
            .expect("unlimited scrollback bytes");
        terminal
            .set_continuation_max_bytes(CONTINUATION_LIMIT)
            .expect("continuation");
        for row in 0..2_000 {
            terminal.vt_write(format!("history-{row:04}\r\n").as_bytes());
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

    fn history_at(
        manager: &mut NativeTerminalManager,
        cursor: &OpaqueHistoryCursor,
        index: usize,
        max_bytes: u32,
        max_rows: u32,
    ) -> Result<CachedNativeHistoryRecord, NativeStateError> {
        loop {
            match manager.history_record_at(cursor, index, max_bytes, max_rows) {
                Ok(record) => return Ok(record),
                Err(NativeStateError::ImportBusy) => {}
                Err(error) => return Err(error),
            }
        }
    }

    fn next_record(
        manager: &mut NativeTerminalManager,
        cursor: &OpaqueHistoryCursor,
        index: usize,
        limits: BootstrapLimits,
    ) -> CachedNativeHistoryRecord {
        history_at(
            manager,
            cursor,
            index,
            limits.max_history_page_bytes(),
            phux_protocol::MAX_HISTORY_PAGE_ROWS,
        )
        .unwrap_or_else(|error| panic!("history record: {error:?}"))
    }

    /// A 200x50 terminal pruned to `history_bytes` of retained scrollback,
    /// filled with styled full-width rows so a row's cost is representative
    /// rather than the best case an all-blank grid would give.
    fn deep_terminal(history_bytes: usize) -> GhosttyTerminal<'static, 'static> {
        let mut terminal = GhosttyTerminal::new(200, 50).expect("deep terminal");
        terminal
            .set_scrollback_max_lines(Some(10_000_000))
            .expect("history rows");
        terminal
            .set_scrollback_max_bytes(Some(history_bytes))
            .expect("retained byte ceiling");
        terminal
            .set_continuation_max_bytes(CONTINUATION_LIMIT)
            .expect("continuation");
        let body = "abcdefghij klmnopqrst uvwxyz0123 456789ABCD EFGHIJKLMN OPQRSTUVWX                     YZabcdefgh ijklmnopqr stuvwxyz01 23456789AB CDEFGHIJKL MNOPQRSTUV                     WXYZabcdef ghijklmnop qrstuvwx";
        // Overshoot the ceiling so pruning, not the write volume, decides how
        // much history the capture actually has to account for.
        for row in 0..(history_bytes / 64 + 4_000) {
            terminal.vt_write(
                format!("\x1b[38;5;{}m{row:06} \x1b[0m{body}\r\n", 16 + row % 216).as_bytes(),
            );
        }
        terminal
    }

    /// The wall time one attach spends inside the terminal's mutation
    /// exclusion, taken as the best of `samples` runs so a loaded build host
    /// cannot turn a constant-time operation into a false regression.
    fn best_detach_cost(
        manager: &mut NativeTerminalManager,
        limits: BootstrapLimits,
        samples: usize,
    ) -> std::time::Duration {
        let mut best = std::time::Duration::MAX;
        for _ in 0..samples {
            let mut capture = manager
                .begin_generation_capture(limits, MAX_NATIVE_PREFIX_BYTES, MAX_NATIVE_PREFIX_CHUNKS)
                .expect("generation capture");
            drain_ready(&mut capture);
            let started = std::time::Instant::now();
            let detached = manager.finish_generation_capture(capture);
            best = best.min(started.elapsed());
            drop(detached.expect("detach generation READY continuation"));
        }
        best
    }

    fn detach_managed_generation(
        manager: &mut NativeTerminalManager,
        limits: BootstrapLimits,
    ) -> (OpaqueHistoryCursor, NativeGenerationSeed) {
        let mut capture = manager
            .begin_generation_capture(limits, MAX_NATIVE_PREFIX_BYTES, MAX_NATIVE_PREFIX_CHUNKS)
            .expect("generation capture");
        drain_ready(&mut capture);
        manager
            .finish_generation_capture(capture)
            .expect("detach generation READY continuation")
    }

    fn install_leased_generation(
        manager: &mut NativeTerminalManager,
        limits: BootstrapLimits,
    ) -> (OpaqueHistoryCursor, u32, u32) {
        let (cursor, seed) = detach_managed_generation(manager, limits);
        let bounds = seed.bounds();
        let reserved = bounds
            .required_reserved_bytes()
            .expect("bounded generation reservation");
        manager
            .install_generation(cursor, seed, bounds, reserved)
            .expect("install leased generation");
        (
            cursor,
            u32::try_from(bounds.max_record_bytes).expect("protocol byte bound"),
            u32::try_from(bounds.max_rows).expect("protocol row bound"),
        )
    }

    fn drain_generation(
        manager: &mut NativeTerminalManager,
        cursor: &OpaqueHistoryCursor,
        start: usize,
        max_bytes: u32,
        max_rows: u32,
    ) -> Result<usize, NativeStateError> {
        let mut rows = 0;
        let mut index = start;
        loop {
            let record = history_at(manager, cursor, index, max_bytes, max_rows)?;
            rows += record.rows;
            if record.finish {
                return Ok(rows);
            }
            index += 1;
        }
    }

    fn lease_through(
        manager: &mut NativeTerminalManager,
        limits: BootstrapLimits,
        mutation: &[u8],
    ) -> Result<usize, NativeStateError> {
        for row in 0..2_000 {
            manager.vt_write(format!("history-{row:04}\r\n").as_bytes());
        }
        let (cursor, max_bytes, max_rows) = install_leased_generation(manager, limits);
        let first = history_at(manager, &cursor, 0, max_bytes, max_rows)
            .expect("HISTORY_BEGIN from a live lease");
        assert!(
            !first.finish,
            "fixture must have a history page before FINISH"
        );
        manager.vt_write(mutation);
        let outcome = drain_generation(manager, &cursor, 1, max_bytes, max_rows);
        manager
            .release_generation(&cursor)
            .expect("release leased generation");
        outcome
    }

    const fn is_lease_tombstone(error: NativeStateError) -> bool {
        matches!(
            error,
            NativeStateError::Pruned
                | NativeStateError::Stale
                | NativeStateError::Reset
                | NativeStateError::Resize
                | NativeStateError::WrongGeneration
        )
    }

    #[test]
    fn snapshot_split_leaves_history_suffix() {
        let mut source = history_terminal();
        let mut encoded = Vec::new();
        source.encode_snapshot(&mut encoded).expect("encode");
        let ready_at = snapshot_ready_offset(&encoded).expect("ready");
        assert!(
            ready_at < encoded.len(),
            "READY offset {ready_at} consumed the whole {}-byte snapshot; history suffix is empty",
            encoded.len()
        );
    }

    #[test]
    fn advertises_official_progressive_snapshot_v1() {
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
                .contains(EngineCodec::LibghosttySnapshotV1)
        );
        assert!(
            !advertised
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
        let first = next_record(&mut manager, &cursor, 0, limits);
        let again = next_record(&mut manager, &cursor, 0, limits);
        assert_eq!(first.bytes.as_ptr(), again.bytes.as_ptr());
        manager.release_generation(&cursor).expect("r1");
        manager.release_generation(&cursor).expect("r2");
        assert!(!manager.generations.contains_key(&cursor));
    }

    #[test]
    fn managed_capture_defers_multiple_pages_and_finish_past_ready() {
        let limits = BootstrapLimits::new(phux_protocol::DEFAULT_BOOTSTRAP_CHUNK_BYTES, 64 * 1024)
            .expect("limits");
        let mut source = terminal(200, 3);
        source
            .set_scrollback_max_lines(Some(5_000))
            .expect("history rows");
        source
            .set_scrollback_max_bytes(None)
            .expect("history bytes");
        for row in 0..3_000 {
            source.vt_write(format!("history-{row:04}\r\n").as_bytes());
        }
        let mut manager = NativeTerminalManager::new(source, 1).expect("manager");
        let mut capture = manager
            .capture_generation_bounded(limits, MAX_NATIVE_PREFIX_BYTES, MAX_NATIVE_PREFIX_CHUNKS)
            .expect("capture");
        drain_ready(&mut capture);
        let (cursor, seed) = manager.finish_generation_capture(capture).expect("READY");
        let bounds = seed.bounds();
        manager
            .install_generation(
                cursor,
                seed,
                bounds,
                bounds.required_reserved_bytes().expect("reservation"),
            )
            .expect("install");

        let mut index = 0;
        let mut pages = 0;
        loop {
            let record = next_record(&mut manager, &cursor, index, limits);
            assert!(
                !record.bytes.is_empty(),
                "PAGE and FINISH carry native authentication"
            );
            if record.finish {
                assert_eq!(record.rows, 0);
                break;
            }
            assert!(record.rows > 0);
            pages += 1;
            if pages == 1 {
                manager.vt_write(b"live-between-history-pages\r\n");
            }
            index += 1;
        }
        assert!(pages >= 2, "fixture must produce multiple deferred pages");
    }

    #[test]
    fn repeated_ready_prefixes_receive_distinct_generation_cursors() {
        let limits = BootstrapLimits::default();
        let mut manager = NativeTerminalManager::new(history_terminal(), 2).expect("manager");
        let mut first = manager.capture(limits).expect("first capture");
        drain_ready(&mut first);
        let (first_cursor, first_seed) = manager
            .finish_generation_capture(first)
            .expect("first READY");
        drop(first_seed);

        let mut second = manager.capture(limits).expect("second capture");
        drain_ready(&mut second);
        let (second_cursor, second_seed) = manager
            .finish_generation_capture(second)
            .expect("second READY");
        drop(second_seed);

        assert_ne!(
            first_cursor, second_cursor,
            "generation freshness must not depend on history bytes being encoded before READY"
        );
    }

    #[test]
    fn old_history_cut_waits_while_a_new_prefix_temporarily_owns_the_terminal() {
        let limits = BootstrapLimits::default();
        let mut manager = NativeTerminalManager::new(history_terminal(), 2).expect("manager");
        let mut first = manager.capture(limits).expect("first capture");
        drain_ready(&mut first);
        let (cursor, seed) = manager
            .finish_generation_capture(first)
            .expect("first READY");
        let bounds = seed.bounds();
        manager
            .install_generation(
                cursor,
                seed,
                bounds,
                bounds.required_reserved_bytes().expect("reservation"),
            )
            .expect("install old cut");

        let second = manager.capture(limits).expect("second prefix");
        assert!(matches!(
            manager.history_record_at(
                &cursor,
                0,
                limits.max_history_page_bytes(),
                phux_protocol::MAX_HISTORY_PAGE_ROWS,
            ),
            Err(NativeStateError::ImportBusy)
        ));
        assert!(manager.has_generation(&cursor));

        manager.abort_generation_capture(second);
        let record = next_record(&mut manager, &cursor, 0, limits);
        assert!(!record.bytes.is_empty());
        assert!(manager.has_generation(&cursor));
    }

    /// The attach critical path must not scale with `defaults.history-bytes`.
    ///
    /// Releasing a READY capture registers an engine lease and encodes
    /// nothing, so the exclusion is held for the same time whatever the
    /// scrollback depth. The bound is relative: 16 MiB may cost at most
    /// three times what 2 MiB does, plus 2 ms for timer and scheduling noise
    /// that best-of-seven sampling does not absorb.
    #[test]
    fn attach_detach_cost_is_flat_in_retained_history() {
        let limits = BootstrapLimits::default();
        let mut shallow =
            NativeTerminalManager::new(deep_terminal(2 * 1024 * 1024), 4).expect("shallow manager");
        let shallow_cost = best_detach_cost(&mut shallow, limits, 7);
        let mut deep =
            NativeTerminalManager::new(deep_terminal(16 * 1024 * 1024), 4).expect("deep manager");
        let deep_cost = best_detach_cost(&mut deep, limits, 7);

        let allowed = shallow_cost.saturating_mul(3) + std::time::Duration::from_millis(2);
        assert!(
            deep_cost <= allowed,
            "detach cost grew with retained history: 2 MiB {shallow_cost:?}, \
             16 MiB {deep_cost:?}, allowed {allowed:?}; a leased cut encodes \
             nothing at attach"
        );
    }

    /// A page evicted between cursor issue and request is a typed engine
    /// status, never stale bytes.
    #[test]
    fn history_evicted_under_a_live_lease_tombstones_rather_than_lying() {
        let limits = BootstrapLimits::default();
        let mut manager =
            NativeTerminalManager::new(deep_terminal(2 * 1024 * 1024), 4).expect("native manager");
        let (cursor, max_bytes, max_rows) = install_leased_generation(&mut manager, limits);
        history_at(&mut manager, &cursor, 0, max_bytes, max_rows)
            .expect("HISTORY_BEGIN from a live lease");

        for row in 0..200_000 {
            manager.vt_write(format!("evicting-{row:06}\r\n").as_bytes());
        }

        let error = history_at(&mut manager, &cursor, 1, max_bytes, max_rows)
            .expect_err("a pruned lease cannot serve a page");
        assert!(
            is_lease_tombstone(error),
            "eviction must name its cause; `history_tombstone_reason` turns \
             exactly these into a tombstone the client can act on, and anything \
             else into an opaque CodecFailure. Got {error:?}"
        );
    }

    /// An engine failure at the frontier reaches every owner with its cause.
    #[test]
    fn a_failed_frontier_gives_every_owner_the_engine_reason() {
        let limits = BootstrapLimits::default();
        let mut manager =
            NativeTerminalManager::new(deep_terminal(2 * 1024 * 1024), 4).expect("native manager");
        let (cursor, max_bytes, max_rows) = install_leased_generation(&mut manager, limits);
        manager
            .retain_generation(&cursor)
            .expect("second generation owner");
        history_at(&mut manager, &cursor, 0, max_bytes, max_rows)
            .expect("HISTORY_BEGIN from a live lease");
        for row in 0..200_000 {
            manager.vt_write(format!("evicting-{row:06}\r\n").as_bytes());
        }

        let first = history_at(&mut manager, &cursor, 1, max_bytes, max_rows)
            .expect_err("a pruned lease cannot serve a page");
        assert!(
            is_lease_tombstone(first),
            "output past the oldest leased page is a typed invalidation, got {first:?}"
        );
        let second = history_at(&mut manager, &cursor, 1, max_bytes, max_rows)
            .expect_err("the spent frontier cannot serve the second owner either");
        assert_eq!(
            second, first,
            "every owner must get the engine's reason, not a spent-handle status"
        );
        assert!(
            manager.generations[&cursor].capture.is_none(),
            "a cut the engine marked dead must not be kept"
        );
        assert_eq!(manager.generations[&cursor].failure, Some(first));
    }

    /// A wide, styled pane still pages under the default 1 MiB page limit.
    #[test]
    fn a_wide_styled_pane_pages_under_the_default_page_limit() {
        let limits = BootstrapLimits::default();
        let mut manager =
            NativeTerminalManager::new(deep_terminal(2 * 1024 * 1024), 4).expect("native manager");
        let (cursor, _, max_rows) = install_leased_generation(&mut manager, limits);
        let rows = drain_generation(
            &mut manager,
            &cursor,
            0,
            limits.max_history_page_bytes(),
            max_rows,
        )
        .expect("every slice fits the default page limit");
        assert!(rows > 500, "the pane's history was delivered: {rows} rows");
    }

    /// A live lease stays well-behaved across every way the terminal can
    /// invalidate its history, and across the manager's own teardown.
    #[test]
    fn a_live_lease_survives_invalidation_and_manager_teardown() {
        let limits = BootstrapLimits::default();
        let mut manager =
            NativeTerminalManager::new(paged_history_terminal(), 2).expect("native manager");

        let alternate = lease_through(
            &mut manager,
            limits,
            b"\x1b[?1049halternate screen output\r\n\x1b[?1049l",
        );
        assert!(
            matches!(alternate, Ok(rows) if rows > 0),
            "the primary screen's history is untouched by an alternate-screen \
             visit: {alternate:?}"
        );
        let erased = lease_through(&mut manager, limits, b"\x1b[3J");
        assert!(
            matches!(
                erased,
                Err(NativeStateError::Stale | NativeStateError::Pruned)
            ),
            "erasing scrollback invalidates the cut: {erased:?}"
        );
        assert_eq!(
            lease_through(&mut manager, limits, b"\x1bc"),
            Err(NativeStateError::Reset),
            "a full reset invalidates the cut"
        );

        let (resized, max_bytes, max_rows) = install_leased_generation(&mut manager, limits);
        history_at(&mut manager, &resized, 0, max_bytes, max_rows)
            .expect("HISTORY_BEGIN from a live lease");
        manager.resize(21, 4, 8, 16).expect("resize live terminal");
        assert!(
            !manager.has_generation(&resized),
            "resize retires every generation; the actor tombstones them as Resize"
        );
        assert_eq!(
            manager.history_record_at(&resized, 1, max_bytes, max_rows),
            Err(NativeStateError::InvalidHandle)
        );

        let (live, max_bytes, max_rows) = install_leased_generation(&mut manager, limits);
        history_at(&mut manager, &live, 0, max_bytes, max_rows)
            .expect("HISTORY_BEGIN from a live lease");
        drop(manager);
    }

    /// Output under a scroll region between READY and the first request.
    ///
    /// The incremental lease reported `Stale` here: a fixed status line
    /// (`DECSTBM`) rewrites the page holding the newest pin. The official
    /// GHOSTSNP cut does not: both plain output and scroll-region output
    /// leave the retained pages deliverable. Pin that so a later engine
    /// change cannot silently start dropping the cut.
    #[test]
    fn scroll_region_output_after_ready_is_reported_not_misread() {
        fn lease_unrequested_through(mutation: &[u8]) -> Result<usize, NativeStateError> {
            let mut terminal = GhosttyTerminal::new(80, 24).expect("terminal with headroom");
            terminal
                .set_scrollback_max_lines(Some(100_000))
                .expect("row headroom");
            terminal
                .set_scrollback_max_bytes(Some(16 * 1024 * 1024))
                .expect("byte headroom, so nothing prunes");
            terminal
                .set_continuation_max_bytes(CONTINUATION_LIMIT)
                .expect("continuation");
            for row in 0..2_000 {
                terminal.vt_write(format!("history-{row:04}\r\n").as_bytes());
            }
            let limits = BootstrapLimits::default();
            let mut manager = NativeTerminalManager::new(terminal, 2).expect("native manager");
            let (cursor, max_bytes, max_rows) = install_leased_generation(&mut manager, limits);
            manager.vt_write(mutation);
            drain_generation(&mut manager, &cursor, 0, max_bytes, max_rows)
        }

        let mut plain = Vec::new();
        let mut region = b"\x1b[1;23r\x1b[23;1H".to_vec();
        for row in 0..50 {
            plain.extend_from_slice(format!("plain-{row:02}\r\n").as_bytes());
            region.extend_from_slice(format!("region-{row:02}\r\n").as_bytes());
        }
        region.extend_from_slice(b"\x1b[24;1Hstatus line\x1b[r");

        let control = lease_unrequested_through(&plain);
        assert!(
            matches!(control, Ok(rows) if rows >= 1_500),
            "plain output after READY leaves the cut deliverable: {control:?}"
        );
        let region = lease_unrequested_through(&region);
        assert!(
            matches!(region, Ok(rows) if rows >= 1_500),
            "scroll-region output after READY must leave the cut deliverable, \
             not a different or truncated stream: {region:?}"
        );
    }
}
